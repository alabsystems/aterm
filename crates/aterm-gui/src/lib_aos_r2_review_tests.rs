// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host-path tests for the Rewind R2 review fixes: private keys, the attach scrub, the watcher's handoff, `aos ls` framing and the export directory.

#[cfg(unix)]
use std::sync::Arc;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

#[cfg(unix)]
use aterm_effects::aos_momentum::MomentumEvent;
use aterm_effects::aos_momentum::{MomentumCode, MomentumSchema, MomentumState};
#[cfg(unix)]
use aterm_session::sink::SinkWriter;
#[cfg(unix)]
use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

use super::aos_repair_tests::{
    KEY_GAP, WID, leave_prompt, momentum_app, status, stranded, type_keys,
};
#[cfg(unix)]
use super::aos_repair_tests::{exported_log, momentum_app_on, sign_prompt};
use crate::App;
use crate::aos_host::{
    NO_ECHO_KEY, RAW_KEY, SECURE_ENTRY, WATCH_SPAWN_REFUSED, WATCH_THREAD, Worked, key_privacy,
};
#[cfg(unix)]
use crate::input::{InputEvent, PasteFraming, Source};
#[cfg(unix)]
use crate::scripted_redraw_fixture::{feed, scripted_app_with_sink};

/// The keys in a log.
#[cfg(unix)]
fn keys(log: &[MomentumEvent]) -> usize {
    log.iter()
        .filter(|e| matches!(e, MomentumEvent::Key { .. }))
        .count()
}

/// Whether `app`'s timeline lists a detach for `why` (spaces as underscores).
fn detached_for(app: &App, why: &str) -> bool {
    app.aos_ls_now()
        .iter()
        .any(|l| l.starts_with("slot=0 row=") && l.ends_with(&format!(" detached why={why}")))
}

/// Dispatches `ev` at `at` as the person's.
#[cfg(unix)]
fn press(app: &mut App, ev: InputEvent, at: Instant) {
    crate::app_input::pin_input_clock(Some(at));
    let _ = app.input(WID, ev, Source::Human);
    crate::app_input::pin_input_clock(None);
}

/// A plain Enter press.
#[cfg(unix)]
fn enter() -> InputEvent {
    InputEvent::Key {
        key: Key::Named(NamedKey::Enter),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    }
}

/// A real PTY pair whose slave's local modes a test sets, with a sink on its master.
#[cfg(unix)]
struct Pty {
    master: i32,
    slave: i32,
    sink: Arc<SinkWriter>,
}

#[cfg(unix)]
impl Pty {
    /// The pair with `edit` applied to the slave's `c_lflag`.
    fn new(edit: fn(&mut libc::tcflag_t)) -> Self {
        let (master, slave) = aterm_pty::open_pty_pair().expect("a pty pair");
        // SAFETY: `libc::termios` is plain integers and a byte array; zeroed is valid.
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: `slave` is open and `t` is a valid termios to fill.
        assert_eq!(unsafe { libc::tcgetattr(slave, &mut t) }, 0, "tcgetattr");
        edit(&mut t.c_lflag);
        // SAFETY: `t` is the slave's own termios, edited.
        assert_eq!(
            unsafe { libc::tcsetattr(slave, libc::TCSANOW, &t) },
            0,
            "tcsetattr"
        );
        Self {
            master,
            slave,
            sink: Arc::new(SinkWriter::new(master)),
        }
    }

    /// A momentum app at a signed prompt whose session writes to this PTY.
    fn app(&self) -> App {
        momentum_app_on(scripted_app_with_sink(self.sink.clone()), false)
    }
}

#[cfg(unix)]
impl Drop for Pty {
    fn drop(&mut self) {
        // SAFETY: both fds are this pair's and open; the sink borrows `master` and never closes it.
        unsafe {
            libc::close(self.master);
            libc::close(self.slave);
        }
    }
}

