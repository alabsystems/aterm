// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[cfg(unix)]
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::super::accounts::{self, Auth, Cli, Kind, Row};
#[cfg(unix)]
use super::super::agy_quota_store::LAUNCH_READ_BUDGET;
use super::super::agy_quota_store::SIGNED_OUT;
use super::*;

const NOW: i64 = 1_792_803_313;

/// A harness state, a user home and a config path in one temp dir, with the agy gate over them.
struct Bed {
    root: aterm_tempfile::TempDir,
    state: PathBuf,
    home: PathBuf,
    config: PathBuf,
}

impl Bed {
    fn new(tag: &str) -> Self {
        let root = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-agy-gate-{tag}-"))
            .tempdir()
            .expect("temp dir");
        let state = root.path().join("state");
        let home = root.path().join("home");
        std::fs::create_dir_all(&state).expect("state");
        std::fs::create_dir_all(&home).expect("home");
        let config = root.path().join("aterm.toml");
        Self {
            root,
            state,
            home,
            config,
        }
    }

    /// The gate of the person's own agy login.
    fn own(&self) -> Gate {
        Gate::at(&self.state, &self.home, Some(&self.config), None)
    }

    /// The gate of the login in `dir`.
    fn at(&self, dir: &Path) -> Gate {
        Gate::at(&self.state, &self.home, Some(&self.config), Some(dir))
    }

    /// List `rows` in the agy roster beside the config.
    fn list(&self, rows: &[Row]) {
        let path = accounts::roster_beside_for(&self.config, Cli::Agy).expect("roster path");
        std::fs::write(
            path,
            accounts::append_rows_for(Cli::Agy, "", rows).expect("roster text"),
        )
        .expect("roster");
    }

    /// The word on record for `label`.
    fn auth(&self, label: &str) -> Auth {
        let (state, _) = self.own().store().load(NOW + 1_000).expect("state");
        state.account(label).auth
    }

    /// The ledger's `(kind, account, why)` rows.
    fn ledger(&self) -> Vec<(Kind, String, String)> {
        self.own()
            .store()
            .tail(50)
            .expect("ledger")
            .into_iter()
            .map(|event| (event.kind, event.account, event.why))
            .collect()
    }
}

/// Mark this machine's own agy login working, as a served turn does.
fn serve(gate: &Gate, at: i64) -> bool {
    gate.served(Some("person@example.com"), at, at)
}

#[test]
fn a_login_nothing_is_known_of_is_not_admitted() {
    let bed = Bed::new("unknown");
    let gate = bed.own();
    let refusal = gate.admit(&Proof::None, NOW).expect_err("unknown");
    assert!(matches!(refusal, Refusal::NotKnown { .. }), "{refusal:?}");
    assert!(
        refusal.words().starts_with(NOT_KNOWN),
        "{}",
        refusal.words()
    );
    assert!(!refusal.signed_out());
    // Asking wrote nothing: the store is as absent as it was.
    assert!(!gate.store().dir.join("state.json").exists());
    // CONTROL: the same login, once a served turn is on record, is admitted.
    assert!(serve(&gate, NOW));
    assert_eq!(
        gate.admit(&Proof::None, NOW + 1).map(|pass| pass.at()),
        Ok(NOW + 1)
    );
}

#[test]
fn a_served_turn_marks_this_machines_own_login_working_once() {
    let bed = Bed::new("served");
    let gate = bed.own();
    assert!(serve(&gate, NOW), "the first served turn is news");
    assert_eq!(bed.auth("main"), Auth::Ok);
    assert!(
        bed.ledger()
            .iter()
            .any(|(kind, account, _)| *kind == Kind::SignedIn && account == "main"),
        "a signed-in row for the journal: {:?}",
        bed.ledger()
    );
    let rows = bed.ledger().len();
    // A later turn on a login already on record as working records nothing.
    assert!(!serve(&gate, NOW + 60));
    assert_eq!(bed.ledger().len(), rows);
}

#[test]
fn a_sign_out_on_record_holds_until_a_turn_served_after_it() {
    let bed = Bed::new("held");
    let gate = bed.own();
    assert!(serve(&gate, NOW));
    gate.signed_out(Some("person@example.com"), NOW + 10)
        .expect("recorded");
    assert_eq!(bed.auth("main"), Auth::SignedOut);
    assert!(
        bed.ledger()
            .iter()
            .any(|(kind, account, why)| *kind == Kind::Auth
                && account == "main"
                && why == "signed-out"),
        "the ledger row the journal carries as signed-out: {:?}",
        bed.ledger()
    );
    let refusal = gate.admit(&Proof::None, NOW + 20).expect_err("held");
    assert_eq!(
        refusal,
        Refusal::SignedOut {
            name: "main".into(),
            since: Some(NOW + 10)
        }
    );
    assert!(
        refusal.words().starts_with(SIGNED_OUT),
        "{}",
        refusal.words()
    );
    // A turn served before the sign-out is not news of the login since: the gate holds.
    assert!(!serve(&gate, NOW + 5));
    assert!(gate.admit(&Proof::None, NOW + 30).is_err());
    // Even a grant file does not open a gate a prompt closed: the grant is what expired.
    assert!(gate.admit(&Proof::Grant, NOW + 30).is_err());
    // One served after it is: the person signed in again in a tab.
    assert!(serve(&gate, NOW + 40));
    assert_eq!(bed.auth("main"), Auth::Ok);
    assert!(gate.admit(&Proof::None, NOW + 50).is_ok());
}

