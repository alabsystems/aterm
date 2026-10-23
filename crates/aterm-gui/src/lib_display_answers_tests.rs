// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    DisplayEndpoint, LinuxTrialStart, a_display_server_answers_from, display_endpoint_answers,
    display_endpoints_from, linux_trial_start, loop_goes_first,
};

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

#[test]
fn a_display_variable_nothing_answers_is_no_window_start() {
    let root = crate::test_scratch::short_dir("a display server's socket");
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

#[test]
fn a_wayland_display_is_asked_at_its_socket() {
    let root = crate::test_scratch::short_dir("a display server's socket");
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

#[test]
fn winits_choice_of_display_decides_whether_a_start_is_counted() {
    let root = crate::test_scratch::short_dir("a display server's socket");
    let runtime = root.path().to_str().unwrap();
    let x_server = root.path().join("X0");
    let _x_server = std::os::unix::net::UnixListener::bind(&x_server).unwrap();
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

#[test]
fn a_launch_whose_display_answers_forks_session_zero_before_the_loop() {
    let root = crate::test_scratch::short_dir("a display server's socket");
    let live = root.path().join("X0");
    let _server = std::os::unix::net::UnixListener::bind(&live).unwrap();
    let gone = root.path().join("X1");
    server_gone_socket(&gone);
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
