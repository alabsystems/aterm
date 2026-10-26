// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `aterm pkg repair` steps as converge items ([`Pass::Repair`], [`Pass::Doctor`], [`Ground`]).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aterm_converge::{Class, Explain, Item, Layer, Probe, Remedy};

use crate::store::Layout;

/// `base.shell.copied-commands`.
pub const COPIED_COMMANDS: &str = super::COPIED_COMMANDS;
/// `base.shell.links`.
pub const LINKS: &str = "base.shell.links";
/// `base.shell.hooks`.
pub const HOOKS: &str = super::HOOKS;
/// `base.shell.rc` ([`crate::hooks::login_rc`]).
pub const RC: &str = super::RC;
/// `base.pkg.stubs`.
pub const STUBS: &str = super::STUBS;
/// `base.pkg.rustup.seam`.
pub const RUSTUP_SEAM: &str = super::RUSTUP_SEAM;
/// `base.pkg.rustup.local`.
pub const RUSTUP_LOCAL: &str = super::RUSTUP_LOCAL;
/// `base.pkg.held-update`.
pub const HELD_UPDATE: &str = super::HELD_UPDATE;
/// `base.pkg.shims`.
pub const SHIMS: &str = super::SHIMS;
/// `base.pkg.exec-roots`.
pub const EXEC_ROOTS: &str = super::EXEC_ROOTS;
/// `base.pkg.store`: every installed program's build on disk and not withdrawn.
pub const STORE: &str = super::STORE;

/// `base.shell.rc`'s reading when every rc that exists is wired.
pub const RC_ALL_WIRED: &str = "every rc that exists sources ~/.aterm/shell.d";

/// Every repair item's id, in run order.
pub const IDS: [&str; 11] = [
    COPIED_COMMANDS,
    LINKS,
    HOOKS,
    RC,
    STUBS,
    RUSTUP_SEAM,
    RUSTUP_LOCAL,
    HELD_UPDATE,
    SHIMS,
    EXEC_ROOTS,
    STORE,
];

/// Which surface runs the items, and so the one act that differs between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    /// `aterm pkg repair` ([`Item::refresh`]).
    Repair,
    /// Every other pass.
    Doctor,
}

/// What every repair item reads and writes.
#[derive(Debug, Clone)]
pub struct Ground {
    /// The store.
    pub layout: Layout,
    /// The account's home (`~/.aterm`, the rc files, `~/.local/bin`).
    pub home: Option<PathBuf>,
    /// The executable running the pass, canonical.
    pub exe: Option<PathBuf>,
    /// rustup's home (`$RUSTUP_HOME`, else `~/.rustup`).
    pub rustup_home: Option<PathBuf>,
    /// The rc the account's login shell reads ([`crate::hooks::login_rc`]).
    pub login_rc: Option<&'static str>,
    /// Which surface runs the items.
    pub pass: Pass,
    /// What the items read once and share for this pass.
    pub reads: Reads,
}

/// One active Trust build's exec root as [`crate::compat::inspect`] reads it.
type Inspected = Option<Result<crate::compat::Inspection, String>>;

/// What one pass's items read once and share: the shim plan ([`shim_plan`], which the shims and
/// store steps both weigh) and the active Trust build's exec root ([`crate::compat::inspect`],
/// which the exec-roots step and the packages read both walk, `aterm doctor` reading them on two
/// threads at once). An apply or a refresh forgets both, so whatever reads after a change reads
/// afresh. A clone starts empty.
pub struct Reads {
    plan: Mutex<Option<Arc<ShimPlan>>>,
    inspected: Mutex<std::collections::BTreeMap<u64, Arc<std::sync::OnceLock<Inspected>>>>,
    inspect: fn(&Layout, u64) -> Inspected,
}

impl Default for Reads {
    fn default() -> Self {
        Self::reading(crate::compat::inspect)
    }
}

impl Clone for Reads {
    fn clone(&self) -> Self {
        Self::reading(self.inspect)
    }
}

impl std::fmt::Debug for Reads {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reads").finish_non_exhaustive()
    }
}

impl Reads {
    /// Reads whose exec-root walks go through `inspect`: [`crate::compat::inspect`], or a test's
    /// counter.
    #[must_use]
    pub fn reading(inspect: fn(&Layout, u64) -> Inspected) -> Self {
        Self {
            plan: Mutex::new(None),
            inspected: Mutex::new(std::collections::BTreeMap::new()),
            inspect,
        }
    }

    /// `build`'s exec root, walked once however many readers ask, at once or after: a reader
    /// that asks while another walks it waits for that walk.
    #[must_use]
    pub fn inspect(&self, layout: &Layout, build: u64) -> Inspected {
        let once = Arc::clone(
            self.inspected
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entry(build)
                .or_default(),
        );
        once.get_or_init(|| (self.inspect)(layout, build)).clone()
    }

    /// The shim plan, weighed once.
    fn plan(&self, layout: &Layout) -> Arc<ShimPlan> {
        let mut plan = self
            .plan
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(plan.get_or_insert_with(|| Arc::new(shim_plan(layout))))
    }

