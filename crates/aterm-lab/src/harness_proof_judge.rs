// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE JUDGES of `lab harness-proof` ([`crate::harness_proof`]): each leg of
//! `docs/DESIGN-harness-repos-2026-10-24.md` §9 step 2 judged from what aterm showed, as plain
//! data, so a leg that must fail on a broken harness is checked here without one
//! (`harness_proof_judge_tests.rs`).
//!
//! What aterm shows, and these read:
//! - the stand-in vendor's frame on aterm's screen ([`frame_of`]): its footer names the build it
//!   runs, its pid, its session and which start of that session it is;
//! - `status` ([`ClaimSeen`]): the program aterm names the tab's foreground leader by, the claim's
//!   holder and whether aterm proved it;
//! - `who`'s `fgpgid=` ([`fgpgid`]): the tab's foreground group, whose leader is the job the
//!   shell started (the harness, or the vendor in its place after a pass-through);
//! - aterm's log ([`held_lines`]): the line a proven contract 3 claim writes, with its versions and
//!   scope, and the acts of the built-in engine's relaunch and upgrade lanes ([`builtin_acts`]);
//! - `blocks --json` ([`blocks_of`]): each command the tab's shell ran, its exit, and the line
//!   aterm's built-in relaunch lane typed.

use crate::pyjson::J;
use crate::screen::field;

/// One check of a leg: what it says, and whether it held.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Check {
    pub label: String,
    pub ok: bool,
}

fn check(ok: bool, label: impl Into<String>) -> Check {
    Check {
        label: label.into(),
        ok,
    }
}

/// Whether every check of a leg held (a leg with none held nothing).
#[must_use]
pub(crate) fn passed(checks: &[Check]) -> bool {
    !checks.is_empty() && checks.iter().all(|c| c.ok)
}

/// The stand-in's frame, read off aterm's screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Frame {
    /// The vendor build it runs (its store record's version).
    pub version: String,
    pub pid: u32,
    pub session: String,
    /// Which start of the session this process is (1 for a fresh one).
    pub start: u32,
    /// The composer's text (`❯ <text>`), trimmed.
    pub composer: String,
}

/// The stand-in's newest frame in a screen's text: its footer
/// (`standin · <version> · pid <pid> · session <uuid> · start <n>`, the last such row) and the
/// composer row above it.
#[must_use]
pub(crate) fn frame_of(screen: &str) -> Option<Frame> {
    let rows: Vec<&str> = screen.lines().collect();
    let at = rows
        .iter()
        .rposition(|r| r.trim_start().starts_with("standin · "))?;
    let parts: Vec<&str> = rows[at].trim().split(" · ").collect();
    let [_, version, pid, session, start] = parts.as_slice() else {
        return None;
    };
    let composer = rows[..at]
        .iter()
        .rev()
        .find_map(|r| r.strip_prefix('❯'))
        .map_or_else(String::new, |c| c.trim().to_string());
    Some(Frame {
        version: (*version).to_string(),
        pid: pid.strip_prefix("pid ")?.parse().ok()?,
        session: session.strip_prefix("session ")?.to_string(),
        start: start.strip_prefix("start ")?.trim().parse().ok()?,
        composer,
    })
}

/// Whether the screen shows the stand-in saying it got `signal` (`standin: got SIGTERM`).
#[must_use]
pub(crate) fn said_signal(screen: &str, signal: &str) -> bool {
    screen.contains(&format!("standin: got {signal}"))
}

/// What `status` said of the tab's program and claim.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ClaimSeen {
    /// `program=`: what aterm names the foreground leader by (`None` for `-`).
    pub program: Option<String>,
    /// `supervisor=` (`None` for `-`).
    pub holder: Option<String>,
    /// `supervisor_proven=1`.
    pub proven: bool,
}

/// [`ClaimSeen`] from one `status` line.
#[must_use]
pub(crate) fn claim_seen(status: &str) -> ClaimSeen {
    let named = |key: &str| field(status, key).filter(|v| v != "-" && !v.is_empty());
    ClaimSeen {
        program: named("program"),
        holder: named("supervisor"),
        proven: field(status, "supervisor_proven").as_deref() == Some("1"),
    }
}

/// `sid`'s foreground group in a `who` reply (`fgpgid=`).
#[must_use]
pub(crate) fn fgpgid(who: &str, sid: &str) -> Option<u32> {
    who.lines()
        .find(|l| l.split_whitespace().nth(1) == Some(sid))
        .and_then(|l| field(l, "fgpgid"))
        .and_then(|v| v.parse().ok())
}

