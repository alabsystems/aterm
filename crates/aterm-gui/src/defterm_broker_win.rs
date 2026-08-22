// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! The DefTerm **handoff-broker thread**: a single-threaded-apartment (STA) COM
//! server with a real Win32 message pump, running BESIDE winit rather than
//! inside it.
//!
//! # Why a satellite thread at all
//!
//! An out-of-process COM server registered into an STA is only reachable while
//! somebody pumps that apartment's messages: `CoRegisterClassObject` publishes a
//! class factory, and every incoming activation arrives as a window message that
//! `GetMessage`/`DispatchMessage` must retrieve for the COM runtime to unmarshal
//! and dispatch it. winit's event loop does not pump an apartment we created and
//! cannot be made to without owning its loop.
//!
//! The rejected alternative was to invert that — let COM own the main loop and
//! drive winit from it (the "Option B" rewrite). It would put every keystroke and
//! every frame behind an apartment pump aterm does not control, for a feature
//! that fires a handful of times a day. So: **winit stays the UI event loop; this
//! is a satellite.** The broker owns one thread, that thread owns the apartment,
//! and the only thing crossing back to the UI thread is a wake carrying the
//! adopted handles — which `aterm_pty::adopt_handoff` then turns into an ordinary
//! session, indistinguishable downstream from one we spawned ourselves.
//!
//! # What is built here, and what is not
//!
//! Built and tested: the thread, the apartment, the pump, and — the part that is
//! genuinely easy to get wrong — a shutdown handshake that cannot race. NOT
//! built: `CoRegisterClassObject` and the class factory, because the interface
//! cannot be marshalled without a proxy/stub aterm does not ship (see
//! [`crate::defterm_win::handoff_server_available`] for the field evidence).
//! [`start`] therefore refuses, and nothing spawns a thread in production.
//!
//! The remaining wiring, for whoever lands the server: on `EstablishPtyHandoff`
//! the pump thread calls `aterm_pty::adopt_handoff` (or forwards the raw handles)
//! and posts a `Wake` to the winit `EventLoopProxy`; the main thread opens a tab
//! around the returned master. The handoff must NOT block on the UI thread — COM
//! is waiting on the caller's RPC — so the wake is fire-and-forget and the reply
//! is `S_OK` as soon as the handles are owned.

#![cfg(windows)]
#![allow(dead_code)] // The whole module is staged behind `handoff_server_available`.

use std::io;
use std::sync::mpsc;

/// `COINIT_APARTMENTTHREADED` — an STA, which is what a class factory serving a
/// UI app must live in.
const COINIT_APARTMENTTHREADED: u32 = 0x2;
/// `WM_QUIT`: what breaks the `GetMessage` loop.
const WM_QUIT: u32 = 0x0012;
/// `PM_NOREMOVE` — peek without consuming. Used solely to FORCE the thread's
/// message queue into existence; see [`pump_until_quit`].
const PM_NOREMOVE: u32 = 0x0000;
/// `S_FALSE` — `CoInitializeEx` succeeded but the apartment was already
/// initialized. Success, and it means we must still balance `CoUninitialize`.
const S_FALSE: i32 = 1;

/// Win32 `POINT`, for [`Msg`]'s tail.
#[repr(C)]
#[derive(Default)]
struct Point {
    x: i32,
    y: i32,
}

/// Win32 `MSG`.
///
/// Field-for-field IDENTICAL to `notify.rs`'s `win_balloon::MSG` — deliberately,
/// and it must stay that way. Both modules declare `PeekMessageW`/
/// `TranslateMessage`/`DispatchMessageW` against their own `MSG`, and rustc's
/// `clashing_extern_declarations` lint compares those signatures STRUCTURALLY
/// across the crate: any divergence (even inserting explicit padding fields that
/// `repr(C)` would have added anyway) makes the two declarations of the same
/// symbol disagree, and a genuine layout mismatch there is a stack buffer
/// overrun inside `GetMessageW`. Letting `repr(C)` do the padding keeps the two
/// structurally equal, so the lint stays quiet for the RIGHT reason.
#[repr(C)]
#[derive(Default)]
struct Msg {
    hwnd: isize,
    message: u32,
    w_param: usize,
    l_param: isize,
    time: u32,
    pt: Point,
}

#[link(name = "ole32")]
unsafe extern "system" {
    fn CoInitializeEx(reserved: *mut core::ffi::c_void, co_init: u32) -> i32;
    fn CoUninitialize();
}