    /// Forget everything read: something changed the store.
    pub fn forget(&self) {
        *self
            .plan
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        self.inspected
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}

impl Ground {
    /// The ground of this process.
    #[must_use]
    pub fn of_process(layout: Layout, pass: Pass) -> Self {
        let home = aterm_types::dirs::home_dir();
        Self {
            layout,
            login_rc: home.as_deref().and_then(crate::hooks::account_login_rc),
            home,
            exe: std::env::current_exe()
                .and_then(|exe| exe.canonicalize())
                .ok(),
            rustup_home: crate::seam::rustup_home(),
            pass,
            reads: Reads::default(),
        }
    }
}

/// The repair items over `ground`, in run order ([`IDS`]).
#[must_use]
pub fn items(ground: &Arc<Ground>) -> Vec<Box<dyn Item>> {
    [
        Step::CopiedCommands,
        Step::Links,
        Step::Hooks,
        Step::Rc,
        Step::Stubs,
        Step::RustupSeam,
        Step::RustupLocal,
        Step::HeldUpdate,
        Step::Shims,
        Step::ExecRoots,
        Step::Store,
    ]
    .into_iter()
    .map(|step| {
        Box::new(Repair {
            step,
            ground: Arc::clone(ground),
            said: Mutex::new(Vec::new()),
        }) as Box<dyn Item>
    })
    .collect()
}

/// One of repair's steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    CopiedCommands,
    Links,
    Hooks,
    Rc,
    Stubs,
    RustupSeam,
    RustupLocal,
    HeldUpdate,
    Shims,
    ExecRoots,
    Store,
}

struct Repair {
    step: Step,
    ground: Arc<Ground>,
    /// What the apply or the refresh did, one line each: the row's detail.
    said: Mutex<Vec<String>>,
}

impl Repair {
    /// The step's change, which [`Item::apply`] follows by forgetting the pass's reads.
    fn apply_step(&self) -> Result<(), String> {
        match self.step {
            Step::CopiedCommands => self.apply_copied(),
            Step::Links => self.apply_links(),
            Step::Hooks => crate::hooks::lay_hooks(self.layout(), self.home()?),
            Step::Rc => self.apply_rc(),
            Step::Stubs => self.apply_stubs(),
            Step::RustupSeam => self.apply_seam(false),
            Step::RustupLocal => self.apply_seam(true),
            Step::HeldUpdate => {
                if crate::quiet::end_outlived(self.layout()) {
                    self.say([
                        "ended the record of a held Trust toolchain update that outlived every \
                         build it moves off"
                            .to_owned(),
                    ]);
                }
                Ok(())
            }
            Step::Shims => self.apply_shims(),
            Step::ExecRoots => self.apply_exec_roots(),
            Step::Store => Err(
                "only an install puts a build back: `aterm pkg install --default-set`".to_owned(),
            ),
        }
    }

    fn say(&self, lines: impl IntoIterator<Item = String>) {
        self.said
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(lines);
    }

    fn layout(&self) -> &Layout {
        &self.ground.layout
    }

    fn reruns(&self) -> bool {
        self.ground.pass == Pass::Repair
            && matches!(self.step, Step::Shims | Step::ExecRoots | Step::RustupSeam)
    }

    fn home(&self) -> Result<&Path, String> {
        self.ground
            .home
            .as_deref()
            .ok_or_else(|| "$HOME is unknown".to_owned())
    }
}

fn change(from: impl Into<String>, to: impl Into<String>) -> Probe {
    Probe::Change {
        from: from.into(),
        to: to.into(),
    }
}

fn ok(now: impl Into<String>) -> Probe {
    Probe::Ok { now: now.into() }
}

fn not_applicable(why: impl Into<String>) -> Probe {
    Probe::NotApplicable { why: why.into() }
}

fn blocked(why: impl Into<String>) -> Probe {
    Probe::Blocked { why: why.into() }
}

fn shown(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

impl Item for Repair {
    fn id(&self) -> String {
        match self.step {
            Step::CopiedCommands => COPIED_COMMANDS,
            Step::Links => LINKS,
            Step::Hooks => HOOKS,
            Step::Rc => RC,
            Step::Stubs => STUBS,
            Step::RustupSeam => RUSTUP_SEAM,
            Step::RustupLocal => RUSTUP_LOCAL,
            Step::HeldUpdate => HELD_UPDATE,
            Step::Shims => SHIMS,
            Step::ExecRoots => EXEC_ROOTS,
            Step::Store => STORE,
        }
        .to_owned()
    }

    fn family(&self) -> &'static str {
        match self.step {
            Step::CopiedCommands | Step::Links | Step::Hooks | Step::Rc => "shell",
            _ => "pkg",
        }
    }

    fn layer(&self) -> Layer {
        Layer::Base
    }

    fn class(&self) -> Class {
        match self.step {
            Step::CopiedCommands | Step::RustupLocal => Class::Asked,
            Step::Rc if self.ground.pass == Pass::Repair => Class::Asked,
            Step::Store => Class::Install,
            _ => Class::Relay,
        }
    }

    fn title(&self) -> String {
        match self.step {
            Step::CopiedCommands => "copied commands in ~/.local/bin",
            Step::Links => "the ~/.local/bin links to the installed aterm",
            Step::Hooks => "the ~/.aterm/shell.d hooks",
            Step::Rc => "atpkg's block in your shell rc files",
            Step::Stubs => "the reroute stubs",
            Step::RustupSeam => "rustup's trust entry",
            Step::RustupLocal => "a stale rustup trust link",
            Step::HeldUpdate => "the held Trust toolchain update",
            Step::Shims => "the installed programs' commands",
            Step::ExecRoots => "the trust build's exec root",
            Step::Store => "the installed programs' builds",
        }
        .to_owned()
    }

    fn probe(&self) -> Probe {
        let probe = match self.step {
            Step::CopiedCommands => self.probe_copied(),
            Step::Links => self.probe_links(),
            Step::Hooks => self.probe_hooks(),
            Step::Rc => self.probe_rc(),
            Step::Stubs => self.probe_stubs(),
            Step::RustupSeam => self.probe_seam(),
            Step::RustupLocal => self.probe_local(),
            Step::HeldUpdate => self.probe_held(),
            Step::Shims => self.probe_shims(),
            Step::ExecRoots => self.probe_exec_roots(),
            Step::Store => self.probe_store(),
        };
        match probe {
            Probe::Unusable { why } if self.reruns() => change(why, "put in order by the step"),
            probe => probe,
        }
    }

    fn apply(&self) -> Result<(), String> {
        let applied = self.apply_step();
        self.ground.reads.forget();
        applied
    }

    fn refresh(&self) -> Option<Result<(), String>> {
        if !self.reruns() {
            return None;
        }
        let refreshed = match self.step {
            Step::Shims => Some(self.apply_shims()),
            Step::ExecRoots => Some(self.apply_exec_roots()),
            Step::RustupSeam => Some(self.apply_seam(false)),
            _ => None,
        };
        self.ground.reads.forget();
        refreshed
    }

    fn refresh_changes(&self) -> bool {
        self.reruns()
    }

    fn explain(&self, probe: &Probe) -> Explain {
        let why = match probe {
            Probe::Ok { now } => now.clone(),
            Probe::Change { from, to } => format!("{from}; repair makes it {to}"),
            Probe::Person { how, .. } => how.clone(),
            Probe::Blocked { why }
            | Probe::Waits { why, .. }
            | Probe::NotApplicable { why }
            | Probe::Deferred { why }
            | Probe::Unusable { why } => why.clone(),
        };
        let remedy = match (self.step, probe) {
            (Step::Store, _) if why.contains("withdrew") => {
                Remedy::Command("aterm pkg update".to_owned())
            }
            (Step::Store, _) => Remedy::Command("aterm pkg install --default-set".to_owned()),
            #[cfg(unix)]
            (Step::CopiedCommands, Probe::Blocked { .. }) => self
                .ground
                .home
                .as_deref()
                .and_then(|home| crate::hooks::copied_command_links(home).into_iter().next())
                .map_or(Remedy::None, |path| {
                    Remedy::Command(crate::hooks::copied_command_remedy(&path))
                }),
            (_, Probe::NotApplicable { .. } | Probe::Ok { .. }) => Remedy::None,
            _ => Remedy::Repair,
        };
        Explain { why, remedy }
    }

    fn after(&self) -> Vec<String> {
        match self.step {
            Step::Rc => vec![HOOKS.to_owned()],
            _ => Vec::new(),
        }
    }

    fn detail(&self) -> Option<String> {
        let said = self
            .said
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (!said.is_empty()).then(|| said.join("\n"))
    }
}

