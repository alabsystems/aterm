// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The composed `aterm doctor` over a scratch home, prefix and state root (design S5).

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
            gather: &|fix, _| {
                gathers.borrow_mut().push(fix);
                facts(shell)
            },
            ground: Some(&ground),
            packages: &|| scratch.packages(),
            state: journaled.then_some(state.as_path()),
            not_own_account: not_own,
            develop: None,
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

/// `--json` TIMES THE RUN: every row, a fact's, a repair's, a package's and the missing
/// evidence's alike, carries its time, and `phases` names each step of the run, read-only and
/// applying (presence only, never a budget). An old report without either still reads.
#[test]
fn every_json_row_carries_its_time_and_the_run_its_phases() {
    let scratch = broken("timed");
    for words in [&["--read-only", "--json"][..], &["--json"]] {
        let json = report(&run(&scratch, Mark::Ok, words, true));
        assert_eq!(
            json.phases.keys().map(String::as_str).collect::<Vec<_>>(),
            ["census", "develop", "gather", "lock", "packages", "repairs"],
            "{words:?}"
        );
        assert!(
            json.items.iter().any(|row| row.family == "pkg"),
            "{words:?}"
        );
        let untimed: Vec<&str> = json
            .items
            .iter()
            .filter(|row| row.elapsed_ms.is_none())
            .map(|row| row.id.as_str())
            .collect();
        assert!(untimed.is_empty(), "{words:?}: {untimed:?}");
    }
    let missing = report(&injected(Mark::Ok, &["--read-only", "--json"], &[]));
    assert!(
        missing
            .items
            .iter()
            .any(|row| row.id == "base.pkg.store" && row.elapsed_ms == Some(0))
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
    assert!(
        ran.stdout,
        "a NOT READY report goes to stdout too: `aterm doctor | less` reads it"
    );
    let json = run(&scratch, Mark::Fail, &["--json"], true);
    assert_eq!(json.code, 1);
    assert!(json.stdout, "the schema goes to stdout whatever the code");
    assert!(!report(&json).complete);

    let broken = broken("exit-broken");
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
            gather: &|_, _| {
                Gathered::of_facts(&[
                    ("shell:", Mark::Ok, "/bin/sh (executable)"),
                    ("privacy:", Mark::Note, "not applicable to this host"),
                ])
            },
            ground: None,
            packages: &Vec::new,
            state: None,
            not_own_account: None,
            develop: None,
        },
    );
    assert_eq!(note.code, 0, "{}", note.text);
    assert_eq!(report(&note).readiness.unwrap().not_applicable, 1);
}

