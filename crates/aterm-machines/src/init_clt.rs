// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `base.clt` item for Apple's Command Line Tools ([`crate::items::developer`]).

use crate::items::{Ctx, Item};
use crate::model::Probe;
use crate::port::{Cmd, System};
use std::path::PathBuf;

/// The item's id, as `aterm init --only` and every remedy that names it spell it.
pub const ID: &str = "base.clt";

/// What the C toolchain probe found, in the item's terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// The tools build a C program.
    Present(String),
    /// No developer directory holds a compiler.
    Missing(String),
    /// A compiler is installed and refuses to build.
    Refuses(String),
    /// The probe could not tell (a compiler that did not answer in time).
    Unknown(String),
    /// Not a question here (not macOS).
    NotHere(String),
}

/// The probe [`CommandLineTools`] reads.
pub type ProbeClt = Box<dyn Fn(&dyn System) -> Found + Send + Sync>;

/// `base.clt`: installs Apple's Command Line Tools by the root path that exists.
pub struct CommandLineTools {
    found: ProbeClt,
    /// The `aterm` that `sudo` runs (this executable).
    aterm: PathBuf,
}

impl CommandLineTools {
    /// The item over `found`.
    #[must_use]
    pub fn new(found: ProbeClt, aterm: PathBuf) -> Self {
        Self { found, aterm }
    }

    /// The one-table baseline the root half applies.
    #[must_use]
    pub fn baseline() -> String {
        format!(
            "schema = {}\n\n[developer]\ncommand_line_tools = true\n",
            crate::SCHEMA
        )
    }

    /// The argv the apply runs on the person's terminal for the baseline at `baseline`.
    #[must_use]
    pub fn command(&self, baseline: &std::path::Path) -> Cmd {
        Cmd::new("/usr/bin/sudo")
            .arg(self.aterm.as_os_str())
            .args(["machines", "here", "apply"])
            .arg(baseline.as_os_str())
    }
}

/// Where `aterm init` places the item.
#[must_use]
pub fn place() -> crate::engine::Place {
    crate::engine::Place {
        layer: aterm_converge::Layer::Base,
        class: aterm_converge::Class::Elevate,
        requires: aterm_converge::Requires::Account,
        remedy: aterm_converge::Remedy::Init {
            only: ID.to_owned(),
        },
    }
}

/// Whether the `--only` word `word` names the item ([`aterm_converge::selects`]).
#[must_use]
pub fn selected(word: &str) -> bool {
    aterm_converge::selects(word, ID, "developer", Some(aterm_converge::Layer::Base))
}

/// Why the item cannot install from here.
pub const NO_TTY: &str = "installing the Command Line Tools asks for your password, and this \
                          run has no terminal to ask it in: run `aterm init --only base.clt` \
                          in a terminal";

impl Item for CommandLineTools {
    fn id(&self) -> String {
        ID.to_owned()
    }