#[cfg(unix)]
const COOKED: fn(&mut libc::tcflag_t) = |_| {};
#[cfg(unix)]
const NO_ECHO: fn(&mut libc::tcflag_t) = |l| *l &= !libc::ECHO;
#[cfg(unix)]
const RAW: fn(&mut libc::tcflag_t) = |l| *l &= !(libc::ECHO | libc::ICANON);

#[test]
fn a_key_is_private_unless_aterm_sees_it_echoed() {
    let tty = |echo, canonical| {
        move || {
            Some(aterm_pty::TtyEcho {
                echo,
                canonical,
                signals: None,
            })
        }
    };
    let unreadable = || None;
    assert_eq!(
        key_privacy(true, false, true, tty(false, true)),
        Some(NO_ECHO_KEY)
    );
    assert_eq!(
        key_privacy(false, true, true, tty(true, true)),
        Some(SECURE_ENTRY)
    );
    assert_eq!(
        key_privacy(false, false, false, tty(true, true)),
        None,
        "cooked echo"
    );
    assert_eq!(
        key_privacy(false, false, false, tty(true, false)),
        None,
        "raw echo"
    );
    assert_eq!(
        key_privacy(false, false, false, tty(false, false)),
        Some(RAW_KEY),
        "raw, no echo, no prompt"
    );
    assert_eq!(
        key_privacy(false, false, true, tty(false, false)),
        None,
        "raw at an open prompt of aterm's shell"
    );
    assert_eq!(
        key_privacy(false, false, false, unreadable),
        Some(RAW_KEY),
        "unreadable"
    );
    assert_eq!(key_privacy(false, false, true, unreadable), None);
}

#[cfg(unix)]
#[test]
fn a_key_is_recorded_only_where_the_tty_or_an_open_prompt_shows_it() {
    for (name, mode, at_prompt, detach) in [
        ("cooked at a prompt", COOKED, true, None),
        ("cooked in a program", COOKED, false, None),
        (
            "canonical no-echo at a prompt",
            NO_ECHO,
            true,
            Some("no-echo_key"),
        ),
        (
            "canonical no-echo in a program",
            NO_ECHO,
            false,
            Some("no-echo_key"),
        ),
        ("raw at a prompt", RAW, true, None),
        (
            "raw in a program (ssh, Node, sudo pwfeedback)",
            RAW,
            false,
            Some("raw_key"),
        ),
    ] {
        let pty = Pty::new(mode);
        let t0 = Instant::now();
        let mut app = pty.app();
        app.redraw_window_at(WID, t0);
        if !at_prompt {
            leave_prompt(&app);
        }
        app.aos_attach_at(MomentumCode::Shipped, t0)
            .expect("attaches");
        let _ = type_keys(&mut app, t0 + KEY_GAP, 3);
        match detach {
            None => {
                assert!(status(&app).attached, "{name}: still recording");
                assert_eq!(keys(&exported_log(&app, t0)), 3, "{name}: each key logged");
            }
            Some(why) => {
                assert!(!status(&app).attached, "{name}: the first key detached");
                assert!(detached_for(&app, why), "{name}: {:?}", app.aos_ls_now());
            }
        }
    }
}

#[test]
fn a_key_to_a_tty_whose_mode_is_unreadable_and_no_prompt_detaches() {
    let t0 = Instant::now();
    let mut app = momentum_app(false);
    app.redraw_window_at(WID, t0);
    leave_prompt(&app);
    app.aos_attach_at(MomentumCode::Shipped, t0)
        .expect("attaches");
    let _ = type_keys(&mut app, t0 + KEY_GAP, 1);
    assert!(!status(&app).attached);
    assert!(detached_for(&app, "raw_key"), "{:?}", app.aos_ls_now());
}

