// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn completed_cold_intent_survives_with_or_without_a_future_observation() {
    let process = |pid| Process {
        pid,
        birth: u64::from(pid) * 10,
        parent: 22,
        group: pid,
        session: 22,
        image: Image {
            path: b"/private/vendor".to_vec(),
            device: 7,
            inode: 8,
        },
    };
    let saved = super::super::native_observe::SavedObservation {
        registration: super::super::native_observe::Registration {
            id: "1".repeat(32),
            host_epoch: "2".repeat(32),
            vendor: process(33),
            shell: process(22),
        },
        binding: super::super::native_observe::ObservationBinding {
            id: "1".repeat(32),
            host_epoch: "2".repeat(32),
            capability: "3".repeat(64),
        },
    };
    for intent in [
        super::super::accounts_pool::ExecutionIntent::external(),
        super::super::accounts_pool::ExecutionIntent::Stored {
            namespace: "4".repeat(64),
            login: Some("/private/login".into()),
            owner: Some("work".into()),
            generation: "5".repeat(64),
        },
    ] {
        let text = aterm_json::to_string(&intent.to_value()).unwrap();
        let account = aterm_codec::hex::encode_lower(text.as_bytes());
        let observation = aterm_codec::hex::encode_lower(saved.encode().unwrap().as_bytes());
        let plain = format!(
            "OK native-act state=committed claim=builtin revision=- account-intent={account}"
        );
        for with_watch in [false, true] {
            let wire = if with_watch {
                format!("{plain} observation={observation}")
            } else {
                plain.clone()
            };
            assert_eq!(
                Reply::parse(&wire),
                Some(Reply::ColdRestartCommitted {
                    claim: ClaimOutcome::Builtin,
                    observation: with_watch.then(|| Box::new(saved.clone())),
                    intent: Box::new(intent.clone())
                })
            );
            for tail in [
                " extra=1",
                &format!(" account-intent={account}"),
                &format!(" observation={observation} observation={observation}"),
            ] {
                assert!(Reply::parse(&(wire.clone() + tail)).is_none());
            }
        }
        let old = format!(
            "OK native-act state=committed claim=builtin revision=- observation={observation}"
        );
        assert_eq!(
            Reply::parse(&old),
            Some(Reply::RestartCommitted(Box::new(saved.clone())))
        );
        assert!(Reply::parse(&(old + &format!(" account-intent={account}"))).is_none());
        assert!(Reply::parse(&plain.replace("claim=builtin", "claim=lost")).is_none());
        let noncanonical = aterm_codec::hex::encode_lower(format!(" {text}").as_bytes());
        assert!(Reply::parse(&plain.replace(&account, &noncanonical)).is_none());
        assert!(Reply::parse(&plain.replace(&account, "7b7d")).is_none());
    }
}
