// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lab told-once`: a NEEDS YOU need told once (design §14.18) in a real window.
//!
//! A private windowed instance whose scratch `aterm.toml` turns desktop alerts on. Its first tab,
//! which a second tab puts in the background so no notice is spent on an owner looking at it,
//! raises one NEEDS YOU set the way aterm's own loop does: the set first (`meta set
//! harness_needs`, which the dev seam `ATERM_DEV_LAB_NEEDS` admits with no agent's claim, into the
//! timeline's own set), then its `owner=needs` entry. While it stands the set is only re-ordered,
//! then re-counted (a need gone); then a new need joins it. Each step waits for its decision, then
//! [`STEP`], past the herald's per-session floor and within its burst limit, so every decision is
//! one `Info` line in the instance's own `aterm.log`, the count this lane reads: one `queued` for
//! the set, one `not queued: told once` for each repeat, and one `queued` for the new need. The
//! negative control runs the same steps with told memory off (`ATERM_DEV_HERALD_TOLD_OFF`) and
//! must see every step queued.
//!
//! No banner reaches Notification Center: aterm drops its own notices before delivery
//! (`notify::delivers` in aterm-gui), so `queued` is the herald's decision, counted where it writes
//! it. The instance's aterm must be a debug or dev-seams build: a release image reads neither seam,
//! and the run says the set was refused.

use std::path::Path;
use std::time::Duration;

use aterm_json::{Value, json};

use crate::instance::{Instance, Launch, World, config_with, resolve_aterm, take_flag};
use crate::{Fail, Res};

/// A need each step's set may carry, as a worker words it.
const A: &str = "grant Screen Recording to com.aterm.aterm in System Settings";
const B: &str = "the release credential to sign 0.48.0";
const C: &str = "a second reviewer for the parser's error paths";

/// Each step's set, in the worker's order: told, re-ordered, re-counted (one need gone), and a
/// new need beside a told one.
const STEPS: [&[&str]; 4] = [&[A, B], &[B, A], &[A], &[C, A]];

/// What the herald decides at each step with its told memory on.
const TOLD: [&str; 4] = [QUEUED, TOLD_ONCE, TOLD_ONCE, QUEUED];

/// What it decides with the memory off: every step a new transition, posted.
const UNTOLD: [&str; 4] = [QUEUED; 4];

/// The herald's decision line for a posted notice (`status_item::herald_log_line`).
const QUEUED: &str = "queued";

/// Its decision line for a repeat told once.
const TOLD_ONCE: &str = "not queued: told once";

/// The time from one step's decision to the next step: past the herald's 20 s per-session floor,
/// so each post is due at once and each quiet decision logs at `Info`, and at most three posts in
/// its 60 s burst window.
const STEP: Duration = Duration::from_secs(25);

/// The hang detector of each wait that must end: the socket, a step's decision line.
const HANG: Duration = Duration::from_secs(60);

/// The window's memory reservation: a functional window, since the lane claims no latency.
const WINDOW_MEMORY: u64 = 2 * 1024 * 1024 * 1024;

/// The program a needs entry names (`needs::harness_ask` reads a known program's name).
const PROGRAM: &str = "claude";

/// What one leg saw.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Leg {
    /// The herald's decision for the tab's need, one per `Info` line, in order.
    pub decisions: Vec<String>,
    /// The first ctl answer that was not `OK`, with its request.
    pub refused: Option<String>,
}

/// What the run saw: told memory on, and the negative control with it off.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Evidence {
    pub told: Leg,
    pub control: Leg,
}

fn fail(error: impl std::fmt::Display) -> Fail {
    Fail::die(format!("told-once: {error}"))
}

/// The `owner=needs` entry aterm's own loop writes for `set` (aterm-agent's needs entry): its
/// first need, the count past it, and that the worker carries on.
fn entry(set: &[&str]) -> String {
    let more = match set.len() {
        0 | 1 => String::new(),
        n => format!(" (+{} more)", n - 1),
    };
    format!(
        "{PROGRAM} needs you: {}{more} \u{2014} carrying on",
        set.first().copied().unwrap_or_default()
    )
}

/// The `meta set harness_needs` words that tell `set` beside its entry, each value
/// percent-encoded.
fn needs_words(set: &[&str]) -> Vec<String> {
    let pct = aterm_uds::runs::pct_encode;
    std::iter::once(format!("entry={}", pct(&entry(set))))
        .chain(set.iter().map(|need| format!("need={}", pct(need))))
        .collect()
}

/// The herald's decisions for session `local`'s attention need in `log` (an `aterm.log`), in
/// order: what follows the line's `): `.
fn decisions(log: &str, local: u64) -> Vec<String> {
    let head = format!("escalation notification for session {local} (attention ");
    log.lines()
        .filter_map(|line| {
            let (_, rest) = line.split_once(&head)?;
            let (_, decision) = rest.split_once("): ")?;
            Some(decision.trim_end().to_owned())
        })
        .collect()
}

