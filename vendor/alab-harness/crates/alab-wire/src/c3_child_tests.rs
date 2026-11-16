// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::claim::ChildProcess;

const HOLDER: &str = "example-alab@4242";
const SID: &str = "t0000000001";
const PENDING: &str = "OK schema=1 supervisor=example-alab@4242 supervisor_proven=1 supervisor_scope=upgrade supervisor_revision=7 supervisor_child_start=17 supervisor_child=-";

fn generation(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).unwrap()
}

#[test]
fn absent_and_unique_capabilities_select_only_the_negotiated_protocol() {
    assert_eq!(Mode::parse("OK"), Ok(Mode::Absent));
    assert_eq!(
        Mode::parse("OK native-act own-image=example:upgrade"),
        Ok(Mode::Absent)
    );
    assert_eq!(Mode::parse("OK child-proof=1"), Ok(Mode::Proof));
    assert_eq!(
        Mode::parse("OK child-start=1 child-proof=1 extension=a extension=b"),
        Ok(Mode::Start)
    );
}

#[test]
fn present_invalid_or_duplicate_capabilities_fail_closed() {
    for info in [
        "",
        "ERR child-proof=1",
        "OKAY child-proof=1",
        "OK child-proof=0",
        "OK child-proof=01",
        "OK child-proof=2",
        "OK child-proof=",
        "OK child-start=1",
        "OK child-proof=1 child-proof=1",
        "OK child-proof=1 child-proof=0",
        "OK child-proof=1 child-start=1 child-start=1",
        "OK child-proof=1 child-start=0",
        "OK child-proof=1 child-start=01",
        "OK child-proof=1 child-start=",
    ] {
        assert!(Mode::parse(info).is_err(), "{info}");
    }
}

#[test]
fn claims_keep_c4_timestamps_separate_and_absent_capability_bytes_unchanged() {
    let config = Config::new("example-alab", "example", &[Scope::Upgrade], "0.2.0").unwrap();
    let metadata = Child {
        vendor: Some("2.1.325"),
        login: Some("example-login"),
        process: Some(ChildProcess {
            pid: 9999,
            start: 9_000_000,
        }),
    };
    let native = NativeChild::from_native(4242, 12_345).unwrap();
    let base = claim::claim_verb(&config, Contract::Three, SID, 5151, metadata).unwrap();
    assert_eq!(
        claim(
            &config,
            SID,
            5151,
            metadata,
            Some(native),
            Mode::Absent,
            None
        ),
        Some(base.clone())
    );
    assert_eq!(
        claim(
            &config,
            SID,
            5151,
            metadata,
            Some(native),
            Mode::Proof,
            None
        ),
        Some(format!("{base} child=4242:12345"))
    );
    assert_eq!(
        claim(&config, SID, 5151, metadata, None, Mode::Proof, None),
        Some(base.clone())
    );
    assert_eq!(
        claim(
            &config,
            SID,
            5151,
            metadata,
            Some(native),
            Mode::Start,
            Some(generation(17))
        ),
        Some(format!("{base} child=4242:12345 child-start=17"))
    );
    assert_eq!(
        claim(
            &config,
            SID,
            5151,
            metadata,
            None,
            Mode::Start,
            Some(generation(17))
        ),
        Some(format!("{base} child-start=17"))
    );
    let c4 = claim::claim_verb(&config, Contract::Four, SID, 5151, metadata).unwrap();
    assert!(c4.contains(" child=9999:9000000"));
    assert!(!c4.contains("child-start="));
}

#[test]
fn claim_rejects_unsendable_metadata_or_a_generation_without_start_support() {
    let config = Config::new("example-alab", "example", &[Scope::Upgrade], "0.2.0").unwrap();
    for mode in [Mode::Absent, Mode::Proof] {
        assert!(
            claim(
                &config,
                SID,
                5151,
                Child::default(),
                None,
                mode,
                Some(generation(17))
            )
            .is_none()
        );
    }
    assert!(
        claim(
            &config,
            "../bad",
            5151,
            Child::default(),
            None,
            Mode::Absent,
            None
        )
        .is_none()
    );
    assert!(claim(&config, SID, 1, Child::default(), None, Mode::Absent, None).is_none());
    let c4_only = config.with_contracts(&[Contract::Four]).unwrap();
    assert!(
        claim(
            &c4_only,
            SID,
            5151,
            Child::default(),
            None,
            Mode::Absent,
            None
        )
        .is_none()
    );
}

#[test]
fn pending_requires_exact_current_holder_generation_and_scope_set() {
    assert!(pending(PENDING, HOLDER, &[Scope::Upgrade], generation(17)).is_some());
    assert!(
        pending(
            PENDING,
            "example-alab@4243",
            &[Scope::Upgrade],
            generation(17)
        )
        .is_none()
    );
    assert!(pending(PENDING, HOLDER, &[Scope::Relaunch], generation(17)).is_none());
    assert!(pending(PENDING, HOLDER, &[Scope::Upgrade], generation(18)).is_none());
    assert!(
        pending(
            &PENDING.replace(
                "supervisor_scope=upgrade",
                "supervisor_scope=upgrade,relaunch"
            ),
            HOLDER,
            &[Scope::Relaunch, Scope::Upgrade],
            generation(17)
        )
        .is_some()
    );
    assert!(
        pending(
            &PENDING.replace(
                "supervisor=example-alab@4242",
                "supervisor=example-alab%404242"
            ),
            HOLDER,
            &[Scope::Upgrade],
            generation(17)
        )
        .is_some()
    );
    assert!(
        pending(
            &format!("{PENDING} extension=a extension=b"),
            HOLDER,
            &[Scope::Upgrade],
            generation(17)
        )
        .is_some()
    );
}

