// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
