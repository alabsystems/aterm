// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A LOGIN MADE IN A TAB NOBODY SUPERVISES IS ADOPTED BY THE UPKEEP (plan package FL-174), end to
//! end through the upkeep with `claude auth login` scripted ([`UpkeepEffects::run_attempt`]):
//! the replay of 2026-11-15 on the default Claude home (a sign-out recorded with no tab
//! supervisor left, the person's `/login` in that tab 38 s later, then a served turn), the
//! request in flight across the sign-out that lifts nothing, the sign-in the ladder no longer
//! starts for a login that is serving (with a shared authority installed too), the one
//! unattended sign-in a lift ends, the row a step cannot lift, and the run a sleep interrupts.
//! Every fixture address is `example.test`.

// Read by the tests a scripted run of the ladder must reach, which run on Unix alone, where
// aterm links the opener its page is handed to (`cli::opener_link`).
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::ladder_tests::{Bench, attempt_of, hand_roster, runs_signed_out};
use super::login_tests::{Desk, NOW};
use super::*;
#[cfg(unix)]
use crate::harness::cli::RenewBrowser;
use crate::harness::cli::Renewal;
use crate::harness::login_adoption::tests::{
    tap_path, write_login_row, write_profile, write_response_across_a_tool_result,
    write_served_turn, write_tap,
};
use crate::harness::login_adoption::{Lift, Pass, Refusal, Step};
use crate::harness::tap;

/// When the fixture row's sign-out is recorded.
const T: i64 = NOW - 60;
/// The default login's row and its address.
const MAIN: &str = "main";
const OWN: &str = "main.m30@example.test";

/// The usage-tap file of the tab on the default login: its figures first reported at `at`, its
/// API time `api_ms`, with a seven-day figure.
fn main_tap(desk: &Desk, at: i64, api_ms: u64) {
    write_tap(
        &desk.state,
        None,
        Some(tap::Route::Stored),
        (at, api_ms),
        true,
    );
}

/// The desk of the replay: the default login's row `main` signed out at [`T`] with a session
/// on it and no worker anywhere, its mailbox there so the ladder has a route, and the default
/// home's profile naming `profile`.
fn desk_with_profile(profile: &str) -> Desk {
    let desk = Desk::new("m30");
    desk.default_login(profile);
    let row = accounts::Row {
        label: MAIN.to_owned(),
        class: Class::Preferred,
        email: Some(OWN.to_owned()),
        ..accounts::Row::default()
    };
    fs::write(desk.roster(), accounts::row_text(&row).unwrap() + "\n").unwrap();
    runs_signed_out(&desk, MAIN, T);
    fs::create_dir_all(
        desk.store()
            .dir
            .join(crate::harness::open_url::DEFAULT_MAILBOX_DIR),
    )
    .unwrap();
    desk
}

/// [`desk_with_profile`] as the upkeep's adoption step first saw its tab after the sign-out
/// (at `T + 10`: the baseline).
fn desk_seen() -> Desk {
    let desk = desk_with_profile(OWN);
    main_tap(&desk, T - 5, 4_000);
    let step = crate::harness::login_adoption::adopt_live_logins(
        &desk.home,
        &desk.state,
        &desk.roster(),
        T + 10,
        &Pass::default(),
        &|_| {},
    );
    assert_eq!(
        step,
        Step::default(),
        "the first look takes a baseline alone"
    );
    desk
}

fn upkeep_at(desk: &Desk, bench: &Bench, now: i64) -> Upkept {
    upkeep(
        &desk.home,
        &desk.state,
        (
            &desk.roster(),
            aterm_uds::trace::Now::at(now),
            UpkeepStep::Both,
        ),
        &bench.fx(),
    )
}

fn signed_out(desk: &Desk, label: &str, now: i64) -> bool {
    let store = accounts::Store::at_for(&desk.state, Cli::Claude, &desk.home);
    let records = store
        .read_account_records(Some(&desk.roster()), now)
        .unwrap();
    accounts::signed_out_here(records.roster.row(label).unwrap(), &records.state)
}

