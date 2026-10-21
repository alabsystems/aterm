// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The local, per-user macOS setup, sharing the machines probe/apply/read-back engine.

use crate::engine::{Journal, Place};
use crate::items::{Ctx, Item};
use crate::model::{HostFacts, ItemReport, Mode, Outcome};
use crate::port::{Cmd, Live, System};
use aterm_converge::{Class, Layer, Remedy, Report, ReportMode, Requires};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The local setup's command-line help.
pub const USAGE: &str = "aterm init [--plan | --check] [--json]\n\
    Configure this macOS login: Caps Lock as Control, Natural scrolling off,\n\
    no widgets, only aterm, System Settings and Safari pinned in the Dock,\n\
    sign out of Messages and FaceTime, and keep rustup's offline docs (rust-docs,\n\
    tens of thousands of pages Spotlight indexes) off this login: the minimal\n\
    profile, and rust-docs removed from the toolchains rustup installed. Applies\n\
    and reads back each setting; safe to run again.\n\
    Optional ~/.config/aterm/init.toml [github] config authenticates gh and clones\n\
    all accessible owned repositories into your home; see aterm help init.\n\
    Run as the logged-in user, without sudo. Account sign-out may need\n\
    Accessibility and Automation permission for aterm in System Settings.\n\
\n\
    --plan    Report the changes without applying them.\n\
    --check   Read only; exit nonzero if anything still needs attention.\n\
    --json    Print the per-setting report as JSON.\n\
    --help    Show this help without inspecting or changing the Mac.\n";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Action {
    #[default]
    Apply,
    Plan,
    Check,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Options {
    action: Action,
    json: bool,
    help: bool,
}

fn options(args: &[OsString]) -> Result<Options, String> {
    let mut options = Options::default();
    for arg in args {
        match arg.to_str() {
            Some("--help" | "-h") => options.help = true,
            Some("--json") => options.json = true,
            Some("--plan" | "--check") => {
                let action = if arg == "--plan" {
                    Action::Plan
                } else {
                    Action::Check
                };
                if options.action != Action::Apply && options.action != action {
                    return Err("choose either --plan or --check".into());
                }
                options.action = action;
            }
            _ => return Err(format!("unknown argument {arg:?}")),
        }
    }
    Ok(options)
}

/// Run `aterm init`. Parsing and help precede every live probe, including HOME checks.
pub fn main_entry(args: Vec<OsString>) -> ExitCode {
    let options = match options(&args) {
        Ok(options) => options,
        Err(why) => {
            eprintln!("aterm init: {why}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    if options.help {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if !crate::sessions::is_live_account() {
        eprintln!("aterm init: run in your own macOS login and home directory");
        return ExitCode::FAILURE;
    }
    let sys = Live::new();
    let result = crate::engine::account_journal().and_then(|mut journal| {
        let ctx = local_context(&sys)?;
        let groups = groups(&sys, &ctx)?;
        let mode = if options.action == Action::Apply {
            Mode::Apply
        } else {
            Mode::Plan
        };
        run(&sys, &ctx, &groups, mode, &mut journal)
    });
    let rows = match result {
        Ok(rows) => rows,
        Err(why) => {
            eprintln!("aterm init: {why}");
            return ExitCode::FAILURE;
        }
    };
    let complete = complete(&rows, options.action);
    if options.json {
        // The one envelope every `--json` surface prints (`aterm pkg doctor --json` too).
        let mode = match options.action {
            Action::Apply => ReportMode::Apply,
            Action::Plan => ReportMode::Plan,
            Action::Check => ReportMode::Check,
        };
        match aterm_json::to_string(&Report::new(mode, complete, rows)) {
            Ok(report) => println!("{report}"),
            Err(why) => {
                eprintln!("aterm init: cannot encode report: {why}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        for row in &rows {
            println!("{}: {}", word(row.outcome), row.title);
            if let (Some(from), Some(to)) = (&row.from, &row.to) {
                println!("    {from} → {to}");
            }
            if let Some(why) = &row.why {
                println!("    {why}");
            }
        }
        let summary = match (options.action, complete) {
            (Action::Plan, true) => "plan ready; no changes applied",
            (_, true) => "all requested settings verified",
            (_, false) => "settings still need attention; see the rows above",
        };
        println!("aterm init: {summary}");
    }
    if complete {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Items that share one place in the desired state.
type Group = (Vec<Box<dyn Item>>, Place);

/// What init converges, in run order, each list with its place: all of it the
/// workstation layer, run at the console (design §4.3); the GitHub sign-in and checkouts
/// reach the network.
fn groups(sys: &dyn System, ctx: &Ctx) -> Result<Vec<Group>, String> {
    let place = |class| Place {
        layer: Layer::Workstation,
        class,
        requires: Requires::Console,
        remedy: Remedy::Command("aterm init".into()),
    };
    Ok(vec![
        (
            crate::init_preferences::items(ctx)?,
            place(Class::Preference),
        ),
        (crate::init_trackpad::items(ctx)?, place(Class::Preference)),
        // rustup's profile and the rust-docs its toolchains hold: an act on the
        // person's own toolchains that only a run they started makes (§3.2 `asked`).
        (crate::init_spotlight::items(ctx)?, place(Class::Asked)),
        (crate::init_accounts::items(ctx)?, place(Class::Preference)),
        (crate::init_github::items(sys, ctx)?, place(Class::Install)),
    ])
}

/// Run `groups` against `sys`, each change journaled in `journal` before it is made.
fn run(
    sys: &dyn System,
    ctx: &Ctx,
    groups: &[Group],
    mode: Mode,
    journal: &mut dyn Journal,
) -> Result<Vec<ItemReport>, String> {
    let bound = crate::engine::bind(
        groups.iter().flat_map(|(items, place)| {
            items.iter().map(move |item| (item.as_ref(), place.clone()))
        }),
        sys,
        ctx,
    );
    aterm_converge::run(&bound, mode, journal).map_err(|refused| refused.to_string())
}

/// Collect only what this setup uses, avoiding fleet, TCC database and unrelated app probes.
fn local_context(sys: &dyn System) -> Result<Ctx, String> {
    if sys.euid() == 0 {
        return Err("run as the logged-in user, without sudo".into());
    }
    let name = read(sys, &Cmd::new("/usr/bin/id").arg("-un"))?;
    let user = crate::facts::user(sys, name.trim())?;
    let console = sys
        .stat(Path::new("/dev/console"))
        .map_err(|e| format!("cannot read the console owner: {e}"))?;
    if user.uid != sys.euid() || console.uid != user.uid {
        return Err("run as the user currently logged in at this Mac's console".into());
    }
    let command = crate::facts::hidutil_keyboards();
    let keyboard_list = sys
        .run_complete_stdout(&command)
        .map_err(|e| format!("{}: {e}", command.display()))?;
    if !keyboard_list.success() {
        return Err(format!(
            "{}: {}",
            command.display(),
            keyboard_list.describe()
        ));
    }
    let keyboards = crate::facts::parse_hidutil_keyboards_complete(&keyboard_list.out())?;
    let facts = HostFacts {
        arch: read(sys, &Cmd::new("/usr/bin/uname").arg("-m"))?
            .trim()
            .into(),
        os_version: read(sys, &Cmd::new("/usr/bin/sw_vers").arg("-productVersion"))?
            .trim()
            .into(),
        keyboards,
        console_user: Some(user.name.clone()),
        users: vec![user.clone()],
        ..HostFacts::default()
    };
    Ok(Ctx {
        facts,
        users: vec![user.clone()],
        console: Some(user),
        host_name: String::new(),
        cache_dir: PathBuf::new(),
        disrupt: true,
    })
}

fn read(sys: &dyn System, cmd: &Cmd) -> Result<String, String> {
    let ran = sys
        .run(cmd)
        .map_err(|e| format!("{}: {e}", cmd.display()))?;
    if !ran.success() {
        return Err(format!("{}: {}", cmd.display(), ran.describe()));
    }
    Ok(ran.out())
}

fn complete(rows: &[ItemReport], action: Action) -> bool {
    !rows.is_empty()
        && rows.iter().all(|row| {
            matches!(
                row.outcome,
                Outcome::Ok | Outcome::Changed | Outcome::NotApplicable
            ) || (action == Action::Plan && row.outcome == Outcome::Planned)
        })
}

const fn word(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Ok => "ok",
        Outcome::Changed => "changed",
        Outcome::Planned => "planned",
        Outcome::Person => "needs action",
        Outcome::Blocked => "blocked",
        Outcome::NotApplicable => "not applicable",
        Outcome::Deferred => "deferred",
        Outcome::Unknown => "unknown",
        Outcome::Failed => "failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::items::defaults::tests::{ctx, export, user};
    use crate::model::{Effect, Probe};
    use crate::port::{Fake, Ran, Stat};
    use aterm_converge::FileJournal;
    use std::sync::Mutex;

    /// A machines item whose probes answer from a list (the last one repeats).
    struct Listed {
        id: &'static str,
        probes: Mutex<Vec<Probe>>,
    }

    impl Listed {
        fn boxed(id: &'static str, probes: Vec<Probe>) -> Box<dyn Item> {
            Box::new(Self {
                id,
                probes: Mutex::new(probes),
            })
        }
    }

    impl Item for Listed {
        fn id(&self) -> String {
            self.id.into()
        }
        fn family(&self) -> &'static str {
            "test"
        }
        fn title(&self) -> String {
            self.id.into()
        }
        fn probe(&self, _: &dyn System, _: &Ctx) -> Probe {
            let mut probes = self.probes.lock().unwrap();
            if probes.len() > 1 {
                probes.remove(0)
            } else {
                probes[0].clone()
            }
        }
        fn apply(&self, _: &dyn System, _: &Ctx) -> Result<(), String> {
            Ok(())
        }
        fn revert(&self) -> Option<String> {
            Some(format!("undo {}", self.id))
        }
    }

    fn ok() -> Probe {
        Probe::Ok { now: "b".into() }
    }

    fn change() -> Probe {
        Probe::Change {
            from: "a".into(),
            to: "b".into(),
        }
    }

    /// The trackpad item's preference read, answering `com.apple.swipescrolldirection`
    /// saved as off.
    fn saved_off(sys: &Fake) {
        sys.on(
            "alice$ /usr/bin/defaults export -g -",
            export("<dict><key>com.apple.swipescrolldirection</key><false/></dict>"),
        );
    }

    fn place(class: Class) -> Place {
        Place {
            layer: Layer::Workstation,
            class,
            requires: Requires::Console,
            remedy: Remedy::Command("aterm init".into()),
        }
    }

    fn journaled(journal: &FileJournal) -> Vec<aterm_json::Value> {
        match std::fs::read_to_string(journal.path()) {
            Ok(text) => text
                .lines()
                .map(|line| aterm_json::from_str(line).unwrap())
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => panic!("{}: {e}", journal.path().display()),
        }
    }

    #[test]
    fn init_on_a_fake_system_journals_one_line_per_changed_item() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let mut journal = FileJournal::under(scratch.path());
        let alice = ctx(&[user("alice", 501)]);
        // Natural scrolling is saved off but still on in the session: init's trackpad item
        // changes it, and its read-back reads off.
        let sys = Fake::root();
        sys.scroll_state(true);
        saved_off(&sys);
        let groups = vec![
            (
                vec![
                    Listed::boxed("init.test.changes", vec![change(), ok()]),
                    Listed::boxed("init.test.right", vec![ok()]),
                ],
                place(Class::Preference),
            ),
            (
                crate::init_trackpad::items(&alice).unwrap(),
                place(Class::Preference),
            ),
        ];
        let rows = run(&sys, &alice, &groups, Mode::Apply, &mut journal).unwrap();
        assert_eq!(
            rows.iter().map(|r| r.outcome).collect::<Vec<_>>(),
            [Outcome::Changed, Outcome::Ok, Outcome::Changed]
        );
        let lines = journaled(&journal);
        assert_eq!(
            lines
                .iter()
                .map(|l| l["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["init.test.changes", "init.trackpad.scroll-direction@alice"]
        );
        for (line, row) in lines
            .iter()
            .zip(rows.iter().filter(|r| r.outcome == Outcome::Changed))
        {
            assert_eq!(line["from"].as_str(), row.from.as_deref());
            assert_eq!(line["to"].as_str(), row.to.as_deref());
            assert_eq!(line["revert"].as_str(), row.revert.as_deref());
        }
        assert!(rows.iter().all(|r| r.layer == Some(Layer::Workstation)));
    }

    #[test]
    fn init_journals_nothing_when_every_item_is_already_right() {
        // The negative control: the same run with nothing to change, the trackpad's
        // native refresh included, writes no line and makes no journal.
        let scratch = aterm_tempfile::tempdir().unwrap();
        let mut journal = FileJournal::under(scratch.path());
        let alice = ctx(&[user("alice", 501)]);
        let sys = Fake::root();
        sys.scroll_state(false);
        saved_off(&sys);
        let groups = vec![
            (
                vec![Listed::boxed("init.test.right", vec![ok()])],
                place(Class::Preference),
            ),
            (
                crate::init_trackpad::items(&alice).unwrap(),
                place(Class::Preference),
            ),
        ];
        let rows = run(&sys, &alice, &groups, Mode::Apply, &mut journal).unwrap();
        assert!(rows.iter().all(|r| r.outcome == Outcome::Ok), "{rows:?}");
        assert!(sys.calls().contains(&"scroll-direction set false".into()));
        assert!(journaled(&journal).is_empty());
        assert!(!scratch.path().join(FileJournal::DIR).exists());
    }

    #[test]
    fn init_places_every_item_at_the_workstation_layer_run_at_the_console() {
        let alice = ctx(&[user("alice", 501)]);
        let mut sys = Fake::root();
        sys.euid = 501;
        let profile =
            b"[github]\naccount='octocat'\nowner='octocat'\ntoken_file='~/.secrets/github'\n";
        let path = "/Users//alice/.config/aterm/init.toml";
        sys.file(path, profile).stat_of(
            path,
            Stat {
                uid: 501,
                gid: 20,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: profile.len() as u64,
            },
        );
        let groups = groups(&sys, &alice).unwrap();
        let bound = crate::engine::bind(
            groups.iter().flat_map(|(items, place)| {
                items.iter().map(move |item| (item.as_ref(), place.clone()))
            }),
            &sys,
            &alice,
        );
        let github: Vec<String> = bound
            .iter()
            .filter(|b| b.class() == Class::Install)
            .map(|b| b.id())
            .collect();
        assert_eq!(
            github.len(),
            2,
            "the GitHub sign-in and checkouts: {github:?}"
        );
        assert!(github.iter().all(|id| id.contains("github")), "{github:?}");
        // rustup's profile and rust-docs: acts on the person's toolchains, asked by the run.
        let asked: Vec<String> = bound
            .iter()
            .filter(|b| b.class() == Class::Asked)
            .map(|b| b.id())
            .collect();
        assert_eq!(
            asked,
            [
                "init.spotlight.rustup-profile@alice",
                "init.spotlight.rust-docs@alice"
            ]
        );
        for b in &bound {
            assert_eq!(b.layer(), Layer::Workstation, "{}", b.id());
            assert_eq!(b.requires(), Requires::Console, "{}", b.id());
            assert!(
                matches!(b.class(), Class::Preference | Class::Install | Class::Asked),
                "{}",
                b.id()
            );
        }
    }

    #[test]
    fn modes_are_exclusive_and_invalid_arguments_fail_before_probes() {
        assert!(options(&["--plan".into(), "--check".into()]).is_err());
        assert!(options(&["--unknown".into(), "--help".into()]).is_err());
        assert_eq!(
            options(&["--check".into(), "--json".into()]).unwrap(),
            Options {
                action: Action::Check,
                json: true,
                help: false
            }
        );
    }

    #[test]
    fn root_is_refused_without_running_any_command() {
        let sys = Fake::root();
        assert!(local_context(&sys).unwrap_err().contains("without sudo"));
        assert!(sys.calls().is_empty());
    }

    #[test]
    fn a_different_console_owner_is_refused_before_preferences_or_accounts_are_read() {
        let mut sys = Fake::root();
        sys.euid = 501;
        sys.on("/usr/bin/id -un", Ran::ok("alice\n"));
        sys.on(
            "/usr/bin/dscl . -read /Groups/admin GroupMembership",
            Ran::ok("GroupMembership: alice\n"),
        );
        sys.on("/usr/bin/dscl . -read /Users//alice UniqueID PrimaryGroupID NFSHomeDirectory UserShell", Ran::ok("UniqueID: 501\nPrimaryGroupID: 20\nNFSHomeDirectory: /Users//alice\nUserShell: /bin/zsh\n"));
        sys.stat_of(
            "/dev/console",
            Stat {
                uid: 502,
                gid: 20,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: 0,
            },
        );
        assert!(
            local_context(&sys)
                .unwrap_err()
                .contains("currently logged in")
        );
        assert!(!sys.calls().iter().any(|call| call.contains("defaults")
            || call.contains("hidutil")
            || call.contains("accounts.data")));
        let current_user = || {
            let mut sys = Fake::root();
            sys.euid = 501;
            sys.on("/usr/bin/id -un", Ran::ok("alice\n"));
            sys.on(
                "/usr/bin/dscl . -read /Groups/admin GroupMembership",
                Ran::ok("GroupMembership: alice\n"),
            );
            sys.on("/usr/bin/dscl . -read /Users//alice UniqueID PrimaryGroupID NFSHomeDirectory UserShell", Ran::ok("UniqueID: 501\nPrimaryGroupID: 20\nNFSHomeDirectory: /Users//alice\nUserShell: /bin/zsh\n"));
            sys.stat_of(
                "/dev/console",
                Stat {
                    uid: 501,
                    gid: 20,
                    mode: 0o600,
                    dir: false,
                    symlink: false,
                    len: 0,
                },
            );
            sys.on("/usr/bin/uname -m", Ran::ok("arm64\n"));
            sys.on("/usr/bin/sw_vers -productVersion", Ran::ok("27.0\n"));
            sys
        };
        let command = crate::facts::hidutil_keyboards();
        for invalid in [
            "Services:\nUnknownHeader\nnonempty unparsed row\n",
            "Devices:\n",
        ] {
            let sys = current_user();
            sys.on(&command.display(), Ran::ok(invalid));
            assert!(local_context(&sys).is_err());
            assert!(
                !sys.calls()
                    .iter()
                    .any(|call| call.contains("defaults") || call.contains("accounts.data"))
            );
        }
        let sys = current_user();
        sys.on(&command.display(), Ran::ok("Services:\n\nDevices:\n"));
        assert!(local_context(&sys).unwrap().facts.keyboards.is_empty());
        let sys = current_user();
        sys.on(&command.display(), Ran::ok("Services:\n"));
        sys.refuse_complete_stdout(&command.display(), "incomplete captured service list");
        assert!(local_context(&sys).is_err());
    }

    #[test]
    fn pending_permission_or_unverified_changes_never_count_as_complete() {
        let mut row = ItemReport {
            id: "test".into(),
            family: "test".into(),
            title: "test".into(),
            outcome: Outcome::Ok,
            from: None,
            to: None,
            why: None,
            revert: None,
            effect: Effect::Now,
            detail: None,
            layer: None,
            class: None,
            explain: None,
            stood_aside: None,
            essential: false,
        };
        assert!(!complete(&[], Action::Apply));
        for outcome in [
            Outcome::Person,
            Outcome::Blocked,
            Outcome::Deferred,
            Outcome::Unknown,
            Outcome::Failed,
        ] {
            row.outcome = outcome;
            for action in [Action::Apply, Action::Plan, Action::Check] {
                assert!(!complete(&[row.clone()], action));
            }
        }
        row.outcome = Outcome::Planned;
        assert!(complete(&[row.clone()], Action::Plan));
        assert!(!complete(&[row.clone()], Action::Apply));
        assert!(!complete(&[row], Action::Check));
    }
}