#[link(name = "user32")]
unsafe extern "system" {
    fn GetMessageW(msg: *mut Msg, hwnd: isize, min: u32, max: u32) -> i32;
    fn TranslateMessage(msg: *const Msg) -> i32;
    fn DispatchMessageW(msg: *const Msg) -> isize;
    fn PeekMessageW(msg: *mut Msg, hwnd: isize, min: u32, max: u32, remove: u32) -> i32;
    fn PostThreadMessageW(thread_id: u32, msg: u32, w: usize, l: isize) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentThreadId() -> u32;
}

/// A running broker. Dropping it stops the pump and JOINS the thread, so the
/// apartment is always torn down on a thread that actually entered it — COM
/// requires `CoUninitialize` on the same thread as `CoInitializeEx`, which is
/// only guaranteed if we join rather than detach.
#[derive(Debug)]
pub(crate) struct Broker {
    thread_id: u32,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Broker {
    /// Ask the pump to exit, without waiting.
    fn post_quit(&self) {
        // SAFETY: `thread_id` names a thread that has already created its message
        // queue (the ready handshake guarantees it), so the post cannot be lost.
        unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0) };
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        self.post_quit();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Start the handoff broker.
///
/// # Errors
/// `Unsupported` while no COM handoff server can answer — which is always,
/// today. Checked FIRST, so no thread and no apartment are created: an idle STA
/// pump that can never receive an activation is pure cost, and a broker that
/// looks alive while the registration is impossible is exactly the kind of
/// half-wired state that makes a later integrator believe the lane works.
pub(crate) fn start() -> io::Result<Broker> {
    if !crate::defterm_win::handoff_server_available() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no COM handoff server in this build; the DefTerm broker is not started",
        ));
    }
    spawn_pump()
}

/// Spawn the STA pump thread and wait for it to be REACHABLE.
///
/// The handshake is the load-bearing part. `PostThreadMessageW` silently fails
/// against a thread that has not yet created its message queue, and a thread
/// only gets one the first time it calls a message API. A naive
/// `spawn` + `PostThreadMessage(WM_QUIT)` therefore loses the quit and hangs
/// forever in `Drop`'s join — intermittently, under load, which is the worst
/// possible way to find out. So the thread calls `PeekMessageW` to force the
/// queue, and only THEN sends its id back; `spawn_pump` does not return until it
/// has that id, so every `Broker` in existence is already postable.
fn spawn_pump() -> io::Result<Broker> {
    let (tx, rx) = mpsc::channel::<u32>();
    let join = std::thread::Builder::new()
        .name("aterm-defterm-broker".into())
        .spawn(move || {
            // SAFETY: no reserved param; STA is the required apartment for a
            // class factory serving a UI app.
            let hr = unsafe { CoInitializeEx(std::ptr::null_mut(), COINIT_APARTMENTTHREADED) };
            // hr < 0 is a real failure; S_OK and S_FALSE both mean "initialized"
            // and both must be balanced by CoUninitialize.
            let co_ok = hr >= 0;
            if !co_ok {
                // Report readiness anyway so the starter is never left hanging;
                // the pump below then exits immediately on the quit it receives.
                let _ = tx.send(0);
                return;
            }

            // THE REGISTRATION WOULD GO HERE:
            //   CoRegisterClassObject(CLSID_ATERM_TERMINAL, factory,
            //                         CLSCTX_LOCAL_SERVER, REGCLS_MULTIPLEUSE, &cookie)
            // followed by CoResumeClassObjects(). It is absent on purpose — see
            // the module docs; without a proxy/stub the interface cannot be
            // marshalled, and publishing a factory that cannot serve calls is
            // worse than publishing none.

            pump_until_quit(&tx);

            // SAFETY: balanced against the successful CoInitializeEx above, on
            // the SAME thread, as COM requires.
            unsafe { CoUninitialize() };
        })
        .map_err(|e| io::Error::other(format!("failed to spawn the DefTerm broker thread: {e}")))?;

    // Block until the thread is postable (see the handshake note above).
    let thread_id = rx
        .recv()
        .map_err(|_| io::Error::other("the DefTerm broker thread exited before becoming ready"))?;
    Ok(Broker {
        thread_id,
        join: Some(join),
    })
}

