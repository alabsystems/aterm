// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! What a control verb does to a session an update has parked, before the
//! Commit: its blocking write into a session not in front (`paste-bin`,
//! `feed-bin`, `send`, `paste --stdin`) is never cut by the Commit's `_exit`
//! (`fence_handoff_egress`), and its `reset` is never answered `OK` and then
//! undone by the successor's restore (`SessionFabric::note_direct_reset`).

use std::sync::{Arc, Mutex};

use super::ParkAttempt;
use super::late_park_record_tests::prelaunch_record;
use crate::native_updater_service::ApplyMode;

const BODY: usize = 64 * 1024;
const BRACKETED: &[u8] = b"\x1b[?2004h";
const FRAME: usize = BODY + b"\x1b[200~\x1b[201~".len();

struct Parked {
    app: crate::App,
    slave: i32,
    term: Arc<Mutex<aterm_core::terminal::Terminal>>,
    ctx: Arc<crate::SessionCtx>,
    nonce: String,
    reject: Option<std::sync::mpsc::SyncSender<()>>,
    _channels: (
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::Receiver<super::HandoffStandDown>,
        std::sync::mpsc::Receiver<super::HandoffTransferJob>,
        std::sync::mpsc::Receiver<()>,
    ),
    // Last, so the descriptors close after the `App` that wrote to them.
    _pty: Pty,
}

struct Pty(i32, i32);

impl Drop for Pty {
    fn drop(&mut self) {
        aterm_pty::close_fd(self.1);
        aterm_pty::close_fd(self.0);
    }
}

impl Parked {
    fn new(armed: &[u8]) -> Self {
        Self::with_attention(armed, None)
    }

    fn with_attention(armed: &[u8], attention: Option<&str>) -> Self {
        let (parked, attempt) = Self::with_app(armed, attention, |_| {});
        assert!(
            matches!(attempt, ParkAttempt::Parked),
            "PRECONDITION: the park landed ({attempt:?})"
        );
        parked
    }

    fn with_app(
        armed: &[u8],
        attention: Option<&str>,
        configure: impl FnOnce(&mut crate::App),
    ) -> (Self, ParkAttempt) {
        let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
        // SAFETY: a zeroed termios filled by `tcgetattr` on the live slave.
        unsafe {
            let mut raw: libc::termios = std::mem::zeroed();
            assert_eq!(libc::tcgetattr(slave, &mut raw), 0);
            libc::cfmakeraw(&mut raw);
            assert_eq!(libc::tcsetattr(slave, libc::TCSANOW, &raw), 0);
        }
        let mut app = crate::App::headless_for_test_with_sink(Arc::new(
            aterm_session::sink::SinkWriter::new(master),
        ));
        let session = &mut app.pool.sessions.get_mut(&0).expect("session 0").session;
        session.master = master;
        let term = session.term.clone();
        let ctx = session.ctx.clone();
        crate::term_lock(&term).process(armed);
        if let Some(attention) = attention {
            crate::session_timeline::write_attention_owned(
                &ctx,
                "review",
                crate::session_timeline::MetaEdit::Set(attention),
                aterm_slog::Actor::Ctl,
            )
            .expect("the alert stands before the manifest is drawn");
        }
        configure(&mut app);
        let (record, cancelled, stood_down, transferred) =
            prelaunch_record(7, ApplyMode::Immediate);
        let nonce = record.nonce.clone();
        app.update_handoff_prelaunch = Some(record);
        let attempt = app.park_and_transfer_to_prelaunched_successor(std::time::Instant::now());
        app.handoff_reposts = Some(Vec::new());
        let (reject, rejected) = std::sync::mpsc::sync_channel(1);
        (
            Self {
                app,
                slave,
                term,
                ctx,
                nonce,
                reject: Some(reject),
                _channels: (cancelled, stood_down, transferred, rejected),
                _pty: Pty(master, slave),
            },
            attempt,
        )
    }

    fn proof_ready(&mut self) -> crate::UpdateHandoffCompletion {
        let (commit_rd, commit_wr) = std::io::pipe().expect("a commit pipe");
        drop(commit_rd);
        crate::UpdateHandoffCompletion {
            attempt_id: 7,
            nonce: Some(self.nonce.clone()),
            child_pid: None,
            outcome: crate::UpdateHandoffOutcome::ProofReady,
            commit_fd: Some(commit_wr.into()),
            reject: self.reject.take(),
            reconcile: None,
            detail: String::new(),
            input_drain_spins: 1,
            child_death: crate::ChildDeathEvidence::Unobserved,
        }
    }

    fn pass(
        &mut self,
        completion: crate::UpdateHandoffCompletion,
    ) -> Vec<crate::UpdateHandoffCompletion> {
        if let Some(pending) = self.app.pending_update_handoff.as_mut() {
            pending.commit_drain_started =
                Some(std::time::Instant::now() - std::time::Duration::from_millis(100));
        }
        self.app.finish_proven_handoff(completion);
        self.app
            .handoff_reposts
            .replace(Vec::new())
            .expect("re-posts")
    }

    fn run_to_verdict(&mut self, completion: crate::UpdateHandoffCompletion) -> String {
        let mut next = Some(completion);
        while let Some(completion) = next.take() {
            assert!(
                self.app.handoff_rejections.is_empty(),
                "{:?}",
                self.app.handoff_rejections
            );
            next = self.pass(completion).pop();
        }
        assert_eq!(self.app.handoff_rejections.len(), 1);
        self.app.handoff_rejections[0].clone()
    }