/// What the model wrote into a tab's transcript.
#[derive(Clone, Copy)]
enum Wrote {
    /// A request typed at the first time and answered at the second.
    Turn(i64, i64),
    /// A response to a request typed at the first time that runs a tool while it streams: its
    /// first row, its tool's result, and a later row of the same response, at these times.
    AcrossAToolResult(i64, (i64, i64, i64)),
}

impl Wrote {
    /// Appends it to the fixture conversation's transcript under the default home of `home`.
    fn write(self, home: &Path) {
        match self {
            Self::Turn(asked, answered) => write_served_turn(home, asked, answered),
            Self::AcrossAToolResult(asked, rows) => {
                write_response_across_a_tool_result(home, asked, rows);
            }
        }
    }

    /// When its last row was written: what the Unix-only in-flight run reports.
    #[cfg(unix)]
    fn ended(self) -> i64 {
        match self {
            Self::Turn(_, answered) => answered,
            Self::AcrossAToolResult(_, (_, _, later)) => later,
        }
    }
}

/// What the tab did between the upkeep's two passes.
#[derive(Clone, Copy)]
struct Tab {
    /// A `/login` that succeeded then.
    login_at: Option<i64>,
    /// What the model wrote then.
    turn: Option<Wrote>,
    /// When its figures were next reported, and its API time then.
    tap: (i64, u64),
}

/// One replay: the upkeep's pass 10 s after the sign-out (the tab's file as it stood), then
/// what the tab did, then the upkeep's next pass at `T + 120`.
fn replay(profile: &str, tab: Tab) -> (Desk, Bench) {
    let desk = desk_with_profile(profile);
    let bench = Bench::ending(Renewal::Failed);
    main_tap(&desk, T - 5, 4_000);
    let _ = upkeep_at(&desk, &bench, T + 10);
    assert!(signed_out(&desk, MAIN, T + 10), "nothing is newer yet");
    if let Some(at) = tab.login_at {
        write_login_row(&desk.home, at);
    }
    if let Some(wrote) = tab.turn {
        wrote.write(&desk.home);
    }
    main_tap(&desk, tab.tap.0, tab.tap.1);
    let _ = upkeep_at(&desk, &bench, T + 120);
    (desk, bench)
}