impl Repair {
    #[cfg(unix)]
    fn probe_copied(&self) -> Probe {
        let Ok(home) = self.home() else {
            return not_applicable("$HOME is unknown, so ~/.local/bin cannot be read");
        };
        let copies = crate::hooks::copied_command_links(home);
        if copies.is_empty() {
            return ok("no copied command in ~/.local/bin");
        }
        let from = format!(
            "{} {} a file, not a link to the installed aterm",
            shown(&copies),
            if copies.len() == 1 { "is" } else { "are" }
        );
        match self
            .ground
            .exe
            .as_deref()
            .ok_or("this aterm could not find its own path")
            .and_then(crate::hooks::command_link_check)
        {
            Ok(_) => change(from, "moved aside, the installed app linked in its place"),
            Err(why) => blocked(if cfg!(target_os = "macos") {
                format!("{from}, and this aterm does not replace it ({why})")
            } else {
                format!("{from}, and no aterm here replaces it")
            }),
        }
    }

    #[cfg(not(unix))]
    fn probe_copied(&self) -> Probe {
        not_applicable("no command links off unix")
    }

    #[cfg(unix)]
    fn apply_copied(&self) -> Result<(), String> {
        let home = self.home()?;
        let exe = self
            .ground
            .exe
            .as_deref()
            .ok_or("this aterm could not find its own path")?;
        let secs = aterm_time::unix_secs().unwrap_or(0);
        let outcome = crate::hooks::move_aside_copied_commands_at(home, exe, secs)?;
        self.say(outcome.moved.iter().map(|moved| {
            format!(
                "moved {} aside to {} — a file, not a link to the app{}",
                moved.from.display(),
                moved.to.display(),
                if moved.linked {
                    " — and linked the app in its place"
                } else {
                    "; nothing takes its place, `aterm ctl` replaces it"
                }
            )
        }));
        if outcome.failed.is_empty() {
            Ok(())
        } else {
            Err(outcome
                .failed
                .iter()
                .map(|(path, why)| format!("could not move {} aside: {why}", path.display()))
                .collect::<Vec<_>>()
                .join("; "))
        }
    }

    #[cfg(not(unix))]
    fn apply_copied(&self) -> Result<(), String> {
        Ok(())
    }

    /// The links the pass would write ([`crate::hooks::command_links_to_write`]).
    #[cfg(unix)]
    fn links_to_write(&self) -> Vec<(PathBuf, Option<PathBuf>)> {
        let Some(exe) = self.ground.exe.as_deref() else {
            return Vec::new();
        };
        let mut writes = self
            .ground
            .home
            .as_deref()
            .map(|home| crate::hooks::command_links_to_write(home, exe))
            .unwrap_or_default();
        if crate::hooks::HEALS_STORE {
            writes.extend(
                crate::hooks::store_aliases_absent(exe)
                    .into_iter()
                    .map(|alias| (alias, None)),
            );
        }
        writes
    }

