// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for the routing fabric's per-edge FAIL-CLOSED gate: the
//! shipping `decide_edge` permits only an exact matching grant, with a
//! non-vacuous negative control (op-scope split, wrong dst, stale nonce,
//! unknown token).

use aterm_session::{EdgeDecision, EdgeTable, EdgeToken, LaunchNonce, Op, SessionId, decide_edge};

#[test]
fn decide_edge_conforms_to_fail_closed_gate_with_negative_control() {
    let src = SessionId::new("s-src");
    let dst = SessionId::new("s-dst");
    let other = SessionId::new("s-other");
    let nonce = LaunchNonce::from_bytes([3u8; 16]);
    let stale = LaunchNonce::from_bytes([4u8; 16]);

    let mut tbl = EdgeTable::new();
    let read_tok = tbl.grant(src.clone(), dst.clone(), Op::ReadScreen, nonce);
    let write_tok = tbl.grant(src.clone(), dst.clone(), Op::WriteInput, nonce);

    // Project a real query onto the abstract (granted, decision) and assert the
    // binding-level FailClosed: a Permit implies a matching grant exists, and an
    // exact matching grant permits.
    let check = |tok: &EdgeToken, d: &SessionId, op: Op, n: &LaunchNonce, matching: bool| {
        let permit = decide_edge(&tbl, tok, d, op, n) == EdgeDecision::Permit;
        assert!(
            !permit || matching,
            "decide_edge PERMITTED without a matching grant (op={op:?}, matching={matching})"
        );
        if matching {
            assert!(permit, "an exact matching grant must permit (op={op:?})");
        }
    };

    // Matching grants permit.
    check(&read_tok, &dst, Op::ReadScreen, &nonce, true);
    check(&write_tok, &dst, Op::WriteInput, &nonce, true);

    // NEGATIVE CONTROL (non-vacuous): the op-scope split (§7.2) — a READ token on a
    // WRITE op (and vice-versa) must DENY, never silently widen.
    check(&read_tok, &dst, Op::WriteInput, &nonce, false);
    check(&read_tok, &dst, Op::Signal, &nonce, false);
    check(&write_tok, &dst, Op::ReadScreen, &nonce, false);
    // Wrong dst (a token minted for B cannot drive A), stale nonce (target restarted),
    // and an unknown token all DENY.
    check(&write_tok, &other, Op::WriteInput, &nonce, false);
    check(&write_tok, &dst, Op::WriteInput, &stale, false);
    check(&EdgeToken::generate(), &dst, Op::WriteInput, &nonce, false);
}