/// AN UNSUPERVISED TAB'S LOGIN LIFTS THE SIGN-OUT ON THE NEXT UPKEEP PASS. The replay of
/// 2026-11-15, on the default Claude home: the row's sign-out is recorded at `T` (a
/// `Please run /login` screen) and no tab supervisor is left (Autopilot was off, so the worker
/// stopped); the person types `/login` in that tab 38 s later, and a turn typed at `T + 95` is
/// served by `T + 97`. The upkeep's next pass, with no worker and no tab asked, records the row
/// signed in, writes the ledger row the journal notes, clears the failed sign-in the sign-out
/// led to, says so in the log, and opens no sign-in window for it. The `/login` alone, before
/// any turn is served, does the same.
///
/// NEGATIVE CONTROLS, each leaving the row signed out: a reading, a request and a `/login` row
/// dated at or before the sign-out; API time that did not grow and no `/login`; a profile that
/// names another address; a request authorized before the sign-out that ends after it, with
/// the API time it grew; and that request's response running a tool while it streams, so that
/// its tool's result and a later row of it are both written after the sign-out and the
/// baseline.
#[test]
fn login_adopted_an_unsupervised_tabs_login_lifts_the_sign_out_on_the_next_upkeep_pass() {
    let now = T + 120;
    for (what, tab) in [
        (
            "a /login and a served turn",
            Tab {
                login_at: Some(T + 38),
                turn: Some(Wrote::Turn(T + 95, T + 97)),
                tap: (T + 97, 9_000),
            },
        ),
        (
            "a /login alone",
            Tab {
                login_at: Some(T + 38),
                turn: None,
                tap: (T + 40, 4_000),
            },
        ),
    ] {
        let (desk, bench) = replay(OWN, tab);
        assert!(
            !signed_out(&desk, MAIN, now),
            "{what}: the login the person made in the tab is on record: {:?}",
            bench.logged()
        );
        let state = desk.store().load(now).unwrap().0;
        assert_eq!(state.account(MAIN).auth, accounts::Auth::Ok, "{what}");
        assert_eq!(
            state.account(MAIN).auth_at,
            Some(T + 38),
            "{what}: stamped at the evidence's own time, so every process that folds it agrees"
        );
        let ledger = fs::read_to_string(desk.store().ledger_path()).unwrap();
        assert!(
            ledger
                .lines()
                .any(|line| line.contains("signed-in") && line.contains(MAIN)),
            "{what}: the ledger row the journal notes: {ledger}"
        );
        assert_eq!(
            attempt_of(&desk, MAIN),
            None,
            "{what}: the failed sign-in the sign-out led to is cleared"
        );
        assert!(
            bench
                .logged()
                .iter()
                .any(|line| line.starts_with("account main: signed in again")),
            "{what}: {:?}",
            bench.logged()
        );
        // The pass after it starts no sign-in, with the ladder's own waits ended.
        bench.cool();
        let handed = bench.handed().len();
        let _ = upkeep_at(&desk, &bench, now + 60);
        assert_eq!(
            bench.handed().len(),
            handed,
            "{what}: no sign-in window for a live login"
        );
    }

    for (what, profile, tab) in [
        (
            "a reading, a request and a login row dated at or before the sign-out",
            OWN,
            Tab {
                login_at: Some(T - 5),
                turn: Some(Wrote::Turn(T - 3, T)),
                tap: (T, 9_000),
            },
        ),
        (
            "API time that did not grow",
            OWN,
            Tab {
                login_at: None,
                turn: Some(Wrote::Turn(T + 95, T + 97)),
                tap: (T + 97, 4_000),
            },
        ),
        (
            "a profile naming another address",
            "another@example.test",
            Tab {
                login_at: Some(T + 38),
                turn: Some(Wrote::Turn(T + 95, T + 97)),
                tap: (T + 97, 9_000),
            },
        ),
        (
            "a request in flight across the sign-out",
            OWN,
            Tab {
                login_at: None,
                turn: Some(Wrote::Turn(T - 20, T + 97)),
                tap: (T + 97, 9_000),
            },
        ),
        (
            "a later row of the response in flight, after its tool's result",
            OWN,
            Tab {
                login_at: None,
                turn: Some(Wrote::AcrossAToolResult(T - 20, (T + 15, T + 20, T + 22))),
                tap: (T + 22, 9_000),
            },
        ),
    ] {
        let (desk, bench) = replay(profile, tab);
        assert!(signed_out(&desk, MAIN, now), "{what}: {:?}", bench.logged());
        assert!(
            !bench
                .logged()
                .iter()
                .any(|line| line.contains("signed in again")),
            "{what}"
        );
    }
}

/// THE UPKEEP STARTS NO SIGN-IN FOR AN ACCOUNT WITH NEWER SERVED EVIDENCE, at the ladder's own
/// read ([`read_renewal_candidates`]), before anything has lifted the record: the row is no
/// candidate, and a pass of the ladder alone starts nothing and lifts nothing. The same holds
/// with a shared authority installed, as the app installs one: the ladder then ranks again
/// from the same records, and that ranking is filtered as the read's own was. CONTROL: with
/// the same files and no served request, the row is the candidate and the pass starts its
/// sign-in.
#[test]
fn login_adopted_the_upkeep_starts_no_sign_in_for_an_account_with_newer_served_evidence() {
    let authority: Box<shared_renewal::Factory> =
        Box::new(|_, _, _| Err("no shared custody on this desk".to_owned()));
    for shared in [false, true] {
        for served in [true, false] {
            let what = format!("shared={shared} served={served}");
            let desk = desk_seen();
            if served {
                write_served_turn(&desk.home, T + 95, T + 97);
                main_tap(&desk, T + 97, 9_000);
            }
            let now = T + 120;
            let store = accounts::Store::at_for(&desk.state, Cli::Claude, &desk.home);
            let read = read_renewal_candidates(
                Cli::Claude,
                &store,
                (&desk.home, &desk.state),
                &desk.roster(),
                now,
                &Pass::default(),
            )
            .unwrap();
            let ranked: Vec<&str> = read
                .tiered
                .iter()
                .map(|(_, row)| row.label.as_str())
                .collect();
            assert_eq!(ranked, if served { vec![] } else { vec![MAIN] }, "{what}");
            let bench = Bench::ending(Renewal::Failed);
            let fx = UpkeepEffects {
                shared: shared_renewal::Effects {
                    authority: shared.then_some(&*authority),
                    ..shared_renewal::Effects::default()
                },
                ..bench.fx()
            };
            assert_eq!(
                auto_renew_next_account(&desk.home, &desk.state, &desk.roster(), now, &fx),
                !served,
                "{what}"
            );
            // The control's sign-in is handed its page, and its attempt is on record, on Unix
            // alone, where aterm links the opener an unattended run's page is handed to
            // (`cli::opener_link`); off Unix it is refused before its provider runs, and
            // `an_unattended_run_is_handed_nothing_where_aterm_links_no_opener` holds that.
            if served || cfg!(unix) {
                assert_eq!(bench.handed().len(), usize::from(!served), "{what}");
                assert_eq!(attempt_of(&desk, MAIN).is_some(), !served, "{what}");
            }
            assert!(
                signed_out(&desk, MAIN, now),
                "{what}: the ladder's read hid the row and lifted nothing; the upkeep's step \
                 lifts the record"
            );
        }
    }
}