/// One proven contract 3 claim as aterm logged it for a session: `<versions> holds the session
/// under contract 3, scope=<words>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Held {
    /// `<id> <version> (<vendor> <vendor version>)`.
    pub versions: String,
    pub scope: String,
}

/// The [`Held`] lines aterm's log wrote for `sid`, in order.
#[must_use]
pub(crate) fn held_lines(log: &str, sid: &str) -> Vec<Held> {
    let at = format!("harness @{sid}: ");
    log.lines()
        .filter_map(|l| {
            let rest = &l[l.find(&at)? + at.len()..];
            let (versions, after) =
                rest.split_once(" holds the session under contract 3, scope=")?;
            let scope = after.split(';').next()?.trim();
            Some(Held {
                versions: versions.to_string(),
                scope: scope.to_string(),
            })
        })
        .collect()
}

/// The acts aterm's built-in `kind` lane (`relaunch` or `upgrade`) logged for `sid`: its
/// `<kind> pid=… tab=… session=… from=… to=… step=<step>` lines (`upgrade_drive.rs`
/// `Report::line_as`) whose step is something it did, never a wait, a skip, a busy look, a
/// `would-` or `current` (`Report::is_act`).
#[must_use]
pub(crate) fn builtin_acts(log: &str, sid: &str, kind: &str) -> Vec<String> {
    let at = format!("harness @{sid}: {kind} pid=");
    log.lines()
        .filter_map(|l| {
            let rest = &l[l.find(&at)? + at.len() - "pid=".len()..];
            let step = field(rest, "step")?;
            let looked = step == "current"
                || ["wait:", "skip:", "busy:", "would-"]
                    .iter()
                    .any(|p| step.starts_with(p));
            (!looked).then(|| format!("{kind} {rest}"))
        })
        .collect()
}

/// One command block of a tab, as `blocks --json` lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Block {
    pub cmdline: String,
    /// `complete`, `executing`, …
    pub state: String,
    pub exit: Option<i64>,
}

