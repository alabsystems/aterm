// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Policy-only native TUI lane tests for refusal handling and retry-loop prevention.

use super::*;
use crate::codex_alab_model::policy::{self, ADVANCE, BOUNDARY, DRAFT, RULE};

const HANG: Duration = Duration::from_secs(60);
const QUIET: Duration = Duration::from_secs(130);
const CONFIG_EXTRA: &str = "[tui]\nscreen_reader_detection_done = true\n";

pub(super) fn legs(proof: &mut Proof, package: &Path) -> Result<(), Fail> {
    proof.link(package).map_err(Fail::die)?;
    proof.inst.await_socket(instance::BOOT)?;
    let _ = proof.inst.ctl(&["ready"]);
    println!("policy-only (real Codex TUI, synthetic refusal, four isolated tabs)");

    let draft = proof.spawn_leg(
        "policydraft",
        "Record the permitted local build results.",
        CONFIG_EXTRA,
        Folder::Scratch,
        None,
    )?;
    let draft_pid = proof.claim_with_limit(&draft, HANG)?;
    let requested = proof.await_tag(&draft, "refused", HANG)?;
    proof.check(
        "policy draft: the first request reached the held refusal",
        requested,
    );
    let sent = proof.on(&draft.sid, &["send", DRAFT]);
    let drafted = Proof::wait_for(HANG, Duration::from_millis(250), || {
        composer(proof, &draft).as_deref() == Some(DRAFT)
    })?;
    proof.check(
        "policy draft: the draft stands before the refusal is released",
        sent.code == 0 && drafted,
    );
    policy::release_draft_error();
    let draft_reply = await_refusal(proof, &draft, "refused")?;
    let mut deferred = false;
    let draft_stopped = Proof::wait_for(HANG, Duration::from_millis(250), || {
        let record = draft_pid.and_then(|pid| Proof::record(&draft, pid));
        deferred = record.as_ref().is_some_and(|record| {
            record
                .get("owed")
                .and_then(|owed| owed.get("rule"))
                .and_then(J::as_str)
                == Some(RULE)
                && record.get("draftEmpty") == Some(&J::Bool(false))
        });
        deferred || proof.on(&draft.sid, &["meta"]).first().contains("stopped:")
    })?;
    let allowed = proof.spawn_leg(
        "policy",
        "Record the permitted local build results.",
        CONFIG_EXTRA,
        Folder::Scratch,
        None,
    )?;
    let repeated = proof.spawn_leg(
        "policyagain",
        "Record the permitted local build results.",
        CONFIG_EXTRA,
        Folder::Scratch,
        None,
    )?;
    let _ = proof.claim_with_limit(&allowed, HANG)?;
    let _ = proof.claim_with_limit(&repeated, HANG)?;
    let exhausted = proof.spawn_leg(
        "policyempty",
        "Record the permitted local build results.",
        CONFIG_EXTRA,
        Folder::Scratch,
        None,
    )?;
    let _ = proof.claim_with_limit(&exhausted, HANG)?;

    if deferred && let Some(reply) = draft_reply {
        proof.sleep_past_reply(reply, Duration::from_secs(65))?;
    }
    proof.check(
        "policy draft: the unsent draft is preserved without a request after input grace",
        deferred
            && draft_reply.is_some()
            && draft_stopped
            && composer(proof, &draft).as_deref() == Some(DRAFT)
            && proof.requests(draft.name).len() == 1,
    );
    let clear = proof.on(&draft.sid, &["key", "ctrl+u"]);
    let cleared = Proof::wait_for(HANG, Duration::from_millis(250), || {
        composer(proof, &draft).as_deref() != Some(DRAFT)
    })?;
    proof.check(
        "policy draft: only the fixture's unsent draft is cleared",
        clear.code == 0 && cleared,
    );
    let deferred_observed = Instant::now();

    let all = [&draft, &allowed, &repeated, &exhausted];
    let deadline = if deferred {
        deferred_observed + Duration::from_secs(60) + HANG
    } else {
        Instant::now() + HANG
    };
    let _ = Proof::wait_for(
        deadline.saturating_duration_since(Instant::now()),
        Duration::from_millis(250),
        || {
            all.iter()
                .all(|leg| proof.acts(&leg.sid).iter().any(|act| act == RULE))
        },
    )?;
    let mut advanced = true;
    for leg in all {
        let act = proof.acts(&leg.sid).iter().any(|act| act == RULE);
        let tag = if leg.name == "policyagain" {
            "advance-refused"
        } else if leg.name == "policyempty" {
            "advance-exhausted"
        } else {
            "advance"
        };
        let requested = act && proof.await_tag(leg, tag, HANG)?;
        advanced &= requested;
        let rows = proof.requests(leg.name);
        let first = rows
            .iter()
            .filter(|row| str_of(row, "tag") == format!("{}:refused", leg.name))
            .count();
        let advances = rows
            .iter()
            .filter(|row| str_of(row, "u_full").starts_with(ADVANCE))
            .count();
        proof.check(
            &format!(
                "{}: exactly one distinct policy-aware advance follows one refusal",
                leg.name
            ),
            requested
                && first == 1
                && advances == 1
                && rows
                    .iter()
                    .all(|row| !str_of(row, "u_full").contains(DRAFT))
                && !rows
                    .iter()
                    .any(|row| str_of(row, "tag").ends_with(":unexpected")),
        );
    }

    if advanced {
        let allowed_reply = proof.await_reply(&allowed, "advance", HANG)?;
        if let Some(reply) = allowed_reply {
            proof.sleep_past_reply(reply, Duration::from_secs(120))?;
        }
        let next = allowed_reply.is_some() && proof.await_tag(&allowed, "after-boundary", HANG)?;
        let rows = proof.requests(allowed.name);
        proof.check(
            "policy: later automatic continuation retains the refused-action exclusion",
            next && rows
                .iter()
                .any(|row| str_of(row, "u_full").contains(BOUNDARY)),
        );
        let second_refusal = await_refusal(proof, &repeated, "advance-refused")?;
        if let Some(reply) = second_refusal {
            proof.sleep_past_reply(reply, QUIET)?;
        }
        let rows = proof.requests(repeated.name);
        proof.check(
            "policy repeat: a second refusal stops the advance without a retry loop",
            second_refusal.is_some()
                && rows.len() == 2
                && proof
                    .acts(&repeated.sid)
                    .iter()
                    .filter(|act| act.as_str() == RULE)
                    .count()
                    == 1,
        );
        let exhausted_reply = proof.await_reply(&exhausted, "advance-exhausted", HANG)?;
        if let Some(reply) = exhausted_reply {
            proof.sleep_past_reply(reply, QUIET)?;
        }
        proof.check(
            "policy exhausted: no independent allowed work ends automatic continuation",
            exhausted_reply.is_some()
                && proof.requests(exhausted.name).len() == 2
                && proof
                    .acts(&exhausted.sid)
                    .iter()
                    .filter(|act| act.as_str() == RULE)
                    .count()
                    == 1,
        );
    }
    let account_stable = all.iter().all(|leg| {
        let rows = proof.requests(leg.name);
        !leg.home.join("auth.json").exists()
            && std::fs::read_to_string(leg.home.join("config.toml"))
                .ok()
                .as_deref()
                == Some(codex_config(proof.port, &leg.proj, &leg.home, CONFIG_EXTRA).as_str())
            && rows
                .first()
                .and_then(|row| row.get("model"))
                .filter(|first| first.as_str().is_some_and(|model| !model.is_empty()))
                .is_some_and(|first| rows.iter().all(|row| row.get("model") == Some(first)))
            && rows
                .iter()
                .all(|row| row.get("access_programs") == Some(&J::Null))
    });
    proof.check(
        "policy: account credentials, access program and Daybreak configuration stay unchanged",
        account_stable,
    );
    for leg in all {
        println!("  acts {} {}", leg.name, proof.acts(&leg.sid).join(" "));
        println!("  model {} {}", leg.name, proof.tags(leg.name).join(" "));
    }
    Ok(())
}