/// A LIFT ENDS THAT ACCOUNT'S UNATTENDED SIGN-IN IN FLIGHT AND NOTHING ELSE. The ladder's own
/// sign-in of the row is running (its scripted `claude auth login`) when the tab is served;
/// the upkeep's adoption step lifts the row, cancels that run's own operation, and logs
/// `sign-in superseded`. The upkeep step the run was started under is NOT cancelled: its other
/// work goes on. The run is then counted neither failed nor blocked, and keeps its spacing,
/// since its sign-in page may have loaded.
///
/// NEGATIVE CONTROLS: a request in flight across the sign-out, which ends after it with the
/// API time it grew, lifts nothing and cancels nothing, whether its answer is one row or its
/// response ran a tool while it streamed (the tool's result and a later row of that response
/// are then both written after the sign-out and the baseline), and neither does no evidence:
/// the run is counted failed and its work is not cancelled.
///
/// Unix only, where aterm links the opener an unattended run's page is handed to
/// (`cli::opener_link`): off Unix the run never starts, and
/// `an_unattended_run_is_handed_nothing_where_aterm_links_no_opener` holds what runs there. A
/// run superseded before it began and a person's sign-in are held on every platform
/// ([`login_adopted_a_lift_reaches_no_run_before_it_began_and_no_desk_sign_in`]).
#[cfg(unix)]
#[test]
fn login_adopted_a_lift_supersedes_an_unattended_sign_in_in_flight() {
    for (what, turn, lifts) in [
        (
            "a request served after the sign-out",
            Some(Wrote::Turn(T + 95, T + 97)),
            true,
        ),
        (
            "a request in flight across it",
            Some(Wrote::Turn(T - 20, T + 97)),
            false,
        ),
        (
            "a later row of the response in flight, after its tool's result",
            Some(Wrote::AcrossAToolResult(T - 20, (T + 15, T + 20, T + 22))),
            false,
        ),
        ("no evidence", None, false),
    ] {
        let desk = desk_seen();
        let bench = Bench::ending(Renewal::Failed);
        let lifetime = crate::harness::account_work::Lifetime::default();
        let scope = lifetime.enter().unwrap();
        // The scripted run: while it runs, the tab does what it does and the upkeep's next
        // step folds that in, as a later tick does while a real `claude auth login` waits on
        // its page.
        let (home, state, roster) = (desk.home.clone(), desk.state.clone(), desk.roster());
        let guard = Arc::clone(&bench.guard);
        let stepped: Arc<Mutex<Vec<String>>> = Arc::default();
        let said = Arc::clone(&stepped);
        let runs = Arc::new(AtomicUsize::new(0));
        let ran = Arc::clone(&runs);
        let run_cancelled = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::clone(&run_cancelled);
        let run: RunAttempt = Arc::new(move |_: &RenewBrowser, _: bool| {
            ran.fetch_add(1, Ordering::SeqCst);
            if let Some(wrote) = turn {
                wrote.write(&home);
                write_tap(
                    &state,
                    None,
                    Some(tap::Route::Stored),
                    (wrote.ended(), 9_000),
                    true,
                );
            }
            adopt_live_logins_in_upkeep(
                (&home, &state, &roster),
                T + 120,
                &guard,
                &Pass::default(),
                &|line| {
                    said.lock().unwrap().push(line.to_owned());
                },
            );
            let own = guard.lock().unwrap().unattended_runs.get(MAIN).cloned();
            cancelled.store(
                own.and_then(|run| run.cancel)
                    .expect("the run is marked, with its own operation")
                    .cancelled(),
                Ordering::SeqCst,
            );
            Renewal::Failed
        });
        let fx = UpkeepEffects {
            scope: Some(scope.clone()),
            run_attempt: Some(run),
            ..bench.fx()
        };
        // The pass runs its sign-in inline here, so a superseded one ends with its own work
        // cancelled and reports nothing started; in a window the cancelled work is an earlier
        // pass's.
        assert_eq!(
            auto_renew_next_account(&desk.home, &desk.state, &desk.roster(), T + 20, &fx),
            !lifts,
            "{what}"
        );
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "{what}: the ladder's own sign-in ran"
        );
        let stepped = stepped.lock().unwrap().clone();
        let store = desk.store();
        assert_eq!(
            stepped
                .iter()
                .any(|line| line.contains("sign-in superseded")),
            lifts,
            "{what}: {stepped:?}"
        );
        assert_eq!(
            run_cancelled.load(Ordering::SeqCst),
            lifts,
            "{what}: the run's own work is ended"
        );
        assert!(
            !scope.cancelled(),
            "{what}: the upkeep step that started the run goes on"
        );
        assert_eq!(!signed_out(&desk, MAIN, T + 120), lifts, "{what}");
        assert_eq!(
            attempt_of(&desk, MAIN).is_none(),
            lifts,
            "{what}: a superseded run is counted neither failed nor blocked"
        );
        assert!(
            crate::harness::sign_in_ladder::last_started(&store)
                .unwrap()
                .is_some(),
            "{what}: a run that reached its provider keeps its spacing"
        );
        assert!(
            bench.guard.lock().unwrap().unattended_runs.is_empty(),
            "{what}: the mark ends with the run's work"
        );
    }
}