    fn read_frame(&self, writer: std::thread::JoinHandle<()>) -> Vec<u8> {
        let mut frame = Vec::with_capacity(FRAME);
        let mut buf = [0u8; 4096];
        while frame.len() < FRAME {
            assert!(
                readable(self.slave, 60_000),
                "the rest of the frame arrives"
            );
            // SAFETY: reads into a live stack buffer from the open slave.
            let n = unsafe { libc::read(self.slave, buf.as_mut_ptr().cast(), buf.len()) };
            let n = usize::try_from(n).expect("a read");
            frame.extend_from_slice(&buf[..n]);
        }
        writer.join().expect("the paste's writer");
        assert!(!readable(self.slave, 0), "nothing after the frame");
        frame
    }
}

fn paste_mid_frame(
    term: Arc<Mutex<aterm_core::terminal::Terminal>>,
    ctx: Arc<crate::SessionCtx>,
    slave: i32,
) -> std::thread::JoinHandle<()> {
    let writer = std::thread::spawn(move || {
        let _ = crate::app_input::tracked_egress(
            &term,
            &ctx.modes,
            &ctx.sink,
            &ctx.output_echo,
            &crate::input::InputEvent::Paste("x".repeat(BODY), crate::input::PasteFraming::AtDrain),
            crate::input::EgressMode::Backpressured,
        );
    });
    assert!(
        aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            std::time::Duration::from_millis(1),
            || readable(slave, 0).then_some(())
        )
        .is_some(),
        "the paste's first bytes reach the kernel"
    );
    assert!(
        !writer.is_finished(),
        "PRECONDITION: the frame is part-way in"
    );
    writer
}

impl Drop for Parked {
    fn drop(&mut self) {
        self.app.pending_update_handoff = None;
        self.app.update_handoff_prelaunch = None;
    }
}

fn readable(fd: i32, timeout_ms: i32) -> bool {
    let mut pollfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one initialized pollfd.
    unsafe { libc::poll(&mut pollfd, 1, timeout_ms) == 1 }
}

fn assert_whole(frame: &[u8]) {
    assert_eq!(frame.len(), FRAME);
    assert!(frame.starts_with(b"\x1b[200~"), "the paste opens");
    assert!(frame.ends_with(b"\x1b[201~"), "and closes");
}

const WRITTEN: &str = "attempt-bound Commit pipe closed before its atomic write";

#[test]
fn a_setup_offer_edited_after_collected_facts_cannot_reach_the_commit_write() {
    use crate::native_app::{ActionInvocation, AppEvent, SemanticInput};
    use crate::workstation_setup::Request;
    use aterm_types::macos_setup::Plan;

    fn edit_offer(app: &mut crate::App) {
        let wid = crate::WindowId(0);
        let (_, view) = app.active_native_view(wid).unwrap();
        app.dispatch_native_view_event(
            wid,
            view,
            AppEvent::Action(ActionInvocation {
                id: crate::native_ui::ActionId::new("system-state/url"),
                value: Some(SemanticInput::Text("http://draft.local/v1/team".into())),
            }),
        )
        .unwrap();
        assert!(app.workstation_setup_holds_handoff());
    }

    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    for edit_after_facts in [false, true] {
        let (send, receive) = std::sync::mpsc::channel();
        let request = Request::new(Plan::default(), send);
        let offered = request.clone();
        let (mut parked, attempt) = Parked::with_app(b"", None, move |app| {
            app.headless = false;
            app.user_input_recent = |_| false;
            app.windows
                .get_mut(&crate::WindowId(0))
                .unwrap()
                .os_window_for_test = true;
            app.offer_workstation_setup(offered);
            app.headless = true;
        });
        assert!(matches!(attempt, ParkAttempt::Parked), "{attempt:?}");
        assert!(!parked.app.workstation_setup_holds_handoff());
        if edit_after_facts {
            super::BEFORE_FINAL_SETUP_CHECK.set(Some(edit_offer));
        }
        let completion = parked.proof_ready();
        let verdict = parked.run_to_verdict(completion);
        if edit_after_facts {
            assert_ne!(verdict, WRITTEN, "the final guard preserves the new draft");
            assert!(
                super::BEFORE_FINAL_SETUP_CHECK.take().is_none(),
                "the late edit ran"
            );
        } else {
            assert_eq!(verdict, WRITTEN, "an untouched offer permits the write");
        }
        assert!(request.pending(), "a returned Commit preserves the offer");
        assert!(receive.try_recv().is_err(), "no approval was fabricated");
    }
}

#[cfg(target_os = "macos")]
mod queued_setup_custody {
    use std::cell::RefCell;

    use super::*;
    use crate::native_app::{ActionInvocation, AppEvent};
    use crate::native_ui::ActionId;
    use crate::system_state::{SetupOperation, SetupStatus};
    use crate::system_state_host::{InitialSetupApplyReceipt, InitialSetupHandlers};
    use aterm_types::macos_setup::Plan;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Operation {
        Apply,
        Restore,
    }

    #[derive(Default)]
    struct Worker {
        operation: Option<Operation>,
        plan: Option<Plan>,
        receipt: Option<InitialSetupApplyReceipt>,
    }

    thread_local! {
        static WORKER: RefCell<Worker> = const { RefCell::new(Worker {
            operation: None,
            plan: None,
            receipt: None,
        }) };
    }

    struct WorkerScope;

