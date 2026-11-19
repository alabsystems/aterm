// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The watchdog's ending of a harness past its claim's re-exec grace (HX-15e): a real session's
//! claim, its connection's close and the lapse that ends its grace, then the ending against a
//! kernel stand-in that holds one kept child, so no process of the machine is ever signalled.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use aterm_agent::harness::contract::{self, Scope};
use aterm_agent::harness::relaunch_type::RunLineRefusal;
use aterm_uds::process::{Birth, EndControl, Ended, Identity};

use super::*;
use crate::harness_claim::KeptChild;

/// The kernel a kept child's end reads: `pid` reads at `birth` until a signal ends it.
pub(crate) struct Kernel {
    pid: i32,
    birth: Birth,
    ended: bool,
    /// The kernel refuses every signal (`EPERM`), so the child keeps reading at its birth.
    pub(crate) refuses: bool,
    /// Every signal sent, with the identity it was sent by.
    pub(crate) sent: Vec<(Identity, i32)>,
}

impl Kernel {
    /// A kernel where `pid` reads at `birth`.
    pub(crate) fn reading(pid: u32, birth: u64) -> Self {
        Self {
            pid: i32::try_from(pid).expect("a pid"),
            birth: Birth(birth),
            ended: false,
            refuses: false,
            sent: Vec::new(),
        }
    }
}

impl EndControl for Kernel {
    fn identity(&mut self, pid: i32) -> Option<Identity> {
        (pid == self.pid && !self.ended).then_some(Identity {
            pid,
            birth: self.birth,
            idversion: None,
        })
    }

    fn signal(&mut self, token: &Identity, sig: i32) -> std::io::Result<()> {
        self.sent.push((*token, sig));
        if self.refuses {
            return Err(std::io::Error::from_raw_os_error(libc::EPERM));
        }
        self.ended = true;
        Ok(())
    }

    fn nap(&mut self, _pause: Duration) {}
}

/// A world whose claimant is gone and whose clock the watchdog never reads here: what each act
/// waited out is kept.
#[derive(Default)]
pub(crate) struct Gone {
    /// `quiet=`, in milliseconds.
    pub(crate) quiet_ms: AtomicU64,
    pub(crate) waited: Mutex<Vec<(Act, Hand)>>,
    /// Each relaunch's back-off and words, in order.
    pub(crate) paused: Mutex<Vec<Duration>>,
    pub(crate) typed: Mutex<Vec<Vec<String>>>,
}

impl World for Gone {
    fn awake_ms(&self) -> Option<u64> {
        None
    }

    fn woke_ms(&self) -> Option<u64> {
        None
    }

    fn stopped(&self, _pid: u32) -> Option<bool> {
        None
    }

    fn alive(&self, _proven: &Proven) -> bool {
        false
    }

    fn quiet(&self) -> Duration {
        Duration::from_millis(self.quiet_ms.load(Ordering::Acquire))
    }

    fn look_again(&self) -> Duration {
        Duration::from_millis(5)
    }

    fn stall_after(&self) -> Option<Duration> {
        None
    }

    fn stall_term(&self) -> bool {
        true
    }

    fn waits(&self, _sid: &str, act: Act, hand: Hand) {
        self.waited
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((act, hand));
    }

    fn snapshot(&self, _sid: &str, _pid: u32) -> Option<crate::session_timeline::Resume> {
        None
    }

    fn pause(&self, _sid: &str, pause: Duration, stands: &dyn Fn() -> bool) -> bool {
        self.paused
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(pause);
        stands()
    }

    fn type_run(
        &self,
        _sid: &str,
        words: &[String],
        _cwd: Option<&str>,
    ) -> Result<(), RunLineRefusal> {
        self.typed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(words.to_vec());
        Ok(())
    }
}

/// [`Gone`]'s world with a claimant that still runs.
#[derive(Default)]
struct Lives(Gone);

impl World for Lives {
    fn awake_ms(&self) -> Option<u64> {
        None
    }

    fn woke_ms(&self) -> Option<u64> {
        None
    }

    fn stopped(&self, _pid: u32) -> Option<bool> {
        None
    }

    fn alive(&self, _proven: &Proven) -> bool {
        true
    }

    fn quiet(&self) -> Duration {
        self.0.quiet()
    }

    fn look_again(&self) -> Duration {
        self.0.look_again()
    }

    fn stall_after(&self) -> Option<Duration> {
        self.0.stall_after()
    }

    fn stall_term(&self) -> bool {
        self.0.stall_term()
    }

