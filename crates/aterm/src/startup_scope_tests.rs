// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Startup's scope: a launch on the account's own home may start first-run setup and one on a
//! scratch home never does, and startup's first record observation must belong to a resolved
//! account home.

use super::{
    DoctorStartupEnv, PreparedSetup, SetupAdmission, SetupStartup, doctor_startup_env,
    doctor_startup_env_from, first_lan_uses_wait_for_the_step, first_run_window_eligible,
    ordinary_window, prepare_setup,
};
use aterm_cli::macos_doctor::{
    AutomaticSetupClaim, InitialSetupObservation, InitialSetupSnapshot, ProvisionalSetupWatchTest,
    claim_initial_setup_rerun_at, initial_setup_scope_ready_at, lasting_scope_refusal_at,
    set_aside_unreadable_setup_at,
};
use aterm_gui::WorkstationSetupDecision;
use aterm_types::macos_setup::{Plan, Setting};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy)]
enum AfterBind {
    Corrupt,
    TemporarilyUnreadable,
}

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
    /// Bring-forwards still to meet the profile lock busy.
    busy: usize,
    /// The last bring-forward met it busy: the next backoff waits for it to let go.
    lock_waits: bool,
    /// The records aterm set aside, and the backoffs and binds before.
    set_aside: Vec<(Vec<String>, usize, usize)>,
    /// The records start afresh: no run to resume, so the startup offers its plan, which the
    /// page admits as it stands.
    fresh: bool,
    /// The plans the startup offered.
    offered: Vec<Plan>,
    /// A real local read failure that starts after the startup captured readable state.
    after_bind: Option<AfterBind>,
    /// An update successor's startup, which may show no review.
    successor: bool,
    /// The reviews it withheld.
    withheld: usize,
    /// The admitted runs whose plan the startup offered back for the window's own answer.
    resumed: Vec<SetupAdmission>,
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

    fn set_aside(&mut self) -> Result<(), String> {
        if let Some(set) = set_aside_unreadable_setup_at(&self.root(), false)? {
            self.set_aside.push((set.kept, self.backoffs, self.binds));
        }
        Ok(())
    }

    fn recover_after_read_error(&mut self) -> Result<bool, String> {
        let before = self.set_aside.len();
        self.set_aside()?;
        Ok(self.set_aside.len() != before)
    }

    fn bring_forward(&mut self) -> Result<bool, String> {
        self.brought.push((self.backoffs, self.binds));
        self.lock_waits = self.busy > 0;
        if self.lock_waits {
            self.busy -= 1;
        }
        Ok(!self.lock_waits)
    }

    fn bind_authorization(&mut self) -> Result<u64, String> {
        self.binds += 1;
        assert_eq!(self.binds, 1, "capture the resolved home's records once");
        let revision = self.snapshot()?.revision;
        let path = self.root().join("initial-setup.json");
        match self.after_bind {
            Some(AfterBind::Corrupt) => std::fs::write(path, b"{").unwrap(),
            Some(AfterBind::TemporarilyUnreadable) => {
                std::fs::rename(&path, self.root().join("progress-held")).unwrap();
                std::fs::create_dir(path).unwrap();
            }
            None => {}
        }
        Ok(revision)
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
        if self.fresh {
            return Ok(None);
        }
        // The real startup admission reader, via the existing scratch fixture: the admitted
        // pending run is returned without any record write, as the before/after pins prove.
        let revision = ProvisionalSetupWatchTest::at(&self.root()).recover()?;
        Ok(Some(SetupAdmission {
            plan: self.snapshot()?.plan,
            revision,
            completed: false,
        }))
    }

    fn offer_resume(&mut self, admission: &SetupAdmission) {
        self.resumed.push(admission.clone());
    }

    fn may_offer(&mut self) -> bool {
        self.withheld += usize::from(self.successor);
        !self.successor
    }

    fn initial(&mut self) -> Result<Plan, String> {
        assert!(
            self.fresh,
            "an admitted pending run resumes without another offer"
        );
        assert!(!self.successor, "a withheld review reads no plan");
        Ok(self.snapshot()?.plan)
    }

    fn offer(&mut self, plan: Plan) -> WorkstationSetupDecision {
        assert!(self.fresh, "scope recovery must not show another offer");
        self.offered.push(plan.clone());
        WorkstationSetupDecision::Begin(plan)
    }

    fn authorize(&mut self, _: &Plan) -> Result<Option<u64>, String> {
        assert!(
            self.fresh,
            "scope recovery must not reauthorize the saved run"
        );
        Ok(Some(1))
    }

    fn authorization_pending(&self) -> bool {
        false
    }

    fn wait_holding_claim(&mut self) -> bool {
        panic!("no accepted choice is being saved")
    }

    fn backoff(&mut self) {
        self.backoffs += 1;
        if std::mem::take(&mut self.lock_waits) {
            return;
        }
        if matches!(self.after_bind, Some(AfterBind::TemporarilyUnreadable)) {
            let path = self.root().join("initial-setup.json");
            std::fs::remove_dir(&path).unwrap();
            std::fs::rename(self.root().join("progress-held"), path).unwrap();
            self.after_bind = None;
            return;
        }
        assert!(
            !matches!(self.after_bind, Some(AfterBind::Corrupt)),
            "newly corrupt records must retire this bound review so fresh recovery can run"
        );
        let hidden = self
            .hidden
            .take()
            .expect("only unresolved scope retries: a corrupt record is set aside at once");
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
        busy: 0,
        lock_waits: false,
        set_aside: Vec::new(),
        fresh: false,
        offered: Vec::new(),
        after_bind: None,
        successor: false,
        withheld: 0,
        resumed: Vec::new(),
    }
}

