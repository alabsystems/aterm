// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    DisplayEndpoint, LinuxTrialStart, a_display_server_answers_from, display_endpoint_answers,
    display_endpoints_from, linux_trial_start, loop_goes_first,
};

/// Leave at `path` the socket file of a display server that exited: one nothing answers.
/// A datagram socket, not a dropped stream listener: macOS sets close-on-exec only after
/// `socket()` returns, so a concurrent test's `posix_spawn` can inherit the listener and keep it
/// listening, and a connect then succeeds into its backlog. A stream connect to a datagram
/// socket's path is refused (`EPROTOTYPE`) whoever holds it.
fn server_gone_socket(path: &std::path::Path) {
    drop(std::os::unix::net::UnixDatagram::bind(path).unwrap());
}

fn only<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    move |var| {
        vars.iter()
            .find(|(name, _)| *name == var)
            .map(|(_, value)| (*value).to_string())
    }
}

/// A `DISPLAY` nothing answers (`:99` with no X server: a stale socket file or none) is no
/// window start at the boot apply: the launch fails "cannot open a display", so its start is
/// left for the display's opening. Negative control: a listening socket is a window start.
#[test]
fn a_display_variable_nothing_answers_is_no_window_start() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let socket = root.path().join("X99");
    let display = [("DISPLAY", ":99")];
    let at = |path: &std::path::Path| {
        let path = path.to_path_buf();
        move |endpoint: &DisplayEndpoint| match endpoint {
            DisplayEndpoint::Socket(at)
                if at.as_path() == std::path::Path::new("/tmp/.X11-unix/X99") =>
            {
                display_endpoint_answers(&DisplayEndpoint::Socket(path.clone()))
            }
            _ => false,
        }
    };
    assert_ne!(
        linux_trial_start(
            false,
            a_display_server_answers_from(only(&display), at(&socket))
        ),
        LinuxTrialStart::Window,
        "no socket at all"
    );
    // A socket file whose server is gone refuses the connection.
    server_gone_socket(&socket);
    assert!(socket.exists());
    assert_ne!(
        linux_trial_start(
            false,
            a_display_server_answers_from(only(&display), at(&socket))
        ),
        LinuxTrialStart::Window,
        "a socket left by a server that is gone"
    );
    std::fs::remove_file(&socket).unwrap();
    let _listening = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    assert_eq!(
        linux_trial_start(
            false,
            a_display_server_answers_from(only(&display), at(&socket))
        ),
        LinuxTrialStart::Window,
        "a server listens"
    );
}

/// A Wayland compositor answers at `$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY` (or an
/// absolute `WAYLAND_DISPLAY`); with no runtime directory there is nowhere to ask.
#[test]
fn a_wayland_display_is_asked_at_its_socket() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let runtime = root.path().to_str().unwrap();
    let vars = [
        ("WAYLAND_DISPLAY", "wayland-1"),
        ("XDG_RUNTIME_DIR", runtime),
    ];
    assert_eq!(
        display_endpoints_from(only(&vars)),
        [DisplayEndpoint::Socket(root.path().join("wayland-1"))]
    );
    assert!(!a_display_server_answers_from(
        only(&vars),
        display_endpoint_answers
    ));
    let _compositor =
        std::os::unix::net::UnixListener::bind(root.path().join("wayland-1")).unwrap();
    assert!(a_display_server_answers_from(
        only(&vars),
        display_endpoint_answers
    ));
    assert!(display_endpoints_from(only(&[("WAYLAND_DISPLAY", "wayland-1")])).is_empty());
    assert!(
        display_endpoints_from(only(&[
            ("WAYLAND_DISPLAY", "wayland-1"),
            ("XDG_RUNTIME_DIR", "run/user/1000"),
        ]))
        .is_empty(),
        "libwayland asks under an absolute runtime directory only"
    );
    assert_eq!(
        display_endpoints_from(only(&[("WAYLAND_DISPLAY", "/run/w/wayland-0")])),
        [DisplayEndpoint::Socket("/run/w/wayland-0".into())]
    );
    assert_eq!(
        display_endpoints_from(only(&[("WAYLAND_SOCKET", "5")])),
        [DisplayEndpoint::Inherited(5)]
    );
    // `WAYLAND_SOCKET`, present at all, is the connection: libwayland reads it
    // first and alone, so one that is no number is no connection.
    assert_eq!(
        display_endpoints_from(only(&[
            ("WAYLAND_SOCKET", "7"),
            ("WAYLAND_DISPLAY", "/run/w/wayland-0"),
        ])),
        [DisplayEndpoint::Inherited(7)]
    );
    assert!(
        display_endpoints_from(only(&[
            ("WAYLAND_SOCKET", ""),
            ("WAYLAND_DISPLAY", "/run/w/wayland-0"),
        ]))
        .is_empty()
    );
}

