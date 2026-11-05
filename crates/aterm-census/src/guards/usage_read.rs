// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! `usage-read-on-timer`: the owner's rule that nothing polls a provider. A walk over the shipped
//! call graph, by name, from every timer path must reach no provider usage read and no sign-in
//! except through a door that holds an account in use: the row a launch starts on, a live
//! session's own turn end, wall or footer, a person's command, and for a sign-in a signed-out row
//! a session here runs on or one due login this Mac uses (design 2026-11-05 §4.15). A door is a
//! function that means one of those; the walk does not enter it, and a timer's own body naming one
//! is a finding, since a wait is no use.

use std::collections::{BTreeMap, BTreeSet};

use super::graph::{attribute, reach};
use super::rules::test_path;
use super::{Finding, Rule, SourceFile, Tree, is_word};

const SCOPE: &[&str] = &[
    "crates/aterm-agent/src/",
    "crates/aterm-gui/src/",
    "crates/aterm/src/",
];

/// Token constructors and their evidence doors: no timer may mint provider-read or sign-in
/// authority. The word [`read_bodies`] records for a spawn of `accounts refresh`, which `/usage`
/// follows.
const SPAWNED_READ: &str = "accounts refresh";

const READS: &[&str] = &[
    SPAWNED_READ,
    "launch_read_use",
    "session_read_use",
    "wall_read_use",
    "asked_read_use",
    "launch_use",
    "footer_use",
    "host_use",
    "session_sign_in",
    "renewal_sign_in",
    "asked_sign_in",
    "selected_sign_in",
    "launch_sign_in",
    "granted_sign_in",
];

/// The functions that make a read for an account they hold in use. The walk does not enter them.
const IN_USE: &[(&str, &str)] = &[
    (
        "launch_env_using",
        "one launch and its shared read allowance",
    ),
    ("run_agy", "a person's command for the active agy home"),
    (
        "run_kimi_refresh_with",
        "a person's command for the active Kimi row",
    ),
    (
        "prepare_quota_wall",
        "a running Codex session's weekly wall",
    ),
    ("read_launch_rows", "the row a launch starts on"),
    ("pooled_starter", "the row a worker's launch starts on"),
    ("codex_cold_live", "the row a cold relaunch starts on"),
    (
        "cold_launch_who_capturing",
        "the retained row a cold relaunch starts on",
    ),
    ("rotate_worker", "the agy a launch starts"),
    (
        "rotate_worker_at_wall",
        "a running agy stopped at its own wall",
    ),
    ("usage_probe", "a live Claude Code session's turn end"),
    ("probe_step", "a live Claude Code session's turn end"),
    (
        "agy_subscription_with_store_in_dir",
        "a running agy's first footer",
    ),
    ("muse_subscription_for", "a running Muse's footer"),
    (
        "upkeep_sign_in",
        "a signed-out row a session here runs on, or one due login this Mac uses",
    ),
    (
        "selection_sign_in",
        "a person's saved selection of one account",
    ),
    (
        "approved_sign_in",
        "a root's approval of one account for this Mac's first custody",
    ),
    (
        "custody_sign_in",
        "a root's current decision for one account in this Mac's custody, not yet answered here",
    ),
];

/// A name declared this many times or more says nothing about which one a call reaches.
const AMBIGUOUS_AT: usize = 3;

/// A line that waits a set time in a body is that function being a timer's.
const TIMED_WAITS: &[&str] = &[
    "sleep(",
    "recv_timeout(",
    "wait_timeout(",
    "park_timeout(",
    "recv_deadline(",
];

pub(super) const USAGE_READ_ON_TIMER: Rule = Rule {
    id: "usage-read-on-timer",
    why: "a timer that reads a provider's usage for, or signs in to, an account nobody is using \
          is the central polling the owner ruled out, which can trigger botting protections",
    scope: SCOPE,
    check: usage_read_on_timer,
};

/// Whether `func`'s name says a timer drives it, it is the window's upkeep, or it is the
/// supervisor's wait, whose timed wait sits behind `wait` and `park`.
fn timer_named(func: &str) -> bool {
    func == "upkeep_live"
        || func == "wait_for_next"
        || func == "tick"
        || func.starts_with("tick_")
        || func.ends_with("_tick")
        || func.ends_with("_timer")
        || func.ends_with("_poll")
}