#[test]
fn a_pool_rows_login_is_the_account_of_its_directory() {
    let bed = Bed::new("row");
    let dir = bed.home.join("logins").join("one");
    std::fs::create_dir_all(&dir).expect("login dir");
    bed.list(&[
        Row {
            label: "own".into(),
            email: Some("own@example.com".into()),
            ..Row::default()
        },
        Row {
            label: "one".into(),
            email: Some("one@example.com".into()),
            dir: Some(dir.to_str().expect("utf-8").into()),
            ..Row::default()
        },
    ]);
    let one = bed.at(&dir);
    // Nothing on record, and its grant file is here: the file is the proof.
    assert!(one.admit(&Proof::Grant, NOW).is_ok());
    // The same row with no grant file is signed out, named by its email.
    let refusal = one
        .admit(&Proof::NoGrant(dir.join(".gemini")), NOW)
        .expect_err("no grant");
    assert!(refusal.signed_out());
    assert!(
        refusal.words().contains("one@example.com"),
        "{}",
        refusal.words()
    );
    // A prompt met on it is its record, not the own login's.
    one.signed_out(None, NOW + 1).expect("recorded");
    assert_eq!(bed.auth("one"), Auth::SignedOut);
    assert_eq!(bed.auth("own"), Auth::Unseen);
    assert!(one.admit(&Proof::Grant, NOW + 2).is_err());
    // The person's own home is the row with no directory, whatever it is called.
    let own = bed.own();
    assert!(matches!(
        own.admit(&Proof::None, NOW + 2),
        Err(Refusal::NotKnown { name }) if name == "own@example.com"
    ));
    // The person's own home named outright is the same login.
    assert!(matches!(
        bed.at(&bed.home).admit(&Proof::None, NOW + 2),
        Err(Refusal::NotKnown { name }) if name == "own@example.com"
    ));
}

#[test]
fn a_home_no_account_answers_to_passes_only_on_its_grant_file() {
    let bed = Bed::new("unlisted");
    let dir = bed.root.path().join("elsewhere");
    let gate = bed.at(&dir);
    assert_eq!(
        gate.admit(&Proof::None, NOW).unwrap_err(),
        Refusal::Unlisted
    );
    assert!(gate.admit(&Proof::Grant, NOW).is_ok());
    assert!(
        gate.admit(&Proof::NoGrant(dir.clone()), NOW)
            .unwrap_err()
            .signed_out()
    );
    // There is no account to record on, and none is invented.
    assert!(gate.signed_out(None, NOW).is_err());
    assert!(!gate.served(None, NOW, NOW));
    assert!(bed.ledger().is_empty());
}

/// An agy stand-in in `dir` (`harness::stand_in::proven`): `body` runs when it is given arguments.
#[cfg(unix)]
fn stand_in(dir: &Path, body: &str) -> PathBuf {
    let agy = dir.join("agy");
    crate::harness::stand_in::proven(&agy, body);
    agy
}

#[cfg(unix)]
fn read(gate: &Gate, program: &Path, at: i64) -> Result<Captured, String> {
    let pass = gate.admit(&Proof::None, at).map_err(|r| r.words())?;
    gate.read(
        pass,
        program.as_os_str(),
        &[OsString::from("-p"), OsString::from("/usage")],
        &[],
        (LAUNCH_READ_BUDGET, 64 * 1024),
        Some("person@example.com"),
    )
}