fn injected(shell: Mark, words: &[&str], rows: &[ItemReport]) -> Ran {
    super::run(
        &args(words),
        &Ports {
            gather: &|_, _| facts(shell),
            ground: None,
            packages: &|| rows.to_vec(),
            state: None,
            not_own_account: None,
            develop: None,
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
/// still a name. The hand-over is left due only where the run may fix, prints a report a
/// person reads, and its words reach the Developer Tools row: never for `--read-only`,
/// never for `--json` (a document's reader), never for `--only pkg`.
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
    assert!(!refused.stdout, "a usage refusal goes to stderr");
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
        (&["--json"][..], false),
        (&["--json", "--only", "base.privacy.devtools"][..], false),
    ] {
        let (ran, gathers) = run_as(&scratch, Mark::Ok, words, false, None);
        assert!(ran.code <= 1, "{words:?}: {}", ran.text);
        assert_eq!(gathers, vec![hand_over], "{words:?}: gathered once");
    }
}

/// A run's reads over `scratch`: what it answered, the `--only` words each gather was
/// handed, and how many times the packages were read.
fn reads(scratch: &Scratch, words: &[&str]) -> (Ran, Vec<Vec<String>>, usize) {
    let ground = scratch.ground();
    let state = scratch.root.join("state");
    let gathers = std::sync::Mutex::new(Vec::new());
    let packages = std::sync::atomic::AtomicUsize::new(0);
    let ran = super::run(
        &args(words),
        &Ports {
            gather: &|_, only| {
                gathers.lock().unwrap().push(only.to_vec());
                facts(Mark::Ok)
            },
            ground: Some(&ground),
            packages: &|| {
                packages.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                scratch.packages()
            },
            state: Some(&state),
            not_own_account: None,
            develop: None,
        },
    );
    (ran, gathers.into_inner().unwrap(), packages.into_inner())
}

/// `--ONLY NARROWS WHAT IS READ, NOT ONLY WHAT IS PRINTED`: the help's "shell usability
/// alone" check reads no package row (no word names one) and hands the gather its words, so
/// it reads only the shell's facts; a word is judged by the names alone, so a packages row
/// this machine does not report is still a name. NEGATIVE CONTROL: with no words the
/// packages are read once, and a repair that changed something reads them again after it
/// (the first read, made beside the repairs, may be stale).
#[test]
fn only_reads_what_its_words_name() {
    let scratch = Scratch::new("only-reads");
    let (ran, gathers, packages) = reads(&scratch, &["--read-only", "--only", "base.shell.starts"]);
    assert_eq!(ran.code, 0, "{}", ran.text);
    assert_eq!(packages, 0, "no word names a packages row");
    assert_eq!(gathers, [["base.shell.starts"]]);

    let (ran, _, packages) = reads(
        &scratch,
        &["--read-only", "--only", "base.pkg.toolset.platform"],
    );
    assert!(
        ran.code <= 1,
        "a name, read and not reported here: {}",
        ran.text
    );
    assert_eq!(packages, 1);

    let (ran, gathers, packages) = reads(&scratch, &["--read-only"]);
    assert!(ran.code <= 1, "{}", ran.text);
    assert_eq!(packages, 1, "no words: read once");
    assert_eq!(gathers, [Vec::<String>::new()]);

    let broken = broken("only-reads-changed");
    let (ran, _, packages) = reads(&broken, &["--json"]);
    assert!(
        report(&ran)
            .items
            .iter()
            .any(|row| row.outcome == Outcome::Changed),
        "{}",
        ran.text
    );
    assert_eq!(packages, 2, "read again after the repair changed something");
}

/// THE PACKAGES ARE READ BESIDE THE GATHER, NOT AFTER IT: each waits on the same two-party
/// barrier, which only a run that starts both at once passes. The one-minute wait is the
/// hang detector a serial run trips (the gather then the repairs then the packages: the
/// gather's wait never returns).
#[test]
fn the_packages_are_read_beside_the_gather() {
    let scratch = Scratch::new("beside");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let ground = scratch.ground();
        let both = std::sync::Barrier::new(2);
        let ran = super::run(
            &args(&["--read-only"]),
            &Ports {
                gather: &|_, _| {
                    both.wait();
                    facts(Mark::Ok)
                },
                ground: Some(&ground),
                packages: &|| {
                    both.wait();
                    Vec::new()
                },
                state: None,
                not_own_account: None,
                develop: None,
            },
        );
        let _ = tx.send(ran.code);
    });
    let code = rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the gather and the packages' read never met: one waited for the other");
    assert!(code <= 1);
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

/// `base.clt` reads a C toolchain verdict, beside the git init runs, as the install's to
/// fix only where the tools are missing: a refusing compiler is installed already, and one
/// that did not answer is unknown.
#[cfg(target_os = "macos")]
#[test]
fn base_clt_reads_a_missing_toolchain_as_the_install_s_and_nothing_else() {
    use aterm_machines::init_clt::Found;
    use atpkg::prereq::CcVerdict;
    let cc = PathBuf::from("/usr/bin/cc");
    assert!(matches!(
        super::clt_found(&CcVerdict::NoDeveloperDir, true),
        Found::Missing(_)
    ));
    assert!(matches!(
        super::clt_found(&CcVerdict::NoDriver, true),
        Found::Missing(_)
    ));
    assert!(matches!(
        super::clt_found(&CcVerdict::Ready { driver: cc.clone() }, true),
        Found::Present(_)
    ));
    assert!(matches!(
        super::clt_found(
            &CcVerdict::Broken {
                driver: cc.clone(),
                why: "license".into()
            },
            true
        ),
        Found::Refuses(_)
    ));
    assert!(matches!(
        super::clt_found(&CcVerdict::Unanswered { driver: cc }, true),
        Found::Unknown(_)
    ));
    assert!(matches!(
        super::clt_found(&CcVerdict::NotProbed, true),
        Found::NotHere(_)
    ));
}

