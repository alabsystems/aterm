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
                .is_current_now(&paths)
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
            let _ = selection.request_credential(self.sys.as_ref(), ready.current.as_ref())?;
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
mod tests {
    use super::*;
    use aterm_machines::init::Holds;
    use aterm_machines::init_toolset::{Atpkg, Found};
    use aterm_machines::port::{Fake, Ran};
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Choices {
        holds: Mutex<Holds>,
        packages: Mutex<atpkg::config::PackagesConfig>,
        // This is the injected package-reader's existing consent result. No
        // package command runs in these fixtures or overrides a real manager.
        removed: AtomicBool,
        calls: Mutex<Vec<String>>,
    }

    fn fixture() -> (Arc<Fake>, Arc<Choices>, Prerequisites) {
        fixture_at("/fixture/repository-user")
    }

    fn inventory_bytes(required: Option<&[&str]>) -> Vec<u8> {
        let repositories: Vec<_> = required
            .into_iter()
            .map(|paths| {
                aterm_json::json!({
                    "id":1,"name":"private-code","owner":{"login":"fixture"},
                    "full_name":"fixture/private-code","private":true,
                    "clone_url":"https://github.com/fixture/private-code.git",
                    "default_branch":"main","required_submodules":paths
                })
            })
            .collect();
        aterm_json::to_vec(&aterm_json::json!({
            "schema":2,"account":"fixture","repositories":repositories
        }))
        .unwrap()
    }

    fn inventory(sys: &Fake, home: &str, required: Option<&[&str]>) {
        let bytes = inventory_bytes(required);
        let path = std::path::Path::new(home).join(".secrets/github-repositories.json");
        sys.file(path.to_str().unwrap(), &bytes).stat_of(
            path.to_str().unwrap(),
            aterm_machines::port::Stat {
                uid: 501,
                gid: 20,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: bytes.len() as u64,
            },
        );
    }

    fn fixture_at(home: &str) -> (Arc<Fake>, Arc<Choices>, Prerequisites) {
        let mut sys = Fake::default();
        sys.euid = 501;
        let who = aterm_machines::model::User {
            name: "fixture".into(),
            uid: 501,
            gid: 20,
            home: home.into(),
            shell: "/bin/zsh".into(),
            admin: false,
        };
        sys.logged_in(&who);
        sys.stat_of(
            home,
            aterm_machines::port::Stat {
                uid: who.uid,
                gid: who.gid,
                mode: 0o700,
                dir: true,
                symlink: false,
                len: 0,
            },
        );
        inventory(&sys, home, Some(&[]));
        let ctx = super::super::repository_context(&sys).unwrap();
        let sys = Arc::new(sys);
        let choices = Arc::new(Choices {
            holds: Mutex::new(Holds::NONE),
            packages: Mutex::new(atpkg::config::PackagesConfig::default()),
            removed: AtomicBool::new(false),
            calls: Mutex::new(Vec::new()),
        });
        let found = Arc::clone(&sys);
        let landing = Arc::clone(&sys);
        let called = Arc::clone(&choices);
        let tools = Toolset::new(
            Box::new(move || {
                if aterm_machines::init_github::git_installed(found.as_ref()) {
                    Found::Complete("Git is ready".into())
                } else {
                    Found::Missing("Git is missing".into())
                }
            }),
            Box::new(move |args, _, _| {
                called.calls.lock().unwrap().push(args.join(" "));
                if args.first().is_some_and(|verb| verb == "update") {
                    landing.executable(aterm_machines::items::github::CLT_GIT, b"fixture Git");
                    landing.executable(aterm_machines::items::github::SYSTEM_GIT, b"fixture Git");
                }
                Ok(Ran::ok(""))
            }),
            None,
            Atpkg {
                update_flags: &["--defer-busy-flip"],
                left_for_next_pass: |_| Vec::new(),
            },
        )
        .default_selected("git");
        let check_sys = Arc::clone(&sys);
        let check_choices = Arc::clone(&choices);
        let capture: Capture = Box::new(move |_| {
            let paths = Paths::locate_for_home(std::path::Path::new(&ctx.users[0].home));
            let selected = ctx.clone();
            let sys = Arc::clone(&check_sys);
            let choices = Arc::clone(&check_choices);
            let current: Current = Box::new(move || {
                check_scope(
                    sys.as_ref(),
                    &selected,
                    &paths,
                    None,
                    &choices.holds.lock().unwrap(),
                    Ok(true),
                )
                .map_err(Gate::why)?;
                let cfg = choices.packages.lock().unwrap();
                if !cfg.enabled()
                    || !cfg.installs_unattended()
                    || choices.removed.load(Ordering::SeqCst)
                {
                    return Err("package installation held".into());
                }
                Ok(())
            });
            if let Err(why) = current() {
                return Ok(Scope::Off(why));
            }
            let current: Arc<dyn Fn() -> Result<(), String> + Send + Sync> = Arc::from(current);
            let continuing = Arc::clone(&current);
            Ok(Scope::Admitted(Box::new(Admission {
                ctx: ctx.clone(),
                current: Box::new(move || current()),
                continuing: Box::new(move || continuing()),
                credential: None,
            })))
        });
        let item = Prerequisites::new(sys.clone(), tools, capture);
        (sys, choices, item)
    }

