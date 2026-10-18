// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A file citation end to end through the real pointer handlers: the hover
//! hand and its caption, then the Cmd-click that opens aterm's own editor at
//! the cited line (`crate::path_link`).

use std::time::Instant;

use crate::native_app::AppViewState;
use crate::{App, WindowId, term_lock};

fn link_modifier() -> winit::keyboard::ModifiersState {
    #[cfg(target_os = "macos")]
    {
        winit::keyboard::ModifiersState::SUPER
    }
    #[cfg(not(target_os = "macos"))]
    {
        winit::keyboard::ModifiersState::CONTROL
    }
}

/// The group the fixture's shell leads in front of its PTY. No process leads
/// it, so no OS cwd stands in front of the OSC 7; it is also never a real fd
/// (the fixture's master carries it, below).
const SHELL: i32 = i32::MAX - 1;

/// Put `program` in front of `wid`'s front session as group `pgid`: the PTY's
/// foreground group as a fresh read finds it (the factory's probe answers the
/// master's own number, which the fixture sets to `pgid`) and the status
/// sweep's name for it (`status program=`).
fn in_front(app: &mut App, wid: WindowId, pgid: i32, program: &str) {
    named_front(app, wid, pgid, pgid, program);
}

/// [`in_front`] with the fresh read and the sweep's name apart: `fresh` is
/// in front now, the sweep last named `named` (a group it has not caught up
/// with).
fn named_front(app: &mut App, wid: WindowId, fresh: i32, named: i32, program: &str) {
    let session = app.front_terminal(wid).expect("terminal").session;
    app.session_factory.fg_probe = |master| master;
    app.pool.sessions.get_mut(&session).unwrap().session.master = fresh;
    let ctx = app.pool.get(session).unwrap().ctx.clone();
    let mut timeline = ctx.timeline.lock().unwrap();
    timeline.note_foreground_group(named);
    timeline.set_program(named, Some(program.into()));
}

/// The status sweep's witness of the engine's current cwd report: the group
/// in front at its look.
fn sweep_sees_cwd(app: &App, wid: WindowId, pgid: i32) {
    let front = app.front_terminal(wid).expect("terminal");
    let seq = term_lock(&front.term).reported_cwd_seq();
    app.pool
        .get(front.session)
        .unwrap()
        .ctx
        .timeline
        .lock()
        .unwrap()
        .witness_cwd_report(seq, pgid);
}

/// A scratch project (`src/foo.rs`, `src/sub/`) and a headless App whose
/// session's shell reported it as its directory (OSC 7), with `row` printed
/// and the shell (`zsh`) in front.
fn app_with(row: &str) -> (App, WindowId, aterm_tempfile::TempDir, std::path::PathBuf) {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let root = std::fs::canonicalize(dir.path()).expect("canonical");
    std::fs::create_dir_all(root.join("src/sub")).unwrap();
    std::fs::write(root.join("src/foo.rs"), "one\ntwo\nthree\nfour\n").unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    term_lock(&term)
        .process(format!("\x1b]7;file://localhost{}\x07{row}\r\n", root.display()).as_bytes());
    in_front(&mut app, wid, SHELL, "zsh");
    app.on_modifiers_changed(wid, link_modifier());
    (app, wid, dir, root)
}

/// Hover window cell `(row, col)` through the real motion path; the hand.
fn hover(app: &mut App, wid: WindowId, row: u16, col: u16) -> bool {
    let (cw, ch) = app.win_cell_size(wid);
    let strip_px = f64::from(app.tab_strip_rows) * ch as f64;
    let x = app.win_pad(wid) as f64 + cw as f64 * (f64::from(col) + 0.5);
    let y = (app.win_pad_top(wid) + app.win_head(wid)) as f64
        + strip_px
        + ch as f64 * (f64::from(row) + 0.5);
    app.on_cursor_moved(wid, x, y);
    assert_eq!(app.windows[&wid].last_mouse_cell, (row, col));
    app.windows[&wid].hover_pointer
}

