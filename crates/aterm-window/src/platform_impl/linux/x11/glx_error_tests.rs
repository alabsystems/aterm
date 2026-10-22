// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! An X error that a GLX request raises belongs to the GL renderer, which reads its failures
//! from GLX's own answers. The error handler keeps such an error out of
//! `XConnection::latest_error`. Otherwise the next window call's `check_errors` would take it for
//! its own: on a server whose GLX makes no OpenGL ES 3.0 context, aterm-gpu's refused context
//! would make the next window fail to open. The GLX request here is `glXMakeContextCurrent` with a
//! drawable and no context, which GLX refuses with `BadMatch` under its own major opcode. An
//! ordinary request's error (`GetAtomName` of `None`) is still the connection's. That read
//! cannot change a resource belonging to another X client.
//!
//! The negative control: take the GLX check out of `x_error_callback`, and the sync after the GLX
//! request returns GLX's `BadMatch`. Both errors land in the one connection the process's X11
//! backend shares, which every other window test in this binary checks for errors, so the work
//! runs in a child process: this test binary run again on this one test, with [`CHILD`] set.
//!
//! It runs where `$DISPLAY` names an X server; elsewhere it says SKIP and returns.

use std::ffi::{c_int, c_ulong, c_void};
use std::time::{Duration, Instant};

use super::ffi;
use crate::platform_impl::{ActiveEventLoop as PlatformActiveEventLoop, EventLoop};

/// Set in the child's environment: run the test's body.
const CHILD: &str = "ATERM_WINDOW_GLX_ERROR_CHILD";

/// What the child prints once both errors went where they belong.
const DONE: &str = "X11-GLX-ERROR: the GLX error was the renderer's, the other the connection's";

/// The hang detector: how long the child may take, which it never needs.
const HANG: Duration = Duration::from_secs(60);

/// `GetAtomName`'s major opcode (X11 protocol).
const X_GET_ATOM_NAME: u8 = 17;

#[test]
fn a_glx_requests_error_is_not_the_next_window_calls() {
    if std::env::var_os("DISPLAY").is_none() {
        eprintln!("SKIP: no DISPLAY");
        return;
    }
    if std::env::var_os(CHILD).is_some() {
        raise_a_glx_error_and_an_ordinary_one();
        return;
    }
    let name = "a_glx_requests_error_is_not_the_next_window_calls";
    let module = module_path!().split_once("::").map_or(module_path!(), |(_, path)| path);
    let scratch = aterm_tempfile::tempdir().expect("a scratch dir");
    let mut command =
        aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"));
    command
        .args(["--exact", &format!("{module}::{name}"), "--nocapture", "--test-threads=1"])
        .env(CHILD, "1")
        .current_dir(scratch.path());
    let output =
        aterm_uds::deadline::outputs_until(&mut [&mut command], Instant::now() + HANG, HANG)
            .pop()
            .unwrap_or(Err(aterm_uds::deadline::RunError::TimedOut));
    let output = match output {
        Ok(output) => output,
        Err(error) => {
            // A failed bounded reap does not prove the child is gone. Keep its cwd.
            let kept = scratch.keep();
            panic!("the child did not finish: {error:?}; scratch retained at {}", kept.display());
        },
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let said = format!("its stdout:\n{stdout}\nits stderr:\n{stderr}");
    if let Some(why) = stderr.lines().find(|l| l.contains(aterm_uds::host::COULD_NOT_RUN)) {
        aterm_uds::host::could_not_run(format!("the child: {why}"));
    }
    assert!(!stderr.contains("panicked at"), "the child panicked; {said}");
    assert!(output.status.success(), "the child exited {}; {said}", output.status);
    assert!(stdout.contains(DONE), "the child never got through; {said}");
}

/// The child's work: the X11 backend's connection, a GLX request GLX refuses, a sync (the one a
/// new window's creation ends with), then an ordinary request's error and a sync.
fn raise_a_glx_error_and_an_ordinary_one() {
    let event_loop = EventLoop::<()>::new_x11_any_thread().expect("an X11 loop on $DISPLAY");
    let PlatformActiveEventLoop::X(target) = &event_loop.window_target().p else {
        panic!("an X11 loop");
    };
    let xconn = &target.xconn;
    if xconn.glx_major.is_none() {
        aterm_uds::host::could_not_run("$DISPLAY's X server has no GLX");
    }
    // SAFETY: a NUL-terminated name.
    let lib = unsafe { libc::dlopen(c"libGLX.so.0".as_ptr(), libc::RTLD_LAZY) };
    if lib.is_null() {
        aterm_uds::host::could_not_run("no libGLX.so.0 (glvnd's GLX)");
    }
    // SAFETY: a live handle and a NUL-terminated name.
    let sym = unsafe { libc::dlsym(lib, c"glXMakeContextCurrent".as_ptr()) };
    assert!(!sym.is_null(), "libGLX.so.0 has glXMakeContextCurrent");
    type MakeContextCurrent =
        unsafe extern "C" fn(*mut ffi::Display, c_ulong, c_ulong, *mut c_void) -> c_int;
    // SAFETY: glXMakeContextCurrent's GLX 1.3 signature.
    let make_current = unsafe { std::mem::transmute::<*mut c_void, MakeContextCurrent>(sym) };
    // SAFETY: the backend's live display; its root window as both drawables.
    let root = unsafe { (xconn.xlib.XDefaultRootWindow)(xconn.display) };
    // SAFETY: as above; a drawable with no context, which GLX refuses with `BadMatch`.
    let made = unsafe { make_current(xconn.display, root, root, std::ptr::null_mut()) };
    assert_eq!(made, 0, "GLX refuses a drawable with no context");
    xconn.sync_with_server().expect("the GLX request's error is not the connection's");

    let mut atom: ffi::Atom = 0;
    let mut name = std::ptr::null_mut();
    // SAFETY: the backend's live display and one-element input/output arrays. Atom None is
    // never a defined atom; asking for its name raises BadAtom without changing any resource.
    unsafe { (xconn.xlib.XGetAtomNames)(xconn.display, &mut atom, 1, &mut name) };
    if !name.is_null() {
        // SAFETY: a name returned by XGetAtomNames is an Xlib allocation released by XFree.
        unsafe { (xconn.xlib.XFree)(name.cast()) };
    }
    let ordinary = xconn.sync_with_server().expect_err("an ordinary request's error is kept");
    assert_eq!(ordinary.request_code, X_GET_ATOM_NAME, "the error kept is GetAtomName's");
    println!("{DONE}");
}
