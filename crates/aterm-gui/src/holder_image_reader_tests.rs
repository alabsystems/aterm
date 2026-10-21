// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HOLDER IMAGE THROUGH THE REAL READER (card engine §3.4-§3.6, §7 "A,
//! identity and edges"): `attach_reader_inner`, the gather and parse pipeline
//! every session runs, on a real pseudo-terminal whose slave the test writes
//! as the shell and its programs. The foreground group (`fg_probe`) and each
//! group's image (`edge_image`) are scripted per master and per group, as
//! `cwd_report_reader_tests` scripts the probe; the parse stage's hold hook
//! (`holder_image::hold_hook`) records, from inside each term-lock hold, the
//! token the session's timeline held then.
//!
//! I1: every hold that parses a byte of image T finds T on the timeline. The
//! binds here are the sampled group edge (a launch typed at a prompt), the
//! byte-driven arm's `exec` in place after a signed C (at once, or after any
//! number of output batches), and its Launcher re-read (atpkg's shim
//! `exec`ing the store build), across a re-attached reader too. Each has a
//! negative control that must break it: publishing after the hold, the arm
//! off, the C cut off. The image published is the one the gather read at the
//! edge, and the publication's own wake is posted before the edge's hold.
//!
//! The kernel edges (§3.3, macOS's arm): with a scripted process watch
//! injected through `SessionFactory::proc_watch`, a launch's fork and `exec`s
//! posted to it are heard by the gather's own poll before any byte, each a
//! byte-free edge whose hold already finds its image published and, for the
//! drawer, the engine fence's span laid, before Claude Code's first byte is
//! read. Publishing after the hold breaks it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use aterm_core::terminal::{ShellIntegrationNonce, Terminal};
use atpkg::image::{ImageBuild, ImageSource, ProgramTag};

use aterm_uds::procwatch::{ProcEvent, ProcScript, ProcWatch};

use super::{attach_reader_inner, park_reader};
use crate::holder_image::{HolderImage, hold_hook};
use crate::{App, Wake, WindowId};

const KEY: [u8; 32] = [0x6A; 32];

/// A hang detector, never a latency budget.
const PATIENCE: Duration = aterm_spec::wait::MUST_HAPPEN;

/// Claude Code 2.1.304's store id.
const CLAUDE_304: u64 = 1_000_002_000_001_000_304;

/// The group each master's foreground probe names.
static FG: Mutex<Option<HashMap<i32, i32>>> = Mutex::new(None);
/// Each scripted group's images, read in order: a read takes the first
/// while more than one is queued, then keeps answering the last.
static IMAGES: Mutex<Option<HashMap<i32, Vec<HolderImage>>>> = Mutex::new(None);

/// A master's next foreground sample, armed by a test: run once in the
/// gather, at that sample, and its answer is the sample's.
type Armed = Box<dyn FnOnce() -> i32 + Send>;

static ARMED: Mutex<Option<HashMap<i32, Armed>>> = Mutex::new(None);

/// Run `act` at `master`'s next foreground sample, which answers what it
/// returns.
fn arm_probe(master: i32, act: impl FnOnce() -> i32 + Send + 'static) {
    ARMED
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(master, Box::new(act));
}

fn set_fg(master: i32, fg: i32) {
    FG.lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(master, fg);
}

fn scripted_fg(master: i32) -> i32 {
    let armed = ARMED
        .lock()
        .unwrap()
        .as_mut()
        .and_then(|m| m.remove(&master));
    if let Some(act) = armed {
        return act();
    }
    FG.lock()
        .unwrap()
        .as_ref()
        .and_then(|m| m.get(&master).copied())
        .unwrap_or(-1)
}

/// Each scripted shell's process watch, as the test posts to it.
static WATCHES: Mutex<Option<HashMap<i32, ProcScript>>> = Mutex::new(None);

/// `SessionFactory::proc_watch` for a rig that runs the kernel arm: a
/// scripted watch of `shell`, its script kept for the test.
fn scripted_watch(shell: i32) -> Option<ProcWatch> {
    let (watch, script) = ProcWatch::scripted(shell)?;
    WATCHES
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(shell, script);
    Some(watch)
}

/// How many times each scripted group's image was read.
static READS: Mutex<Option<HashMap<i32, usize>>> = Mutex::new(None);

fn reads(pgid: i32) -> usize {
    READS
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|m| m.get(&pgid).copied())
        .unwrap_or(0)
}

fn scripted_image(pgid: i32, _last: Option<&HolderImage>) -> HolderImage {
    *READS
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .entry(pgid)
        .or_default() += 1;
    let mut images = IMAGES.lock().unwrap();
    match images.as_mut().and_then(|m| m.get_mut(&pgid)) {
        Some(queue) if queue.len() > 1 => queue.remove(0),
        Some(queue) => queue
            .first()
            .cloned()
            .unwrap_or_else(|| HolderImage::unread(pgid)),
        None => HolderImage::unread(pgid),
    }
}

fn set_image(pgid: i32, image: HolderImage) {
    set_images(pgid, vec![image]);
}