    impl WorkerScope {
        fn new() -> Self {
            WORKER.with(|worker| *worker.borrow_mut() = Worker::default());
            Self
        }
    }

    impl Drop for WorkerScope {
        fn drop(&mut self) {
            WORKER.with(|worker| *worker.borrow_mut() = Worker::default());
        }
    }

    /// The setup worker boundary: an Apply's actual receipt is retained without
    /// accepting it, as while another process holds the setup records. No Mac
    /// setup or profile lock runs in this GUI test.
    fn handlers() -> InitialSetupHandlers {
        InitialSetupHandlers {
            apply: |plan, _, _, receipt| {
                WORKER.with(|worker| {
                    let mut worker = worker.borrow_mut();
                    assert!(worker.operation.is_none());
                    worker.operation = Some(Operation::Apply);
                    worker.plan = Some(plan);
                    worker.receipt = Some(receipt);
                });
                true
            },
            restore: || {
                WORKER.with(|worker| {
                    let mut worker = worker.borrow_mut();
                    assert!(worker.operation.is_none());
                    worker.operation = Some(Operation::Restore);
                });
                true
            },
            ..InitialSetupHandlers::inert_for_test()
        }
    }

    fn pending(operation: Operation) -> bool {
        WORKER.with(|worker| {
            let worker = worker.borrow();
            worker.operation == Some(operation)
                && (operation != Operation::Apply || worker.receipt.is_some())
        })
    }

    fn queued_status(operation: Operation) -> SetupStatus {
        assert!(
            pending(operation),
            "the actual handler owns the queued intent"
        );
        SetupStatus {
            busy: true,
            queued: true,
            operation: (operation == Operation::Restore).then_some(SetupOperation::Restore),
            plan: WORKER
                .with(|worker| worker.borrow().plan.clone())
                .or_else(|| Some(Plan::default())),
            ..SetupStatus::default()
        }
    }

    fn cannot_cut_queued_operation(operation: Operation) {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        for queue in [false, true] {
            let _worker = WorkerScope::new();
            let wid = crate::WindowId(0);
            let mut view = None;
            let (mut parked, attempt) = Parked::with_app(b"", None, |app| {
                let window = app.windows.get_mut(&wid).expect("the private window");
                window.rows = 90;
                window.cols = 160;
                assert!(app.open_system_state_tab());
                view = Some(app.active_native_view(wid).expect("System State"));
                app.set_system_state_handlers_for_test(handlers());
                app.publish_system_state(SetupStatus {
                    plan: Some(Plan::default()),
                    ..SetupStatus::default()
                });
                if queue {
                    let (_, view) = view.expect("the setup page");
                    let key = match operation {
                        Operation::Apply => "system-state/group/2/leave",
                        Operation::Restore => "system-state/restore",
                    };
                    app.dispatch_native_view_event(
                        wid,
                        view,
                        AppEvent::Action(ActionInvocation {
                            id: ActionId::new(key),
                            value: None,
                        }),
                    )
                    .expect("the page's real action is dispatched before the park");
                    app.drain_system_state_actions();
                    assert!(pending(operation), "the real action reached its handler");
                    app.publish_system_state(queued_status(operation));
                    app.resolve_system_state_apply();
                    assert!(pending(operation), "no durable admission was acknowledged");
                }
            });
            let (instance, view) = view.expect("the page was open before the park");
            if queue {
                assert!(
                    parked
                        .app
                        .native_runtime
                        .presentation(instance, view)
                        .expect("the actual page projection")
                        .indicators
                        .busy,
                    "the queued worker is still presented as pending"
                );
            }
            assert!(
                !parked.app.workstation_setup.pending(),
                "this is a manual page operation, not the original startup offer"
            );
            match attempt {
                ParkAttempt::Parked => {}
                ParkAttempt::NotYet(_) | ParkAttempt::Failed(_) if queue => {
                    assert!(pending(operation), "a refusal retains the queued intent");
                    assert!(
                        parked.app.pending_update_handoff.is_none(),
                        "a refused startup has no parked transfer to Commit"
                    );
                    assert!(
                        parked
                            .app
                            .handoff_reposts
                            .as_ref()
                            .is_some_and(Vec::is_empty),
                        "a refused startup posts no Commit continuation"
                    );
                    continue;
                }
                other => panic!("PRECONDITION: the unchanged park must land ({other:?})"),
            }
            let capture = parked
                .app
                .pending_update_handoff
                .as_ref()
                .expect("the queued operation predates this actual manifest");
            assert_eq!(
                parked.app.update_handoff_activity_epoch, capture.activity_epoch,
                "the manifest was drawn after the page action, without resetting its epoch"
            );
            assert!(
                parked.app.carried_claims_held(&capture.carried_claims),
                "the ordinary session claims remain unchanged"
            );
            let completion = parked.proof_ready();
            let verdict = parked.run_to_verdict(completion);
            if queue {
                assert!(
                    pending(operation),
                    "the queued intent still has no durable successor"
                );
                assert_ne!(
                    verdict, WRITTEN,
                    "a queued {operation:?} must survive instead of reaching Commit's atomic write"
                );
            } else {
                assert_eq!(
                    verdict, WRITTEN,
                    "CONTROL: the unchanged page permits the actual Commit write"
                );
            }
        }
    }

    #[test]
    fn a_queued_apply_waiting_for_setup_cannot_be_cut_by_commit() {
        cannot_cut_queued_operation(Operation::Apply);
    }

