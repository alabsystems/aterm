// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The composed `aterm doctor` over a scratch home, prefix and state root (design S5):
//! `--read-only` changes nothing, bare doctor restores and reads back, a held store lock
//! is a note, and unresolved checks keep readiness non-green. The facts are given
//! ([`Gathered::of_facts`]) and the packages' rows are read over the scratch tree, so no
//! test reads or writes this machine's own state.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aterm_cli::Mark;
use aterm_cli::doctor::{Args, Gathered};
use aterm_converge::{ItemReport, Outcome, Readiness, Report, ReportMode};
use atpkg::converge::repair::{self, Ground, Pass};
use atpkg::store::{Layout, ToolName};

use super::{CONTENDED, Ports, Ran};

/// A scratch home, prefix and state root under one directory, removed on drop.
struct Scratch {
    _dir: aterm_tempfile::TempDir,
    root: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> Self {
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-doctor-{label}-"))
            .tempdir()
            .expect("a scratch directory");
        let root = dir.path().canonicalize().unwrap();
        for sub in ["home", "prefix", "state"] {
            std::fs::create_dir(root.join(sub)).unwrap();
            std::fs::set_permissions(root.join(sub), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        let scratch = Self { _dir: dir, root };
        scratch.release_exe();
        scratch
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    fn layout(&self) -> Layout {
        Layout {
            prefix: self.root.join("prefix"),
        }
    }

    /// The doctor's ground, run from the installed release app.
    fn ground(&self) -> Arc<Ground> {
        Arc::new(Ground {
            layout: self.layout(),
            home: Some(self.home()),
            exe: Some(self.release_exe()),
            rustup_home: Some(self.home().join(".rustup")),
            login_rc: None,
            pass: Pass::Doctor,
        })
    }

    /// A release-named app bundle under the scratch home: the executable the command
    /// links name.
    fn release_exe(&self) -> PathBuf {
        let macos = self.home().join("Applications/aterm.app/Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        for name in ["aterm", "atpkg"] {
            let exe = macos.join(name);
            if !exe.exists() {
                std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
                std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        macos.join("aterm")
    }

    /// A ready build of `program`, its shim laid and its `current` link pointed at it.
    fn install(&self, program: &str, build: u64) -> PathBuf {
        let layout = self.layout();
        let dir = layout.build_dir(program, build);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let exe = dir.join("bin").join(program);
        std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        atpkg::store::mark_build_ready(&dir).unwrap();
        atpkg::activate::install_shims(
            &layout,
            &dir,
            &[program.to_owned()],
            atpkg::activate::Aliases::Off,
        )
        .unwrap();
        atpkg::activate::activate_build(&layout, &dir).unwrap();
        dir
    }

    fn shim(&self, program: &str) -> PathBuf {
        self.layout().shim(&ToolName::new(program).unwrap())
    }

    fn link(&self) -> PathBuf {
        self.home().join(".local/bin/aterm")
    }

    fn zshrc(&self) -> PathBuf {
        self.home().join(".zshrc")
    }

    /// `aterm pkg doctor`'s rows over the scratch home and prefix.
    fn packages(&self) -> Vec<aterm_converge::ItemReport> {
        atpkg::doctor::report_with(
            &self.layout(),
            Some(&self.home()),
            Some(OsStr::new("/usr/bin:/bin")),
            1_790_812_800,
            None,
            &atpkg::doctor::Probes::default(),
        )
    }

    /// Every path under the scratch root with what it holds: a file's bytes and mode, a
    /// link's target, a directory's mode — and their hash.
    fn tree(&self) -> (u64, BTreeMap<PathBuf, String>) {
        fn walk(root: &Path, at: &Path, into: &mut BTreeMap<PathBuf, String>) {
            for entry in std::fs::read_dir(at).unwrap().flatten() {
                let path = entry.path();
                let meta = std::fs::symlink_metadata(&path).unwrap();
                let mode = meta.permissions().mode();
                let held = if meta.file_type().is_symlink() {
                    format!("-> {}", std::fs::read_link(&path).unwrap().display())
                } else if meta.is_dir() {
                    walk(root, &path, into);
                    format!("dir {mode:o}")
                } else {
                    format!("{mode:o} {:?}", std::fs::read(&path).unwrap())
                };
                into.insert(path.strip_prefix(root).unwrap().to_path_buf(), held);
            }
        }
        let mut tree = BTreeMap::new();
        walk(&self.root, &self.root, &mut tree);
        let mut hasher = std::hash::DefaultHasher::new();
        std::hash::Hash::hash(&tree, &mut hasher);
        (std::hash::Hasher::finish(&hasher), tree)
    }
}

/// The facts of a machine whose shell `shell` reads so, and nothing else to say.
fn facts(shell: Mark) -> Gathered {
    let fact = if shell == Mark::Fail {
        "/no/such/shell (not found)"
    } else {
        "/bin/sh (executable)"
    };
    Gathered::of_facts(&[
        ("shell:", shell, fact),
        ("privacy:", Mark::Warn, "Full Disk Access is off"),
    ])
}

fn args(words: &[&str]) -> Args {
    let words: Vec<String> = words.iter().map(ToString::to_string).collect();
    aterm_cli::doctor::args(&words).unwrap()
}

/// Run the doctor as `words` ask over `scratch`, in the account's own shell, its shell
/// reading `shell`, with a state root when `journaled`.
fn run(scratch: &Scratch, shell: Mark, words: &[&str], journaled: bool) -> Ran {
    run_as(scratch, shell, words, journaled, None).0
}

/// [`run`] from a shell that is not the account's own when `not_own` says why, and each
/// gather the run made (`true`: with the Developer Tools hand-over).
fn run_as(
    scratch: &Scratch,
    shell: Mark,
    words: &[&str],
    journaled: bool,
    not_own: Option<&'static str>,
) -> (Ran, Vec<bool>) {
    let ground = scratch.ground();
    let state = scratch.root.join("state");
    let gathers = std::cell::RefCell::new(Vec::new());
    let ran = super::run(
        &args(words),
        &Ports {
            gather: &|fix| {
                gathers.borrow_mut().push(fix);
                facts(shell)
            },
            ground: Some(&ground),
            packages: &|| scratch.packages(),
            state: journaled.then_some(state.as_path()),
            not_own_account: not_own,
        },
    );
    (ran, gathers.into_inner())
}

/// The report's line for `label`, and the lines under it.
fn section(report: &str, label: &str) -> String {
    let mut lines = report.lines().skip_while(|line| !line.starts_with(label));
    let mut out = lines
        .next()
        .unwrap_or_else(|| panic!("no {label} in\n{report}"))
        .to_owned();
    for line in lines.take_while(|line| line.starts_with(' ')) {
        out.push('\n');
        out.push_str(line);
    }
    out
}

/// A report line's mark: the word after its label.
fn mark(line: &str) -> &str {
    line.split_whitespace().nth(1).unwrap_or_default()
}

fn report(ran: &Ran) -> Report {
    aterm_json::from_str(&ran.text).unwrap_or_else(|why| panic!("{why}: {}", ran.text))
}

/// A machine with something to restore: an installed program whose shim was deleted, the
/// `~/.local/bin` link to the app missing, and a `~/.zshrc` atpkg never wired.
fn broken(label: &str) -> Scratch {
    let scratch = Scratch::new(label);
    scratch.install("ay", 17);
    std::fs::remove_file(scratch.shim("ay")).unwrap();
    std::fs::write(scratch.zshrc(), "export EDITOR=vi\n").unwrap();
    scratch
}

/// `--read-only` CHANGES NOTHING: the scratch home, prefix and state root hash the same
/// before and after, though every repair item had something to do (and says so). The
/// negative control: bare doctor over the same tree changes it.
#[test]
fn read_only_changes_nothing_in_the_home_the_store_or_the_state_root() {
    let scratch = broken("read-only");
    let before = scratch.tree();
    let ran = run(&scratch, Mark::Ok, &["--read-only"], true);
    assert_eq!(ran.code, 1, "planned repairs are not ready: {}", ran.text);
    assert_eq!(scratch.tree(), before, "--read-only wrote:\n{}", ran.text);
    let repair = section(&ran.text, "repair:");
    assert_eq!(mark(&repair), "warn", "{repair}");
    for broken in [
        "installed programs' commands",
        "~/.local/bin links",
        "shell rc files",
    ] {
        assert!(
            repair.contains(broken) && repair.contains("bare `aterm doctor` restores it"),
            "{broken}: {repair}"
        );
    }
    let json = report(&run(&scratch, Mark::Ok, &["--read-only", "--json"], true));
    assert_eq!(json.mode, ReportMode::Check);
    assert!(
        json.items
            .iter()
            .any(|row| row.id == repair::SHIMS && row.outcome == Outcome::Planned)
    );
    assert_eq!(scratch.tree(), before);

    let bare = run(&scratch, Mark::Ok, &[], true);
    assert_ne!(
        scratch.tree().0,
        before.0,
        "bare doctor repairs:\n{}",
        bare.text
    );
}

/// BARE DOCTOR RESTORES a deleted shim and a missing `~/.local/bin` link, each read back
/// from disk, and journals each change; an rc the person opted out of (deleted atpkg's
/// block from) stays as they left it.
#[test]
fn bare_doctor_restores_a_deleted_shim_and_a_missing_link_and_leaves_an_opted_out_rc() {
    let scratch = broken("bare");
    let build = scratch.install("ay", 17);
    std::fs::remove_file(scratch.shim("ay")).unwrap();
    let ran = run(&scratch, Mark::Ok, &[], true);
    assert_eq!(ran.code, 1, "the manual privacy step remains: {}", ran.text);
    let target = atpkg::platform::resolve_shim(&scratch.shim("ay")).expect("the shim is back");
    assert!(target.starts_with(&build), "{}", target.display());
    assert_eq!(
        std::fs::read_link(scratch.link()).expect("the link is back"),
        scratch.release_exe()
    );
    assert!(
        std::fs::read_to_string(scratch.zshrc())
            .unwrap()
            .contains("atpkg shell integration"),
        "an rc atpkg never wired is wired"
    );
    let repair = section(&ran.text, "repair:");
    assert!(repair.starts_with("repair:"), "{repair}");
    assert!(
        repair.contains("appended atpkg's block to ~/.zshrc"),
        "{repair}"
    );
    let journal = std::fs::read_to_string(scratch.root.join("state/converge/journal.ndjson"))
        .expect("the changes are journaled");
    for id in [repair::SHIMS, repair::LINKS, repair::RC] {
        assert!(journal.contains(id), "{id} in\n{journal}");
    }

    // The person deletes atpkg's block: the opt-out. The shim and the link go again.
    std::fs::write(scratch.zshrc(), "export EDITOR=vi\n").unwrap();
    std::fs::remove_file(scratch.shim("ay")).unwrap();
    std::fs::remove_file(scratch.link()).unwrap();
    let json = report(&run(&scratch, Mark::Ok, &["--json"], true));
    assert_eq!(json.mode, ReportMode::Apply);
    assert!(!json.complete, "the manual privacy step remains");
    let outcome = |id: &str| {
        json.items
            .iter()
            .find(|row| row.id == id)
            .unwrap_or_else(|| panic!("no {id}"))
            .outcome
    };
    assert_eq!(outcome(repair::SHIMS), Outcome::Changed);
    assert_eq!(outcome(repair::LINKS), Outcome::Changed);
    assert_eq!(
        outcome(repair::RC),
        Outcome::Ok,
        "the opt-out is the person's"
    );
    assert!(
        atpkg::platform::resolve_shim(&scratch.shim("ay")).is_some(),
        "read back"
    );
    assert!(std::fs::read_link(scratch.link()).is_ok(), "read back");
    assert_eq!(
        std::fs::read_to_string(scratch.zshrc()).unwrap(),
        "export EDITOR=vi\n",
        "left unwired"
    );
}

/// A HELD STORE LOCK makes the repair a note, never exit 75: an install pass holds
/// `store.lock`, so every item stands aside (the shim stays deleted, nothing is
/// journaled), the `repair:` row is a note saying why, and readiness remains false.
#[test]
fn a_held_store_lock_is_a_note_and_nothing_is_repaired() {
    let scratch = broken("locked");
    let held = atpkg::lock::try_lock_store(&scratch.layout()).expect("the test holds the lock");
    let ran = run(&scratch, Mark::Ok, &[], true);
    assert_eq!(ran.code, 1, "unresolved work is not ready: {}", ran.text);
    let repair = section(&ran.text, "repair:");
    assert_eq!(mark(&repair), "note", "{repair}");
    assert!(
        repair.contains("rc files: waits for base.shell.hooks\n"),
        "{repair}"
    );
    assert!(repair.contains(CONTENDED), "{repair}");
    assert!(
        std::fs::symlink_metadata(scratch.shim("ay")).is_err(),
        "nothing restored"
    );
    assert!(
        std::fs::symlink_metadata(scratch.link()).is_err(),
        "nothing linked"
    );
    assert!(
        !scratch.root.join("state/converge").exists(),
        "no journal line for a change nobody made"
    );
    let json = report(&run(&scratch, Mark::Ok, &["--json"], true));
    assert!(!json.complete);
    let shims = json
        .items
        .iter()
        .find(|row| row.id == repair::SHIMS)
        .unwrap();
    assert_eq!(shims.outcome, Outcome::Blocked);
    assert_eq!(
        shims.stood_aside,
        Some(aterm_converge::StoodAside::Lock(CONTENDED.to_owned()))
    );

    // Released, the same run repairs.
    drop(held);
    let ran = run(&scratch, Mark::Ok, &[], true);
    assert!(
        !section(&ran.text, "repair:").contains(CONTENDED),
        "{}",
        ran.text
    );
    assert!(atpkg::platform::resolve_shim(&scratch.shim("ay")).is_some());
}

/// A USABLE SHELL IS NOT A READY MACHINE: failed repairs, manual steps and broken
/// packages remain non-green. `--json`'s `complete` agrees with the readiness result.
#[test]
fn a_usable_shell_does_not_hide_failed_repairs_or_manual_steps() {
    let scratch = Scratch::new("exit");
    let ran = run(&scratch, Mark::Fail, &[], true);
    assert_eq!(ran.code, 1, "{}", ran.text);
    assert!(ran.text.contains("readiness: NOT READY"), "{}", ran.text);
    let json = run(&scratch, Mark::Fail, &["--json"], true);
    assert_eq!(json.code, 1);
    assert!(json.json, "the schema goes to stdout whatever the code");
    assert!(!report(&json).complete);

    let broken = broken("exit-broken");
    // A program whose build ALab withdrew: no local step fixes it.
    broken.install("az", 3);
    atpkg::activate::install_tombstone_shim(&broken.layout(), &ToolName::new("az").unwrap())
        .unwrap();
    let ran = run(&broken, Mark::Ok, &[], false);
    assert_eq!(ran.code, 1, "{}", ran.text);
    let repair = section(&ran.text, "repair:");
    assert!(
        mark(&repair) == "warn" && repair.contains("NOT done"),
        "{repair}"
    );
    assert!(ran.text.contains("readiness: NOT READY"), "{}", ran.text);
    assert!(!ran.text.contains("health: OK"), "{}", ran.text);
    let json = run(&broken, Mark::Ok, &["--json"], false);
    assert_eq!(json.code, 1);
    let json = report(&json);
    assert!(!json.complete);
    assert!(
        json.items
            .iter()
            .any(|row| row.id == aterm_cli::doctor::SHELL && row.outcome == Outcome::Ok)
    );
    assert!(!json.readiness.unwrap().all_green);
}

/// Each result is supplied without platform probes or writes. Nonessential rows still
/// count toward readiness, while notes/not-applicable remain nonfatal.
#[test]
fn readiness_distinguishes_confirmed_state_from_plans_manual_steps_and_unknowns() {
    for outcome in [
        Outcome::Ok,
        Outcome::Changed,
        Outcome::NotApplicable,
        Outcome::Planned,
        Outcome::Person,
        Outcome::Blocked,
        Outcome::Deferred,
        Outcome::Unknown,
        Outcome::Failed,
    ] {
        let mut row = super::unmeasured("fixture.check", "fixture", None, "evidence".into());
        row.outcome = outcome;
        let rows = vec![row];
        let ran = injected(Mark::Ok, &["--json", "--only", "fixture"], &rows);
        let report = report(&ran);
        let ready = matches!(
            outcome,
            Outcome::Ok | Outcome::Changed | Outcome::NotApplicable
        );
        assert_eq!(ran.code, u8::from(!ready), "{outcome:?}: {}", ran.text);
        assert_eq!(report.complete, ready, "{outcome:?}");
        let summary = report.readiness.unwrap();
        assert_eq!(summary.all_green, ready, "{outcome:?}");
        assert_eq!(summary.scope, ["fixture"]);
        assert_eq!(
            summary.not_applicable,
            usize::from(outcome == Outcome::NotApplicable)
        );
        assert_eq!(summary.unresolved.is_empty(), ready, "{outcome:?}");
        if !ready {
            assert_eq!(summary.unresolved, ["fixture.check"]);
        }
    }
    let note = super::run(
        &args(&["--read-only", "--json", "--only", "privacy"]),
        &Ports {
            gather: &|_| {
                Gathered::of_facts(&[
                    ("shell:", Mark::Ok, "/bin/sh (executable)"),
                    ("privacy:", Mark::Note, "not applicable to this host"),
                ])
            },
            ground: None,
            packages: &Vec::new,
            state: None,
            not_own_account: None,
        },
    );
    assert_eq!(note.code, 0, "{}", note.text);
    assert_eq!(report(&note).readiness.unwrap().not_applicable, 1);
}

fn injected(shell: Mark, words: &[&str], rows: &[ItemReport]) -> Ran {
    super::run(
        &args(words),
        &Ports {
            gather: &|_| facts(shell),
            ground: None,
            packages: &|| rows.to_vec(),
            state: None,
            not_own_account: None,
        },
    )
}

#[test]
fn filtered_readiness_excludes_unrequested_checks_but_keeps_shell_usability() {
    let mut selected = super::unmeasured("fixture.ready", "fixture", None, "ready".into());
    selected.outcome = Outcome::Ok;
    let omitted = super::unmeasured("fixture.unknown", "fixture", None, "unreadable".into());
    let rows = [selected, omitted];
    let words = ["--read-only", "--json", "--only", "fixture.ready"];
    let ran = injected(Mark::Ok, &words, &rows);
    assert_eq!(ran.code, 0, "{}", ran.text);
    let json = report(&ran);
    assert_eq!(json.items.len(), 2, "selected check and shell only");
    assert_eq!(
        json.readiness.unwrap(),
        Readiness {
            all_green: true,
            scope: vec!["fixture.ready".into()],
            satisfied: 2,
            not_applicable: 0,
            pending_activation: vec![],
            unresolved: vec![],
        }
    );
    let text = injected(Mark::Ok, &["--read-only", "--only", "fixture.ready"], &rows);
    assert!(text.text.contains("readiness: ALL GREEN"), "{}", text.text);
    assert!(
        text.text
            .contains("scope: fixture.ready (plus shell usability)"),
        "{}",
        text.text
    );
    let broken_shell = injected(Mark::Fail, &words, &rows);
    assert_eq!(broken_shell.code, 1);
    assert_eq!(
        report(&broken_shell).readiness.unwrap().unresolved,
        [aterm_cli::doctor::SHELL]
    );
}

#[test]
fn a_missing_store_is_unknown_in_full_scope_but_does_not_block_a_shell_only_check() {
    let full = injected(Mark::Ok, &["--read-only", "--json"], &[]);
    assert_eq!(full.code, 1);
    let json = report(&full);
    assert!(
        json.items
            .iter()
            .any(|row| row.id == "base.pkg.store" && row.outcome == Outcome::Unknown)
    );
    assert!(
        json.readiness
            .unwrap()
            .unresolved
            .contains(&"base.pkg.store".to_owned())
    );
    let shell = injected(
        Mark::Ok,
        &["--read-only", "--json", "--only", aterm_cli::doctor::SHELL],
        &[],
    );
    assert_eq!(shell.code, 0, "{}", shell.text);
    assert_eq!(report(&shell).items.len(), 1);
}

#[test]
fn changed_settings_awaiting_login_or_restart_are_not_green() {
    for effect in [
        aterm_converge::Effect::Login,
        aterm_converge::Effect::Restart,
    ] {
        let mut row = super::unmeasured("fixture.setting", "fixture", None, "configured".into());
        row.outcome = Outcome::Changed;
        row.effect = effect;
        let rows = [row];
        let ran = injected(Mark::Ok, &["--json", "--only", "fixture"], &rows);
        assert_eq!(ran.code, 1);
        let readiness = report(&ran).readiness.unwrap();
        assert_eq!(readiness.pending_activation, ["fixture.setting"]);
        assert!(readiness.unresolved.is_empty());
        assert_eq!(readiness.satisfied, 1, "only the shell is satisfied");
        let text = injected(Mark::Ok, &["--only", "fixture"], &rows);
        assert!(
            text.text
                .contains("pending activation (login/restart): fixture.setting"),
            "{}",
            text.text
        );
    }
}

#[test]
fn an_accepted_selector_without_a_reported_check_is_not_green() {
    let ran = injected(
        Mark::Ok,
        &["--json", "--only", "base.machine.spotlight"],
        &[],
    );
    assert_eq!(ran.code, 1);
    let json = report(&ran);
    assert_eq!(
        json.readiness.unwrap().unresolved,
        ["doctor.selection.base.machine.spotlight"]
    );
}

/// `--only` narrows the items and the packages' rows to what it names (the shell is
/// always read as a prerequisite); a word that names nothing is the usage refusal, and
/// nothing has been changed when it is said.
#[test]
fn only_narrows_the_run_and_a_word_naming_nothing_is_refused() {
    let scratch = broken("only");
    let json = report(&run(
        &scratch,
        Mark::Ok,
        &["--json", "--only", "base.pkg.shims"],
        true,
    ));
    let ids: Vec<&str> = json.items.iter().map(|row| row.id.as_str()).collect();
    assert!(json.complete, "the selected shim repair was confirmed");
    assert!(json.readiness.as_ref().unwrap().all_green);
    assert!(ids.contains(&"base.shell.starts"), "{ids:?}");
    assert!(ids.contains(&repair::SHIMS), "{ids:?}");
    assert!(!ids.contains(&repair::LINKS), "{ids:?}");
    assert!(!ids.contains(&"base.privacy.fda"), "{ids:?}");
    assert!(
        std::fs::symlink_metadata(scratch.link()).is_err(),
        "links were not asked for"
    );
    assert!(atpkg::platform::resolve_shim(&scratch.shim("ay")).is_some());

    let before = scratch.tree();
    let refused = run(&scratch, Mark::Ok, &["--only", "links,no-such-thing"], true);
    assert_eq!(refused.code, 2, "{}", refused.text);
    assert!(refused.text.contains("no-such-thing"), "{}", refused.text);
    assert!(!refused.text.contains("links:"), "{}", refused.text);
    assert_eq!(scratch.tree(), before, "refused before anything changed");
}

/// `--ONLY IS CHECKED BEFORE ANYTHING IS READ`: a word that names nothing is refused
/// before the facts are gathered, so `--only privacy,typo` never opens the Developer Tools
/// pane on its way to exit 2; a word that names a row of another platform's doctor is
/// still a name. The hand-over runs only where the run may fix and its words reach the
/// Developer Tools row: never for `--read-only`, never for `--only pkg`.
#[test]
fn only_is_checked_before_the_facts_are_gathered_and_hands_over_only_where_it_reaches() {
    let scratch = Scratch::new("hand-over");
    let (refused, gathers) = run_as(
        &scratch,
        Mark::Ok,
        &["--only", "privacy,no-such-thing"],
        true,
        None,
    );
    assert_eq!(refused.code, 2, "{}", refused.text);
    assert!(refused.text.contains("no-such-thing"), "{}", refused.text);
    assert_eq!(
        gathers,
        Vec::<bool>::new(),
        "refused before anything is read"
    );
    for (words, hand_over) in [
        (&[][..], true),
        (&["--only", "privacy"][..], true),
        (&["--only", "base.privacy.devtools"][..], true),
        (&["--only", "pkg"][..], false),
        (&["--only", "base.pkg.shims"][..], false),
        (&["--only", "base.shell.conpty"][..], false),
        (&["--read-only"][..], false),
        (&["--read-only", "--only", "privacy"][..], false),
    ] {
        let (ran, gathers) = run_as(&scratch, Mark::Ok, words, false, None);
        assert!(ran.code <= 1, "{words:?}: {}", ran.text);
        assert_eq!(gathers, vec![hand_over], "{words:?}: gathered once");
    }
}

/// OUTSIDE THE ACCOUNT'S OWN SHELL (root, another `$HOME`, an isolated instance's private
/// state root) doctor reads every item and changes nothing: the scratch home, prefix and
/// state root hash the same (no journal, no store lock file), the Developer Tools are not
/// handed over, and the `repair:` row says what is not as laid and why it was left. The
/// shell is still read, and the unrepaired checks stay non-green. The negative control is bare
/// doctor over the same tree in the account's own shell, which repairs it.
#[test]
fn outside_the_accounts_own_shell_doctor_reads_every_item_and_changes_nothing() {
    let scratch = broken("not-own");
    let before = scratch.tree();
    let hold = aterm_converge::not_own_account("it runs as root");
    for shell in [Mark::Ok, Mark::Fail] {
        let expected_code = 1;
        let (ran, gathers) = run_as(&scratch, shell, &[], true, Some("it runs as root"));
        assert_eq!(ran.code, expected_code, "{}", ran.text);
        assert_eq!(
            scratch.tree(),
            before,
            "changed outside its own shell:\n{}",
            ran.text
        );
        assert_eq!(gathers, vec![false], "no hand-over");
        let repair = section(&ran.text, "repair:");
        assert_eq!(mark(&repair), "warn", "{repair}");
        assert!(repair.contains(&hold), "{repair}");
        assert!(
            repair.contains("installed programs' commands: ")
                && !repair.contains("bare `aterm doctor` restores it"),
            "{repair}"
        );
        let (json, gathers) = run_as(&scratch, shell, &["--json"], true, Some("it runs as root"));
        assert_eq!(json.code, expected_code, "the shell is read: {}", json.text);
        assert_eq!(gathers, vec![false], "JSON does not hand over");
        let json = report(&json);
        assert_eq!(json.complete, expected_code == 0);
        let shims = json
            .items
            .iter()
            .find(|row| row.id == repair::SHIMS)
            .unwrap();
        assert_eq!(shims.outcome, Outcome::Blocked);
        assert_eq!(shims.why.as_deref(), Some(hold.as_str()));
        assert!(shims.from.is_some(), "what is not as laid is named");
        assert_eq!(scratch.tree(), before);
    }

    run(&scratch, Mark::Ok, &[], true);
    assert_ne!(scratch.tree().0, before.0, "its own shell repairs");
}

/// A STORE LOCK THAT CANNOT BE TAKEN (here a directory where `store.lock` goes) is not an
/// install pass holding it: nothing is repaired or journaled, and the `repair:` row warns
/// and names each change it left, where contention is only a note.
#[test]
fn a_store_lock_that_cannot_be_taken_is_a_warning_and_nothing_is_repaired() {
    let scratch = broken("lock-io");
    std::fs::create_dir(scratch.layout().store_lock()).unwrap();
    let ran = run(&scratch, Mark::Ok, &[], true);
    assert_eq!(ran.code, 1, "{}", ran.text);
    let repair = section(&ran.text, "repair:");
    assert_eq!(mark(&repair), "warn", "{repair}");
    assert!(
        repair.contains("the store lock could not be taken"),
        "{repair}"
    );
    assert!(!repair.contains(CONTENDED), "{repair}");
    assert!(
        repair.contains("installed programs' commands: "),
        "{repair}"
    );
    assert!(
        std::fs::symlink_metadata(scratch.shim("ay")).is_err(),
        "nothing restored"
    );
    assert!(
        !scratch.root.join("state/converge").exists(),
        "no journal line for a change nobody made"
    );
    let json = report(&run(&scratch, Mark::Ok, &["--json"], true));
    let shims = json
        .items
        .iter()
        .find(|row| row.id == repair::SHIMS)
        .unwrap();
    assert_eq!(shims.outcome, Outcome::Blocked);
    assert_eq!(shims.stood_aside, None, "not contention: {shims:?}");
}

/// INIT'S BASE LAYER AND THE STORE LOCK: held by an install pass, each item stands aside
/// on the lock (init's 75, "retry when it exits"); not to be taken for another reason (a
/// directory where `store.lock` goes), each change is held back and nothing stands aside
/// (init's 1: no retry takes it). Neither changes anything. Free, init holds the lock for
/// its base items; a plan takes nothing.
#[cfg(target_os = "macos")]
#[test]
fn init_stands_aside_for_an_install_pass_and_fails_on_a_lock_it_cannot_take() {
    let scratch = broken("init-lock");
    let ground = scratch.ground();
    let shims = |base: aterm_machines::init::Base| {
        assert!(base.held.is_none());
        aterm_converge::run_scoped(
            &base.items,
            aterm_converge::Mode::Apply,
            aterm_converge::Scope::EVERY,
            &mut aterm_converge::ReadOnly,
            &mut |_| {},
        )
        .unwrap()
        .into_iter()
        .find(|row| row.id == repair::SHIMS)
        .unwrap()
    };
    let held = atpkg::lock::try_lock_store(&scratch.layout()).expect("the test holds the lock");
    let row = shims(super::init_base_over(&ground, true));
    assert_eq!(row.outcome, Outcome::Blocked);
    assert!(
        matches!(row.stood_aside, Some(aterm_converge::StoodAside::Lock(_))),
        "{row:?}"
    );
    drop(held);

    let lock = scratch.layout().store_lock();
    std::fs::remove_file(&lock).unwrap();
    std::fs::create_dir(&lock).unwrap();
    let row = shims(super::init_base_over(&ground, true));
    assert_eq!(row.outcome, Outcome::Blocked);
    assert_eq!(row.stood_aside, None, "{row:?}");
    assert!(
        row.why
            .as_deref()
            .is_some_and(|why| why.contains("not changed: the store lock could not be taken")),
        "{row:?}"
    );
    assert!(
        std::fs::symlink_metadata(scratch.shim("ay")).is_err(),
        "nothing restored"
    );
    std::fs::remove_dir(&lock).unwrap();

    assert!(super::init_base_over(&ground, false).held.is_none());
    let base = super::init_base_over(&ground, true);
    assert!(base.held.is_some());
    assert!(matches!(
        atpkg::lock::try_lock_store(&scratch.layout()),
        Err(atpkg::lock::StoreLockError::Contended(_))
    ));
    drop(base);
    assert!(atpkg::lock::try_lock_store(&scratch.layout()).is_ok());
}

/// The front door hands `aterm doctor` to the composed doctor, and leaves `aterm doctor
/// --help` to the diagnostics' help.
#[test]
fn the_front_door_routes_doctor_here_and_its_help_to_the_diagnostics() {
    let os = |words: &[&str]| -> Vec<std::ffi::OsString> {
        words.iter().map(std::ffi::OsString::from).collect()
    };
    assert!(crate::routes_to_doctor("doctor", &[]));
    assert!(crate::routes_to_doctor(
        "doctor",
        &os(&["--read-only", "--json"])
    ));
    assert!(
        crate::routes_to_doctor("doctor", &os(&["--bogus"])),
        "the grammar refuses it there"
    );
    assert!(!crate::routes_to_doctor("doctor", &os(&["--help"])));
    assert!(!crate::routes_to_doctor("doctor", &os(&["--json", "-h"])));
    assert!(!crate::routes_to_doctor("show-config", &[]));
}

/// `base.clt` reads a C toolchain verdict as the install's to fix only where the tools are
/// missing: a refusing compiler is installed already, and one that did not answer is
/// unknown.
#[cfg(target_os = "macos")]
#[test]
fn base_clt_reads_a_missing_toolchain_as_the_install_s_and_nothing_else() {
    use aterm_machines::init_clt::Found;
    use atpkg::prereq::CcVerdict;
    let cc = PathBuf::from("/usr/bin/cc");
    assert!(matches!(
        super::clt_found(&CcVerdict::NoDeveloperDir),
        Found::Missing(_)
    ));
    assert!(matches!(
        super::clt_found(&CcVerdict::NoDriver),
        Found::Missing(_)
    ));
    assert!(matches!(
        super::clt_found(&CcVerdict::Ready { driver: cc.clone() }),
        Found::Present(_)
    ));
    assert!(matches!(
        super::clt_found(&CcVerdict::Broken {
            driver: cc.clone(),
            why: "license".into()
        }),
        Found::Refuses(_)
    ));
    assert!(matches!(
        super::clt_found(&CcVerdict::Unanswered { driver: cc }),
        Found::Unknown(_)
    ));
    assert!(matches!(
        super::clt_found(&CcVerdict::NotProbed),
        Found::NotHere(_)
    ));
}