/// `pgid` reads as each of `images` in turn, then as the last for good.
fn set_images(pgid: i32, images: Vec<HolderImage>) {
    IMAGES
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(pgid, images);
}

/// `pgid`'s image: `exe` started at `start`, classified as `build`.
fn image(pgid: i32, start: u64, exe: &str, build: ImageBuild) -> HolderImage {
    HolderImage::of(pgid, Some(start), Some(PathBuf::from(exe)), build, 1)
}

fn zsh(pgid: i32) -> HolderImage {
    image(
        pgid,
        1,
        "/bin/zsh",
        ImageBuild::unknown(ProgramTag::new("zsh")),
    )
}

fn store_claude(pgid: i32, start: u64) -> HolderImage {
    image(
        pgid,
        start,
        "/p/pkg/store/claude/1000002000001000304/bin/claude",
        ImageBuild {
            program: ProgramTag::new("claude"),
            build: CLAUDE_304,
            source: ImageSource::Store,
        },
    )
}

fn launcher(pgid: i32, start: u64) -> HolderImage {
    image(
        pgid,
        start,
        "/bin/sh",
        ImageBuild {
            program: ProgramTag::new("claude"),
            build: 0,
            source: ImageSource::Launcher,
        },
    )
}

fn signed(seq: &str) -> Vec<u8> {
    format!("\x1b]{seq};id={}\x07", ShellIntegrationNonce(KEY).to_hex()).into_bytes()
}

fn prompt() -> Vec<u8> {
    [signed("133;A"), b"$ ".to_vec(), signed("133;B")].concat()
}

/// Claude Code's first bytes of an inline launch, through its launch card:
/// 2.1.304's own (`fixtures/cc-2.1.304-inline-startup.bin`), captured in the
/// real-Claude lab with `ctl subscribe bytes` from the first byte after the
/// launcher's `clear` through the card's burst, the lab's project directory
/// redacted to `/private/tmp/at.XXXX/proj` (same length).
const CLAUDE_FIRST: &[u8] = include_bytes!("fixtures/cc-2.1.304-inline-startup.bin");

fn screen_has(t: &Terminal, needle: &str) -> bool {
    (0..usize::from(t.rows())).any(|r| t.row_text(r).is_some_and(|l| l.contains(needle)))
}

/// What a knob of the reader's wiring runs.
#[derive(Clone, Copy)]
struct Knobs {
    byte_arm: bool,
    publish_after_hold: bool,
    no_c_cut: bool,
    /// A process watch (scripted): the kernel edges, as on macOS.
    kernel: bool,
}

/// The reader with no process watch (Linux's, or macOS's after `EMFILE`).
const SHIPPING: Knobs = Knobs {
    byte_arm: true,
    publish_after_hold: false,
    no_c_cut: false,
    kernel: false,
};

/// The reader as macOS ships it: a process watch beside the byte arm.
const KERNEL: Knobs = Knobs {
    kernel: true,
    ..SHIPPING
};

/// A `Wake::Output` the reader posted: how many holds the session had
/// recorded by then, and the holder token its timeline held.
#[derive(Clone, Copy, Debug)]
struct PostedWake {
    holds: usize,
    token: Option<u64>,
}

struct Rig {
    id: u64,
    session: crate::Session,
    slave: i32,
    app: App,
    /// Every `Wake::Output` posted, in order.
    wakes: Arc<Mutex<Vec<PostedWake>>>,
}

impl Rig {
    fn attach(id: u64, shell: i32, knobs: Knobs) -> Self {
        let mut app = App::headless_for_test();
        app.session_factory.fg_probe = scripted_fg;
        app.session_factory.fg_gone = |_, _, _| false;
        app.session_factory.program_probe = |_, _| false;
        app.session_factory.edge_image = scripted_image;
        app.session_factory.byte_arm = knobs.byte_arm;
        if knobs.kernel {
            app.session_factory.proc_watch = scripted_watch;
        }
        let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
        // SAFETY: a zeroed termios is a valid out-slot for tcgetattr, then
        // cfmakeraw/tcsetattr on the test-owned slave fd.
        unsafe {
            let mut tio: libc::termios = std::mem::zeroed();
            assert_eq!(libc::tcgetattr(slave, &mut tio), 0, "tcgetattr");
            libc::cfmakeraw(&mut tio);
            assert_eq!(libc::tcsetattr(slave, libc::TCSANOW, &tio), 0, "tcsetattr");
        }
        FG.lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(master, shell);
        let mut session = crate::stub_session(id);
        session.master = master;
        session.pid = shell;
        {
            let mut t = session.term.lock().expect("terminal lock");
            t.authorize_shell_integration(KEY);
            t.set_require_shell_integration_nonce(true);
        }
        hold_hook::watch(
            id,
            hold_hook::Knobs {
                publish_after_hold: knobs.publish_after_hold,
                no_c_cut: knobs.no_c_cut,
            },
        );
        let mut rig = Self {
            id,
            session,
            slave,
            app,
            wakes: Arc::default(),
        };
        rig.start_reader();
        rig
    }