/// An update successor never shows a review: on records nobody admitted a run of, it reads no
/// plan, offers nothing and leaves setup as it was, where a launch the person started offers the
/// plan. A run already admitted resumes in the successor as in any launch, and either offers its
/// admitted plan back for the window's own answer (design §3.3, §6): the page answers it only with
/// a person at this Mac.
#[test]
fn an_update_successor_resumes_an_admitted_run_and_never_offers_a_review() {
    for successor in [true, false] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let home = scratch.path().join("home");
        let (_, before) = pending(&home);
        let mut host = startup(&home);
        host.fresh = true;
        host.successor = successor;
        let prepared = prepare_setup(&mut host).unwrap();
        if successor {
            assert!(matches!(prepared, PreparedSetup::Unavailable));
            assert!(host.offered.is_empty(), "{:?}", host.offered);
            assert_eq!(host.withheld, 1);
            assert_eq!(host.snapshot().unwrap().observation, before);
        } else {
            assert!(matches!(
                prepared,
                PreparedSetup::Authorized { revision: 1, .. }
            ));
            assert_eq!(host.offered.len(), 1, "a cold launch offers its plan");
            assert_eq!(host.withheld, 0);
        }

        let scratch = aterm_tempfile::tempdir().unwrap();
        let home = scratch.path().join("home");
        let (revision, _) = pending(&home);
        let mut host = startup(&home);
        host.successor = successor;
        assert!(matches!(
            prepare_setup(&mut host).unwrap(),
            PreparedSetup::Authorized { revision: resumed, .. } if resumed == revision
        ));
        assert!(host.offered.is_empty());
        assert_eq!(
            host.withheld, 0,
            "an admitted run asks nothing of the review"
        );
        let saved = host.snapshot().unwrap().plan;
        assert!(saved.selection.contains(Setting::Permissions));
        assert_eq!(
            host.resumed,
            [SetupAdmission {
                plan: saved,
                revision,
                completed: false,
            }],
            "a launch and a successor alike offer the admitted plan back (successor {successor})"
        );
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
    let at = |home: &Path| {
        doctor_startup_env_from(|_| None, lasting_scope_refusal_at(home, &account), || false)
    };

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
            || false,
        );
        assert!(!first_run_window_eligible(&window, env), "{marker}");
    }
    // Outside the console owner's graphic session (no window server, another login at the
    // screen), with no marker set, the window leaves setup to one started at this Mac.
    let away = doctor_startup_env_from(
        |_| None,
        lasting_scope_refusal_at(&account, &account),
        || true,
    );
    assert!(away.remote);
    assert!(!first_run_window_eligible(&window, away));

    // The launch's own reader: a test binary runs under its private state root, a lasting
    // refusal, so this process is never the person's.
    assert!(doctor_startup_env().outside_person_scope);
}