/// A LIFT REACHES NO RUN BEFORE IT BEGAN AND NO SIGN-IN A PERSON STARTED: the ladder's run of a
/// row superseded before its provider's step started loaded nothing, so its spacing goes back
/// and no cooldown is left on its row; and a sign-in a person started is never marked, so a
/// lift of its account cancels nothing. CONTROL: the ladder's own run under the same scope is
/// superseded, and it alone, and what stops the step still stops the run.
#[test]
fn login_adopted_a_lift_reaches_no_run_before_it_began_and_no_desk_sign_in() {
    // Superseded before its provider's step started: nothing loaded, so its spacing goes back
    // and no cooldown is left on its row.
    let desk = desk_seen();
    let bench = Bench::ending(Renewal::Failed);
    let lifetime = crate::harness::account_work::Lifetime::default();
    let scope = lifetime.enter().unwrap();
    let guard = Arc::clone(&bench.guard);
    let spawn = move |work: Box<dyn FnOnce() + Send>| -> io::Result<()> {
        supersede_unattended(&guard, &[MAIN.to_owned()], &|_| {});
        work();
        Ok(())
    };
    let fx = UpkeepEffects {
        scope: Some(scope.clone()),
        spawn_renewal: &spawn,
        ..bench.fx()
    };
    assert!(!auto_renew_next_account(
        &desk.home,
        &desk.state,
        &desk.roster(),
        T + 20,
        &fx
    ));
    assert_eq!(bench.handed().len(), 0, "its provider's step never started");
    assert_eq!(
        crate::harness::sign_in_ladder::last_started(&desk.store()).unwrap(),
        None,
        "and its spacing is given back"
    );
    assert_eq!(attempt_of(&desk, MAIN), None);
    assert!(!scope.cancelled());
    assert!(
        !bench.guard.lock().unwrap().last_attempt.contains_key(MAIN),
        "no cooldown is left on its row"
    );

    // A person's sign-in carries no mark, so nothing a lift does reaches it.
    let guard = AutoRenewGuard::default().shared();
    let lifetime = crate::harness::account_work::Lifetime::default();
    let scope = lifetime.enter().unwrap();
    assert!(RunMark::of(&guard, MAIN, false, Some(&scope)).is_none());
    let said = Mutex::new(Vec::new());
    supersede_unattended(&guard, &[MAIN.to_owned()], &|line| {
        said.lock().unwrap().push(line.to_owned());
    });
    assert!(!scope.cancelled(), "a desk sign-in is never cancelled");
    assert_eq!(*said.lock().unwrap(), Vec::<String>::new());
    // CONTROL: the ladder's own, under the same scope, is, and it alone.
    let mark = RunMark::of(&guard, MAIN, true, Some(&scope)).unwrap();
    let own = mark.scope.clone().expect("its own operation");
    supersede_unattended(&guard, &[MAIN.to_owned()], &|line| {
        said.lock().unwrap().push(line.to_owned());
    });
    assert!(own.cancelled() && mark.superseded());
    assert!(!scope.cancelled(), "the step it was started under goes on");
    assert_eq!(said.lock().unwrap().len(), 1);
    // What stops the step still stops the run.
    let other = RunMark::of(&guard, "other", true, Some(&scope)).unwrap();
    let run = other.scope.clone().expect("its own operation");
    assert!(!run.cancelled());
    scope.cancellation().cancel();
    assert!(run.cancelled());
}

