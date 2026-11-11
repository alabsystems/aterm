// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn ids() -> (SessionId, SessionId, LaunchNonce) {
    (
        SessionId::new("s-a"),
        SessionId::new("s-b"),
        LaunchNonce::from_bytes([7u8; 16]),
    )
}

#[test]
fn op_wire_tokens_roundtrip_and_unknown_fails_closed() {
    for op in [
        Op::ReadScreen,
        Op::WriteInput,
        Op::Signal,
        Op::ConfigWrite,
        Op::ClipboardWrite,
        Op::DeriveLoop,
    ] {
        assert_eq!(Op::parse(op.as_str()), Some(op));
    }
    assert_eq!(
        Op::parse("write-keys"),
        None,
        "an unknown op must not parse"
    );
    assert_eq!(Op::parse(""), None);
}

/// Retained grants keep their token identity and remain revocable;
/// grants for other operations are removed from the same table.
#[test]
fn retaining_an_op_preserves_its_grants_and_removes_others() {
    let (a, b, nonce) = ids();
    let mut tbl = EdgeTable::new();
    let read = tbl.grant(a.clone(), b.clone(), Op::ReadScreen, nonce);
    let write = tbl.grant(a.clone(), b.clone(), Op::WriteInput, nonce);
    assert!(
        decide_edge(&tbl, &write, &b, Op::WriteInput, &nonce).is_permitted(),
        "positive control: the write was granted"
    );
    tbl.keep_only(Op::ReadScreen);
    assert_eq!(tbl.len(), 1);
    assert!(decide_edge(&tbl, &read, &b, Op::ReadScreen, &nonce).is_permitted());
    assert!(!decide_edge(&tbl, &write, &b, Op::WriteInput, &nonce).is_permitted());
    assert!(!tbl.revoke(&write), "the write grant was removed");
    assert_eq!(tbl.revoke_src(&a), 1);
    assert!(!decide_edge(&tbl, &read, &b, Op::ReadScreen, &nonce).is_permitted());
}

#[test]
fn decide_edge_permits_only_exact_match() {
    let (a, b, nonce) = ids();
    let mut tbl = EdgeTable::new();
    let tok = tbl.grant(a.clone(), b.clone(), Op::WriteInput, nonce);

    assert_eq!(
        decide_edge(&tbl, &tok, &b, Op::WriteInput, &nonce),
        EdgeDecision::Permit
    );

    assert_eq!(
        decide_edge(&tbl, &tok, &b, Op::ReadScreen, &nonce),
        EdgeDecision::Deny
    );
    assert_eq!(
        decide_edge(&tbl, &tok, &b, Op::Signal, &nonce),
        EdgeDecision::Deny
    );

    assert_eq!(
        decide_edge(&tbl, &tok, &a, Op::WriteInput, &nonce),
        EdgeDecision::Deny
    );

    let restarted = LaunchNonce::from_bytes([9u8; 16]);
    assert_eq!(
        decide_edge(&tbl, &tok, &b, Op::WriteInput, &restarted),
        EdgeDecision::Deny
    );

    let mut ungranted = *tok.as_bytes();
    ungranted[0] ^= 0xff;
    let unknown = EdgeToken::from_bytes(ungranted);
    assert_eq!(
        decide_edge(&tbl, &unknown, &b, Op::WriteInput, &nonce),
        EdgeDecision::Deny
    );
}

#[test]
fn revoke_makes_the_edge_deny() {
    let (a, b, nonce) = ids();
    let mut tbl = EdgeTable::new();
    let tok = tbl.grant(a, b.clone(), Op::ReadScreen, nonce);
    assert!(decide_edge(&tbl, &tok, &b, Op::ReadScreen, &nonce).is_permitted());
    assert!(tbl.revoke(&tok), "revoke removes a live edge");
    assert!(!tbl.revoke(&tok), "double-revoke is a no-op");
    assert_eq!(
        decide_edge(&tbl, &tok, &b, Op::ReadScreen, &nonce),
        EdgeDecision::Deny
    );
}

#[test]
fn authorize_returns_op_for_a_live_edge_else_none() {
    let (a, b, nonce) = ids();
    let mut tbl = EdgeTable::new();
    let tok = tbl.grant(a, b.clone(), Op::ReadScreen, nonce);
    assert_eq!(tbl.authorize(&tok, &b, &nonce), Some(Op::ReadScreen));
    assert_eq!(tbl.authorize(&tok, &SessionId::new("s-x"), &nonce), None);
    assert_eq!(
        tbl.authorize(&tok, &b, &LaunchNonce::from_bytes([0u8; 16])),
        None
    );
    let mut ungranted = *tok.as_bytes();
    ungranted[0] ^= 0xff;
    let unknown = EdgeToken::from_bytes(ungranted);
    assert_eq!(tbl.authorize(&unknown, &b, &nonce), None);
}