/// Compose the fill, strip, and caption, then model an accepted present for
/// these App routing fixtures. No real display or present is exercised; the
/// separate local-data aggregate checks the actual receipt commit policy.
fn band(app: &mut App, wid: WindowId) -> String {
    assert!(
        app.prepare_terminal_capture_grid_with_cursor_fx(
            wid,
            crate::app_render::ComposedCursorFxClock::Advance(Instant::now()),
        )
        .is_some()
    );
    app.splice_tab_strip(wid);
    let caption = app.compose_link_caption(wid, None);
    crate::app_mouse::commit_presented_caption(
        &mut app.windows.get_mut(&wid).expect("window").shown_caption,
        caption,
        &Ok(Some(0)),
    );
    app.windows[&wid]
        .input_scratch
        .cells
        .last()
        .expect("rows")
        .iter()
        .map(|cell| cell.ch)
        .collect()
}

#[test]
fn cmd_click_on_a_citation_opens_aterms_editor_at_its_line() {
    let (mut app, wid, _dir, root) = app_with("error: src/foo.rs:3:2 here");
    assert!(
        app.active_native_view(wid).is_none(),
        "precondition: a terminal tab"
    );
    // Every cell of the citation offers the click; the words around it do not.
    for col in 7..20 {
        assert!(hover(&mut app, wid, 0, col), "col {col}");
    }
    assert!(!hover(&mut app, wid, 0, 2), "`error:` is not a file");
    assert!(!hover(&mut app, wid, 0, 23), "nor `here`");
    assert!(hover(&mut app, wid, 0, 10));
    assert!(
        app.windows[&wid].link_hover.is_some(),
        "the citation is disclosed"
    );
    let caption = band(&mut app, wid);
    assert!(
        caption.contains("opens in aterm: ") && caption.contains("/src/foo.rs:3:2"),
        "the band names aterm, where the file is, and the line: {caption:?}"
    );
    assert_eq!(
        app.link_under_pointer(wid),
        None,
        "no URL there: nothing for the OS"
    );
    let target = app
        .path_link_under_pointer(wid)
        .expect("the press reads the citation");
    assert_eq!(target.path, root.join("src/foo.rs"));
    assert_eq!((target.line, target.col), (Some(3), Some(2)));

    assert!(
        app.open_link_under_pointer(wid),
        "the press opened something"
    );
    let (instance, view) = app.active_native_view(wid).expect("an editor tab in front");
    let document = app
        .native_runtime
        .document_id(instance)
        .expect("a document");
    let text = app
        .document_store
        .snapshot(document)
        .expect("snapshot")
        .text;
    assert_eq!(text.as_ref(), "one\ntwo\nthree\nfour\n");
    let Some(AppViewState::Editor(editor)) = app.native_runtime.view_state(view) else {
        panic!("an Editor, not a Markdown view");
    };
    let caret = editor
        .buffer
        .as_ref()
        .expect("buffer")
        .primary_selection()
        .head;
    assert_eq!(caret, "one\ntwo\nt".len(), "line 3, column 2");
    assert_eq!(editor.status.as_deref(), Some("Line 3"));
}

/// The canonical file the standing caption names, if it names a citation.
fn captioned(app: &App, wid: WindowId) -> Option<std::path::PathBuf> {
    app.windows[&wid]
        .hover_memo
        .as_ref()
        .and_then(|memo| memo.caption_citation())
        .map(std::path::Path::to_path_buf)
}

/// Feed `bytes` to the front session's engine, with no frame after it: what
/// the caption drew is still what stands.
fn feed(app: &App, wid: WindowId, bytes: &[u8]) {
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    term_lock(&term).process(bytes);
}