    #[cfg(unix)]
    fn probe_links(&self) -> Probe {
        let writes = self.links_to_write();
        if !writes.is_empty() {
            let at: Vec<String> = writes
                .iter()
                .map(|(link, old)| match old {
                    None => format!("nothing at {}", link.display()),
                    Some(old) => format!("{} -> {}", link.display(), old.display()),
                })
                .collect();
            return change(at.join(", "), "a link to the installed aterm");
        }
        let lays = self
            .ground
            .exe
            .as_deref()
            .is_some_and(|exe| crate::hooks::command_link_source(exe).is_some());
        if lays {
            ok("the ~/.local/bin links are in place")
        } else {
            not_applicable(
                "only the installed release aterm.app lays the ~/.local/bin links, and this \
                 aterm is not it",
            )
        }
    }

    #[cfg(not(unix))]
    fn probe_links(&self) -> Probe {
        not_applicable("no command links off unix")
    }

    #[cfg(unix)]
    fn apply_links(&self) -> Result<(), String> {
        let exe = self
            .ground
            .exe
            .as_deref()
            .ok_or("this aterm could not find its own path")?;
        let writes = self.links_to_write();
        if let Some(home) = self.ground.home.as_deref() {
            crate::hooks::ensure_command_links_from(home, exe);
        }
        if crate::hooks::HEALS_STORE {
            crate::hooks::heal_store_aliases(exe);
        }
        let written: Vec<PathBuf> = writes
            .into_iter()
            .filter(|(link, old)| {
                std::fs::read_link(link).is_ok_and(|now| old.as_ref() != Some(&now))
            })
            .map(|(link, _)| link)
            .collect();
        if !written.is_empty() {
            self.say([format!("linked {}", shown(&written))]);
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn apply_links(&self) -> Result<(), String> {
        Ok(())
    }

    fn other_bundle(&self) -> Option<&'static str> {
        (self.ground.pass == Pass::Doctor
            && self
                .ground
                .exe
                .as_deref()
                .is_some_and(crate::hooks::is_non_release_bundle_exe))
        .then_some(
            "this aterm runs from an app bundle that is not the installed aterm.app, so it \
             lays no shell integration (`aterm pkg repair` does, from anywhere)",
        )
    }

    fn probe_hooks(&self) -> Probe {
        if let Some(why) = self.other_bundle() {
            return ok(format!("left as they are: {why}"));
        }
        let Ok(home) = self.home() else {
            return blocked("$HOME is unknown, so ~/.aterm/shell.d cannot be found");
        };
        let loose = crate::hooks::shell_dirs_not_private(home);
        let stale = crate::hooks::hooks_not_current(self.layout(), home);
        let mut from = Vec::new();
        if !loose.is_empty() {
            from.push(format!(
                "{} not your own private (0700) directory",
                shown(&loose)
            ));
        }
        if !stale.is_empty() {
            from.push(format!(
                "~/.aterm/shell.d: {} not as atpkg lays it",
                stale.join(", ")
            ));
        }
        if from.is_empty() {
            ok("~/.aterm/shell.d hooks in place")
        } else {
            change(
                from.join("; "),
                "the hooks atpkg lays, in your own private (0700) directories",
            )
        }
    }

    /// The absent login rc ([`Ground::login_rc`], [`crate::hooks::absent_login_rc`]).
    #[cfg(unix)]
    fn absent_login_rc(&self, home: &Path) -> Option<(&'static str, crate::hooks::RcState)> {
        let rc = self.ground.login_rc?;
        crate::hooks::absent_login_rc(home, rc).map(|state| (rc, state))
    }

    /// The rc files a pass of this kind would wire now.
    #[cfg(unix)]
    fn rc_to_wire(&self, home: &Path) -> Vec<&'static str> {
        use crate::hooks::RcState;
        crate::hooks::rc_wiring(home)
            .into_iter()
            .chain(self.absent_login_rc(home))
            .filter(|(rc, state)| {
                crate::hooks::RC_FILES.iter().any(|(row, _)| row == rc)
                    && match state {
                        RcState::Unwired => true,
                        RcState::OptedOut => self.ground.pass == Pass::Repair,
                        RcState::Wired | RcState::SourcedElsewhere | RcState::ConsentFenced => {
                            false
                        }
                    }
            })
            .map(|(rc, _)| rc)
            .collect()
    }

    /// The rc block may name only the current hooks in this account's private directories.
    #[cfg(unix)]
    fn require_current_hooks(&self) -> Result<(), String> {
        let home = self.home()?;
        if crate::hooks::shell_dirs_not_private(home).is_empty()
            && crate::hooks::hooks_not_current(self.layout(), home).is_empty()
        {
            Ok(())
        } else {
            Err(
                "the shell hooks are not current in your own private (0700) directories; \
                 restore them first with `aterm doctor --only base.shell.hooks` or \
                 `aterm pkg repair`"
                    .to_owned(),
            )
        }
    }

