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
use std::sync::Weak;

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

/// Hear this Mac's sleep and wake on a thread of its own, handing each announcement to
/// `handler` there and answering the system as [`answer`] does, until `alive` has no strong
/// reference left (the thread looks every few seconds). Whether the system took the
/// registration. Elsewhere than macOS: `false`, and nothing is heard.
pub fn listen<T: Send + Sync + 'static>(
    alive: Weak<T>,
    handler: Box<dyn FnMut(Power) + Send>,
) -> bool {
    imp::listen(alive, handler)
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::Power;
    use std::sync::Weak;

    pub(super) fn listen<T>(_alive: Weak<T>, _handler: Box<dyn FnMut(Power) + Send>) -> bool {
        false
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::iokit::{
        CFRunLoopAddSource, CFRunLoopGetCurrent, CFRunLoopRemoveSource, CFRunLoopRunInMode,
        IOAllowPowerChange, IODeregisterForSystemPower, IONotificationPortDestroy,
        IONotificationPortGetRunLoopSource, IONotificationPortRef, IORegisterForSystemPower,
        IOServiceClose, io_connect_t, io_object_t, io_service_t, kCFRunLoopDefaultMode,
        kIOMessageCanSystemSleep, kIOMessageSystemHasPoweredOn, kIOMessageSystemWillNotSleep,
        kIOMessageSystemWillSleep,
    };
    use super::{Message, Power, answer};
    use std::ffi::c_void;
    use std::sync::{Weak, mpsc};
    use std::time::Duration;

    /// How long the listener's run loop runs between two looks at whether its owner is gone.
    const LOOK: Duration = Duration::from_secs(5);

    /// What the callback reaches through its refcon: the root domain session it answers on,
    /// and the handler.
    struct Listener {
        root: io_connect_t,
        handler: Box<dyn FnMut(Power) + Send>,
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
    pub(super) fn listen<T: Send + Sync + 'static>(
        alive: Weak<T>,
        handler: Box<dyn FnMut(Power) + Send>,
    ) -> bool {
        let (told, registered) = mpsc::sync_channel(1);
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
                    let _ = told.send(false);
                    return;
                }
                // SAFETY: no callback runs before this thread runs its run loop below.
                unsafe { (*listener).root = root };
                // SAFETY: `port` is the live port the registration made; its source is
                // borrowed while the port lives, and this thread's run loop is its own.
                let (run_loop, source) = unsafe {
                    let source = IONotificationPortGetRunLoopSource(port);
                    let run_loop = CFRunLoopGetCurrent();
                    CFRunLoopAddSource(run_loop, source, kCFRunLoopDefaultMode);
                    (run_loop, source)
                };
                let _ = told.send(true);
                while alive.strong_count() > 0 {
                    // SAFETY: this thread's own run loop, in the mode its source was added to.
                    unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, LOOK.as_secs_f64(), 0) };
                }
                // SAFETY: the teardown IOPMLib.h orders: remove the source, deregister the
                // notifier, close the session, destroy the port; no callback runs after the
                // run loop stopped running, so `listener` is freed last, by this thread alone.
                unsafe {
                    CFRunLoopRemoveSource(run_loop, source, kCFRunLoopDefaultMode);
                    IODeregisterForSystemPower(&raw mut notifier);
                    IOServiceClose(root);
                    IONotificationPortDestroy(port);
                    drop(Box::from_raw(listener));
                }
            });
        spawned.is_ok() && registered.recv().unwrap_or(false)
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
    /// handler) once its owner is gone. No sleep is driven: a handler hears nothing here.
    /// NEGATIVE CONTROL: a listener loop that runs on with its owner gone (`while
    /// alive.strong_count() < usize::MAX`) keeps the handler, and the hang detector fails the
    /// test: "the listener outlived its owner".
    #[cfg(target_os = "macos")]
    #[test]
    fn a_listener_registers_with_the_root_domain_and_ends_with_its_owner() {
        use std::sync::{Arc, mpsc};
        const HANG_DETECTOR: std::time::Duration = std::time::Duration::from_secs(60);
        let owner = Arc::new(());
        let (kept, gone) = mpsc::channel::<()>();
        assert!(listen(
            Arc::downgrade(&owner),
            Box::new(move |_| {
                let _ = kept.send(());
            })
        ));
        drop(owner);
        loop {
            match gone.recv_timeout(HANG_DETECTOR) {
                Ok(()) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => panic!("the listener outlived its owner"),
            }
        }
    }
}
