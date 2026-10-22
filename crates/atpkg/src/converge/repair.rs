// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `aterm pkg repair`'s steps as converge items (docs/DESIGN-init-system-2026-10-20.md §3.2,
//! §4.2, S5), which bare `aterm doctor` and `aterm init` run too.
//!
//! Each item probes what its step would change, changing nothing, and its apply is the step
//! itself: the engine applies only a probe that is not `Ok` and judges the apply by a second
//! probe. So `aterm doctor --read-only` reads all of them and moves nothing, and a deleted
//! shim, a missing `~/.local/bin` link or a stale hook is restored by whichever surface runs
//! the items in apply mode.
//!
//! The two repair classes are kept apart (§3.2): `relay` lays again what a pass lays and
//! owns; `asked` moves what a person may have put there (a copied command, a rustup link
//! to an older toolchain, a stale copy of aterm) and runs only on a surface a person
//! started. The one act only `aterm pkg repair` takes is the rc block laid again over a
//! recorded opt-out ([`Pass::Repair`]); bare `doctor` leaves an opted-out rc as the person
//! left it ([`Pass::Doctor`]).
//!
//! Every item holds its own [`Ground`] (§3.1, "an item owns its ports"): the store
//! layout, the account's home, the executable that runs the pass and rustup's home, so a
//! test drives the items over a scratch prefix and home.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aterm_converge::{Class, Explain, Item, Layer, Probe, Remedy};

use crate::store::Layout;

/// `base.shell.copied-commands`: a copied `aterm`, `atpkg` or `aterm-ctl` in
/// `~/.local/bin`, moved aside.
pub const COPIED_COMMANDS: &str = super::COPIED_COMMANDS;
/// `base.shell.links`: the `~/.local/bin` links to the installed app, laid where absent.
pub const LINKS: &str = "base.shell.links";
/// `base.shell.hooks`: the `~/.aterm/shell.d` hooks.
pub const HOOKS: &str = super::HOOKS;
/// `base.shell.rc`: atpkg's block in each rc that exists, and in the login shell's own rc,
/// the one file it creates when absent ([`crate::hooks::login_rc`]).
pub const RC: &str = super::RC;
/// `base.pkg.stubs`: the reroute stubs, abrew's `brew` included.
pub const STUBS: &str = super::STUBS;
/// `base.pkg.rustup.seam`: rustup's `trust` entry as every pass re-asserts it (relay).
pub const RUSTUP_SEAM: &str = super::RUSTUP_SEAM;
/// `base.pkg.rustup.local`: a rustup `trust` link to nothing or to an older toolchain,
/// re-pointed at the store (asked).
pub const RUSTUP_LOCAL: &str = super::RUSTUP_LOCAL;
/// `base.pkg.held-update`: a held toolchain update that outlived every build it moves off.
pub const HELD_UPDATE: &str = super::HELD_UPDATE;
/// `base.pkg.shims`: each installed program's `bin/` shims.
pub const SHIMS: &str = super::SHIMS;
/// `base.pkg.exec-roots`: the trust build's exec root and the shims routed through it.
pub const EXEC_ROOTS: &str = super::EXEC_ROOTS;
/// `base.pkg.store`: every installed program's build on disk and not withdrawn — what no
/// local step fixes (install class, probe only here).
pub const STORE: &str = super::STORE;

/// `base.shell.rc`'s reading when every rc that exists is wired: the one reading `pkg
/// repair` says nothing of (any other names an rc it left alone, and why).
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
    /// `aterm pkg repair`, the pass a person asks for: an rc block the person deleted is
    /// laid again (and said), and the steps repair always re-ran — every program's shims,
    /// the exec roots at `Deep`, the recorded rustup seams — run again over an `Ok` probe
    /// ([`Item::refresh`]), as they did before they were items.
    Repair,
    /// Every other pass — bare `aterm doctor`, `aterm init`, the seed and the end of an
    /// install or update pass (`atpkg`'s `relay_base`): each item applies only where its
    /// probe differs, and a deleted rc block (or a deleted login rc) stays deleted (design
    /// §5: "minus the opt-out re-lay").
    Doctor,
}

/// What every repair item reads and writes: one per run, shared.
#[derive(Debug, Clone)]
pub struct Ground {
    /// The store.
    pub layout: Layout,
    /// The account's home (`~/.aterm`, the rc files, `~/.local/bin`); `None` when unknown.
    pub home: Option<PathBuf>,
    /// The executable running the pass, canonical: the command links name its bundle.
    pub exe: Option<PathBuf>,
    /// rustup's home (`$RUSTUP_HOME`, else `~/.rustup`); `None` when unknown.
    pub rustup_home: Option<PathBuf>,
    /// The rc the account's login shell reads, the one rc a pass creates when it is absent
    /// ([`crate::hooks::login_rc`]); `None` when the login shell reads no rc atpkg wires.
    pub login_rc: Option<&'static str>,
    /// Which surface runs the items.
    pub pass: Pass,
}

