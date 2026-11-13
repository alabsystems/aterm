// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::Cell;

const ROWS: usize = 6;

/// A Windows tab as the two sides of a pseudoconsole see it: OUR engine,
/// and a model of the console host's own buffers.
struct Tab {
    ours: Terminal,
    host: Terminal,
    host_kind: Option<ConsoleHostKind>,
    clients: u32,
    /// The shell reads raw (PSReadLine), not in line mode (cmd.exe).
    raw_reader: bool,
    /// Where the shell's line editor anchored its line: the host's cursor
    /// once its last prompt was drawn, read before any resync.
    anchor: (u16, u16),
}

impl Tab {
    fn new(host_kind: Option<ConsoleHostKind>) -> Self {
        let mut tab = Self::bare(host_kind, false);
        tab.shell(b"\x1b]133;A\x07C:\\> \x1b]133;B\x07less file\r\n\x1b]133;C\x07");
        tab.app(b"\x1b[?1049h\x1b[H\x1b[2Jpager 1\r\npager 2\r\n:");
        tab
    }

    fn bare(host_kind: Option<ConsoleHostKind>, raw_reader: bool) -> Self {
        let mut ours = Terminal::new(ROWS as u16, 40);
        apply_posture(&mut ours, ResizePolicy::ConPty, host_kind);
        ours.process(b"\x1b[?9001h");
        Self {
            ours,
            host: Terminal::new(ROWS as u16, 40),
            host_kind,
            clients: 0,
            raw_reader,
            anchor: (0, 0),
        }
    }

    /// Bytes the shell wrote: the host draws them without the marks, ours
    /// with them, and a prompt's end anchors the line editor.
    fn shell(&mut self, bytes: &[u8]) {
        self.host.process(&without_marks(bytes));
        self.ours.process(bytes);
        if bytes.windows(7).any(|w| w == b"]133;B\x07") {
            let c = self.host.cursor();
            self.anchor = (c.row, c.col);
        }
        if self.ours.take_alt_screen_resync_request() {
            let host_kind = self.host_kind;
            let mut forwarded = None;
            assert!(start_with(7, carry_of(&self.ours), |master, carry| {
                assert_eq!(master, 7, "the session's own host");
                assert!(host_resyncs(host_kind), "only a forwarding host is asked");
                forwarded = Some(carry);
                Ok(())
            }));
            self.clients += 1;
            if let Some(carry) = forwarded {
                let mut leave = b"\x1b[?1049l".to_vec();
                let host = self.host.cursor();
                if let Some((cursor, text)) = carry
                    && self.raw_reader
                    && cursor == (host.row, host.col)
                {
                    leave.extend_from_slice(text.as_bytes());
                }
                self.app(&leave);
            }
        }
    }

    /// The line editor redraws its line from its anchor, as PSReadLine does
    /// on every key.
    fn type_line(&mut self, text: &str) {
        let (row, col) = self.anchor;
        self.app(format!("\x1b[{};{}H{text}", row + 1, col + 1).as_bytes());
    }

    /// Bytes an app wrote: both sides draw them alike.
    fn app(&mut self, bytes: &[u8]) {
        self.host.process(bytes);
        self.ours.process(bytes);
    }

    fn rows(t: &Terminal) -> Vec<String> {
        (0..ROWS)
            .map(|r| t.row_text(r).unwrap_or_default().trim_end().to_string())
            .collect()
    }

    /// Whether our screen shows what the host's does, cursor included.
    fn in_step(&self) -> Result<(), String> {
        let (a, b) = (self.ours.cursor(), self.host.cursor());
        let (ours, host) = (Self::rows(&self.ours), Self::rows(&self.host));
        if ours == host
            && (a.row, a.col) == (b.row, b.col)
            && self.ours.is_alternate_screen() == self.host.is_alternate_screen()
        {
            Ok(())
        } else {
            Err(format!(
                "ours {ours:?} at {:?} alt={} vs host {host:?} at {:?} alt={}",
                (a.row, a.col),
                self.ours.is_alternate_screen(),
                (b.row, b.col),
                self.host.is_alternate_screen()
            ))
        }
    }
}