#[cfg(unix)]
#[test]
fn after_enter_or_a_pasted_line_the_prompt_no_longer_vouches_for_a_raw_key() {
    let pty = Pty::new(RAW);
    let t0 = Instant::now();
    let mut app = pty.app();
    app.redraw_window_at(WID, t0);
    app.aos_attach_at(MomentumCode::Shipped, t0)
        .expect("attaches");
    let at = type_keys(&mut app, t0 + KEY_GAP, 2);
    assert_eq!(
        keys(&exported_log(&app, t0)),
        2,
        "keys at the prompt are logged"
    );
    press(&mut app, enter(), at + KEY_GAP);
    assert!(status(&app).attached, "Enter itself logs nothing private");
    let at = type_keys(&mut app, at + KEY_GAP * 2, 1);
    assert!(
        !status(&app).attached,
        "a key after the line went before the shell's next mark detached"
    );
    assert!(detached_for(&app, "raw_key"), "{:?}", app.aos_ls_now());

    sign_prompt(&app);
    app.aos_attach_at(MomentumCode::Shipped, at)
        .expect("re-attaches");
    let at = type_keys(&mut app, at + KEY_GAP, 2);
    assert!(
        status(&app).attached,
        "the shell's next prompt vouches again"
    );
    assert_eq!(keys(&exported_log(&app, at)), 2);

    let line =
        |text: &str| InputEvent::Paste(text.to_owned(), PasteFraming::Gesture { bracketed: false });
    feed(&app, b"\x1b[?2004h");
    press(&mut app, line("a\nb"), at + KEY_GAP);
    let at = type_keys(&mut app, at + KEY_GAP * 2, 1);
    assert!(
        status(&app).attached,
        "negative control: a bracketed paste's line break runs nothing"
    );
    feed(&app, b"\x1b[?2004l");
    press(&mut app, line("ls\r"), at + KEY_GAP);
    let _ = type_keys(&mut app, at + KEY_GAP * 2, 1);
    assert!(
        !status(&app).attached,
        "a key after an unbracketed pasted line detached"
    );
}

/// The glow's typing-rate stamp in `app`'s attach checkpoint.
fn checkpoint_rate_at(app: &App, epoch: Instant) -> Option<i128> {
    let trace = app.windows[&WID]
        .fx
        .cursor()
        .momentum
        .aos_trace()
        .expect("attached");
    let state: MomentumState = aterm_aos::checkpoint::Checkpoint::decode(&trace.checkpoint)
        .expect("decodes")
        .restore(&MomentumSchema::new(epoch), &trace.identity)
        .expect("restores");
    state.glow().snapshot(epoch).expect("a snapshot").rate_at
}

#[test]
fn an_attach_after_a_private_key_starts_the_glow_from_rest() {
    let t0 = Instant::now();
    for private in [true, false] {
        let mut app = momentum_app(false);
        app.redraw_window_at(WID, t0);
        if private {
            leave_prompt(&app);
        }
        let last = type_keys(&mut app, t0 + KEY_GAP, 3);
        let epoch = last + Duration::from_millis(10);
        app.aos_attach_at(MomentumCode::Shipped, epoch)
            .expect("attaches");
        let glow = &app.windows[&WID].fx.cursor().momentum;
        let value = glow.value(epoch, aterm_effects::cursor_momentum::MOMENTUM_GLOW_TAU_S);
        let stamp = checkpoint_rate_at(&app, epoch);
        if private {
            assert_eq!(value, 0.0, "the private keys' rate is gone");
            assert_eq!(stamp, None, "the checkpoint holds no key's instant");
        } else {
            assert!(value > 0.0, "negative control: echoed keys carry momentum");
            assert_eq!(
                stamp,
                Some(-10_000_000),
                "negative control: the checkpoint holds the last key's instant"
            );
        }
    }
}