    #[test]
    fn explicit_holds_and_package_choices_start_no_prerequisite_child() {
        let (_, choices, allowed) = fixture();
        assert!(matches!(allowed.probe(), Probe::Change { .. }));
        allowed.apply().unwrap();
        assert!(
            matches!(allowed.probe(), Probe::Ok { .. }),
            "new Git is read back through installed_git"
        );
        assert_eq!(
            *choices.calls.lock().unwrap(),
            ["update --defer-busy-flip --default-selected git"]
        );
        let (sys, choices, empty) = fixture();
        inventory(&sys, "/fixture/repository-user", None);
        assert!(
            matches!(empty.probe(), Probe::Ok { .. }),
            "an empty inventory needs no Git"
        );
        assert!(choices.calls.lock().unwrap().is_empty());
        for choice in [
            "package-off",
            "removed",
            "repositories-off",
            "stop",
            "profile-off",
        ] {
            let (sys, choices, item) = fixture();
            match choice {
                "package-off" => {
                    *choices.packages.lock().unwrap() =
                        atpkg::config::parse_packages("[packages]\nenabled=false\n")
                }
                "removed" => choices.removed.store(true, Ordering::SeqCst),
                "repositories-off" => choices
                    .holds
                    .lock()
                    .unwrap()
                    .off
                    .set(Setting::Repositories, true),
                "stop" => choices.holds.lock().unwrap().stopped = Some("setup stopped".into()),
                "profile-off" => {
                    let bytes = b"[github]\nenabled=false\n";
                    let path = "/fixture/repository-user/.config/aterm/init.toml";
                    sys.file(path, bytes).stat_of(
                        path,
                        aterm_machines::port::Stat {
                            uid: 501,
                            gid: 20,
                            mode: 0o600,
                            dir: false,
                            symlink: false,
                            len: bytes.len() as u64,
                        },
                    );
                }
                _ => unreachable!(),
            }
            assert!(
                matches!(item.probe(), Probe::NotApplicable { .. }),
                "{choice}"
            );
            assert!(choices.calls.lock().unwrap().is_empty(), "{choice}");
        }
    }