#[test]
fn edges_enumerates_rows_without_leaking_tokens() {
    let (a, b, nonce) = ids();
    let mut tbl = EdgeTable::new();
    assert!(tbl.edges().is_empty(), "empty table enumerates nothing");
    let _t1 = tbl.grant(a.clone(), b.clone(), Op::ReadScreen, nonce);
    let _t2 = tbl.grant(a.clone(), b.clone(), Op::WriteInput, nonce);

    let mut got = tbl.edges();
    assert_eq!(got.len(), 2, "both granted edges enumerate");
    got.sort_by_key(|e| e.op.as_str());
    assert_eq!(
        got[0],
        Edge {
            src: a.clone(),
            dst: b.clone(),
            op: Op::ReadScreen
        }
    );
    assert_eq!(
        got[1],
        Edge {
            src: a.clone(),
            dst: b.clone(),
            op: Op::WriteInput
        }
    );

    let to_revoke = *tbl.rows.keys().next().expect("a token");
    tbl.revoke(&to_revoke);
    assert_eq!(tbl.edges().len(), 1, "revoked edge no longer enumerates");
}

#[test]
fn edge_of_reads_the_row_identity_without_the_token() {
    let (a, b, nonce) = ids();
    let mut tbl = EdgeTable::new();
    let tok = tbl.grant(a.clone(), b.clone(), Op::Signal, nonce);
    assert_eq!(
        tbl.edge_of(&tok),
        Some(Edge {
            src: a,
            dst: b,
            op: Op::Signal
        })
    );
    let mut ungranted = *tok.as_bytes();
    ungranted[0] ^= 0xff;
    let unknown = EdgeToken::from_bytes(ungranted);
    assert_eq!(tbl.edge_of(&unknown), None);
    tbl.revoke(&tok);
    assert_eq!(tbl.edge_of(&tok), None);
}

#[test]
fn authority_does_not_flow_along_a_cycle_or_self_loop() {
    let a = SessionId::new("s-a");
    let b = SessionId::new("s-b");
    let na = LaunchNonce::from_bytes([1u8; 16]);
    let nb = LaunchNonce::from_bytes([2u8; 16]);

    let mut tbl_b = EdgeTable::new();
    let mut tbl_a = EdgeTable::new();
    let tok_ab = tbl_b.grant(a.clone(), b.clone(), Op::WriteInput, nb);
    let tok_ba = tbl_a.grant(b.clone(), a.clone(), Op::WriteInput, na);

    assert!(decide_edge(&tbl_b, &tok_ab, &b, Op::WriteInput, &nb).is_permitted());
    assert!(decide_edge(&tbl_a, &tok_ba, &a, Op::WriteInput, &na).is_permitted());
    assert_eq!(
        decide_edge(&tbl_a, &tok_ab, &a, Op::WriteInput, &na),
        EdgeDecision::Deny
    );

    let mut tbl_self = EdgeTable::new();
    let tok_aa = tbl_self.grant(a.clone(), a.clone(), Op::WriteInput, na);
    assert!(decide_edge(&tbl_self, &tok_aa, &a, Op::WriteInput, &na).is_permitted());
}

#[test]
fn revoke_src_sweeps_exactly_the_matching_source() {
    let (a, b, nonce) = ids();
    let c = SessionId::new("s-c");
    let mut tbl = EdgeTable::new();
    let t1 = tbl.grant(a.clone(), b.clone(), Op::ReadScreen, nonce);
    let t2 = tbl.grant(a.clone(), b.clone(), Op::WriteInput, nonce);
    let t3 = tbl.grant(c.clone(), b.clone(), Op::WriteInput, nonce);

    assert_eq!(tbl.revoke_src(&a), 2);
    assert_eq!(
        decide_edge(&tbl, &t1, &b, Op::ReadScreen, &nonce),
        EdgeDecision::Deny
    );
    assert_eq!(
        decide_edge(&tbl, &t2, &b, Op::WriteInput, &nonce),
        EdgeDecision::Deny
    );
    assert!(decide_edge(&tbl, &t3, &b, Op::WriteInput, &nonce).is_permitted());

    assert_eq!(tbl.revoke_src(&a), 0);
    assert_eq!(tbl.len(), 1);
}

