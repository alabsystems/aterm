// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn only_an_updates_relaunch_withholds_the_bootstrap_payload() {
    let payload = Some(vec![std::ffi::OsString::from("./deploy.sh")]);
    let mut factory = payload.clone();
    assert_eq!(super::withhold_bootstrap_payload(&mut factory, false), None);
    assert_eq!(
        factory, payload,
        "a person's launch runs it in its first tab"
    );
    let withheld = super::withhold_bootstrap_payload(&mut factory, true);
    assert_eq!(factory, None, "session 0 of a relaunch spawns without it");
    assert_eq!(withheld, payload, "…and it is kept to be put back");
}