/// A develop layer stand-in (macOS's is `aterm_machines::init::Doctor`): the rows its items
/// read, whether its profile could not be read, and what doctor asked of it.
#[derive(Default)]
struct Stand {
    rows: Vec<ItemReport>,
    unreadable: bool,
    /// Each composition's `asked`.
    composed: std::cell::RefCell<Vec<bool>>,
    /// Each read's `--only` words.
    read: std::cell::RefCell<Vec<Vec<String>>>,
}

impl Stand {
    /// A Mac whose `init.toml` records `~/aterm`: its four rows, the checkout one commit
    /// behind origin as last fetched.
    fn recorded() -> Self {
        let mut checkout = develop_row("develop.checkout@~/aterm", "checkout", Outcome::Planned);
        checkout.from = Some("1 commit behind origin/main, as last fetched".into());
        checkout.to = Some("fast-forwarded to origin/main at 0123456789ab".into());
        checkout.explain = Some(aterm_converge::Explain {
            why: "behind".into(),
            remedy: aterm_converge::Remedy::Develop,
        });
        let mut tools = develop_row(
            "develop.workspace-tools@~/aterm",
            "tooling",
            Outcome::NotApplicable,
        );
        tools.why = Some("none of the rows asked for is pinned for this host".into());
        Self {
            rows: vec![
                develop_row("develop.record", "profile", Outcome::Ok),
                checkout,
                develop_row("develop.submodules@~/aterm", "checkout", Outcome::Ok),
                tools,
            ],
            ..Self::default()
        }
    }

    /// A Mac whose `init.toml` cannot be read: one record row saying why.
    fn broken() -> Self {
        let mut record = develop_row("develop.record", "profile", Outcome::Unknown);
        record.why = Some("invalid ~/.config/aterm/init.toml".into());
        Self {
            rows: vec![record],
            unreadable: true,
            ..Self::default()
        }
    }
}

fn develop_row(id: &str, family: &str, outcome: Outcome) -> ItemReport {
    let mut row = super::unmeasured(
        id,
        family,
        Some(aterm_converge::Layer::Develop),
        String::new(),
    );
    row.title = id.to_owned();
    row.outcome = outcome;
    row.why = None;
    if outcome == Outcome::Ok {
        row.to = Some(format!("{id} as read"));
    }
    row
}

/// `aterm_machines::init_develop::names` for the stand-in (that crate is macOS's alone).
fn develop_word(word: &str) -> bool {
    word == "develop"
        || word.starts_with("develop.")
        || ["profile", "checkout", "tooling"].contains(&word)
}

/// [`run_as`] with `stand` as the develop layer.
fn run_developing(
    scratch: &Scratch,
    words: &[&str],
    stand: &Stand,
    not_own: Option<&'static str>,
) -> (Ran, Vec<bool>) {
    let ground = scratch.ground();
    let gathers = std::cell::RefCell::new(Vec::new());
    let compose = |asked| {
        stand.composed.borrow_mut().push(asked);
        super::DevelopLayer {
            items: stand
                .rows
                .iter()
                .map(|row| (row.id.clone(), row.family.clone()))
                .collect(),
            unreadable: stand.unreadable,
            rows: Box::new(move |only: &[String]| {
                stand.read.borrow_mut().push(only.to_vec());
                Ok(stand
                    .rows
                    .iter()
                    .filter(|row| {
                        only.is_empty()
                            || stand.unreadable
                            || only.iter().any(|word| super::named_by(word, row))
                    })
                    .cloned()
                    .collect())
            }),
        }
    };
    let ran = super::run(
        &args(words),
        &Ports {
            gather: &|fix, _| {
                gathers.borrow_mut().push(fix);
                facts(Mark::Ok)
            },
            ground: Some(&ground),
            packages: &|| scratch.packages(),
            state: None,
            not_own_account: not_own,
            develop: Some(super::Develop {
                names: &develop_word,
                compose: &compose,
            }),
        },
    );
    (ran, gathers.into_inner())
}

fn develop_ids(json: &Report) -> Vec<&str> {
    json.items
        .iter()
        .filter(|row| row.layer == Some(aterm_converge::Layer::Develop))
        .map(|row| row.id.as_str())
        .collect()
}