#[test]
fn a_reattach_keeps_the_dropped_recordings_segment_for_the_watcher() {
    let t0 = Instant::now();
    let (mut app, parked) = stranded(false, t0);
    let later = parked.at + Duration::from_millis(500);
    app.aos_repair_at(MomentumCode::Shipped, later)
        .expect("repairs");
    assert_eq!(
        app.windows[&WID].fx.cursor().momentum.aos_retired_waiting(),
        1
    );
    app.aos_attach_at(MomentumCode::Shipped, later + Duration::from_millis(10))
        .expect("re-attaches");
    app.aos_watch();
    let lines = app.aos_ls_now();
    assert!(lines[1].contains(" waiting=0 kept=1 rows=1 "), "{lines:?}");
    assert!(
        lines[2].starts_with("slot=0 row=0 segment cause=committed code=demo-stranded-settle ")
            && lines[2].contains(" judged=fail:HotHasWake@"),
        "{lines:?}"
    );
}

#[test]
fn a_watcher_that_cannot_start_lists_each_segment_unjudged_and_claims_no_detach() {
    let t0 = Instant::now();
    for refused in [true, false] {
        let (mut app, parked) = stranded(false, t0);
        app.aos_repair_at(
            MomentumCode::Shipped,
            parked.at + Duration::from_millis(500),
        )
        .expect("repairs");
        let (post, worked) = channel();
        WATCH_SPAWN_REFUSED.with(|r| r.set(refused));
        app.aos_watch_via(Some(move |w: Worked| {
            let _ = post.send((std::thread::current().name().map(str::to_owned), w));
        }));
        WATCH_SPAWN_REFUSED.with(|r| r.set(false));
        if !refused {
            let (thread, w) = worked
                .recv_timeout(Duration::from_secs(60))
                .expect("the worker posts its judgements");
            assert_eq!(
                thread.as_deref(),
                Some(WATCH_THREAD),
                "judged on the watcher's thread"
            );
            app.aos_worked(w);
        }
        let lines = app.aos_ls_now();
        assert!(status(&app).attached, "refused={refused}: still recording");
        assert!(
            !lines.iter().any(|l| l.contains(" detached why=")),
            "refused={refused}: {lines:?}"
        );
        assert!(lines[1].contains(" waiting=0 "), "{lines:?}");
        let row = &lines[2];
        assert!(
            row.starts_with("slot=0 row=0 segment cause=committed code=demo-stranded-settle "),
            "{lines:?}"
        );
        if refused {
            assert!(
                row.ends_with(" judged=unjudged:the_watcher's_host_worker_did_not_run kept=no"),
                "{row}"
            );
        } else {
            assert!(
                row.contains(" judged=fail:HotHasWake@") && row.ends_with(" kept=0"),
                "{row}"
            );
        }
    }
}

#[test]
fn aos_ls_answers_every_row_in_lines_framing() {
    use crate::aos_verb::{Asked, main_reply};
    let t0 = Instant::now();
    let (mut app, parked) = stranded(false, t0);
    app.aos_repair_at(
        MomentumCode::Shipped,
        parked.at + Duration::from_millis(500),
    )
    .expect("repairs");
    app.aos_watch();
    let rows = app.aos_ls_now();
    assert_eq!(rows.len(), 3, "fixture: tasks, the window and one segment");
    let reply = main_reply(Asked::Ls, Ok(rows.join("\n")));
    let (head, body) = reply.split_once('\n').expect("a head line");
    let n: usize = head
        .strip_prefix("OK ")
        .and_then(|n| n.parse().ok())
        .expect("`OK <n>`");
    let got: Vec<&str> = body.lines().collect();
    assert_eq!(n, rows.len(), "{reply:?}");
    assert_eq!(got, rows.iter().map(String::as_str).collect::<Vec<_>>());
    assert!(
        reply.ends_with('\n') && !body.ends_with("\n\n"),
        "{reply:?}"
    );
    assert_eq!(
        aterm_types::control_verbs::framing_of("aos", "aos ls"),
        aterm_types::control_verbs::Framing::Lines
    );
    assert_eq!(
        main_reply(Asked::Status, Ok("window=0 attached=true".into())),
        "OK window=0 attached=true\n"
    );
    assert_eq!(
        main_reply(Asked::Ls, Err("no window".into())),
        "ERR no window\n"
    );
}