    #[test]
    fn a_queued_restore_waiting_for_setup_cannot_be_cut_by_commit() {
        cannot_cut_queued_operation(Operation::Restore);
    }
}

#[test]
fn an_alert_clear_acknowledged_at_the_commit_write_cannot_restore_the_drawn_alert() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    for clear in [false, true] {
        let mut parked = Parked::with_attention(b"", Some("Review is needed"));
        let drawn = parked
            .app
            .pending_update_handoff
            .as_ref()
            .expect("the parked attempt")
            .carried_claims
            .clone();
        assert!(
            parked.app.carried_claims_held(&drawn),
            "PRECONDITION: the manifest carries the standing alert"
        );
        assert!(
            !parked
                .ctx
                .meta
                .lock()
                .unwrap()
                .handoff_attention_owners()
                .is_empty(),
            "PRECONDITION: the live alert stands"
        );
        let (term, ctx, store) = (
            parked.term.clone(),
            parked.ctx.clone(),
            parked.app.store.clone(),
        );
        let (answered, reply) = std::sync::mpsc::sync_channel(1);
        parked.app.handoff_before_egress_fence = Some(Box::new(move || {
            let result = clear.then(|| {
                crate::control::cmd_meta(&term, &store, 0, &ctx, "unset attention owner=review")
            });
            answered.send(result).expect("the test receives the reply");
        }));
        let completion = parked.proof_ready();
        let verdict = parked.run_to_verdict(completion);
        let answered = reply.try_recv().expect("the hook ran before the write");
        if clear {
            assert_eq!(answered, Some(("OK\n".to_string(), true)));
            assert!(
                parked
                    .ctx
                    .meta
                    .lock()
                    .unwrap()
                    .handoff_attention_owners()
                    .is_empty(),
                "the acknowledged clear took the live alert down"
            );
            assert!(
                !parked.app.carried_claims_held(&drawn),
                "the manifest's alert is obsolete"
            );
            assert!(
                verdict.contains("structural activity"),
                "an acknowledged alert clear stands down the stale Commit: {verdict}"
            );
        } else {
            assert_eq!(answered, None);
            assert_eq!(
                verdict, WRITTEN,
                "NEGATIVE CONTROL: an unchanged alert does not hold the Commit"
            );
        }
    }
}

#[test]
fn persisted_policy_changes_at_the_commit_write_stand_down_the_old_manifest() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    for change in [None, Some("set autopilot off"), Some("set questions ask")] {
        let mut parked = Parked::new(b"");
        let (term, ctx, store) = (
            parked.term.clone(),
            parked.ctx.clone(),
            parked.app.store.clone(),
        );
        let (answered, reply) = std::sync::mpsc::sync_channel(1);
        parked.app.handoff_before_egress_fence = Some(Box::new(move || {
            answered
                .send(change.map(|change| crate::control::cmd_meta(&term, &store, 0, &ctx, change)))
                .expect("the test receives the metadata reply");
        }));
        let completion = parked.proof_ready();
        let verdict = parked.run_to_verdict(completion);
        let answered = reply.try_recv().expect("the write-edge hook ran");
        if change.is_some() {
            assert_eq!(answered, Some(("OK\n".to_string(), true)));
            assert!(
                verdict.contains("structural activity"),
                "the acknowledged policy must cross the next handoff: {change:?}: {verdict}"
            );
        } else {
            assert_eq!(answered, None);
            assert_eq!(
                verdict, WRITTEN,
                "unchanged metadata permits the actual write"
            );
        }
    }
}

#[test]
fn the_final_claim_fence_refuses_contention_and_holds_its_locks_through_the_write() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let parked = Parked::new(b"");
    let expected = parked
        .app
        .pending_update_handoff
        .as_ref()
        .expect("parked attempt")
        .carried_claims
        .first()
        .expect("one handed session")
        .clone();
    let sources = [super::HandoffClaimSource {
        expected,
        ctx: parked.ctx.clone(),
        topics: crate::fabric::render_topics(&parked.ctx.fabric.topics()),
    }];
    for busy in ["meta", "timeline", "banner"] {
        let meta = (busy == "meta").then(|| parked.ctx.meta.lock().unwrap());
        let timeline = (busy == "timeline").then(|| parked.ctx.timeline.lock().unwrap());
        let banner = (busy == "banner").then(|| parked.ctx.box_banner.try_guard().unwrap());
        let mut wrote = false;
        let refused = super::with_handoff_claims(&sources, || wrote = true);
        assert_eq!(refused, None);
        assert!(!wrote, "a contended claim fence cannot write a Commit");
        drop(banner);
        drop(timeline);
        drop(meta);
        let written = super::with_handoff_claims(&sources, || {
            assert!(matches!(
                parked.ctx.meta.try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            ));
            assert!(matches!(
                parked.ctx.timeline.try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            ));
            assert!(parked.ctx.box_banner.try_guard().is_none());
            Err::<(), _>("fixture write failure")
        });
        assert_eq!(written, Some(Err("fixture write failure")));
        assert!(
            parked.ctx.meta.try_lock().is_ok(),
            "rollback can acquire meta"
        );
        assert!(
            parked.ctx.timeline.try_lock().is_ok(),
            "rollback can acquire timeline"
        );
        assert!(
            parked.ctx.box_banner.try_guard().is_some(),
            "rollback can acquire banner state"
        );
    }
}