/// The local id and sid of each terminal `ls` lists (`<pid> <local> <sid> …`), in its order.
fn roster(ls: &str) -> Vec<(u64, String)> {
    ls.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let _pid = words.next()?;
            let local = words.next()?.parse().ok()?;
            let sid = words.next().filter(|sid| sid.starts_with("s-"))?;
            Some((local, sid.to_owned()))
        })
        .collect()
}

/// Run `args`; `Err` names the request and its answer where it is not `OK`.
fn ok(inst: &Instance, args: &[&str]) -> Result<(), String> {
    let reply = inst.ctl(args);
    let first = reply.first();
    if reply.code == 0 && first.starts_with("OK") {
        Ok(())
    } else {
        Err(format!("{}: {first}", args.join(" ")))
    }
}

/// One leg: a fresh world and window, the steps, the herald's lines read back.
fn leg(aterm: &Path, told_off: bool) -> Result<Leg, Fail> {
    let world = World::new(&config_with(&["desktop_alerts = true".to_owned()])).map_err(fail)?;
    let mut env = world.env();
    env.push(("ATERM_DEV_LAB_NEEDS".to_owned(), "1".to_owned()));
    if told_off {
        env.push(("ATERM_DEV_HERALD_TOLD_OFF".to_owned(), "1".to_owned()));
    }
    let mut inst = Instance::start_functional(
        &world,
        Launch {
            aterm: aterm.to_path_buf(),
            headless: false,
            args: Vec::new(),
            env,
            cwd: world.home.clone(),
            sandbox: None,
            reroute: false,
        },
        WINDOW_MEMORY,
    )
    .map_err(fail)?;
    let log = world.state().join("logs").join("aterm.log");
    let ran = steps(&mut inst, &log);
    crate::instance::after_stop(ran, inst.stop())
}

/// The steps on a running instance, `log` its `aterm.log`.
fn steps(inst: &mut Instance, log: &Path) -> Result<Leg, Fail> {
    inst.await_socket(HANG)?;
    let mut first = None;
    let mut answer = String::new();
    let listed = crate::wait_until(HANG, || {
        let ls = inst.ctl(&["ls"]);
        first = roster(&ls.stdout).into_iter().next();
        answer = format!("exit {}: {}{}", ls.code, ls.stdout, ls.stderr);
        first.is_some()
    });
    let (local, sid) = first.filter(|_| listed).ok_or_else(|| {
        fail(format!(
            "no terminal in `ls` within a minute; it last answered {}",
            answer.trim()
        ))
    })?;
    let mut leg = Leg::default();
    if let Err(refused) = ok(inst, &["tab", "new"]) {
        leg.refused = Some(refused);
        return Ok(leg);
    }
    let at = format!("@{sid}");
    let read = || {
        decisions(
            &crate::util::read_regular_text(log).unwrap_or_default(),
            local,
        )
    };
    for set in STEPS {
        let before = read().len();
        let words = needs_words(set);
        let mut tell: Vec<&str> = vec![&at, "meta", "set", "harness_needs"];
        tell.extend(words.iter().map(String::as_str));
        let text = entry(set);
        let raise = [
            &at,
            "meta",
            "set",
            "attention",
            "owner=needs",
            "kind=ask",
            text.as_str(),
        ];
        if let Err(refused) = ok(inst, &tell).and_then(|()| ok(inst, &raise)) {
            leg.refused = Some(refused);
            break;
        }
        // A step the herald never decides shows as a decision missing from the leg.
        let _decided = crate::wait_until(HANG, || read().len() > before);
        crate::stop::sleep(STEP).map_err(|stopped| fail(format!("stopped: {stopped:?}")))?;
    }
    leg.decisions = read();
    Ok(leg)
}

/// Each check of the run and whether it held.
fn judge(ev: &Evidence) -> Vec<(String, bool)> {
    let told = |k: usize| ev.told.decisions.get(k).map(String::as_str) == Some(TOLD[k]);
    vec![
        (
            "every request answered OK".to_owned(),
            ev.told.refused.is_none() && ev.control.refused.is_none(),
        ),
        ("the set is posted".to_owned(), told(0)),
        ("its re-order is told once".to_owned(), told(1)),
        ("its re-count is told once".to_owned(), told(2)),
        ("a new need beside it is posted".to_owned(), told(3)),
        (
            "nothing else is decided for it".to_owned(),
            ev.told.decisions.len() == TOLD.len(),
        ),
        (
            "CONTROL: told memory off posts every step".to_owned(),
            ev.control.decisions == UNTOLD,
        ),
    ]
}

