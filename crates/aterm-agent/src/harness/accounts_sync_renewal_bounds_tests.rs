// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! L5, THE RENEWAL DOOR'S TIME BOUNDS (accounts upkeep on a cause, commit 10b). On the keeper's
//! thread (`aterm_uds::trace::keeper`), the upkeep's renewal duty ([`memo::Duty::ClaudeRenew`])
//! compares every time it reads through the pass's reading (`aterm_uds::trace::Now`): its run that
//! found no login to renew is kept, it is skipped at each pass short of the second its door opens,
//! and it runs at that second and starts the renewal. Two bounds open it: a login's forecast end
//! coming within [`accounts::DUE_S`], and another Mac's lease on the account lapsing at its
//! `until` ([`accounts::claim_ours_or_none`]). Every address is synthetic.

use super::*;
use crate::harness::accounts_upkeep_counts::{UpkeepCounts, counts_here};
use std::sync::{Arc, Mutex};

/// The second each bound is placed at.
const T: i64 = 1_800_000_000;
const HOUR: i64 = 60 * 60;
const DAY: i64 = 24 * HOUR;
/// The one row's label.
const LABEL: &str = "kept";

type Work = Box<dyn FnOnce() + Send>;

/// A Mac with one Claude Code login, [`LABEL`], made here in a directory of its own and run on by a
/// session here a day before [`T`].
struct Mac {
    _dir: aterm_tempfile::TempDir,
    home: PathBuf,
    state: PathBuf,
    config: PathBuf,
}

impl Mac {
    /// The Mac, its login renewed there at `renewed`, with `record` made on its records beside
    /// the renewal and the session.
    fn new(renewed: i64, record: impl FnOnce(&mut accounts::State, &mut accounts::Logins)) -> Self {
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-renewal-bounds-")
            .tempdir()
            .expect("tempdir");
        let root = dir.path().to_path_buf();
        let (home, config, state) = (root.join("home"), root.join("config"), root.join("state"));
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&config).unwrap();
        aterm_uds::atomic_file::write(
            &config.join("aterm.toml"),
            b"[fleet]\nmachine = \"m27\"\n",
            0o600,
        )
        .unwrap();
        let store = accounts::Store::at(&state);
        store.machine_id("0123456789abcdef").unwrap();
        let made = store.logins_dir().join(LABEL);
        fs::create_dir_all(&made).unwrap();
        fs::write(made.join(".claude.json"), "{}").unwrap();
        let made = made.to_str().unwrap().to_owned();
        let roster = Roster {
            rows: vec![Row {
                label: LABEL.to_owned(),
                dir: Some(made.clone()),
                email: Some(format!("{LABEL}@example.test")),
                ..Row::default()
            }],
        };
        fs::write(
            config.join("accounts.toml"),
            format_roster_toml(&roster).unwrap(),
        )
        .unwrap();
        store
            .transact_logins(T - DAY, &mut None, |state, logins| {
                let mut events = accounts::note_renewed(
                    state,
                    logins,
                    LABEL,
                    Some(&made),
                    &made,
                    Some("max"),
                    renewed,
                );
                events.extend(accounts::note_session(state, "s-0", LABEL, None, T - DAY));
                state.sessions.clear();
                record(state, logins);
                Ok(events)
            })
            .unwrap();
        Self {
            _dir: dir,
            home,
            state,
            config,
        }
    }

    fn roster(&self) -> PathBuf {
        self.config.join("accounts.toml")
    }

    /// The renewal duty of one keeper's pass at each of `at`, in order, under the live upkeep's
    /// effects (`with_scope_effects`, which records each renewal started), every read judged
    /// settled as of a minute past the clock: for each pass, the renewals it started (queued, then
    /// dropped unrun, so no provider is contacted) and what it moved this thread's duty counts by,
    /// `[runs, skips, kept, poisoned]`.
    fn renewal_passes(&self, at: &[i64]) -> Vec<(usize, [u64; 4])> {
        let queued: Arc<Mutex<Vec<Work>>> = Arc::new(Mutex::new(Vec::new()));
        let into_queue = Arc::clone(&queued);
        let spawn = move |work| {
            into_queue.lock().unwrap().push(work);
            Ok(())
        };
        let fetch = |_: &Env, source: &str| -> Result<String, String> {
            Err(format!("no fetch in a test: {source}"))
        };
        let guard = AutoRenewGuard::default().shared();
        let cooldown = Watched::new("accounts_sync/sync_cooldown", None);
        let rediscovery = Watched::new("accounts_sync/rediscovery", Rediscovery::default());
        let fx = UpkeepEffects {
            scope: None,
            fetch: &fetch,
            spawn_renewal: &spawn,
            sync_cooldown: &cooldown,
            rediscovery: &rediscovery,
            renew_guard: &guard,
            find_browser: &|_: &Path| true,
            vault_sync: |_: &Path, _scope| super::super::sign_in_ladder::Vault::NotMember,
            spawn_vault: &|_work| Ok(()),
            default_check: |_: &Path, _scope| {
                crate::harness::cli::LoginCheck::CouldNotRun(String::new())
            },
            // A route the ladder can take, so only the door decides what starts.
            mailbox: &|_, _, _| (super::super::sign_in_ladder::Mailbox::Ready, None),
            log: Arc::new(|_: &str| {}),
            run_attempt: None,
            tokens_here: |_, _, _, _| None,
            shared: shared_renewal::Effects::default(),
        };
        let lifetime = crate::harness::account_work::Lifetime::default();
        let scope = lifetime
            .enter()
            .expect("a new account lifetime admits work");
        let duties = |c: UpkeepCounts| [c.duty_runs, c.duty_skips, c.duty_kept, c.duty_poisoned];
        let roster = self.roster();
        with_scope_effects(scope, fx, |fx| {
            trace::keeper(|| {
                trace::read_as_if_later(Duration::from_secs(60), || {
                    at.iter()
                        .map(|&now| {
                            let now = trace::Now::at(now);
                            let before = duties(counts_here());
                            memo::pass(
                                || (self.home.clone(), self.state.clone(), roster.clone()),
                                now,
                                guard_now(fx.renew_guard),
                                || {
                                    duty(fx, Duty::ClaudeRenew, (), || {
                                        auto_renew_next_account(
                                            &self.home,
                                            &self.state,
                                            &roster,
                                            now,
                                            fx,
                                        )
                                    })
                                },
                            );
                            let after = duties(counts_here());
                            // Dropping the queued work releases its admission unrun.
                            let started = queued.lock().unwrap().drain(..).count();
                            (started, std::array::from_fn(|i| after[i] - before[i]))
                        })
                        .collect()
                })
            })
        })
    }
}

