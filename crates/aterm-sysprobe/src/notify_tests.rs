// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

// The test's own posts, in libSystem's notify (`notify.h`): 0 on success; and the key's name
// as `OSThermalNotification.h` exports it.
#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn notify_set_state(token: std::ffi::c_int, state: u64) -> u32;
    fn notify_post(name: *const std::ffi::c_char) -> u32;
    #[link_name = "kOSThermalNotificationPressureLevelName"]
    static PRESSURE_LEVEL_NAME: *const std::ffi::c_char;
}

/// The live key is the one `OSThermalNotification.h` names (any other name registers and reads
/// 0, Nominal, for good) and reads a level the model knows. A key is read (`notify_get_state`)
/// at its first check and then only on a post: reads with nothing posted call it no more and
/// keep the state. CONTROL: a post of a private key, with a new state, is read by exactly one
/// call, which answers the new state.
#[cfg(target_os = "macos")]
#[test]
fn the_pressure_key_reads_a_documented_level_and_rereads_on_change() {
    // SAFETY: libSystem's exported constant, a NUL-terminated string it keeps for the process.
    let documented = unsafe { CStr::from_ptr(PRESSURE_LEVEL_NAME) };
    assert_eq!(THERMAL_PRESSURE, documented);
    let mut w = thermal_pressure();
    let first = w.state().expect("the documented key registers and reads");
    assert!(first <= 4, "level {first}");

    let name = std::ffi::CString::new(format!(
        "com.aterm.sysprobe.test.notify-{}",
        std::process::id()
    ))
    .expect("a name without NUL");
    let mut quiet = Watch::new(&name);
    assert_eq!(
        quiet.state(),
        Ok(0),
        "a key nobody posted reads its initial state"
    );
    assert_eq!(quiet.get_states, 1, "the first check reads");
    for _ in 0..3 {
        assert_eq!(quiet.state(), Ok(0));
    }
    assert_eq!(quiet.get_states, 1, "nothing posted, nothing read");

    let token = quiet.token.expect("registered");
    // SAFETY: the watch's live token, and a NUL-terminated name.
    unsafe {
        assert_eq!(notify_set_state(token, 42), 0);
        assert_eq!(notify_post(name.as_ptr()), 0);
    }
    // notifyd delivers the post on its own time: a one-minute hang detector.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while quiet.state() != Ok(42) {
        assert!(
            std::time::Instant::now() < deadline,
            "the post never arrived"
        );
        aterm_uds::deadline::poll_yield();
    }
    assert_eq!(quiet.get_states, 2, "control: one post, one read");
}

/// Off macOS a watch says it has no source, never a level.
#[cfg(not(target_os = "macos"))]
#[test]
fn a_system_without_notify_reads_unsupported() {
    assert_eq!(thermal_pressure().state(), Err("unsupported"));
}
