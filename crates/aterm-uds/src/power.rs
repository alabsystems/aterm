// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! This Mac's sleep and wake, as the system announces them to a process that registered for
//! them (`IORegisterForSystemPower`). [`listen`] hands each announcement to a handler on a
//! thread of its own and acknowledges what waits for an acknowledgement ([`answer`]): a
//! will-sleep only once the handler returned, so a handler can finish what must end before the
//! Mac sleeps (the vault server's `dns-sd` registrations, `aterm-secrets`). Elsewhere than
//! macOS nothing is heard.

#[cfg(target_os = "macos")]
mod iokit;

use std::panic::AssertUnwindSafe;

/// What the system announced, as a [`listen`] handler hears it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Power {
    /// The system will sleep (`kIOMessageSystemWillSleep`). It waits for this process's
    /// acknowledgement, which [`answer`] gives once the handler returns; macOS sleeps anyway
    /// 30 s on, so a handler that needs time bounds it well inside that.
    WillSleep,
    /// The system woke (`kIOMessageSystemHasPoweredOn`), or an idle sleep it pondered did not
    /// come (`kIOMessageSystemWillNotSleep`).
    Woke,
}

/// One message from the system's root power domain, as [`answer`] takes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Message {
    /// `kIOMessageCanSystemSleep`: the system ponders an idle sleep; it waits for an
    /// acknowledgement (or a veto).
    CanSleep,
    /// `kIOMessageSystemWillSleep`: the system will sleep; it waits for an acknowledgement.
    WillSleep,
    /// `kIOMessageSystemWillNotSleep`: the idle sleep it pondered does not come.
    WillNotSleep,
    /// `kIOMessageSystemHasPoweredOn`: the system woke.
    HasPoweredOn,
}

/// Answer one message: hand `handler` what it announces, and acknowledge (`allow`) the two that
/// wait for an acknowledgement. A will-sleep is acknowledged only once the handler returned,
/// and also when it panicked, so the Mac never waits on a handler that failed. A can-sleep is
/// acknowledged at once and reaches no handler: this process never keeps the Mac from an idle
/// sleep, and acts only on the will-sleep that follows one. A will-not-sleep and a wake reach
/// the handler as [`Power::Woke`] and are not acknowledged (the system waits for neither).
pub fn answer(message: Message, handler: &mut dyn FnMut(Power), allow: impl FnOnce()) {
    let mut hand = |power| {
        let _ = std::panic::catch_unwind(AssertUnwindSafe(|| handler(power)));
    };
    match message {
        Message::CanSleep => allow(),
        Message::WillSleep => {
            hand(Power::WillSleep);
            allow();
        }
        Message::WillNotSleep | Message::HasPoweredOn => hand(Power::Woke),
    }
}

/// A listener [`listen`] started: it hears until this is dropped, which ends its thread at once
/// (its run loop is stopped, so nothing looks on a timer for its owner meanwhile).
#[derive(Debug)]
pub struct Listening {
    /// What ends the thread; none for an [`Listening::inert`] one.
    _stop: Option<imp::Stop>,
}

impl Listening {
    /// A listener that hears nothing, for a test's arm of a seam that returns one.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn inert() -> Self {
        Self { _stop: None }
    }
}

