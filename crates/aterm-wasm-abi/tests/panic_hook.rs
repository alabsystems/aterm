// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The installed hook, natively: a real panic reaches the injected console
//! once, in upstream's format, and a second `set_once` changes nothing. Its
//! own test binary, because the hook is process-wide.

use std::sync::Mutex;

use aterm_wasm_abi::panic_hook::{Console, set_once};

static LOGGED: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn record(msg: &str) {
    LOGGED.lock().unwrap().push(msg.to_string());
}

fn other(_: &str) {
    LOGGED
        .lock()
        .unwrap()
        .push("the second console".to_string());
}

#[test]
fn a_panic_reaches_the_injected_console_once_in_upstreams_format() {
    set_once(Console {
        error: record,
        stack: || "Error\n    at smoke (wasm://wasm/0:1)".to_string(),
    });
    set_once(Console {
        error: other,
        stack: String::new,
    });

    let joined = std::thread::spawn(|| panic!("aterm smoke panic {}", 1234)).join();
    // std's hook back, so a failed assertion below is printed, not recorded.
    drop(std::panic::take_hook());
    assert!(joined.is_err(), "the thread panicked");

    let logged = LOGGED.lock().unwrap().clone();
    assert_eq!(logged.len(), 1, "{logged:?}");
    let msg = &logged[0];
    assert!(msg.starts_with("panicked at "), "{msg:?}");
    assert!(msg.contains("tests/panic_hook.rs:"), "{msg:?}");
    assert!(
        msg.ends_with(
            ":\naterm smoke panic 1234\n\nStack:\n\nError\n    at smoke (wasm://wasm/0:1)\n\n"
        ),
        "{msg:?}"
    );
}