    /// Attach the real reader and wait for its gather. Each `Wake::Output`
    /// is recorded and handled at once (the latch cleared, as the UI's pass
    /// does), so every chunk's wake posts.
    fn start_reader(&mut self) {
        let (ready_tx, ready_rx) = mpsc::channel::<()>();
        let (id, wakes) = (self.id, Arc::clone(&self.wakes));
        let latch = Arc::clone(&self.session.output_wake_pending);
        let timeline = Arc::clone(&self.session.ctx.timeline);
        let post = Arc::new(move |wake: Wake| {
            match wake {
                Wake::Ready { .. } => {
                    let _ = ready_tx.send(());
                }
                Wake::Output { .. } => {
                    let token = timeline
                        .lock()
                        .unwrap()
                        .holder_image()
                        .and_then(|i| i.token);
                    wakes.lock().unwrap().push(PostedWake {
                        holds: hold_hook::holds_so_far(id),
                        token,
                    });
                    latch.store(0, Ordering::Relaxed);
                }
                _ => {}
            }
            true
        });
        attach_reader_inner(
            &mut self.session,
            WindowId(0),
            post,
            &self.app.session_factory,
            None,
        )
        .expect("attach the real reader");
        ready_rx
            .recv_timeout(PATIENCE)
            .expect("the reader's gather started");
    }

    /// Park the reader and attach a new one, as an overlap handoff does: the
    /// engine, the timeline and the foreground holder carry over.
    fn reattach(&mut self) {
        assert!(
            park_reader(&mut self.session, Instant::now() + PATIENCE),
            "parked"
        );
        self.start_reader();
    }

    /// Write `bytes` then `marker` as the holder `fg`, and wait until the
    /// engine has parsed the marker. A CAN between them ends any sequence
    /// `bytes` left open, so the marker prints.
    fn write(&self, fg: i32, bytes: &[u8], marker: &str) {
        set_fg(self.session.master, fg);
        let all = [bytes, b"\x18", marker.as_bytes()].concat();
        let mut sent = 0;
        while sent < all.len() {
            // SAFETY: a bounded write of a live slice to the test-owned slave.
            let wrote =
                unsafe { libc::write(self.slave, all[sent..].as_ptr().cast(), all.len() - sent) };
            assert!(wrote > 0, "write {marker}");
            sent += wrote.cast_unsigned();
        }
        let seen = aterm_spec::wait::within(PATIENCE, Duration::from_millis(1), || {
            screen_has(&self.session.term.lock().expect("terminal lock"), marker).then_some(())
        });
        assert!(seen.is_some(), "timed out waiting for {marker}");
    }

    /// The shell's process watch hears `events`, while `fg` holds the
    /// terminal; returns once the timeline holds `published`.
    fn hear(&self, fg: i32, events: &[ProcEvent], published: Option<u64>) {
        set_fg(self.session.master, fg);
        let script = self.script();
        script.post(events);
        let seen = aterm_spec::wait::within(PATIENCE, Duration::from_millis(1), || {
            (self.holder_token() == published).then_some(())
        });
        assert!(seen.is_some(), "timed out waiting for {published:?}");
        assert_eq!(script.pending(), 0, "the gather drained what was posted");
    }

    /// The script of the process watch this rig's reader built.
    fn script(&self) -> ProcScript {
        WATCHES
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|m| m.get(&self.session.pid).cloned())
            .expect("the reader built its process watch")
    }

    fn holder_token(&self) -> Option<u64> {
        self.session
            .ctx
            .timeline
            .lock()
            .unwrap()
            .holder_image()
            .and_then(|i| i.token)
    }

    fn rows(&self, kind: &str) -> usize {
        self.session
            .ctx
            .timeline
            .lock()
            .unwrap()
            .since(None)
            .filter(|e| e.kind == kind)
            .count()
    }

    /// Stop the reader; the holds the hook recorded.
    fn finish(mut self) -> Vec<hold_hook::Hold> {
        let _ = park_reader(&mut self.session, Instant::now() + PATIENCE);
        hold_hook::unwatch(self.id)
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = park_reader(&mut self.session, Instant::now() + PATIENCE);
        hold_hook::unwatch(self.id);
        if let Some(m) = WATCHES.lock().unwrap().as_mut() {
            m.remove(&self.session.pid);
        }
        let master = std::mem::replace(&mut self.session.master, -1);
        if let Some(m) = FG.lock().unwrap().as_mut() {
            m.remove(&master);
        }
        aterm_pty::close_fd(self.slave);
        aterm_pty::close_fd(master);
    }
}

/// The marker written after each image's bytes in these binds.
const CARD_END: &str = "<card>";

