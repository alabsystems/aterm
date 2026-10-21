// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::terminal_emacs_search_direction;
#[cfg(unix)]
use crate::{App, PhysicalPressOwner, SearchRepeatAction, WindowId, term_lock};
use aterm_window::keyboard::{Key, ModifiersState};
#[cfg(unix)]
use aterm_window::keyboard::{KeyCode, PhysicalKey};

fn character(value: &str) -> Key {
    Key::Character(value.into())
}

/// Exhaust all 16 modifier subsets for both navigation keys (plus case and
/// a non-navigation negative control), at a shell and in an agent's composer.
/// The exact bare-Super subset (where `HARDCODED_SUPER_CHORDS` is on) is
/// aterm's everywhere; the bare-Control subset only in an agent's composer, so
/// at a shell bash's readline gets its C-r / C-s. The front program is asked
/// for a bare Control-S/R and nothing else.
#[test]
fn bare_super_s_r_classifier_is_exhaustive_over_modifier_power_set() {
    use std::cell::Cell;

    let flags = [
        ModifiersState::SHIFT,
        ModifiersState::CONTROL,
        ModifiersState::ALT,
        ModifiersState::SUPER,
    ];
    for agent_composer in [false, true] {
        for mask in 0u8..16 {
            let mut mods = ModifiersState::empty();
            for (bit, flag) in flags.into_iter().enumerate() {
                if mask & (1 << bit) != 0 {
                    mods |= flag;
                }
            }
            let bare_ctrl = mods == ModifiersState::CONTROL;
            let expected = (super::HARDCODED_SUPER_CHORDS && mods == ModifiersState::SUPER)
                || (bare_ctrl && agent_composer);
            for (key, forward) in [("s", true), ("R", false)] {
                let asked = Cell::new(0u32);
                assert_eq!(
                    terminal_emacs_search_direction(&character(key), mods, || {
                        asked.set(asked.get() + 1);
                        agent_composer
                    }),
                    expected.then_some(forward),
                    "{key} mask={mask:04b} agent_composer={agent_composer}"
                );
                assert_eq!(
                    asked.get(),
                    u32::from(bare_ctrl),
                    "the front program is read for a bare Control-{key} alone, \
                     mask={mask:04b}"
                );
            }
            assert_eq!(
                terminal_emacs_search_direction(&character("f"), mods, || {
                    panic!("Control-F is never a search chord; nothing reads the front")
                }),
                None,
                "Cmd-F keeps its legacy path, mask={mask:04b}"
            );
        }
    }
}

