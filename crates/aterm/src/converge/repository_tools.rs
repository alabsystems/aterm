// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A joined headless account owns the same unattended package attempt as init. It
//! runs after the base lock and fleet work, separately from repository acquisition.

use aterm_converge::{Class, Explain, Item, Layer, Probe, Remedy, Requires, StoodAside};
use aterm_machines::init::BaseLock;
use aterm_machines::init_github::RepositorySelection;
use aterm_machines::init_toolset::Toolset;
use aterm_machines::items::{Ctx, Item as MachineItem};
use aterm_machines::port::{Live, System};
use aterm_secrets::client::{self, Paths};
use aterm_types::macos_setup::Setting;
use std::sync::{Arc, Mutex, PoisonError};

const ID: &str = "sync.repository-prerequisites";
type Current = Box<dyn Fn() -> Result<(), String> + Send + Sync>;

struct Admission {
    ctx: Ctx,
    current: Current,
    continuing: Current,
    credential: Option<RepositorySelection>,
}

enum Scope {
    Off(String),
    Waiting(String),
    Admitted(Box<Admission>),
}

enum Gate {
    Off(String),
    Unread(String),
}

impl Gate {
    fn why(self) -> String {
        match self {
            Self::Off(why) | Self::Unread(why) => why,
        }
    }
}

type Capture = Box<dyn Fn(&dyn System) -> Result<Scope, String> + Send + Sync>;

pub(super) struct Prerequisites {
    sys: Arc<dyn System + Send>,
    toolset: Toolset,
    capture: Capture,
    admitted: Mutex<Option<Box<Admission>>>,
    aside: Mutex<Option<StoodAside>>,
}

impl Prerequisites {
    pub(super) fn live() -> Self {
        Self::new(
            Arc::new(Live::new()),
            crate::doctor::toolset(BaseLock::Try).default_selected("git"),
            Box::new(capture),
        )
    }

    fn new(sys: Arc<dyn System + Send>, toolset: Toolset, capture: Capture) -> Self {
        Self {
            sys,
            toolset,
            capture,
            admitted: Mutex::new(None),
            aside: Mutex::new(None),
        }
    }

    fn waiting(why: String) -> Probe {
        Probe::Deferred { why }
    }
}

/// Package choices are read afresh, including a damaged removal record. The actual
/// selected update retains its own policy checks under the package store lock.
fn packages() -> Result<(), Gate> {
    let cfg = atpkg::config::load();
    let layout = atpkg::store::resolve_from(&cfg);
    packages_in(&cfg, layout.as_ref())
}

fn packages_in(
    cfg: &atpkg::config::PackagesConfig,
    layout: Option<&atpkg::store::Layout>,
) -> Result<(), Gate> {
    if cfg.unreadable {
        return Err(Gate::Unread(
            "package choices cannot be read; repository tools wait".into(),
        ));
    }
    if !cfg.enabled() || !cfg.installs_unattended() {
        return Err(Gate::Off("automatic package installation is off".into()));
    }
    let layout = layout.ok_or_else(|| Gate::Unread("no package store is configured".into()))?;
    if cfg.exclude().iter().any(|name| name == "git") {
        return Err(Gate::Off(
            "Git is excluded in package settings; repository tools stay unchanged".into(),
        ));
    }
    if layout
        .removed_programs()
        .map_err(|_| {
            Gate::Unread("package removal choices cannot be read; repository tools wait".into())
        })?
        .contains("git")
    {
        return Err(Gate::Off(
            "Git was removed by you; automatic setup leaves it unchanged".into(),
        ));
    }
    if let Some(why) = atpkg::adoption::unattended_install_off(layout, cfg) {
        return Err(Gate::Off(why.into()));
    }
    Ok(())
}

fn current_scope(sys: &dyn System, ctx: &Ctx, paths: &Paths) -> Result<(), Gate> {
    intent_scope(sys, ctx, paths)?;
    packages()
}

fn intent_scope(sys: &dyn System, ctx: &Ctx, paths: &Paths) -> Result<(), Gate> {
    check_scope(
        sys,
        ctx,
        paths,
        super::vault_held(),
        &super::setup_holds(),
        aterm_secrets::swarm::enabled_in_home(&paths.home).map_err(|why| why.to_string()),
    )
}