/// The blocks of a `blocks --json` reply (an `OK ` head allowed), oldest first.
#[must_use]
pub(crate) fn blocks_of(reply: &str) -> Vec<Block> {
    let json = reply.trim();
    let json = json.strip_prefix("OK ").unwrap_or(json);
    let Ok(j) = J::parse(json) else {
        return Vec::new();
    };
    j.get("blocks")
        .and_then(J::as_arr)
        .map(|blocks| {
            blocks
                .iter()
                .map(|b| Block {
                    cmdline: b
                        .get("cmdline")
                        .and_then(J::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    state: b
                        .get("state")
                        .and_then(J::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    exit: b.get("exit").and_then(J::as_i64),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The exit of the newest block whose command line holds `cmd`, once that block is complete: an
/// older run's exit never answers for one still running.
#[must_use]
pub(crate) fn exit_of(blocks: &[Block], cmd: &str) -> Option<i64> {
    blocks
        .iter()
        .rev()
        .find(|b| b.cmdline.contains(cmd))
        .filter(|b| b.state == "complete")
        .and_then(|b| b.exit)
}

/// Whether a block that aterm's built-in lane typed resumes `session` (its command line names
/// `resume` and the session, as `( cd -- '<dir>' && exec '<prefix>/agents/muse' 'resume' '<id>' )`).
#[must_use]
pub(crate) fn resume_typed(blocks: &[Block], session: &str) -> bool {
    blocks
        .iter()
        .any(|b| b.cmdline.contains("resume") && b.cmdline.contains(session))
}

/// THE CLAIM: aterm's roster names the harness (`rostered`), `status` names the claim
/// `<id>@<leader>` (the tab's foreground leader) and proven, and aterm logged it under contract 3
/// with exactly the scope the harness should take (`scope`).
#[must_use]
pub(crate) fn claim(
    id: &str,
    scope: &str,
    rostered: bool,
    seen: &ClaimSeen,
    leader: Option<u32>,
    held: &[Held],
) -> Vec<Check> {
    let holder = leader.map(|pid| format!("{id}@{pid}"));
    vec![
        check(
            rostered,
            format!("aterm's contract 3 roster names {id}, so its claim can be proven"),
        ),
        check(
            holder.is_some() && seen.holder == holder,
            format!(
                "status supervisor={} (the tab's foreground leader's claim; aterm saw {})",
                holder.as_deref().unwrap_or("<id>@<leader>"),
                seen.holder.as_deref().unwrap_or("-")
            ),
        ),
        check(seen.proven, "status supervisor_proven=1"),
        check(
            held.iter()
                .any(|h| h.scope == scope && h.versions.starts_with(&format!("{id} "))),
            format!(
                "aterm logged the claim under contract 3 with scope={scope} (it logged {})",
                held_said(held)
            ),
        ),
    ]
}

fn held_said(held: &[Held]) -> String {
    if held.is_empty() {
        return "none".to_string();
    }
    held.iter()
        .map(|h| format!("`{}` scope={}", h.versions, h.scope))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether the claim still stands as `<id>@<leader>`, proven.
fn still_claimed(id: &str, leader: Option<u32>, seen: &ClaimSeen) -> Check {
    let holder = leader.map(|pid| format!("{id}@{pid}"));
    check(
        holder.is_some() && seen.holder == holder && seen.proven,
        format!(
            "the claim still stands, proven, by the same holder (status supervisor={} proven={})",
            seen.holder.as_deref().unwrap_or("-"),
            u8::from(seen.proven)
        ),
    )
}

fn kept_pid(before: Option<u32>, after: Option<u32>) -> Check {
    check(
        before.is_some() && before == after,
        format!(
            "the harness kept its pid (fgpgid {} then {})",
            shown(before),
            shown(after)
        ),
    )
}

fn shown(pid: Option<u32>) -> String {
    pid.map_or_else(|| "-".to_string(), |p| p.to_string())
}

/// What a leg read of the tab after an act.
#[derive(Clone, Debug, Default)]
pub(crate) struct After<'a> {
    /// The stand-in's frame once it came back (`None`: it never did).
    pub frame: Option<&'a Frame>,
    /// The tab's foreground leader before and after.
    pub leader: (Option<u32>, Option<u32>),
    /// `status` after.
    pub seen: ClaimSeen,
}

/// THE CRASH RELAUNCH: after the child crashed, a new child came up on the same session as its next
/// start, the harness kept its pid and its claim, and aterm's built-in relaunch lane stood down
/// (`builtin`: it acted).
#[must_use]
pub(crate) fn relaunch(id: &str, before: &Frame, after: &After<'_>, builtin: bool) -> Vec<Check> {
    let f = after.frame;
    vec![
        check(
            f.is_some_and(|f| f.pid != before.pid),
            format!(
                "a new child came up after the crash (pid {} then {})",
                before.pid,
                f.map_or_else(|| "none".to_string(), |f| f.pid.to_string())
            ),
        ),
        check(
            f.is_some_and(|f| f.session == before.session),
            format!("on the same session {}", before.session),
        ),
        check(
            f.is_some_and(|f| f.start == before.start + 1),
            format!(
                "as that session's next start ({} then {})",
                before.start,
                f.map_or_else(|| "-".to_string(), |f| f.start.to_string())
            ),
        ),
        kept_pid(after.leader.0, after.leader.1),
        still_claimed(id, after.leader.0, &after.seen),
        check(!builtin, "aterm's built-in relaunch lane stood down"),
    ]
}

/// NO MOVE while `what` (a draft, a person typing) is present: every read of the window shows the
/// child `before` still running (its pid, build and session), and, for a draft, `composer` still
/// in the composer.
#[must_use]
pub(crate) fn held_still(
    what: &str,
    before: &Frame,
    reads: &[Option<Frame>],
    composer: Option<&str>,
) -> Vec<Check> {
    let unmoved = reads.iter().all(|r| {
        r.as_ref().is_some_and(|f| {
            f.pid == before.pid && f.version == before.version && f.session == before.session
        })
    });
    let mut checks = vec![check(
        !reads.is_empty() && unmoved,
        format!(
            "no move while {what}: {} reads, each the child pid {} on {}",
            reads.len(),
            before.pid,
            before.version
        ),
    )];
    if let Some(draft) = composer {
        checks.push(check(
            !reads.is_empty()
                && reads
                    .iter()
                    .all(|r| r.as_ref().is_some_and(|f| f.composer == draft)),
            format!("the draft `{draft}` stood in the composer throughout"),
        ));
    }
    checks
}

/// THE MOVE at a safe point: the child came back on build `to` as a new process on the same
/// session, the harness kept its pid and its claim, and aterm's built-in upgrade lane stood down
/// (`builtin`: it acted).
#[must_use]
pub(crate) fn moved(
    id: &str,
    before: &Frame,
    to: &str,
    after: &After<'_>,
    builtin: bool,
) -> Vec<Check> {
    let f = after.frame;
    vec![
        check(
            f.is_some_and(|f| f.version == to),
            format!(
                "the child moved onto the new build {to} (it runs {})",
                f.map_or("nothing", |f| f.version.as_str())
            ),
        ),
        check(
            f.is_some_and(|f| f.pid != before.pid),
            "as a new process".to_string(),
        ),
        check(
            f.is_some_and(|f| f.session == before.session),
            format!("on the same session {}", before.session),
        ),
        kept_pid(after.leader.0, after.leader.1),
        still_claimed(id, after.leader.0, &after.seen),
        check(!builtin, "aterm's built-in upgrade lane stood down"),
    ]
}

/// SIGNAL PROPAGATION, the group's `SIGINT`: the child got it and kept running, and so did the
/// harness.
#[must_use]
pub(crate) fn interrupted(before: &Frame, after: &After<'_>, said: bool) -> Vec<Check> {
    vec![
        check(said, "the child got SIGINT (it said so on the screen)"),
        check(
            after.frame.is_some_and(|f| f.pid == before.pid),
            "the child kept running".to_string(),
        ),
        kept_pid(after.leader.0, after.leader.1),
    ]
}

/// EXIT PROPAGATION: the shell saw `want` for the harness's command (`exit`), the child said it got
/// `signal` when one was sent, and nothing came back after (`relaunched`).
#[must_use]
pub(crate) fn ended(
    what: &str,
    exit: Option<i64>,
    want: i64,
    signal: Option<(&str, bool)>,
    relaunched: bool,
) -> Vec<Check> {
    let mut checks = Vec::new();
    if let Some((name, said)) = signal {
        checks.push(check(said, format!("the child got {name}")));
    }
    checks.push(check(
        exit == Some(want),
        format!(
            "the shell saw exit {want} ({what}; it saw {})",
            exit.map_or_else(|| "none".to_string(), |e| e.to_string())
        ),
    ));
    checks.push(check(!relaunched, "nothing was relaunched after it"));
    checks
}

/// THE HARNESS'S OWN RE-EXEC onto its new build: the same pid now runs the new build's image
/// (`image_new`), it claimed again and aterm proved it, aterm logged the new claim with the new
/// version (`version`), and the child ran on through it.
#[must_use]
pub(crate) fn reexec(
    id: &str,
    version: &str,
    image_new: bool,
    held: &[Held],
    child: (&Frame, Option<&Frame>),
    after: &After<'_>,
) -> Vec<Check> {
    vec![
        kept_pid(after.leader.0, after.leader.1),
        check(
            image_new,
            "that pid now runs the new build's image (the store's new build directory)",
        ),
        still_claimed(id, after.leader.0, &after.seen),
        check(
            held.iter()
                .any(|h| h.versions.starts_with(&format!("{id} {version} "))),
            format!(
                "aterm logged the claim made again with the new version {version} (it logged {})",
                held_said(held)
            ),
        ),
        check(
            child.1.is_some_and(|f| f.pid == child.0.pid),
            format!("the child ran on through it (pid {})", child.0.pid),
        ),
    ]
}

/// THE NEGATIVE CONTROL'S PASS-THROUGH: with no `contract=3` served, the vendor took the harness's
/// place (aterm names the tab's program by the vendor's command, `vendor`, and the job's leader is
/// the stand-in's own pid, `frame`), and nothing claimed the session.
#[must_use]
pub(crate) fn passed_through(
    vendor: &str,
    seen: &ClaimSeen,
    leader: Option<u32>,
    frame: Option<&Frame>,
) -> Vec<Check> {
    vec![
        check(
            seen.program.as_deref() == Some(vendor),
            format!(
                "aterm names the tab's program {vendor}, not the harness (status program={})",
                seen.program.as_deref().unwrap_or("-")
            ),
        ),
        check(
            frame.is_some_and(|f| Some(f.pid) == leader),
            format!(
                "the vendor took the harness's place: the job's leader is the stand-in (fgpgid {}, stand-in pid {})",
                shown(leader),
                frame.map_or_else(|| "-".to_string(), |f| f.pid.to_string())
            ),
        ),
        check(
            seen.holder.is_none(),
            format!(
                "nothing claimed the session (status supervisor={})",
                seen.holder.as_deref().unwrap_or("-")
            ),
        ),
    ]
}

/// THE BUILT-IN LANES TOOK THE SESSION: after the vendor crashed, aterm's built-in relaunch lane
/// typed its resume line (`typed`) and the vendor came back on the same session as a new process.
#[must_use]
pub(crate) fn builtin_took(before: &Frame, typed: bool, after: Option<&Frame>) -> Vec<Check> {
    vec![
        check(
            typed,
            format!(
                "aterm's built-in relaunch lane typed the resume line for session {}",
                before.session
            ),
        ),
        check(
            after.is_some_and(|f| f.session == before.session && f.pid != before.pid),
            "the vendor came back on the same session as a new process".to_string(),
        ),
    ]
}

#[cfg(test)]
#[path = "harness_proof_judge_tests.rs"]
mod tests;