/// A DEVELOP MACHINE'S DOCTOR READS ITS DEVELOP ROWS beside every other (design §5, S9): the
/// record, the checkout, its submodule and its tools are rows of the report, bare and
/// `--read-only` alike, under a `develop:` section whose behind checkout names `aterm init
/// --develop`, and a planned one is unresolved. The layer is composed once from what the Mac
/// records (nothing asked of it) and read once, after the repair. Negative control: the same
/// machine with no develop layer reports none of them.
#[test]
fn a_develop_machine_s_doctor_reads_its_develop_rows_beside_the_others() {
    let scratch = Scratch::new("develop-rows");
    for words in [&["--read-only", "--json"][..], &["--json"][..]] {
        let stand = Stand::recorded();
        let (ran, _) = run_developing(&scratch, words, &stand, None);
        assert_eq!(ran.code, 1, "{}", ran.text);
        let json = report(&ran);
        assert_eq!(
            develop_ids(&json),
            [
                "develop.record",
                "develop.checkout@~/aterm",
                "develop.submodules@~/aterm",
                "develop.workspace-tools@~/aterm"
            ],
            "{words:?}"
        );
        assert!(
            json.items
                .iter()
                .any(|row| row.id == aterm_cli::doctor::SHELL)
        );
        assert!(
            json.readiness
                .unwrap()
                .unresolved
                .contains(&"develop.checkout@~/aterm".to_owned())
        );
        assert_eq!(*stand.composed.borrow(), [false]);
        assert_eq!(*stand.read.borrow(), [Vec::<String>::new()]);
    }
    let stand = Stand::recorded();
    let (ran, _) = run_developing(&scratch, &["--read-only"], &stand, None);
    let develop = section(&ran.text, "develop:");
    assert_eq!(mark(&develop), "warn", "{develop}");
    assert!(
        develop.contains(
            "develop.checkout@~/aterm: 1 commit behind origin/main, as last fetched → \
             fast-forwarded to origin/main at 0123456789ab — `aterm init --develop`"
        ),
        "{develop}"
    );
    assert!(
        develop.contains("develop.workspace-tools@~/aterm: not applicable: none of the rows"),
        "{develop}"
    );
    assert!(ran.text.contains("shell:"), "{}", ran.text);

    let ran = run(&scratch, Mark::Ok, &["--read-only", "--json"], false);
    assert_eq!(develop_ids(&report(&ran)), Vec::<&str>::new());
    assert!(
        !run(&scratch, Mark::Ok, &["--read-only"], false)
            .text
            .contains("develop:")
    );
}

/// A MAC THAT RECORDS NO `[develop]` TABLE HAS NO DEVELOP ROW: its layer composes nothing,
/// so the report has no develop row and no `develop:` section, and `--only develop` reads its
/// record row (the stand-in's: none) without refusing.
#[test]
fn a_mac_without_a_develop_record_has_no_develop_row() {
    let scratch = Scratch::new("develop-none");
    let stand = Stand::default();
    let (ran, _) = run_developing(&scratch, &["--read-only", "--json"], &stand, None);
    assert_eq!(develop_ids(&report(&ran)), Vec::<&str>::new());
    let (ran, _) = run_developing(&scratch, &["--read-only"], &stand, None);
    assert!(!ran.text.contains("develop:"), "{}", ran.text);
    assert_eq!(*stand.composed.borrow(), [false, false]);
}