/// A SIGN-IN IS HELD ONLY WHERE THE LIFT LANDED OR THE ACCOUNTS LOCK HELD IT UP, and every
/// other reason a row the files show live stays signed out is logged once per change, by label
/// alone, while the ladder goes on with the row.
///
/// The last ask under the sign-in lock ([`login_live_again`]): evidence the step lifts holds
/// the sign-in (CONTROL), and so does a lift the held accounts lock put off to the next pass;
/// a row an earlier step was refused is not held, though the files show it live, and no
/// evidence holds nothing. [`settle_adoption`]: a refusal is logged when it is new or its
/// reason changed, never again while it stands, and never with an address; a held lock says
/// nothing and keeps what was said; a lift ends the memory.
#[test]
fn login_adopted_a_sign_in_is_held_only_where_the_lift_landed_and_a_refusal_is_said_once() {
    let now = T + 120;
    let ask = |desk: &Desk, pass: &Pass| {
        let guard = AutoRenewGuard::default().shared();
        login_live_again(
            (&desk.home, &desk.state, &desk.roster()),
            MAIN,
            now,
            &guard,
            pass,
            &|_| {},
        )
    };
    let live = || {
        let desk = desk_seen();
        write_login_row(&desk.home, T + 38);
        desk
    };
    // Landed.
    let desk = live();
    assert!(ask(&desk, &Pass::default()));
    assert!(
        !signed_out(&desk, MAIN, now),
        "the ask's own step lifted it"
    );
    // The accounts lock is held throughout: the lift waits for the next pass, and so does
    // the sign-in.
    let desk = live();
    let held = desk.store().acquire().unwrap();
    assert!(accounts::with_store_ladder(&[], || ask(
        &desk,
        &Pass::default()
    )));
    drop(held);
    assert!(signed_out(&desk, MAIN, now), "nothing was written");
    // Refused by an earlier step: the row is the ladder's.
    let desk = live();
    assert!(!ask(&desk, &Pass::knowing([MAIN.to_owned()])));
    assert!(signed_out(&desk, MAIN, now));
    // No evidence.
    assert!(!ask(&desk_seen(), &Pass::default()));

    let guard = AutoRenewGuard::default().shared();
    let said = Mutex::new(Vec::new());
    let settle = |lift: Lift| {
        let pass = Pass::default();
        let step = Step {
            lifts: vec![(MAIN.to_owned(), lift)],
            ..Step::default()
        };
        settle_adoption(&guard, &step, &pass, &|line| {
            said.lock().unwrap().push(line.to_owned());
        });
        (pass.refused(MAIN), said.lock().unwrap().len())
    };
    let later = Lift::Refused(Refusal::Record(accounts::NotAdopted::LaterWord));
    assert_eq!(settle(later), (true, 1), "a new refusal is said");
    assert_eq!(settle(later), (true, 1), "and not again while it stands");
    assert_eq!(settle(Lift::Busy), (false, 1), "a held lock says nothing");
    assert!(
        live_logins_known(&guard).refused(MAIN),
        "and what was said of the row stands for a pass with no step of its own"
    );
    assert_eq!(
        settle(Lift::Refused(Refusal::Store)),
        (true, 2),
        "a changed reason is said"
    );
    settle_adoption(
        &guard,
        &Step {
            unread: true,
            ..Step::default()
        },
        &Pass::default(),
        &|_| panic!("a step that read nothing says nothing"),
    );
    assert!(live_logins_known(&guard).refused(MAIN));
    assert_eq!(
        settle(Lift::Landed(accounts::LoginEvidence::SignedIn(T + 38))),
        (false, 2)
    );
    assert!(!live_logins_known(&guard).refused(MAIN), "a lift ends it");
    assert_eq!(
        settle(later),
        (true, 3),
        "one that comes back is said again"
    );
    // Baselines that could not be kept: once, until they are.
    let unkept = |unkept: bool| {
        settle_adoption(
            &guard,
            &Step {
                baselines_unkept: unkept,
                ..Step::default()
            },
            &Pass::default(),
            &|line| said.lock().unwrap().push(line.to_owned()),
        );
        said.lock()
            .unwrap()
            .iter()
            .filter(|line| line.contains("baselines could not be kept"))
            .count()
    };
    assert_eq!(
        (unkept(true), unkept(true), unkept(false), unkept(true)),
        (1, 1, 1, 2)
    );
    let said = said.lock().unwrap();
    assert!(said.iter().all(|line| !line.contains('@')), "{said:?}");
    assert!(
        said[0].starts_with("account main: ") && said[0].contains("its record holds a later word"),
        "{said:?}"
    );
}

