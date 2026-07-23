// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// END-TO-END proof, headless: spawn a REAL shell in a PTY → its output drives
// the VT engine → the engine grid is rasterized to pixels. This exercises the
// entire terminal pipeline (PTY + engine + renderer) and verifies the result,
// with no display required. If this passes, the terminal works; the window is
// just a different place to put the same framebuffer.

// forkpty/libc PTY plumbing is POSIX-only; the Windows ConPTY twin lives with
// the aterm-pty seam.
#![cfg(unix)]

use std::ptr;

use aterm_core::terminal::Terminal;
use aterm_render::{Renderer, Theme};

#[test]
fn renders_a_real_shell_session_to_pixels() {
    let Some(mut renderer) = Renderer::from_system(16.0, Theme::default()) else {
        eprintln!("SKIP: no system monospace font found");
        return;
    };

    // --- spawn /bin/sh -c "printf 'ATERM_RENDER_OK\n'" in a PTY ---
    let ws = libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let mut master: libc::c_int = -1;
    let pid = unsafe {
        libc::forkpty(
            &mut master,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::addr_of!(ws).cast_mut(),
        )
    };
    assert!(pid >= 0, "forkpty failed");
    if pid == 0 {
        let sh = std::ffi::CString::new("/bin/sh").unwrap();
        let dashc = std::ffi::CString::new("-c").unwrap();
        let cmd = std::ffi::CString::new("printf 'ATERM_RENDER_OK\\n'").unwrap();
        let argv = [sh.as_ptr(), dashc.as_ptr(), cmd.as_ptr(), ptr::null()];
        unsafe {
            libc::execvp(sh.as_ptr(), argv.as_ptr());
            libc::_exit(127);
        }
    }

    // --- parent: read all the shell's output into the engine ---
    let mut term = Terminal::new(24, 80);
    let mut buf = [0u8; 4096];
    let mut total = 0usize;
    loop {
        let n = unsafe { libc::read(master, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n <= 0 {
            break; // EOF when the shell exits
        }
        term.process(&buf[..n as usize]);
        total += n as usize;
        if total > (1 << 20) {
            break;
        }
    }
    unsafe {
        libc::close(master);
        let mut st = 0;
        libc::waitpid(pid, &mut st, 0);
    }

    // --- the engine model captured the real output ---
    assert!(total > 0, "the shell produced output");
    let content = term.visible_content();
    assert!(
        content.contains("ATERM_RENDER_OK"),
        "engine model should contain the shell output; got: {content:?}"
    );

    // --- the renderer turned that session into actual pixels ---
    let frame = renderer.render_input(&term.cell_frame(24, 80));
    assert_eq!(frame.pixels.len(), frame.width * frame.height);
    let bg = Theme::default().bg;
    let non_bg = frame.pixels.iter().filter(|&&p| p != bg).count();
    assert!(
        non_bg > 100,
        "expected rasterized glyph + cursor pixels; only {non_bg} non-background pixels"
    );
}
