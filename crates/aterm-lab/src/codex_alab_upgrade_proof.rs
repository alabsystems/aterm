// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A narrow live handoff between two real packages, preserving one paced act.

use super::*;

const HANG: Duration = Duration::from_secs(60);

fn package_version(package: &Path) -> Result<String, Fail> {
    let path = package.join("codex-package.json");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let json = J::parse(&text)?;
    json.get("version")
        .and_then(J::as_str)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .ok_or_else(|| Fail::die(format!("{}: no version", path.display())))
}

pub(super) fn describe_package(package: &Path) -> Result<(), Fail> {
    let path = package.join("alab-build.json");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let json = J::parse(&text)?;
    println!(
        "upgrade target codex-alab {} (source {}) at {}",
        package_version(package)?,
        json.get("source").and_then(J::as_str).unwrap_or("?"),
        package.display(),
    );
    Ok(())
}

/// These fields belong to the conversation and process, rather than the new image.
fn same_custody(before: &J, after: &J) -> bool {
    [
        "pid",
        "identity",
        "sessionId",
        "launchArgv0",
        "resumeArgv",
        "carriedOn",
        "atermSession",
    ]
    .iter()
    .all(|key| {
        before
            .get(key)
            .is_some_and(|v| v != &J::Null && after.get(key) == Some(v))
    })
}

/// Epoch conversion may round a deadline up, but must never shorten it.
fn same_owed(before: &J, after: &J) -> bool {
    let (J::Obj(before_fields), J::Obj(after_fields)) = (before, after) else {
        return false;
    };
    before_fields.len() == after_fields.len()
        && before_fields
            .iter()
            .filter(|(key, _)| key != "notBefore")
            .all(|(key, value)| after.get(key) == Some(value))
        && before
            .get("notBefore")
            .and_then(J::as_i64)
            .is_some_and(|before| {
                before > 0
                    && after
                        .get("notBefore")
                        .and_then(J::as_i64)
                        .is_some_and(|after| after >= before)
            })
}

fn one_carried_request(rows: &[J], owed: &J, not_before: i64) -> bool {
    rows.len() == 1
        && rows[0]
            .get("t")
            .and_then(J::as_i64)
            .is_some_and(|at| at >= not_before)
        && owed.get("text").and_then(J::as_str).is_some_and(|text| {
            rows[0]
                .get("u")
                .and_then(J::as_str)
                .is_some_and(|user| user.contains(text))
        })
}

/// The first new writer snapshot can still contain the predecessor's owed
/// state while this image has not claimed yet. Read its restored deadline only
/// after its own record reports the expected active image and native claimant.
fn active_image_record(record: &J, version: &str, pid: i32) -> bool {
    record.get("version").and_then(J::as_str) == Some(version)
        && record.get("pid").and_then(J::as_i64) == Some(i64::from(pid))
        && record.get("autopilot").and_then(J::as_str) == Some("active")
        && record.get("claim").and_then(J::as_str) == Some(format!("codex-alab@{pid}").as_str())
}