    fn family(&self) -> &'static str {
        "developer"
    }

    fn title(&self) -> String {
        "Apple's Command Line Tools (cc, git, the macOS SDK)".to_owned()
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        match (self.found)(sys) {
            Found::Present(now) => Probe::Ok { now },
            Found::Missing(from) if !sys.has_tty() => Probe::Blocked {
                why: format!("{from}; {NO_TTY}"),
            },
            // Asked before the password: the download waits for a network without Low Data
            // Mode.
            Found::Missing(from) if let Some(why) = crate::items::developer::low_data(sys) => {
                Probe::Blocked {
                    why: format!("{from}; {why}"),
                }
            }
            Found::Missing(from) => Probe::Change {
                from,
                to: format!(
                    "installed as root by `{} machines here apply <baseline>` (one password \
                     prompt)",
                    Cmd::new("/usr/bin/sudo")
                        .arg(self.aterm.as_os_str())
                        .display()
                ),
            },
            Found::Refuses(why) => Probe::Blocked { why },
            Found::Unknown(why) => Probe::Unusable { why },
            Found::NotHere(why) => Probe::NotApplicable { why },
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        if !sys.has_tty() {
            return Err(NO_TTY.to_owned());
        }
        let scratch = sys
            .scratch_dir(None, "base-clt")
            .map_err(|e| format!("no private scratch for the baseline: {e}"))?;
        let baseline = scratch.path().join("command-line-tools.toml");
        sys.write_file(&baseline, Self::baseline().as_bytes(), 0o600)
            .map_err(|e| format!("{}: {e}", baseline.display()))?;
        let command = self.command(&baseline);
        match sys.run_tty(&command) {
            Ok(Some(0)) => Ok(()),
            Ok(status) => Err(format!(
                "`{}` exited {}",
                command.display(),
                status.map_or_else(|| "on a signal".to_owned(), |code| code.to_string())
            )),
            Err(e) => Err(format!("`{}`: {e}", command.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::bind;
    use crate::items::defaults::tests::ctx;
    use crate::model::{Mode, Outcome};
    use crate::port::{Fake, Ran};
    use aterm_converge::{Class, Layer, NoJournal, Scope};

    const ATERM: &str = "/Applications/aterm.app/Contents/MacOS/aterm";

    const CC: &str = "/Library/Developer/CommandLineTools/usr/bin/cc";

    fn item(found: Found) -> CommandLineTools {
        CommandLineTools::new(
            Box::new(move |sys: &dyn System| {
                if sys.read_file(std::path::Path::new(CC)).is_ok() {
                    Found::Present(CC.into())
                } else {
                    found.clone()
                }
            }),
            PathBuf::from(ATERM),
        )
    }

    fn run(sys: &Fake, item: &CommandLineTools, mode: Mode) -> aterm_converge::ItemReport {
        let ctx = ctx(&[]);
        let bound = bind([(item as &dyn Item, place())], sys, &ctx);
        let mut rows =
            aterm_converge::run_scoped(&bound, mode, Scope::EVERY, &mut NoJournal, &mut |_| {})
                .unwrap();
        rows.remove(0)
    }

    #[test]
    fn missing_tools_plan_the_machines_here_argv_and_apply_runs_it_on_the_terminal() {
        let mut sys = Fake::default();
        sys.tty = true;
        let item = item(Found::Missing("no developer directory".into()));

        let row = run(&sys, &item, Mode::Plan);
        assert_eq!(row.outcome, Outcome::Planned, "{row:?}");
        assert_eq!(
            row.to.as_deref(),
            Some(
                "installed as root by `/usr/bin/sudo /Applications/aterm.app/Contents/MacOS/\
                 aterm machines here apply <baseline>` (one password prompt)"
            ),
            "{row:?}"
        );
        assert_eq!(row.layer, Some(Layer::Base));
        assert_eq!(row.class, Some(Class::Elevate));
        assert!(
            !sys.calls().iter().any(|call| call.starts_with("tty ")),
            "a plan runs nothing: {:?}",
            sys.calls()
        );

        let scratch = Fake::scratch_path(None, "base-clt", 0);
        let baseline = scratch.join("command-line-tools.toml");
        let argv = format!(
            "tty /usr/bin/sudo {ATERM} machines here apply {}",
            baseline.display()
        );
        sys.on(&argv, Ran::ok(""))
            .on_write(&argv, std::path::Path::new(CC), b"");
        let row = run(&sys, &item, Mode::Apply);
        assert_eq!(row.outcome, Outcome::Changed, "{row:?} {:?}", sys.calls());
        let calls = sys.calls();
        let wrote = calls
            .iter()
            .position(|call| call == &format!("write {} 600", baseline.display()))
            .unwrap_or_else(|| panic!("the baseline is written: {calls:?}"));
        let ran = calls
            .iter()
            .position(|call| call == &argv)
            .unwrap_or_else(|| panic!("the argv runs on the terminal: {calls:?}"));
        assert!(wrote < ran, "{calls:?}");
        assert_eq!(
            calls.iter().filter(|call| call.contains("sudo")).count(),
            1,
            "one password prompt: {calls:?}"
        );
        assert!(
            calls
                .iter()
                .any(|call| call == &format!("scratch closed {}", scratch.display())),
            "the scratch goes with the run: {calls:?}"
        );
        let parsed = crate::baseline::parse(&CommandLineTools::baseline()).unwrap();
        assert_eq!(
            parsed.developer.and_then(|table| table.command_line_tools),
            Some(true)
        );
    }

    /// In Low Data Mode the item asks no password and runs nothing: the download waits, and
    /// the row says how to go on; on a network without it the same Mac plans the install.
    #[test]
    fn low_data_mode_asks_no_password_and_says_how_to_go_on() {
        let mut sys = Fake::default();
        sys.tty = true;
        sys.low_data = Some(true);
        let item = item(Found::Missing("no developer directory".into()));
        for mode in [Mode::Plan, Mode::Apply] {
            let row = run(&sys, &item, mode);
            assert_eq!(row.outcome, Outcome::Blocked, "{mode:?}: {row:?}");
            let why = row.why.unwrap_or_default();
            assert!(
                why.starts_with("no developer directory; Low Data Mode is on"),
                "{why}"
            );
        }
        assert!(
            sys.calls().iter().all(|call| !call.starts_with("tty ")),
            "{:?}",
            sys.calls()
        );
        sys.low_data = Some(false);
        assert_eq!(run(&sys, &item, Mode::Plan).outcome, Outcome::Planned);
    }

    #[test]
    fn without_a_terminal_the_item_is_blocked_on_init_only_base_clt() {
        let sys = Fake::default();
        let item = item(Found::Missing("no developer directory".into()));
        for mode in [Mode::Plan, Mode::Apply] {
            let row = run(&sys, &item, mode);
            assert_eq!(row.outcome, Outcome::Blocked, "{mode:?}: {row:?}");
            assert!(
                row.why
                    .as_deref()
                    .is_some_and(|why| why.contains("`aterm init --only base.clt`")),
                "{row:?}"
            );
        }
        assert!(sys.calls().is_empty(), "{:?}", sys.calls());
    }

    #[test]
    fn the_probe_maps_each_finding() {
        let mut sys = Fake::default();
        sys.tty = true;
        for (found, outcome) in [
            (Found::Present("cc".into()), Outcome::Ok),
            (Found::Refuses("license".into()), Outcome::Blocked),
            (Found::Unknown("no answer".into()), Outcome::Unknown),
            (Found::NotHere("not macOS".into()), Outcome::NotApplicable),
        ] {
            let row = run(&sys, &item(found.clone()), Mode::Apply);
            assert_eq!(row.outcome, outcome, "{found:?}: {row:?}");
        }
        assert!(sys.calls().is_empty(), "{:?}", sys.calls());
    }
}