/// The press opens only what the DRAWN caption named. The session's
/// directory moves under a still pointer (an OSC 7, no output, no frame): the
/// caption drawn still names the first project's `src/foo.rs`, a fresh
/// resolution names the second's, and that press opens nothing; the caption
/// is asked again. A press before that new caption is drawn still opens
/// nothing (the memo already names the second file, the screen does not).
/// NEGATIVE CONTROL: once drawn, the press opens the file it names.
#[test]
fn a_press_opens_only_the_file_the_drawn_caption_named() {
    let (mut app, wid, _dir, root) = app_with("error: src/foo.rs:3:2 here");
    let other_dir = aterm_tempfile::tempdir().expect("scratch");
    let other = std::fs::canonicalize(other_dir.path()).expect("canonical");
    std::fs::create_dir_all(other.join("src")).unwrap();
    std::fs::write(other.join("src/foo.rs"), "uno\ndos\ntres\n").unwrap();
    assert!(hover(&mut app, wid, 0, 10));
    let first = band(&mut app, wid);
    assert_eq!(captioned(&app, wid), Some(root.join("src/foo.rs")));

    feed(
        &app,
        wid,
        format!("\x1b]7;file://localhost{}\x07", other.display()).as_bytes(),
    );
    assert_eq!(
        app.path_link_under_pointer(wid).map(|target| target.path),
        Some(other.join("src/foo.rs")),
        "precondition: a fresh resolution names the second"
    );
    assert!(
        !app.open_link_under_pointer(wid),
        "a press the drawn caption did not name opens nothing"
    );
    assert!(app.active_native_view(wid).is_none(), "nothing opened");
    assert_eq!(
        captioned(&app, wid),
        Some(other.join("src/foo.rs")),
        "the caption was asked again, and names what a press now opens"
    );
    assert!(
        !app.open_link_under_pointer(wid),
        "not yet drawn: the screen still shows the first file"
    );
    assert!(app.active_native_view(wid).is_none(), "nothing opened");
    let second = band(&mut app, wid);
    assert_ne!(
        first, second,
        "the band reads differently: {first:?} / {second:?}"
    );

    assert!(app.open_link_under_pointer(wid), "caption and press agree");
    let (instance, _) = app.active_native_view(wid).expect("an editor tab in front");
    let document = app
        .native_runtime
        .document_id(instance)
        .expect("a document");
    let text = app
        .document_store
        .snapshot(document)
        .expect("snapshot")
        .text;
    assert_eq!(
        text.as_ref(),
        "uno\ndos\ntres\n",
        "the file the caption named"
    );
}

/// A citation never opens unseen: a press with no caption drawn for the cell
/// (the look came back after the frame, no frame yet) opens nothing and asks
/// the caption. NEGATIVE CONTROL: drawn, the same press opens it.
#[test]
fn a_citation_never_opens_before_its_caption_is_drawn() {
    let (mut app, wid, _dir, _root) = app_with("error: src/foo.rs:3:2 here");
    assert!(hover(&mut app, wid, 0, 10));
    assert!(app.path_link_under_pointer(wid).is_some(), "precondition");
    assert!(!app.open_link_under_pointer(wid), "no caption drawn yet");
    assert!(app.active_native_view(wid).is_none(), "nothing opened");
    band(&mut app, wid);
    assert!(app.open_link_under_pointer(wid), "drawn, it opens");
}

/// The rule the press is held to, arm by arm ([`crate::app_mouse::caption_admits`]):
/// a citation only under a caption that named its file, line and column; a
/// URL under a caption only when it is the URL the caption named, and under
/// no caption as before. NEGATIVE CONTROLS: the same file at another line or
/// column, another file, a URL caption over a citation press, a citation
/// caption over a URL press, and a URL the caption did not name.
#[test]
fn a_press_opens_only_what_the_caption_named() {
    use crate::app_mouse::{CaptionNames, caption_admits};
    let target = |path: &str, line, col| crate::path_link::PathTarget {
        path: path.into(),
        line,
        col,
        shown: String::new(),
    };
    let cites = |path: &str, line, col| CaptionNames::Citation {
        path: path.into(),
        line,
        col,
    };
    let url = |u: &str| CaptionNames::Url(u.into());
    let named = cites("/p/a.rs", Some(10), Some(2));
    assert!(caption_admits(
        Some(&named),
        None,
        Some(&target("/p/a.rs", Some(10), Some(2)))
    ));
    for other in [
        target("/p/a.rs", Some(99), Some(2)),
        target("/p/a.rs", Some(10), None),
        target("/q/a.rs", Some(10), Some(2)),
    ] {
        assert!(
            !caption_admits(Some(&named), None, Some(&other)),
            "{other:?}"
        );
    }
    let a = target("/p/a.rs", None, None);
    assert!(!caption_admits(None, None, Some(&a)), "no caption drawn");
    assert!(!caption_admits(
        Some(&url("https://a.example/")),
        None,
        Some(&a)
    ));
    assert!(!caption_admits(
        Some(&named),
        Some("https://a.example/"),
        None
    ));
    assert!(caption_admits(
        Some(&url("https://a.example/")),
        Some("https://a.example/"),
        None
    ));
    assert!(!caption_admits(
        Some(&url("https://a.example/")),
        Some("https://b.example/"),
        None
    ));
    assert!(
        caption_admits(None, Some("https://a.example/"), None),
        "a plain URL"
    );
    assert!(caption_admits(None, None, None), "nothing to open");
}

