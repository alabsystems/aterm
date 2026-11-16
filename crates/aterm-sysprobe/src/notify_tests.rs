// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

// The test's own state, in libSystem's notify (`notify.h`): 0 on success; and the key's name
// as `OSThermalNotification.h` exports it.
#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn notify_set_state(token: std::ffi::c_int, state: u64) -> u32;
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
    // SAFETY: the watch's live token.
    unsafe {
        assert_eq!(notify_set_state(token, 42), 0);
    }
    assert!(post(&name), "notifyd takes the post");
    // notifyd delivers the post on its own time: a one-minute hang detector.
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the post to arrive", || {
        (quiet.state() == Ok(42)).then_some(())
    });
    assert_eq!(quiet.get_states, 2, "control: one post, one read");
}

/// A [`Posts`] hears a post of its own name once and no post of another name, and its first
/// look (taken as it registers) is spent: nothing was posted since. A post of the other name
/// waits until that name's own ear heard it, so notifyd had handled it when this ear is asked.
#[cfg(target_os = "macos")]
#[test]
fn posts_hears_its_own_name_and_no_other() {
    let named = |which: &str| {
        std::ffi::CString::new(format!(
            "com.aterm.sysprobe.test.posts-{which}-{}",
            std::process::id()
        ))
        .expect("a name without NUL")
    };
    let (mine, other) = (named("mine"), named("other"));
    let mut ear = Posts::new(&mine).expect("registers");
    let mut other_ear = Posts::new(&other).expect("registers");
    assert_eq!(ear.posted(), Ok(false), "the first look is spent");

    let heard = |ear: &mut Posts, what: &str| {
        // notifyd delivers the post on its own time: a one-minute hang detector.
        aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, what, || {
            (ear.posted() == Ok(true)).then_some(())
        });
    };
    assert!(post(&other));
    heard(&mut other_ear, "the other name's post");
    assert_eq!(ear.posted(), Ok(false), "another name's post is not heard");
    assert!(post(&mine));
    heard(&mut ear, "its own post");
    assert_eq!(ear.posted(), Ok(false), "one post, heard once");
}

/// Off macOS a watch says it has no source, never a level, and a post goes nowhere.
#[cfg(not(target_os = "macos"))]
#[test]
fn a_system_without_notify_reads_unsupported() {
    assert_eq!(thermal_pressure().state(), Err("unsupported"));
    assert!(!post(c"com.aterm.sysprobe.test.notify-none"));
}