#[test]
fn revoke_src_ops_filters_by_op() {
    let (a, b, nonce) = ids();
    let c = SessionId::new("s-c");
    let mut tbl = EdgeTable::new();
    let read = tbl.grant(a.clone(), b.clone(), Op::ReadScreen, nonce);
    let write = tbl.grant(a.clone(), b.clone(), Op::WriteInput, nonce);
    let sig = tbl.grant(a.clone(), b.clone(), Op::Signal, nonce);
    let other = tbl.grant(c.clone(), b.clone(), Op::WriteInput, nonce);

    assert_eq!(
        tbl.revoke_src_ops(&a, Some(&[Op::WriteInput, Op::Signal])),
        2
    );
    assert!(decide_edge(&tbl, &read, &b, Op::ReadScreen, &nonce).is_permitted());
    assert_eq!(
        decide_edge(&tbl, &write, &b, Op::WriteInput, &nonce),
        EdgeDecision::Deny
    );
    assert_eq!(
        decide_edge(&tbl, &sig, &b, Op::Signal, &nonce),
        EdgeDecision::Deny
    );
    assert!(decide_edge(&tbl, &other, &b, Op::WriteInput, &nonce).is_permitted());

    assert_eq!(tbl.revoke_src_ops(&a, Some(&[Op::Signal])), 0);
    assert_eq!(tbl.revoke_src_ops(&a, None), 1);
    assert_eq!(tbl.len(), 1, "only the foreign source's row remains");
}

#[test]
fn connection_kind_ops_are_the_closed_human_fidelity_mapping() {
    assert_eq!(ConnectionKind::Pull.ops(), [Op::ReadScreen]);
    assert_eq!(ConnectionKind::Push.ops(), [Op::WriteInput, Op::Signal]);
    assert_eq!(
        ConnectionKind::Both.ops(),
        [Op::ReadScreen, Op::WriteInput, Op::Signal]
    );
    for kind in [
        ConnectionKind::Pull,
        ConnectionKind::Push,
        ConnectionKind::Both,
    ] {
        for op in [Op::ConfigWrite, Op::ClipboardWrite, Op::DeriveLoop] {
            assert!(
                !kind.ops().contains(&op),
                "{kind:?} must not express {op:?}"
            );
        }
    }
}

#[test]
fn grant_connection_mints_exactly_the_kinds_ops() {
    let (a, b, nonce) = ids();

    let mut tbl = EdgeTable::new();
    let pulled = tbl.grant_connection(&a, &b, ConnectionKind::Pull, &nonce);
    assert_eq!(pulled.len(), 1);
    assert_eq!(pulled[0].0, Op::ReadScreen);
    assert_eq!(tbl.len(), 1, "one row per minted op, nothing extra");

    let mut tbl = EdgeTable::new();
    let both = tbl.grant_connection(&a, &b, ConnectionKind::Both, &nonce);
    let ops: Vec<Op> = both.iter().map(|(op, _)| *op).collect();
    assert_eq!(ops, [Op::ReadScreen, Op::WriteInput, Op::Signal]);
    assert_eq!(tbl.len(), 3);
}

#[test]
fn grant_connection_refuses_a_self_loop() {
    let (a, _b, nonce) = ids();
    let mut tbl = EdgeTable::new();
    for kind in [
        ConnectionKind::Pull,
        ConnectionKind::Push,
        ConnectionKind::Both,
    ] {
        assert!(
            tbl.grant_connection(&a, &a, kind, &nonce).is_empty(),
            "{kind:?} self-loop must mint nothing"
        );
    }
    assert!(tbl.is_empty(), "a refused self-loop leaves no row behind");
}

#[test]
fn grant_connection_tokens_authorize_and_revoke_src_kills_them() {
    let (a, b, nonce) = ids();
    let mut tbl = EdgeTable::new();
    let minted = tbl.grant_connection(&a, &b, ConnectionKind::Push, &nonce);
    assert_eq!(minted.len(), 2);

    for (op, tok) in &minted {
        assert!(decide_edge(&tbl, tok, &b, *op, &nonce).is_permitted());
    }

    assert_eq!(tbl.revoke_src(&a), 2);
    for (op, tok) in &minted {
        assert_eq!(decide_edge(&tbl, tok, &b, *op, &nonce), EdgeDecision::Deny);
    }
}
