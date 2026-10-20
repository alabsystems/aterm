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
//! byte-driven arm's `exec` in place after a signed C, and its Launcher re-read
//! (atpkg's shim `exec`ing the store build). Each has a negative control that
//! must break it: publishing after the hold, the arm off, the C cut off.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use aterm_core::terminal::{ShellIntegrationNonce, Terminal};
use atpkg::image::{ImageBuild, ImageSource, ProgramTag};

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
/// Each scripted group's image.
static IMAGES: Mutex<Option<HashMap<i32, HolderImage>>> = Mutex::new(None);

fn scripted_fg(master: i32) -> i32 {
    FG.lock()
        .unwrap()
        .as_ref()
        .and_then(|m| m.get(&master).copied())
        .unwrap_or(-1)
}

fn scripted_image(pgid: i32) -> HolderImage {
    IMAGES
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|m| m.get(&pgid).cloned())
        .unwrap_or_else(|| HolderImage::unread(pgid))
}

fn set_image(pgid: i32, image: HolderImage) {
    IMAGES
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(pgid, image);
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
}

const SHIPPING: Knobs = Knobs {
    byte_arm: true,
    publish_after_hold: false,
    no_c_cut: false,
};

struct Rig {
    id: u64,
    session: crate::Session,
    slave: i32,
    app: App,
}

impl Rig {
    fn attach(id: u64, shell: i32, knobs: Knobs) -> Self {
        let mut app = App::headless_for_test();
        app.session_factory.fg_probe = scripted_fg;
        app.session_factory.fg_gone = |_, _, _| false;
        app.session_factory.program_probe = |_, _| false;
        app.session_factory.edge_image = scripted_image;
        app.session_factory.byte_arm = knobs.byte_arm;
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
        let (ready_tx, ready_rx) = mpsc::channel::<()>();
        let post = Arc::new(move |wake: Wake| {
            if matches!(wake, Wake::Ready { .. }) {
                let _ = ready_tx.send(());
            }
            true
        });
        attach_reader_inner(&mut session, WindowId(0), post, &app.session_factory, None)
            .expect("attach the real reader");
        ready_rx
            .recv_timeout(PATIENCE)
            .expect("the reader's gather started");
        Self {
            id,
            session,
            slave,
            app,
        }
    }

    /// Write `bytes` then `marker` as the holder `fg`, and wait until the
    /// engine has parsed the marker. A CAN between them ends any sequence
    /// `bytes` left open, so the marker prints.
    fn write(&self, fg: i32, bytes: &[u8], marker: &str) {
        FG.lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(self.session.master, fg);
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