/// I1 over the holds: every hold that parses a byte of image `token` — from
/// the first that parses any of `first` up to the one that parses the
/// [`CARD_END`] marker written after them — found `token` on the timeline.
fn i1_violations(holds: &[hold_hook::Hold], first: &[u8], token: u64) -> Vec<String> {
    let has = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).any(|w| w == needle);
    let probe = &first[..first.len().min(16)];
    let mut found = false;
    let mut violations = Vec::new();
    for hold in holds {
        found |= has(&hold.bytes, probe);
        if !found || hold.bytes.is_empty() {
            continue;
        }
        if hold.token != Some(token) {
            violations.push(format!(
                "hold at {} ({} bytes) found {:?}, not {token:016x}",
                hold.off,
                hold.bytes.len(),
                hold.token.map(|t| format!("{t:016x}"))
            ));
        }
        if has(&hold.bytes, CARD_END.as_bytes()) {
            return violations;
        }
    }
    violations.push("no hold parsed the image's bytes through its end".into());
    violations
}

/// The index of the first hold that parsed any of `first`.
fn first_hold_of(holds: &[hold_hook::Hold], first: &[u8]) -> Option<usize> {
    let probe = &first[..first.len().min(16)];
    holds
        .iter()
        .position(|h| h.bytes.windows(probe.len()).any(|w| w == probe))
}

/// A launch typed at a signing shell's prompt: the shell runs `claude`, the
/// job takes the terminal and writes its launch card. Returns the holds, the
/// job image's token, and the rig (still attached).
fn launch(id: u64, shell: i32, job: i32, knobs: Knobs) -> (Rig, u64) {
    set_image(shell, zsh(shell));
    let claude = store_claude(job, 7_000 + u64::from(job.cast_unsigned()));
    let token = claude.token.expect("a tracked image");
    set_image(job, claude);
    let rig = Rig::attach(id, shell, knobs);
    rig.write(shell, &prompt(), "<p1>");
    rig.write(
        shell,
        &[&b"claude\r\n"[..], &signed("133;C")].concat(),
        "<typed>",
    );
    rig.write(job, CLAUDE_FIRST, CARD_END);
    (rig, token)
}

/// THE BUILD BEFORE THE BYTES (I1), through the real reader: the gather reads
/// the job's image at the sampled edge, and the parse stage publishes it
/// before the hold of the slice that starts at the cut, so the hold that
/// parses Claude Code's first byte already finds its token. Publication also
/// queues the session in the App's edged inbox and records the dev rows; an
/// edge between two images no card cares about publishes nothing.
#[test]
fn the_holder_image_is_published_before_the_hold_of_its_first_byte() {
    let (rig, token) = launch(9301, 5100, 5200, SHIPPING);
    assert_eq!(rig.holder_token(), Some(token));
    assert_eq!(
        rig.app.session_factory.edge_inbox.take(),
        vec![9301],
        "the edge queued its session once"
    );
    assert_eq!(rig.rows("holder-edge"), 1);
    assert_eq!(rig.rows("holder-image"), 1);
    // The job exits and the shell takes the terminal back: leaving a drawer
    // publishes the shell's image; a later `ls` edge publishes nothing.
    rig.write(5100, &prompt(), "<p2>");
    assert_eq!(rig.holder_token(), zsh(5100).token);
    set_image(
        5250,
        image(
            5250,
            3,
            "/bin/ls",
            ImageBuild::unknown(ProgramTag::new("ls")),
        ),
    );
    rig.write(5250, b"file\r\n", "<ls>");
    rig.write(5100, &prompt(), "<p3>");
    assert_eq!(rig.holder_token(), zsh(5100).token, "no card edge");
    assert_eq!(rig.rows("holder-image"), 2, "entering and leaving only");
    let holds = rig.finish();
    assert_eq!(
        i1_violations(&holds, CLAUDE_FIRST, token),
        Vec::<String>::new()
    );
}

/// NEGATIVE CONTROL for I1: the mutant that publishes after the hold (the
/// same publication, one hold late) leaves Claude Code's first bytes parsed
/// under no image, and the bind catches it.
#[test]
fn control_publishing_after_the_hold_breaks_i1() {
    let knobs = Knobs {
        publish_after_hold: true,
        ..SHIPPING
    };
    let (rig, token) = launch(9302, 5300, 5400, knobs);
    assert_eq!(rig.holder_token(), Some(token), "published, only late");
    let holds = rig.finish();
    assert!(
        !i1_violations(&holds, CLAUDE_FIRST, token).is_empty(),
        "the late publication must be caught"
    );
}

/// `exec claude` at a signing shell's prompt: the shell's own image changes
/// in place, and no foreground group moves. Returns whether the hold that
/// parsed the new image's first bytes found its token.
fn exec_in_place(id: u64, shell: i32, knobs: Knobs) -> (Vec<String>, Option<u64>, Rig) {
    set_image(shell, zsh(shell));
    let rig = Rig::attach(id, shell, knobs);
    rig.write(shell, &prompt(), "<p1>");
    // The `exec` happened by the time its first bytes are read.
    let claude = store_claude(shell, 9_000 + u64::from(shell.cast_unsigned()));
    let token = claude.token.expect("a tracked image");
    set_image(shell, claude);
    rig.write(
        shell,
        &[&b"exec claude\r\n"[..], &signed("133;C"), CLAUDE_FIRST].concat(),
        CARD_END,
    );
    let published = rig.holder_token();
    let holds = hold_hook::unwatch(id);
    (i1_violations(&holds, CLAUDE_FIRST, token), published, rig)
}