/// A BROKEN `init.toml` IS ONE ROW AND NEVER STOPS DOCTOR'S OTHER ROWS: the develop layer
/// that cannot be read is its one `develop.record` row (unknown, saying why, under a warning
/// `develop:` section), and the shell, the facts, the repair items and the packages are read
/// and reported as ever; an `--only` word naming a checkout is not refused (the record row
/// answers it). `--only shell` names no develop word, so no profile is read at all.
#[test]
fn a_broken_init_toml_is_one_develop_row_and_never_stops_the_others() {
    let scratch = broken("develop-broken");
    let stand = Stand::broken();
    let (ran, _) = run_developing(&scratch, &["--read-only", "--json"], &stand, None);
    let json = report(&ran);
    assert_eq!(develop_ids(&json), ["develop.record"]);
    let record = json
        .items
        .iter()
        .find(|row| row.id == "develop.record")
        .unwrap();
    assert_eq!(record.outcome, Outcome::Unknown);
    for id in [
        aterm_cli::doctor::SHELL,
        "base.privacy.fda",
        repair::SHIMS,
        repair::LINKS,
    ] {
        assert!(
            json.items.iter().any(|row| row.id == id),
            "{id}: {:?}",
            json.items
        );
    }
    assert!(
        json.items
            .iter()
            .any(|row| row.layer == Some(aterm_converge::Layer::Base) && row.family == "pkg"),
        "the packages are read"
    );
    let (ran, _) = run_developing(&scratch, &["--read-only"], &stand, None);
    let develop = section(&ran.text, "develop:");
    assert_eq!(mark(&develop), "warn", "{develop}");
    assert!(
        develop.contains("invalid ~/.config/aterm/init.toml"),
        "{develop}"
    );
    assert!(section(&ran.text, "repair:").contains("bare `aterm doctor` restores it"));

    let (ran, _) = run_developing(
        &scratch,
        &[
            "--read-only",
            "--json",
            "--only",
            "develop.checkout@~/aterm",
        ],
        &stand,
        None,
    );
    assert_eq!(ran.code, 1, "{}", ran.text);
    assert_eq!(develop_ids(&report(&ran)), ["develop.record"]);

    let stand = Stand::broken();
    let (ran, _) = run_developing(
        &scratch,
        &["--read-only", "--json", "--only", "shell"],
        &stand,
        None,
    );
    assert_eq!(develop_ids(&report(&ran)), Vec::<&str>::new());
    assert!(stand.composed.borrow().is_empty(), "no profile is read");
}

/// A DEVELOP WORD THAT NAMES NO ITEM OF THIS MAC IS THE USAGE REFUSAL, said before the facts
/// are gathered (nothing handed over) and before any develop item is read, naming the
/// checkouts it records. Negative control: the word for a recorded checkout narrows the
/// develop rows to that one, composed as asked.
#[test]
fn a_develop_word_naming_no_item_of_this_mac_is_refused_before_anything_is_read() {
    let scratch = Scratch::new("develop-only");
    let stand = Stand::recorded();
    let (ran, gathers) = run_developing(
        &scratch,
        &["--only", "develop.checkout@~/elsewhere"],
        &stand,
        None,
    );
    assert_eq!(ran.code, 2, "{}", ran.text);
    assert!(
        ran.text.contains(
            "names no develop item of this Mac (its checkouts: develop.checkout@~/aterm)"
        ),
        "{}",
        ran.text
    );
    assert!(gathers.is_empty(), "nothing gathered or handed over");
    assert!(stand.read.borrow().is_empty(), "no develop item read");

    let stand = Stand::recorded();
    let (ran, _) = run_developing(
        &scratch,
        &[
            "--read-only",
            "--json",
            "--only",
            "develop.checkout@~/aterm",
        ],
        &stand,
        None,
    );
    assert_eq!(develop_ids(&report(&ran)), ["develop.checkout@~/aterm"]);
    assert_eq!(*stand.composed.borrow(), [true]);
    assert_eq!(
        *stand.read.borrow(),
        [vec!["develop.checkout@~/aterm".to_owned()]]
    );
}

/// OUTSIDE THE ACCOUNT'S OWN SHELL NO DEVELOP LAYER IS READ (root, another `$HOME`, a private
/// state root: the checkouts and their code are the account's): no profile is read, no row is
/// reported, and `--only develop` is an unmeasured selection, never green.
#[test]
fn outside_the_account_s_own_shell_no_develop_layer_is_read() {
    let scratch = Scratch::new("develop-not-own");
    let stand = Stand::recorded();
    let not_own = Some("$HOME is not this account's home");
    let (ran, _) = run_developing(&scratch, &["--read-only", "--json"], &stand, not_own);
    assert_eq!(develop_ids(&report(&ran)), Vec::<&str>::new());
    let (ran, _) = run_developing(
        &scratch,
        &["--read-only", "--json", "--only", "develop"],
        &stand,
        not_own,
    );
    assert_eq!(ran.code, 1, "{}", ran.text);
    assert_eq!(
        report(&ran).readiness.unwrap().unresolved,
        ["doctor.selection.develop"]
    );
    assert!(stand.composed.borrow().is_empty());
}