    fn waits(&self, sid: &str, act: Act, hand: Hand) {
        self.0.waits(sid, act, hand);
    }

    fn snapshot(&self, sid: &str, pid: u32) -> Option<crate::session_timeline::Resume> {
        self.0.snapshot(sid, pid)
    }

    fn pause(&self, sid: &str, pause: Duration, stands: &dyn Fn() -> bool) -> bool {
        self.0.pause(sid, pause, stands)
    }

    fn type_run(
        &self,
        sid: &str,
        words: &[String],
        cwd: Option<&str>,
    ) -> Result<(), RunLineRefusal> {
        self.0.type_run(sid, words, cwd)
    }
}

/// The child every claim here keeps: a pid and the birth aterm read of it.
const CHILD: u32 = 4243;
const BIRTH: u64 = 1_792_000_000_000_000;
/// The connection the claims are placed on.
const CONN: u64 = 77;

/// A contract `contract_no` claim of `state` and `relaunch`, keeping [`CHILD`] under contract 4.
fn claim(contract_no: u32) -> Scoped {
    let mut scoped =
        crate::session_timeline::scoped_input_tests::scoped(&[Scope::State, Scope::Relaunch]);
    scoped.claim.contract = contract_no;
    scoped.child = (contract_no == contract::UNIVERSAL).then_some(KeptChild {
        pid: CHILD,
        claimed_us: BIRTH,
        birth: Birth(BIRTH),
    });
    scoped
}

/// Place `scoped` on `ctx`'s tab on [`CONN`], close that connection and lapse what its close
/// left at the grace's end: what the lapse removed.
fn past_the_grace(ctx: &SessionCtx, scoped: &Scoped) -> crate::session_timeline::Lapsed {
    let holder = "grok-alab@4242";
    crate::session_timeline::claim_supervisor_scoped(
        ctx,
        holder,
        Some(CONN),
        scoped.clone(),
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    )
    .expect("the claim");
    let grace_us = alab_wire::record::REEXEC_GRACE_MS * 1000;
    let closed = crate::metrics::now_us();
    assert!(crate::session_timeline::release_closed_connection(
        ctx,
        CONN,
        closed,
        Some(grace_us)
    ));
    let expiry = ctx
        .meta
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .supervisor_expiry();
    // A contract 4 claim outlives its connection by the grace; any other ends with it.
    let at = expiry.unwrap_or(closed);
    crate::session_timeline::publish_lapse_supervisor(ctx, at)
        .published()
        .unwrap_or_else(|| crate::session_timeline::Lapsed {
            holder: holder.to_string(),
            universal: None,
        })
}

fn need_word(app: &mut crate::App, ctx: &SessionCtx) -> &'static str {
    app.publish_need(0);
    ctx.timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .need()
        .needs_word()
}

/// HX-15e EXIT TEST: A DEAD HARNESS HAS ITS KEPT CHILD ENDED BY IDENTITY AND READS UNSUPERVISED
/// (D28). A contract 4 claim keeping a child loses its connection; the re-exec grace runs out with
/// nothing claiming again, and the lapse hands the claim to the watchdog. Its claimant is gone, so
/// the watchdog ends the kept child by the identity aterm keeps: `SIGTERM` to the pid only while
/// it still reads at the kept birth, sent by that very identity. The tab's need reads
/// `unsupervised` until a harness claims it again.
/// NEGATIVE HALF: where the kept pid reads at another birth (the child ended and its pid names
/// another process), nothing is signalled; a new claim ends `unsupervised`.
#[test]
fn a_dead_harness_has_its_kept_child_ended_by_identity_and_reads_unsupervised() {
    let mut app = crate::App::headless_for_test();
    let ctx = Arc::clone(&app.pool.get(0).expect("session 0").ctx);
    let weak = Arc::downgrade(&ctx);
    assert_ne!(need_word(&mut app, &ctx), "unsupervised");
    let lapsed = past_the_grace(&ctx, &claim(contract::UNIVERSAL));
    let scoped = lapsed
        .universal
        .expect("a contract 4 claim is handed to the watchdog");
    let world = Gone::default();

    // NEGATIVE: the kept pid now names another process.
    let mut reused = Kernel::reading(CHILD, BIRTH + 1);
    assert_eq!(
        end_harness(
            &weak,
            &lapsed.holder,
            &scoped,
            Why::Grace,
            &world,
            &mut reused
        ),
        Ending::Ended {
            child: Some(Ok(Ended {
                gone: true,
                ..Ended::default()
            }))
        }
    );
    assert!(
        reused.sent.is_empty(),
        "a pid at another birth is never signalled: {:?}",
        reused.sent
    );

    // The kept child still reads at its birth: it is sent SIGTERM by that identity and ends.
    let mut kernel = Kernel::reading(CHILD, BIRTH);
    assert_eq!(
        end_harness(
            &weak,
            &lapsed.holder,
            &scoped,
            Why::Grace,
            &world,
            &mut kernel
        ),
        Ending::Ended {
            child: Some(Ok(Ended {
                termed: true,
                killed: false,
                gone: true,
            }))
        }
    );
    let pid = i32::try_from(CHILD).expect("a pid");
    assert_eq!(
        kernel.sent,
        vec![(
            Identity {
                pid,
                birth: Birth(BIRTH),
                idversion: None,
            },
            libc::SIGTERM
        )]
    );
    assert!(
        world
            .waited
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty(),
        "no hand held the tab"
    );
    assert_eq!(
        ctx.meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .unsupervised
            .as_ref()
            .map(|ended| ended.holder.as_str()),
        Some("grok-alab@4242")
    );
    assert_eq!(need_word(&mut app, &ctx), "unsupervised");

    // A harness that claims the tab again supervises it.
    crate::session_timeline::claim_supervisor_scoped(
        &ctx,
        "grok-alab@4242",
        Some(CONN + 1),
        claim(contract::UNIVERSAL),
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    )
    .expect("the new claim");
    assert!(
        ctx.meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .unsupervised
            .is_none()
    );
    assert_ne!(need_word(&mut app, &ctx), "unsupervised");
}