/// A read that meets agy's sign-in prompt records the login signed out, and the next read runs
/// no agy at all.
#[cfg(unix)]
#[test]
fn a_prompt_a_read_meets_is_recorded_and_the_gate_then_holds() {
    let bed = Bed::new("prompt");
    let gate = bed.own();
    assert!(serve(&gate, NOW));
    let parked = aterm_spec::wait::PARKED_SECS;
    let agy = stand_in(
        bed.root.path(),
        &format!(
            "echo run >> \"$0.runs\"\n\
             printf 'Authentication required. Please visit the URL to log in: https://accounts.example.invalid/\\n'\n\
             exec /bin/sleep {parked}"
        ),
    );
    let runs = || {
        std::fs::read_to_string(bed.root.path().join("agy.runs"))
            .map_or(0, |text| text.lines().count())
    };
    let why = read(&gate, &agy, NOW + 10).expect_err("a prompt is no reading");
    assert!(why.starts_with(SIGNED_OUT), "{why}");
    assert_eq!(runs(), 1, "the admitted read ran agy once");
    assert_eq!(bed.auth("main"), Auth::SignedOut);
    // Five minutes on, an hour on: no agy process, only the gate's answer.
    for later in [NOW + 301, NOW + 3_700] {
        let why = read(&gate, &agy, later).expect_err("the gate holds");
        assert!(why.starts_with(SIGNED_OUT), "{why}");
    }
    assert_eq!(runs(), 1, "no later read started agy");
}

/// CONTROL: with the prompt left unrecorded, every later read starts agy again, which is what
/// `a_prompt_a_read_meets_is_recorded_and_the_gate_then_holds` forbids.
#[cfg(unix)]
#[test]
fn control_an_unrecorded_prompt_lets_every_later_read_start_agy() {
    let bed = Bed::new("prompt-control");
    let gate = bed.own();
    assert!(serve(&gate, NOW));
    let parked = aterm_spec::wait::PARKED_SECS;
    let agy = stand_in(
        bed.root.path(),
        &format!(
            "echo run >> \"$0.runs\"\n\
             printf 'Authentication required. Please visit the URL to log in: https://accounts.example.invalid/\\n'\n\
             exec /bin/sleep {parked}"
        ),
    );
    PROMPT_UNRECORDED.set(true);
    let first = read(&gate, &agy, NOW + 10);
    let second = read(&gate, &agy, NOW + 301);
    PROMPT_UNRECORDED.set(false);
    assert!(first.is_err() && second.is_err());
    assert_eq!(bed.auth("main"), Auth::Ok, "nothing was recorded");
    let runs = std::fs::read_to_string(bed.root.path().join("agy.runs")).expect("runs");
    assert_eq!(runs.lines().count(), 2, "agy was started for each read");
}

/// What the stand-in does to reach the desk: run `osascript`, which prints `1` and touches
/// nothing where it may run. Under the read's sandbox its exec is denied, as `open`'s is.
#[cfg(target_os = "macos")]
const REACH_DESK: &str = "/usr/bin/osascript -e 'return 1' > \"$0.desk\" 2>/dev/null\n\
                          printf '{\"status\":\"SUCCESS\"}\\n'";

/// A read's agy cannot reach the desk: the program that would open its sign-in page is denied.
#[cfg(target_os = "macos")]
#[test]
fn a_reads_agy_cannot_reach_the_desk() {
    let bed = Bed::new("off-screen");
    let gate = bed.own();
    assert!(serve(&gate, NOW));
    let agy = stand_in(bed.root.path(), REACH_DESK);
    let got = read(&gate, &agy, NOW + 10).expect("the read itself ran");
    assert!(got.success(), "agy ran and answered: {got:?}");
    assert_eq!(
        std::fs::read_to_string(bed.root.path().join("agy.desk")).expect("the stand-in tried"),
        "",
        "the desk program did not run"
    );
}

/// CONTROL: run as any child, the same stand-in reaches the desk program, so
/// `a_reads_agy_cannot_reach_the_desk` is the sandbox's doing.
#[cfg(target_os = "macos")]
#[test]
fn control_a_child_outside_the_sandbox_reaches_the_desk() {
    let bed = Bed::new("on-desk");
    let gate = bed.own();
    assert!(serve(&gate, NOW));
    let agy = stand_in(bed.root.path(), REACH_DESK);
    ON_DESK.set(true);
    let got = read(&gate, &agy, NOW + 10);
    ON_DESK.set(false);
    assert!(got.expect("ran").success());
    assert_eq!(
        std::fs::read_to_string(bed.root.path().join("agy.desk"))
            .expect("the stand-in tried")
            .trim(),
        "1"
    );
}

/// The command a read runs is the sandbox's on macOS, and never agy bare.
#[cfg(target_os = "macos")]
#[test]
fn a_read_is_built_under_the_no_desk_sandbox() {
    let scratch = aterm_tempfile::tempdir().expect("temp dir");
    let args = [OsString::from("-p"), OsString::from("/usage")];
    let (cmd, _held) = off_screen("agy".as_ref(), &args, scratch.path()).expect("command");
    assert_eq!(cmd.get_program(), crate::harness::login_drive::SANDBOX_EXEC);
    let argv: Vec<OsString> = cmd.get_args().map(OsString::from).collect();
    assert_eq!(
        argv,
        [
            "-p",
            crate::harness::login_drive::NO_DESK,
            "agy",
            "-p",
            "/usage"
        ]
        .map(OsString::from)
    );
}