/// `base.ai.accounts` names each other CLI's pool this machine keeps after Claude Code's
/// (`…; other CLIs: codex 1, kimi 1`), and stands on those pools alone where Claude Code has no
/// source. CONTROL: a machine with Claude Code's pool alone reads Claude Code's probe word for
/// word.
#[cfg(target_os = "macos")]
#[test]
fn base_ai_accounts_names_each_kept_pool_after_claudes() {
    use aterm_agent::harness::cli::Env;
    use aterm_converge::Probe;
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-doctor-pools-")
        .tempdir()
        .expect("a scratch directory");
    let root = dir.path().canonicalize().unwrap();
    let (home, config, state) = (root.join("home"), root.join("config"), root.join("state"));
    for d in [&home, &config, &state] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(config.join("aterm.toml"), "[fleet]\nmachine = \"m16\"\n").unwrap();
    std::fs::write(
        config.join("accounts.toml"),
        "[[account]]\nlabel = \"m16\"\nclass = \"dedicated\"\nemail = \"claude-one@example.com\"\n",
    )
    .unwrap();
    std::fs::write(
        config.join("fleet-accounts.toml"),
        "opt_in = \"named\"\n\n[[account]]\nlabel = \"m16\"\nmachines = [\"m16\"]\n\
         email = \"claude-one@example.com\"\n",
    )
    .unwrap();
    let env = Env {
        state,
        aterm_state: None,
        cwd: home.clone(),
        home: Some(home.clone()),
        claude_dir: Some(home.join(".claude")),
        claude_config_unusable: None,
        claude_profile: Some(home.join(".claude.json")),
        claude_custom_oauth: false,
        claude_session: None,
        claude_pid: None,
        codex_thread: None,
        agy_conversation: None,
        codex_home: None,
        now: 1_792_660_000,
        tz_s: 0,
        sid: String::new(),
        sock: None,
        config: Some(config.join("aterm.toml")),
        claude_tmp: None,
        claude_running: None,
    };
    {
        use aterm_agent::harness::{accounts, accounts_sync};
        let roster = config.join("accounts.toml");
        let read = accounts::Roster::load(&roster);
        assert!(read.as_ref().is_ok_and(|r| !r.rows.is_empty()), "{read:?}");
        assert!(
            accounts_sync::configured_source(Some(&config.join("aterm.toml")), Some(&roster))
                .is_some(),
            "the fleet file opts in by name"
        );
        assert!(!accounts_sync::needs_init_accounts_sync(
            &home, &env.state, &roster, env.now
        ));
    }
    let Probe::Ok { now: claude } = super::ai_accounts_claude_probe(&env) else {
        panic!(
            "the fixture's Claude Code pool is synced: {:?}",
            super::ai_accounts_claude_probe(&env)
        );
    };
    assert!(
        claude.starts_with("1 accounts synced (1 enrolled and not signed out"),
        "{claude}"
    );
    assert_eq!(
        format!("{:?}", super::ai_accounts_probe(&env)),
        format!(
            "{:?}",
            Probe::Ok {
                now: claude.clone()
            }
        ),
        "CONTROL: Claude Code's pool alone reads as it always did"
    );

    for cli in ["codex", "kimi"] {
        std::fs::write(
            config.join(format!("accounts-{cli}.toml")),
            "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n",
        )
        .unwrap();
    }
    assert_eq!(
        format!("{:?}", super::ai_accounts_probe(&env)),
        format!(
            "{:?}",
            Probe::Ok {
                now: format!("{claude}; other CLIs: codex 1, kimi 1")
            }
        )
    );

    for cli in [
        aterm_agent::harness::accounts::Cli::Claude,
        aterm_agent::harness::accounts::Cli::Kimi,
    ] {
        for damaged in ["state", "logins"] {
            let mut unknown = env.clone();
            unknown.state = root.join(format!("unknown-{cli:?}-{damaged}"));
            let store = aterm_agent::harness::accounts::Store::at_for(&unknown.state, cli, &home);
            store.ensure().unwrap();
            let path = if damaged == "state" {
                store.state_file()
            } else {
                store.logins_file()
            };
            std::fs::write(&path, "{").unwrap();
            assert!(matches!(
                super::ai_accounts_probe(&unknown),
                Probe::Unusable { .. }
            ));
            assert_eq!(std::fs::read(path).unwrap(), b"{");
        }
    }
    std::fs::remove_file(config.join("accounts.toml")).unwrap();
    std::fs::remove_file(config.join("fleet-accounts.toml")).unwrap();
    assert!(matches!(
        super::ai_accounts_claude_probe(&env),
        Probe::NotApplicable { .. }
    ));
    assert_eq!(
        format!("{:?}", super::ai_accounts_probe(&env)),
        format!(
            "{:?}",
            Probe::Ok {
                now: "other CLIs: codex 1, kimi 1".to_owned()
            }
        ),
        "the other pools stand alone where Claude Code has no source"
    );
}

