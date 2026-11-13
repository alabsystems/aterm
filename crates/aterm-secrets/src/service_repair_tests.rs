// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

struct Fake {
    observed: Observed,
    installs: Vec<String>,
    deny: bool,
    revoke_after_baseline: bool,
    candidate_bad: bool,
    new_failure: Option<&'static str>,
    rollback_bad: bool,
    huge_failure: bool,
}
impl Backend for Fake {
    fn authorize(&mut self, _: &Request) -> Result<()> {
        if self.deny {
            Err(err("grant revoked"))
        } else {
            Ok(())
        }
    }
    fn observe(&mut self, _: Job) -> Result<Observed> {
        Ok(self.observed.clone())
    }
    fn verify_candidate(&mut self, _: &Request) -> Result<()> {
        if self.candidate_bad {
            Err(err("forged/revoked artifact or unsupported protocol"))
        } else {
            Ok(())
        }
    }
    fn install(&mut self, _: Job, u: &Unit) -> Result<()> {
        self.installs.push(u.image.clone());
        if u.image == "new" && self.new_failure == Some("failed start") {
            return Err(err("failed start"));
        }
        self.observed.unit = Some(u.clone());
        Ok(())
    }
    fn prove(&mut self, _: Job, u: &Unit, _: &HealthRequirement) -> Result<()> {
        if self.huge_failure && !self.installs.is_empty() && (u.image == "new" || self.rollback_bad)
        {
            return Err(err("🦀".repeat(MAX_JOURNAL_BYTES)));
        }
        if u.image == "old" && self.installs.is_empty() && self.revoke_after_baseline {
            self.deny = true;
        }
        if u.image == "new"
            && let Some(reason) = self.new_failure
        {
            return Err(err(reason));
        }
        if u.image == "old" && self.rollback_bad && !self.installs.is_empty() {
            return Err(err("old service health failed"));
        }
        Ok(())
    }
}
fn setup() -> (Request, Fake) {
    let old = Unit {
        program: "/store/old/aterm".into(),
        args: vec!["secrets".into(), "serve".into()],
        definition: "exact old unit".into(),
        image: "old".into(),
        executable_sha256: "old-exe".into(),
    };
    let observed = Observed {
        unit: Some(old),
        off: false,
        pin: None,
    };
    let new = Unit {
        program: "/store/new/aterm".into(),
        args: vec!["secrets".into(), "serve".into()],
        definition: "exact new unit".into(),
        image: "new".into(),
        executable_sha256: "new-exe".into(),
    };
    (
        Request {
            node: "receiver".into(),
            requester: "controller".into(),
            nonce: "0123456789abcdef".into(),
            job: Job::Server,
            current_image: "old".into(),
            desired_image: "new".into(),
            precondition: observed.clone(),
            unit: new,
            health: HealthRequirement {
                vault: "signed vault".into(),
                signer: "signing key".into(),
                records: vec!["record".into()],
                frontier: 42,
            },
            required_protocols: vec![1],
        },
        Fake {
            observed,
            installs: vec![],
            deny: false,
            revoke_after_baseline: false,
            candidate_bad: false,
            new_failure: None,
            rollback_bad: false,
            huge_failure: false,
        },
    )
}
#[test]
fn committed_nonce_replays_without_restart_and_rejects_rebinding() {
    let home = aterm_tempfile::tempdir().unwrap();
    let (mut r, mut b) = setup();
    let first = activate(home.path(), &r, &mut b).unwrap();
    assert_eq!(first.verdict, Verdict::Committed);
    assert_eq!(activate(home.path(), &r, &mut b).unwrap(), first);
    assert_eq!(b.installs, ["new"]);
    r.requester = "another controller".into();
    assert!(
        activate(home.path(), &r, &mut b)
            .unwrap_err()
            .to_string()
            .contains("nonce reused")
    );
    b.deny = true;
    assert!(activate(home.path(), &first.request, &mut b).is_err());
    assert_eq!(b.installs, ["new"]);
}
#[test]
fn refusal_controls_never_install() {
    for control in [
        "off",
        "pin",
        "race-off",
        "race-pin",
        "unit",
        "artifact",
        "protocol",
        "identity",
        "absent",
        "authority",
    ] {
        let home = aterm_tempfile::tempdir().unwrap();
        let (mut r, mut b) = setup();
        match control {
            "off" => r.precondition.off = true,
            "pin" => r.precondition.pin = Some("old".into()),
            "race-off" => b.observed.off = true,
            "race-pin" => b.observed.pin = Some("old".into()),
            "unit" => b
                .observed
                .unit
                .as_mut()
                .unwrap()
                .definition
                .push_str(" changed"),
            "artifact" => b.candidate_bad = true,
            "protocol" => r.required_protocols.clear(),
            "identity" => r.current_image = "wrong".into(),
            "absent" => r.precondition.unit = None,
            "authority" => b.deny = true,
            _ => unreachable!(),
        }
        let result = activate(home.path(), &r, &mut b);
        if control == "authority" {
            assert!(result.is_err());
        } else {
            assert_eq!(result.unwrap().verdict, Verdict::Refused, "{control}");
        }
        assert!(b.installs.is_empty(), "{control}");
    }
}
#[test]
fn failed_start_identity_and_authenticated_health_restore_exact_old_unit() {
    for reason in [
        "failed start",
        "wrong executable",
        "wrong vault",
        "stale frontier",
    ] {
        let home = aterm_tempfile::tempdir().unwrap();
        let (r, mut b) = setup();
        b.new_failure = Some(reason);
        let out = activate(home.path(), &r, &mut b).unwrap();
        assert_eq!(out.verdict, Verdict::RolledBack);
        assert_eq!(out.detail, reason);
        assert_eq!(b.installs, ["new", "old"]);
        assert_eq!(b.observed, r.precondition);
        assert_eq!(activate(home.path(), &r, &mut b).unwrap(), out);
        assert_eq!(b.installs.len(), 2);
    }
}
#[test]
fn restart_rolls_back_prepared_journal_before_next_activation() {
    let home = aterm_tempfile::tempdir().unwrap();
    let (r, mut b) = setup();
    write(
        home.path(),
        &active(home.path(), r.job),
        &json!({"schema":1,"phase":"prepared","request":request_value(&r)}),
    )
    .unwrap();
    b.observed.unit = Some(r.unit.clone());
    let out = recover(home.path(), r.job, &mut b).unwrap().unwrap();
    assert_eq!(out.verdict, Verdict::RolledBack);
    assert_eq!(b.installs, ["old"]);
    assert_eq!(b.observed, r.precondition);
    assert_eq!(activate(home.path(), &r, &mut b).unwrap(), out);
    assert_eq!(b.installs, ["old"]);
}
#[test]
fn failed_rollback_is_durable_and_blocks_new_activation() {
    let home = aterm_tempfile::tempdir().unwrap();
    let (mut r, mut b) = setup();
    b.new_failure = Some("wrong vault");
    b.rollback_bad = true;
    assert_eq!(
        activate(home.path(), &r, &mut b).unwrap().verdict,
        Verdict::RollbackFailed
    );
    r.nonce = "differentnonce0123".into();
    assert!(
        activate(home.path(), &r, &mut b)
            .unwrap_err()
            .to_string()
            .contains("failed rollback")
    );
    assert_eq!(b.installs, ["new", "old"]);
    assert_eq!(
        recover(home.path(), r.job, &mut b)
            .unwrap()
            .unwrap()
            .verdict,
        Verdict::RollbackFailed
    );
}
#[test]
fn truncated_journal_and_nonce_paths_fail_closed() {
    let home = aterm_tempfile::tempdir().unwrap();
    let (mut r, mut b) = setup();
    r.nonce = "../../escape012345".into();
    assert!(activate(home.path(), &r, &mut b).is_err());
    crate::fsx::ensure_private_dir(&root(home.path())).unwrap();
    crate::fsx::write_atomic(&active(home.path(), r.job), b"{", 0o600).unwrap();
    assert!(recover(home.path(), r.job, &mut b).is_err());
    assert!(b.installs.is_empty());
}