/// HX-15e EXIT TEST: A CONTRACT 4 CLAIM WITHOUT A STREAM HAS ITS KEPT CHILD ENDED PAST THE GRACE
/// (D28: every contract 4 claim, streaming or not). A contract 4 claim that never opened a
/// record stream keeps a child and loses its connection: the grace keeps it, and at the grace's
/// end the lapse hands it to the watchdog, which ends its kept child once its claimant is gone
/// and hands the tab to the relaunch (`unsupervised`).
/// NEGATIVE HALF: a contract 3 claim keeps no grace and is handed to no watchdog; a contract 4
/// claimant that still runs but does not lead its tab is never signalled, and its kept child is
/// left alone.
#[test]
fn a_contract_4_claim_without_a_stream_has_its_kept_child_ended_past_the_grace() {
    let app = crate::App::headless_for_test();
    let ctx = Arc::clone(&app.pool.get(0).expect("session 0").ctx);
    let weak = Arc::downgrade(&ctx);

    // NEGATIVE: contract 3 ends with its connection, and nothing is handed on.
    let lapsed = past_the_grace(&ctx, &claim(3));
    assert_eq!(lapsed.universal, None);
    assert!(
        ctx.meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .supervisor
            .is_none()
    );

    let lapsed = past_the_grace(&ctx, &claim(contract::UNIVERSAL));
    assert!(
        ctx.meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .stream
            .is_none(),
        "the claim never streamed"
    );
    let scoped = lapsed.universal.expect("handed to the watchdog");
    assert_eq!(scoped.child.map(|kept| kept.pid), Some(CHILD));

    // NEGATIVE: a claimant that still runs is signalled only while it leads its tab; this one
    // does not (the test tab has no terminal), so nothing is sent to it or its kept child.
    let mut kernel = Kernel::reading(CHILD, BIRTH);
    let dir = aterm_tempfile::TempDir::new().expect("a state directory");
    let ending = crate::harness_host::with_harness_state(dir.path(), || {
        end_harness(
            &weak,
            &lapsed.holder,
            &scoped,
            Why::Grace,
            &Lives(Gone::default()),
            &mut kernel,
        )
    });
    assert_eq!(ending, Ending::ClaimantLives);
    assert!(kernel.sent.is_empty(), "{:?}", kernel.sent);
    assert!(
        ctx.meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .unsupervised
            .is_none()
    );

    // Its claimant is gone: the kept child is ended and the tab reads unsupervised.
    let mut kernel = Kernel::reading(CHILD, BIRTH);
    assert_eq!(
        end_harness(
            &weak,
            &lapsed.holder,
            &scoped,
            Why::Grace,
            &Gone::default(),
            &mut kernel
        ),
        Ending::Ended {
            child: Some(Ok(Ended {
                termed: true,
                killed: false,
                gone: true,
            }))
        }
    );
    assert_eq!(
        kernel.sent.iter().map(|(_, sig)| *sig).collect::<Vec<_>>(),
        vec![libc::SIGTERM]
    );
    assert!(
        ctx.meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .unsupervised
            .is_some()
    );
}

