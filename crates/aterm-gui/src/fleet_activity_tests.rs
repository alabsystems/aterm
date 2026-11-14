// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_types::fleet_activity::Plain;

const NODE: &str = "05050505050505050505050505050505";
const SID: &str = "s-0123456789abcdef0123";

/// A driver's label names its host before its node, or its node alone: the record keeps the
/// host and leaves a driver the book names by no host to its node id.
#[test]
fn a_drivers_row_names_the_host_its_label_leads_with() {
    assert_eq!(driver_host("mac-05 (node 05050505)"), Some("mac-05"));
    assert_eq!(
        driver_host("192.168.1.172 (node 87be5228)"),
        Some("192.168.1.172")
    );
    assert_eq!(driver_host("node 05050505"), None);
    let row = |host: &str| {
        driven(
            &crate::fabric::Driver {
                node: NODE.to_owned(),
                host: host.to_owned(),
            },
            DriveKind::Turn,
            Some(SID),
            Outcome::Admitted,
        )
        .expect("a session's request")
    };
    let named = row("mac-05 (node 05050505)");
    assert_eq!(
        (
            named.way,
            named.node.as_str(),
            named.host.as_deref(),
            named.what
        ),
        (Way::In, NODE, Some("mac-05"), Plain::Typed)
    );
    assert_eq!(named.by.as_deref(), Some(SID));
    assert_eq!(row("node 05050505").host, None);
}

/// Mail a window sends another Mac for one of its sessions is an outgoing message that names
/// the session, blocked where the answer's first line is that Mac's lock refusing it; an
/// answer that is neither served nor the lock's refusal (no such session) decided nothing and
/// has no row.
#[cfg(unix)]
#[test]
fn mail_a_window_sends_another_mac_is_an_outgoing_message() {
    use aterm_types::fleet_access::FleetAccess;
    const SENDER: &str = "s-fedcba98765432100123";
    let request = format!("@{SID} post key=k kind=note -- hi");
    let admitted =
        sent((NODE, "m86"), Some(SENDER), &request, "OK posted f1\n").expect("a message");
    assert_eq!(
        (
            admitted.way,
            admitted.node.as_str(),
            admitted.host.as_deref()
        ),
        (Way::Out, NODE, Some("m86"))
    );
    assert_eq!(
        (admitted.what, admitted.outcome),
        (Plain::Message, Outcome::Admitted)
    );
    assert_eq!(
        admitted.by.as_deref(),
        Some(SENDER),
        "the session that sent it, never the one it is for"
    );
    let refusal = FleetAccess::ReadOnly
        .refusal(DriveKind::Post)
        .expect("Read Only refuses a message");
    let refused = format!("{refusal}; not `post`\n");
    let Some(blocked) = sent((NODE, "m86"), Some(SENDER), &request, &refused) else {
        panic!("the lock's refusal is a blocked row: {refused}");
    };
    assert_eq!(
        blocked.outcome,
        Outcome::Blocked {
            level: FleetAccess::ReadOnly,
            needs: FleetAccess::ReadWrite
        }
    );
    assert_eq!(blocked.by.as_deref(), Some(SENDER));
    assert_eq!(
        sent(
            (NODE, "m86"),
            Some(SENDER),
            &request,
            "ERR no such session\n"
        ),
        None
    );
}
