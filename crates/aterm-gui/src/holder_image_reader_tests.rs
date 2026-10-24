// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Holder image timeline publication tests across the gather and parse pipeline.

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

const PATIENCE: Duration = aterm_spec::wait::MUST_HAPPEN;

const CLAUDE_304: u64 = 1_000_002_000_001_000_304;

static FG: Mutex<Option<HashMap<i32, i32>>> = Mutex::new(None);
static IMAGES: Mutex<Option<HashMap<i32, Vec<HolderImage>>>> = Mutex::new(None);

type Armed = Box<dyn FnOnce() -> i32 + Send>;

static ARMED: Mutex<Option<HashMap<i32, Armed>>> = Mutex::new(None);

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

static WATCHES: Mutex<Option<HashMap<i32, ProcScript>>> = Mutex::new(None);

fn scripted_watch(shell: i32) -> Option<ProcWatch> {
    let (watch, script) = ProcWatch::scripted(shell)?;
    WATCHES
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(shell, script);
    Some(watch)
}

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

fn set_images(pgid: i32, images: Vec<HolderImage>) {
    IMAGES
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(pgid, images);
}

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
    no_retire: bool,
}

const SHIPPING: Knobs = Knobs {
    byte_arm: true,
    publish_after_hold: false,
    no_c_cut: false,
    kernel: false,
    no_retire: false,
};

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
        // SAFETY: A zeroed termios is a valid out-slot for tcgetattr, then cfmakeraw/tcsetattr on the test-owned slave fd.
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
                no_retire: knobs.no_retire,
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

    fn reattach(&mut self) {
        assert!(
            park_reader(&mut self.session, Instant::now() + PATIENCE),
            "parked"
        );
        self.start_reader();
    }

    fn write(&self, fg: i32, bytes: &[u8], marker: &str) {
        set_fg(self.session.master, fg);
        let all = [bytes, b"\x18", marker.as_bytes()].concat();
        let mut sent = 0;
        while sent < all.len() {
            // SAFETY: Bounded write of a live slice to the test-owned slave.
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

    /// The watch hears `pid` exit, and the gather drains it: the next write's batch follows the
    /// exit the gather forwarded, so the parse stage has handled it once that write shows.
    fn die(&self, pid: i32) {
        let script = self.script();
        script.post(&[ProcEvent::Exit { pid }]);
        let drained = aterm_spec::wait::within(PATIENCE, Duration::from_millis(1), || {
            (script.pending() == 0).then_some(())
        });
        assert!(
            drained.is_some(),
            "timed out waiting for {pid}'s exit to drain"
        );
    }

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

const CARD_END: &str = "<card>";

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

fn first_hold_of(holds: &[hold_hook::Hold], first: &[u8]) -> Option<usize> {
    let probe = &first[..first.len().min(16)];
    holds
        .iter()
        .position(|h| h.bytes.windows(probe.len()).any(|w| w == probe))
}

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

fn exec_in_place(id: u64, shell: i32, knobs: Knobs) -> (Vec<String>, Option<u64>, Rig) {
    set_image(shell, zsh(shell));
    let rig = Rig::attach(id, shell, knobs);
    rig.write(shell, &prompt(), "<p1>");
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
    assert!(
        published.is_none() || published == store_claude(5700, 9_000 + 5700).token,
        "{published:?}"
    );
}

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

#[test]
fn control_without_the_arm_a_launchers_exec_is_missed() {
    let off = Knobs {
        byte_arm: false,
        ..SHIPPING
    };
    assert!(!shim_then_store(9307, 6000, 6100, off).is_empty());
}

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

/// Card engine §3.7, marks retired by liveness: drawer 0 stops (Ctrl-Z) and lives; drawers 1-7
/// launch, stop, and die, each exit heard by the process watch; then drawer 8 launches. The
/// fence's tokens after drawer 8's lay, and drawer 0's.
fn dead_stopped_drawers_then_a_launch(id: u64, shell: i32, knobs: Knobs) -> (Vec<u64>, u64) {
    use aterm_core::terminal::MAX_HOLDER_MARKS;
    set_image(shell, zsh(shell));
    let rig = Rig::attach(id, shell, knobs);
    rig.write(shell, &prompt(), "<p>");
    let mut first = None;
    for i in 0..=MAX_HOLDER_MARKS {
        let job = shell + 1 + i32::try_from(i).expect("a small index");
        let drawer = store_claude(job, 100 + i as u64);
        let token = drawer.token.expect("a tracked image");
        first.get_or_insert(token);
        set_image(job, drawer);
        rig.write(
            job,
            format!("drawer {i}\r\n").as_bytes(),
            &format!("<d{i}>"),
        );
        assert_eq!(
            rig.session
                .term
                .lock()
                .unwrap()
                .holder_fence()
                .current_token(),
            Some(token),
            "PRECONDITION: drawer {i} holds the fence"
        );
        if i == MAX_HOLDER_MARKS {
            break;
        }
        rig.write(shell, &prompt(), &format!("<stopped {i}>"));
        if i > 0 {
            rig.die(job);
        }
    }
    let marks = rig
        .session
        .term
        .lock()
        .unwrap()
        .holder_fence()
        .tokens()
        .collect();
    (marks, first.expect("drawer 0"))
}

#[test]
fn a_stopped_drawers_death_retires_its_mark_so_a_live_one_survives() {
    let (marks, live) = dead_stopped_drawers_then_a_launch(9340, 8400, KERNEL);
    assert!(
        marks.contains(&live),
        "the live stopped drawer keeps its mark: {marks:?}"
    );
    assert_eq!(marks.len(), 2, "only it and the current drawer: {marks:?}");
}

#[test]
fn control_without_the_exit_arm_dead_stopped_marks_evict_a_live_one() {
    let knobs = Knobs {
        no_retire: true,
        ..KERNEL
    };
    let (marks, live) = dead_stopped_drawers_then_a_launch(9341, 8500, knobs);
    assert!(
        !marks.contains(&live),
        "CONTROL: with no retirement, eviction by count takes the live stopped mark: {marks:?}"
    );
}

#[test]
fn the_reader_counts_only_the_feeds_that_move_the_screen() {
    const MUSE_AT_REST: &[u8] = b"\x1b[39m\x1b[49m\x1b[59m\x1b[0m\x1b[18;3H";
    let shell = 8100;
    set_image(shell, zsh(shell));
    let rig = Rig::attach(9320, shell, SHIPPING);
    set_fg(rig.session.master, shell);
    let fed = |bytes: &[u8]| {
        let before = rig.wakes.lock().unwrap().len();
        let mut sent = 0;
        while sent < bytes.len() {
            // SAFETY: Bounded write of a live slice to the test-owned slave.
            let wrote = unsafe {
                libc::write(rig.slave, bytes[sent..].as_ptr().cast(), bytes.len() - sent)
            };
            assert!(wrote > 0, "write {bytes:?}");
            sent += wrote.cast_unsigned();
        }
        let woke = aterm_spec::wait::within(PATIENCE, Duration::from_millis(1), || {
            (rig.wakes.lock().unwrap().len() > before).then_some(())
        });
        assert!(woke.is_some(), "the reader fed {bytes:?} and woke the loop");
        rig.session.ctx.screen_moves.count()
    };
    let drawn = fed(b"$ ");
    assert!(drawn >= 1, "the prompt moved the screen: {drawn}");
    assert_eq!(fed(MUSE_AT_REST), drawn, "the colour reset moved no cell");
    assert_eq!(fed(MUSE_AT_REST), drawn, "nor again");
    assert_eq!(fed(b"x"), drawn + 1, "a byte drawn moved it");
}

/// THE READER FEEDS THE STUCK ENGINE (system-awareness §19 P2a): every byte it read, a line stamp
/// only from a read that held a line feed, and the shell's newest block, open from OSC 133 C with
/// the command typed and closed by D with its status.
#[test]
fn the_reader_feeds_the_jobs_engine_its_bytes_lines_and_block() {
    let shell = 8200;
    set_image(shell, zsh(shell));
    let rig = Rig::attach(9330, shell, SHIPPING);
    let epoch = Instant::now();
    let feed = || rig.session.ctx.job_feed.at(epoch);
    let sent = |bytes: &[u8], marker: &str| {
        u64::try_from(bytes.len() + 1 + marker.len()).expect("a short write")
    };
    let typed = [&prompt()[..], b"make\r\n", &signed("133;C")].concat();
    rig.write(shell, &typed, "<typed>");
    let running = feed();
    let block = running.block.clone().expect("the running block");
    assert_eq!((block.open, block.command.as_str()), (true, "make"));
    assert!(
        running.line_output_ms.is_some() && running.output_ms.is_some(),
        "{running:?}"
    );
    assert_eq!(running.output_bytes, sent(&typed, "<typed>"));
    rig.write(shell, b"\r 50%", "<bar>");
    let bar = feed();
    assert_eq!(
        bar.line_output_ms, running.line_output_ms,
        "a redraw is no line"
    );
    assert!(bar.output_ms >= running.output_ms, "{bar:?}");
    assert_eq!(
        bar.output_bytes - running.output_bytes,
        sent(b"\r 50%", "<bar>")
    );
    assert_eq!(bar.block, running.block, "output keeps the block");
    rig.write(shell, &signed("133;D;2"), "<done>");
    let ended = feed().block.expect("the ended block");
    assert_eq!(
        (ended.id, ended.open, ended.status, ended.started_ms),
        (block.id, false, Some(2), block.started_ms)
    );
}

/// A READER ATTACHING SEEDS THE JOBS FEED WITH THE BLOCK ALREADY RUNNING: an engine restored
/// with its command open holds a block no reader fed, and the block is the feed's once a reader
/// attaches, before the program writes a byte.
#[test]
fn a_reader_attaching_seeds_the_jobs_feed_with_the_block_already_running() {
    let shell = 8210;
    set_image(shell, zsh(shell));
    let mut rig = Rig::attach(9331, shell, SHIPPING);
    assert!(
        park_reader(&mut rig.session, Instant::now() + PATIENCE),
        "parked"
    );
    let typed = [&prompt()[..], b"make\r\n", &signed("133;C")].concat();
    rig.session
        .term
        .lock()
        .expect("terminal lock")
        .process(&typed);
    assert!(rig.session.ctx.job_feed.at(Instant::now()).block.is_none());
    rig.start_reader();
    let block = rig
        .session
        .ctx
        .job_feed
        .at(Instant::now())
        .block
        .expect("the seeded block");
    assert_eq!((block.open, block.command.as_str()), (true, "make"));
}