/// A ROW ON A LOGIN DIRECTORY OF ITS OWN IS LEFT TO THE LADDER, WITH THE REASON LOGGED ONCE,
/// through the upkeep: `w3`'s tab on its login directory grows its API time after the
/// sign-out, and no file without a credential says which account that place holds. The upkeep
/// lifts nothing, logs why once however many passes see it, and its ladder still ranks the row
/// and signs it in.
#[test]
fn login_adopted_a_row_on_its_own_login_directory_is_left_to_the_ladder_and_said_once() {
    let desk = Desk::new("m30");
    hand_roster(&desk, false);
    fs::create_dir_all(
        desk.store()
            .dir
            .join(crate::harness::open_url::DEFAULT_MAILBOX_DIR),
    )
    .unwrap();
    let w3 = desk.store().logins_dir().join("w3");
    write_profile(&w3, "w3.m30.3@example.test");
    let w3_tap = |at: i64, api_ms: u64| {
        write_tap(
            &desk.state,
            Some(&w3.to_string_lossy()),
            Some(tap::Route::Stored),
            (at, api_ms),
            true,
        );
    };
    let bench = Bench::ending(Renewal::Failed);
    w3_tap(T - 5, 4_000);
    let _ = upkeep_at(&desk, &bench, T + 10);
    // Its page is handed on Unix alone, where aterm links the opener an unattended run's page
    // is handed to (`cli::opener_link`); off Unix the ladder's run is refused before it starts
    // (`an_unattended_run_is_handed_nothing_where_aterm_links_no_opener`), and the row stays
    // its candidate all the same.
    #[cfg(unix)]
    assert_eq!(bench.handed().len(), 1, "the ladder signs the row in");
    write_login_row(&desk.home, T + 38);
    write_served_turn(&desk.home, T + 95, T + 97);
    w3_tap(T + 97, 9_000);
    let declined = |bench: &Bench| {
        bench
            .logged()
            .iter()
            .filter(|line| {
                line.starts_with("account w3: a tab on its login looks live again")
                    && line.contains("login directory of its own")
            })
            .count()
    };
    for now in [T + 120, T + 180, T + 240] {
        let _ = upkeep_at(&desk, &bench, now);
        assert!(signed_out(&desk, "w3", now), "{:?}", bench.logged());
        assert_eq!(declined(&bench), 1, "{now}: {:?}", bench.logged());
    }
    assert!(tap_path(&desk.state).is_file());
    let store = accounts::Store::at_for(&desk.state, Cli::Claude, &desk.home);
    let read = read_renewal_candidates(
        Cli::Claude,
        &store,
        (&desk.home, &desk.state),
        &desk.roster(),
        T + 240,
        &live_logins_known(&bench.guard),
    )
    .unwrap();
    assert!(
        read.tiered.iter().any(|(_, row)| row.label == "w3"),
        "the row stays the ladder's candidate"
    );
}

