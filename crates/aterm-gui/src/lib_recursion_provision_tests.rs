// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::spawn::{
    install_parent_edges, is_valid_session_id, parse_injected_identity,
    provision_child_identity_env, provision_child_recursion_env,
};
use aterm_session::{EdgeTable, LaunchNonce, Op, SessionId};

#[test]
fn session_id_shape_validation() {
    assert!(is_valid_session_id(SessionId::generate().as_str()));
    assert!(!is_valid_session_id("nope"));
    assert!(!is_valid_session_id("s-xyz"));
    assert!(!is_valid_session_id("s-")); // no hex
    assert!(!is_valid_session_id(&format!("s-{}", "g".repeat(20)))); // non-hex
    assert!(!is_valid_session_id(&format!("s-{}", "A".repeat(20)))); // not minted
}

#[test]
fn adopt_injected_identity_roundtrips_and_fails_closed() {
    let sid = SessionId::generate();
    let nonce = LaunchNonce::generate();
    let got = parse_injected_identity(Some(sid.as_str()), Some(&nonce.to_hex()))
        .expect("valid pair adopts");
    assert_eq!(got.0.as_str(), sid.as_str());
    assert!(got.1.ct_eq(&nonce));
    assert!(parse_injected_identity(Some(sid.as_str()), None).is_none());
    assert!(parse_injected_identity(None, Some(&nonce.to_hex())).is_none());
    assert!(parse_injected_identity(Some("bad"), Some(&nonce.to_hex())).is_none());
    assert!(parse_injected_identity(Some(sid.as_str()), Some("xyz")).is_none());
}

#[test]
fn provision_child_env_is_symmetric_across_all_three_ops() {
    let parent = SessionId::generate();
    let mut env = provision_child_identity_env(&parent);
    let (recursion_env, prov) = provision_child_recursion_env();
    env.extend(recursion_env);
    let get = |k: &str| env.iter().find(|(ek, _)| ek == k).map(|(_, v)| v.clone());

    let (child_sid, child_nonce) = parse_injected_identity(
        get("ATERM_SESSION_ID").as_deref(),
        get("ATERM_LAUNCH_NONCE").as_deref(),
    )
    .expect("child adopts injected identity");
    assert!(
        get("ATERM_EDGE_READ").is_none(),
        "edge secrets must NOT be in the identity env (F1)"
    );
    let mut table = EdgeTable::new();
    let n = install_parent_edges(
        &mut table,
        &child_sid,
        &child_nonce,
        get("ATERM_PARENT_SESSION_ID").as_deref(),
        Some(&prov.read.to_hex()),
        Some(&prov.write.to_hex()),
        Some(&prov.signal.to_hex()),
    );
    assert_eq!(n, 3, "all three op edges recorded");

    assert_eq!(
        table.authorize(&prov.read, &child_sid, &child_nonce),
        Some(Op::ReadScreen)
    );
    assert_eq!(
        table.authorize(&prov.write, &child_sid, &child_nonce),
        Some(Op::WriteInput)
    );
    assert_eq!(
        table.authorize(&prov.signal, &child_sid, &child_nonce),
        Some(Op::Signal)
    );

    let stale = LaunchNonce::generate();
    assert_eq!(table.authorize(&prov.read, &child_sid, &stale), None);
    assert_eq!(
        table.authorize(
            &aterm_session::EdgeToken::generate(),
            &child_sid,
            &child_nonce
        ),
        None,
    );
}

#[test]
fn install_parent_edges_requires_parent_id() {
    let mut table = EdgeTable::new();
    let n = install_parent_edges(
        &mut table,
        &SessionId::generate(),
        &LaunchNonce::generate(),
        None,
        Some(&aterm_session::EdgeToken::generate().to_hex()),
        None,
        None,
    );
    assert_eq!(n, 0);
}