    #[test]
    fn package_cancellation_is_nonwaiting_and_full_readiness_still_guards_each_pass() {
        for change in ["source", "file", "writer", "stop"] {
            use client::{JoinOptions, JoinPassword};
            let root = aterm_tempfile::tempdir().unwrap();
            let home = root.path().join("home");
            let source = root.path().join("private-tools.secrets");
            let paths = Paths::for_home(&home);
            let before = format!(
                "gh_access_token=fixture-one\ngithub-repositories.json={}\n",
                String::from_utf8(inventory_bytes(Some(&[]))).unwrap()
            );
            aterm_uds::atomic_file::write(&source, before.as_bytes(), 0o600).unwrap();
            client::join(
                &paths,
                &JoinOptions {
                    source: source.to_string_lossy().into_owned(),
                    host: Some("fixture".into()),
                    interactive: false,
                    wait: std::time::Duration::ZERO,
                    confirm: &|_| false,
                    password: JoinPassword::Given(None),
                    fetched: None,
                    requested: &|| {},
                    say: &|_| {},
                },
            )
            .unwrap();
            let (_, choices, mut item) = fixture_at(home.to_str().unwrap());
            let original = item.capture;
            let scoped = paths.clone();
            item.capture = Box::new(move |sys| match original(sys)? {
                Scope::Admitted(ready) => {
                    let policy: Arc<dyn Fn() -> Result<(), String> + Send + Sync> =
                        Arc::from(ready.current);
                    let intent = Arc::clone(&policy);
                    admit(
                        sys,
                        ready.ctx,
                        scoped.clone(),
                        Box::new(move || intent()),
                        Box::new(move || policy()),
                    )
                }
                other => Ok(other),
            });
            let calls = Arc::clone(&choices);
            item.toolset = Toolset::new(
                Box::new(|| Found::Missing("Git is missing".into())),
                Box::new(move |args, _, stopped| {
                    calls.calls.lock().unwrap().push(args.join(" "));
                    assert!(!stopped(), "unchanged admission continues");
                    match change {
                        "source" => {
                            aterm_uds::atomic_file::write(
                            &source,
                            b"gh_access_token=fixture-two\ngithub-repositories.json={\"schema\":2,\"account\":\"fixture\",\"repositories\":[]}\n",
                            0o600,
                        )
                        .unwrap();
                            client::sync(&paths, None).unwrap();
                            assert!(stopped(), "changed recorded inputs cancel the child");
                        }
                        "file" => {
                            std::fs::write(paths.home.join(".secrets/gh_access_token"), b"changed")
                                .unwrap();
                            assert!(!stopped(), "continuation does not grant file integrity");
                        }
                        "writer" => {
                            let writer = client::test_hold_state_lock(&paths).unwrap();
                            assert!(
                                stopped(),
                                "a busy writer cancels without waiting for release"
                            );
                            drop(writer);
                            assert!(
                                !stopped(),
                                "the unchanged source is observable after release"
                            );
                            return Err(aterm_machines::port::RunFailed::TimedOut(
                                std::time::Duration::ZERO,
                            ));
                        }
                        "stop" => {
                            calls.holds.lock().unwrap().stopped = Some("setup stopped".into());
                            assert!(stopped(), "Stop is checked during the child");
                        }
                        _ => unreachable!(),
                    }
                    Ok(Ran::ok(""))
                }),
                None,
                Atpkg {
                    update_flags: &[],
                    left_for_next_pass: |_| Vec::new(),
                },
            ).default_selected("git");
            let probe = item.probe();
            assert!(matches!(probe, Probe::Change { .. }), "{change}: {probe:?}");
            let result = item.apply();
            assert_eq!(
                *choices.calls.lock().unwrap(),
                ["update --default-selected git"],
                "{change}"
            );
            if change == "writer" {
                assert!(
                    result.is_err(),
                    "cancelled package attempt remains unfinished"
                );
                assert!(matches!(item.probe(), Probe::Change { .. }));
            } else {
                result.unwrap();
                assert!(matches!(item.stood_aside(), Some(StoodAside::Waiting(_))));
                assert!(
                    matches!(item.probe(), Probe::Deferred { .. }),
                    "the full boundary refuses {change} after the owned selected update"
                );
            }
        }

        // The real inventory can precede its credential. A package refusal must not
        // swallow that independent intent, and recording it starts no package or Git child.
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let paths = Paths::for_home(&home);
        let source = root.path().join("inventory.secrets");
        let payload = String::from_utf8(inventory_bytes(Some(&[]))).unwrap();
        aterm_uds::atomic_file::write(
            &source,
            format!("github-repositories.json={payload}\n").as_bytes(),
            0o600,
        )
        .unwrap();
        client::join(
            &paths,
            &client::JoinOptions {
                source: source.to_string_lossy().into_owned(),
                host: Some("fixture".into()),
                interactive: false,
                wait: std::time::Duration::ZERO,
                confirm: &|_| false,
                password: client::JoinPassword::Given(None),
                fetched: None,
                requested: &|| {},
                say: &|_| {},
            },
        )
        .unwrap();
        let (sys, choices, mut item) = fixture_at(home.to_str().unwrap());
        sys.stat_of(
            home.to_str().unwrap(),
            aterm_machines::port::Stat {
                uid: 501,
                gid: 20,
                mode: 0o700,
                dir: true,
                symlink: false,
                len: 0,
            },
        );
        sys.simulate(|_, command| panic!("credential intent started {}", command.display()));
        let ctx = super::super::repository_context(sys.as_ref()).unwrap();
        let checked_sys = Arc::clone(&sys);
        let checked_choices = Arc::clone(&choices);
        let checked_paths = paths.clone();
        let expected = ctx.clone();
        let intent: Arc<dyn Fn() -> Result<(), String> + Send + Sync> = Arc::new(move || {
            check_scope(
                checked_sys.as_ref(),
                &expected,
                &checked_paths,
                None,
                &checked_choices.holds.lock().unwrap(),
                Ok(true),
            )
            .map_err(Gate::why)
        });
        let capture_paths = paths.clone();
        item.capture = Box::new(move |system| {
            let current = Arc::clone(&intent);
            admit(
                system,
                ctx.clone(),
                capture_paths.clone(),
                Box::new(move || current()),
                Box::new(|| Err("package installation is off".into())),
            )
        });
        assert!(matches!(item.probe(), Probe::Change { .. }));
        assert!(aterm_secrets::need::peek(&paths).unwrap().is_empty());
        assert!(
            item.apply().is_err(),
            "this private fixture grants no credential-release policy"
        );
        assert_eq!(aterm_secrets::need::peek(&paths).unwrap().len(), 1);
        assert!(choices.calls.lock().unwrap().is_empty());
        choices
            .holds
            .lock()
            .unwrap()
            .off
            .set(Setting::Repositories, true);
        assert!(matches!(item.probe(), Probe::Deferred { .. }));
        assert_eq!(aterm_secrets::need::peek(&paths).unwrap().len(), 1);
    }