pub(super) fn legs(proof: &mut Proof, old: &Path, new: &Path) -> Result<(), Fail> {
    proof.link(old).map_err(Fail::die)?;
    proof.inst.await_socket(instance::BOOT)?;
    let _ = proof.inst.ctl(&["ready"]);
    println!("upgrade-only (one real TUI, saved paced continuation, private package link)");
    let leg = proof.spawn_leg(
        "continue",
        "Write a plan, then stop.",
        "[tui]\nscreen_reader_detection_done = true\n",
        Folder::Scratch,
        Some("cruise"),
    )?;
    let Some(pid) = proof.claim_with_limit(&leg, HANG)? else {
        return Ok(());
    };
    let mut before = None;
    let paced = Proof::wait_for(HANG, Duration::from_millis(250), || {
        before = Proof::record(&leg, pid);
        before.as_ref().is_some_and(|record| {
            record.get("status").and_then(J::as_str) == Some("idle")
                && record.get("owed").is_some_and(|owed| {
                    owed.get("rule").and_then(J::as_str) == Some("continue@v1")
                        && owed
                            .get("notBefore")
                            .and_then(J::as_i64)
                            .is_some_and(|at| at > crate::util::now_ms())
                })
        })
    })?;
    proof.check(
        "upgrade-only: the old image owes a paced continuation while idle",
        paced,
    );
    let Some(before) = before.filter(|_| paced) else {
        proof.diagnose(&[&leg]);
        return Ok(());
    };
    let old_version = package_version(old)?;
    let new_version = package_version(new)?;
    proof.check(
        &format!("upgrade-only: the running old image reports {old_version}"),
        before.get("version").and_then(J::as_str) == Some(old_version.as_str()),
    );
    let owed = before.get("owed").expect("observed owed act");
    let original_not_before = owed
        .get("notBefore")
        .and_then(J::as_i64)
        .expect("observed deadline");
    println!("  saved record: {}", before.py());
    proof.upgrade_leg_with_limits(&leg, new, HANG, HANG)?;
    let mut after = None;
    let active = Proof::wait_for(HANG, Duration::from_millis(250), || {
        after = Proof::record(&leg, pid);
        after
            .as_ref()
            .is_some_and(|record| active_image_record(record, &new_version, pid))
    })?;
    proof.check(
        "upgrade-only: the new image's own record reports its active native claim before its restored deadline is read",
        active,
    );
    proof.check(
        &format!("upgrade-only: the new image reports {new_version} with the same conversation and process custody"),
        active && after.as_ref().is_some_and(|after| {
            same_custody(&before, after)
                && after.get("version").and_then(J::as_str) == Some(new_version.as_str())
        }),
    );
    if let Some(after) = &after {
        println!("  resumed record: {}", after.py());
    }
    proof.check(
        "upgrade-only: the new image retains the exact owed act without shortening its deadline",
        active
            && crate::util::now_ms() < original_not_before
            && after
                .as_ref()
                .and_then(|after| after.get("owed"))
                .is_some_and(|after| same_owed(owed, after))
            && proof
                .tags(leg.name)
                .iter()
                .all(|tag| tag != "continue:continue"),
    );
    let mut not_before = after
        .as_ref()
        .and_then(|after| after.get("owed"))
        .and_then(|owed| owed.get("notBefore"))
        .and_then(J::as_i64)
        .map_or(original_not_before, |restored| {
            restored.max(original_not_before)
        });
    // The known paced wait is part of the contract. Only the minute after its
    // original deadline is a hang detector. A broken restore that postpones its
    // deadline by hours must fail this run rather than extend the detector;
    // the actual request still must respect both the original and restored time.
    let wait = Duration::from_millis(
        original_not_before
            .saturating_sub(crate::util::now_ms())
            .max(0)
            .unsigned_abs(),
    ) + HANG;
    // Claim publication and resume/timer publication are independent. Keep the
    // maximum observed lower bound for this exact owed act through the wait;
    // never mistake the next act's new deadline for this continuation's.
    let mut deadline_record = after.clone();
    let acted = Proof::wait_for(wait, Duration::from_millis(250), || {
        if let Some(record) = Proof::record(&leg, pid)
            && active_image_record(&record, &new_version, pid)
            && let Some(restored) = record
                .get("owed")
                .filter(|restored| same_owed(owed, restored))
            && let Some(at) = restored.get("notBefore").and_then(J::as_i64)
            && at > not_before
        {
            not_before = at;
            deadline_record = Some(record);
        }
        proof
            .tags(leg.name)
            .iter()
            .any(|tag| tag == "continue:continue")
    })?;
    if let Some(record) = &deadline_record {
        let path = proof.s.join("upgrade-deadline-record.json");
        std::fs::write(&path, record.py())
            .map_err(|error| format!("{}: {error}", path.display()))?;
        println!(
            "  maximum witnessed deadline {not_before}: {}",
            path.display()
        );
    }
    let completed = Proof::wait_for(HANG, Duration::from_millis(250), || {
        Proof::record(&leg, pid).is_some_and(|record| {
            record.get("status").and_then(J::as_str) == Some("idle")
                && record
                    .get("owed")
                    .and_then(|owed| owed.get("rule"))
                    .and_then(J::as_str)
                    == Some(DEFEND)
        })
    })?;
    let rows: Vec<J> = proof
        .requests(leg.name)
        .into_iter()
        .filter(|row| row.get("tag").and_then(J::as_str) == Some("continue:continue"))
        .collect();
    proof.check(
        &format!("upgrade-only: the carried continuation ran exactly once at or after {not_before}, and its reply completed"),
        acted && completed && one_carried_request(&rows, owed, not_before)
            && proof.proven_pid(&leg.sid) == Some(pid)
            && Proof::record(&leg, pid).as_ref().is_some_and(|after| same_custody(&before, after)),
    );
    if !proof.fails.is_empty() {
        proof.diagnose(&[&leg]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handoff_witness_rejects_new_conversation_process_or_act_and_shortened_deadline() {
        let record = J::obj([
            ("pid", J::Int(42)),
            ("identity", J::s("kernel identity")),
            ("sessionId", J::s("conversation")),
            ("launchArgv0", J::s("/private/launch")),
            (
                "resumeArgv",
                J::Arr(vec![J::s("resume"), J::s("conversation")]),
            ),
            ("carriedOn", J::Int(0)),
            ("atermSession", J::s("s-private")),
        ]);
        assert!(same_custody(&record, &record));
        let J::Obj(fields) = &record else {
            unreachable!()
        };
        for (key, _) in fields {
            let mut changed = fields.clone();
            changed.iter_mut().find(|(k, _)| k == key).expect("field").1 = J::s("different");
            assert!(!same_custody(&record, &J::Obj(changed)), "{key}");
            let missing = J::Obj(fields.iter().filter(|(k, _)| k != key).cloned().collect());
            assert!(!same_custody(&record, &missing), "missing {key}");
        }
        let owed = |text: &str, at| {
            J::obj([
                ("thread", J::s("conversation")),
                ("rule", J::s("continue@v1")),
                ("text", J::s(text)),
                ("notBefore", J::Int(at)),
            ])
        };
        assert!(same_owed(&owed("continue", 100), &owed("continue", 101)));
        let J::Obj(mut reordered) = owed("continue", 101) else {
            unreachable!()
        };
        reordered.reverse();
        assert!(same_owed(&owed("continue", 100), &J::Obj(reordered)));
        assert!(!same_owed(&owed("continue", 100), &owed("continue", 99)));
        assert!(!same_owed(
            &owed("continue", 100),
            &owed("retry refused action", 101)
        ));
        assert!(!same_owed(&J::Null, &J::Null));
        let request = |text: &str, at| J::obj([("u", J::s(text)), ("t", J::Int(at))]);
        let original = owed("continue", 100);
        let at_deadline = request("continue", 200);
        assert!(one_carried_request(
            std::slice::from_ref(&at_deadline),
            &original,
            200
        ));
        assert!(!one_carried_request(
            &[request("continue", 150)],
            &original,
            200
        ));
        assert!(!one_carried_request(
            &[request("different action", 200)],
            &original,
            200
        ));
        assert!(!one_carried_request(
            &[at_deadline.clone(), at_deadline],
            &original,
            200
        ));
        assert!(!one_carried_request(&[], &original, 200));
    }

    #[test]
    fn restored_deadline_is_not_read_from_the_initial_unclaimed_writer_snapshot() {
        let record = |version, pid, autopilot, claim| {
            J::obj([
                ("version", J::s(version)),
                ("pid", J::Int(pid)),
                ("autopilot", J::s(autopilot)),
                ("claim", claim),
            ])
        };
        assert!(active_image_record(
            &record("new", 42, "active", J::s("codex-alab@42")),
            "new",
            42
        ));
        assert!(!active_image_record(
            &record("new", 42, "off", J::Null),
            "new",
            42
        ));
        assert!(!active_image_record(
            &record("old", 42, "active", J::s("codex-alab@42")),
            "new",
            42
        ));
        assert!(!active_image_record(
            &record("new", 42, "active", J::s("codex-alab@43")),
            "new",
            42
        ));
        assert!(!active_image_record(
            &record("new", 43, "active", J::s("codex-alab@42")),
            "new",
            42
        ));
    }
}
