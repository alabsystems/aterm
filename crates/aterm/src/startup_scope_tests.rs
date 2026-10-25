// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Startup's scope: a launch on the account's own home may start first-run setup and one on a
//! scratch home never does, and startup's first record observation must belong to a resolved
//! account home.

use super::{
    PreparedSetup, SetupAdmission, SetupStartup, doctor_startup_env, doctor_startup_env_from,
    first_run_window_eligible, prepare_setup,
};
use aterm_cli::macos_doctor::{
    AutomaticSetupClaim, InitialSetupObservation, InitialSetupSnapshot, ProvisionalSetupWatchTest,
    claim_initial_setup_rerun_at, initial_setup_scope_ready_at, lasting_scope_refusal_at,
};
use aterm_gui::WorkstationSetupDecision;
use aterm_types::macos_setup::Plan;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

struct Startup {
    home: PathBuf,
    account_home: Option<PathBuf>,
    hidden: Option<PathBuf>,
    binds: usize,
    claims: usize,
    backoffs: usize,
    failures: Vec<String>,
    /// The backoffs and binds before each bring-forward.
    brought: Vec<(usize, usize)>,
}

impl Startup {
    fn root(&self) -> PathBuf {
        self.home.join("doctor")
    }

    fn snapshot(&self) -> Result<InitialSetupSnapshot, String> {
        ProvisionalSetupWatchTest::at(&self.root())
            .snapshot()
            .map_err(|why| why.to_string())
    }
}

impl SetupStartup for Startup {
    type Claim = AutomaticSetupClaim;

    fn scope_ready(&mut self) -> Result<bool, String> {
        initial_setup_scope_ready_at(
            &self.home,
            self.account_home.as_deref().unwrap_or(&self.home),
        )
    }

    fn bring_forward(&mut self) {
        self.brought.push((self.backoffs, self.binds));
    }

    fn bind_authorization(&mut self) -> Result<u64, String> {
        self.binds += 1;
        assert_eq!(self.binds, 1, "capture the resolved home's records once");
        Ok(self.snapshot()?.revision)
    }

    fn revision(&mut self) -> Result<u64, String> {
        Ok(self.snapshot()?.revision)
    }

    fn claim(&mut self) -> Result<Option<Self::Claim>, String> {
        self.claims += 1;
        if !self.scope_ready()? {
            return Ok(None);
        }
        claim_initial_setup_rerun_at(&self.root())
    }

    fn finished(&mut self) -> Result<bool, String> {
        Ok(self.snapshot()?.finished)
    }

    fn admission(&mut self) -> Result<Option<SetupAdmission>, String> {
        // The real startup admission reader, via the existing scratch fixture: the admitted
        // pending run is returned without any record write, as the before/after pins prove.
        let revision = ProvisionalSetupWatchTest::at(&self.root()).recover()?;
        Ok(Some(SetupAdmission {
            revision,
            completed: false,
        }))
    }

    fn initial(&mut self) -> Result<Plan, String> {
        panic!("an admitted pending run resumes without another offer")
    }

    fn offer(&mut self, _: Plan) -> WorkstationSetupDecision {
        panic!("scope recovery must not show another offer")
    }

    fn authorize(&mut self, _: &Plan) -> Result<Option<u64>, String> {
        panic!("scope recovery must not reauthorize the saved run")
    }

    fn authorization_pending(&self) -> bool {
        false
    }

    fn wait_holding_claim(&mut self) -> bool {
        panic!("no accepted choice is being saved")
    }

    fn backoff(&mut self) {
        self.backoffs += 1;
        let hidden = self
            .hidden
            .take()
            .expect("only unresolved scope retries; record capture errors must end startup");
        std::fs::rename(hidden, &self.home).unwrap();
    }

    fn report_failure(&mut self, why: &str) {
        self.failures.push(why.to_owned());
    }
}

fn pending(home: &Path) -> (u64, InitialSetupObservation) {
    std::fs::create_dir(home).unwrap();
    let root = home.join("doctor");
    let run = ProvisionalSetupWatchTest::new(&root).unwrap();
    run.choose().unwrap();
    run.leave_permissions_pending().unwrap();
    let saved = run.snapshot().unwrap();
    assert!(!saved.finished);
    (saved.revision, saved.observation)
}