/// What a pass that ran the duty and kept its run moves `[runs, skips, kept, poisoned]` by.
const RAN_KEPT: [u64; 4] = [1, 0, 1, 0];
/// What a pass that skipped the duty, its kept run standing, moves them by.
const SKIPPED: [u64; 4] = [0, 1, 0, 0];
/// What a pass that ran the duty and started a renewal moves them by: the start takes the
/// reading raw, so the run is poisoned and not kept.
const RAN: [u64; 4] = [1, 0, 0, 1];

/// A LOGIN COMING DUE IS RENEWED AT THE SECOND IT COMES DUE AND NOTHING RUNS A SECOND BEFORE. Its
/// forecast end is [`accounts::DUE_S`] past [`T`]: renewed a login's first lifetime
/// ([`accounts::LIFETIME_START_S`]) before that. At `T - 2` the duty runs, finds no login due and
/// is kept; at `T - 1` it is skipped; at `T` its kept run no longer stands, so it runs and starts
/// the renewal.
///
/// NEGATIVE CONTROL (2026-11-15): `recorded_login_state`'s due comparison made unrecorded
/// (`trace::unrecorded(|| now.raw()) >= by.saturating_sub(DUE_S)`) failed `the renewal not
/// started at its due second`, left `(0, [0, 1, 0, 0])`, right `(1, [1, 0, 0, 1])`: the kept run
/// stood at `T` and the duty was skipped. In the same mutant build the lease test below passed
/// (its login is due all along, and the lease decides); restored, the file touched.
#[test]
fn a_login_coming_due_is_renewed_at_its_due_second_and_nothing_runs_a_second_before() {
    let mac = Mac::new(T + accounts::DUE_S - accounts::LIFETIME_START_S, |_, _| {});
    let passes = mac.renewal_passes(&[T - 2, T - 1, T]);
    assert_eq!(passes[0], (0, RAN_KEPT), "two seconds before it comes due");
    assert_eq!(passes[1], (0, SKIPPED), "a second before it comes due");
    assert_eq!(
        passes[2],
        (1, RAN),
        "the renewal not started at its due second"
    );
}

/// A LOGIN ANOTHER MAC HOLDS THE LEASE OF IS RENEWED AT THE SECOND THE LEASE LAPSES AND NOTHING
/// RUNS A SECOND BEFORE. The login is due (renewed a day before [`T`], a notice then ends it at
/// `T`), and `m99`'s lease on the
/// account runs until `T`: the door stays shut while the lease stands
/// ([`accounts::claim_ours_or_none`]). At `T - 2` the duty runs and is kept; at `T - 1` it is
/// skipped; at `T` the lease has lapsed, so it runs and starts the renewal.
///
/// NEGATIVE CONTROL (2026-11-15): `claim_ours_or_none`'s lease made unrecorded
/// (`.filter(|lease| trace::unrecorded(|| now.raw()) < lease.until)`) failed `the renewal not
/// started as the lease lapsed`, left `(0, [0, 1, 0, 0])`, right `(1, [1, 0, 0, 1])`, while the
/// test above passed in that build; restored, the file touched.
#[test]
fn a_login_under_another_macs_lease_is_renewed_at_the_second_the_lease_lapses() {
    let mac = Mac::new(T - DAY, |state, logins| {
        assert!(accounts::note_login_notice(logins, LABEL, 1, T - DAY));
        state.leases.insert(
            LABEL.to_owned(),
            accounts::AccountLease {
                holder: "m99".to_owned(),
                until: T,
                at: T - DAY,
            },
        );
    });
    let passes = mac.renewal_passes(&[T - 2, T - 1, T]);
    assert_eq!(
        passes[0],
        (0, RAN_KEPT),
        "two seconds before the lease lapses"
    );
    assert_eq!(passes[1], (0, SKIPPED), "a second before the lease lapses");
    assert_eq!(
        passes[2],
        (1, RAN),
        "the renewal not started as the lease lapsed"
    );
}