pub fn cmd(args: &[String]) -> Res {
    let mut args = args.to_vec();
    let as_json = take_flag(&mut args, "--json");
    if args.len() > 1 {
        return Err(Fail::usage("told-once takes --json and an aterm at most"));
    }
    let aterm = resolve_aterm(args.pop()).map_err(|e| Fail::exit(3, e))?;
    let ev = Evidence {
        told: leg(&aterm, false)?,
        control: leg(&aterm, true)?,
    };
    let checks = judge(&ev);
    let pass = checks.iter().all(|(_, held)| *held);
    let report: Value = json!({
        "lane": "told-once",
        "pass": pass,
        "checks": checks.iter().map(|(name, held)| json!({"check": name, "pass": held})).collect::<Vec<_>>(),
        "told": ev.told.decisions,
        "control": ev.control.decisions,
        "refused": [ev.told.refused, ev.control.refused],
    });
    let text = if as_json {
        format!("{}\n", aterm_json::to_string(&report).map_err(fail)?)
    } else {
        let mut text =
            String::from("told-once: a private window, desktop_alerts = true, one NEEDS YOU set\n");
        for (name, held) in &checks {
            text.push_str(&format!(
                "  {}  {name}\n",
                if *held { "PASS" } else { "FAIL" }
            ));
        }
        for (name, leg) in [("told", &ev.told), ("control", &ev.control)] {
            text.push_str(&format!("  {name}: {}\n", leg.decisions.join(" | ")));
            if let Some(refused) = &leg.refused {
                text.push_str(&format!("  {name} refused: {refused}\n"));
            }
        }
        text.push_str(if pass { "PASS\n" } else { "FAIL\n" });
        text
    };
    print!("{text}");
    Ok(i32::from(!pass))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passing() -> Evidence {
        Evidence {
            told: Leg {
                decisions: TOLD.iter().map(|d| (*d).to_owned()).collect(),
                refused: None,
            },
            control: Leg {
                decisions: UNTOLD.iter().map(|d| (*d).to_owned()).collect(),
                refused: None,
            },
        }
    }

    /// Every check holds on a passing run, and each fails on its own: a repeat posted, an extra
    /// decision, the control told once, a refused request.
    #[test]
    fn each_check_fails_alone() {
        assert!(judge(&passing()).iter().all(|(_, held)| *held));
        let mut posted = passing();
        posted.told.decisions[1] = QUEUED.to_owned();
        let mut extra = passing();
        extra.told.decisions.push(QUEUED.to_owned());
        let mut quiet = passing();
        quiet.control.decisions[2] = TOLD_ONCE.to_owned();
        let mut refused = passing();
        refused.told.refused = Some("meta set harness_needs: ERR denied".to_owned());
        for ev in [posted, extra, quiet, refused] {
            assert_eq!(
                judge(&ev).iter().filter(|(_, held)| !*held).count(),
                1,
                "{ev:?}"
            );
        }
    }

    /// The herald's lines are read for the tab's own session alone, its decision whole.
    #[test]
    fn the_herald_lines_read_back_per_session() {
        let log = "2026-11-05T10:00:00Z INFO escalation notification for session 0 (attention \
                   text_key=0x00000000000000ab): queued\n\
                   2026-11-05T10:00:25Z INFO escalation notification for session 1 (attention \
                   text_key=0x00000000000000cd): queued\n\
                   2026-11-05T10:00:25Z INFO escalation notification for session 0 (attention \
                   text_key=0x00000000000000ef): not queued: told once\n\
                   2026-11-05T10:00:30Z INFO escalation notification for session 0 (prompt \
                   agent_rev=3): queued\n";
        assert_eq!(decisions(log, 0), [QUEUED, TOLD_ONCE]);
        assert_eq!(decisions(log, 1), [QUEUED]);
    }

    /// The entry names the first need and counts the rest, as aterm's own loop writes it, and
    /// the set's words carry it and each need percent-encoded, one word each.
    #[test]
    fn the_entry_and_the_set_words_are_the_loops() {
        assert_eq!(
            entry(&[B, A]),
            format!("claude needs you: {B} (+1 more) \u{2014} carrying on")
        );
        assert_eq!(
            entry(&[A]),
            format!("claude needs you: {A} \u{2014} carrying on")
        );
        let words = needs_words(&[A, B]);
        assert_eq!(words.len(), 3);
        assert!(
            words[0].starts_with("entry=claude%20needs%20you:"),
            "{words:?}"
        );
        assert!(words.iter().all(|w| !w.contains(' ')), "{words:?}");
        assert_eq!(
            words[2],
            "need=the%20release%20credential%20to%20sign%200.48.0"
        );
    }

    #[test]
    fn the_roster_names_each_terminal() {
        let ls = "0 0 s-fc1f - alive aterm meta=0 window=0 active=0\n\
                  0 1 s-2f00 - alive aterm meta=0 window=0 active=1 *\n";
        assert_eq!(
            roster(ls),
            [(0, "s-fc1f".to_owned()), (1, "s-2f00".to_owned())]
        );
        assert!(
            roster("0 s-fc1f - alive aterm\n").is_empty(),
            "a row without its instance's pid is not `ls`'s"
        );
    }
}