    #[cfg(unix)]
    fn probe_rc(&self) -> Probe {
        use crate::hooks::RcState;
        if let Some(why) = self.other_bundle() {
            return not_applicable(why);
        }
        let Ok(home) = self.home() else {
            return blocked("$HOME is unknown, so no rc file can be read");
        };
        let absent = self.absent_login_rc(home);
        let wire = self.rc_to_wire(home);
        if !wire.is_empty() {
            if let Err(why) = self.require_current_hooks() {
                return Probe::Waits {
                    on: HOOKS.to_owned(),
                    why,
                };
            }
            let (create, edit): (Vec<&str>, Vec<&str>) = wire
                .iter()
                .partition(|rc| absent.is_some_and(|(login, _)| login == **rc));
            let names = |rcs: &[&str]| -> String {
                rcs.iter()
                    .map(|rc| format!("~/{rc}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let mut from = Vec::new();
            let mut to = Vec::new();
            if !edit.is_empty() {
                from.push(format!("{} does not source ~/.aterm/shell.d", names(&edit)));
                to.push(format!("atpkg's block in {}", names(&edit)));
            }
            if !create.is_empty() {
                from.push(format!(
                    "{} does not exist, and your login shell reads it",
                    names(&create)
                ));
                to.push(format!("{} holding atpkg's block", names(&create)));
            }
            return change(from.join("; "), to.join("; "));
        }
        let wiring = crate::hooks::rc_wiring(home);
        let stands = crate::hooks::RC_FILES
            .iter()
            .any(|(rc, _)| std::fs::symlink_metadata(home.join(rc)).is_ok());
        if wiring.is_empty() && !stands && absent.is_none() {
            let names: Vec<String> = crate::hooks::RC_FILES
                .iter()
                .map(|(rc, _)| format!("~/{rc}"))
                .collect();
            return not_applicable(format!(
                "none of {} exists and your login shell reads none of them, so no rc is wired \
                 (atpkg creates only the login shell's own rc)",
                names.join(", ")
            ));
        }
        let mut now: Vec<String> = Vec::new();
        if let Some((rc, state)) = absent {
            match state {
                RcState::OptedOut => now.push(format!(
                    "~/{rc} left absent — you deleted the one atpkg made for your login shell \
                     (`aterm pkg repair` makes it again)"
                )),
                RcState::ConsentFenced => now.push(format!(
                    "~/{rc} not made — its folder resolves under one macOS guards with a consent \
                     dialog, which no atpkg pass opens; source ~/.aterm/shell.d from your login \
                     shell's rc yourself, or use the export line `aterm pkg doctor` prints"
                )),
                RcState::Wired | RcState::SourcedElsewhere | RcState::Unwired => {}
            }
        }
        for (rc, state) in &wiring {
            match state {
                RcState::OptedOut => now.push(format!(
                    "~/{rc} left alone — you deleted atpkg's block (`aterm pkg repair` lays it \
                     again)"
                )),
                RcState::ConsentFenced => now.push(format!(
                    "~/{rc} left alone — it resolves under a folder macOS guards with a consent \
                     dialog (Documents, Desktop, Downloads, iCloud Drive, a sync provider's \
                     folder, a mounted volume), which no atpkg pass opens; source \
                     ~/.aterm/shell.d from it yourself, or use the export line `aterm pkg \
                     doctor` prints"
                )),
                RcState::Wired | RcState::SourcedElsewhere | RcState::Unwired => {}
            }
        }
        for (rc, _) in crate::hooks::RC_FILES {
            if std::fs::symlink_metadata(home.join(rc)).is_ok()
                && !wiring.iter().any(|(wired, _)| wired == &rc)
            {
                now.push(format!(
                    "~/{rc} left alone — atpkg edits only a regular file it can read as UTF-8 \
                     (a dangling link, a directory, a FIFO or device, or other bytes is never \
                     touched)"
                ));
            }
        }
        if now.is_empty() {
            ok(RC_ALL_WIRED)
        } else {
            ok(now.join("; "))
        }
    }

    #[cfg(not(unix))]
    fn probe_rc(&self) -> Probe {
        not_applicable("no rc file is wired off unix")
    }

    #[cfg(unix)]
    fn apply_rc(&self) -> Result<(), String> {
        use crate::hooks::{RcOutcome, RcWiring};
        let home = self.home()?;
        self.require_current_hooks()?;
        let wiring = match self.ground.pass {
            Pass::Repair => RcWiring::Rewire,
            Pass::Doctor => RcWiring::HonorOptOut,
        };
        let mut failed = Vec::new();
        for (rc, outcome) in
            crate::hooks::ensure_rc_sources_hooks(home, wiring, self.ground.login_rc)
        {
            match outcome {
                RcOutcome::Appended => self.say([format!(
                    "appended atpkg's block to ~/{rc} (it sources ~/.aterm/shell.d; delete the \
                     block to opt out)"
                )]),
                RcOutcome::Created => self.say([format!(
                    "created ~/{rc}, the rc your login shell reads, holding atpkg's block (it \
                     sources ~/.aterm/shell.d; delete the block or the file to opt out)"
                )]),
                RcOutcome::RelaidOverOptOut => self.say([format!(
                    "re-laid atpkg's block in ~/{rc} — you had deleted it; delete it again to \
                     opt out (install and update passes then leave it deleted)"
                )]),
                RcOutcome::WriteFailed => failed.push(format!(
                    "~/{rc} could NOT be rewritten (the file or its directory refused the write)"
                )),
                RcOutcome::AlreadyWired
                | RcOutcome::OptOutKept
                | RcOutcome::ConsentFenced
                | RcOutcome::Unreadable => {}
            }
        }
        if failed.is_empty() {
            Ok(())
        } else {
            Err(failed.join("; "))
        }
    }

    #[cfg(not(unix))]
    fn apply_rc(&self) -> Result<(), String> {
        Ok(())
    }
}

impl Repair {
    fn stubs_pending(&self) -> Vec<String> {
        crate::reroute::lay_pending(
            self.layout(),
            &crate::stub::Embedder::this_process(),
            crate::config::load_reroute().strict(),
        )
    }

    fn probe_stubs(&self) -> Probe {
        if cfg!(windows) {
            return not_applicable("Windows lays no reroute stubs yet");
        }
        if self.layout().declined().is_file() {
            return not_applicable(
                "no reroute stub is laid: the toolset was removed on purpose (`aterm pkg \
                 install --default-set` brings it back)",
            );
        }
        let pending = self.stubs_pending();
        if pending.is_empty() {
            ok("the reroute stubs are laid")
        } else {
            change(
                format!("reroute stubs not as laid: {}", pending.join(", ")),
                "laid",
            )
        }
    }

    fn apply_stubs(&self) -> Result<(), String> {
        let pending = self.stubs_pending();
        crate::reroute::lay(self.layout()).map_err(|err| {
            format!(
                "reroute stubs not laid under {}: {err}",
                crate::reroute::dir(self.layout()).display()
            )
        })?;
        if !pending.is_empty() {
            self.say([format!("laid the reroute stubs: {}", pending.join(", "))]);
        }
        Ok(())
    }

    fn probe_seam(&self) -> Probe {
        use crate::seam::Entry;
        let Some(rustup_home) = self.ground.rustup_home.as_deref() else {
            return not_applicable("no rustup home here");
        };
        let layout = self.layout();
        let status = crate::seam::status(layout, rustup_home, crate::seam::DEFAULT_SEAM);
        if !status.rustup_present {
            return not_applicable("no rustup here (no toolchains directory)");
        }
        match &status.entry {
            Ok(Entry::Link(_)) if status.targets_view => ok("rustup `trust` -> the managed view"),
            Ok(Entry::Link(_)) if status.in_prefix => change(
                "rustup `trust` -> inside the store, not the view atpkg lays",
                "a link to the managed view",
            ),
            Ok(Entry::Link(_) | Entry::Dir | Entry::File | Entry::Other) => {
                not_applicable(crate::doctor::seam_line(&status, layout).map_or_else(
                    || "rustup `trust` is not atpkg's; left as it stands".to_owned(),
                    |(row, _)| row.text,
                ))
            }
            Ok(Entry::Absent) => {
                let wanted = std::fs::symlink_metadata(crate::seam::store_current(layout)).is_ok()
                    || matches!(
                        crate::seam::view_source(layout),
                        crate::seam::ViewSource::Linked(_)
                    );
                if wanted {
                    change("no rustup `trust` entry", "a link to the managed view")
                } else {
                    not_applicable("no Trust toolchain installed, so no rustup `trust` entry")
                }
            }
            Err(why) => Probe::Unusable {
                why: format!(
                    "rustup `trust` at {} could not be inspected ({why})",
                    status.path.display()
                ),
            },
        }
    }

    fn probe_local(&self) -> Probe {
        let Some(rustup_home) = self.ground.rustup_home.as_deref() else {
            return not_applicable("no rustup home here");
        };
        match crate::doctor::probe_local_seal_at(self.layout(), rustup_home) {
            Some(seal) => match seal.stale {
                Some(stale) => change(
                    format!("rustup `trust` -> {} ({stale})", seal.link_target.display()),
                    "the managed store's toolchain",
                ),
                None => not_applicable(format!(
                    "rustup `trust` names a local toolchain that is not stale ({}); yours to keep",
                    seal.link_target.display()
                )),
            },
            None => ok("rustup `trust` names no stale local toolchain"),
        }
    }

    fn apply_seam(&self, stale: bool) -> Result<(), String> {
        let Some(rustup_home) = self.ground.rustup_home.as_deref() else {
            return Ok(());
        };
        let events = if stale {
            crate::seam::repair(self.layout(), rustup_home)
        } else {
            crate::seam::reassert(self.layout(), rustup_home)
        };
        self.say(events.iter().map(ToString::to_string));
        Ok(())
    }

    fn probe_held(&self) -> Probe {
        match crate::quiet::held(self.layout()) {
            crate::quiet::Held::Outlived(_) => change(
                "a held Trust toolchain update outlived every build it moves off",
                "no record",
            ),
            crate::quiet::Held::Waits(_) => {
                ok("a held Trust toolchain update waits for a quiet moment")
            }
            crate::quiet::Held::Nothing => ok("no held toolchain update"),
        }
    }

    fn probe_shims(&self) -> Probe {
        let plan = self.ground.reads.plan(self.layout());
        let broken: Vec<String> = plan
            .lay
            .iter()
            .flat_map(|planned| {
                shims_of(self.layout(), planned)
                    .into_iter()
                    .filter(|shim| !shim_runs(shim))
                    .filter_map(|shim| {
                        shim.file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                    })
            })
            .collect();
        let refused = crate::activate::refused_name_shims(self.layout());
        if !broken.is_empty()
            || !refused.is_empty()
            || (!plan.failed.is_empty() && self.ground.pass == Pass::Repair)
        {
            let mut from = Vec::new();
            if !broken.is_empty() {
                from.push(format!(
                    "bin/ shims missing or broken: {}",
                    broken.join(", ")
                ));
            }
            if !refused.is_empty() {
                from.push(format!(
                    "bin/ shims under a name no shim may carry: {}",
                    refused.join(", ")
                ));
            }
            from.extend(plan.failed.iter().cloned());
            return change(from.join("; "), "forwarding to the installed builds");
        }
        if let Some(why) = plan.failed.first() {
            return blocked(why.clone());
        }
        if plan.lay.is_empty() {
            ok("no installed program's commands to restore")
        } else {
            ok(format!(
                "the commands of {} program(s) forward to their builds",
                plan.lay.len()
            ))
        }
    }

    fn apply_shims(&self) -> Result<(), String> {
        let removed = crate::activate::remove_refused_name_shims(self.layout(), None);
        if !removed.is_empty() {
            self.say([format!(
                "removed bin/ shim(s) an older atpkg laid under a name no shim may carry: {}",
                removed.join(", ")
            )]);
        }
        let relaid = relay(self.layout());
        self.say(relaid.notes);
        if relaid.relaid > 0 {
            self.say([format!(
                "restored the commands of {} program(s)",
                relaid.relaid
            )]);
        }
        if relaid.failed.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "{} program(s) still have shims that could not be re-laid: {}",
                relaid.failed.len(),
                relaid.failed.join("; ")
            ))
        }
    }

