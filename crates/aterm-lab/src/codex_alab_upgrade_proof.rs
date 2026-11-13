// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A narrow live handoff between two real packages, preserving one paced act.

use super::*;

const HANG: Duration = Duration::from_secs(60);

fn package_version(package: &Path) -> Result<String, Fail> {
    let path = package.join("codex-package.json");
    let text =
        crate::util::read_regular_text(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let json = J::parse(&text)?;
    json.get("version")
        .and_then(J::as_str)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .ok_or_else(|| Fail::die(format!("{}: no version", path.display())))
}

pub(super) fn describe_package(package: &Path) -> Result<(), Fail> {
    let path = package.join("alab-build.json");
    let text =
        crate::util::read_regular_text(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let json = J::parse(&text)?;
    println!(
        "upgrade target codex-alab {} (source {}) at {}",
        package_version(package)?,
        json.get("source").and_then(J::as_str).unwrap_or("?"),
        package.display(),
    );
    Ok(())
}

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

/// The tags the continue leg's stub gives a continuation's requests (`model::answerer`): the
/// first continuation's own request, answered with its one failing command (`continue:fail`),
/// and every other, the request after that command included, answered done
/// (`continue:continue`).
const CONTINUATION_TAGS: [&str; 2] = ["continue:fail", "continue:continue"];

/// Whether `tags` hold a continuation's request ([`CONTINUATION_TAGS`]).
fn continued(tags: &[String]) -> bool {
    tags.iter()
        .any(|tag| CONTINUATION_TAGS.contains(&tag.as_str()))
}

/// The requests of `rows` that deliver a continuation: tagged as one ([`CONTINUATION_TAGS`]) and
/// ending in its user text, with no tool's output. The request after the failing command carries
/// that command's output and delivers nothing, so one row is one delivery of the act.
fn carried_requests(rows: Vec<J>) -> Vec<J> {
    rows.into_iter()
        .filter(|row| {
            row.get("tag")
                .and_then(J::as_str)
                .is_some_and(|tag| CONTINUATION_TAGS.contains(&tag))
                && row.get("tool_output").and_then(J::as_str) == Some("null")
        })
        .collect()
}

fn one_carried_request(rows: &[J], owed: &J, not_before: i64) -> bool {
    rows.len() == 1
        && rows[0]
            .get("t")
            .and_then(J::as_i64)
            .is_some_and(|at| at >= not_before)
        && owed.get("text").and_then(J::as_str).is_some_and(|text| {
            rows[0]
                .get("u_full")
                .or_else(|| rows[0].get("u"))
                .and_then(J::as_str)
                .is_some_and(|user| user.contains(text))
        })
}

fn active_image_record(record: &J, version: &str, pid: i32) -> bool {
    record.get("version").and_then(J::as_str) == Some(version)
        && record.get("pid").and_then(J::as_i64) == Some(i64::from(pid))
        && record.get("autopilot").and_then(J::as_str) == Some("active")
        && record.get("claim").and_then(J::as_str) == Some(format!("codex-alab@{pid}").as_str())
}

const UPGRADE_CONFIG_EXTRA: &str =
    "[tui]\nscreen_reader_detection_done = true\n\n[features]\ngoals = true\n";
const NATIVE_GOAL_OBJECTIVE: &str = "align native codex goal";

fn package_supports_auth_and_goal(package: &Path) -> Result<bool, Fail> {
    let path = package.join("bin/codex");
    let bytes = crate::util::read_program(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(bytes
        .windows(b"CODEX_AUTH_PATH".len())
        .any(|w| w == b"CODEX_AUTH_PATH"))
}

fn record_carries_auth_path(record: &J, auth_path: &Path) -> bool {
    let Some(auth_str) = auth_path.to_str() else {
        return false;
    };
    record
        .get("env")
        .and_then(|env| env.get("CODEX_AUTH_PATH"))
        .and_then(J::as_str)
        == Some(auth_str)
}

pub(super) fn legs(proof: &mut Proof, old: &Path, new: &Path) -> Result<(), Fail> {
    proof.link(old).map_err(Fail::die)?;
    proof.inst.await_socket(instance::BOOT)?;
    let _ = proof.inst.ctl(&["ready"]);
    println!("upgrade-only (one real TUI, saved paced continuation, private package link)");
    let leg = proof.start_leg_using_auth(
        "continue",
        Some("Write a plan, then stop."),
        UPGRADE_CONFIG_EXTRA,
        Folder::Scratch,
        Some("cruise"),
        None,
        true,
    )?;
    let Some(pid) = proof.claim_with_limit(&leg, HANG)? else {
        return Ok(());
    };
    let Some(before) = paced_record(proof, &leg, pid, None)? else {
        return Ok(());
    };
    handoff(proof, &leg, pid, before, old, new)
}

fn paced_record(
    proof: &mut Proof,
    leg: &Leg,
    pid: i32,
    version: Option<&str>,
) -> Result<Option<J>, Stopped> {
    let mut before = None;
    let paced = Proof::wait_for(HANG, Duration::from_millis(250), || {
        before = Proof::record(leg, pid);
        before.as_ref().is_some_and(|record| {
            record.get("status").and_then(J::as_str) == Some("idle")
                && version.is_none_or(|version| active_image_record(record, version, pid))
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
    if !paced {
        proof.diagnose(&[leg]);
    }
    Ok(before.filter(|_| paced))
}

fn handoff(
    proof: &mut Proof,
    leg: &Leg,
    pid: i32,
    before: J,
    old: &Path,
    new: &Path,
) -> Result<(), Fail> {
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
    proof.upgrade_leg_with_limits(leg, new, HANG, HANG)?;
    let mut after = None;
    let active = Proof::wait_for(HANG, Duration::from_millis(250), || {
        after = Proof::record(leg, pid);
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
            && !continued(&proof.tags(leg.name)),
    );
    let mut not_before = after
        .as_ref()
        .and_then(|after| after.get("owed"))
        .and_then(|owed| owed.get("notBefore"))
        .and_then(J::as_i64)
        .map_or(original_not_before, |restored| {
            restored.max(original_not_before)
        });
    let wait = Duration::from_millis(
        original_not_before
            .saturating_sub(crate::util::now_ms())
            .max(0)
            .unsigned_abs(),
    ) + HANG;
    let mut deadline_record = after.clone();
    let acted = Proof::wait_for(wait, Duration::from_millis(250), || {
        if let Some(record) = Proof::record(leg, pid)
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
        !carried_requests(proof.requests(leg.name)).is_empty()
    })?;
    if let Some(record) = &deadline_record {
        let path = proof.s.join("upgrade-deadline-record.json");
        crate::util::write_fresh(&path, record.py())
            .map_err(|error| format!("{}: {error}", path.display()))?;
        println!(
            "  maximum witnessed deadline {not_before}: {}",
            path.display()
        );
    }
    let completed = Proof::wait_for(HANG, Duration::from_millis(250), || {
        Proof::record(leg, pid).is_some_and(|record| {
            record.get("status").and_then(J::as_str) == Some("idle")
                && record
                    .get("owed")
                    .and_then(|owed| owed.get("rule"))
                    .and_then(J::as_str)
                    == Some(DEFEND)
        })
    })?;
    let rows = carried_requests(proof.requests(leg.name));
    proof.check(
        &format!("upgrade-only: the carried continuation ran exactly once at or after {not_before}, and its reply completed"),
        acted && completed && one_carried_request(&rows, owed, not_before)
            && proof.proven_pid(&leg.sid) == Some(pid)
            && Proof::record(leg, pid).as_ref().is_some_and(|after| same_custody(&before, after)),
    );
    if let Some(auth_path) = leg.auth_path.as_deref() {
        if package_supports_auth_and_goal(old)? {
            proof.check(
                "upgrade-only: the old image records isolated CODEX_AUTH_PATH outside CODEX_HOME",
                record_carries_auth_path(&before, auth_path)
                    && !leg.home.join("auth.json").exists(),
            );
        }
        if package_supports_auth_and_goal(new)? {
            proof.check(
                "upgrade-only: the upgraded image preserves isolated CODEX_AUTH_PATH across live execve",
                after
                    .as_ref()
                    .is_some_and(|after| record_carries_auth_path(after, auth_path))
                    && !leg.home.join("auth.json").exists(),
            );
            verify_native_goal_roundtrip(proof, leg, pid)?;
        }
    }
    if !proof.fails.is_empty() {
        proof.diagnose(&[leg]);
    }
    Ok(())
}

fn verify_native_goal_roundtrip(proof: &mut Proof, leg: &Leg, pid: i32) -> Result<(), Stopped> {
    let set_line = format!("/goal {NATIVE_GOAL_OBJECTIVE}\r");
    let sent = proof.inst.ctl_stdin(
        &[&format!("@{}", leg.sid), "send", "--stdin"],
        set_line.as_bytes(),
    );
    let pid_u32 = u32::try_from(pid).unwrap_or(0);
    let mut published = false;
    let observed = Proof::wait_for(HANG, Duration::from_millis(250), || {
        let Some(record) = Proof::record(leg, pid) else {
            return false;
        };
        let Some(thread) = record.get("sessionId").and_then(J::as_str) else {
            return false;
        };
        let record_ok = record.get("goal").and_then(J::as_str) == Some(NATIVE_GOAL_OBJECTIVE);
        let reader_ok =
            aterm_agent::harness::self_claimed::read_record_goal(&leg.home, pid_u32, thread)
                .as_deref()
                == Some(NATIVE_GOAL_OBJECTIVE);
        let survey_ok = proof
            .survey_of(&leg.sid)
            .and_then(|row| row.get("goal").and_then(J::as_str).map(str::to_owned))
            .as_deref()
            == Some(NATIVE_GOAL_OBJECTIVE);
        published = record_ok && reader_ok && survey_ok;
        published
    })?;
    proof.check(
        "upgrade-only: `/goal` publishes Record.goal in sessions/<pid>.json and `aterm ctl survey` reads it natively",
        sent.code == 0 && observed && published,
    );

    let clear_sent = proof.inst.ctl_stdin(
        &[&format!("@{}", leg.sid), "send", "--stdin"],
        b"/goal clear\r",
    );
    let cleared = Proof::wait_for(HANG, Duration::from_millis(250), || {
        let record_cleared = Proof::record(leg, pid)
            .is_some_and(|record| record.get("goal").is_none_or(|g| g == &J::Null));
        let survey_cleared = proof
            .survey_of(&leg.sid)
            .and_then(|row| row.get("goal").and_then(J::as_str).map(str::to_owned))
            .as_deref()
            == Some("-");
        record_cleared && survey_cleared
    })?;
    proof.check(
        "upgrade-only: `/goal clear` clears Record.goal and resets `aterm ctl survey` goal=-",
        clear_sent.code == 0 && cleared,
    );
    Ok(())
}

fn logical_resume(record: &J, entry: &Path) -> bool {
    entry.to_str().is_some_and(|entry| {
        record.get("launchArgv0").and_then(J::as_str) == Some(entry)
            && matches!(record.get("resumeArgv"), Some(J::Arr(argv))
                if argv.first().and_then(J::as_str) == Some(entry))
    })
}

fn same_conversation(before: &J, after: &J) -> bool {
    ["sessionId", "atermSession", "resumeArgv"]
        .iter()
        .all(|key| {
            before
                .get(key)
                .is_some_and(|value| value != &J::Null && after.get(key) == Some(value))
        })
}

fn launch_snapshot(
    proof: &Proof,
    record: &J,
    pid: i32,
    argv0: &Path,
    image: &Path,
    entry: &Path,
    name: &str,
) -> Result<bool, Fail> {
    let kernel_argv = aterm_uds::process::read_process_argv(pid);
    let kernel_argv0 = kernel_argv
        .as_ref()
        .and_then(|argv| argv.first())
        .map(PathBuf::from);
    let kernel_exec_path = u32::try_from(pid)
        .ok()
        .and_then(atpkg::caller_shell::process_args)
        .map(|args| PathBuf::from(args.exec_path));
    let kernel_image = aterm_uds::process::read_process_path(pid);
    let expected_image = std::fs::canonicalize(image).ok();
    let image_matches = expected_image.as_ref().is_some_and(|expected| {
        kernel_image
            .as_ref()
            .and_then(|path| std::fs::canonicalize(path).ok())
            .as_ref()
            == Some(expected)
            && record
                .get("exe")
                .and_then(J::as_str)
                .and_then(|path| std::fs::canonicalize(path).ok())
                .as_ref()
                == Some(expected)
    });
    let shown = |path: Option<&Path>| {
        path.map_or(J::Null, |path| J::s(path.to_string_lossy().into_owned()))
    };
    let snapshot = J::obj([
        ("pid", J::Int(i64::from(pid))),
        ("kernelArgv0", shown(kernel_argv0.as_deref())),
        ("kernelExecPath", shown(kernel_exec_path.as_deref())),
        ("kernelImage", shown(kernel_image.as_deref())),
        ("expectedArgv0", shown(Some(argv0))),
        ("expectedImage", shown(Some(image))),
        ("logicalEntry", shown(Some(entry))),
        ("record", record.clone()),
    ]);
    let path = proof.s.join(name);
    crate::util::write_fresh(&path, snapshot.py())
        .map_err(|error| format!("{}: {error}", path.display()))?;
    println!("  kernel/record witness: {}", path.display());
    Ok(kernel_argv0.as_deref() == Some(argv0)
        && kernel_exec_path.as_deref() == Some(argv0)
        && image_matches
        && logical_resume(record, entry))
}

fn absent(path: &Path) -> bool {
    matches!(std::fs::symlink_metadata(path), Err(error)
        if error.kind() == std::io::ErrorKind::NotFound)
}

/// Exercise the producer's physical fallback, then the native crash repair, before
/// handing the same paced conversation to the ordinary live-upgrade witness.
pub(super) fn raw_legs(proof: &mut Proof, old: &Path, new: &Path) -> Result<(), Fail> {
    let prefix = proof.h.join("Library/Application Support/aterm/pkg");
    let entry = prefix.join("store/codex/exec/codex");
    let primary = prefix.join("bin/codex");
    let physical = old.join("bin/codex");
    let newly_created = absent(&entry);
    proof.link(old).map_err(Fail::die)?;
    proof.inst.await_socket(instance::BOOT)?;
    let _ = proof.inst.ctl(&["ready"]);
    println!(
        "raw-upgrade-only (produced primary, missing entry, automatic crash repair, live handoff)"
    );
    let producer = crate::util::read_regular_text(&primary)
        .map_err(|error| format!("{}: {error}", primary.display()))?;
    let exports = [
        "export ATPKG_MANAGED_LAUNCH_PID=\"$$\"".to_string(),
        format!(
            "export ATPKG_MANAGED_LAUNCH_PATH={}",
            sh_quote(&entry.to_string_lossy())
        ),
        format!(
            "export ATPKG_MANAGED_LAUNCH_TARGET={}",
            sh_quote(&physical.to_string_lossy())
        ),
    ];
    let produced = std::fs::symlink_metadata(&primary)
        .is_ok_and(|metadata| metadata.file_type().is_file())
        && exports
            .iter()
            .all(|line| producer.lines().any(|actual| actual == line));
    proof.check(
        "raw: the supplied native host produced all managed fallback exports",
        produced,
    );
    let removable = newly_created
        && std::fs::symlink_metadata(&entry)
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        && std::fs::canonicalize(&physical)
            .ok()
            .is_some_and(|physical| std::fs::canonicalize(&entry).ok() == Some(physical));
    proof.check(
        "raw: only the newly produced stable entry symlink is removed",
        removable,
    );
    if !produced || !removable {
        return Ok(());
    }
    crate::util::write_fresh(&proof.s.join("raw-produced-primary.txt"), producer)
        .map_err(|error| format!("cannot preserve produced primary: {error}"))?;
    std::fs::remove_file(&entry)
        .map_err(|error| format!("cannot remove the owned entry symlink: {error}"))?;
    let leg = proof.start_leg_using_auth(
        "continue",
        Some("Write a plan, then stop."),
        UPGRADE_CONFIG_EXTRA,
        Folder::Scratch,
        Some("cruise"),
        Some(&primary),
        true,
    )?;
    let Some(pid) = proof.claim_with_limit(&leg, HANG)? else {
        return Ok(());
    };
    let old_version = package_version(old)?;
    let Some(initial) = paced_record(proof, &leg, pid, Some(&old_version))? else {
        return Ok(());
    };
    let initial_launch = launch_snapshot(
        proof,
        &initial,
        pid,
        &physical,
        &physical,
        &entry,
        "raw-initial-kernel-record.json",
    )?;
    proof.check(
        "raw: kernel argv0, exec route and image are physical A while the record resumes the logical entry",
        initial_launch && active_image_record(&initial, &old_version, pid),
    );
    let missing_before_kill = absent(&entry) && proof.proven_pid(&leg.sid) == Some(pid);
    proof.check(
        "raw: the stable entry is still missing immediately before the owned crash",
        missing_before_kill,
    );
    if !initial_launch || !missing_before_kill {
        proof.diagnose(&[&leg]);
        return Ok(());
    }
    let Some(relaunched_pid) =
        proof.crash_once_with_limit(&leg, pid, "raw: crash the proven physical fallback", HANG)?
    else {
        return Ok(());
    };
    let Some(recovered) = paced_record(proof, &leg, relaunched_pid, Some(&old_version))? else {
        return Ok(());
    };
    let repaired = std::fs::symlink_metadata(&entry)
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
        && std::fs::canonicalize(&physical)
            .ok()
            .is_some_and(|physical| std::fs::canonicalize(&entry).ok() == Some(physical));
    let recovered_launch = launch_snapshot(
        proof,
        &recovered,
        relaunched_pid,
        &entry,
        &physical,
        &entry,
        "raw-relaunched-kernel-record.json",
    )?;
    proof.check(
        "raw: automatic native crash relaunch repaired the entry and changed kernel argv0 and exec route from physical A to the logical entry",
        repaired && recovered_launch && active_image_record(&recovered, &old_version, relaunched_pid),
    );
    if let Some(auth_path) = leg.auth_path.as_deref() {
        let relaunched_env_ok = u32::try_from(relaunched_pid)
            .ok()
            .and_then(atpkg::caller_shell::process_args)
            .and_then(|args| {
                args.env
                    .into_iter()
                    .find_map(|entry| entry.strip_prefix("CODEX_AUTH_PATH=").map(PathBuf::from))
            })
            .as_deref()
            == Some(auth_path)
            && !leg.home.join("auth.json").exists();
        proof.check(
            "raw: native crash relaunch preserves isolated CODEX_AUTH_PATH outside CODEX_HOME",
            relaunched_env_ok,
        );
    }
    let carried = same_conversation(&initial, &recovered)
        && initial
            .get("owed")
            .zip(recovered.get("owed"))
            .is_some_and(|(before, after)| same_owed(before, after))
        && initial
            .get("owed")
            .and_then(|owed| owed.get("notBefore"))
            .and_then(J::as_i64)
            .is_some_and(|deadline| crate::util::now_ms() < deadline)
        && !continued(&proof.tags(leg.name));
    proof.check("raw: crash repair resumed the same conversation and owed act without shortening its deadline", carried);
    if !repaired || !recovered_launch || !carried {
        proof.diagnose(&[&leg]);
        return Ok(());
    }
    handoff(proof, &leg, relaunched_pid, recovered, old, new)
}

#[cfg(test)]
#[path = "codex_alab_upgrade_proof_tests.rs"]
mod tests;