/// A URL press under a drawn caption opens only the URL the caption named:
/// the program re-points the cell's OSC 8 link between the frame and the
/// press, and the press opens nothing. NEGATIVE CONTROL: once the new
/// caption is drawn, the press reaches the URL (here a scheme the click
/// refuses, so nothing launches either way; the refusal is the open path's).
#[test]
fn a_url_press_opens_only_the_url_the_drawn_caption_named() {
    let (mut app, wid, _dir, _root) =
        app_with("\x1b]8;;https://one.example/\x1b\\link\x1b]8;;\x1b\\");
    assert!(hover(&mut app, wid, 0, 1));
    let caption = band(&mut app, wid);
    assert!(caption.contains("one.example"), "{caption:?}");
    feed(
        &app,
        wid,
        b"\r\x1b[1A\x1b]8;;sftp://two.example/\x1b\\link\x1b]8;;\x1b\\\r\n",
    );
    assert_eq!(
        app.link_under_pointer(wid).as_deref(),
        Some("sftp://two.example/"),
        "precondition: the cell's link moved"
    );
    let shown = app.windows[&wid].shown_caption.clone();
    assert!(
        !crate::app_mouse::caption_admits(
            shown.as_ref().map(|shown| &shown.names),
            Some("sftp://two.example/"),
            None
        ),
        "the drawn caption named the first URL"
    );
    assert!(!app.open_link_under_pointer(wid));
    band(&mut app, wid);
    let shown = app.windows[&wid].shown_caption.clone();
    assert!(crate::app_mouse::caption_admits(
        shown.as_ref().map(|shown| &shown.names),
        Some("sftp://two.example/"),
        None
    ));
}

