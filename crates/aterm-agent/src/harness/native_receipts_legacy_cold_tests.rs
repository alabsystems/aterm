// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::accounts::{AuthRoute, Mode};
use crate::harness::native_act::{ColdBilling, ColdIntent};

#[test]
fn a_legacy_cold_noexec_receipt_survives_route_normalization() {
    for stored_login in [false, true] {
        let (_home, paths, mut prepare, binding) = fixture();
        let cold = ColdIntent {
            login: Some("m3.2".into()),
            home: Some("/private/account".into()),
            auth_observed: true,
            route: if stored_login {
                AuthRoute::Stored
            } else {
                AuthRoute::Outside
            },
            conversation: Some("original-conversation".into()),
            profile: Some(crate::harness::profile::Context {
                plain: "/private/account/.claude.json".into(),
                records: "/private/account/.claude".into(),
                selection: crate::harness::profile::Selection::CustomOauth,
                home: Some("/private/account".into()),
                config: None,
                login: Some(String::new()),
            }),
            mode: Mode::Wall,
            billing: ColdBilling::Keep,
        };
        let NativePlan::Restart { cold: slot, .. } = &mut prepare.plan else {
            unreachable!();
        };
        *slot = Some(Box::new(cold));
        // The exact v0.157 ColdIntent wire order, before `route` replaced
        // `stored_login`. It is receipt data, not the current serializer.
        let old_cold = format!(
            "{{\"login\":\"m3.2\",\"home\":\"/private/account\",\"auth_observed\":true,\"stored_login\":{stored_login},\"conversation\":\"original-conversation\",\"profile\":{{\"plain\":\"/private/account/.claude.json\",\"records\":\"/private/account/.claude\",\"selection\":\"CustomOauth\",\"home\":\"/private/account\",\"config\":null,\"login\":\"\"}},\"mode\":\"wall\",\"billing\":\"Keep\"}}"
        );
        let current = Request::prepare(prepare.clone()).encode().unwrap();
        let (prefix, _) = current.rsplit_once(" cold=").unwrap();
        let legacy = format!(
            "{prefix} cold={}",
            aterm_codec::hex::encode_lower(old_cold.as_bytes())
        );
        assert_ne!(legacy, current, "the fixture must cross an encoding change");
        assert_eq!(
            Request::parse(&legacy),
            Some(Request::prepare(prepare.clone()))
        );
        assert_eq!(
            Request::prepare(prepare.clone()).encode_legacy_cold(),
            Some(legacy.clone())
        );

        let key = write_key(&paths).unwrap();
        let receipt = Receipt {
            operation: key.operation(&prepare.id),
            prepare: key.digest(b"prepare\0", legacy.as_bytes()),
            capability: key.capability(&binding),
        };
        assert_ne!(receipt.prepare, key.prepare(&prepare).unwrap());
        let epoch = key.digest(b"epoch\0", prepare.host_epoch.as_bytes());
        let (mut log, view) = open(&paths, &key, LIMITS).unwrap();
        log.append(
            ISSUER_SUBJECT,
            view.encode_event(&key, &receipt, epoch, Event::Prepare, 17),
        )
        .unwrap();
        drop(log);
        let (mut log, view) = open_existing(&paths, &key, LIMITS).unwrap();
        let expected = Observed {
            sequence: view.sequence,
            recorded_ms: 18,
        };
        log.append(
            PHASE_SUBJECT,
            view.encode_phase(&key, &receipt, epoch, ImagePhase::Forward, 18),
        )
        .unwrap();
        drop(log);
        let before = std::fs::read(directory(&paths).join("noexec.astream")).unwrap();
        // The historical record is authenticated and readable even before a
        // new-format request is matched to its old preparation digest.
        assert_eq!(observe_at(&paths, &binding).unwrap(), Some(expected));
        assert_eq!(
            observe_prepare_at(&paths, &prepare).unwrap(),
            Some(expected)
        );
        assert_eq!(
            observe_phase_at(&paths, &prepare, &binding).unwrap(),
            Some(PhaseObserved {
                phase: ImagePhase::Forward,
                sequence: expected.sequence,
                recorded_ms: 18
            })
        );
        let mut changed = prepare.clone();
        changed.deadline_us += 1;
        assert_eq!(observe_prepare_at(&paths, &changed).unwrap(), None);
        assert_eq!(observe_phase_at(&paths, &changed, &binding).unwrap(), None);
        let changes: &[fn(&mut ColdIntent)] = &[
            |cold| cold.login = Some("m3.1".into()),
            |cold| cold.home = None,
            |cold| cold.auth_observed = false,
            |cold| cold.conversation = None,
            |cold| cold.profile.as_mut().unwrap().plain = "/different".into(),
            |cold| cold.profile.as_mut().unwrap().records = "/different".into(),
            |cold| {
                cold.profile.as_mut().unwrap().selection =
                    crate::harness::profile::Selection::Standard
            },
            |cold| cold.profile.as_mut().unwrap().home = None,
            |cold| cold.profile.as_mut().unwrap().config = Some(String::new()),
            |cold| cold.profile.as_mut().unwrap().login = None,
            |cold| cold.mode = Mode::Off,
            |cold| cold.billing = ColdBilling::UsageCredits,
        ];
        for change in changes {
            let mut changed = prepare.clone();
            let NativePlan::Restart {
                cold: Some(cold), ..
            } = &mut changed.plan
            else {
                unreachable!();
            };
            change(cold);
            assert_eq!(observe_prepare_at(&paths, &changed).unwrap(), None);
            assert_eq!(observe_phase_at(&paths, &changed, &binding).unwrap(), None);
        }
        changed = prepare.clone();
        changed.host_epoch = "78".repeat(16);
        assert_eq!(observe_prepare_at(&paths, &changed).unwrap(), None);
        assert_eq!(observe_phase_at(&paths, &changed, &binding).unwrap(), None);
        changed = prepare.clone();
        let NativePlan::Restart {
            cold: Some(cold), ..
        } = &mut changed.plan
        else {
            unreachable!();
        };
        cold.route = AuthRoute::VaultToken {
            entry: crate::harness::accounts::TokenEntry::parse(
                "aterm-claude-token-ac-0123456789abcdef",
            )
            .unwrap(),
        };
        assert!(
            Request::prepare(changed.clone())
                .encode_legacy_cold()
                .is_none()
        );
        assert_eq!(observe_prepare_at(&paths, &changed).unwrap(), None);
        assert_eq!(observe_phase_at(&paths, &changed, &binding).unwrap(), None);
        // Mutation continues to require the original canonical digest; a
        // read-only compatibility match cannot grant or rewrite this record.
        assert!(record_event_at(&paths, &prepare, &binding, Some(Event::MayExecute)).is_err());
        assert!(record_event_at(&paths, &prepare, &binding, Some(Event::Seal)).is_err());
        let mut wrong = binding.clone();
        wrong.capability = "12".repeat(32);
        assert_eq!(observe_phase_at(&paths, &prepare, &wrong).unwrap(), None);
        assert_eq!(
            std::fs::read(directory(&paths).join("noexec.astream")).unwrap(),
            before
        );
    }
}