#[test]
fn interrupted_recovery_preserves_later_off_pin_and_unit_changes() {
    for control in ["off", "pin", "unit", "removed"] {
        let home = aterm_tempfile::tempdir().unwrap();
        let (r, mut b) = setup();
        write(
            home.path(),
            &active(home.path(), r.job),
            &json!({"schema":1,"phase":"prepared","request":request_value(&r)}),
        )
        .unwrap();
        match control {
            "off" => b.observed.off = true,
            "pin" => b.observed.pin = Some("owner pin".into()),
            "unit" => b
                .observed
                .unit
                .as_mut()
                .unwrap()
                .definition
                .push_str(" owner change"),
            "removed" => b.observed.unit = None,
            _ => unreachable!(),
        }
        let before = b.observed.clone();
        assert!(recover(home.path(), r.job, &mut b).is_err(), "{control}");
        assert_eq!(before, b.observed);
        assert!(b.installs.is_empty());
        assert_eq!(
            read(&active(home.path(), r.job)).unwrap().unwrap()["phase"].as_str(),
            Some("prepared")
        );
    }
}

#[test]
fn prepared_directory_flush_failure_never_stops_service() {
    let home = aterm_tempfile::tempdir().unwrap();
    let (r, mut b) = setup();
    crate::fsx::ensure_private_dir(&root(home.path())).unwrap();
    // Four ancestor flushes pass; the atomic journal's directory flush fails
    // after its rename. The prepared file exists, but activation must not start.
    let failure = aterm_durable::seam::FailingDirs::after(4, aterm_durable::seam::EIO);
    assert!(activate(home.path(), &r, &mut b).is_err());
    assert!(b.installs.is_empty());
    assert_eq!(b.observed, r.precondition);
    drop(failure);
    assert_eq!(
        read(&active(home.path(), r.job)).unwrap().unwrap()["phase"].as_str(),
        Some("prepared")
    );
    assert_eq!(
        recover(home.path(), r.job, &mut b)
            .unwrap()
            .unwrap()
            .verdict,
        Verdict::RolledBack
    );
}