    fn probe_exec_roots(&self) -> Probe {
        use crate::compat::RootState;
        let layout = self.layout();
        let Some(&build) = crate::ops::active_builds(layout).get(crate::seam::SEAM_PROGRAM) else {
            return ok("no active trust build, so no exec root");
        };
        match self.ground.reads.inspect(layout, build) {
            None => ok(format!("trust build {build} needs no exec root")),
            Some(Err(why)) => Probe::Unusable {
                why: format!("trust build {build}'s exec root could not be read ({why})"),
            },
            Some(Ok(inspection)) => match &inspection.state {
                RootState::Blocked(blocked_at) => blocked(blocked_at.to_string()),
                RootState::Absent => change(
                    format!("no exec root for trust build {build}"),
                    "an exec root every trust shim runs through",
                ),
                RootState::Differs(at) => change(
                    format!(
                        "trust build {build}'s exec root differs from the build at {}",
                        at.display()
                    ),
                    "a clone of the build",
                ),
                RootState::Matches if !inspection.unrouted.is_empty() => change(
                    format!(
                        "trust shims not routed through the exec root: {}",
                        inspection.unrouted.join(", ")
                    ),
                    "routed through it",
                ),
                RootState::Matches => ok(format!(
                    "trust build {build}'s exec root matches it, every shim routed"
                )),
            },
        }
    }