impl Ground {
    /// The ground of this process: its home, its own executable, rustup's home and the
    /// login shell's rc (the account record's shell, never `$SHELL`).
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

/// One repair step as an item: what it reads, and what its apply said (its detail).
struct Repair {
    step: Step,
    ground: Arc<Ground>,
    /// What the apply or the refresh did, one line each: the row's detail.
    said: Mutex<Vec<String>>,
}

impl Repair {
    fn say(&self, lines: impl IntoIterator<Item = String>) {
        self.said
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(lines);
    }

    fn layout(&self) -> &Layout {
        &self.ground.layout
    }

    /// Whether this pass re-runs the step whatever its probe reads: `pkg repair`'s shims,
    /// exec roots and rustup seams ([`Pass::Repair`]).
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

/// `Probe::Change` from the value now to the value wanted.
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

/// Paths as one comma-separated phrase.
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
            // Moves a file a person may have put there, or re-points a link they made.
            Step::CopiedCommands | Step::RustupLocal => Class::Asked,
            // Laying the block again over a recorded opt-out is the asked act (§3.2).
            Step::Rc if self.ground.pass == Pass::Repair => Class::Asked,
            // Only an install puts a missing build back.
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
        // `pkg repair` runs these steps whatever they read: one whose state could not be
        // read is attempted, and the read-back judges it (still unread: `Failed`, exit 1).
        match probe {
            Probe::Unusable { why } if self.reruns() => change(why, "put in order by the step"),
            probe => probe,
        }
    }