/// A citation resolves only while the session is known to run here, read
/// when it is asked. Refused: an OSC 7 naming another host; any program in
/// front aterm does not know runs here (remote shells, a multiplexer,
/// wrappers with their own terminal, an editor); a group in front the sweep
/// has not named yet; a remote shell as the shell's running command; a cwd a
/// checkpoint carried, which is no base. NEGATIVE CONTROLS: the same session
/// reporting `localhost` or this machine's own name; a shell, an agent by
/// name and Claude Code under `node` by its screen in front; a local
/// command; the remote report once its ssh has left the front; the shell's
/// next report after the carry.
#[test]
fn a_citation_resolves_only_while_the_session_runs_here() {
    let (mut app, wid, _dir, root) = app_with("error: src/foo.rs:3:2 here");
    let local = root.join("src/foo.rs");
    // The press path reads fresh; the hover is re-asked from scratch where the
    // pointer stands (a still pointer moves nothing, so no motion would ask).
    let press = |app: &App| app.path_link_under_pointer(wid).map(|target| target.path);
    let rehover = |app: &mut App| {
        app.windows.get_mut(&wid).unwrap().hover_memo = None;
        app.update_hover_cursor(wid);
        app.windows[&wid].hover_pointer
    };
    assert!(hover(&mut app, wid, 0, 10));
    assert!(rehover(&mut app));
    assert_eq!(press(&app), Some(local.clone()), "control: a local session");

    // OSC 7 naming another machine: the same directory, on that machine.
    let osc7 = |host: &str| format!("\x1b]7;file://{host}{}\x07", root.display());
    feed(&app, wid, osc7("build-box.example.invalid").as_bytes());
    assert_eq!(press(&app), None, "an OSC 7 host that is not this machine");
    assert!(!rehover(&mut app), "and no hand offers it");
    band(&mut app, wid);
    assert!(!app.open_link_under_pointer(wid));
    assert!(app.active_native_view(wid).is_none());
    // Seen by the sweep under the shell itself: it still stands.
    sweep_sees_cwd(&app, wid, SHELL);
    assert_eq!(press(&app), None, "reported under the shell in front");
    let me = crate::tab_label::this_machine_name();
    if !me.is_empty() {
        feed(&app, wid, osc7(me).as_bytes());
        assert_eq!(press(&app), Some(local.clone()), "this machine by name");
    }
    feed(&app, wid, osc7("localhost").as_bytes());
    assert_eq!(press(&app), Some(local.clone()), "localhost");

    // An ssh's remote shell reports its directory, and the ssh exits back to
    // a shell that really runs here, in the project (its OS cwd is a base).
    // A shell integration that sends no OSC 7 of its own leaves the remote
    // report standing in the engine.
    let mut shell = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("120")
        .current_dir(&root)
        .spawn()
        .expect("a local process in the project");
    let here = i32::try_from(shell.id()).expect("a pid");
    let ssh = SHELL - 1;
    in_front(&mut app, wid, ssh, "ssh");
    feed(&app, wid, osc7("build-box.example.invalid").as_bytes());
    sweep_sees_cwd(&app, wid, ssh);
    assert_eq!(press(&app), None, "ssh in front");
    in_front(&mut app, wid, here, "zsh");
    assert_eq!(
        press(&app),
        Some(local.clone()),
        "the ssh's report left the front with it; the shell's own cwd answers"
    );
    // The negative control of the witness: the same remote report NOT seen
    // under the ssh (newer than the sweep's look) stands, and refuses.
    in_front(&mut app, wid, ssh, "ssh");
    feed(&app, wid, osc7("build-box.example.invalid").as_bytes());
    in_front(&mut app, wid, here, "zsh");
    assert_eq!(press(&app), None, "a report no sweep has seen stands");
    let _ = shell.kill();
    let _ = shell.wait();
    in_front(&mut app, wid, SHELL, "zsh");
    feed(&app, wid, osc7("localhost").as_bytes());

    // Only a program known to run here offers a citation.
    let other = SHELL - 2;
    for program in [
        "ssh",
        "mosh-client",
        "et",
        "autossh",
        "tmux",
        "screen",
        "sudo",
        "sshpass",
        "docker",
        "kubectl",
        "telnet",
        "vim",
    ] {
        in_front(&mut app, wid, other, program);
        assert_eq!(press(&app), None, "{program} in front");
        assert!(!rehover(&mut app), "{program} in front");
    }
    for program in ["zsh", "bash", "claude", "codex"] {
        in_front(&mut app, wid, other, program);
        assert_eq!(press(&app), Some(local.clone()), "{program} in front");
    }
    in_front(&mut app, wid, other, "node");
    assert_eq!(press(&app), None, "node, not known to be an agent");
    {
        let session = app.front_terminal(wid).unwrap().session;
        let ctx = app.pool.get(session).unwrap().ctx.clone();
        ctx.timeline.lock().unwrap().publish_agent(
            "idle",
            None,
            None,
            Some(aterm_phase::Program::Claude),
            crate::session_timeline::AgentStamp {
                generation: crate::control::ScreenGen { epoch: 0, seq: 1 },
                fp: 1,
            },
        );
    }
    assert_eq!(press(&app), Some(local.clone()), "Claude Code under node");
    // A group in front the sweep has not named yet: its old name is not it.
    named_front(&mut app, wid, other - 1, other, "zsh");
    assert_eq!(press(&app), None, "a group not named yet");
    in_front(&mut app, wid, SHELL, "zsh");
    assert_eq!(press(&app), Some(local.clone()), "named");

    // The shell's running command (OSC 133 C, its 633 E line): `ssh` runs.
    feed(
        &app,
        wid,
        b"\x1b]133;A\x07$ \x1b]133;B\x07\x1b]633;E;ssh build-box\x07\x1b]133;C\x07",
    );
    assert_eq!(
        crate::session_status::executing_detail(&term_lock(
            &app.front_terminal(wid).unwrap().term.clone()
        ))
        .as_deref(),
        Some("ssh"),
        "precondition: detail=ssh"
    );
    assert_eq!(press(&app), None, "a remote shell as the running command");
    // It ends; a local command runs.
    feed(
        &app,
        wid,
        b"\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07\x1b]633;E;make\x07\x1b]133;C\x07",
    );
    assert_eq!(press(&app), Some(local.clone()), "a local command");

    // A seamless update carried the cwd: no report this engine saw stands
    // behind it, so it is no base.
    {
        let term = app.front_terminal(wid).unwrap().term.clone();
        let mut term = term_lock(&term);
        let carry = term.checkpoint();
        term.restore_checkpoint(&carry);
        assert_eq!(term.reported_cwd_seq(), None, "precondition: carried");
    }
    assert_eq!(press(&app), None, "a carried cwd");
    feed(&app, wid, osc7("localhost").as_bytes());
    assert_eq!(press(&app), Some(local), "the shell's next report"); // The fixture's master was a group's number, never an fd; give it back.
    let session = app.front_terminal(wid).unwrap().session;
    app.pool.sessions.get_mut(&session).unwrap().session.master = -1;
}