/// THE BYTE-DRIVEN ARM, `exec` in place (§3.6): the slice ends right after
/// the signed C, the next slice re-reads the shell's image before its hold,
/// and the change is an Exec edge at that slice's start, so the hold that
/// parses Claude Code's first byte finds its token.
#[test]
fn an_exec_after_a_signed_c_is_an_exec_edge_before_its_first_byte() {
    let (violations, published, rig) = exec_in_place(9303, 5500, SHIPPING);
    assert_eq!(violations, Vec::<String>::new());
    assert_eq!(published, store_claude(5500, 9_000 + 5500).token);
    let exec_rows = rig
        .session
        .ctx
        .timeline
        .lock()
        .unwrap()
        .since(None)
        .filter(|e| e.kind == "holder-edge" && e.payload.starts_with("kind=exec"))
        .count();
    assert_eq!(exec_rows, 1, "one Exec edge");
}

/// NEGATIVE CONTROLS for the arm: with it off nothing sees the `exec`; with
/// it on but no cut after the C, the C and Claude Code's first bytes share a
/// hold, and the re-read comes one hold late.
#[test]
fn control_without_the_arm_or_its_cut_the_exec_is_missed() {
    let off = Knobs {
        byte_arm: false,
        ..SHIPPING
    };
    let (violations, published, _rig) = exec_in_place(9304, 5600, off);
    assert!(!violations.is_empty(), "no arm: the exec is never seen");
    assert_eq!(published, None);
    let uncut = Knobs {
        no_c_cut: true,
        ..SHIPPING
    };
    let (violations, published, _rig) = exec_in_place(9305, 5700, uncut);
    assert!(
        !violations.is_empty(),
        "no cut: the re-read is one hold late"
    );
    // Late, not never: when the PTY hands the write over in more than one
    // read, the next batch's first slice still re-reads the image.
    assert!(
        published.is_none() || published == store_claude(5700, 9_000 + 5700).token,
        "{published:?}"
    );
}

/// THE ARM'S FIRST READ OF A GROUP IS A GROUP EDGE (§3.6, §3.8): a holder the
/// reader finds already in front (`aterm -e claude`, a restored or handed
/// over reader) is published as found, a Group edge as the arm's cut is, so
/// a registry row that came for its group before its image stays that
/// image's (`SessionTimeline::offer_holder_image`). CONTROL
/// (`an_exec_after_a_signed_c_is_an_exec_edge_before_its_first_byte`): a
/// re-read that finds a new image of a group the arm read before is an Exec
/// edge. Mutation-checked: an arm that calls every read an Exec fails this.
#[test]
fn the_arms_first_read_of_a_group_is_a_group_edge() {
    let shell = 7900;
    let claude = store_claude(shell, 9_100);
    set_image(shell, claude.clone());
    let rig = Rig::attach(9319, shell, SHIPPING);
    rig.write(shell, CLAUDE_FIRST, CARD_END);
    assert_eq!(rig.holder_token(), claude.token);
    let edges: Vec<String> = rig
        .session
        .ctx
        .timeline
        .lock()
        .unwrap()
        .since(None)
        .filter(|e| e.kind == "holder-edge")
        .map(|e| e.payload.to_string())
        .collect();
    assert_eq!(edges.len(), 1, "{edges:?}");
    assert!(edges[0].starts_with("kind=group"), "{edges:?}");
}

/// THE LAUNCHER RE-READ (§3.6): the sampled edge finds atpkg's shim before
/// its `exec` (a Launcher, published); the shim `exec`s the store build in
/// place, and the next batch's first slice re-reads it before its hold.
/// Returns the I1 violations for the store image's first bytes.
fn shim_then_store(id: u64, shell: i32, job: i32, knobs: Knobs) -> Vec<String> {
    set_image(shell, zsh(shell));
    set_image(job, launcher(job, 11));
    let rig = Rig::attach(id, shell, knobs);
    rig.write(shell, &prompt(), "<p1>");
    rig.write(job, b"\x1b[0m", "<shim>");
    assert_eq!(rig.holder_token(), launcher(job, 11).token, "the launcher");
    let store = store_claude(job, 11);
    let token = store.token.expect("a tracked image");
    set_image(job, store);
    rig.write(job, CLAUDE_FIRST, CARD_END);
    let holds = rig.finish();
    i1_violations(&holds, CLAUDE_FIRST, token)
}

#[test]
fn a_launchers_exec_is_read_before_the_next_batch() {
    assert_eq!(
        shim_then_store(9306, 5800, 5900, SHIPPING),
        Vec::<String>::new()
    );
}

/// NEGATIVE CONTROL: without the arm the store build's bytes are parsed
/// under the launcher's token.
#[test]
fn control_without_the_arm_a_launchers_exec_is_missed() {
    let off = Knobs {
        byte_arm: false,
        ..SHIPPING
    };
    assert!(!shim_then_store(9307, 6000, 6100, off).is_empty());
}

