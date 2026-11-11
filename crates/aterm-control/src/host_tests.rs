// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A roster selector reads as the wire grammar reads its body: a local sid
/// stays one, an `Id` spelled as a stable id is that id, and any other `Id`
/// names no session.
#[test]
fn selector_reads_as_the_wire_grammar() {
    use aterm_types::control_request::{Selector as Wire, Sid};
    assert_eq!(Wire::from(Selector::Local(12)), Wire::Local(12));
    let sid = Sid::parse("s-0f1e").expect("a stable id");
    assert_eq!(Wire::from(Selector::Id("s-0f1e")), Wire::Sid(sid));
    assert_eq!(Wire::from(Selector::Id("S-0F1E")), Wire::Invalid("S-0F1E"));
    assert_eq!(Selector::Local(12).local_sid(), Some(12));
    assert_eq!(Selector::Id("s-0f1e").local_sid(), None);
    assert_eq!(Selector::Id("s-0f1e").stable_id(), Some("s-0f1e"));
    assert_eq!(Selector::Local(12).stable_id(), None);
}
