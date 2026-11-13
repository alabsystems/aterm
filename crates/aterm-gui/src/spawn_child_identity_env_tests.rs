// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    provision_child_identity_env, provision_child_identity_env_with, provision_child_recursion_env,
};
use aterm_session::SessionId;
use aterm_types::domain::{ENV_LAUNCH_NONCE, ENV_PARENT_SESSION_ID, ENV_SESSION_ID};

fn keys(env: &[(String, String)]) -> Vec<&str> {
    env.iter().map(|(k, _)| k.as_str()).collect()
}

/// NEGATIVE CONTROL: absent `$TMUX` and `$STY` still emit the `"|"` delimiter rather than omitting `ENV_MUX_BASE`.
#[test]
fn identity_env_carries_the_parent_id_and_the_multiplexer_baseline() {
    let parent = SessionId::generate();
    for (tmux, sty, want_base) in [
        (None, None, "|"),
        (
            Some("/tmp/tmux-501/default,123,0"),
            None,
            "/tmp/tmux-501/default,123,0|",
        ),
        (None, Some("456.pts-0.host"), "|456.pts-0.host"),
        (
            Some("/tmp/tmux-501/default,123,0"),
            Some("456.pts-0.host"),
            "/tmp/tmux-501/default,123,0|456.pts-0.host",
        ),
    ] {
        let env = provision_child_identity_env_with(&parent, tmux, sty);
        assert_eq!(
            keys(&env),
            vec![ENV_PARENT_SESSION_ID, aterm_types::domain::ENV_MUX_BASE],
            "identity is WHO spawned it plus WHAT it was born into — both pure \
             facts, neither a capability"
        );
        assert_eq!(env[0].1, parent.as_str());
        assert_eq!(env[1].1, want_base);
    }

    let live = provision_child_identity_env(&parent);
    assert_eq!(
        keys(&live),
        vec![ENV_PARENT_SESSION_ID, aterm_types::domain::ENV_MUX_BASE]
    );
    assert_eq!(live[0].1, parent.as_str());
    assert!(live[1].1.contains('|'), "got {:?}", live[1].1);
}

#[test]
fn controller_observe_env_is_the_origin_sid_only_and_one_hop() {
    use aterm_types::domain::{ENV_OBSERVE_SESSION_ID, is_ai_env_var};
    let origin = SessionId::generate();
    let env = super::provision_observe_env(&origin);
    assert_eq!(keys(&env), vec![ENV_OBSERVE_SESSION_ID]);
    assert_eq!(env[0].1, origin.as_str());
    assert!(
        is_ai_env_var(ENV_OBSERVE_SESSION_ID),
        "the hint must be stripped on inheritance — a grandchild is not the controller"
    );
}

#[test]
fn identity_plus_recursion_covers_the_original_contract() {
    let parent = SessionId::generate();
    let mut env = provision_child_identity_env(&parent);
    let (rec, _prov) = provision_child_recursion_env();
    env.extend(rec);
    let k = keys(&env);
    for want in [ENV_PARENT_SESSION_ID, ENV_SESSION_ID, ENV_LAUNCH_NONCE] {
        assert!(k.contains(&want), "missing {want} after split");
    }
    let mut sorted = k.clone();
    sorted.sort_unstable();
    let before = sorted.len();
    sorted.dedup();
    assert_eq!(before, sorted.len(), "no key may be emitted twice");
}