#[test]
fn a_notice_admitted_after_the_manifest_stands_commit_down() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let parked = Parked::new(b"");
    let expected = parked
        .app
        .pending_update_handoff
        .as_ref()
        .unwrap()
        .carried_claims[0]
        .clone();
    let mut sources = [super::HandoffClaimSource {
        expected,
        ctx: parked.ctx.clone(),
        topics: crate::fabric::render_topics(&parked.ctx.fabric.topics()),
    }];
    assert_eq!(
        super::with_handoff_claims(&sources, || "unchanged"),
        Some("unchanged")
    );
    let report = crate::term_lock(&parked.term).program_state();
    assert!(
        parked.ctx.box_banner.admit(&report),
        "the program notification passed after capture"
    );
    let mut wrote = false;
    assert_eq!(super::with_handoff_claims(&sources, || wrote = true), None);
    assert!(
        !wrote,
        "the manifest cannot lose the admitted notice's custody"
    );
    sources[0].expected = sources[0]
        .expected
        .clone()
        .with_banner(parked.ctx.box_banner.snapshot());
    assert_eq!(
        super::with_handoff_claims(&sources, || "recaptured"),
        Some("recaptured")
    );
    assert!(
        parked.ctx.box_banner.admit(&report),
        "another notice may leave the same seq standing"
    );
    assert_eq!(
        super::with_handoff_claims(&sources, || "stale"),
        None,
        "the revision detects equal-valued repeated notices"
    );
    sources[0].expected = sources[0]
        .expected
        .clone()
        .with_banner(parked.ctx.box_banner.snapshot());
    assert_eq!(
        super::with_handoff_claims(&sources, || "current"),
        Some("current")
    );
    parked.ctx.box_banner.herald(7, |_| ((), true));
    assert_eq!(
        super::with_handoff_claims(&sources, || "stale"),
        None,
        "a newly queued box receipt cannot be omitted by Commit"
    );
}

#[test]
fn every_handed_session_needs_exactly_one_claim_row_at_the_write() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let parked = Parked::new(b"");
    let pending = parked
        .app
        .pending_update_handoff
        .as_ref()
        .expect("parked attempt");
    assert!(
        super::handoff_claim_sources(&parked.app.pool, &pending.live, &pending.carried_claims)
            .is_some(),
        "the unchanged manifest covers its actual handed session"
    );
    assert!(
        super::handoff_claim_sources(&parked.app.pool, &pending.live, &[]).is_none(),
        "a missing claim row cannot leave that session unfenced"
    );
    let mut wrong = pending.carried_claims.clone();
    wrong[0].local_id = u64::MAX;
    assert!(
        super::handoff_claim_sources(&parked.app.pool, &pending.live, &wrong).is_none(),
        "matching counts cannot substitute a different session"
    );
    let duplicate = [
        pending.carried_claims[0].clone(),
        pending.carried_claims[0].clone(),
    ];
    assert!(
        super::handoff_claim_sources(&parked.app.pool, &pending.live, &duplicate).is_none(),
        "an extra row cannot borrow the handed session"
    );
}

#[test]
fn a_control_paste_in_flight_holds_the_commit_until_its_frame_is_whole() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut parked = Parked::new(BRACKETED);
    let writer = paste_mid_frame(parked.term.clone(), parked.ctx.clone(), parked.slave);
    let live = parked
        .app
        .pending_update_handoff
        .as_ref()
        .expect("the parked attempt")
        .live
        .clone();
    assert!(
        !super::handoff_egress_settled(
            &parked.app.pool,
            &live,
            &crate::control::input_replies_of(&parked.app.store)
        ),
        "a frame part-way into the kernel is egress still in flight"
    );
    let completion = parked.proof_ready();
    let mut reposted = parked.pass(completion);
    assert_eq!(
        parked.app.handoff_rejections,
        Vec::<String>::new(),
        "no Commit is written over the open paste"
    );
    assert_eq!(reposted.len(), 1, "the drain gate waits for it");

    assert_whole(&parked.read_frame(writer));
    assert!(super::handoff_egress_settled(
        &parked.app.pool,
        &live,
        &crate::control::input_replies_of(&parked.app.store)
    ));
    assert_eq!(
        parked.run_to_verdict(reposted.remove(0)),
        WRITTEN,
        "the frame whole, the Commit is written"
    );
}

#[test]
fn a_control_paste_begun_after_the_drain_gate_stands_the_commit_down() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut parked = Parked::new(BRACKETED);
    let (began, writer) = std::sync::mpsc::sync_channel(1);
    let slave = parked.slave;
    let (term, ctx) = (parked.term.clone(), parked.ctx.clone());
    parked.app.handoff_before_egress_fence = Some(Box::new(move || {
        began
            .send(paste_mid_frame(term, ctx, slave))
            .expect("the test waits for the writer");
    }));
    let completion = parked.proof_ready();
    let verdict = parked.run_to_verdict(completion);
    let writer = writer.try_recv().expect("the paste began at the write");
    assert!(
        verdict.contains("tolerated input"),
        "the egress fence refused the write: {verdict}"
    );
    assert_whole(&parked.read_frame(writer));
}