/// `WAYLAND_SOCKET` names a socket the session handed down by number: it answers when
/// the number is an open socket, and not when it is open on anything else or not open at
/// all. Read from the descriptor itself, so every platform's tests read it (a path of
/// the number, `/proc/self/fd/N`, answered nothing off Linux).
#[test]
fn an_inherited_display_socket_answers_and_no_other_descriptor_does() {
    use std::os::fd::AsRawFd as _;
    let (socket, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
    let file = std::fs::File::open("/dev/null").unwrap();
    let answers = |fd| display_endpoint_answers(&DisplayEndpoint::Inherited(fd));
    assert!(answers(socket.as_raw_fd()), "an open socket");
    assert!(!answers(file.as_raw_fd()), "an open file that is no socket");
    assert!(!answers(-1), "no descriptor");
}

/// ROUND EIGHT OF THE UPDATE AUDIT, THE REVIEW OF #82 — WINIT'S CHOICE OF DISPLAY
/// DECIDES, NOT ANY DISPLAY THAT ANSWERS. A tmux server started in a Wayland session
/// is attached from an X11 one: tmux refreshes `DISPLAY`, not `WAYLAND_DISPLAY`, so
/// `:0` answers and `wayland-0` is stale. winit picks Wayland whenever a Wayland
/// variable is set and never falls back to X11, so the window fails "cannot open a
/// display" and its start must not be counted at the boot apply. The same holds
/// for a relative `WAYLAND_DISPLAY` with no runtime directory, and a
/// `WAYLAND_SOCKET` that is no number: neither names an endpoint, and `DISPLAY`
/// does not stand in.
///
/// FAILS WITHOUT THE FIX: any endpoint that answered made it a window start, so the
/// live X server counted a start winit never made.
///
/// NEGATIVE CONTROL: with no Wayland variable, the live X server is a window start;
/// with the compositor live, so is the Wayland one.
#[test]
fn winits_choice_of_display_decides_whether_a_start_is_counted() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let runtime = root.path().to_str().unwrap();
    let x_server = root.path().join("X0");
    let _x_server = std::os::unix::net::UnixListener::bind(&x_server).unwrap();
    // The live X server stands where `:0` names it; every other endpoint is asked.
    let answers = |endpoint: &DisplayEndpoint| match endpoint {
        DisplayEndpoint::Socket(at)
            if at.as_path() == std::path::Path::new("/tmp/.X11-unix/X0") =>
        {
            display_endpoint_answers(&DisplayEndpoint::Socket(x_server.clone()))
        }
        DisplayEndpoint::AbstractX11(_) => false,
        other => display_endpoint_answers(other),
    };
    let start = |vars: &[(&str, &str)]| {
        linux_trial_start(false, a_display_server_answers_from(only(vars), answers))
    };
    // A compositor socket left by a session that ended.
    let stale = root.path().join("wayland-0");
    server_gone_socket(&stale);
    let attached = [
        ("DISPLAY", ":0"),
        ("WAYLAND_DISPLAY", "wayland-0"),
        ("XDG_RUNTIME_DIR", runtime),
    ];
    assert_ne!(
        start(&attached),
        LinuxTrialStart::Window,
        "a stale compositor, with a live X server beside it"
    );
    assert_ne!(
        start(&[("DISPLAY", ":0"), ("WAYLAND_DISPLAY", "wayland-0")]),
        LinuxTrialStart::Window,
        "a relative WAYLAND_DISPLAY with no runtime directory"
    );
    assert_ne!(
        start(&[("DISPLAY", ":0"), ("WAYLAND_SOCKET", "wayland")]),
        LinuxTrialStart::Window,
        "a WAYLAND_SOCKET that is no number"
    );
    assert_eq!(
        start(&[
            ("DISPLAY", ":0"),
            ("WAYLAND_DISPLAY", ""),
            ("WAYLAND_SOCKET", "")
        ]),
        LinuxTrialStart::Window,
        "with no Wayland variable set, the X server winit opens answers"
    );
    std::fs::remove_file(&stale).unwrap();
    let _compositor = std::os::unix::net::UnixListener::bind(&stale).unwrap();
    assert_eq!(
        start(&attached),
        LinuxTrialStart::Window,
        "the compositor winit opens answers"
    );
}