#[test]
fn revocation_during_health_proof_refuses_before_journaled_restart() {
    let home = aterm_tempfile::tempdir().unwrap();
    let (r, mut b) = setup();
    b.revoke_after_baseline = true;
    let out = activate(home.path(), &r, &mut b).unwrap();
    assert_eq!(out.verdict, Verdict::Refused);
    assert_eq!(out.detail, "grant revoked");
    assert!(b.installs.is_empty());
}

#[test]
fn revoked_remote_recovery_retains_prepared_journal_without_restart() {
    let home = aterm_tempfile::tempdir().unwrap();
    let (r, mut b) = setup();
    write(
        home.path(),
        &active(home.path(), r.job),
        &json!({"schema":1,"phase":"prepared","request":request_value(&r)}),
    )
    .unwrap();
    let result = recover_checked(home.path(), r.job, &mut b, || {
        Err(err("transport key revoked"))
    });
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("transport key revoked")
    );
    assert!(b.installs.is_empty());
    assert_eq!(
        read(&active(home.path(), r.job)).unwrap().unwrap()["phase"].as_str(),
        Some("prepared")
    );
}

#[test]
fn explicit_recovery_unblocks_new_activation_without_rewriting_original_nonce() {
    let home = aterm_tempfile::tempdir().unwrap();
    let (r, mut b) = setup();
    b.new_failure = Some("failed start");
    b.rollback_bad = true;
    let original = activate(home.path(), &r, &mut b).unwrap();
    assert_eq!(original.verdict, Verdict::RollbackFailed);
    b.rollback_bad = false;
    b.new_failure = None;
    let recovered = recover(home.path(), r.job, &mut b).unwrap().unwrap();
    assert_eq!(recovered.verdict, Verdict::RolledBack);
    assert_eq!(b.observed, r.precondition);
    let installs = b.installs.len();
    assert_eq!(activate(home.path(), &r, &mut b).unwrap(), original);
    assert_eq!(b.installs.len(), installs);
    assert_eq!(
        recover(home.path(), r.job, &mut b).unwrap(),
        Some(recovered)
    );
    assert_eq!(b.installs.len(), installs);
    let mut next = r.clone();
    next.nonce = "next-nonce-123456".into();
    assert_eq!(
        activate(home.path(), &next, &mut b).unwrap().verdict,
        Verdict::Committed
    );
    assert_eq!(
        parse_outcome(&read(&receipt(home.path(), &r)).unwrap().unwrap()).unwrap(),
        original
    );
}

#[test]
fn interrupted_recovery_retains_failure_receipt_and_rechecks_owner_policy() {
    let home = aterm_tempfile::tempdir().unwrap();
    let (r, mut b) = setup();
    b.new_failure = Some("wrong vault");
    b.rollback_bad = true;
    let original = activate(home.path(), &r, &mut b).unwrap();
    write(
        home.path(),
        &active(home.path(), r.job),
        &json!({"schema":1,"phase":"recovering",
        "request":request_value(&r),"original_result":outcome_value(&original)}),
    )
    .unwrap();
    b.rollback_bad = false;
    b.new_failure = None;
    b.observed.off = true;
    let installs = b.installs.len();
    assert!(recover(home.path(), r.job, &mut b).is_err());
    assert_eq!(b.installs.len(), installs);
    b.observed.off = false;
    assert_eq!(
        recover(home.path(), r.job, &mut b)
            .unwrap()
            .unwrap()
            .verdict,
        Verdict::RolledBack
    );
    assert_eq!(activate(home.path(), &r, &mut b).unwrap(), original);
}