/// Put a person's hold on `ctx`'s tab (`on`) or lift it.
fn hold(ctx: &SessionCtx, on: bool) {
    crate::fabric::apply_hold_for_test(
        ctx,
        on.then(|| crate::fabric::Hold {
            reason: "test".to_string(),
            origin: "local".to_string(),
        }),
    );
}

/// Wait until `waited` holds `want` (a hang detector).
fn waits_for(waited: &Mutex<Vec<(Act, Hand)>>, want: &[(Act, Hand)]) {
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        &format!("the ending waits: {want:?}"),
        || {
            (waited
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_slice()
                == want)
                .then_some(())
        },
    );
}

fn unsupervised(ctx: &SessionCtx) -> bool {
    ctx.meta
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .unsupervised
        .is_some()
}

/// REVIEW OF HX-15e: A CLAIM PLACED WHILE THE WATCHDOG'S ENDING WAITS STOPS IT. Past the grace a
/// live claimant's group signals wait out a person's hold; meanwhile the claimant reconnects and
/// claims the tab again: once the hold lifts, the ending sends nothing to its group or its kept
/// child and the tab never reads `unsupervised`. A dead claimant's kept child that another
/// harness's claim keeps by the time the hold lifts is left alone the same way.
/// NEGATIVE HALF: before these fixes the live claimant's ending went on to its group's signals
/// (here it does not lead its tab, so `ClaimantLives`), and the kept child another claim held was
/// sent `SIGTERM`.
#[test]
fn a_claim_placed_while_the_ending_waits_stops_it() {
    let app = crate::App::headless_for_test();
    let ctx = Arc::clone(&app.pool.get(0).expect("session 0").ctx);
    let weak = Arc::downgrade(&ctx);

    // A live claimant claims again while its group's signals wait.
    let lapsed = past_the_grace(&ctx, &claim(contract::UNIVERSAL));
    let scoped = lapsed.universal.clone().expect("handed to the watchdog");
    let world = Lives::default();
    hold(&ctx, true);
    std::thread::scope(|scope| {
        let ending = scope.spawn(|| {
            let mut kernel = Kernel::reading(CHILD, BIRTH);
            let ending = end_harness(
                &weak,
                &lapsed.holder,
                &scoped,
                Why::Grace,
                &world,
                &mut kernel,
            );
            (ending, kernel.sent)
        });
        waits_for(&world.0.waited, &[(Act::Group, Hand::Hold)]);
        crate::session_timeline::claim_supervisor_scoped(
            &ctx,
            &lapsed.holder,
            Some(CONN + 1),
            claim(contract::UNIVERSAL),
            crate::metrics::now_us(),
            aterm_slog::Actor::Ctl,
        )
        .expect("it claims again");
        hold(&ctx, false);
        let (ending, sent) = ending.join().expect("the ending");
        assert_eq!(ending, Ending::Reclaimed);
        assert!(sent.is_empty(), "{sent:?}");
    });
    assert!(!unsupervised(&ctx));

    // A dead claimant: another harness claims the tab, keeping the same child, while the kept
    // child's end waits.
    let lapsed = past_the_grace(&ctx, &claim(contract::UNIVERSAL));
    let scoped = lapsed.universal.clone().expect("handed to the watchdog");
    let world = Gone::default();
    hold(&ctx, true);
    std::thread::scope(|scope| {
        let ending = scope.spawn(|| {
            let mut kernel = Kernel::reading(CHILD, BIRTH);
            let ending = end_harness(
                &weak,
                &lapsed.holder,
                &scoped,
                Why::Grace,
                &world,
                &mut kernel,
            );
            (ending, kernel.sent)
        });
        waits_for(&world.waited, &[(Act::Child, Hand::Hold)]);
        let mut another = claim(contract::UNIVERSAL);
        another.proven = Proven {
            pid: 4343,
            start: "another-birth".to_string(),
        };
        crate::session_timeline::claim_supervisor_scoped(
            &ctx,
            "grok-alab@4343",
            Some(CONN + 2),
            another,
            crate::metrics::now_us(),
            aterm_slog::Actor::Ctl,
        )
        .expect("another harness's claim");
        hold(&ctx, false);
        let (ending, sent) = ending.join().expect("the ending");
        assert_eq!(ending, Ending::Reclaimed);
        assert!(sent.is_empty(), "{sent:?}");
    });
    assert!(!unsupervised(&ctx));
}