/// Where no foreground group can be read (a ConPTY), the shell's own
/// integration is the reading: at its prompt, or running a shell or an agent
/// by its command's word. NEGATIVE CONTROLS: no integration at all, and any
/// other command (a remote shell, an editor).
#[test]
fn with_no_foreground_group_the_shells_own_reading_stands_in() {
    use crate::app_mouse::shell_reads_local;
    assert!(shell_reads_local(true, None), "integrated, at its prompt");
    for command in [
        "claude --resume",
        "codex",
        "pwsh",
        "C:\\Tools\\codex.exe",
        "bash",
    ] {
        assert!(shell_reads_local(true, Some(command)), "{command}");
    }
    assert!(!shell_reads_local(false, None), "no integration");
    for command in ["ssh build-box", "ssh.exe host", "vim", "tmux"] {
        assert!(!shell_reads_local(true, Some(command)), "{command}");
    }
}

#[test]
fn a_citation_of_a_directory_or_a_missing_file_offers_nothing() {
    let (mut app, wid, _dir, _root) = app_with("see src/sub and src/gone.rs:4 and ../../ok");
    for col in [4u16, 8, 16, 22] {
        assert!(!hover(&mut app, wid, 0, col), "col {col}");
        assert!(app.windows[&wid].link_hover.is_none(), "col {col}");
        assert!(app.path_link_under_pointer(wid).is_none(), "col {col}");
        assert!(!app.open_link_under_pointer(wid), "col {col}");
    }
    assert!(app.active_native_view(wid).is_none(), "nothing opened");
}

#[test]
fn a_url_over_a_citation_still_wins_and_a_refused_scheme_opens_nothing() {
    // An OSC 8 link the engine admits (`sftp:`) but the click refuses, drawn
    // over a real citation: the click is the link's, and a refused link opens
    // nothing, the file beneath it included.
    let (mut app, wid, _dir, _root) =
        app_with("\x1b]8;;sftp://host.example/x\x1b\\src/foo.rs:2\x1b]8;;\x1b\\");
    assert!(hover(&mut app, wid, 0, 3));
    assert_eq!(
        app.link_under_pointer(wid).as_deref(),
        Some("sftp://host.example/x")
    );
    assert!(!app.open_link_under_pointer(wid));
    assert!(app.active_native_view(wid).is_none());
}