#[test]
fn an_input_reply_still_owed_holds_the_commit_until_it_is_delivered() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut parked = Parked::new(b"");
    let replies = crate::control::input_replies_of(&parked.app.store);
    let live = parked
        .app
        .pending_update_handoff
        .as_ref()
        .expect("the parked attempt")
        .live
        .clone();
    let owed = replies.enter("@s-1 paste-bin 6").expect("an input verb");
    assert!(
        !super::handoff_egress_settled(&parked.app.pool, &live, &replies),
        "a reply owed is not settled"
    );
    let completion = parked.proof_ready();
    let mut reposted = parked.pass(completion);
    assert_eq!(
        parked.app.handoff_rejections,
        Vec::<String>::new(),
        "no Commit is written over the owed reply"
    );
    assert_eq!(reposted.len(), 1, "the drain gate waits for it");

    drop(owed);
    assert!(super::handoff_egress_settled(
        &parked.app.pool,
        &live,
        &replies
    ));
    assert_eq!(
        parked.run_to_verdict(reposted.remove(0)),
        WRITTEN,
        "the reply delivered, the Commit is written"
    );
}

#[test]
fn an_input_reply_owed_at_the_write_stands_the_commit_down() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    for owed in [true, false] {
        let mut parked = Parked::new(b"");
        let replies = crate::control::input_replies_of(&parked.app.store);
        let (held, guard) = std::sync::mpsc::sync_channel(1);
        parked.app.handoff_before_egress_fence = Some(Box::new(move || {
            held.send(owed.then(|| replies.enter("send ls").expect("an input verb")))
                .expect("the test holds the reply");
        }));
        let completion = parked.proof_ready();
        let verdict = parked.run_to_verdict(completion);
        let _guard = guard.try_recv().expect("the hook ran at the write");
        if owed {
            assert!(
                verdict.contains("tolerated input"),
                "an owed reply stands the Commit down: {verdict}"
            );
        } else {
            assert_eq!(
                verdict, WRITTEN,
                "NEGATIVE CONTROL: nothing owed, the Commit is written"
            );
        }
    }
}

#[test]
fn a_reset_on_a_parked_session_stands_the_commit_down() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let stuck = b"\x1b[?1049h\x1b[?1003h\x1b[?1006h\x1b[?2004h\x1b[>1u";
    for reset in [true, false] {
        let mut parked = Parked::new(stuck);
        assert!(
            crate::term_lock(&parked.term).program_owns_terminal(),
            "PRECONDITION: the program's modes were carried"
        );
        if reset {
            let reply = crate::manual_reset::cmd_reset(0, &parked.term, &parked.ctx, "");
            assert!(
                reply.starts_with("OK reset reverted=kitty-alt,alt,mouse,mouse-encoding,paste"),
                "{reply:?}"
            );
        }
        let completion = parked.proof_ready();
        let verdict = parked.run_to_verdict(completion);
        if reset {
            assert!(
                verdict.contains("structural activity"),
                "the reset stands the Commit down: {verdict}"
            );
            assert!(
                !crate::term_lock(&parked.term).program_owns_terminal(),
                "and this engine runs on, reset"
            );
        } else {
            assert_eq!(verdict, WRITTEN, "CONTROL: no reset, the Commit is written");
        }
    }
}

#[test]
fn a_reset_at_the_write_is_never_answered_ok() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut parked = Parked::new(b"\x1b[?1049h\x1b[?1003h");
    let (answered, reply) = std::sync::mpsc::sync_channel(1);
    let (term, ctx) = (parked.term.clone(), parked.ctx.clone());
    parked.app.handoff_before_egress_fence = Some(Box::new(move || {
        answered
            .send(crate::manual_reset::request(
                0,
                &term,
                &ctx,
                crate::manual_reset::ResetSource::Verb,
                Some(std::time::Duration::from_millis(20)),
            ))
            .expect("the test waits for the reply");
    }));
    let completion = parked.proof_ready();
    let verdict = parked.run_to_verdict(completion);
    let reply = reply.try_recv().expect("the reset ran at the write");
    assert_eq!(
        reply,
        Err(crate::manual_reset::RESET_BUSY.to_string()),
        "a reset the Commit can no longer see is not answered OK"
    );
    assert_eq!(verdict, WRITTEN, "and the Commit is written");
}

#[test]
fn the_outgoing_commit_writes_the_persons_stamps_at_its_write() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    for (keyed, typed) in [(true, false), (true, true), (false, false)] {
        let mut parked = Parked::new(b"");
        if keyed {
            parked
                .app
                .pool
                .get(0)
                .expect("session 0")
                .ctx
                .human_input
                .carry(90_000, crate::metrics::now_us());
        }
        if typed {
            parked
                .app
                .pool
                .get(0)
                .expect("session 0")
                .ctx
                .human_input
                .carry_typed(30_000, crate::metrics::now_us());
        }
        let path = crate::seamless::person_stamps_path(
            &crate::seamless::outgoing_manifest_path(&parked.nonce).expect("a manifest path"),
        );
        let (seen, at_write) = std::sync::mpsc::sync_channel(1);
        let probe = path.clone();
        parked.app.handoff_before_egress_fence = Some(Box::new(move || {
            seen.send(std::fs::read_to_string(&probe).ok())
                .expect("the test waits for the read");
        }));
        let completion = parked.proof_ready();
        assert_eq!(parked.run_to_verdict(completion), WRITTEN);
        let body = at_write
            .try_recv()
            .expect("read at the write")
            .expect("the sidecar stands at the write");
        let rows: Vec<_> = body
            .lines()
            .map(|line| line.split_once(' ').expect("a stamp row"))
            .collect();
        assert_eq!(rows.len(), 2, "one gesture and one typing row: {body:?}");
        let age = rows
            .iter()
            .find_map(|&(id, age)| (id == "0").then_some(age))
            .expect("the session's gesture row");
        if keyed {
            assert!(age.parse::<u64>().is_ok_and(|ms| ms >= 90_000), "{body:?}");
        } else {
            assert!(
                age.strip_prefix('-')
                    .is_some_and(|ms| ms.parse::<u64>().is_ok()),
                "NEGATIVE CONTROL: nobody keyed: {body:?}"
            );
        }
        let typing_age = rows
            .iter()
            .find_map(|&(id, age)| (id == "t0").then_some(age))
            .expect("the session's typing provenance row");
        if typed {
            assert!(
                typing_age.parse::<u64>().is_ok_and(|ms| ms >= 30_000),
                "{body:?}"
            );
        } else {
            assert_eq!(typing_age, "-", "no typing was carried: {body:?}");
        }
        assert!(!path.exists(), "the Commit returned: taken back");
    }
}