/// The bytes without their OSC 133 marks (`ESC ] 133 ; … BEL`).
fn without_marks(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"\x1b]133;") {
            i += bytes[i..]
                .iter()
                .position(|&b| b == 0x07)
                .map_or(bytes.len(), |p| p + 1);
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// The audit's pager run, in bytes: the pager is killed (no `?1049l`),
/// the shell prompts, a command runs, a second pager starts and quits
/// normally, and the shell prompts again.
fn killed_pager_then_second_pager(tab: &mut Tab) {
    tab.shell(b"\r\n\x1b]133;D;1\x07\x1b]133;A\x07C:\\> \x1b]133;B\x07");
    tab.shell(b"echo x2\r\n\x1b]133;C\x07x2\r\n\x1b]133;D;0\x07\x1b]133;A\x07C:\\> \x1b]133;B\x07");
    tab.shell(b"less file\r\n\x1b]133;C\x07");
    tab.app(b"\x1b[?1049h\x1b[H\x1b[2Jpager 1 again\r\n:");
    tab.app(b"\x1b[?1049l");
    tab.shell(b"\x1b]133;D;0\x07\x1b]133;A\x07C:\\> \x1b]133;B\x07echo AFTER\r\n");
}

#[test]
fn under_openconsole_a_killed_pager_leaves_both_screens_in_step() {
    let mut tab = Tab::new(Some(ConsoleHostKind::OpenConsole));
    killed_pager_then_second_pager(&mut tab);
    assert_eq!(tab.clients, 1, "one resync for the one stuck screen");
    tab.in_step().unwrap();
    assert!(!tab.ours.is_alternate_screen());
    assert_eq!(
        Tab::rows(&tab.ours)[0],
        "C:\\> less file",
        "the host's main buffer"
    );
}

/// A pwsh tab under OpenConsole: two commands' rows, then vim on the
/// alternate screen with its cursor on its top row, killed there, and
/// pwsh's next prompt drawn on the dead screen (no newline before it).
fn pwsh_after_a_killed_vim(raw_reader: bool) -> Tab {
    let mut tab = Tab::bare(Some(ConsoleHostKind::OpenConsole), raw_reader);
    tab.shell(b"\x1b]133;A\x07PS> \x1b]133;B\x07echo one\r\n\x1b]133;C\x07one\r\n");
    tab.shell(b"\x1b]133;D;0\x07\x1b]133;A\x07PS> \x1b]133;B\x07vim f\r\n\x1b]133;C\x07");
    tab.app(b"\x1b[?1049h\x1b[H\x1b[2Jline 1\r\nline 2\r\nline 3\x1b[H");
    tab.shell(b"\x1b]133;D;-1\x07\x1b]133;A\x07PS> \x1b]133;B\x07");
    tab
}

#[test]
fn a_raw_line_editor_finds_its_prompt_where_it_anchored_it() {
    let mut tab = pwsh_after_a_killed_vim(true);
    assert_eq!(tab.anchor, (0, 4), "anchored on the dead screen's top row");
    tab.type_line("echo after");
    tab.in_step().unwrap();
    let rows = Tab::rows(&tab.ours);
    assert_eq!(rows[0], "PS> echo after");
    assert!(rows[1..].iter().all(String::is_empty), "{rows:?}");
    let grid = tab.ours.grid();
    let history: Vec<String> = (0..grid.scrollback_lines())
        .map(|i| {
            grid.get_history_line(i)
                .as_ref()
                .and_then(|l| l.as_str())
                .map(|s| s.trim_end().to_string())
                .unwrap_or_default()
        })
        .collect();
    assert_eq!(history, ["PS> echo one", "one", "PS> vim f"]);

    let mut plain = pwsh_after_a_killed_vim(false);
    plain.type_line("echo after");
    plain.in_step().unwrap();
    assert_eq!(
        Tab::rows(&plain.ours)[..4],
        ["PS> echo after", "one", "PS> vim f", ""],
        "the plain leave's overwrite"
    );
    assert_eq!(plain.ours.grid().scrollback_lines(), 0);
}

/// A leave client that did not start, or that waits on the session's running one, gives the
/// request back: the next prompt on the stuck screen asks again, and the local leave never runs.
#[test]
fn a_client_that_did_not_start_gives_the_request_back() {
    for kind in [io::ErrorKind::ResourceBusy, io::ErrorKind::NotFound] {
        let term = Mutex::new(Terminal::new(ROWS as u16, 40));
        {
            let mut t = crate::term_lock(&term);
            apply_posture(
                &mut t,
                ResizePolicy::ConPty,
                Some(ConsoleHostKind::OpenConsole),
            );
            t.process(b"\x1b[?1049hpager\r\n\x1b]133;A\x07C:\\> ");
            assert!(t.take_alt_screen_resync_request(), "{kind:?}");
        }
        start_on(&term, 7, |_, _| Err(io::Error::from(kind)));
        let mut t = crate::term_lock(&term);
        t.process(b"\x1b]133;A\x07C:\\> ");
        assert!(t.is_alternate_screen(), "{kind:?}: no local leave");
        assert!(t.take_alt_screen_resync_request(), "{kind:?}: asked again");
    }
    let term = Mutex::new(Terminal::new(ROWS as u16, 40));
    {
        let mut t = crate::term_lock(&term);
        apply_posture(
            &mut t,
            ResizePolicy::ConPty,
            Some(ConsoleHostKind::OpenConsole),
        );
        t.process(b"\x1b[?1049hpager\r\n\x1b]133;A\x07C:\\> ");
        assert!(t.take_alt_screen_resync_request());
    }
    start_on(&term, 7, |_, _| Ok(()));
    let mut t = crate::term_lock(&term);
    t.process(b"\x1b]133;A\x07C:\\> ");
    assert!(
        !t.take_alt_screen_resync_request(),
        "a started client keeps the request until the host's leave arrives"
    );
}

#[test]
fn a_required_mainscreen_waits_for_its_actual_host_switch() {
    for age in [Duration::from_secs(3), Duration::from_secs(61)] {
        let expired = age >= aterm_spec::wait::MUST_HAPPEN;
        let term = Mutex::new(Terminal::new(ROWS as u16, 40));
        {
            let mut t = crate::term_lock(&term);
            apply_posture(
                &mut t,
                ResizePolicy::ConPty,
                Some(ConsoleHostKind::OpenConsole),
            );
            t.process(b"\x1b[?1049h\x1b[?1000hpager");
            assert!(t.is_alternate_screen());
        }
        let asked = Cell::new(false);
        let mut observations = 0;
        let reply = mainscreen_with_clock(
            &term,
            Some(ConsoleHostKind::OpenConsole),
            |carry| {
                assert!(
                    carry.is_none(),
                    "a pager without a shell prompt has no prompt carry"
                );
                asked.set(true);
                Ok(())
            },
            MAINSCREEN_HANG,
            || {
                observations += 1;
                if observations == 2 && !expired {
                    crate::term_lock(&term).process(b"\x1b[?1049l");
                }
                Instant::now()
                    - if observations == 1 {
                        age
                    } else {
                        Duration::ZERO
                    }
            },
        );
        assert!(asked.get());
        assert!(
            observations >= 2,
            "the actual first look still found the alternate screen"
        );
        let mut t = crate::term_lock(&term);
        if expired {
            assert!(
                reply
                    .as_ref()
                    .is_some_and(|reply| reply.starts_with("ERR timeout: "))
            );
            assert!(
                t.is_alternate_screen(),
                "a hang detector does not fabricate the host's switch"
            );
            assert_ne!(t.modes().mouse_mode, aterm_core::terminal::MouseMode::None);
            t.process(b"\r\n\x1b]133;A\x07C:\\> ");
            assert!(t.is_alternate_screen(), "there was no local fallback leave");
            assert!(
                t.take_alt_screen_resync_request(),
                "the next prompt can request the missing host leave"
            );
            t.process(b"\x1b[?1049l");
        } else {
            assert_eq!(
                reply.as_deref(),
                Some("OK left=1\n"),
                "a completed host switch is not a timeout"
            );
        }
        assert!(
            !t.is_alternate_screen(),
            "the host's actual switch reached the engine"
        );
        assert_eq!(t.modes().mouse_mode, aterm_core::terminal::MouseMode::None);
    }
}

#[test]
fn mainscreen_waits_on_the_sessions_running_leave_client() {
    let term = Mutex::new(Terminal::new(ROWS as u16, 40));
    crate::term_lock(&term).process(b"\x1b[?1049hpager");
    let reply = mainscreen_with(&term, Some(ConsoleHostKind::OpenConsole), |_| {
        crate::term_lock(&term).process(b"\x1b[?1049l");
        Err(io::Error::from(io::ErrorKind::ResourceBusy))
    });
    assert_eq!(reply.as_deref(), Some("OK left=1\n"));
}

/// A `mainscreen` whose leave never came (the running client had already left an earlier
/// screen) gives the request back, so the next prompt on the stuck screen asks again.
#[test]
fn a_mainscreen_that_times_out_gives_the_request_back() {
    let term = Mutex::new(Terminal::new(ROWS as u16, 40));
    {
        let mut t = crate::term_lock(&term);
        apply_posture(
            &mut t,
            ResizePolicy::ConPty,
            Some(ConsoleHostKind::OpenConsole),
        );
        t.process(b"\x1b[?1049hpager");
    }
    let reply = mainscreen_waiting(
        &term,
        Some(ConsoleHostKind::OpenConsole),
        |_| Err(io::Error::from(io::ErrorKind::ResourceBusy)),
        Duration::ZERO,
    );
    assert!(reply.is_some_and(|r| r.starts_with("ERR timeout: ")));
    let mut t = crate::term_lock(&term);
    t.process(b"\r\n\x1b]133;A\x07C:\\> ");
    assert!(t.is_alternate_screen(), "no local leave");
    assert!(
        t.take_alt_screen_resync_request(),
        "the next prompt asks again"
    );
}

#[test]
fn the_local_leave_under_a_forwarding_host_is_the_desync() {
    let mut tab = Tab::new(None);
    killed_pager_then_second_pager(&mut tab);
    assert_eq!(tab.clients, 0);
    assert!(
        tab.in_step().is_err(),
        "the local leave must desync a host that never left: {:?}",
        Tab::rows(&tab.ours)
    );
}

#[test]
fn conhost_and_hostless_sessions_keep_the_local_leave() {
    for host in [Some(ConsoleHostKind::Inbox), None] {
        let mut t = Terminal::new(ROWS as u16, 40);
        apply_posture(&mut t, ResizePolicy::ConPty, host);
        t.process(b"\x1b[?1049hpager\r\n\x1b]133;A\x07C:\\> ");
        assert!(!t.is_alternate_screen(), "{host:?}: the local leave ran");
        assert!(!t.take_alt_screen_resync_request(), "{host:?}");
    }
    assert!(host_resyncs(Some(ConsoleHostKind::OpenConsole)));
    assert!(!host_resyncs(Some(ConsoleHostKind::Inbox)));
    assert!(!host_resyncs(None));
}

#[test]
fn kp_begin_follows_the_sessions_console_host() {
    use aterm_types::keyboard::{Key, KeyboardMode, Modifiers, NamedKey, encode_key};
    let begin = |mode| encode_key(&Key::Named(NamedKey::NumpadBegin), Modifiers::empty(), mode);
    let record = b"\x1b[12;76;0;1;0;1_\x1b[12;76;0;0;0;1_".to_vec();
    let mut t = Terminal::new(ROWS as u16, 40);
    apply_posture(
        &mut t,
        ResizePolicy::ConPty,
        Some(ConsoleHostKind::OpenConsole),
    );
    t.process(b"\x1b[?9001h");
    assert!(t.keyboard_mode().contains(KeyboardMode::OPENCONSOLE_HOST));
    assert_eq!(begin(t.keyboard_mode()), record);
    assert_eq!(begin(t.mode_mirror().keyboard_mode()), record);
    t.process(b"\x1bc\x1b[?9001h");
    assert_eq!(begin(t.mode_mirror().keyboard_mode()), record, "after RIS");
    for host in [Some(ConsoleHostKind::Inbox), None] {
        apply_posture(&mut t, ResizePolicy::ConPty, host);
        assert!(!t.keyboard_mode().contains(KeyboardMode::OPENCONSOLE_HOST));
        assert_eq!(begin(t.keyboard_mode()), b"\x1b[E", "{host:?}");
        assert_eq!(
            begin(t.mode_mirror().keyboard_mode()),
            b"\x1b[E",
            "{host:?}"
        );
    }
    let mut native = Terminal::new(ROWS as u16, 40);
    apply_posture(
        &mut native,
        ResizePolicy::Native,
        Some(ConsoleHostKind::OpenConsole),
    );
    native.process(b"\x1b[?9001h");
    assert_eq!(begin(native.keyboard_mode()), b"\x1b[E");
}

#[test]
fn the_posture_is_reapplied_both_ways() {
    let mut t = Terminal::new(ROWS as u16, 40);
    apply_posture(
        &mut t,
        ResizePolicy::ConPty,
        Some(ConsoleHostKind::OpenConsole),
    );
    t.process(b"\x1b[?1049hpager\r\n\x1b]133;A\x07C:\\> ");
    assert!(t.is_alternate_screen(), "the host is asked instead");
    apply_posture(&mut t, ResizePolicy::ConPty, Some(ConsoleHostKind::Inbox));
    assert!(
        !t.take_alt_screen_resync_request(),
        "the request went with the posture"
    );
    t.process(b"\x1b[?1049l\x1b[?1049hpager 2\r\n\x1b]133;A\x07C:\\> ");
    assert!(!t.is_alternate_screen(), "the local leave is back");
}

#[test]
fn mainscreen_asks_a_forwarding_host_and_reports_our_screen() {
    let term = Mutex::new(Terminal::new(ROWS as u16, 40));
    let started = Cell::new(false);
    let reply = mainscreen_with(&term, Some(ConsoleHostKind::OpenConsole), |_| {
        started.set(true);
        Ok(())
    });
    assert_eq!(reply.as_deref(), Some("OK left=0\n"));
    assert!(!started.get(), "nothing up, nothing started");

    crate::term_lock(&term).process(b"\x1b[?1049hpager");
    for host in [Some(ConsoleHostKind::Inbox), None] {
        assert_eq!(mainscreen_with(&term, host, |_| panic!("not asked")), None);
    }
    let failed = mainscreen_with(&term, Some(ConsoleHostKind::OpenConsole), |_| {
        Err(io::Error::other("no client"))
    });
    assert_eq!(failed, None, "the engine's leave is the recovery left");
}

#[test]
fn mainscreen_says_when_the_host_never_left() {
    let term = Mutex::new(Terminal::new(ROWS as u16, 40));
    crate::term_lock(&term).process(b"\x1b[?1049h\x1b[?1000hpager");
    let reply = mainscreen_waiting(
        &term,
        Some(ConsoleHostKind::OpenConsole),
        |_| Ok(()),
        Duration::ZERO,
    );
    let reply = reply.expect("a forwarding host answers the verb itself");
    assert!(reply.starts_with("ERR timeout: "), "{reply:?}");
    assert!(
        crate::term_lock(&term).is_alternate_screen(),
        "no local leave"
    );

    crate::term_lock(&term).process(b"\x1b[?1049l");
    let t = crate::term_lock(&term);
    assert!(!t.is_alternate_screen());
    assert_eq!(
        t.modes().mouse_mode,
        aterm_core::terminal::MouseMode::None,
        "the late switch is the dead app's exit"
    );
}

#[test]
fn mainscreen_carries_the_prompt_on_the_dead_screen() {
    let ask = |bytes: &[u8]| {
        let term = Mutex::new(Terminal::new(ROWS as u16, 40));
        {
            let mut t = crate::term_lock(&term);
            t.set_host_resyncs_alt_screen(true);
            t.process(bytes);
        }
        let mut got = None;
        let reply = mainscreen_waiting(
            &term,
            Some(ConsoleHostKind::OpenConsole),
            |carry| {
                got = Some(carry);
                Ok(())
            },
            Duration::ZERO,
        );
        assert!(reply.is_some_and(|r| r.starts_with("ERR timeout: ")));
        got.expect("the client was started")
    };
    let (cursor, text) = ask(b"\x1b[?1049h\x1b[Hline 1\x1b[H\x1b]133;A\x07PS> \x1b]133;B\x07")
        .expect("a prompt to carry");
    assert_eq!(cursor, (0, 4));
    assert!(text.contains("PS> "), "{text:?}");
    assert_eq!(ask(b"\x1b[?1049h\x1b[Hpager"), None);
}