    fn apply_exec_roots(&self) -> Result<(), String> {
        let layout = self.layout();
        let report = crate::compat::reconcile(layout, crate::seam::Depth::Deep);
        self.say(
            report
                .built
                .iter()
                .map(|n| crate::compat::laid_line(layout, *n)),
        );
        if !report.routed.is_empty() {
            let names: Vec<String> = report
                .routed
                .iter()
                .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .collect();
            self.say([format!("Trust commands updated: {}", names.join(", "))]);
        }
        self.say(
            report
                .swept
                .iter()
                .map(|path| format!("removed unused build copy {}", path.display())),
        );
        if report.errors.is_empty() {
            Ok(())
        } else {
            Err(report.errors.join("; "))
        }
    }

    fn probe_store(&self) -> Probe {
        let plan = self.ground.reads.plan(self.layout());
        if plan.weighed == 0 {
            return not_applicable(
                "no programs installed — `aterm pkg install --default-set` installs the toolset",
            );
        }
        let mut why = Vec::new();
        if !plan.missing.is_empty() {
            why.push(format!(
                "{} program(s) have no build on disk and need a reinstall: {}",
                plan.missing.len(),
                plan.missing.join(", ")
            ));
        }
        for revoked in &plan.revoked {
            why.push(format!("ALab withdrew {revoked} — left disabled"));
        }
        if why.is_empty() {
            ok("every installed program's build is on disk")
        } else {
            blocked(why.join("; "))
        }
    }
}

/// One program [`relay`] re-lays.
pub(crate) struct Planned {
    pub(crate) program: String,
    pub(crate) build: u64,
    pub(crate) build_dir: PathBuf,
    pub(crate) exposes: Vec<String>,
}

/// What a re-lay of the installed programs' shims works from: one rule for the probe and the lay
/// ([`shim_plan`]).
pub(crate) struct ShimPlan {
    /// The programs to re-lay.
    pub(crate) lay: Vec<Planned>,
    /// `program@build` of each whose build is gone.
    pub(crate) missing: Vec<String>,
    /// `program@build` of each left tombstoned.
    pub(crate) revoked: Vec<String>,
    /// What was left alone and why (a dev link, a build whose every command another file holds).
    pub(crate) notes: Vec<String>,
    /// What could not even be read (a development link), or a build without a record of its
    /// commands that leaves none to re-lay.
    pub(crate) failed: Vec<String>,
    /// How many programs it weighed, whatever became of each, which leaves out a dev-linked
    /// program and one whose development link could not be read.
    pub(crate) weighed: usize,
}

/// Which builds a re-lay of the shims works from: the build ACTIVATION selects
/// (`store/<program>/current`), or the newest complete build when there is no usable link.
pub(crate) fn shim_plan(layout: &Layout) -> ShimPlan {
    let mut builds: std::collections::BTreeMap<String, std::collections::BTreeSet<u64>> =
        std::collections::BTreeMap::new();
    for (program, build) in crate::list_installed(layout) {
        builds.entry(program).or_default().insert(build);
    }
    let recorded = crate::status::read(layout)
        .map(|s| s.programs)
        .unwrap_or_default();
    let mut plan = ShimPlan {
        lay: Vec::new(),
        missing: Vec::new(),
        revoked: Vec::new(),
        notes: Vec::new(),
        failed: Vec::new(),
        weighed: 0,
    };
    if let Err(error) = crate::linkmode::linked_programs_checked(layout) {
        plan.failed
            .push(format!("development links could not be read: {error}"));
        return plan;
    }
    for (program, complete) in &builds {
        match crate::linkmode::linked_checkout_checked(layout, program) {
            Ok(Some(checkout)) => {
                plan.notes.push(format!(
                    "{program}: dev-linked from {} — store shims left alone",
                    checkout.display()
                ));
                continue;
            }
            Ok(None) => {}
            Err(error) => {
                plan.failed.push(format!(
                    "{program}: development link could not be read ({error}); shims left alone"
                ));
                continue;
            }
        }
        let Some(&newest) = complete.last() else {
            continue;
        };
        plan.weighed = plan.weighed.saturating_add(1);
        let build = std::fs::read_link(layout.program_current(program))
            .ok()
            .and_then(|t| crate::ops::store_build_of(&layout.prefix, &t))
            .filter(|(owner, b)| owner == program && complete.contains(b))
            .map_or(newest, |(_, b)| b);
        let build_dir = layout.build_dir(program, build);
        if !build_dir.exists() {
            plan.missing.push(format!("{program}@{build}"));
            continue;
        }
        let exposes = exposes(layout, program, &build_dir);
        let tombstoned = recorded
            .get(program)
            .is_some_and(|r| r.state.starts_with("tombstoned:"))
            || exposes
                .as_deref()
                .unwrap_or(std::slice::from_ref(program))
                .iter()
                .any(|tool| crate::cli::is_tombstone_shim(layout, tool));
        if tombstoned {
            plan.revoked.push(format!("{program}@{build}"));
            continue;
        }
        let Some(exposes) = exposes else {
            plan.failed.push(format!(
                "{program}@{build} — the build keeps no record of its commands and none \
                 survives under its own name to re-lay from; `aterm pkg install {program}` puts \
                 them back"
            ));
            continue;
        };
        if exposes.is_empty() {
            // An empty record (a build that puts no command on PATH) leaves nothing held to name.
            if crate::store::recorded_exposes(&build_dir).is_some_and(|r| !r.is_empty()) {
                plan.notes.push(format!(
                    "{program}@{build}: every command it exposes is held by another file — left \
                     alone"
                ));
            }
            continue;
        }
        plan.lay.push(Planned {
            program: program.clone(),
            build,
            build_dir,
            exposes,
        });
    }
    plan
}

