// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A refused Approve says why whole, in a record and not on screen: the refusal of a key the
/// queue lists under two names ends with the command that approves it, past one detail
/// line's cap.
/// NEGATIVE CONTROL: the refusal as one detail line is clipped before its command (asserted
/// below).
#[test]
fn a_refused_approve_says_why_whole_in_a_record() {
    let key = "age1qyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqs3290gq";
    let request = |host: &str| aterm_secrets::client::Request {
        recipient: key.to_string(),
        host: host.to_string(),
        code: "ABCD-EFGH-JKMN-PQRS".to_string(),
        at: None,
        allowed: None,
        proof: None,
    };
    let pending = [request("mc"), request(&"a-name-a-lan-host-chose".repeat(2))];
    let why = aterm_secrets::client::one_name_asked(&pending, &pending[0])
        .expect_err("a key listed under two names")
        .to_string();
    let command = format!("aterm secrets approve {key} --name <host>");
    assert!(why.ends_with(&command), "{why}");
    let said = refused(Some("mc"), &why).normalized();
    assert_eq!(said.title, "Couldn't approve mc");
    assert!(
        matches!(said.need(), aterm_messages::need::Need::Record),
        "a record, not an alert: {:?}",
        said.need()
    );
    let said = said.detail.join(" ");
    assert!(
        said.contains(&format!("Approve did not let it in: {why}")),
        "{said}"
    );
    let clipped = Message::new(tags::PRIVACY, Severity::Warn, "x")
        .line(&why)
        .normalized()
        .detail
        .join(" ");
    assert!(!clipped.contains(&command), "{clipped}");
}