    fn apply(&self) -> Result<(), String> {
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

    fn refresh(&self) -> Option<Result<(), String>> {
        // `aterm pkg repair` re-runs what it always re-ran, whatever the probe read: every
        // program's shims (each build's exported environment with them), the exec roots
        // proven at `Deep`, and every recorded rustup seam with its view.
        if !self.reruns() {
            return None;
        }
        match self.step {
            Step::Shims => Some(self.apply_shims()),
            Step::ExecRoots => Some(self.apply_exec_roots()),
            Step::RustupSeam => Some(self.apply_seam(false)),
            _ => None,
        }
    }

    fn refresh_changes(&self) -> bool {
        // A re-lay rewrites the shims, can rebuild a root and sweep unused build copies:
        // the engine journals it first.
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
        // The block never leads the hooks: an rc block naming a hook that is not on disk
        // sources nothing, and being marker-bounded no later pass would rewrite it.
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

// The shell's half: copied commands, the command links, the hooks and the rc block.
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
            // On macOS the reason names what keeps this copy from replacing it; elsewhere
            // no aterm lays these links, and the remedy alone is the way.
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

    /// The links the pass would write: each with `None` where nothing stands, or the
    /// target of a link into another `aterm.app` it re-points
    /// ([`crate::hooks::command_links_to_write`]), and the store's absent argv0 aliases.
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
        // What stands at each name now says what was written; the read-back judges the rest.
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

    /// Why this pass writes nothing of the shell's: the doctor's pass run from an app
    /// bundle that is not the release `aterm.app` (a stale dev bundle would lay its older
    /// hook text), as the update pass's hooks stand aside for it. `pkg repair` runs from
    /// anywhere.
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
        // `Ok`, not `NotApplicable`: this pass wants the hooks as they are, and the rc
        // item after it answers for itself.
        if let Some(why) = self.other_bundle() {
            return ok(format!("left as they are: {why}"));
        }
        let Ok(home) = self.home() else {
            return blocked("$HOME is unknown, so ~/.aterm/shell.d cannot be found");
        };
        // The directories first: a hook current in a directory another account can write
        // into is no hook to wire an rc to (the `after` edge holds the rc back on it).
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

    /// The absent login rc ([`Ground::login_rc`]) and how a pass treats it
    /// ([`crate::hooks::absent_login_rc`]): `None` when it stands, or none is to be made.
    #[cfg(unix)]
    fn absent_login_rc(&self, home: &Path) -> Option<(&'static str, crate::hooks::RcState)> {
        let rc = self.ground.login_rc?;
        crate::hooks::absent_login_rc(home, rc).map(|state| (rc, state))
    }

    /// The rc files a pass of this kind would wire now, the absent login rc it would create
    /// among them.
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

    /// The rc block may name only the current hooks in this account's private
    /// directories. Selection can leave the hooks item out of the run, so the rc item
    /// enforces this itself, again immediately before its writer runs.
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
        // An rc that stands but that no pass edits (a dangling link, a directory, other
        // bytes than UTF-8) is not in the wiring; it is said, never touched.
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

// The store's half: the stubs, rustup, the held update, the shims, the exec roots.
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
            // A link into the store, not the view: every pass re-points it.
            Ok(Entry::Link(_)) if status.in_prefix => change(
                "rustup `trust` -> inside the store, not the view atpkg lays",
                "a link to the managed view",
            ),
            // A link the person made, or a directory or file: theirs. A stale one is
            // `base.pkg.rustup.local`'s; `aterm pkg doctor` names the rest.
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

    /// Re-assert the rustup seams, or with `stale` re-point a stale link too
    /// ([`crate::seam::repair`]); the events are the row's detail.
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
        let plan = shim_plan(self.layout());
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
        // A shim an older atpkg laid under a name no shim may carry now (design S13) is the
        // sweep's to remove, whichever program's store it forwards into.
        let refused = crate::activate::refused_name_shims(self.layout());
        // A program whose development link cannot be read keeps its shims, and the rest
        // are still re-laid (the apply then fails on it, naming it): one unreadable marker
        // holds no other program's commands back. `pkg repair` re-lays every program, as
        // it always has, so there it is a change whatever the rest read.
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
        // The refused-name shims go first: the re-lay reads the names a program's shims
        // still carry, and would only refuse each of these again.
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
        match crate::compat::inspect(layout, build) {
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
        let plan = shim_plan(self.layout());
        if plan.lay.is_empty() && plan.missing.is_empty() && plan.revoked.is_empty() {
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

/// One program [`relay`] re-lays: its name, the build, the build's directory and the
/// names it exposes.
pub(crate) struct Planned {
    pub(crate) program: String,
    pub(crate) build: u64,
    pub(crate) build_dir: PathBuf,
    pub(crate) exposes: Vec<String>,
}

/// What a re-lay of the installed programs' shims works from: one rule for the probe and
/// the lay ([`shim_plan`]).
pub(crate) struct ShimPlan {
    /// The programs to re-lay.
    pub(crate) lay: Vec<Planned>,
    /// `program@build` of each whose build is gone.
    pub(crate) missing: Vec<String>,
    /// `program@build` of each left tombstoned: only an update pass may lift that.
    pub(crate) revoked: Vec<String>,
    /// What was left alone and why (a dev link).
    pub(crate) notes: Vec<String>,
    /// What could not even be read (a development link).
    pub(crate) failed: Vec<String>,
}

/// Which builds a re-lay of the shims works from: the build ACTIVATION selects
/// (`store/<program>/current`), not the highest on disk — `rollback` keeps the build it
/// rolled off until gc, and re-laying that one silently undid the rollback and minted a
/// LinkShimMismatch (audit K2). With no usable link, the newest complete build. A
/// dev-linked program owns its shims, and a tombstoned one keeps its tombstone (audit K2).
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
    };
    // A linked checkout owns its shims even when an older store build remains.
    // Validate the bounded directory as well as each marker: malformed intent
    // is not permission to replace a developer's active tools with store copies.
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
        // Never over a tombstone (audit K2): a revoked build keeps its tree, marker and
        // `current` links; only its shims turn into failing notices.
        let tombstoned = recorded
            .get(program)
            .is_some_and(|r| r.state.starts_with("tombstoned:"))
            || exposes
                .iter()
                .any(|tool| crate::cli::is_tombstone_shim(layout, tool));
        if tombstoned {
            plan.revoked.push(format!("{program}@{build}"));
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

/// The `bin/` shims a re-lay of `planned` lays: each exposed name, and its `alab-` alias
/// where the program's aliases are laid ([`crate::activate::Aliases::laid_for`]).
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

/// Whether the shim at `shim` forwards to something that is there. A target that cannot
/// be checked is no evidence of a fault (the doctor's own rule for check (4)).
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
    /// What was said along the way: a dev link left alone, a refused shim name.
    pub(crate) notes: Vec<String>,
}

/// Re-lay every installed program's shims from the build [`shim_plan`] selects, each with
/// the build's own `shim_env` (its sidecar, design S7): a re-lay exports what the signed
/// manifest declared, never less — `claude`'s DISABLE_AUTOUPDATER=1 above all.
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
        // What the lay says (an exec root it laid) is the row's, never stdout's: the JSON
        // surfaces print one document there.
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

/// What [`relay`] re-lays for `program` from `build_dir`: the names its own shims still
/// carry (they resolve even when their TARGET is gone, which is the audited shape), falling
/// back to the program's own name — the overwhelmingly common single-tool case — when none
/// survive.
///
/// PLUS THE PROGRAM'S OWN NAME WHEN ONLY ITS SHIM IS GONE: the build ships `bin/<program>`
/// and `bin/<program>` in the managed `bin/` is absent. The survivors are not a record of
/// the signed exposes — nothing local is — so reading them alone would make a lost shim
/// permanent (the end state a trust rollback leaves once the next trust flip prunes what
/// it re-pointed, see `flow::rollback_may_claim`). An absent shim is the only case taken:
/// a present one — a dev link, a tombstone, another program's shim — is never overwritten.
pub(crate) fn exposes(layout: &Layout, program: &str, build_dir: &Path) -> Vec<String> {
    let Some(mut exposes) = crate::installed_exposes(layout, program) else {
        return vec![program.to_string()];
    };
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
    exposes
}

#[cfg(all(test, unix))]
mod tests;