/// Whether a walk starts at `func`. A timer's name is a timer however many share it (every
/// `tick`). A timed wait is only as good as the name it sits under: bodies merge by name, so one
/// `run` of many that sleeps says nothing about the others.
fn is_timer(func: &str, body: &Body<'_>) -> bool {
    timer_named(func) || (body.timed && body.declared < AMBIGUOUS_AT)
}

fn words(code: &str) -> impl Iterator<Item = &str> {
    code.split(|c: char| !(c.is_ascii() && is_word(c as u8)))
        .filter(|word| !word.is_empty())
}

/// What the walk reads of one shipped function: the words of its body, each with its first spot.
#[derive(Default)]
struct Body<'a> {
    words: BTreeMap<&'a str, (&'a str, usize)>,
    declared: usize,
    timed: bool,
}

fn read_bodies<'a>(files: &[&'a SourceFile]) -> BTreeMap<&'a str, Body<'a>> {
    let mut bodies: BTreeMap<&str, Body<'_>> = BTreeMap::new();
    for file in files {
        let shipped: BTreeSet<usize> = file.shipped_lines().map(|line| line.no).collect();
        for line in attribute(file) {
            let Some(func) = line.func.filter(|_| !line.in_test) else {
                continue;
            };
            if !shipped.contains(&line.line.no) {
                continue;
            }
            let body = bodies.entry(func).or_default();
            let code = line.line.code;
            if super::graph::fn_name(code) == Some(func) {
                body.declared += 1;
            }
            body.timed |= TIMED_WAITS.iter().any(|wait| code.contains(wait));
            for word in words(code) {
                body.words
                    .entry(word)
                    .or_insert((file.rel.as_str(), line.line.no));
            }
            // A read another process makes for it: a spawn of the verb that opens a quota tab.
            let text = line.line.text;
            if text.contains("\"accounts\"") && text.contains("\"refresh\"") {
                body.words
                    .entry(SPAWNED_READ)
                    .or_insert((file.rel.as_str(), line.line.no));
            }
        }
    }
    bodies
}