fn composer(proof: &Proof, leg: &Leg) -> Option<String> {
    composer_row("codex", &proof.on(&leg.sid, &["text"]).stdout)
}

fn await_refusal(proof: &Proof, leg: &Leg, step: &str) -> Result<Option<ReplyWrite>, Stopped> {
    let head = format!("{}:{step}", leg.name);
    let mut observed = None;
    Proof::wait_for(HANG, Duration::from_millis(250), || {
        observed = refusal_written(
            &proof.requests(leg.name),
            &head,
            codex_rig::monotonic_ms(proof.api_clock),
        );
        observed.is_some()
    })?;
    Ok(observed)
}

fn refusal_written(rows: &[J], head: &str, now: i64) -> Option<ReplyWrite> {
    let row = rows.iter().find(|row| str_of(row, "tag") == head)?;
    let id = row.get("request_id")?.as_i64()?;
    let request = row.get("mono_ms")?.as_i64()?;
    let written = row.get("reply_ms")?.as_i64()?;
    (row.get("status")?.as_i64()? == 400
        && id > 0
        && request >= 0
        && written >= request
        && written <= now)
        .then_some(ReplyWrite {
            request_id: id,
            written_ms: written,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_requires_its_completed_past_server_write() {
        let row = |status, id, request, written| {
            J::obj([
                ("tag", J::s("policy:refused")),
                ("status", J::Int(status)),
                ("request_id", J::Int(id)),
                ("mono_ms", J::Int(request)),
                ("reply_ms", J::Int(written)),
            ])
        };
        assert!(refusal_written(&[row(400, 1, 5, 10)], "policy:refused", 11).is_some());
        for bad in [
            row(200, 1, 5, 10),
            row(400, 0, 5, 10),
            row(400, 1, 10, 5),
            row(400, 1, 5, 12),
        ] {
            assert!(refusal_written(&[bad], "policy:refused", 11).is_none());
        }
        assert!(refusal_written(&[row(400, 1, 5, 10)], "policy:other", 11).is_none());
    }
}