/// C2, THE READ AT THE EDGE: the image the gather read at the sampled edge
/// rides the batch to the cut, so a parse stage that reaches the cut after
/// the image changed (here: every later read of the job answers another
/// build) still publishes the image of the edge. The mutant that re-reads at
/// the cut publishes the later build and breaks the bind.
#[test]
fn the_image_published_is_the_one_read_at_the_edge() {
    let (shell, job) = (6200, 6300);
    set_image(shell, zsh(shell));
    let at_edge = store_claude(job, 13);
    let token = at_edge.token.expect("a tracked image");
    let mut later = store_claude(job, 13);
    later.build.build = 1_000_002_000_001_000_303;
    later.token = Some(crate::holder_image::token_of(
        job,
        13,
        &later.exe,
        &later.build,
    ));
    assert_ne!(later.token, Some(token), "PRECONDITION: two images");
    set_images(job, vec![at_edge, later]);
    let rig = Rig::attach(9308, shell, SHIPPING);
    rig.write(shell, &prompt(), "<p1>");
    rig.write(
        shell,
        &[&b"claude\r\n"[..], &signed("133;C")].concat(),
        "<typed>",
    );
    rig.write(job, CLAUDE_FIRST, CARD_END);
    assert_eq!(rig.holder_token(), Some(token), "the edge's image stands");
    let holds = rig.finish();
    assert_eq!(
        i1_violations(&holds, CLAUDE_FIRST, token),
        Vec::<String>::new()
    );
}

/// THE PUBLICATION'S WAKE: a published edge posts the coalesced
/// `Wake::Output` itself, before the hold of the slice at its cut (the status
/// sweep then names the program on that wake, §3.5), not only at the batch's
/// end after every hold.
#[test]
fn the_publication_posts_its_wake_before_the_edges_hold() {
    let (rig, token) = launch(9309, 6400, 6500, SHIPPING);
    let wakes = rig.wakes.lock().unwrap().clone();
    let holds = rig.finish();
    let cut = first_hold_of(&holds, CLAUDE_FIRST).expect("the job's bytes were parsed");
    assert!(
        wakes
            .iter()
            .any(|w| w.token == Some(token) && w.holds <= cut),
        "a wake under the job's token before hold {cut}: {wakes:?}"
    );
}

/// THE ARM HAS NO EXPIRY: `git pull && exec claude`, a shell function that
/// prints first: the signed C's shell stays unresolved through any number of
/// output batches, each re-read once, so the `exec` after them is still read
/// before Claude Code's first byte.
#[test]
fn an_exec_after_many_output_batches_is_read_before_its_bytes() {
    let shell = 6600;
    set_image(shell, zsh(shell));
    let rig = Rig::attach(9310, shell, SHIPPING);
    rig.write(shell, &prompt(), "<p1>");
    rig.write(
        shell,
        &[&b"git pull && exec claude\r\n"[..], &signed("133;C")].concat(),
        "<typed>",
    );
    for line in 0..8 {
        rig.write(
            shell,
            format!("remote: object {line}\r\n").as_bytes(),
            &format!("<o{line}>"),
        );
    }
    let claude = store_claude(shell, 9_000 + 6600);
    let token = claude.token.expect("a tracked image");
    set_image(shell, claude);
    rig.write(shell, CLAUDE_FIRST, CARD_END);
    let holds = rig.finish();
    assert_eq!(
        i1_violations(&holds, CLAUDE_FIRST, token),
        Vec::<String>::new()
    );
}

/// THE SEEDED ARM, a command in flight: a reader re-attached while the
/// engine carries an accepted, executing C (its C predates the reader)
/// keeps the shell unresolved, so an `exec` after the re-attach and some
/// output is still read before its first byte.
#[test]
fn a_reattached_reader_keeps_watching_a_command_in_flight() {
    let shell = 6700;
    set_image(shell, zsh(shell));
    let mut rig = Rig::attach(9311, shell, SHIPPING);
    rig.write(shell, &prompt(), "<p1>");
    rig.write(
        shell,
        &[&b"sleep 1; exec claude\r\n"[..], &signed("133;C")].concat(),
        "<typed>",
    );
    rig.reattach();
    for line in 0..3 {
        rig.write(
            shell,
            format!("line {line}\r\n").as_bytes(),
            &format!("<r{line}>"),
        );
    }
    let claude = store_claude(shell, 9_000 + 6700);
    let token = claude.token.expect("a tracked image");
    set_image(shell, claude);
    rig.write(shell, CLAUDE_FIRST, CARD_END);
    let holds = rig.finish();
    assert_eq!(
        i1_violations(&holds, CLAUDE_FIRST, token),
        Vec::<String>::new()
    );
}

