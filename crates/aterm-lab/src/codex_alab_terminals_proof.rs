// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `codex-alab-proof --terminals-only` (gap 28): a real codex-alab TUI's survey `bg=` is its own
//! session record's count of the terminals its footer counts. Two watchers outlive their call's
//! yield and end only when the proof creates their stop files, then a command a running call
//! still waits on counts too. Every wait is a hang detector; nothing races a timer. This proves
//! the count end to end, not the settle read's timing (`codex_rollout_tap`'s and
//! `session_status`'s tests are its control).

use super::*;
use crate::codex_alab_model::terminals::{
    self, LEG, QUICK_CHECK, STOP_A, STOP_B, STOP_C, TERMINALS_RUN,
};

const HANG: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(250);

/// One look at the leg's survey: its record's `bg` (`-` when it has none) and its text line.
struct Look {
    bg: String,
    line: String,
}

impl Look {
    /// Whether the record says `bg` and the text line names `bg=<bg>` (none for `0`, as it prints
    /// nothing for none running).
    fn says(&self, bg: &str) -> bool {
        let named = self
            .line
            .split_whitespace()
            .find_map(|w| w.strip_prefix("bg="));
        self.bg == bg && named == (bg != "0").then_some(bg)
    }

    fn shown(&self) -> String {
        format!("bg={}; line {:?}", self.bg, self.line)
    }
}

fn look(proof: &Proof, leg: &Leg) -> Look {
    let bg = proof
        .survey_of(&leg.sid)
        .and_then(|s| {
            s.get("bg").and_then(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .or_else(|| v.as_i64().map(|n| n.to_string()))
            })
        })
        .unwrap_or_else(|| "-".to_string());
    let line = proof
        .inst
        .ctl(&["survey", "--sid", &leg.sid])
        .stdout
        .lines()
        .find(|l| l.split_whitespace().next() == Some(leg.sid.as_str()))
        .unwrap_or("")
        .to_string();
    Look { bg, line }
}

/// Waits up to [`HANG`] for the leg's survey to say `bg`: the last look either way.
fn await_bg(proof: &Proof, leg: &Leg, bg: &str) -> Result<(bool, Look), Stopped> {
    let mut last = Look {
        bg: "-".to_string(),
        line: String::new(),
    };
    let held = Proof::wait_for(HANG, POLL, || {
        last = look(proof, leg);
        last.says(bg)
    })?;
    Ok((held, last))
}

fn stop_file(leg: &Leg, file: &str) -> bool {
    crate::util::write_fresh(&leg.proj.join(file), "").is_ok()
}

pub(super) fn legs(proof: &mut Proof, package: &Path) -> Result<(), Fail> {
    proof.link(package).map_err(Fail::die)?;
    proof.inst.await_socket(instance::BOOT)?;
    let _ = proof.inst.ctl(&["ready"]);
    println!("terminals-only (real Codex TUI, two watchers and a foreground command, one tab)");

    // The watchers and the foreground command must really run, and codex's own sandbox cannot
    // apply inside the instance's jail.
    proof.sandbox = CodexSandbox::Jail;
    let leg = proof.spawn_leg(LEG, "Start the two watchers.", "", Folder::Scratch, None)?;
    let pid = proof.claim_with_limit(&leg, HANG)?;
    let on = proof.inst.ctl(&["hold", &leg.sid, "on", "reason=lab"]);
    proof.check(
        &format!("terminals: held before its first answer ({})", on.first()),
        on.first().contains("hold=1"),
    );
    let asked = proof.await_tag(&leg, "first-terminal", HANG)?;
    terminals::release_first();
    proof.check(
        "terminals: the model was asked for the first watcher, held until the hold stood",
        asked,
    );
    let ended = proof.await_tag(&leg, "after-terminals", HANG)?;
    proof.check(
        &format!("terminals: the turn ended with both watchers running ({TERMINALS_RUN:?})"),
        ended,
    );

    let (two, seen) = await_bg(proof, &leg, "2")?;
    proof.check(
        &format!(
            "terminals: two background terminals run: survey bg=2 is codex-alab's own record's count ({})",
            seen.shown()
        ),
        ended && two,
    );
    let (one, seen) = if stop_file(&leg, STOP_A) {
        await_bg(proof, &leg, "1")?
    } else {
        (false, look(proof, &leg))
    };
    proof.check(
        &format!("terminals: one ends: bg=1 ({})", seen.shown()),
        two && one,
    );
    let (none, seen) = if stop_file(&leg, STOP_B) {
        await_bg(proof, &leg, "0")?
    } else {
        (false, look(proof, &leg))
    };
    proof.check(
        &format!(
            "terminals: both end: bg=0, and the text line names none ({})",
            seen.shown()
        ),
        one && none,
    );

    // A hold refuses every PTY-reaching verb, so the typed turn runs under Autopilot Off instead:
    // its claim given back while its process runs on in front, the tap reads on read only. Off is
    // set while the hold still stands, so no continuation can come between them; codex-alab keeps
    // its claim through a hold (a transient off, its CONTRACT §2) and gives it back once the hold
    // is lifted and Off alone stands.
    let set = proof.on(&leg.sid, &["meta", "set", "autopilot", "off"]);
    let off = proof.inst.ctl(&["hold", &leg.sid, "off"]);
    let lifted = set.code == 0 && set.first().starts_with("OK") && off.first().contains("hold=0");
    proof.check(
        &format!(
            "terminals: Autopilot Off is set under the hold, then the hold is lifted to type ({}; {})",
            set.first(),
            off.first()
        ),
        lifted,
    );
    let released = lifted && claim_given_back(proof, &leg)?;
    let typed = released && type_quick_check(proof, &leg)?;
    let called = typed && proof.await_tag(&leg, "foreground", HANG)?;
    let mut answered_early = false;
    let mut last = look(proof, &leg);
    let foreground = called
        && Proof::wait_for(HANG, POLL, || {
            last = look(proof, &leg);
            let tags = proof.tags(leg.name);
            answered_early = tags.iter().any(|t| t == "terminals:after-foreground");
            last.says("1") && !answered_early
        })?;
    proof.check(
        &format!(
            "terminals: a command a running tool call still waits on counts: bg=1 before codex has its output ({}{})",
            last.shown(),
            if answered_early {
                "; codex had its output first"
            } else {
                ""
            }
        ),
        none && foreground,
    );
    let (done, seen) =
        if stop_file(&leg, STOP_C) && proof.await_tag(&leg, "after-foreground", HANG)? {
            await_bg(proof, &leg, "0")?
        } else {
            (false, look(proof, &leg))
        };
    proof.check(
        &format!(
            "terminals: the foreground command ends: bg=0 ({})",
            seen.shown()
        ),
        foreground && done,
    );

    let tags = proof.tags(leg.name);
    let acts = proof.acts(&leg.sid);
    proof.check(
        &format!(
            "terminals: nothing typed a continuation, under the hold or under Autopilot Off (model {tags:?}; acts {acts:?})"
        ),
        !tags.iter().any(|t| t == "terminals:unexpected")
            && !acts.iter().any(|a| a.starts_with("continue")),
    );
    for file in [STOP_A, STOP_B, STOP_C] {
        let _ = stop_file(&leg, file);
    }
    println!(
        "  terminals: codex-alab@{}",
        pid.map_or_else(|| "-".to_string(), |p| p.to_string())
    );
    println!("  acts {} {}", leg.name, acts.join(" "));
    println!("  model {} {}", leg.name, tags.join(" "));
    Ok(())
}