/// A window's other first LAN uses wait for System State's Local Network step only where the
/// window runs first-run setup, whose System State runs the step: one started over ssh or away
/// from the console, under a private state root or outside the person's scope, one that runs a
/// command, and a launch that is not ordinary wait on no step this window will not take.
#[test]
fn only_a_window_that_runs_setup_holds_its_lan_uses_for_the_step() {
    fn start() -> bool {
        true
    }
    let osv = |list: &[&str]| -> Vec<OsString> { list.iter().map(OsString::from).collect() };
    let waits = |args: &[OsString], env: DoctorStartupEnv| {
        let start: fn() -> bool = start;
        first_lan_uses_wait_for_the_step(
            ordinary_window(args),
            first_run_window_eligible(args, env).then_some(start),
        )
    };
    let at_the_mac = DoctorStartupEnv::default();
    assert!(waits(&osv(&["--window"]), at_the_mac));
    assert!(waits(&[], at_the_mac), "a Dock launch");
    for (env, why) in [
        (
            DoctorStartupEnv {
                remote: true,
                ..at_the_mac
            },
            "over ssh, or away from the console",
        ),
        (
            DoctorStartupEnv {
                isolated: true,
                ..at_the_mac
            },
            "a private state root or CI",
        ),
        (
            DoctorStartupEnv {
                outside_person_scope: true,
                ..at_the_mac
            },
            "outside the person's scope",
        ),
    ] {
        assert!(
            ordinary_window(&osv(&["--window"])),
            "{why}: still an ordinary window"
        );
        assert!(!waits(&osv(&["--window"]), env), "{why}");
    }
    for args in [
        osv(&["-e", "zsh"]),
        osv(&["--headless"]),
        osv(&["--control-sock", "/tmp/private"]),
    ] {
        assert!(!waits(&args, at_the_mac), "{args:?}");
    }
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

/// A progress record corrupt for good is set aside once the home resolves, never while it is
/// hidden, and before the records bind (System State design §3.5): it is kept beside itself,
/// the startup binds the fresh records once and goes on with no failure, offering the plan a
/// fresh record gives, with the Mac preference groups left as is and aterm's own permissions
/// set up.
#[test]
fn startup_sets_a_corrupt_record_aside_once_the_home_resolves_and_goes_on() {
    for reveal in [false, true] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let home = scratch.path().join("home");
        pending(&home);
        let record = home.join("doctor/initial-setup.json");
        std::fs::write(&record, b"{").unwrap();
        let mut host = startup(&home);
        host.fresh = true;
        if reveal {
            let hidden = scratch.path().join("not-mounted");
            std::fs::rename(&home, &hidden).unwrap();
            host.hidden = Some(hidden);
        }

        let prepared = prepare_setup(&mut host).unwrap();
        assert!(matches!(
            prepared,
            PreparedSetup::Authorized { revision: 1, .. }
        ));
        assert_eq!(host.binds, 1);
        let backoffs = usize::from(reveal);
        assert_eq!(host.backoffs, backoffs);
        assert_eq!(
            host.set_aside,
            [(
                vec!["initial-setup.json.unreadable".to_owned()],
                backoffs,
                0
            )]
        );
        assert_eq!(host.brought, [(backoffs, 0)]);
        assert_eq!(host.failures.len(), backoffs, "only the hidden home waited");
        assert_eq!(
            std::fs::read(home.join("doctor/initial-setup.json.unreadable")).unwrap(),
            b"{"
        );
        let recovered = host.snapshot().unwrap();
        assert!(record.exists());
        assert_ne!(recovered.revision, 0);
        assert!(recovered.recovered);
        assert!(!recovered.permissions_allowed);
        let [offered] = host.offered.as_slice() else {
            panic!("one offer: {:?}", host.offered);
        };
        assert!(offered.selection.contains(Setting::Permissions));
        assert!(!offered.selection.contains(Setting::Dock));
        assert!(!offered.selection.contains(Setting::Messages));
        assert_eq!(&recovered.plan, offered);
    }
}

