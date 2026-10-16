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

/// The paste body: far more than a tty input queue holds, so a program that
/// does not read leaves the writer parked mid-frame.
const BODY: usize = 64 * 1024;

/// DEC 2004: the program reads pastes bracketed.
const BRACKETED: &[u8] = b"\x1b[?2004h";

/// What a bracketed paste of [`BODY`] bytes puts on the wire, whole.
const FRAME: usize = BODY + b"\x1b[200~\x1b[201~".len();

/// A parked prelaunched attempt over session 0, whose sink is a real PTY
/// master with a raw slave nobody reads: the program behind a tab not in
/// front, its output undrained during the freeze.
struct Parked {
    app: crate::App,
    slave: i32,
    term: Arc<Mutex<aterm_core::terminal::Terminal>>,
    ctx: Arc<crate::SessionCtx>,
    nonce: String,
    reject: Option<std::sync::mpsc::SyncSender<()>>,
    /// What the park and the rejection send, held as the worker holds them.
    _channels: (
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::Receiver<super::HandoffStandDown>,
        std::sync::mpsc::Receiver<super::HandoffTransferJob>,
        std::sync::mpsc::Receiver<()>,
    ),
    /// Last, so the descriptors close after the `App` that wrote to them.
    _pty: Pty,
}

/// The PTY pair, closed on drop.
struct Pty(i32, i32);

impl Drop for Pty {
    fn drop(&mut self) {
        aterm_pty::close_fd(self.1);
        aterm_pty::close_fd(self.0);
    }
}

impl Parked {
    /// Parked after the engine processed `armed`, the modes the program set.
    fn new(armed: &[u8]) -> Self {
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
        let (record, cancelled, stood_down, transferred) =
            prelaunch_record(7, ApplyMode::Immediate);
        let nonce = record.nonce.clone();
        app.update_handoff_prelaunch = Some(record);
        let parked = app.park_and_transfer_to_prelaunched_successor(std::time::Instant::now());
        assert!(
            matches!(parked, ParkAttempt::Parked),
            "PRECONDITION: the park landed ({parked:?})"
        );
        app.handoff_reposts = Some(Vec::new());
        let (reject, rejected) = std::sync::mpsc::sync_channel(1);
        Self {
            app,
            slave,
            term,
            ctx,
            nonce,
            reject: Some(reject),
            _channels: (cancelled, stood_down, transferred, rejected),
            _pty: Pty(master, slave),
        }
    }

    /// The successor's proof, landed: the Commit's completion. Its pipe has no
    /// reader, so a Commit written fails as an EPIPE does and the test runs on.
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

    /// One pass of the Commit, the dispatch fence's loop time already had.
    /// Returns what the drain gate re-posted.
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

    /// Passes until the attempt has a verdict: a claim whose answers took a
    /// fence grain on a loaded host goes through the gate once more, and with
    /// nothing in flight it then reads the same admission.
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

    /// The program reads its input again: the whole frame, then nothing.
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

/// A control verb's paste into session 0, exactly as the hidden arm writes it
/// (`control::run_feed_bin_routed`), on its own thread; returned once its frame
/// is part-way into the kernel (`slave` has input, the writer still runs).
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

/// Whether `fd` has input, waiting at most `timeout_ms`.
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

/// A frame in flight holds the Commit: a control verb's 64 KiB paste parked mid-frame on a
/// program that is not reading is unsettled egress even with the spill empty, since `_exit`
/// would leave a bracketed paste open and every later key read as pasted text. The gate
/// re-posts, and once the frame is read whole the same attempt commits.
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
        !super::handoff_egress_settled(&parked.app.pool, &live),
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
    assert!(super::handoff_egress_settled(&parked.app.pool, &live));
    assert_eq!(
        parked.run_to_verdict(reposted.remove(0)),
        WRITTEN,
        "the frame whole, the Commit is written"
    );
}

/// A frame begun after the drain gate's last reading stands the Commit down: the Commit holds
/// every handed session's egress fence through its write, so the paste waits before its first
/// byte or, already part-way in, refuses the write as unsettled egress (a lossless retry), and
/// its frame is whole when the program reads again.
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
        verdict.starts_with("tolerated input was still on its way to a PTY at Commit"),
        "the egress fence refused the write: {verdict}"
    );
    assert_whole(&parked.read_frame(writer));
}

/// A reset on a parked session stands the Commit down: with the park's reset lane closed,
/// `aterm ctl reset` runs on this process's engine after the carry was taken, so committing
/// would bring the stuck modes back. The reset moves the carry serial, the Commit reads it
/// moved (structural activity, a lossless retry), and this engine runs on, reset.
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
            assert_eq!(
                verdict, "structural activity arrived before Commit",
                "the reset stands the Commit down"
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

/// A reset at the write is never answered `OK`: the Commit reads the serials once, under the
/// hold fence it keeps raised to its `_exit`, and a later reset would land on this engine alone
/// while the successor restores the carry from before it. So it waits on the fence (the update
/// lands with it unanswered, or stands down and answers it). Run at the write from the thread
/// holding the fence, it waits out its bound and is refused `ERR busy update-committing`.
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