#[cfg(unix)]
#[test]
fn an_export_refuses_a_linked_directory_and_tightens_a_loose_one_through_its_fd() {
    use std::os::unix::fs::PermissionsExt;

    use crate::aos_verb::write_export;

    let root = aterm_tempfile::tempdir().expect("a scratch state dir");
    let elsewhere = aterm_tempfile::tempdir().expect("a folder of the person's");
    std::fs::set_permissions(elsewhere.path(), std::fs::Permissions::from_mode(0o755))
        .expect("0755");
    std::os::unix::fs::symlink(elsewhere.path(), root.path().join("aos")).expect("links");
    let refused = write_export(root.path(), "x.trace", b"bytes").expect_err("a link");
    assert!(
        refused.to_string().contains("is a symbolic link"),
        "{refused}"
    );
    let mode = std::fs::metadata(elsewhere.path())
        .expect("there")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o755, "the link's target is untouched");
    assert!(
        std::fs::read_dir(elsewhere.path())
            .expect("lists")
            .next()
            .is_none(),
        "nothing was written through the link"
    );

    let loose = aterm_tempfile::tempdir().expect("a scratch state dir");
    let dir = loose.path().join("aos");
    std::fs::create_dir(&dir).expect("made");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("0755");
    let path = write_export(loose.path(), "x.trace", b"bytes").expect("writes");
    let mode = std::fs::metadata(&dir).expect("there").permissions().mode();
    assert_eq!(
        mode & 0o777,
        0o700,
        "a loose export directory is made private"
    );
    assert_eq!(std::fs::read(path).expect("written"), b"bytes");
}

#[test]
fn turning_secure_keyboard_entry_on_in_the_config_detaches_a_recording() {
    let head = "serious_mode = false\ncursor_trail = true\ncursor_trail_style = \"off\"\n\
                cursor_momentum_glow = true\nmotion = \"full\"\ntrail_sounds = false\n\
                cursor_blink = false\n";
    for secure in [true, false] {
        let t0 = Instant::now();
        let mut app = momentum_app(false);
        app.redraw_window_at(WID, t0);
        app.aos_attach_at(MomentumCode::Shipped, t0)
            .expect("attaches");
        let dir = aterm_tempfile::tempdir().expect("a config dir");
        crate::app_colorscheme::install_resolved_colors::apply_config_text(
            &mut app,
            dir.path(),
            &format!("{head}secure_keyboard_entry = {secure}\n"),
        );
        assert_eq!(
            app.config.secure_keyboard_entry_or_default(),
            secure,
            "fixture: the config applied"
        );
        assert_eq!(status(&app).attached, !secure, "secure={secure}");
        assert_eq!(detached_for(&app, "secure_keyboard_entry"), secure);
    }
}

#[cfg(unix)]
#[test]
fn aos_check_refuses_a_file_past_its_bound_before_it_decodes() {
    use aterm_effects::aos_momentum::MOMENTUM_TRACE_MAX_BYTES;

    use crate::aos_verb::{check_file, write_export};

    let root = aterm_tempfile::tempdir().expect("a scratch state dir");
    let fits = write_export(
        root.path(),
        "fits.trace",
        &vec![0; MOMENTUM_TRACE_MAX_BYTES],
    )
    .expect("writes");
    let past = write_export(
        root.path(),
        "past.trace",
        &vec![0; MOMENTUM_TRACE_MAX_BYTES + 1],
    )
    .expect("writes");
    let refused = check_file(&past, "any-build").expect_err("past the bound");
    assert!(refused.ends_with("exceeded its size bound"), "{refused}");
    let decoded = check_file(&fits, "any-build").expect_err("not a trace");
    assert!(
        decoded.starts_with("aos check refused="),
        "negative control: a file within the bound is decoded: {decoded}"
    );
}