fn check_scope(
    sys: &dyn System,
    ctx: &Ctx,
    paths: &Paths,
    vault_hold: Option<String>,
    holds: &aterm_machines::init::Holds,
    sharing: Result<bool, String>,
) -> Result<(), Gate> {
    let now = super::repository_context(sys).map_err(Gate::Unread)?;
    if now.users != ctx.users || paths.home != std::path::Path::new(&ctx.users[0].home) {
        return Err(Gate::Unread("the repository tools account changed".into()));
    }
    if let Some(why) = vault_hold {
        return Err(Gate::Off(why));
    }
    if holds.holds(Setting::Repositories) {
        return Err(Gate::Off(holds.why(Setting::Repositories)));
    }
    if !sharing.map_err(Gate::Unread)? {
        return Err(Gate::Off(aterm_secrets::swarm::SWARM_OFF.into()));
    }
    if !aterm_machines::init_github::uses_vault_inventory(sys, ctx).map_err(Gate::Unread)? {
        return Err(Gate::Off(
            "the repository source no longer uses the private vault inventory".into(),
        ));
    }
    Ok(())
}

fn capture(sys: &dyn System) -> Result<Scope, String> {
    let ctx = super::repository_context(sys)?;
    let paths = Paths::live().map_err(|why| why.to_string())?;
    match intent_scope(sys, &ctx, &paths) {
        Ok(()) => {}
        Err(Gate::Off(why)) => return Ok(Scope::Off(why)),
        Err(Gate::Unread(why)) => return Ok(Scope::Waiting(why)),
    }
    let expected = ctx.clone();
    let selected = paths.clone();
    let intent_context = ctx.clone();
    let intent_paths = paths.clone();
    admit(
        sys,
        ctx,
        paths,
        Box::new(move || {
            intent_scope(&Live::new(), &intent_context, &intent_paths).map_err(Gate::why)
        }),
        Box::new(move || current_scope(&Live::new(), &expected, &selected).map_err(Gate::why)),
    )
}

fn admit(
    sys: &dyn System,
    ctx: Ctx,
    paths: Paths,
    intent: Current,
    policy: Current,
) -> Result<Scope, String> {
    intent()?;
    let Some(config) = super::vault_config(&paths)? else {
        return Ok(Scope::Off(
            "no private repository vault is configured".into(),
        ));
    };
    let Some(selection) = RepositorySelection::capture(sys, &ctx)? else {
        return Ok(Scope::Waiting(super::REPOSITORIES_SYNCING.into()));
    };
    if selection.count() == 0 {
        selection.ensure_current(sys)?;
        intent()?;
        return Ok(Scope::Off(
            "the private inventory selects no repositories".into(),
        ));
    }
    if selection.needs_credential(sys)? {
        let policy: Arc<dyn Fn() -> Result<(), String> + Send + Sync> = Arc::from(intent);
        let current = Arc::clone(&policy);
        return Ok(Scope::Admitted(Box::new(Admission {
            ctx,
            current: Box::new(move || current()),
            continuing: Box::new(move || policy()),
            credential: Some(selection),
        })));
    }
    if let Err(why) = policy() {
        return Ok(Scope::Off(why));
    }
    let Some(ready) = client::with_materialization_ready_for_entries(
        &paths,
        &config,
        &aterm_machines::init_github::VAULT_INPUT_ENTRIES,
        || Ok(()),
    )
    .map_err(|why| why.to_string())?
    else {
        return Ok(Scope::Waiting(super::REPOSITORIES_SYNCING.into()));
    };
    policy()?;
    let policy: Arc<dyn Fn() -> Result<(), String> + Send + Sync> = Arc::from(policy);
    let continuing_policy = Arc::clone(&policy);
    let continuing_snapshot = ready.snapshot.clone();
    let continuing_paths = paths.clone();
    Ok(Scope::Admitted(Box::new(Admission {
        ctx,
        credential: None,
        current: Box::new(move || {
            policy()?;
            if !ready
                .snapshot
                .is_current(&paths)
                .map_err(|why| why.to_string())?
            {
                return Err(super::REPOSITORIES_SYNCING.into());
            }
            policy()
        }),
        continuing: Box::new(move || {
            continuing_policy()?;
            if !continuing_snapshot
                .source_continues(&continuing_paths)
                .map_err(|why| why.to_string())?
            {
                return Err(super::REPOSITORIES_SYNCING.into());
            }
            continuing_policy()
        }),
    })))
}