/// A driver's `pointer mods` holds its Cmd only until the person's own input:
/// their next key or click reads the modifiers they really hold, so a scripted
/// link modifier never turns a typed `v` into Cmd-V or a plain click into a
/// Cmd-click.
#[test]
fn a_scripted_modifier_ends_at_the_persons_next_input() {
    use crate::app_mouse::PointerAction;
    use winit::keyboard::ModifiersState as M;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.on_modifiers_changed(wid, M::SHIFT);
    app.pointer_cmd(PointerAction::Mods(link_modifier()))
        .expect("mods");
    assert_eq!(app.windows[&wid].mods, link_modifier());
    // A second scripted set still keeps the person's own aside, not the first script's.
    app.pointer_cmd(PointerAction::Mods(M::CONTROL))
        .expect("mods");
    assert_eq!(app.windows[&wid].mods, M::CONTROL);
    app.release_scripted_mods(wid);
    assert_eq!(
        app.windows[&wid].mods,
        M::SHIFT,
        "the person's Shift is back"
    );
    // Negative control: with no script in force the person's input changes nothing.
    app.on_modifiers_changed(wid, M::ALT);
    app.release_scripted_mods(wid);
    assert_eq!(app.windows[&wid].mods, M::ALT);
}

/// A composed or captured caption is not a present receipt. The helper is
/// the one the shipping present wrapper uses; these are captured outcomes,
/// with no App, render target, capture, or display involved.
#[test]
fn only_an_accepted_present_changes_the_caption_a_press_may_open() {
    use crate::app_mouse::{CaptionNames, ShownCaption, commit_presented_caption};
    use crate::metrics::PresentDropReason;
    let caption = |path: &str, line, col| ShownCaption {
        session: 7,
        cell: (2, 3),
        names: CaptionNames::Citation {
            path: path.into(),
            line,
            col,
        },
    };
    let a = caption("/p/a.rs", Some(3), Some(2));
    let b = caption("/q/b.rs", Some(9), None);
    let mut shown = None;
    // Ordinary accepted frames install, replace, and clear their caption.
    commit_presented_caption(&mut shown, Some(a.clone()), &Ok(Some(0)));
    assert_eq!(shown, Some(a.clone()));
    commit_presented_caption(&mut shown, Some(b.clone()), &Ok(Some(1)));
    assert_eq!(shown, Some(b.clone()));
    commit_presented_caption(&mut shown, None, &Ok(Some(2)));
    assert_eq!(shown, None);
    let url = ShownCaption {
        session: 7,
        cell: (2, 3),
        names: CaptionNames::Url("https://a.example/".into()),
    };
    commit_presented_caption(&mut shown, Some(url.clone()), &Ok(Some(3)));
    assert_eq!(shown, Some(url));

    // Model two failed attempts after A was accepted. Evaluate every case
    // before the named final assertion, so one failure cannot hide another.
    let mut pending = Some(a.clone());
    commit_presented_caption(&mut pending, Some(b.clone()), &Ok(None));
    let mut refused = Some(a.clone());
    commit_presented_caption(
        &mut refused,
        Some(b.clone()),
        &Err(PresentDropReason::CpuCommit),
    );
    let mut refused_clear = Some(a.clone());
    commit_presented_caption(
        &mut refused_clear,
        None,
        &Err(PresentDropReason::TargetMismatch),
    );
    let held = pending == Some(a.clone()) && refused == Some(a.clone()) && refused_clear == Some(a);
    // A later accepted retry publishes B, and an accepted uncaptioned frame
    // clears it, even after those earlier pending/refused attempts.
    commit_presented_caption(&mut pending, Some(b.clone()), &Ok(Some(4)));
    assert_eq!(pending, Some(b));
    commit_presented_caption(&mut pending, None, &Ok(Some(5)));
    assert_eq!(pending, None);
    assert!(held, "LINK_CAPTION_COMMITTED_WITHOUT_AN_ACCEPTED_PRESENT");
}