fn shims_of(layout: &Layout, planned: &Planned) -> Vec<PathBuf> {
    let alab = crate::activate::Aliases::laid_for(layout, &planned.program)
        == crate::activate::Aliases::Alab;
    planned
        .exposes
        .iter()
        .filter_map(|name| crate::store::ToolName::new(name))
        .flat_map(|tool| {
            let alias = alab.then(|| tool.alias()).flatten();
            std::iter::once(layout.shim(&tool)).chain(alias.map(|alias| layout.shim(&alias)))
        })
        .collect()
}

fn shim_runs(shim: &Path) -> bool {
    crate::platform::resolve_shim(shim).is_some_and(|target| {
        !matches!(
            crate::store::presence(&target),
            crate::store::Presence::Absent
        )
    })
}

/// What [`relay`] did.
pub(crate) struct Relaid {
    /// How many programs were re-laid.
    pub(crate) relaid: usize,
    /// `program@build — why` of each whose shims could not be re-laid.
    pub(crate) failed: Vec<String>,
    /// What was said along the way.
    pub(crate) notes: Vec<String>,
}

/// Re-lay every installed program's shims from the build [`shim_plan`] selects.
pub(crate) fn relay(layout: &Layout) -> Relaid {
    let plan = shim_plan(layout);
    let mut relaid = Relaid {
        relaid: 0,
        failed: plan.failed,
        notes: plan.notes,
    };
    for planned in plan.lay {
        let Planned {
            program,
            build,
            build_dir,
            exposes,
        } = planned;
        let mut said = Vec::new();
        let laid = crate::activate::install_shims_env_saying(
            layout,
            &build_dir,
            &exposes,
            crate::activate::Aliases::laid_for(layout, &program),
            &crate::shim_env::read_sidecar(&build_dir),
            &mut |line| said.push(line),
        );
        relaid.notes.append(&mut said);
        match laid {
            Ok(refused) => {
                relaid.relaid = relaid.relaid.saturating_add(1);
                for name in refused {
                    relaid.notes.push(format!(
                        "{program}: refused shim {name:?} (not an allowed tool name)"
                    ));
                }
            }
            Err(e) => {
                relaid
                    .notes
                    .push(format!("{program}: could not re-lay shims: {e}"));
                relaid.failed.push(format!("{program}@{build} — {e}"));
            }
        }
    }
    relaid
}

/// What [`relay`] re-lays for `program` from `build_dir`: the commands the build records it
/// exposes ([`crate::store::recorded_exposes`]) whose shim is gone, already `program`'s or a
/// tombstone. For a build staged before that record, the names its own shims still carry,
/// plus `program` when the build ships it and its shim is gone; `None` when that names nothing.
pub(crate) fn exposes(layout: &Layout, program: &str, build_dir: &Path) -> Option<Vec<String>> {
    if let Some(tools) = crate::store::recorded_exposes(build_dir) {
        return Some(
            tools
                .into_iter()
                .filter(|tool| free_for(layout, program, tool))
                .map(|tool| tool.as_str().to_string())
                .collect(),
        );
    }
    let mut exposes = crate::installed_exposes(layout, program).unwrap_or_default();
    let own_binary = build_dir
        .join("bin")
        .join(format!("{program}{}", std::env::consts::EXE_SUFFIX))
        .is_file();
    let own_shim_absent = crate::store::ToolName::new(program).is_some_and(|tool| {
        std::fs::symlink_metadata(layout.shim(&tool))
            .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
    });
    if own_binary && own_shim_absent && !exposes.iter().any(|name| name == program) {
        exposes.push(program.to_string());
    }
    (!exposes.is_empty()).then_some(exposes)
}

/// Whether `tool`'s shim is `program`'s to lay: gone, resolving into `program`'s store, or a
/// tombstone (which the plan reports revoked) — never another program's or a file of the user's.
fn free_for(layout: &Layout, program: &str, tool: &crate::store::ToolName) -> bool {
    let shim = layout.shim(tool);
    match std::fs::symlink_metadata(&shim) {
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
        Ok(_) => crate::platform::resolve_shim(&shim).map_or_else(
            || crate::cli::is_tombstone_shim(layout, tool.as_str()),
            |target| {
                crate::ops::store_build_of(&layout.prefix, &target)
                    .is_some_and(|(owner, _)| owner == program)
            },
        ),
    }
}

#[cfg(all(test, unix))]
mod tests;
