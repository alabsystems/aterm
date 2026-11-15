// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::sender;
use crate::body::Body;

fn from(f: &str) -> Body {
    Body {
        from: Some(f.to_string()),
        ..Body::default()
    }
}

/// The sim names the sender of a node-written record as the bridge does (`fabric::render_from`):
/// the session its `from=` names, in either tab-ID form; a malformed ID of either form, or a
/// `from=` on a session's own lane, leaves the sender the subject's `src`.
#[test]
fn a_node_written_record_is_sent_by_the_session_its_from_names_in_either_form() {
    for sid in ["s-0123456789abcdef0123", "tqn5r6e0148"] {
        assert_eq!(sender("n-a", &from(sid)), sid);
        assert_eq!(
            sender("s-0123456789abcdef0123", &from(sid)),
            "s-0123456789abcdef0123"
        );
    }
    for malformed in ["s-0123456789ABCDEF0123", "tqn5r6e014i", "tqn5r6e01", "s-"] {
        assert_eq!(sender("n-a", &from(malformed)), "n-a", "{malformed}");
    }
    assert_eq!(sender("n-a", &Body::default()), "n-a");
}