fn usage_read_on_timer(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let files: Vec<&SourceFile> = tree
        .corpus
        .under(SCOPE)
        .filter(|file| !test_path(&file.rel))
        .collect();
    let bodies = read_bodies(&files);
    let doors: BTreeSet<&str> = IN_USE.iter().map(|(door, _)| *door).collect();
    let edges: BTreeMap<&str, Vec<&str>> = bodies
        .iter()
        .filter(|(func, _)| !doors.contains(*func))
        .map(|(func, body)| {
            let next = body
                .words
                .keys()
                .copied()
                .filter(|word| {
                    word != func
                        && bodies
                            .get(word)
                            .is_some_and(|callee| callee.declared < AMBIGUOUS_AT)
                })
                .collect();
            (*func, next)
        })
        .collect();
    let starts = bodies
        .iter()
        .filter(|(func, body)| !doors.contains(*func) && is_timer(func, body))
        .map(|(func, _)| *func);
    for (func, body) in bodies
        .iter()
        .filter(|(func, body)| !doors.contains(*func) && is_timer(func, body))
    {
        for door in &doors {
            let Some(&(path, line)) = body.words.get(door) else {
                continue;
            };
            out.push(Finding {
                rule: USAGE_READ_ON_TIMER.id,
                path: path.to_string(),
                line,
                detail: format!(
                    "`{func}` waits on a timer and calls `{door}`: a wait does not hold an account \
                     in use"
                ),
            });
        }
    }
    let from = reach(starts, &edges);
    for func in from.keys() {
        if doors.contains(func) {
            continue;
        }
        let Some(body) = bodies.get(func) else {
            continue;
        };
        for read in READS {
            let Some(&(path, line)) = body.words.get(read) else {
                continue;
            };
            let mut chain = vec![*func];
            let mut at = *func;
            while let Some(Some(prev)) = from.get(at) {
                chain.push(prev);
                at = prev;
            }
            chain.reverse();
            out.push(Finding {
                rule: USAGE_READ_ON_TIMER.id,
                path: path.to_string(),
                line,
                detail: format!(
                    "`{read}` is reached from a timer by {}, through no door that holds the \
                     account in use",
                    chain.join(" -> ")
                ),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::run;
    use super::*;

    const AT: &str = "crates/aterm-agent/src/harness/accounts_sync.rs";

    fn found(src: &str) -> Vec<Finding> {
        run(&USAGE_READ_ON_TIMER, &[(AT, src)])
    }

    #[test]
    fn every_constructor_is_found_in_the_composing_executable_with_a_door_control() {
        for constructor in [
            "launch_read_use",
            "session_read_use",
            "wall_read_use",
            "asked_read_use",
        ] {
            let path = "crates/aterm/src/metrics.rs";
            let bad = format!(
                "fn tick() {{ helper(); }}\nfn helper() {{ Use::{constructor}(evidence); }}\n"
            );
            let findings = run(&USAGE_READ_ON_TIMER, &[(path, &bad)]);
            assert_eq!(findings.len(), 1, "{constructor}: {findings:?}");
            assert_eq!(findings[0].path, path);
            let allowed = format!(
                "fn tick() {{ step(); }}\nfn step() {{ usage_probe(); }}\nfn usage_probe() {{ Use::{constructor}(evidence); }}\n"
            );
            assert!(
                run(&USAGE_READ_ON_TIMER, &[(path, &allowed)]).is_empty(),
                "CONTROL: the same constructor behind the natural-use door"
            );
        }
    }

    /// Every sign-in token's constructor reached from the upkeep is found, the timer's chain
    /// named. CONTROL: the same constructor behind the upkeep's sign-in door is silent.
    #[test]
    fn every_sign_in_constructor_on_a_timer_is_found_with_a_door_control() {
        for constructor in [
            "session_sign_in",
            "renewal_sign_in",
            "asked_sign_in",
            "selected_sign_in",
            "launch_sign_in",
            "granted_sign_in",
        ] {
            let bad = found(&format!(
                "fn upkeep_live() {{ renew_pool_due(); }}
fn renew_pool_due() {{ SignIn::{constructor}(cli, row); }}
"
            ));
            assert_eq!(bad.len(), 1, "{constructor}: {bad:?}");
            assert!(
                bad[0].detail.contains("upkeep_live -> renew_pool_due"),
                "{constructor}: {bad:?}"
            );
            for door in [
                "upkeep_sign_in",
                "selection_sign_in",
                "approved_sign_in",
                "custody_sign_in",
            ] {
                assert!(
                    found(&format!(
                        "fn upkeep_live() {{ renew_pool_due(); }}
fn renew_pool_due() {{ {door}(cli, row); }}
fn {door}() {{ SignIn::{constructor}(cli, row); }}
"
                    ))
                    .is_empty(),
                    "CONTROL: {constructor} behind {door}"
                );
            }
        }
    }

    #[test]
    fn ordinary_launch_and_session_constructors_are_not_provider_authority() {
        assert!(
            found("fn tick() { OsAppearanceSource::for_launch(false); Mode::for_session(sid); }\n")
                .is_empty()
        );
    }

    /// The shape step 9 removed, and its controls: the same read behind a door, or with no timer
    /// over it, is silent, so the finding is the timer path's and not the read's.
    #[test]
    fn a_timer_path_to_a_usage_read_is_found_with_its_chain() {
        let bad = found(
            "fn upkeep_live() { upkeep_pools(); }\n\
             fn upkeep_pools() { refresh_kimi_quota(); }\n\
             fn refresh_kimi_quota() { asked_read_use(); }\n",
        );
        assert_eq!(bad.len(), 1, "{bad:?}");
        assert!(
            bad[0]
                .detail
                .contains("upkeep_live -> upkeep_pools -> refresh_kimi_quota"),
            "{bad:?}"
        );
        assert!(
            found("fn upkeep_live() { launch(); }\nfn launch() { read_launch_rows(); }\nfn read_launch_rows() { launch_read_use(); }\n")
                .is_empty(),
            "CONTROL: a launch's door holds its row in use"
        );
        assert!(
            found("fn a_person() { refresh_kimi_quota(); }\nfn refresh_kimi_quota() { asked_read_use(); }\n")
                .is_empty(),
            "CONTROL: no timer reaches it"
        );
    }

    /// The upkeep's old Kimi refresh: a function value in the effects table, spawning the verb.
    /// CONTROL: the same spawn off any timer is silent.
    #[test]
    fn a_spawned_quota_tab_on_the_upkeep_is_found() {
        let spawn = "fn refresh_kimi_quota() {\n    cmd.args([\"accounts\", \"refresh\", \"--cli\", \"kimi\"]);\n}\n";
        let bad = found(&format!(
            "fn upkeep_live() {{ live_effects(); }}\nfn live_effects() -> E {{\n    E {{ refresh: &refresh_kimi_quota }}\n}}\n{spawn}"
        ));
        assert_eq!(bad.len(), 1, "{bad:?}");
        assert!(found(spawn).is_empty());
    }

    #[test]
    fn a_timed_wait_makes_a_function_a_timer() {
        let bad = found(
            "fn watcher() { loop { std::thread::sleep(EVERY); codex_row(); } }\n\
             fn codex_row() { launch_read_use(a, b); }\n",
        );
        assert_eq!(bad.len(), 1, "{bad:?}");
        assert!(
            found("fn watcher() { loop { codex_row(); } }\nfn codex_row() { launch_read_use(a, b); }\n")
                .is_empty(),
            "CONTROL: a loop with no timed wait is no timer"
        );
    }

    #[test]
    fn tests_and_prose_are_no_timer_path() {
        assert!(
            found(
                "#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() { std::thread::sleep(E); launch_read_use(a); }\n}\n\
                 // upkeep_live reads launch_read_use\n\
                 fn tick() { let why = \"launch_read_use\"; }\n"
            )
            .is_empty()
        );
    }

    /// A door is for the use it names: a timer that calls one has no use to point at, so the door
    /// does not excuse it. CONTROL: an event-driven caller of the same door is silent.
    #[test]
    fn a_timer_that_calls_a_door_is_found() {
        let door = "fn rotate_worker_at_wall() { wall_read_use; }\n";
        let bad = found(&format!(
            "fn watcher() {{ loop {{ rx.recv_timeout(EVERY); rotate_worker_at_wall(); }} }}\n{door}"
        ));
        assert_eq!(bad.len(), 1, "{bad:?}");
        assert!(bad[0].detail.contains("rotate_worker_at_wall"), "{bad:?}");
        assert!(
            found(&format!(
                "fn at_a_wall() {{ rotate_worker_at_wall(); }}\n{door}"
            ))
            .is_empty()
        );
    }

    /// A name many functions share is still a timer: the review's dead `tick` root. CONTROL: three
    /// functions of an ordinary shared name with no wait are no root.
    #[test]
    fn every_tick_is_a_timer_however_many_there_are() {
        let tick = "fn tick() { step(); }\n";
        let bad = found(&format!(
            "{tick}{tick}fn tick() {{ launch_read_use(a); }}\n"
        ));
        assert_eq!(bad.len(), 1, "{bad:?}");
        let run = "fn run() { launch_read_use(a); }\n";
        assert!(found(&format!("{run}{run}{run}")).is_empty());
    }

    /// The supervisor's loop waits through `wait` and `park`, so its name is its root. CONTROL: the
    /// same read behind the loop's turn-end door is silent.
    #[test]
    fn the_supervisors_wait_is_a_timer() {
        assert_eq!(
            found("fn wait_for_next() { launch_read_use(a); }\n").len(),
            1
        );
        assert!(
            found(
                "fn wait_for_next() { step(); }\nfn step() { usage_probe(); }\nfn usage_probe() { launch_read_use(a); }\n"
            )
            .is_empty()
        );
    }

    /// An over-shared name proves no call, so a helper called `read` cannot join every reader;
    /// CONTROL: the same chain through a name declared once is found.
    #[test]
    fn an_ambiguous_name_carries_no_edge() {
        let helper = "fn run() { launch_read_use(a); }\n";
        let thrice = format!("fn tick() {{ run(); }}\n{helper}{helper}{helper}");
        assert!(found(&thrice).is_empty());
        assert_eq!(found(&format!("fn tick() {{ run(); }}\n{helper}")).len(), 1);
    }
}