/// REVIEW OF HX-15e: ANY CLAIM ENDS `unsupervised`, A PROVEN UNSCOPED ONE INCLUDED. A dead
/// harness's ending hands its tab over (`unsupervised`); a supervisor's proven claim under the v2
/// grammar (no scope) supervises it, and the need is over. A proven unscoped claim placed while
/// the ending waits keeps the tab from being handed over at all; the kept child, which that
/// claim does not keep, is still ended.
/// NEGATIVE HALF: before the fix only a scoped claim ended `unsupervised`, and the ending handed
/// over a tab a proven unscoped claim held.
#[test]
fn any_claim_ends_unsupervised() {
    let app = crate::App::headless_for_test();
    let ctx = Arc::clone(&app.pool.get(0).expect("session 0").ctx);
    let weak = Arc::downgrade(&ctx);
    let supervise = |conn: u64| {
        crate::session_timeline::claim_supervisor_as(
            &ctx,
            "supervisor@1",
            Some(conn),
            None,
            Some(crate::self_claim::stand_in::running()),
            crate::metrics::now_us(),
            aterm_slog::Actor::Ctl,
        )
        .expect("a proven unscoped claim");
    };

    let lapsed = past_the_grace(&ctx, &claim(contract::UNIVERSAL));
    let scoped = lapsed.universal.clone().expect("handed to the watchdog");
    let mut kernel = Kernel::reading(CHILD, BIRTH);
    assert!(matches!(
        end_harness(
            &weak,
            &lapsed.holder,
            &scoped,
            Why::Grace,
            &Gone::default(),
            &mut kernel
        ),
        Ending::Ended { child: Some(Ok(_)) }
    ));
    assert!(unsupervised(&ctx));
    supervise(CONN + 1);
    assert!(
        !unsupervised(&ctx),
        "a proven unscoped claim supervises the tab"
    );
    assert!(crate::session_timeline::release_supervisor(
        &ctx,
        None,
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl
    ));

    // Placed while the ending waits: the kept child is ended, the tab is not handed over.
    let lapsed = past_the_grace(&ctx, &claim(contract::UNIVERSAL));
    let scoped = lapsed.universal.clone().expect("handed to the watchdog");
    let world = Gone::default();
    hold(&ctx, true);
    std::thread::scope(|scope| {
        let ending = scope.spawn(|| {
            let mut kernel = Kernel::reading(CHILD, BIRTH);
            let ending = end_harness(
                &weak,
                &lapsed.holder,
                &scoped,
                Why::Grace,
                &world,
                &mut kernel,
            );
            (ending, kernel.sent)
        });
        waits_for(&world.waited, &[(Act::Child, Hand::Hold)]);
        supervise(CONN + 2);
        hold(&ctx, false);
        let (ending, sent) = ending.join().expect("the ending");
        assert!(matches!(ending, Ending::Ended { child: Some(Ok(_)) }));
        assert_eq!(
            sent.iter().map(|(_, sig)| *sig).collect::<Vec<_>>(),
            vec![libc::SIGTERM]
        );
    });
    assert!(
        !unsupervised(&ctx),
        "a claim stands: the tab is not handed over"
    );
}

/// REVIEW OF HX-15e: A STREAM THAT CLOSES LATE LEAVES WHAT THE STREAM OPEN NOW READ. A claimant's
/// older stream lane (one blocked writing to a peer that stopped reading) ends after the same
/// claimant opened a stream on another connection, whose lane read its beats missed (`unknown`):
/// the older lane's close leaves that reading. The close of the stream that counts it clears it.
/// NEGATIVE HALF: before the fix any close of the claimant's streams cleared the reading.
#[test]
fn a_stream_closing_late_leaves_the_reading_of_the_stream_open_now() {
    let app = crate::App::headless_for_test();
    let ctx = Arc::clone(&app.pool.get(0).expect("session 0").ctx);
    let proven = claim(contract::UNIVERSAL).proven;
    let watched = || {
        ctx.timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .agent()
            .watched
            .as_ref()
            .map(|watched| watched.watched)
    };
    let _ = ctx
        .meta
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .open_stream(&proven, "epoch", CONN + 1);
    let _ = ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .set_watched(Some(WatchedClaimant {
            watched: Watched::Missed,
            proven: proven.clone(),
        }));

    stream_closed(&ctx, &proven, CONN);
    assert_eq!(
        watched(),
        Some(Watched::Missed),
        "the open stream's reading stands"
    );

    ctx.meta
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .close_stream(&proven, CONN + 1);
    stream_closed(&ctx, &proven, CONN + 1);
    assert_eq!(watched(), None, "the stream that counted it closed");
}