/// THE SEEDED ARM, a Launcher on the timeline: a reader re-attached while
/// atpkg's shim holds the terminal re-reads it each batch until a resolved
/// image replaces it, even when its first read finds nothing (the process
/// unreadable that instant), so the store build the shim `exec`s is read
/// before its first byte.
#[test]
fn a_reattached_reader_keeps_rereading_a_published_launcher() {
    let (shell, job) = (6800, 6900);
    set_image(shell, zsh(shell));
    set_image(job, launcher(job, 17));
    let mut rig = Rig::attach(9312, shell, SHIPPING);
    rig.write(shell, &prompt(), "<p1>");
    rig.write(job, b"\x1b[0m", "<shim>");
    assert_eq!(rig.holder_token(), launcher(job, 17).token, "the launcher");
    let store = store_claude(job, 17);
    let token = store.token.expect("a tracked image");
    set_images(job, vec![HolderImage::unread(job), store]);
    rig.reattach();
    rig.write(job, b"\x1b[0m", "<unread>");
    rig.write(job, CLAUDE_FIRST, CARD_END);
    let holds = rig.finish();
    assert_eq!(
        i1_violations(&holds, CLAUDE_FIRST, token),
        Vec::<String>::new()
    );
}

/// A launch heard by the process watch: the shell forks the job and gives it
/// the terminal, the job `exec`s atpkg's shim (a Group edge, read at the
/// event), and the shim `exec`s the store build in place (an Exec edge), each
/// heard before any byte and sent byte-free; then Claude Code writes its
/// launch card. Returns the holds and the store image's token.
fn kernel_launch(id: u64, shell: i32, job: i32, knobs: Knobs) -> (Vec<hold_hook::Hold>, u64) {
    set_image(shell, zsh(shell));
    let rig = Rig::attach(id, shell, knobs);
    rig.write(shell, &prompt(), "<p1>");
    rig.write(
        shell,
        &[&b"claude\r\n"[..], &signed("133;C")].concat(),
        "<typed>",
    );
    set_image(job, launcher(job, 23));
    let exec = ProcEvent::Exec {
        pid: job,
        pgid: job,
        late: false,
    };
    rig.hear(
        job,
        &[ProcEvent::Fork { child: job }, exec],
        launcher(job, 23).token,
    );
    let store = store_claude(job, 23);
    let token = store.token.expect("a tracked image");
    set_image(job, store);
    rig.hear(job, &[exec], Some(token));
    rig.write(job, CLAUDE_FIRST, CARD_END);
    (rig.finish(), token)
}

/// THE KERNEL EDGE BEFORE THE BYTES (I1, §3.3-§3.4): the shim's `exec` of the
/// store build is a byte-free Exec edge whose own hold already finds the
/// store image on the timeline and lays the engine fence's span for it, so
/// the hold that parses Claude Code's first byte finds both, and every hold
/// of its card does.
#[test]
fn a_kernel_exec_edge_publishes_and_lays_its_span_before_the_first_byte() {
    let (holds, token) = kernel_launch(9313, 7000, 7100, KERNEL);
    let lay = holds
        .iter()
        .position(|h| h.fence == Some(token))
        .expect("the fence laid the drawer's span");
    assert!(holds[lay].bytes.is_empty(), "laid by the byte-free edge");
    assert_eq!(
        holds[lay].token,
        Some(token),
        "the edge's hold found its image published"
    );
    let first = first_hold_of(&holds, CLAUDE_FIRST).expect("Claude Code's bytes were parsed");
    assert!(lay < first, "the span was laid before the first byte");
    assert_eq!(
        (holds[first].token, holds[first].fence),
        (Some(token), Some(token))
    );
    assert_eq!(
        i1_violations(&holds, CLAUDE_FIRST, token),
        Vec::<String>::new()
    );
}

/// NEGATIVE CONTROL: the same publication one hold late leaves the byte-free
/// edge's hold under the launcher's image.
#[test]
fn control_a_kernel_edge_published_after_its_hold_is_caught() {
    let knobs = Knobs {
        publish_after_hold: true,
        ..KERNEL
    };
    let (holds, token) = kernel_launch(9314, 7200, 7300, knobs);
    let lay = holds
        .iter()
        .position(|h| h.fence == Some(token))
        .expect("the fence laid the drawer's span");
    assert_ne!(
        holds[lay].token,
        Some(token),
        "the late publication must be caught"
    );
}

/// The bytes `unowned` were parsed with no fence mark, and the drawer `token`'s
/// span was laid right after them, in a hold of its own that already found the
/// image published, before Claude Code's first byte; every hold of its card
/// found both.
fn assert_laid_past(holds: &[hold_hook::Hold], unowned: &[u8], token: u64) {
    let has = |h: &hold_hook::Hold| h.bytes.windows(unowned.len()).any(|w| w == unowned);
    let between: Vec<&hold_hook::Hold> = holds.iter().filter(|h| has(h)).collect();
    assert!(!between.is_empty(), "the batch's bytes were parsed");
    assert!(
        between.iter().all(|h| h.fence.is_none()),
        "no holder's bytes: {between:?}"
    );
    let lay = holds
        .iter()
        .position(|h| h.fence == Some(token))
        .expect("the drawer's span was laid");
    assert!(
        holds[lay].bytes.is_empty() && holds[lay].off > 0 && lay > 0 && has(&holds[lay - 1]),
        "laid in a hold of its own, right after the unowned bytes: {:?}",
        &holds[lay.saturating_sub(1)..=lay]
    );
    assert_eq!(holds[lay].token, Some(token), "published before the lay");
    let first = first_hold_of(holds, CLAUDE_FIRST).expect("Claude Code's bytes were parsed");
    assert!(lay < first, "laid before Claude Code's first byte");
    assert_eq!(holds[first].fence, Some(token));
    assert_eq!(
        i1_violations(holds, CLAUDE_FIRST, token),
        Vec::<String>::new()
    );
}