/// Waits for codex-alab to give its claim back under the leg's Autopilot Off: whether it did.
fn claim_given_back(proof: &mut Proof, leg: &Leg) -> Result<bool, Stopped> {
    let mut status = String::new();
    let released = Proof::wait_for(HANG, Duration::from_millis(500), || {
        let Ok(st) = proof.read_line(&leg.sid, &["status"]) else {
            return false;
        };
        status = st;
        field(&status, "supervisor_proven").as_deref() != Some("1")
            && !field(&status, "supervisor").is_some_and(|s| s.starts_with("codex-alab@"))
    })?;
    proof.check(
        &format!(
            "terminals: under Autopilot Off codex-alab gives its claim back (supervisor={} supervisor_proven={})",
            field(&status, "supervisor").unwrap_or_default(),
            field(&status, "supervisor_proven").unwrap_or_default()
        ),
        released,
    );
    Ok(released)
}

/// Types [`QUICK_CHECK`] into codex's composer as a control write (`send`, then `key enter`, as
/// the exchange leg's control path does): whether it left the composer at its Enter.
fn type_quick_check(proof: &mut Proof, leg: &Leg) -> Result<bool, Stopped> {
    let draft = |p: &Proof| composer_row("codex", &p.on(&leg.sid, &["text"]).stdout);
    let sent = proof.on(&leg.sid, &["send", QUICK_CHECK]);
    let stood = sent.code == 0
        && Proof::wait_for(HANG, POLL, || draft(proof).as_deref() == Some(QUICK_CHECK))?;
    let mut entered = false;
    if stood {
        for _ in 0..2 {
            if proof.on(&leg.sid, &["key", "enter"]).code != 0 {
                break;
            }
            entered = Proof::wait_for(Duration::from_secs(5), POLL, || {
                draft(proof).as_deref() != Some(QUICK_CHECK)
            })?;
            if entered {
                break;
            }
        }
    }
    proof.check(
        &format!(
            "terminals: {QUICK_CHECK:?} typed by `send`, `key enter` left codex's composer ({})",
            sent.first()
        ),
        entered,
    );
    Ok(entered)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A look says a count only where the record and the text line agree, the line naming none
    /// for `0`.
    #[test]
    fn a_look_says_a_count_only_where_the_record_and_the_line_agree() {
        let at = |bg: &str, line: &str| Look {
            bg: bg.to_string(),
            line: line.to_string(),
        };
        let sid = "s-0123456789abcdef0123";
        assert!(at("2", &format!("{sid} codex unknown 3s task=- bg=2")).says("2"));
        assert!(!at("2", &format!("{sid} codex unknown 3s")).says("2"));
        assert!(!at("-", &format!("{sid} codex unknown 3s bg=2")).says("2"));
        assert!(!at("2", &format!("{sid} codex unknown 3s bg=20")).says("2"));
        assert!(at("0", &format!("{sid} codex unknown 3s")).says("0"));
        assert!(!at("0", &format!("{sid} codex unknown 3s bg=0")).says("0"));
        assert!(!at("-", &format!("{sid} codex unknown 3s")).says("0"));
    }
}