/// A bring-forward that meets the profile lock busy is asked again after a quiet wait, before
/// the records bind and with no failure reported: a Restore's permissions get their run in this
/// launch, never the next.
#[test]
fn a_busy_bring_forward_is_asked_again_before_the_records_bind() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let (revision, _) = pending(&home);
    let mut host = startup(&home);
    host.busy = 2;

    let prepared = prepare_setup(&mut host).unwrap();
    assert!(matches!(prepared, PreparedSetup::Authorized { revision: got, .. } if got == revision));
    assert_eq!(host.brought, [(0, 0), (1, 0), (2, 0)]);
    assert_eq!(host.binds, 1);
    assert!(host.failures.is_empty(), "{:?}", host.failures);
}

#[test]
fn startup_recovery_retires_a_bound_review_when_its_records_become_corrupt() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    pending(&home);
    let mut old = startup(&home);
    old.after_bind = Some(AfterBind::Corrupt);

    assert!(matches!(
        prepare_setup(&mut old),
        Err(_) | Ok(PreparedSetup::Unavailable)
    ));
    assert_eq!(
        old.binds, 1,
        "the old review never adopts replacement records"
    );
    assert!(old.offered.is_empty());
    assert_eq!(
        old.set_aside.len(),
        1,
        "the old review repaired but never rebound"
    );
    assert!(old.snapshot().unwrap().recovered);

    // As the next startup does, repair before capturing a fresh review. It offers
    // Permissions with the Mac preference groups left alone, rather than replaying them.
    let mut fresh = startup(&home);
    fresh.fresh = true;
    assert!(matches!(
        prepare_setup(&mut fresh).unwrap(),
        PreparedSetup::Authorized { revision: 1, .. }
    ));
    assert_eq!(fresh.binds, 1);
    assert!(
        fresh.set_aside.is_empty(),
        "the old review already repaired the files"
    );
    assert_eq!(
        std::fs::read(home.join("doctor/initial-setup.json.unreadable")).unwrap(),
        b"{"
    );
    let [offered] = fresh.offered.as_slice() else {
        panic!("one fresh recovery offer: {:?}", fresh.offered);
    };
    assert!(offered.selection.contains(Setting::Permissions));
    for setting in Setting::ALL {
        if !matches!(
            setting,
            Setting::Permissions | Setting::Secrets | Setting::Repositories
        ) {
            assert!(!offered.selection.contains(setting), "{setting:?}");
        }
    }
}

#[test]
fn startup_recovery_keeps_its_bound_revision_through_a_transient_local_read_error() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let (revision, before) = pending(&home);
    let mut host = startup(&home);
    host.after_bind = Some(AfterBind::TemporarilyUnreadable);

    assert!(matches!(
        prepare_setup(&mut host).unwrap(),
        PreparedSetup::Authorized { revision: got, .. } if got == revision
    ));
    assert_eq!(host.binds, 1);
    assert_eq!(host.backoffs, 1);
    assert_eq!(host.failures.len(), 1);
    assert!(
        host.offered.is_empty(),
        "the admitted run resumes without another answer"
    );
    assert!(
        host.set_aside.is_empty(),
        "an I/O error is no corrupt record"
    );
    assert_eq!(host.snapshot().unwrap().observation, before);
}