#[cfg(unix)]
fn observe_pty(app: &mut App, wid: WindowId) -> [i32; 2] {
    use std::sync::Arc;

    use aterm_session::sink::SinkWriter;

    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    let flags = unsafe { libc::fcntl(pipe[0], libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(pipe[0], libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    app.windows
        .get_mut(&wid)
        .unwrap()
        .active_terminal
        .as_mut()
        .unwrap()
        .sink = Arc::new(SinkWriter::new(pipe[1]));
    pipe
}

/// Silence is EAGAIN on a live, empty observer pipe, and nothing else is. A
/// positive read is the regression these tests guard (bytes reached the
/// PTY). A zero read is EOF, which a pipe whose write end this test still
/// holds cannot answer: one of the observer's descriptors was closed out
/// from under the test (by another thread closing a number it no longer
/// owned), and it is reported as that rather than as emitted bytes.
#[cfg(unix)]
fn assert_pty_silent_and_close(pipe: [i32; 2]) {
    let mut bytes = [0u8; 64];
    let read = unsafe { libc::read(pipe[0], bytes.as_mut_ptr().cast(), bytes.len()) };
    let error = std::io::Error::last_os_error();
    assert_ne!(
        read, 0,
        "the PTY observer read EOF: a descriptor it owns was closed out from under \
         it, so silence was never observed"
    );
    assert!(
        read < 0,
        "host-owned search press/repeat/release emitted PTY bytes: {:?}",
        &bytes[..usize::try_from(read).unwrap_or(0)]
    );
    assert_eq!(
        error.kind(),
        std::io::ErrorKind::WouldBlock,
        "the observer read failed with {error}, not EAGAIN"
    );
    unsafe {
        libc::close(pipe[0]);
        libc::close(pipe[1]);
    }
}

/// Normal-shell route through the shipping physical-owner seams: Cmd-S opens,
/// typing searches, a held key repeats locally, Cmd-R reverses and wraps, and both
/// releases are swallowed even with Kitty event-type reporting enabled.
#[cfg(unix)]
#[test]
fn normal_terminal_cmd_s_r_press_repeat_release_emit_zero_kitty_bytes() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let pipe = observe_pty(&mut app, wid);
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    term_lock(&term).process(b"\x1b[>2uhit one\r\nhit two");

    let cmd_s = PhysicalKey::Code(KeyCode::KeyS);
    app.terminal_emacs_search_pressed(wid, cmd_s, true);
    assert!(matches!(
        app.physical_press_owners.get(&cmd_s),
        Some(PhysicalPressOwner::Local { .. })
    ));
    app.apply_search_repeat_action(
        wid,
        SearchRepeatAction::Edit(crate::app_search::SearchEdit::Insert("hit".into())),
    );
    assert_eq!(app.windows[&wid].search.as_ref().unwrap().current, 0);
    app.route_physical_repeat(wid, cmd_s);
    assert_eq!(app.windows[&wid].search.as_ref().unwrap().current, 1);
    app.release_physical_press(wid, cmd_s);
    assert!(!app.physical_press_owners.contains_key(&cmd_s));

    let cmd_r = PhysicalKey::Code(KeyCode::KeyR);
    app.terminal_emacs_search_pressed(wid, cmd_r, false);
    assert_eq!(app.windows[&wid].search.as_ref().unwrap().current, 0);
    app.route_physical_repeat(wid, cmd_r);
    assert_eq!(
        app.windows[&wid].search.as_ref().unwrap().current,
        1,
        "backward repeat wraps first → last"
    );
    app.release_physical_press(wid, cmd_r);
    assert_pty_silent_and_close(pipe);
}

/// Codex's protected-footer mutation may force a search recompute, but the local
/// physical owner remains authoritative across the refresh: reverse direction and
/// last-match selection survive, while Kitty observes no press/repeat/release.
#[cfg(unix)]
#[test]
fn codex_footer_refresh_during_cmd_r_hold_is_directional_and_pty_silent() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let rows = app.windows[&wid].rows;
    let pipe = observe_pty(&mut app, wid);
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    term_lock(&term).process(b"\x1b[>2u");
    term_lock(&term)
        .process(format!("\x1b[{};1HCODEX_NEEDLE\x1b[{rows};1HCODEX_NEEDLE", rows - 1).as_bytes());

    let cmd_r = PhysicalKey::Code(KeyCode::KeyR);
    app.terminal_emacs_search_pressed(wid, cmd_r, false);
    app.apply_search_repeat_action(
        wid,
        SearchRepeatAction::Edit(crate::app_search::SearchEdit::Insert("CODEX_NEEDLE".into())),
    );
    assert_eq!(app.windows[&wid].search.as_ref().unwrap().current, 1);

    let region_bottom = rows - 2;
    term_lock(&term)
        .process(format!("\x1b[1;{region_bottom}r\x1b[{region_bottom};1H\r\nX\x1b[r").as_bytes());
    app.search_refresh_for_output(0);
    let refreshed = app.windows[&wid].search.as_ref().unwrap();
    assert_eq!(refreshed.current, refreshed.matches.len() - 1);
    app.route_physical_repeat(wid, cmd_r);
    app.release_physical_press(wid, cmd_r);
    assert_pty_silent_and_close(pipe);
}

/// Claude classic and alternate-screen layouts both remain behind the same host
/// intercept. Each initial Cmd-R query searches only the active grid, captures its
/// repeat locally, and retires without a Kitty byte.
#[cfg(unix)]
#[test]
fn claude_classic_and_alt_cmd_r_route_active_grid_and_emit_zero_kitty_bytes() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let pipe = observe_pty(&mut app, wid);
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    term_lock(&term).process(b"\x1b[>2uCLAUDE_CLASSIC\r\nCLAUDE_CLASSIC");

    let classic_key = PhysicalKey::Code(KeyCode::KeyR);
    app.terminal_emacs_search_pressed(wid, classic_key, false);
    app.apply_search_repeat_action(
        wid,
        SearchRepeatAction::Edit(crate::app_search::SearchEdit::Insert(
            "CLAUDE_CLASSIC".into(),
        )),
    );
    assert_eq!(app.windows[&wid].search.as_ref().unwrap().current, 1);
    app.release_physical_press(wid, classic_key);
    app.search_cancel();

    term_lock(&term).process(b"\x1b[?1049h\x1b[HCLAUDE_ALT\r\nCLAUDE_ALT");
    let alt_key = PhysicalKey::Code(KeyCode::KeyR);
    app.terminal_emacs_search_pressed(wid, alt_key, false);
    app.apply_search_repeat_action(
        wid,
        SearchRepeatAction::Edit(crate::app_search::SearchEdit::Insert("CLAUDE_ALT".into())),
    );
    let alternate = app.windows[&wid].search.as_ref().unwrap();
    assert_eq!(alternate.matches.len(), 2);
    assert_eq!(alternate.current, 1);
    app.route_physical_repeat(wid, alt_key);
    app.release_physical_press(wid, alt_key);
    assert_pty_silent_and_close(pipe);
}

