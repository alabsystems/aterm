// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const FOLDER_PATH: &str = "/tmp/aterm-lane-aarch64-weston-gone/home/gone-folder";

fn orderly() -> Evidence {
    Evidence {
        status: Some("69\n".into()),
        stderr: "aterm-gui: GPU rendering on llvmpipe (LLVM 20.1.2, 128 bits) (Gl)\n\
                 Io error: Connection reset by peer (os error 104)\n\
                 aterm-gui: the display connection ended (Connection reset by peer (os error \
                 104)); quitting as a logout would, exit status 69\n"
            .into(),
        session: Some(format!("[[windows]]\ncwd = \"{FOLDER_PATH}\"\n")),
        journals: Some(Vec::new()),
        folder: FOLDER_PATH.into(),
        unread: None,
    }
}

/// A person's quit as it ends: exit 0, nothing said about the display.
fn quit() -> Evidence {
    Evidence {
        status: Some("0\n".into()),
        stderr: "aterm-gui: GPU rendering on llvmpipe (LLVM 20.1.2, 128 bits) (Gl)\n".into(),
        ..orderly()
    }
}

#[test]
fn an_orderly_end_passes() {
    for end in [End::AtRest, End::MidResize, End::MidCapture] {
        let detail = judge(&orderly(), end).expect("passes");
        assert!(detail.starts_with("exit 69;"), "{detail}");
    }
    let detail = judge(&quit(), End::Quit).expect("passes");
    assert!(detail.starts_with("exit 0;"), "{detail}");
}

/// NEGATIVE CONTROLS: what the lane saw before the fix, and each way an
/// end can fall short of orderly, fail.
#[test]
fn a_panic_a_kill_a_silent_or_unsaved_end_fails() {
    let judge = |e: &Evidence| judge(e, End::AtRest);
    let panicked = Evidence {
        status: Some("101".into()),
        stderr: "thread 'main' panicked at wgpu-29.0.3/src/backend/wgpu_core.rs:\n\
                 Surface does not support the adapter's queue family\n"
            .into(),
        session: None,
        ..orderly()
    };
    assert!(judge(&panicked).unwrap_err().contains("panicked"));
    let status_only = Evidence {
        status: Some("101".into()),
        stderr: String::new(),
        ..orderly()
    };
    assert!(judge(&status_only).unwrap_err().contains("panic"));
    let xio = Evidence {
        status: Some("1".into()),
        stderr: "XIO:  fatal IO error 11 (Resource temporarily unavailable) on X server \":91\"\n"
            .into(),
        session: None,
        ..orderly()
    };
    assert!(judge(&xio).unwrap_err().contains("without a line"));
    let unsaved = Evidence {
        session: None,
        ..orderly()
    };
    assert!(judge(&unsaved).unwrap_err().contains("no session.toml"));
    let other_folder = Evidence {
        session: Some("[[windows]]\ncwd = \"/tmp\"\n".into()),
        ..orderly()
    };
    assert!(judge(&other_folder).unwrap_err().contains("does not hold"));
    let journal_left = Evidence {
        journals: Some(vec!["journal-4242-1.toml".into()]),
        ..orderly()
    };
    assert!(judge(&journal_left).unwrap_err().contains("crash journal"));
    let unreadable_inventory = Evidence {
        journals: None,
        ..orderly()
    };
    assert!(judge(&unreadable_inventory).is_err());
    let killed = Evidence {
        status: Some("134".into()),
        ..orderly()
    };
    assert!(judge(&killed).unwrap_err().contains("signal 6"));
    let zero = Evidence {
        status: Some("0".into()),
        ..orderly()
    };
    assert!(judge(&zero).unwrap_err().contains("exited 0"));
    let hung = Evidence {
        status: None,
        ..orderly()
    };
    assert!(judge(&hung).unwrap_err().contains("still running"));
}

/// A PERSON'S QUIT is not a lost display: one that took the logout's way
/// out (exit 69 and its line, as every `run_app` error did before GONE-2),
/// exited with any other error, or lost the layout fails.
#[test]
fn a_quit_that_is_not_the_persons_fails() {
    let judge = |e: &Evidence| judge(e, End::Quit);
    let as_logout = Evidence {
        stderr: "aterm-gui: the display connection ended (os error at x11/mod.rs:350: X \
                 error: BadWindow); quitting as a logout would, exit status 69\n"
            .into(),
        ..orderly()
    };
    assert!(judge(&as_logout).unwrap_err().contains("not 0"));
    let said_only = Evidence {
        status: Some("0".into()),
        ..orderly()
    };
    assert!(judge(&said_only).unwrap_err().contains(SAID));
    let failed = Evidence {
        status: Some("70".into()),
        ..quit()
    };
    assert!(judge(&failed).unwrap_err().contains("exited 70"));
    let unsaved = Evidence {
        session: None,
        ..quit()
    };
    assert!(judge(&unsaved).unwrap_err().contains("no session.toml"));
    let hung = Evidence {
        status: None,
        ..quit()
    };
    assert!(judge(&hung).unwrap_err().contains("person's quit"));
}

/// The capture's row is X11's alone: a Wayland client asks the
/// compositor nothing for its position.
#[test]
fn the_capture_s_row_is_x11_s_and_fails_on_its_panic() {
    let rows: Vec<Display> = Display::ALL
        .into_iter()
        .filter(|&d| End::MidCapture.applies(d))
        .collect();
    assert_eq!(rows, [Display::Xvfb, Display::Xwayland]);
    for end in [End::AtRest, End::MidResize, End::Quit] {
        assert!(Display::ALL.into_iter().all(|d| end.applies(d)), "{end:?}");
    }
    let held = Evidence {
        unread: Some("24 bytes".into()),
        ..orderly()
    };
    let detail = judge(&held, End::MidCapture).expect("passes");
    assert!(
        detail.ends_with("the server held 24 bytes unread at its kill"),
        "{detail}"
    );
    let panicked = Evidence {
        status: Some("101".into()),
        stderr: "thread 'main' panicked at crates/aterm-window/src/platform_impl/linux/x11/\
                 window.rs:1193:14:\ncalled `Result::unwrap()` on an `Err` value: \
                 X11(Connection(1))\n"
            .into(),
        session: None,
        ..held
    };
    assert!(
        judge(&panicked, End::MidCapture)
            .unwrap_err()
            .contains("panicked")
    );
}

/// The typed line never contains its own echo, so the echo's appearance
/// is the shell's output.
#[test]
fn the_typed_line_never_contains_its_echo() {
    assert!(!TYPED.contains(ECHO));
    assert!(TYPED.contains(FOLDER));
}