/// THE TWO OFFSETS THROUGH THE READER (§3.3 as built): an `exec` heard after
/// bytes of its batch were read cuts where no `exec` can have preceded (here
/// the batch's start: no poll proved a later offset to the cutter) and lays
/// the new drawer's span where it was heard. The bytes between are no
/// holder's: the cut opens no mark, and the lay opens the drawer's before the
/// next byte.
#[test]
fn an_exec_heard_inside_a_batch_lays_the_drawer_where_it_was_heard() {
    const ID: u64 = 9315;
    const SHELL: i32 = 7400;
    set_image(SHELL, zsh(SHELL));
    let rig = Rig::attach(ID, SHELL, KERNEL);
    rig.write(SHELL, &prompt(), "<p1>");
    let store = store_claude(SHELL, 1);
    let token = store.token.expect("a tracked image");
    let script = rig.script();
    let posted = script.clone();
    // The shell `exec`s Claude Code in place after the batch's first read.
    hold_hook::on_next_read(ID, move || {
        set_image(SHELL, store);
        posted.post(&[ProcEvent::Exec {
            pid: SHELL,
            pgid: SHELL,
            late: false,
        }]);
    });
    rig.write(SHELL, b"<unowned>", "<u>");
    assert_eq!(script.pending(), 0, "the batch heard the exec");
    rig.write(SHELL, CLAUDE_FIRST, CARD_END);
    assert_laid_past(&rig.finish(), b"<unowned>", token);
}

/// THE ABSORBED `exec` (§3.3 as built): a sampled edge whose new leader's
/// `exec` is already queued takes that `exec` as its own, read with the edge;
/// as it may have written anywhere after the cut, the drawer's span is laid at
/// the batch's end, and the batch's bytes after the cut are no holder's.
#[test]
fn a_sampled_edge_that_absorbs_its_leaders_exec_lays_at_the_batch_end() {
    const ID: u64 = 9316;
    const SHELL: i32 = 7500;
    const JOB: i32 = 7600;
    set_image(SHELL, zsh(SHELL));
    let rig = Rig::attach(ID, SHELL, KERNEL);
    rig.write(SHELL, &prompt(), "<p1>");
    let store = store_claude(JOB, 5);
    let token = store.token.expect("a tracked image");
    set_image(JOB, store);
    let script = rig.script();
    let posted = script.clone();
    let master = rig.session.master;
    // At the batch-end sample: the job holds the terminal and has `exec`ed.
    arm_probe(master, move || {
        set_fg(master, JOB);
        posted.post(&[ProcEvent::Exec {
            pid: JOB,
            pgid: JOB,
            late: false,
        }]);
        JOB
    });
    rig.write(SHELL, b"<unowned>", "<u>");
    assert_eq!(script.pending(), 0, "the edge absorbed the exec");
    assert!(
        script.follows().contains(&Some(JOB)),
        "the new leader is watched: {:?}",
        script.follows()
    );
    rig.write(JOB, CLAUDE_FIRST, CARD_END);
    assert_laid_past(&rig.finish(), b"<unowned>", token);
}

/// K11 (§3.6): beside a process watch the byte arm neither scans a batch for
/// signed C ends nor re-reads the holder after one, since an `exec` in place
/// is the watch's to hear. With none posted, the `exec claude` a watchless
/// reader finds by its re-read
/// (`an_exec_after_a_signed_c_is_an_exec_edge_before_its_first_byte`) is read
/// nowhere and publishes nothing, and its C ends no slice.
#[test]
fn a_watch_turns_the_byte_arms_c_cuts_and_rereads_off() {
    for (id, shell, knobs) in [(9317, 7700, KERNEL), (9318, 7800, SHIPPING)] {
        set_image(shell, zsh(shell));
        let rig = Rig::attach(id, shell, knobs);
        rig.write(shell, &prompt(), "<p1>");
        let before = reads(shell);
        set_image(shell, store_claude(shell, 3));
        let c = signed("133;C");
        rig.write(
            shell,
            &[&b"exec claude\r\n"[..], &c, CLAUDE_FIRST].concat(),
            CARD_END,
        );
        let reread = reads(shell) > before;
        let published = rig.holder_token().is_some();
        let cut = rig.finish().iter().any(|h| h.bytes.ends_with(&c));
        let arm = !knobs.kernel;
        assert_eq!(
            (reread, published, cut),
            (arm, arm, arm),
            "the arm runs in full only without a watch (watch: {})",
            knobs.kernel
        );
    }
}