#[test]
fn journal_writer_never_replaces_readable_state_with_an_oversized_record() {
    let home = aterm_tempfile::tempdir().unwrap();
    let path = active(home.path(), Job::Server);
    let empty = json!({"padding":""});
    let overhead = aterm_json::to_vec(&empty).unwrap().len();
    let edge = json!({"padding":"x".repeat(MAX_JOURNAL_BYTES-overhead)});
    write(home.path(), &path, &edge).unwrap();
    assert_eq!(read(&path).unwrap(), Some(edge.clone()));
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes.len(), MAX_JOURNAL_BYTES);
    let over = json!({"padding":"x".repeat(MAX_JOURNAL_BYTES-overhead+1)});
    assert!(write(home.path(), &path, &over).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let nested = json!({"schema":1,"phase":"recovering","original_result":edge,"request":over});
    assert!(write(home.path(), &path, &nested).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert!(read(&path).is_ok());
}

#[test]
fn oversized_local_request_cannot_poison_an_existing_transaction() {
    let home = aterm_tempfile::tempdir().unwrap();
    let (mut r, mut b) = setup();
    let previous = activate(home.path(), &r, &mut b).unwrap();
    let path = active(home.path(), r.job);
    let bytes = std::fs::read(&path).unwrap();
    let installs = b.installs.len();
    r.nonce = "oversized-request-123456".into();
    r.unit.definition = "x".repeat(MAX_REQUEST_BYTES);
    assert!(activate(home.path(), &r, &mut b).is_err());
    assert_eq!(b.installs.len(), installs);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(recover(home.path(), r.job, &mut b).unwrap(), Some(previous));
}

#[test]
fn recovery_refuses_readable_records_outside_current_bounds_before_mutation() {
    for kind in [
        "prepared-request",
        "terminal-detail",
        "original-detail",
        "receipt-detail",
    ] {
        let home = aterm_tempfile::tempdir().unwrap();
        let (mut r, mut b) = setup();
        let mut original = Outcome {
            request: r.clone(),
            verdict: Verdict::RollbackFailed,
            detail: "failed".into(),
        };
        let value = match kind {
            "prepared-request" => {
                r.unit.definition = "x".repeat(MAX_REQUEST_BYTES);
                json!({"schema":1,"phase":"prepared","request":request_value(&r)})
            }
            "terminal-detail" => {
                original.detail = "\0".repeat(MAX_DETAIL_BYTES + 1);
                outcome_value(&original)
            }
            "original-detail" => {
                original.detail = "\0".repeat(MAX_DETAIL_BYTES + 1);
                json!({"schema":1,"phase":"recovering","request":request_value(&r),"original_result":outcome_value(&original)})
            }
            "receipt-detail" => {
                let value = outcome_value(&original);
                original.detail = "\0".repeat(MAX_DETAIL_BYTES + 1);
                write(
                    home.path(),
                    &receipt(home.path(), &r),
                    &outcome_value(&original),
                )
                .unwrap();
                value
            }
            _ => unreachable!(),
        };
        let path = active(home.path(), r.job);
        write(home.path(), &path, &value).unwrap();
        let before = std::fs::read(&path).unwrap();
        let failure = recover(home.path(), r.job, &mut b).unwrap_err();
        assert!(
            failure.to_string().contains("current supported"),
            "{kind}: {failure}"
        );
        assert!(b.installs.is_empty(), "{kind}");
        assert_eq!(std::fs::read(&path).unwrap(), before, "{kind}");
    }
}

#[test]
fn huge_failure_after_mutation_keeps_a_durable_bounded_terminal_outcome() {
    for rollback_bad in [false, true] {
        let home = aterm_tempfile::tempdir().unwrap();
        let (r, mut b) = setup();
        b.huge_failure = true;
        b.rollback_bad = rollback_bad;
        let outcome = activate(home.path(), &r, &mut b).unwrap();
        assert_eq!(
            outcome.verdict,
            if rollback_bad {
                Verdict::RollbackFailed
            } else {
                Verdict::RolledBack
            }
        );
        assert_eq!(b.installs, ["new", "old"]);
        assert!(outcome.detail.len() <= MAX_DETAIL_BYTES);
        assert!(outcome.detail.ends_with("[diagnostic truncated]"));
        let durable = parse_outcome(&read(&active(home.path(), r.job)).unwrap().unwrap()).unwrap();
        assert_eq!(durable, outcome);
        assert_eq!(activate(home.path(), &r, &mut b).unwrap(), outcome);
        assert_eq!(b.installs.len(), 2);
        if rollback_bad {
            assert!(outcome.detail.contains("; rollback:"));
            b.huge_failure = false;
            b.rollback_bad = false;
            let recovered = recover(home.path(), r.job, &mut b).unwrap().unwrap();
            assert_eq!(recovered.verdict, Verdict::RolledBack);
            let active = read(&active(home.path(), r.job)).unwrap().unwrap();
            assert_eq!(original_result(&active, &r).unwrap(), Some(outcome.clone()));
            assert!(aterm_json::to_vec(&active).unwrap().len() <= MAX_JOURNAL_BYTES);
            assert_eq!(activate(home.path(), &r, &mut b).unwrap(), outcome);
        }
    }
}