#[test]
fn pending_refuses_missing_or_duplicate_authority_fields() {
    for field in [
        "schema=1",
        "supervisor=example-alab@4242",
        "supervisor_proven=1",
        "supervisor_scope=upgrade",
        "supervisor_revision=7",
        "supervisor_child_start=17",
        "supervisor_child=-",
    ] {
        assert!(
            pending(
                &format!("{PENDING} {field}"),
                HOLDER,
                &[Scope::Upgrade],
                generation(17)
            )
            .is_none(),
            "duplicate {field}"
        );
        assert!(
            pending(
                &PENDING.replace(&format!(" {field}"), ""),
                HOLDER,
                &[Scope::Upgrade],
                generation(17)
            )
            .is_none(),
            "missing {field}"
        );
    }
}

#[test]
fn pending_refuses_unproven_bound_or_noncanonical_acknowledgements() {
    for (from, to) in [
        ("schema=1", "schema=2"),
        ("supervisor_proven=1", "supervisor_proven=0"),
        ("supervisor_revision=7", "supervisor_revision=0"),
        ("supervisor_revision=7", "supervisor_revision=07"),
        ("supervisor_revision=7", "supervisor_revision=-"),
        ("supervisor_child=-", "supervisor_child=10:20"),
        ("supervisor_child_start=17", "supervisor_child_start=017"),
        ("supervisor_child_start=17", "supervisor_child_start=0"),
    ] {
        assert!(
            pending(
                &PENDING.replace(from, to),
                HOLDER,
                &[Scope::Upgrade],
                generation(17)
            )
            .is_none(),
            "{to}"
        );
    }
}

#[test]
fn pending_refuses_empty_duplicate_unknown_or_unrequested_scopes() {
    for scope in [
        "-",
        "upgrade,",
        ",upgrade",
        "upgrade,,",
        "upgrade,,relaunch",
        "upgrade,upgrade",
        "upgrade,unknown",
        "upgrade,approvals",
        "upgrade,relaunch",
    ] {
        let reply = PENDING.replace(
            "supervisor_scope=upgrade",
            &format!("supervisor_scope={scope}"),
        );
        assert!(
            pending(&reply, HOLDER, &[Scope::Upgrade], generation(17)).is_none(),
            "{scope}"
        );
    }
    assert!(pending(PENDING, HOLDER, &[], generation(17)).is_none());
    assert!(
        pending(
            PENDING,
            HOLDER,
            &[Scope::Upgrade, Scope::State],
            generation(17)
        )
        .is_none()
    );
}

#[test]
fn highest_requires_one_canonical_generation_in_a_schema_status() {
    for value in ["0", "1", "17", "18446744073709551615"] {
        assert_eq!(
            highest(&format!("OK schema=1 supervisor_child_start={value}")),
            Ok(value.parse().unwrap())
        );
    }
    for value in [
        "",
        "-",
        "00",
        "017",
        "+17",
        "-1",
        "x",
        "18446744073709551616",
    ] {
        assert!(
            highest(&format!("OK schema=1 supervisor_child_start={value}")).is_err(),
            "{value}"
        );
    }
    for reply in [
        "ERR schema=1 supervisor_child_start=17",
        "OK supervisor_child_start=17",
        "OK schema=2 supervisor_child_start=17",
        "OK schema=1",
        "OK schema=1 supervisor_child_start=17 supervisor_child_start=17",
    ] {
        assert!(highest(reply).is_err(), "{reply}");
    }
}

#[test]
fn binding_snapshots_keep_pending_generation_and_proof_only_identity() {
    let known = NativeChild::from_native(4242, 12_345).unwrap();
    let other = NativeChild::from_native(4242, 12_346).unwrap();
    let proof_only = Binding {
        mode: Mode::Proof,
        known: Some(known),
        generation: None,
        pending: false,
    };
    let proof_snapshot = proof_only;
    assert_eq!(proof_snapshot.mode, Mode::Proof);
    assert_eq!(proof_snapshot.known, Some(known));
    assert_eq!(proof_snapshot.generation, None);
    assert!(!proof_snapshot.pending);
    let started = Binding {
        mode: Mode::Start,
        generation: Some(generation(17)),
        ..proof_only
    };
    assert_eq!(started.generation_for(Some(known)), Some(generation(17)));
    assert_eq!(started.generation_for(None), None);
    assert_eq!(started.generation_for(Some(other)), None);
    let mut pending = Binding {
        known: None,
        pending: true,
        ..started
    };
    let pending_snapshot = pending;
    pending.known = Some(other);
    pending.pending = false;
    assert_eq!(pending.generation_for(Some(other)), Some(generation(17)));
    assert_eq!(pending_snapshot.mode, Mode::Start);
    assert_eq!(pending_snapshot.known, None);
    assert_eq!(pending_snapshot.generation, Some(generation(17)));
    assert!(pending_snapshot.pending);
    assert_eq!(pending_snapshot.generation_for(None), Some(generation(17)));
}
