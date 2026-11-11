// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use aterm_policy::engine::PolicyEngine;
use aterm_policy::{OriginTag, Policy, Response};

fn policy_with_rule(sequence: &str, response: Response) -> Policy {
    crate::terminal::policy_bridge::one_rule_policy(
        sequence,
        response,
        OriginTag::Pty,
        Response::Drop,
    )
}

#[test]
fn host_bits_mint_independently_and_default_to_deny() {
    let mints = |auth: &ClipboardAuth| {
        let gates = PolicyGates::compile(None);
        (
            auth.try_mint_write_capability_with_engine(gates).is_some(),
            auth.try_mint_query_capability_with_engine(gates).is_some(),
        )
    };
    let mut auth = ClipboardAuth::new();
    assert_eq!(mints(&auth), (false, false), "deny by default");
    auth.authorize_write();
    assert_eq!(mints(&auth), (true, false));
    auth.authorize_query();
    auth.revoke_write();
    assert_eq!(mints(&auth), (false, true), "revoking write keeps query");
    auth.authorize_write();
    auth.revoke_query();
    assert_eq!(mints(&auth), (true, false), "revoking query keeps write");
    auth.revoke_write();
    assert_eq!(mints(&auth), (false, false));
}

#[test]
fn explicit_osc52_set_rule_still_allows_revoked_write_access() {
    let auth = ClipboardAuth::new();
    let engine = PolicyEngine::new(policy_with_rule("OSC 52 set", Response::Execute));

    assert!(
        auth.try_mint_write_capability_with_engine(PolicyGates::compile(Some(&engine)))
            .is_some()
    );
}

#[test]
fn explicit_osc52_query_rule_still_allows_revoked_query_access() {
    let auth = ClipboardAuth::new();
    let engine = PolicyEngine::new(policy_with_rule("OSC 52 query", Response::Execute));

    assert!(
        auth.try_mint_query_capability_with_engine(PolicyGates::compile(Some(&engine)))
            .is_some()
    );
}
