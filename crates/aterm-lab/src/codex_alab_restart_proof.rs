// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An explicit restart owes a resume even when the person's last keys are recent.

use super::*;

/// An eventual event's hang detector, not a latency budget.
const HANG: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(100);

pub(super) fn legs(proof: &mut Proof, pkg: &Path) -> Result<(), Fail> {
    match proof.ready_rig(pkg) {
        Ok((true, _)) => {}
        Ok((false, _)) => {
            proof.check("restart: the private instance's bus connected", false);
            return Ok(());
        }
        Err(Unready::Unlinked(why)) => {
            proof.check(&format!("restart: the package is linked ({why})"), false);
            return Ok(());
        }
        Err(Unready::NoSocket(fail)) => return Err(fail),
        Err(Unready::Stopped(stopped)) => return Err(stopped.into()),
    }
    let target = proof.spawn_leg(
        "restart",
        "Say hello and finish.",
        "",
        Folder::Scratch,
        None,
    )?;
    let next_package = proof.s.join("pkg-b");
    leg(proof, &target, pkg, &next_package)?;
    if !proof.fails.is_empty() {
        proof.diagnose(&[&target]);
    }
    Ok(())
}

/// The full proof and `--restart-only` use the same real control operation.
pub(super) fn leg(
    proof: &mut Proof,
    target: &Leg,
    before_package: &Path,
    after_package: &Path,
) -> Result<(), Stopped> {
    let mut before = None;
    Proof::wait_for(HANG, POLL, || {
        before = proof.proven_pid(&target.sid).and_then(|pid| {
            let record = Proof::record(target, pid)?;
            (record.get("status").and_then(J::as_str) == Some("idle")
                && record.get("sessionId").and_then(J::as_str).is_some()
                && record
                    .get("resumeArgv")
                    .and_then(J::as_arr)
                    .is_some_and(|argv| !argv.is_empty())
                && record.get("cwd").and_then(J::as_str).is_some())
            .then_some((pid, record))
        });
        before.is_some()
    })?;
    let Some((pid, before)) = before else {
        proof.check(
            "restart: an idle, proven codex-alab has a resume record",
            false,
        );
        return Ok(());
    };
    let expected_before = package_image(before_package);
    let expected_after = package_image(after_package);
    let before_image = aterm_uds::process::read_process_path(pid)
        .and_then(|path| std::fs::canonicalize(path).ok());
    if !proof.check(
        "restart: the live process maps the selected private package's executable",
        expected_before.is_some() && before_image == expected_before && expected_after.is_some(),
    ) {
        return Ok(());
    }
    if before_package != after_package {
        let linked = proof.link(after_package);
        if !proof.check(
            &format!(
                "restart: the private managed selection moved to {} ({})",
                after_package.display(),
                linked
                    .as_ref()
                    .map_or_else(|why| why.as_str(), |()| "linked")
            ),
            linked.is_ok() && expected_before != expected_after,
        ) {
            return Ok(());
        }
    }

    // A cursor key leaves the composer empty and stamps the same human counter as a key
    // from the window. The restart-only rig keeps this key recent for 120 s, beyond HANG.
    let (_, prior_seq) = person_hand(&proof.status(&target.sid));
    let keyed = proof.on(&target.sid, &["key", "as=person", "left"]);
    let (human_ms, seq) = person_hand(&proof.status(&target.sid));
    if !proof.check(
        &format!(
            "restart: a person's cursor key reached the idle TUI ({}; human_seq {prior_seq}->{seq})",
            keyed.first()
        ),
        keyed.code == 0 && seq > prior_seq && human_ms.is_some_and(|ms| ms < 5_000),
    ) {
        return Ok(());
    }
    let lines_before = proof.host_lines(&target.sid).len();
    let restarted = proof.on(&target.sid, &["restart", &format!("pid={pid}")]);
    if !proof.check(
        &format!(
            "restart: accepted for the proven PID ({})",
            restarted.first()
        ),
        restarted.code == 0
            && restarted
                .first()
                .starts_with(&format!("OK restarting pid {pid} ")),
    ) {
        return Ok(());
    }

    let mut after = None;
    Proof::wait_for(HANG, POLL, || {
        after = proof
            .proven_pid(&target.sid)
            .filter(|next| *next != pid)
            .and_then(|next| {
                let record = Proof::record(target, next)?;
                (record.get("status").and_then(J::as_str) == Some("idle")).then_some((next, record))
            });
        after.is_some()
    })?;
    let Some((next, after)) = after else {
        proof.check(
            "restart: a new proven PID resumed and reached idle without manual input",
            false,
        );
        return Ok(());
    };
    proof.check(
        &format!(
            "restart: PID {pid} became {next} and resumed the same conversation, cwd and flags"
        ),
        ["sessionId", "atermSession", "resumeArgv", "cwd"]
            .iter()
            .all(|key| {
                before
                    .get(key)
                    .is_some_and(|value| value != &J::Null && after.get(key) == Some(value))
            })
            && after.get("cwd").and_then(J::as_str) == target.proj.to_str(),
    );
    let after_image = aterm_uds::process::read_process_path(next)
        .and_then(|path| std::fs::canonicalize(path).ok());
    proof.check(
        "restart: the resumed live process maps the selected executable",
        after_image == expected_after,
    );

    let finished = Proof::wait_for(HANG, POLL, || {
        proof
            .host_lines(&target.sid)
            .iter()
            .skip(lines_before)
            .any(|line| {
                line.contains("self-claimed-relaunch ") && line.contains("step=done:resumed")
            })
    })?;
    let lines: Vec<String> = proof
        .host_lines(&target.sid)
        .into_iter()
        .skip(lines_before)
        .collect();
    let resumes = lines
        .iter()
        .filter(|line| {
            line.contains("self-claimed-relaunch ") && line.contains("step=done:resumed")
        })
        .count();
    let held = lines
        .iter()
        .any(|line| line.contains("self-claimed-relaunch ") && line.contains("step=wait:held"));
    let live = std::fs::read_dir(target.home.join("alab/sessions"))
        .ok()
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| {
                    let pid = entry.path().file_stem()?.to_str()?.parse::<i32>().ok()?;
                    aterm_uds::process::read_process_path(pid).map(|_| pid)
                })
                .collect::<Vec<_>>()
        });
    proof.check(
        &format!("restart: one resume, one live TUI, no stale-key hold or manual recovery (live={live:?})"),
        finished && resumes == 1 && !held && live == Some(vec![next])
            && person_hand(&proof.status(&target.sid)).1 == seq,
    );
    Ok(())
}

fn package_image(package: &Path) -> Option<PathBuf> {
    let manifest = crate::util::read_regular_text(&package.join("codex-package.json")).ok()?;
    let entry = aterm_agent::harness::codex_alab::package_entrypoint(&manifest)?;
    std::fs::canonicalize(package.join(entry)).ok()
}
