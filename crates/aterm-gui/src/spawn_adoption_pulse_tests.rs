// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::adoption_needs_size_pulse;

#[test]
fn a_repaint_adoption_pulses_on_either_screen_and_an_exact_shell_does_not() {
    let main = aterm_core::terminal::Terminal::new(24, 80)
        .checkpoint_carry(0)
        .expect("a fresh engine is Ground");
    let alt = {
        let mut terminal = aterm_core::terminal::Terminal::new(24, 80);
        terminal.process(b"\x1b[?1049h");
        terminal
            .checkpoint_carry(0)
            .expect("Ground after the DECSET")
    };
    assert!(
        !adoption_needs_size_pulse(Some(&main), false),
        "an exact main-screen shell keeps its screen"
    );
    assert!(
        adoption_needs_size_pulse(Some(&alt), false),
        "an alt-screen TUI is told to redraw, as before"
    );
    assert!(
        adoption_needs_size_pulse(Some(&main), true),
        "a blank main screen is repainted"
    );
    assert!(
        adoption_needs_size_pulse(None, true),
        "and so is a session adopted onto a blank engine with no checkpoint at all"
    );
}

#[cfg(unix)]
#[test]
fn the_adoption_pulse_ends_on_the_engines_grid_not_the_windows() {
    let (_master_fd, _slave_fd, master, slave) = open_pty_pair();
    let pane = {
        let mut terminal = aterm_core::terminal::Terminal::new(24, 39);
        terminal.process(b"\x1b[?1049hvim");
        terminal.checkpoint_carry(0).expect("Ground")
    };
    let term = std::sync::Arc::new(std::sync::Mutex::new(aterm_core::terminal::Terminal::new(
        24, 80,
    )));
    term.lock().expect("engine").restore_checkpoint(&pane);
    aterm_pty::resize_with_cell_px(master, 24, 80, Some((8, 16)));
    let holding = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    super::pulse_adopted_pty(
        master,
        &term,
        &holding,
        Some((8, 16)),
        super::PulseHold::BackToBack,
    );
    assert_eq!(
        slave_winsize(slave),
        (24, 39, 39 * 8, 24 * 16),
        "an exact restore's pulse is back to back, at the engine's grid"
    );
    assert!(!holding.load(std::sync::atomic::Ordering::Acquire));
    aterm_pty::resize_with_cell_px(master, 24, 80, Some((8, 16)));
    let pulsed = std::time::Instant::now();
    super::pulse_adopted_pty(
        master,
        &term,
        &holding,
        Some((8, 16)),
        super::PulseHold::Held,
    );
    let deadline = pulsed + std::time::Duration::from_secs(60);
    let ended = loop {
        let ws = slave_winsize(slave);
        if ws.0 == 24 || std::time::Instant::now() > deadline {
            break ws;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    drop(term.lock().expect("engine"));
    assert!(
        !holding.load(std::sync::atomic::Ordering::Acquire),
        "the hold is lowered when the pulse ends"
    );
    assert_eq!(
        ended,
        (24, 39, 39 * 8, 24 * 16),
        "the program sees the pane it is drawn in"
    );
}

#[cfg(unix)]
#[test]
fn the_adoption_pulse_holds_the_transient_size_for_the_program_to_read() {
    let (_master_fd, _slave_fd, master, slave) = open_pty_pair();
    let alt = {
        let mut terminal = aterm_core::terminal::Terminal::new(30, 100);
        terminal.process(b"\x1b[?1049hclaude");
        terminal.checkpoint_carry(0).expect("Ground")
    };
    let term = std::sync::Arc::new(std::sync::Mutex::new(aterm_core::terminal::Terminal::new(
        30, 100,
    )));
    term.lock().expect("engine").restore_checkpoint(&alt);
    aterm_pty::resize_with_cell_px(master, 30, 100, Some((8, 16)));
    let cached = slave_winsize(slave);
    let holding = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    let mut held = None;
    super::pulse_adopted_pty_with(master, &term, &holding, Some((8, 16)), |restore| {
        held = Some(restore);
    });
    assert!(
        holding.load(std::sync::atomic::Ordering::Acquire),
        "the session's hold stands between the halves"
    );
    let read = slave_winsize(slave);
    assert_ne!(
        (read.0, read.1),
        (cached.0, cached.1),
        "the program must read a CHANGED size or it never repaints"
    );
    assert_eq!((read.0, read.1), (29, 100), "the transient is rows-1");
    {
        let engine = term.lock().expect("engine");
        assert_eq!(
            (engine.rows(), engine.cols()),
            (30, 100),
            "only the PTY carries the transient; the engine keeps its grid"
        );
    }
    held.take()
        .expect("the second half is handed to the hold")
        .run();
    assert_eq!(
        slave_winsize(slave),
        (30, 100, 800, 480),
        "the pulse ends on the engine's grid"
    );
    assert!(
        !holding.load(std::sync::atomic::Ordering::Acquire),
        "and lowers the hold"
    );

    let mut held = None;
    super::pulse_adopted_pty_with(master, &term, &holding, Some((8, 16)), |restore| {
        held = Some(restore);
    });
    term.lock().expect("engine").resize(40, 120);
    aterm_pty::resize_with_cell_px(master, 40, 120, Some((8, 16)));
    held.take().expect("held").run();
    assert_eq!(
        (slave_winsize(slave).0, slave_winsize(slave).1),
        (40, 120),
        "the second half writes the engine's grid as it is then"
    );

    super::pulse_adopted_pty_with(master, &term, &holding, Some((8, 16)), drop);
    assert_eq!((slave_winsize(slave).0, slave_winsize(slave).1), (40, 120));
    assert!(!holding.load(std::sync::atomic::Ordering::Acquire));
}

#[cfg(unix)]
fn open_pty_pair() -> (std::os::fd::OwnedFd, std::os::fd::OwnedFd, i32, i32) {
    use std::os::fd::FromRawFd;
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    // SAFETY: both fds were just opened above and are owned here.
    let (m, s) = unsafe {
        (
            std::os::fd::OwnedFd::from_raw_fd(master),
            std::os::fd::OwnedFd::from_raw_fd(slave),
        )
    };
    (m, s, master, slave)
}

#[cfg(unix)]
fn slave_winsize(slave: i32) -> (u16, u16, u16, u16) {
    let mut ws = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `slave` is open; `ws` is a valid out-param for TIOCGWINSZ.
    assert_eq!(unsafe { libc::ioctl(slave, libc::TIOCGWINSZ, &mut ws) }, 0);
    (ws.ws_row, ws.ws_col, ws.ws_xpixel, ws.ws_ypixel)
}
