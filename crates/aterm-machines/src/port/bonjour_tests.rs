// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_txt_record_is_its_length_prefixed_strings_and_a_cut_one_ends_them() {
    assert_eq!(
        txt_strings(b"\x0bmodel=Mac16\x0aosxvers=26"),
        [b"model=Mac16".to_vec(), b"osxvers=26".to_vec()]
    );
    assert_eq!(
        txt_strings(b"\x00\x02ab\x09cut"),
        [Vec::new(), b"ab".to_vec()]
    );
    assert!(txt_strings(b"").is_empty());
}

/// Without a broker, a request is the broker's own words, never a direct Bonjour call: this
/// test binary's broker socket (`aterm_uds::lan::socket_path`, under its own short root) is
/// served by nothing.
#[cfg(target_os = "macos")]
#[test]
fn without_a_broker_a_request_names_it() {
    let why = live(
        &Bonjour::Browse("_ssh._tcp".into()),
        Duration::from_millis(50),
    )
    .expect_err("nothing serves that socket");
    assert_eq!(
        why,
        aterm_uds::lan::Unavailable::NotInstalled.to_string(),
        "{why}"
    );
}