#[cfg(target_os = "macos")]
thread_local! {
    /// What the Commit did, in order: System State's settles and the write.
    static COMMIT_STEPS: std::cell::RefCell<Vec<&'static str>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// Whether each settle waits a fence grain, as one does for a Stop's save.
    static SETTLE_WAITS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `parked`'s System State as a test reads it: each settle noted in
/// [`COMMIT_STEPS`], and the write noted there too.
#[cfg(target_os = "macos")]
fn note_settles_and_the_write(parked: &mut Parked) {
    parked
        .app
        .set_system_state_handlers_for_test(crate::InitialSetupHandlers {
            apply: |_, _, _, _| false,
            check: |_| crate::CheckStarted::default(),
            restore: || false,
            status: crate::SetupStatus::default,
            observe: |_| false,
            look_for_token: |_| false,
            stop: || {},
            settle: || {
                if SETTLE_WAITS.get() {
                    std::thread::sleep(super::HANDOFF_INPUT_DRAIN_YIELD);
                }
                COMMIT_STEPS.with(|steps| steps.borrow_mut().push("settle"));
            },
            cut_off: |going| {
                COMMIT_STEPS.with(|steps| {
                    steps
                        .borrow_mut()
                        .push(if going { "cut off" } else { "taken back" });
                });
            },
            sync_on: || false,
            fda_granted: || {},
            tools_installed: || {},
            join_phrase: |_, _| false,
        });
    parked.app.handoff_before_egress_fence = Some(Box::new(|| {
        COMMIT_STEPS.with(|steps| steps.borrow_mut().push("write"));
    }));
}

/// A System State Stop clicked while the handoff drains input may still owe its save: the
/// Commit settles it among what its claim answers and again right before its write, as a quit
/// does. This attempt never passed the handoff's start, so both settles seen are the Commit's.
/// The setup stages the Commit cuts off are noted right before the write, and, since this
/// write returned (its pipe closed), taken back: the handoff did not go (design §6.5).
#[cfg(target_os = "macos")]
#[test]
fn the_commit_settles_a_system_state_stop_before_its_write() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut parked = Parked::new(b"");
    note_settles_and_the_write(&mut parked);
    let completion = parked.proof_ready();
    assert_eq!(parked.run_to_verdict(completion), WRITTEN);
    assert_eq!(
        COMMIT_STEPS.with(|steps| steps.take()),
        ["settle", "settle", "write", "cut off", "taken back"]
    );
}

/// A settle that waited held the main thread, and keys typed meanwhile would die with the
/// `_exit`, so the claim goes through one more input fence; the write's own settle still comes
/// right before the write.
#[cfg(target_os = "macos")]
#[test]
fn a_system_state_settle_that_waits_holds_the_commit_through_one_more_fence() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut parked = Parked::new(b"");
    note_settles_and_the_write(&mut parked);
    SETTLE_WAITS.set(true);
    let completion = parked.proof_ready();
    let mut reposted = parked.pass(completion);
    assert!(parked.app.handoff_rejections.is_empty());
    assert_eq!(reposted.len(), 1, "the claim goes through the fence again");
    assert_eq!(COMMIT_STEPS.with(|steps| steps.take()), ["settle"]);
    let held = reposted.pop().expect("the held claim's pass");
    assert_eq!(parked.run_to_verdict(held), WRITTEN);
    assert_eq!(
        COMMIT_STEPS.with(|steps| steps.take()),
        ["settle", "write", "cut off", "taken back"]
    );
    SETTLE_WAITS.set(false);
}

/// The process registry's guard for `item`, as an item whose checks still hold takes it: at once,
/// or once a pending handoff on another thread is refused.
fn hold(
    item: crate::app_update_handoff_boundary::GuardedItem,
) -> crate::app_update_handoff_boundary::Held<'static> {
    crate::app_update_handoff_boundary::Boundary::process()
        .hold(item, || Ok::<(), ()>(()))
        .expect("its checks hold")
}

