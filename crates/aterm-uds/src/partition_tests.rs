// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_listed_address_is_unreachable_and_every_other_one_passes() {
    let cut = "# island B\n127.0.0.1:7484\n\n  127.0.0.1:7100  \nnot an address\n[::ffff:127.0.0.1]:41384\n";
    let at = |s: &str| s.parse::<SocketAddr>().unwrap();
    for listed_one in ["127.0.0.1:7484", "127.0.0.1:7100", "127.0.0.1:41384"] {
        let refused = cut_here(cut, || check(at(listed_one))).unwrap_err();
        assert_eq!(
            refused.kind(),
            io::ErrorKind::HostUnreachable,
            "{listed_one}"
        );
        assert!(refused.to_string().contains(SEAM), "{refused}");
    }
    for other in ["127.0.0.1:7485", "[::1]:7484", "10.0.0.1:7100"] {
        assert!(cut_here(cut, || check(at(other))).is_ok(), "{other}");
    }
    assert!(
        cut_here("", || check(at("127.0.0.1:7484"))).is_ok(),
        "healed"
    );
    assert!(check(at("127.0.0.1:7484")).is_ok(), "no cut on this thread");
}