/// `base.clt` plans the install wherever the git init runs is missing (the tools', else
/// Xcode's), beside a compiler that builds or refuses too: the GitHub setup's clones need it,
/// and System State opens the install's tab for exactly that wait. A compiler that did not
/// answer stays unknown.
#[cfg(target_os = "macos")]
#[test]
fn base_clt_wants_the_tools_git_beside_an_xcode_compiler() {
    use aterm_machines::init_clt::Found;
    use atpkg::prereq::CcVerdict;

    let driver = PathBuf::from("/Applications/Xcode.app/Contents/Developer/usr/bin/clang");
    let ready = CcVerdict::Ready {
        driver: driver.clone(),
    };
    let broken = CcVerdict::Broken {
        driver: driver.clone(),
        why: "You have not agreed to the Xcode license agreements.".into(),
    };
    assert!(matches!(super::clt_found(&ready, true), Found::Present(_)));
    assert!(matches!(super::clt_found(&ready, false), Found::Missing(_)));
    assert!(matches!(super::clt_found(&broken, true), Found::Refuses(_)));
    assert!(matches!(
        super::clt_found(&broken, false),
        Found::Missing(_)
    ));
    assert!(matches!(
        super::clt_found(&CcVerdict::Unanswered { driver }, false),
        Found::Unknown(_)
    ));
}

/// Every `aterm init` line this doctor prints for the base layer it composes, and the ones the
/// crates it composes print for the Command Line Tools (aterm-uds's tool table, atpkg's C
/// toolchain act), is one init takes over that base layer (`base.ai.accounts`, `base.clt`,
/// `base`): each remedy rendered as a row's tail, then copied out of the text as a person
/// would. A misspelt id is the negative control.
#[cfg(target_os = "macos")]
#[test]
fn every_init_line_doctor_prints_is_one_init_takes() {
    use aterm_converge::{Explain, Item as _, Probe, Remedy};
    let base = aterm_machines::init::Base {
        items: vec![Box::new(super::AiAccountsItem)],
        held: None,
        clt: super::command_line_tools(),
        targo: None,
    };
    assert!(base.clt.is_some(), "the test binary names itself");
    let tail = |remedy: Remedy| {
        let mut row = super::unmeasured("row", "family", None, String::new());
        row.explain = Some(Explain {
            why: String::new(),
            remedy,
        });
        super::remedy(&row)
    };
    let texts = [
        tail(
            super::AiAccountsItem
                .explain(&Probe::Blocked { why: String::new() })
                .remedy,
        ),
        tail(aterm_machines::init_clt::place().remedy),
        tail(Remedy::Init {
            only: aterm_converge::Layer::Base.name().to_owned(),
        }),
        aterm_uds::host::tool("cc").remedy_on("darwin-arm64"),
        atpkg::prereq::act().to_owned(),
    ];
    for text in &texts {
        let lines = aterm_machines::init::init_lines(text);
        assert!(!lines.is_empty(), "{text}");
        for line in lines {
            assert_eq!(
                aterm_machines::init::refusal(&line, &base, &[]),
                None,
                "{line} (from {text})"
            );
        }
    }
    let misspelt = tail(Remedy::Init {
        only: "base.ai.acounts".to_owned(),
    });
    let line = &aterm_machines::init::init_lines(&misspelt)[0];
    assert!(
        aterm_machines::init::refusal(line, &base, &[]).is_some(),
        "{line}"
    );
}