/// LINUX-6 — A LAUNCH WHOSE DISPLAY ANSWERS FORKS SESSION 0 BEFORE THE EVENT LOOP.
/// Session 0's shell waited for every free-unix window's event-loop build, which opens
/// the display (9.3-9.9 ms on X11, 1.6-1.8 ms on Wayland in the Linux lane, 2026-10-20).
/// Now the build goes first only where no display server answered where winit will
/// connect, so a launch that cannot open its display still takes nothing single-use and
/// forks no shell. A build that opens no display (macOS, a headless run with none
/// named) asks no server; the dev seam puts the loop first everywhere.
///
/// FAILS WITHOUT THE FIX: every build that opens a display went first.
///
/// NEGATIVE CONTROL: the display named by a socket its server left answers nothing, and
/// the loop goes first.
#[test]
fn a_launch_whose_display_answers_forks_session_zero_before_the_loop() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let live = root.path().join("X0");
    let _server = std::os::unix::net::UnixListener::bind(&live).unwrap();
    let gone = root.path().join("X1");
    server_gone_socket(&gone);
    // `:0`, answered at `at` in place of `/tmp/.X11-unix/X0`.
    let answered_at = |at: &std::path::Path| {
        let at = at.to_path_buf();
        move || {
            a_display_server_answers_from(only(&[("DISPLAY", ":0")]), |endpoint| {
                matches!(endpoint, DisplayEndpoint::Socket(path)
                    if path.as_path() == std::path::Path::new("/tmp/.X11-unix/X0"))
                    && display_endpoint_answers(&DisplayEndpoint::Socket(at.clone()))
            })
        }
    };
    assert!(
        !loop_goes_first(true, answered_at(&live), false),
        "a display that answers: session 0 forks first"
    );
    assert!(
        loop_goes_first(true, answered_at(&gone), false),
        "a display named that nothing answers: the loop's build goes first"
    );
    assert!(
        !loop_goes_first(
            false,
            || panic!("a build that opens no display asks no server"),
            false
        ),
        "a build that opens no display"
    );
    assert!(
        loop_goes_first(true, answered_at(&live), true),
        "the dev seam puts the loop first"
    );
}

/// How `DISPLAY` names its server, as libxcb parses it: an empty host or `unix` is
/// the local server's socket; any other host is over the network, which no launch
/// can vouch for (`ssh -X`'s `localhost:10.0`); a path is that socket.
#[test]
fn display_names_its_server_as_libxcb_reads_it() {
    let x = |display: &str| display_endpoints_from(only(&[("DISPLAY", display)]));
    let local = |n: u32| {
        vec![
            DisplayEndpoint::Socket(format!("/tmp/.X11-unix/X{n}").into()),
            DisplayEndpoint::AbstractX11(n),
        ]
    };
    assert_eq!(x(":0"), local(0));
    assert_eq!(x(":1.0"), local(1));
    assert_eq!(x("unix:2"), local(2));
    assert_eq!(x("unix/:3"), local(3));
    assert_eq!(x("localhost:10.0"), [DisplayEndpoint::Remote]);
    assert_eq!(x("tcp/:0"), [DisplayEndpoint::Remote]);
    assert_eq!(x("10.0.0.2:0"), [DisplayEndpoint::Remote]);
    assert_eq!(
        x("/private/tmp/launch-x/org.xquartz:0"),
        [DisplayEndpoint::Socket(
            "/private/tmp/launch-x/org.xquartz".into()
        )]
    );
    assert!(x("garbage").is_empty());
    assert!(x(":x").is_empty());
    assert!(x("").is_empty());
    assert!(!a_display_server_answers_from(
        only(&[("DISPLAY", "localhost:10.0")]),
        display_endpoint_answers
    ));
}