/// Hear this Mac's sleep and wake on a thread of its own, handing each announcement to
/// `handler` there and answering the system as [`answer`] does, until the returned
/// [`Listening`] is dropped; `None` when the system did not take the registration. Elsewhere
/// than macOS: `None`, and nothing is heard.
#[must_use]
pub fn listen(handler: Box<dyn FnMut(Power) + Send>) -> Option<Listening> {
    imp::listen(handler).map(|stop| Listening { _stop: Some(stop) })
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::Power;

    /// Never made: nothing listens here.
    #[derive(Debug)]
    pub(super) enum Stop {}

    pub(super) fn listen(_handler: Box<dyn FnMut(Power) + Send>) -> Option<Stop> {
        None
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::iokit::{
        CFRelease, CFRetain, CFRunLoopAddSource, CFRunLoopGetCurrent, CFRunLoopRef,
        CFRunLoopRemoveSource, CFRunLoopRunInMode, CFRunLoopSourceContext, CFRunLoopSourceCreate,
        CFRunLoopSourceInvalidate, CFRunLoopSourceRef, CFRunLoopSourceSignal, CFRunLoopStop,
        CFRunLoopWakeUp, IOAllowPowerChange, IODeregisterForSystemPower, IONotificationPortDestroy,
        IONotificationPortGetRunLoopSource, IONotificationPortRef, IORegisterForSystemPower,
        IOServiceClose, io_connect_t, io_object_t, io_service_t, kCFRunLoopDefaultMode,
        kIOMessageCanSystemSleep, kIOMessageSystemHasPoweredOn, kIOMessageSystemWillNotSleep,
        kIOMessageSystemWillSleep,
    };
    use super::{Message, Power, answer};
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, mpsc};

    /// How long one run of the listener's run loop may last: CoreFoundation's own "forever"
    /// (`CFRunLoopRun`'s), so it returns only when [`Stop`] stops it.
    const FOREVER: f64 = 1.0e10;

    /// What the callback reaches through its refcon: the root domain session it answers on,
    /// and the handler.
    struct Listener {
        root: io_connect_t,
        handler: Box<dyn FnMut(Power) + Send>,
    }

    /// Ends a listener's thread when dropped: it marks the thread stopped, then signals the
    /// thread's stop source and wakes its run loop. A signalled source stays pending until the
    /// run loop runs, so a drop before the thread first runs it is not lost (a bare
    /// `CFRunLoopStop` then would be).
    #[derive(Debug)]
    pub(super) struct Stop {
        run_loop: CFRunLoopRef,
        source: CFRunLoopSourceRef,
        stopped: Arc<AtomicBool>,
    }

    // SAFETY: `Stop` holds its own retained reference to the run loop and the source, and uses
    // them only through CoreFoundation calls documented safe from any thread
    // (`CFRunLoopSourceSignal`, `CFRunLoopWakeUp`, `CFRelease`).
    unsafe impl Send for Stop {}
    // SAFETY: as for `Send`; a shared `Stop` exposes no call at all.
    unsafe impl Sync for Stop {}

    impl Drop for Stop {
        fn drop(&mut self) {
            self.stopped.store(true, Ordering::Release);
            // SAFETY: both references are the ones the listener's thread retained for this
            // `Stop`, released once here; signalling an invalidated source does nothing.
            unsafe {
                CFRunLoopSourceSignal(self.source);
                CFRunLoopWakeUp(self.run_loop);
                CFRelease(self.source.cast_const().cast());
                CFRelease(self.run_loop.cast_const().cast());
            }
        }
    }

    /// The stop source's perform: ends the current run of the run loop it fires on (the
    /// listener's, while it runs).
    extern "C" fn stop_here(_info: *mut c_void) {
        // SAFETY: the calling thread's own run loop, which is running (this is its callout).
        unsafe { CFRunLoopStop(CFRunLoopGetCurrent()) };
    }

    /// The message an IOKit message type names, if it is one [`answer`] takes.
    #[expect(
        non_upper_case_globals,
        reason = "IOMessage.h's own names, matched as the constants they are"
    )]
    pub(super) fn message(kind: u32) -> Option<Message> {
        match kind {
            kIOMessageCanSystemSleep => Some(Message::CanSleep),
            kIOMessageSystemWillSleep => Some(Message::WillSleep),
            kIOMessageSystemWillNotSleep => Some(Message::WillNotSleep),
            kIOMessageSystemHasPoweredOn => Some(Message::HasPoweredOn),
            _ => None,
        }
    }

    #[cfg_attr(trust_verify, trust::skip)]
    unsafe extern "C" fn heard(
        refcon: *mut c_void,
        _service: io_service_t,
        kind: u32,
        argument: *mut c_void,
    ) {
        let Some(message) = message(kind) else {
            return;
        };
        // SAFETY: `refcon` is the `Box<Listener>` the listener thread leaked for this
        // registration and frees only after deregistering it; IOKit calls this only from that
        // thread's run loop, so no other reference to it is live.
        let listener = unsafe { &mut *refcon.cast::<Listener>() };
        let root = listener.root;
        answer(message, &mut *listener.handler, || {
            // SAFETY: `root` is the open session `IORegisterForSystemPower` returned, and a
            // can-sleep or will-sleep message carries its notification id as its argument.
            unsafe { IOAllowPowerChange(root, argument as isize) };
        });
    }

    #[cfg_attr(trust_verify, trust::skip)]
    pub(super) fn listen(handler: Box<dyn FnMut(Power) + Send>) -> Option<Stop> {
        let (told, handed) = mpsc::sync_channel::<Option<Stop>>(1);
        let spawned = std::thread::Builder::new()
            .name("power-listen".to_owned())
            .spawn(move || {
                let listener = Box::into_raw(Box::new(Listener { root: 0, handler }));
                let mut port: IONotificationPortRef = std::ptr::null_mut();
                let mut notifier: io_object_t = 0;
                // SAFETY: `listener` is a live allocation this thread frees only after the
                // notifier is deregistered below; the port and notifier are out-parameters.
                let root = unsafe {
                    IORegisterForSystemPower(
                        listener.cast(),
                        &raw mut port,
                        Some(heard),
                        &raw mut notifier,
                    )
                };
                if root == 0 {
                    // SAFETY: nothing was registered, so nothing else holds `listener`.
                    drop(unsafe { Box::from_raw(listener) });
                    let _ = told.send(None);
                    return;
                }
                // SAFETY: no callback runs before this thread runs its run loop below.
                unsafe { (*listener).root = root };
                let mut context = CFRunLoopSourceContext {
                    version: 0,
                    info: std::ptr::null_mut(),
                    retain: None,
                    release: None,
                    copyDescription: None,
                    equal: None,
                    hash: None,
                    schedule: None,
                    cancel: None,
                    perform: stop_here,
                };
                // SAFETY: `port` is the live port the registration made; its source is
                // borrowed while the port lives, and this thread's run loop is its own. The stop
                // source is this thread's (the Create rule), its context copied at creation;
                // the run loop and it are each retained once more for the `Stop`, which
                // releases them.
                let (run_loop, source, stop) = unsafe {
                    let source = IONotificationPortGetRunLoopSource(port);
                    let run_loop = CFRunLoopGetCurrent();
                    CFRunLoopAddSource(run_loop, source, kCFRunLoopDefaultMode);
                    let stop = CFRunLoopSourceCreate(std::ptr::null(), 0, &raw mut context);
                    if !stop.is_null() {
                        CFRunLoopAddSource(run_loop, stop, kCFRunLoopDefaultMode);
                        CFRetain(run_loop.cast_const().cast());
                        CFRetain(stop.cast_const().cast());
                    }
                    (run_loop, source, stop)
                };
                if stop.is_null() {
                    let _ = told.send(None);
                } else {
                    let stopped = Arc::new(AtomicBool::new(false));
                    // A `Stop` nobody received is dropped here, which marks this thread
                    // stopped: it then never runs its run loop.
                    let _ = told.send(Some(Stop {
                        run_loop,
                        source: stop,
                        stopped: Arc::clone(&stopped),
                    }));
                    while !stopped.load(Ordering::Acquire) {
                        // SAFETY: this thread's own run loop, in the mode its sources were
                        // added to.
                        unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, FOREVER, 0) };
                    }
                }
                // SAFETY: the teardown IOPMLib.h orders: remove the source, deregister the
                // notifier, close the session, destroy the port; no callback runs after the
                // run loop stopped running, so `listener` is freed last, by this thread alone.
                // The stop source leaves the run loop and is released by its creator (the
                // `Stop` released its own reference, or never got one).
                unsafe {
                    if !stop.is_null() {
                        CFRunLoopRemoveSource(run_loop, stop, kCFRunLoopDefaultMode);
                        CFRunLoopSourceInvalidate(stop);
                        CFRelease(stop.cast_const().cast());
                    }
                    CFRunLoopRemoveSource(run_loop, source, kCFRunLoopDefaultMode);
                    IODeregisterForSystemPower(&raw mut notifier);
                    IOServiceClose(root);
                    IONotificationPortDestroy(port);
                    drop(Box::from_raw(listener));
                }
            });
        spawned.ok()?;
        handed.recv().ok().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// What one [`answer`] did, in order.
    fn answered(message: Message, panics: bool) -> Vec<String> {
        let said = Mutex::new(Vec::new());
        let mut handler = |power: Power| {
            said.lock().expect("said").push(format!("{power:?}"));
            assert!(!panics, "a handler that fails");
        };
        answer(message, &mut handler, || {
            said.lock().expect("said").push("allowed".to_owned());
        });
        said.into_inner().expect("said")
    }

    /// A will-sleep reaches the handler and is acknowledged only after it returned, also when
    /// it panicked; a can-sleep is acknowledged at once and reaches no handler; a
    /// will-not-sleep and a wake reach it as a wake and are not acknowledged.
    /// NEGATIVE CONTROL: acknowledging a will-sleep before handing it on (`allow();
    /// hand(Power::WillSleep);`) fails the first assertion: `["allowed", "WillSleep"]`.
    #[test]
    fn a_will_sleep_is_acknowledged_only_once_its_handler_returned() {
        assert_eq!(
            answered(Message::WillSleep, false),
            ["WillSleep", "allowed"]
        );
        assert_eq!(answered(Message::WillSleep, true), ["WillSleep", "allowed"]);
        assert_eq!(answered(Message::CanSleep, false), ["allowed"]);
        assert_eq!(answered(Message::WillNotSleep, false), ["Woke"]);
        assert_eq!(answered(Message::HasPoweredOn, false), ["Woke"]);
    }

    /// The four IOKit message types [`answer`] takes are read as their messages; any other
    /// (`kIOMessageSystemWillPowerOn`, which nothing waits on) is not.
    /// NEGATIVE CONTROL: swapping `kIOMessageCanSystemSleep`'s arm to `Message::WillSleep`
    /// fails the first assertion: `left: Some(WillSleep)`.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_root_domains_messages_are_read_by_their_type() {
        assert_eq!(imp::message(0xE000_0270), Some(Message::CanSleep));
        assert_eq!(imp::message(0xE000_0280), Some(Message::WillSleep));
        assert_eq!(imp::message(0xE000_0290), Some(Message::WillNotSleep));
        assert_eq!(imp::message(0xE000_0300), Some(Message::HasPoweredOn));
        assert_eq!(
            imp::message(0xE000_0320),
            None,
            "kIOMessageSystemWillPowerOn"
        );
    }

    /// The system takes the registration, and the listener's thread ends (dropping its
    /// handler) once its [`Listening`] is dropped, with no look on a timer: the drop stops its
    /// run loop, also when it comes before the thread first ran it. No sleep is driven: a
    /// handler hears nothing here.
    /// NEGATIVE CONTROL: a `Stop` whose drop marks the thread stopped but signals no source
    /// (`CFRunLoopSourceSignal` and `CFRunLoopWakeUp` left out) leaves the run loop waiting
    /// forever with the handler held, and the hang detector fails the test: "the listener
    /// outlived its owner".
    #[cfg(target_os = "macos")]
    #[test]
    fn a_listener_registers_with_the_root_domain_and_ends_when_dropped() {
        use std::sync::mpsc;
        const HANG_DETECTOR: std::time::Duration = std::time::Duration::from_secs(60);
        let (kept, gone) = mpsc::channel::<()>();
        let listening = listen(Box::new(move |_| {
            let _ = kept.send(());
        }))
        .expect("the root domain takes the registration");
        drop(listening);
        loop {
            match gone.recv_timeout(HANG_DETECTOR) {
                Ok(()) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => panic!("the listener outlived its owner"),
            }
        }
    }
}