/// An item that cannot be cut in half and takes its boundary guard after the handoff's start (no
/// latch held it back here, as for one whose wait on the latch ran out, or one on the handoff's
/// own thread) refuses the Commit: it is refused as activity, so the handoff is tried again, and
/// nothing cuts the item in half. The try's wait is one budget across its start and its Commit:
/// the start that followed an item began it, and past it the Commit is refused for this try (the
/// updater's next check tries again with a new budget), never written over the item. Before, the
/// guard was read only as the handoff started and the Commit was written with a sign-out's clicks
/// under way; then, round 22, a Commit past the budget was written over the item, or held a fresh
/// budget of its own (design §6.7).
#[test]
fn a_guard_taken_after_the_start_check_holds_the_commit() {
    use crate::app_update_handoff_boundary::{GuardedItem, MAX_BOUNDARY_WAIT};

    // The guard is process-wide and every Commit reads it: alone, so no other test's sees it.
    if !crate::test_alone::alone() {
        return;
    }
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let refused_as_activity = |parked: &mut Parked| {
        let completion = parked.proof_ready();
        assert_eq!(
            parked.run_to_verdict(completion),
            "structural activity arrived before Commit",
            "the Commit was written with a sign-out's clicks holding the boundary guard"
        );
        assert!(
            parked
                .app
                .pending_update_handoff
                .as_ref()
                .is_some_and(|pending| pending.revoked_by_activity),
            "an activity refusal, which the handoff tries again"
        );
    };
    // The handoff started with every guard free; now a sign-out's click sequence begins.
    super::end_boundary_wait();
    let clicks = hold(GuardedItem::SignOutSequence);
    refused_as_activity(&mut Parked::new(b""));
    assert!(
        super::BOUNDARY_WAIT_SINCE
            .with(std::cell::Cell::get)
            .is_some(),
        "within the budget the try follows the clicks"
    );
    drop(clicks);
    // A try whose start followed a clone's publish a budget ago: the start found the rename in its
    // section and deferred, then found it done and went on, the try's wait standing.
    super::end_boundary_wait();
    let started = std::time::Instant::now()
        .checked_sub(MAX_BOUNDARY_WAIT)
        .expect("this Mac has been up ten seconds");
    let rename = hold(GuardedItem::ClonePublishRename);
    assert!(
        super::start_at_boundary(started).is_err(),
        "the start follows the rename"
    );
    drop(rename);
    drop(super::start_at_boundary(started).expect("the rename done, the start goes on"));
    // A sign-out's clicks take the guard after the start: the Commit, past the try's budget, is
    // refused for this try, never written over them and never given a fresh budget.
    let clicks = hold(GuardedItem::SignOutSequence);
    refused_as_activity(&mut Parked::new(b""));
    assert_eq!(
        super::BOUNDARY_WAIT_SINCE.with(std::cell::Cell::get),
        None,
        "past the budget the try ends; the next check starts afresh"
    );
    // The updater's next check: a new try, which follows the clicks still under way.
    refused_as_activity(&mut Parked::new(b""));
    // The clicks end: the Commit is written.
    drop(clicks);
    let mut parked = Parked::new(b"");
    let completion = parked.proof_ready();
    assert_eq!(parked.run_to_verdict(completion), WRITTEN);
    super::end_boundary_wait();
}

/// The handoff's boundary latch lasts while the handoff is in flight: an item on another thread
/// asks for its guard and waits, in no critical section, and the loop turn after the handoff
/// ends (refused, its records gone) releases the latch, so the item goes on (design §6.7).
#[test]
fn the_boundary_latch_holds_items_while_the_handoff_is_in_flight() {
    use crate::app_update_handoff_boundary::{
        GuardedItem, HANDOFF_PENDING_BOUND, handoff_pending, is_held, latch_handoff, waiting,
    };

    // The latch is process-wide: alone, so no other test's item waits on it.
    if !crate::test_alone::alone() {
        return;
    }
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut parked = Parked::new(b"");
    parked.app.update_boundary_latch = Some(latch_handoff(HANDOFF_PENDING_BOUND));
    parked.app.settle_update_boundary_latch();
    assert!(handoff_pending(), "in flight, the latch stays");
    let rename = std::thread::spawn(|| {
        let _guard = hold(GuardedItem::ClonePublishRename);
    });
    assert!(
        aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            std::time::Duration::from_millis(1),
            || (waiting() == 1).then_some(())
        )
        .is_some(),
        "the rename waits for the handoff"
    );
    assert!(!is_held(GuardedItem::ClonePublishRename));
    // The handoff is refused and its records go: the next loop turn releases the latch.
    parked.app.pending_update_handoff = None;
    parked.app.update_handoff_prelaunch = None;
    parked.app.settle_update_boundary_latch();
    assert!(!handoff_pending());
    rename.join().expect("the rename goes on");
    assert_eq!(waiting(), 0);
}

/// The handoff's start latches the boundary before it reads the guard: with every guard free it
/// keeps the latch for the handoff in flight, so an item that asks for its guard now waits; with
/// an item already in its section it is refused as activity and leaves no latch behind, so that
/// item and every other goes on (design §6.7).
#[test]
fn the_handoffs_start_latches_the_boundary_before_it_reads_the_guard() {
    use crate::app_update_handoff_boundary::{GuardedItem, handoff_pending};

    // The latch and the guard are process-wide: alone, so no other test's item or Commit sees
    // them.
    if !crate::test_alone::alone() {
        return;
    }
    let now = std::time::Instant::now();
    let latch = super::start_at_boundary(now).expect("every guard free: the handoff starts");
    assert!(handoff_pending(), "the start latched the boundary");
    drop(latch);
    assert!(!handoff_pending());
    let clicks = hold(GuardedItem::SignOutSequence);
    assert!(
        super::start_at_boundary(now).is_err(),
        "a sign-out's clicks under way hold the start"
    );
    assert!(!handoff_pending(), "a refused start leaves no latch");
    drop(clicks);
    super::end_boundary_wait();
}