fn startup(home: &Path) -> Startup {
    Startup {
        home: home.to_path_buf(),
        account_home: None,
        hidden: None,
        binds: 0,
        claims: 0,
        backoffs: 0,
        failures: Vec::new(),
        brought: Vec::new(),
    }
}

/// The window gate's scope input is the doctor's lasting refusal, through the one composition
/// the launch reads: the account's own home keeps first-run setup, a scratch home never starts
/// it, and the environment's markers still refuse on the account's home.
#[test]
fn a_real_home_keeps_first_run_setup_and_a_scratch_home_never_starts_it() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let account = scratch.path().join("account");
    let other = scratch.path().join("scratch-home");
    std::fs::create_dir(&account).unwrap();
    std::fs::create_dir(&other).unwrap();
    let window = [OsString::from("--window")];
    let at =
        |home: &Path| doctor_startup_env_from(|_| None, lasting_scope_refusal_at(home, &account));

    let own = at(&account);
    assert!(!own.outside_person_scope);
    assert!(first_run_window_eligible(&window, own));
    assert!(first_run_window_eligible(&[], own), "a Dock launch");
    let foreign = at(&other);
    assert!(foreign.outside_person_scope);
    assert!(!first_run_window_eligible(&window, foreign));
    for marker in ["ATERM_STATE_HOME", "CI", "SSH_CONNECTION", "SSH_TTY"] {
        let env = doctor_startup_env_from(
            |key| (key == marker).then(|| OsString::from("1")),
            lasting_scope_refusal_at(&account, &account),
        );
        assert!(!first_run_window_eligible(&window, env), "{marker}");
    }

    // The launch's own reader: a test binary runs under its private state root, a lasting
    // refusal, so this process is never the person's.
    assert!(doctor_startup_env().outside_person_scope);
}

#[test]
fn startup_scope_refusal_never_captures_or_claims() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let (_, before) = pending(&home);
    let account = scratch.path().join("actual-account");
    std::fs::create_dir(&account).unwrap();
    let mut host = startup(&home);
    host.account_home = Some(account);

    assert!(matches!(
        prepare_setup(&mut host).unwrap(),
        PreparedSetup::Unavailable
    ));
    assert_eq!((host.binds, host.claims, host.backoffs), (0, 0, 0));
    assert!(
        host.brought.is_empty(),
        "nothing is brought forward for a refused scope"
    );
    assert!(host.failures.is_empty());
    assert_eq!(host.snapshot().unwrap().observation, before);
}

#[test]
fn startup_scope_recovery_captures_the_revealed_pending_run_once() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let (revision, before) = pending(&home);
    let hidden = scratch.path().join("not-mounted");
    std::fs::rename(&home, &hidden).unwrap();
    let mut host = startup(&home);
    host.hidden = Some(hidden);

    let prepared = prepare_setup(&mut host).unwrap();
    assert!(
        matches!(prepared, PreparedSetup::Authorized { revision: got, .. } if got == revision),
        "the resolved home must resume its existing run, not retire an empty baseline"
    );
    assert_eq!(host.binds, 1);
    assert_eq!(host.claims, 1, "scope waits before taking a claim");
    assert_eq!(host.backoffs, 1);
    assert_eq!(
        host.brought,
        [(1, 0)],
        "a restored setup is brought forward once the home resolves, before the records bind"
    );
    assert_eq!(host.failures.len(), 1);
    assert_eq!(host.snapshot().unwrap().observation, before);
}

#[test]
fn startup_scope_recovery_does_not_retry_a_failed_resolved_baseline() {
    for reveal in [false, true] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let home = scratch.path().join("home");
        pending(&home);
        let record = home.join("doctor/initial-setup.json");
        std::fs::write(&record, b"{").unwrap();
        let mut host = startup(&home);
        if reveal {
            let hidden = scratch.path().join("not-mounted");
            std::fs::rename(&home, &hidden).unwrap();
            host.hidden = Some(hidden);
        }

        assert!(prepare_setup(&mut host).is_err());
        assert_eq!(host.binds, 1);
        assert_eq!(host.claims, 0, "a failed baseline takes no claim");
        assert_eq!(host.backoffs, usize::from(reveal));
        assert_eq!(std::fs::read(record).unwrap(), b"{");
    }
}