impl Item for Prerequisites {
    fn id(&self) -> String {
        ID.into()
    }
    fn family(&self) -> &'static str {
        aterm_machines::sync::FAMILY
    }
    fn title(&self) -> String {
        "Git prerequisite for automatic repositories".into()
    }
    fn layer(&self) -> Layer {
        Layer::Workstation
    }
    fn class(&self) -> Class {
        Class::Install
    }
    fn requires(&self) -> Requires {
        Requires::Account
    }
    fn probe(&self) -> Probe {
        let mut admitted = self.admitted.lock().unwrap_or_else(PoisonError::into_inner);
        if admitted.is_none() {
            match (self.capture)(self.sys.as_ref()) {
                Ok(Scope::Off(why)) => return Probe::NotApplicable { why },
                Ok(Scope::Waiting(why)) | Err(why) => return Self::waiting(why),
                Ok(Scope::Admitted(ready)) => *admitted = Some(ready),
            }
        }
        let ready = admitted.as_ref().expect("captured above");
        if let Err(why) = (ready.current)() {
            return Self::waiting(why);
        }
        if let Some(selection) = &ready.credential {
            return match selection.needs_credential(self.sys.as_ref()) {
                Ok(true) => Probe::Change {
                    from: selection.waiting(),
                    to: "record the development credential need before preparing repository tools"
                        .into(),
                },
                Ok(false) => {
                    *admitted = None;
                    Self::waiting(
                        "credential inputs arrived; repository tools await a paired capture".into(),
                    )
                }
                Err(why) => Self::waiting(why),
            };
        }
        // The actual capability check starts no Git child while the vault is
        // locked. Its result cannot outlive source or package-policy admission.
        let usable =
            aterm_machines::init_github::git_ready_for_inventory(self.sys.as_ref(), &ready.ctx);
        if let Err(why) = (ready.current)() {
            return Self::waiting(why);
        }
        let usable = match usable {
            Ok(usable) => usable,
            Err(why) => return Self::waiting(why),
        };
        if usable {
            return Probe::Ok {
                now: "Git meets the selected repositories' requirements".into(),
            };
        }
        match self.toolset.probe(self.sys.as_ref(), &ready.ctx) {
            // The aggregate counts active packages. A missing ready marker or an
            // unusable executable can still require the signed install/update path.
            Probe::Ok { .. } | Probe::Change { .. } => Probe::Change {
                from: "the active toolset has no usable Git for repository setup".into(),
                to: "install the selected managed Git without waiting for unrelated packages"
                    .into(),
            },
            probe => probe,
        }
    }
    fn apply(&self) -> Result<(), String> {
        let admitted = self.admitted.lock().unwrap_or_else(PoisonError::into_inner);
        let ready = admitted
            .as_ref()
            .ok_or("repository tools have no admitted source")?;
        if let Some(selection) = &ready.credential {
            let _ = selection.request_credential(
                self.sys.as_ref(),
                ready.current.as_ref(),
                ready.continuing.as_ref(),
            )?;
            *self.aside.lock().unwrap_or_else(PoisonError::into_inner) =
                Some(StoodAside::Waiting(selection.waiting()));
            return Ok(());
        }
        let result = self.toolset.apply_with_guards(
            self.sys.as_ref(),
            ready.current.as_ref(),
            ready.continuing.as_ref(),
        );
        if let Err(why) = (ready.current)() {
            *self.aside.lock().unwrap_or_else(PoisonError::into_inner) =
                Some(StoodAside::Waiting(why));
            return Ok(());
        }
        result
    }
    fn stood_aside(&self) -> Option<StoodAside> {
        self.aside
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .or_else(|| self.toolset.stood_aside())
    }
    fn explain(&self, probe: &Probe) -> Explain {
        let why = match probe {
            Probe::Ok { now } => now,
            Probe::Change { from, .. } => from,
            Probe::NotApplicable { why }
            | Probe::Deferred { why }
            | Probe::Blocked { why }
            | Probe::Waits { why, .. }
            | Probe::Unusable { why } => why,
            Probe::Person { how, .. } => how,
        };
        Explain {
            why: why.clone(),
            remedy: Remedy::None,
        }
    }
}

#[cfg(test)]
#[path = "repository_tools_tests.rs"]
mod tests;
