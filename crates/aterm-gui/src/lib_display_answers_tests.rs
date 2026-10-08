// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    DisplayEndpoint, LinuxTrialStart, a_display_server_answers_from, display_endpoint_answers,
    display_endpoints_from, linux_trial_start,
};

/// Leave at `path` the socket file a display server that exited leaves behind: one
/// nothing answers. A DATAGRAM socket, never a dropped stream listener: macOS marks a
/// socket close-on-exec only after `socket()` returns, so another test's
/// `posix_spawn` in that window inherits the listener and keeps it listening after
/// this test drops its copy, and a connect then succeeds into its backlog — the
/// merge gate saw "a stale compositor" answer (2026-10-05). A stream connect to a
/// datagram socket's path is refused (`EPROTOTYPE`) whoever still holds it.
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

/// ROUND EIGHT OF THE UPDATE AUDIT, #82 — A DISPLAY VARIABLE NOTHING ANSWERS IS
/// NO WINDOW START AT THE BOOT APPLY. `DISPLAY=:99` with no X server on :99 (a
/// socket file left by a server that is gone, or none at all): a windowed launch
/// fails "cannot open a display" whatever build it is, so its start is left for
/// the display's opening, which never comes.
///
/// FAILS WITHOUT THE FIX: the variable alone made it a window start, counted
/// before the event loop failed; the fourth rolled a healthy build back.
///
/// NEGATIVE CONTROL: a socket a server listens on is a window start.
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

/// THE START THE BOOT APPLY LEAVES IS COUNTED ONCE THE DISPLAY OPENS (the review of
/// #82). A windowed launch whose display cannot be vouched for before it opens — an
/// `ssh -X` forward, which sshd accepts whatever stands behind it — was never
/// counted, so a build whose window crashes under X forwarding was never rolled
/// back from the window lane, and the next release took it as its rollback target.
/// The count is made the moment the event loop has opened the display, before
/// anything else of the window runs, and only for a launch the boot apply left it
/// to (a Linux-only lane, so pinned here by its source).
#[test]
fn the_start_left_by_the_boot_apply_is_counted_once_the_display_opens() {
    let src = include_str!("lib.rs");
    assert!(src.contains(
        "        Ok(el) => {\n            \
         crate::metrics::stamp_claim(crate::metrics::ClaimStamp::EventLoopBuilt);\n            \
         // The display opened: a Linux window start the boot apply could not\n            \
         // vouch for is counted now (round eight of the update audit, #82).\n            \
         count_linux_window_start_once_open();\n            \
         el\n        }"
    ));
    let lane = &src[src
        .find("fn linux_launch_without_a_window(headless: bool) -> bool {")
        .expect("the boot apply's lane")..];
    let lane = &lane[..lane.find("\n}\n").expect("its end")];
    // The executable path is read at the boot apply's point, before the display
    // query, and never first by the late count: a sibling's replacement in between
    // would leave the window's update lane refusing `<path> (deleted)`.
    let captured = lane
        .find("aterm_update::linux_capture_executable();")
        .expect("the lane reads the executable path at the boot point");
    assert!(captured < lane.find("a_display_server_answers()").expect("the query"));
    assert!(lane.contains(
        "            LinuxTrialStart::WindowOnceOpen => {\n                \
         LINUX_WINDOW_START_ONCE_OPEN.store(true, std::sync::atomic::Ordering::Release);\n                \
         true\n            }"
    ));
    let count = &src[src
        .find("fn count_linux_window_start_once_open() {")
        .expect("the count once open")..];
    let count = &count[..count.find("\n}\n").expect("its end")];
    assert!(count.contains(
        "    if LINUX_WINDOW_START_ONCE_OPEN.load(std::sync::atomic::Ordering::Acquire) {\n        \
         match aterm_update::linux_window_opened("
    ));
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