    #[test]
    fn active_toolset_without_the_git_ready_marker_still_gets_a_package_attempt() {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let (sys, choices, mut item) = fixture_at(home.to_str().unwrap());
        let layout = atpkg::store::Layout {
            prefix: home.join("Library/Application Support/aterm/pkg"),
        };
        let build = layout.build_dir("git", 17);
        let payload = build.join("bin/git");
        std::fs::create_dir_all(payload.parent().unwrap()).unwrap();
        std::fs::write(&payload, b"fixture payload, never executed").unwrap();
        atpkg::install_shims(&layout, &build, &["git".into()], atpkg::Aliases::Off).unwrap();
        atpkg::activate_build(&layout, &build).unwrap();
        let row = atpkg::doctor::toolset_row(&layout).unwrap();
        assert_eq!(
            row.outcome,
            aterm_converge::Outcome::Ok,
            "actual aggregate producer"
        );
        assert_eq!(atpkg::active_builds(&layout).get("git"), Some(&17));
        assert!(!build.with_extension("ready").exists());

        // Reuse the real scratch execution layout through the injected port, with
        // no fallback to the host's installed developer tools or protected state.
        for path in [&payload, &layout.bin_dir().join("git")] {
            sys.file(path.to_str().unwrap(), &std::fs::read(path).unwrap())
                .stat_of(path.to_str().unwrap(), Live::new().stat(path).unwrap());
        }
        let current = layout.program_current("git");
        sys.link(
            current.to_str().unwrap(),
            std::fs::read_link(&current).unwrap().to_str().unwrap(),
        );
        assert!(
            !aterm_machines::init_github::git_installed(sys.as_ref()),
            "actual Git selector refuses the incomplete activation"
        );
        let calls = Arc::clone(&choices);
        let online = Arc::new(AtomicBool::new(false));
        let available = Arc::clone(&online);
        let landing = Arc::clone(&sys);
        let selected_build = build.clone();
        let selected_payload = payload.clone();
        item.toolset = Toolset::new(
            Box::new(move || {
                crate::doctor::toolset_reads(
                    atpkg::doctor::toolset_row(&layout).as_ref(),
                    atpkg::Adoption::Consented,
                    true,
                    None,
                )
            }),
            Box::new(move |args, _, _| {
                calls.calls.lock().unwrap().push(args.join(" "));
                if !available.load(Ordering::SeqCst) {
                    return Ok(Ran::exit(69, "offline"));
                }
                if args.first().is_some_and(|verb| verb == "update") {
                    let mut stat = Live::new().stat(&selected_payload).unwrap();
                    stat.mode = 0o755;
                    landing.stat_of(selected_payload.to_str().unwrap(), stat);
                    landing.file(
                        selected_build.with_extension("ready").to_str().unwrap(),
                        b"ready",
                    );
                }
                Ok(Ran::ok(""))
            }),
            None,
            Atpkg {
                update_flags: &[],
                left_for_next_pass: |_| Vec::new(),
            },
        )
        .default_selected("git");
        assert!(
            matches!(item.probe(), Probe::Change { .. }),
            "missing Git must get one bounded attempt despite aggregate completeness"
        );
        item.apply().unwrap();
        assert_eq!(
            *choices.calls.lock().unwrap(),
            ["update --default-selected git"]
        );

        // A native Git can be installed yet incapable of the read-only object
        // operations required by this private inventory: one before 2.44, which refuses
        // `--no-lazy-fetch` and, before the 2.39.4 security release, fetches lazily whatever
        // GIT_NO_LAZY_FETCH says. Presence is not readiness.
        for path in [
            aterm_machines::items::github::CLT_GIT,
            aterm_machines::items::github::SYSTEM_GIT,
        ] {
            sys.executable(path, b"old native Git");
        }
        let native = |command: &aterm_machines::port::Cmd| {
            [
                aterm_machines::items::github::CLT_GIT,
                aterm_machines::items::github::SYSTEM_GIT,
            ]
            .iter()
            .any(|path| command.program == std::path::Path::new(path))
        };
        let old_native = move |obeys: bool| {
            move |_: &Fake, command: &aterm_machines::port::Cmd| {
                if native(command) {
                    aterm_machines::init_github::git_before_no_lazy_fetch(command, obeys)
                } else {
                    command
                        .args
                        .iter()
                        .any(|arg| arg == "--no-lazy-fetch")
                        .then(|| Ran::ok("capable Git"))
                }
            }
        };
        sys.simulate(old_native(false));
        inventory(
            sys.as_ref(),
            home.to_str().unwrap(),
            Some(&["vendor/library"]),
        );
        assert!(aterm_machines::init_github::git_installed(sys.as_ref()));
        assert!(
            matches!(item.probe(), Probe::Change { .. }),
            "installed Git must not strand required dependencies when it refuses the safe object-read capability"
        );
        // One that refuses the option but was seen to obey the variable alone (Ubuntu 24.04's
        // 2.43.0) is ready as it is, with no managed Git installed for it.
        sys.simulate(old_native(true));
        assert!(
            matches!(item.probe(), Probe::Ok { .. }),
            "a native Git shown to obey GIT_NO_LAZY_FETCH meets the inventory's requirements"
        );
        // A question whose setup failed (a full temporary folder, say) shows nothing about the
        // Git: the pass says why and asks again, and installs no Git for it.
        sys.simulate(move |fake, command| {
            if native(command) && command.args.iter().any(|arg| arg == "init") {
                return Some(Ran::exit(
                    128,
                    "fatal: cannot mkdir promisor.git: No space left on device",
                ));
            }
            old_native(true)(fake, command)
        });
        match item.probe() {
            Probe::Deferred { why } => assert!(
                why.contains("git init failed") && why.contains("No space left on device"),
                "{why}"
            ),
            probe => {
                panic!("a probe that could not be set up is not an install instruction: {probe:?}")
            }
        }
        sys.simulate(old_native(false));
        let probes = || {
            sys.calls()
                .iter()
                .filter(|call| call.contains("--no-lazy-fetch"))
                .count()
        };
        let before = probes();
        inventory(sys.as_ref(), home.to_str().unwrap(), Some(&[]));
        assert!(
            matches!(item.probe(), Probe::Ok { .. }),
            "ordinary clones need no dependency capability"
        );
        assert_eq!(probes(), before, "no unnecessary capability process");

        inventory(
            sys.as_ref(),
            home.to_str().unwrap(),
            Some(&["vendor/library"]),
        );
        sys.simulate(|_, _| None);
        assert!(
            matches!(item.probe(), Probe::Deferred { .. }),
            "a probe that could not run is not an install instruction"
        );
        let selected_home = home.clone();
        sys.simulate(move |_, command| {
            if command.program.starts_with(&selected_home) {
                command
                    .args
                    .iter()
                    .any(|arg| arg == "--no-lazy-fetch")
                    .then(|| Ran::ok("capable Git"))
            } else {
                aterm_machines::init_github::git_before_no_lazy_fetch(command, false)
            }
        });
        online.store(true, Ordering::SeqCst);
        choices.calls.lock().unwrap().clear();
        assert!(matches!(item.probe(), Probe::Change { .. }));
        item.apply().unwrap();
        assert_eq!(
            *choices.calls.lock().unwrap(),
            ["update --default-selected git"]
        );
        assert!(
            matches!(item.probe(), Probe::Ok { .. }),
            "normal package update selects capable managed Git"
        );
    }

    #[test]
    fn package_exclusion_removal_and_unreadable_choices_are_not_overridden() {
        let root = aterm_tempfile::tempdir().unwrap();
        let layout = atpkg::store::Layout {
            prefix: root.path().join("packages"),
        };
        std::fs::create_dir_all(&layout.prefix).unwrap();
        for text in [
            "[packages]\nenabled=false\n",
            "[packages]\nauto_install=false\n",
            "[packages]\nexclude=['git']\n",
        ] {
            assert!(matches!(
                packages_in(&atpkg::config::parse_packages(text), Some(&layout)),
                Err(Gate::Off(_))
            ));
        }
        std::fs::write(layout.removed(), b"git\n").unwrap();
        assert!(matches!(
            packages_in(&atpkg::config::PackagesConfig::default(), Some(&layout)),
            Err(Gate::Off(_))
        ));
        std::fs::remove_file(layout.removed()).unwrap();
        std::fs::create_dir(layout.removed()).unwrap();
        assert!(matches!(
            packages_in(&atpkg::config::PackagesConfig::default(), Some(&layout)),
            Err(Gate::Unread(_))
        ));
    }
}