/// AN ATTEMPT SPANNING A SLEEP IS INTERRUPTED, NOT TIMED OUT, TWICE IN A ROW AT MOST: the
/// ladder's run of the row ends with no login after the Mac slept across it. It is logged
/// interrupted, counted neither failed nor blocked, and the very next pass tries again; it
/// keeps its spacing, since its sign-in page may have loaded before the lid shut. A row whose
/// every run a sleep spans is waived twice: the third is counted as it ended, and the ladder's
/// backoff holds the pass after. CONTROL: the same run with no sleep is counted failed at
/// once, and the next pass starts nothing.
#[cfg(unix)]
#[test]
fn login_adopted_an_attempt_spanning_a_sleep_is_interrupted_not_timed_out() {
    for sleeps in [true, false] {
        let desk = desk_with_profile(OWN);
        let bench = Bench::ending(Renewal::Failed);
        let seam = crate::harness::wake_deadline::SleepSeam::hold();
        let sleeper = seam.sleeper();
        let runs = Arc::new(AtomicUsize::new(0));
        let ran = Arc::clone(&runs);
        let run: RunAttempt = Arc::new(move |_: &RenewBrowser, _: bool| {
            ran.fetch_add(1, Ordering::SeqCst);
            // The lid shuts over every run.
            if sleeps {
                sleeper.slept(Duration::from_secs(700));
            }
            Renewal::Failed
        });
        let fx = UpkeepEffects {
            run_attempt: Some(run),
            ..bench.fx()
        };
        let pass =
            |now: i64| auto_renew_next_account(&desk.home, &desk.state, &desk.roster(), now, &fx);
        let interrupted = || {
            bench
                .logged()
                .iter()
                .filter(|line| line.contains("interrupted: this Mac slept while it ran"))
                .count()
        };
        assert!(pass(T + 20), "the ladder's sign-in of the row started");
        let store = desk.store();
        assert_eq!(interrupted(), usize::from(sleeps), "{:?}", bench.logged());
        assert_eq!(
            attempt_of(&desk, MAIN).is_none(),
            sleeps,
            "an interrupted run is counted neither failed nor blocked"
        );
        assert!(
            crate::harness::sign_in_ladder::last_started(&store)
                .unwrap()
                .is_some(),
            "a run that reached its provider keeps its spacing"
        );
        assert_eq!(pass(T + 21), sleeps, "it is tried again at once");
        if !sleeps {
            assert_eq!(runs.load(Ordering::SeqCst), 1);
            continue;
        }
        assert_eq!(interrupted(), 2, "the second in a row is waived too");
        assert_eq!(attempt_of(&desk, MAIN), None);
        assert!(pass(T + 22), "and tried again");
        assert_eq!(
            interrupted(),
            2,
            "the third is counted as it ended: {:?}",
            bench.logged()
        );
        assert!(
            attempt_of(&desk, MAIN).is_some(),
            "a failure on record, with the ladder's backoff"
        );
        assert!(!pass(T + 23), "which holds the next pass");
        assert_eq!(runs.load(Ordering::SeqCst), 3);
    }
    use crate::harness::sign_in_ladder::{INTERRUPTING_SLEEP, interrupted_by_sleep};
    let long = Some(INTERRUPTING_SLEEP);
    assert!(interrupted_by_sleep(long, false, false));
    assert!(
        !interrupted_by_sleep(long, true, false),
        "a login made stands"
    );
    assert!(
        !interrupted_by_sleep(long, false, true),
        "what a person must answer stays what it is"
    );
    assert!(!interrupted_by_sleep(
        Some(INTERRUPTING_SLEEP - Duration::from_secs(1)),
        false,
        false
    ));
    assert!(!interrupted_by_sleep(None, false, false));
}
