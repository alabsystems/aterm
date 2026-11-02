// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `base.clt` item for Apple's Command Line Tools ([`crate::items::developer`]).

use crate::items::{Ctx, Item};
use crate::model::Probe;
use crate::port::System;
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

/// `base.clt`: installs Apple's Command Line Tools through the background update service.
pub struct CommandLineTools {
    found: ProbeClt,
}

impl CommandLineTools {
    /// The item over `found`.
    #[must_use]
    pub fn new(found: ProbeClt, _aterm: PathBuf) -> Self {
        Self { found }
    }
}

/// Where `aterm init` places the item.
#[must_use]
pub fn place() -> crate::engine::Place {
    crate::engine::Place {
        layer: aterm_converge::Layer::Base,
        class: aterm_converge::Class::Install,
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
            // The download waits for a network without Low Data Mode.
            Found::Missing(from) if let Some(why) = crate::items::developer::low_data(sys) => {
                Probe::Blocked {
                    why: format!("{from}; {why}"),
                }
            }
            Found::Missing(from) => Probe::Change {
                from,
                to: "Command Line Tools installed automatically and verified".into(),
            },
            Found::Refuses(why) => Probe::Blocked { why },
            Found::Unknown(why) => Probe::Unusable { why },
            Found::NotHere(why) => Probe::NotApplicable { why },
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        crate::items::developer::install_automatically(sys)
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

    const LIST: &str = "/usr/sbin/softwareupdate -l";
    const INSTALL: &str = "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 26.6-26.6'";
    const CATALOG: &str = "* Label: Command Line Tools for Xcode 26.6-26.6\n\tTitle: Command Line Tools for Xcode, Version: 26.6\n";

    #[test]
    fn missing_tools_install_and_verify_without_a_terminal_or_sudo() {
        let sys = Fake::default();
        let item = item(Found::Missing("no developer directory".into()));
        let row = run(&sys, &item, Mode::Plan);
        assert_eq!(row.outcome, Outcome::Planned);
        assert_eq!(row.class, Some(Class::Install));
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.contains("softwareupdate"))
        );
        sys.on(LIST, Ran::ok(CATALOG));
        sys.on(INSTALL, Ran::ok("Done."))
            .on_write(INSTALL, std::path::Path::new(CC), b"")
            .on_write(
                INSTALL,
                std::path::Path::new(crate::items::developer::CLT_GIT),
                b"",
            );
        let row = run(&sys, &item, Mode::Apply);
        assert_eq!(row.outcome, Outcome::Changed, "{row:?} {:?}", sys.calls());
        assert_eq!(row.layer, Some(Layer::Base));
        assert!(sys.calls().iter().any(|call| call == INSTALL));
        assert!(sys.calls().iter().all(|call| !call.contains("sudo")
            && !call.starts_with("tty ")
            && !call.contains("xcode-select --install")));
    }

    #[test]
    fn an_empty_catalog_never_launches_apples_installer() {
        let sys = Fake::default();
        sys.on(LIST, Ran::ok("No new software available."));
        let row = run(&sys, &item(Found::Missing("missing".into())), Mode::Apply);
        assert_eq!(row.outcome, Outcome::Failed);
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.contains("xcode-select") && !call.contains("sudo"))
        );
    }

    #[test]
    fn installer_success_without_a_working_compiler_is_not_complete() {
        let sys = Fake::default();
        sys.on(LIST, Ran::ok(CATALOG));
        sys.on(INSTALL, Ran::ok("Done.")).on_write(
            INSTALL,
            std::path::Path::new(crate::items::developer::CLT_GIT),
            b"",
        );
        let row = run(
            &sys,
            &item(Found::Missing("compiler missing".into())),
            Mode::Apply,
        );
        assert_eq!(row.outcome, Outcome::Failed);
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