/// Force this thread's message queue into existence, publish its id through
/// `ready`, then run a real `GetMessage`/`Translate`/`Dispatch` loop until
/// `WM_QUIT`.
///
/// Split out from the thread body so the pump contract — becomes reachable,
/// then exits on `WM_QUIT`, then returns — is testable without COM, a class
/// factory, or a registered CLSID.
fn pump_until_quit(ready: &mpsc::Sender<u32>) {
    let mut msg = Msg::default();
    // Materialize the queue BEFORE anyone can post to it: a thread has no
    // message queue until it first calls a message API, and `PostThreadMessageW`
    // FAILS against a thread that has none. `min = max = 0` is the "any message"
    // range (not a filter constant — there is none to pass here), and
    // `PM_NOREMOVE` means the peek consumes nothing.
    // SAFETY: out-param `msg`; PM_NOREMOVE peeks without consuming.
    unsafe { PeekMessageW(&mut msg, 0, 0, 0, PM_NOREMOVE) };
    // SAFETY: no arguments.
    let id = unsafe { GetCurrentThreadId() };
    if ready.send(id).is_err() {
        // Nobody is waiting for us (the starter gave up); nothing to pump for.
        return;
    }
    loop {
        // SAFETY: valid out-param; hwnd 0 + range 0,0 retrieves thread messages
        // too, which is how WM_QUIT from PostThreadMessageW arrives.
        let r = unsafe { GetMessageW(&mut msg, 0, 0, 0) };
        // 0 == WM_QUIT (stop), -1 == error (stop; never spin on a broken queue).
        if r <= 0 {
            return;
        }
        // SAFETY: `msg` was just filled by a successful GetMessageW.
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The broker must not start while nothing can answer a handoff — no thread,
    /// no apartment, no published factory. Same safety invariant as the
    /// registration gate, enforced one layer up.
    #[test]
    fn broker_refuses_to_start_without_a_handoff_server() {
        let err = start().expect_err("the broker must not start while the lane is inert");
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    /// The pump becomes REACHABLE before it is advertised, then exits on the
    /// WM_QUIT posted to it. This is the race the `PeekMessageW` call exists to
    /// close: without the forced queue the post is dropped and the join below
    /// would hang forever. Runs the real Win32 pump on a real thread.
    #[test]
    fn pump_becomes_postable_then_exits_on_quit() {
        let (tx, rx) = mpsc::channel::<u32>();
        let join = std::thread::Builder::new()
            .name("defterm-pump-test".into())
            .spawn(move || pump_until_quit(&tx))
            .expect("spawn");

        let id = rx.recv().expect("the pump must publish its thread id");
        assert_ne!(id, 0, "a real thread id");

        // SAFETY: the handshake guarantees the queue exists, so this post lands.
        let posted = unsafe { PostThreadMessageW(id, WM_QUIT, 0, 0) };
        assert_ne!(
            posted, 0,
            "PostThreadMessageW must succeed against a thread that has a queue"
        );
        join.join().expect("the pump must exit on WM_QUIT");
    }

    /// Two brokers must not collide on thread ids, and each must shut down
    /// independently — the pump keys shutdown on its OWN thread id, never a
    /// process-wide signal.
    #[test]
    fn pumps_are_independent() {
        let mut ids = Vec::new();
        let mut joins = Vec::new();
        for _ in 0..2 {
            let (tx, rx) = mpsc::channel::<u32>();
            joins.push(
                std::thread::Builder::new()
                    .spawn(move || pump_until_quit(&tx))
                    .expect("spawn"),
            );
            ids.push(rx.recv().expect("id"));
        }
        assert_ne!(ids[0], ids[1], "distinct threads have distinct ids");
        // Quit the FIRST only; the second must still be running.
        // SAFETY: both ids came from the ready handshake.
        unsafe { PostThreadMessageW(ids[0], WM_QUIT, 0, 0) };
        joins.remove(0).join().expect("first pump exits");
        // SAFETY: as above.
        unsafe { PostThreadMessageW(ids[1], WM_QUIT, 0, 0) };
        joins.remove(0).join().expect("second pump exits");
    }

    /// The x64 `MSG` layout must match the C struct the Win32 API writes into.
    /// A short struct here is a stack buffer overrun in `GetMessageW`.
    #[test]
    fn msg_matches_the_win32_layout() {
        assert_eq!(std::mem::size_of::<Msg>(), 48, "x64 sizeof(MSG)");
        assert_eq!(std::mem::align_of::<Msg>(), 8);
    }
}