#[cfg(unix)]
#[test]
fn accepting_a_live_composer_match_does_not_write_to_its_program() {
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::sync::Arc;

    let mut pipe = [-1; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    // Own both descriptors before any later assertion can unwind. The sink
    // borrows the write end and never closes it.
    let _reader = unsafe { OwnedFd::from_raw_fd(pipe[0]) };
    let _writer = unsafe { OwnedFd::from_raw_fd(pipe[1]) };
    let flags = unsafe { libc::fcntl(pipe[0], libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(pipe[0], libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.windows
        .get_mut(&wid)
        .unwrap()
        .active_terminal
        .as_mut()
        .unwrap()
        .sink = Arc::new(aterm_session::sink::SinkWriter::new(pipe[1]));
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    term_lock(&term).process(
        "────────────────────\r\n> alpha beta\r\n  gamma delta\r\n────────────────────\x1b[3;14H"
            .as_bytes(),
    );
    let cursor = term_lock(&term).cursor();
    assert_eq!((cursor.row, cursor.col), (2, 13));
    let ctrl_r = PhysicalKey::Code(KeyCode::KeyR);
    app.terminal_emacs_search_pressed(wid, ctrl_r, false);
    app.apply_search_repeat_action(
        wid,
        SearchRepeatAction::Edit(crate::app_search::SearchEdit::Insert("beta".into())),
    );
    app.release_physical_press(wid, ctrl_r);
    assert_eq!(app.windows[&wid].search.as_ref().unwrap().matches.len(), 1);
    app.find_cmd(crate::app_search::FindAction::Accept);
    assert!(app.windows[&wid].search.is_none());
    let terminal = term_lock(&term);
    assert!(terminal.text_selection().has_selection());
    assert_eq!(
        (terminal.cursor().row, terminal.cursor().col),
        (cursor.row, cursor.col)
    );
    drop(terminal);
    let mut byte = [0u8; 1];
    let read = unsafe { libc::read(pipe[0], byte.as_mut_ptr().cast(), byte.len()) };
    assert_eq!(read, -1);
    assert_eq!(
        std::io::Error::last_os_error().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

/// The group a shell leads in front of the fixture's PTY, and the group an
/// agent leads. Neither leads a process or is a real fd: the fixture's master
/// carries the number and the injected probe answers it (`named_front`).
#[cfg(unix)]
const SHELL_GROUP: i32 = i32::MAX - 1;
#[cfg(unix)]
const AGENT_GROUP: i32 = i32::MAX - 2;

/// Put `program` in front of `wid`'s session: `fresh` is the PTY's foreground
/// group as a fresh read finds it, `named` the group the status sweep last
/// named `program` for (`status program=`).
#[cfg(unix)]
fn named_front(app: &mut App, wid: WindowId, fresh: i32, named: i32, program: &str) {
    let session = app.front_terminal(wid).expect("terminal").session;
    app.session_factory.fg_probe = |master| master;
    app.pool.sessions.get_mut(&session).unwrap().session.master = fresh;
    let ctx = app.pool.get(session).unwrap().ctx.clone();
    let mut timeline = ctx.timeline.lock().unwrap();
    timeline.note_foreground_group(named);
    timeline.set_program(named, Some(program.into()));
}

#[cfg(unix)]
fn set_mods(app: &mut App, wid: WindowId, mods: ModifiersState) {
    app.windows.get_mut(&wid).expect("window").mods = mods;
}

/// A physical key event as the platform delivers it: `logical` and `text` are
/// the layout's character, `base` the key without modifiers.
#[cfg(unix)]
fn key_event(
    code: KeyCode,
    logical: &str,
    base: &str,
    state: aterm_window::event::ElementState,
    repeat: bool,
) -> aterm_window::event::KeyEvent {
    use aterm_window::keyboard::{KeyLocation, SmolStr};
    let pressed = state == aterm_window::event::ElementState::Pressed;
    aterm_window::event::KeyEvent::synthetic_for_test_with_base(
        PhysicalKey::Code(code),
        character(logical),
        character(base),
        pressed.then(|| SmolStr::new(logical)),
        KeyLocation::Standard,
        state,
        repeat,
    )
}

/// Press, auto-repeat and release of the US key `letter` through the shipping
/// `on_key` under the window's current modifiers.
#[cfg(unix)]
fn press(app: &mut App, wid: WindowId, code: KeyCode, letter: &str) {
    use aterm_window::event::ElementState;
    app.on_key(
        wid,
        key_event(code, letter, letter, ElementState::Pressed, false),
    );
}

#[cfg(unix)]
fn repeat(app: &mut App, wid: WindowId, code: KeyCode, letter: &str) {
    use aterm_window::event::ElementState;
    app.on_key(
        wid,
        key_event(code, letter, letter, ElementState::Pressed, true),
    );
}

#[cfg(unix)]
fn release(app: &mut App, wid: WindowId, code: KeyCode, letter: &str) {
    use aterm_window::event::ElementState;
    app.on_key(
        wid,
        key_event(code, letter, letter, ElementState::Released, false),
    );
}

/// An App whose one session writes to an observer pipe: the press path and the
/// press-owner's repeat and release path share that session's sink. The read end
/// is nonblocking; the sink owns the write end.
#[cfg(unix)]
fn app_observing_pty() -> (App, std::os::fd::OwnedFd) {
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::sync::Arc;

    let mut pipe = [-1; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    let reader = unsafe { OwnedFd::from_raw_fd(pipe[0]) };
    let writer = unsafe { OwnedFd::from_raw_fd(pipe[1]) };
    let flags = unsafe { libc::fcntl(pipe[0], libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(pipe[0], libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    let sink = Arc::new(aterm_session::sink::SinkWriter::new_owned(writer));
    (App::headless_for_test_with_sink(sink), reader)
}

/// Everything the program has been sent since the last drain; empty is
/// silence, an EAGAIN on the live pipe and nothing else.
#[cfg(unix)]
fn drain(reader: &std::os::fd::OwnedFd) -> Vec<u8> {
    use std::os::fd::AsRawFd;

    let mut out = Vec::new();
    let mut bytes = [0u8; 256];
    loop {
        let read =
            unsafe { libc::read(reader.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len()) };
        let Ok(read) = usize::try_from(read) else {
            let error = std::io::Error::last_os_error();
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::WouldBlock,
                "the observer read failed with {error}, not EAGAIN"
            );
            return out;
        };
        assert_ne!(read, 0, "the PTY observer read EOF");
        out.extend_from_slice(&bytes[..read]);
    }
}

/// The owner's ask (2026-10-20): "control-r for bash should prioritize the
/// emacs-style navigation". At a bash prompt a bare Ctrl-R and Ctrl-S are
/// readline's reverse-i-search and forward-search-history: 0x12 and 0x13 reach
/// the program, a held Ctrl-R steps the search with each repeat, the press,
/// repeat and release follow the press to the program under Kitty event-type
/// reporting, and aterm's find bar never opens.
#[cfg(unix)]
#[test]
fn bare_ctrl_r_and_ctrl_s_at_a_shell_prompt_reach_the_shell_and_open_no_find_bar() {
    let (mut app, pty) = app_observing_pty();
    let wid = WindowId(0);
    named_front(&mut app, wid, SHELL_GROUP, SHELL_GROUP, "bash");
    assert!(!app.front_agent_composer(wid), "bash owns its prompt");
    set_mods(&mut app, wid, ModifiersState::CONTROL);

    press(&mut app, wid, KeyCode::KeyR, "r");
    assert_eq!(drain(&pty), [0x12], "C-r is readline's reverse-i-search");
    repeat(&mut app, wid, KeyCode::KeyR, "r");
    assert_eq!(drain(&pty), [0x12], "a held C-r steps the search");
    release(&mut app, wid, KeyCode::KeyR, "r");
    assert_eq!(drain(&pty), [], "a legacy release writes nothing");
    press(&mut app, wid, KeyCode::KeyS, "s");
    assert_eq!(drain(&pty), [0x13], "C-s is the program's (search or XOFF)");
    release(&mut app, wid, KeyCode::KeyS, "s");
    assert!(app.windows[&wid].search.is_none(), "no find bar opened");

    let term = app.front_terminal(wid).expect("terminal").term.clone();
    term_lock(&term).process(b"\x1b[>3u");
    press(&mut app, wid, KeyCode::KeyR, "r");
    assert_eq!(drain(&pty), b"\x1b[114;5u", "Kitty press");
    repeat(&mut app, wid, KeyCode::KeyR, "r");
    assert_eq!(drain(&pty), b"\x1b[114;5:2u", "Kitty repeat");
    release(&mut app, wid, KeyCode::KeyR, "r");
    assert_eq!(drain(&pty), b"\x1b[114;5:3u", "Kitty release");
    assert!(app.windows[&wid].search.is_none(), "no find bar opened");
    assert_eq!(drain(&pty), [], "nothing more reached the program");
}

/// In an agent's composer a bare Ctrl-R stays aterm's find, as 10d208372f made
/// it: the bar opens searching backward, a held Ctrl-R repeats the search
/// locally, and Claude sees no press, repeat or release byte even under Kitty
/// event-type reporting.
#[cfg(unix)]
#[test]
fn bare_ctrl_r_in_an_agent_composer_opens_find_and_writes_nothing() {
    let (mut app, pty) = app_observing_pty();
    let wid = WindowId(0);
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    term_lock(&term).process(b"\x1b[>3uCLAUDE_NEEDLE\r\nCLAUDE_NEEDLE\r\n> ");
    named_front(&mut app, wid, AGENT_GROUP, AGENT_GROUP, "claude");
    assert!(app.front_agent_composer(wid), "Claude owns its composer");
    set_mods(&mut app, wid, ModifiersState::CONTROL);

    press(&mut app, wid, KeyCode::KeyR, "r");
    assert!(
        app.windows[&wid].search.is_some(),
        "C-r opened aterm's find"
    );
    assert!(matches!(
        app.physical_press_owners
            .get(&PhysicalKey::Code(KeyCode::KeyR)),
        Some(PhysicalPressOwner::Local { .. })
    ));
    app.apply_search_repeat_action(
        wid,
        SearchRepeatAction::Edit(crate::app_search::SearchEdit::Insert(
            "CLAUDE_NEEDLE".into(),
        )),
    );
    let before = app.windows[&wid].search.as_ref().unwrap().current;
    repeat(&mut app, wid, KeyCode::KeyR, "r");
    assert_ne!(
        app.windows[&wid].search.as_ref().unwrap().current,
        before,
        "a held C-r repeats the search"
    );
    release(&mut app, wid, KeyCode::KeyR, "r");
    assert_eq!(drain(&pty), [], "nothing more reached the program");
}

/// Negative control on the agent signal: the status sweep still names Claude
/// for the group that has left the front (it exited to its shell, which a
/// fresh read finds in front). The publication speaks for nobody, so the shell
/// gets its C-r and no find bar opens.
#[cfg(unix)]
#[test]
fn a_departed_agents_publication_leaves_ctrl_r_to_the_shell_in_front() {
    let (mut app, pty) = app_observing_pty();
    let wid = WindowId(0);
    named_front(&mut app, wid, SHELL_GROUP, AGENT_GROUP, "claude");
    assert!(
        !app.front_agent_composer(wid),
        "the agent's group left the front"
    );
    set_mods(&mut app, wid, ModifiersState::CONTROL);
    press(&mut app, wid, KeyCode::KeyR, "r");
    assert_eq!(drain(&pty), [0x12]);
    release(&mut app, wid, KeyCode::KeyR, "r");
    assert!(app.windows[&wid].search.is_none());
    assert_eq!(drain(&pty), [], "nothing more reached the program");
}

/// The review of 2026-10-20: Claude Code runs `$EDITOR` with inherited stdio
/// and no group of its own, so while vim reads the keys the group in front
/// and its leader's name are still Claude's. The agent has handed the
/// terminal off (`SessionFactory::handed_terminal`, asked of the agent's
/// group), so vim gets its C-r (redo) and its repeat, and no find bar opens.
/// NEGATIVE CONTROL: once the editor has exited the same press is aterm's
/// find again.
#[cfg(unix)]
#[test]
fn an_editor_the_agent_handed_the_terminal_to_gets_ctrl_r() {
    let (mut app, pty) = app_observing_pty();
    let wid = WindowId(0);
    named_front(&mut app, wid, AGENT_GROUP, AGENT_GROUP, "claude");
    app.session_factory.handed_terminal = |group| {
        assert_eq!(group, AGENT_GROUP, "asked of the agent's group");
        true
    };
    assert!(!app.front_agent_composer(wid), "vim reads the keys");
    set_mods(&mut app, wid, ModifiersState::CONTROL);
    press(&mut app, wid, KeyCode::KeyR, "r");
    assert_eq!(drain(&pty), [0x12], "C-r is vim's redo");
    repeat(&mut app, wid, KeyCode::KeyR, "r");
    assert_eq!(drain(&pty), [0x12], "a held C-r follows it");
    release(&mut app, wid, KeyCode::KeyR, "r");
    assert!(app.windows[&wid].search.is_none(), "no find bar opened");
    assert_eq!(drain(&pty), [], "nothing more reached the program");

    app.session_factory.handed_terminal = |_| false;
    press(&mut app, wid, KeyCode::KeyR, "r");
    assert!(
        app.windows[&wid].search.is_some(),
        "back in Claude's composer, C-r is aterm's find"
    );
    release(&mut app, wid, KeyCode::KeyR, "r");
    assert_eq!(drain(&pty), [], "the composer saw nothing");
}

/// The members that take the terminal from the agent leading their group are
/// any program but an agent or a runtime one runs as: a launcher's agent child
/// is the agent itself. NEGATIVE CONTROLS: each agent name and runtime.
#[test]
fn any_program_but_an_agent_or_its_runtime_takes_the_terminal_off_it() {
    for editor in ["vim", "nvim", "nano", "emacs", "hx", "sh", "less", "-zsh"] {
        assert!(super::hands_the_terminal_off(editor), "{editor}");
    }
    for agent in [
        "claude", "Claude", "codex", "agy", "muse", "gemini", "node", "bun", "deno",
    ] {
        assert!(!super::hands_the_terminal_off(agent), "{agent}");
    }
}

/// Where no foreground group can be read (a Windows ConPTY's probe always
/// answers `-1`, and its sweep names no group) the agent publication stands
/// alone: an identified agent keeps Ctrl-R for aterm's find, a shell does not.
#[cfg(unix)]
#[test]
fn with_no_foreground_group_to_read_the_agent_publication_stands_alone() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    named_front(&mut app, wid, SHELL_GROUP, 0, "claude");
    app.session_factory.fg_probe = |_| -1;
    assert!(
        app.front_agent_composer(wid),
        "Claude, named under no group"
    );
    named_front(&mut app, wid, SHELL_GROUP, 0, "pwsh");
    app.session_factory.fg_probe = |_| -1;
    assert!(
        !app.front_agent_composer(wid),
        "a shell, named under no group"
    );
}

/// At a shell aterm's find stays one chord away: Cmd-R on macOS, the seeded
/// `ctrl+shift+f` elsewhere (`HARDCODED_SUPER_CHORDS` is off on Linux). Once
/// the bar is open a bare Ctrl-R is its isearch-repeat, and the shell behind
/// it sees nothing.
#[cfg(unix)]
#[test]
fn at_a_shell_the_platform_find_chord_opens_find_and_ctrl_r_then_steps_it() {
    let (mut app, pty) = app_observing_pty();
    app.keybindings = crate::keybinding::Keybindings::platform_defaults();
    let wid = WindowId(0);
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    term_lock(&term).process(b"\x1b[>3uhit one\r\nhit two\r\n$ ");
    named_front(&mut app, wid, SHELL_GROUP, SHELL_GROUP, "zsh");

    #[cfg(target_os = "macos")]
    let (mods, code, logical, base) = (ModifiersState::SUPER, KeyCode::KeyR, "r", "r");
    #[cfg(not(target_os = "macos"))]
    let (mods, code, logical, base) = (
        ModifiersState::CONTROL | ModifiersState::SHIFT,
        KeyCode::KeyF,
        "F",
        "f",
    );
    {
        use aterm_window::event::ElementState;
        set_mods(&mut app, wid, mods);
        app.on_key(
            wid,
            key_event(code, logical, base, ElementState::Pressed, false),
        );
        assert!(
            app.windows[&wid].search.is_some(),
            "the find chord opened it"
        );
        app.on_key(
            wid,
            key_event(code, logical, base, ElementState::Released, false),
        );
    }
    app.apply_search_repeat_action(
        wid,
        SearchRepeatAction::Edit(crate::app_search::SearchEdit::Insert("hit".into())),
    );
    let before = app.windows[&wid].search.as_ref().unwrap().current;
    set_mods(&mut app, wid, ModifiersState::CONTROL);
    press(&mut app, wid, KeyCode::KeyR, "r");
    assert_ne!(
        app.windows[&wid].search.as_ref().unwrap().current,
        before,
        "C-r steps the open find"
    );
    release(&mut app, wid, KeyCode::KeyR, "r");
    assert_eq!(drain(&pty), [], "nothing more reached the program");
}
