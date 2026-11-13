// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{provision_child_proxy_from, provision_child_recursion_env};
use aterm_spec::verify::validate_transition_tiered;
use aterm_types::domain::ENV_EDGE_TOKENS;

#[test]
fn edge_secret_provisioning_conforms_to_capability_secrecy() {
    let model = aterm_models_effects::capability_secrecy_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let label = "CapabilitySecrecy(provision_child_proxy)";
    let dir = std::env::temp_dir().join(format!("aterm-f1-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch socket dir");
    for (what, sock_dir) in [("a private dir", Some(dir.as_path())), ("no dir", None)] {
        let minted = provision_child_recursion_env();
        let secrets = [
            minted.1.read.to_hex(),
            minted.1.write.to_hex(),
            minted.1.signal.to_hex(),
        ];
        let (env, _child) = provision_child_proxy_from(minted, sock_dir);
        let in_env = env
            .iter()
            .any(|(_, value)| secrets.iter().any(|secret| value.contains(secret.as_str())));
        let in_file = env
            .iter()
            .find(|(key, _)| key == ENV_EDGE_TOKENS)
            .and_then(|(_, path)| crate::proxy::read_edge_tokens(path))
            .is_some_and(|(r, w, s)| [r, w, s] == secrets);
        let published = if in_env { 2 } else { i64::from(in_file) };

        if published > 0 {
            let init = model.init_state();
            let mut provisioned = init.clone();
            provisioned.insert("published", published);
            let (ok, why) = validate_transition_tiered(
                &model,
                &[],
                &init,
                &provisioned,
                Some("Provision"),
                label,
            );
            assert!(
                ok,
                "{what}: the secrets were published on channel {published} (2 = the \
                 inheritable env), which the model does not admit — {why}"
            );
            let (admitted, _) = validate_transition_tiered(
                &buggy,
                &[],
                &init,
                &provisioned,
                Some("Provision"),
                "CapabilitySecrecy(Buggy=1)",
            );
            assert!(
                !admitted,
                "{what}: the env-channel mutant admitted the real step"
            );
        }
        assert_eq!(
            published,
            i64::from(sock_dir.is_some()),
            "{what}: the file channel iff there is a private dir to hold the file"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
