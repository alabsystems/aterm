// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::tests::run;
use super::*;

const AT: &str = "crates/aterm-agent/src/harness/accounts_sync.rs";

fn found(src: &str) -> Vec<Finding> {
    run(&USAGE_READ_ON_TIMER, &[(AT, src)])
}

/// The graph joins shipped code alone (`TestCode::holds`): a module the root declares
/// test-only (`rig.rs`, whose name says nothing) adds no body, and a shipped module whose
/// name opens with `test` does.
#[test]
fn the_walk_reads_shipped_bodies_whatever_their_names() {
    let lib = "#[cfg(test)]\nmod rig;\npub mod test_binary;\n";
    let helper = "fn helper() {\n    Use::launch_read_use(evidence);\n}\n";
    let read = |rel: &str| {
        let found = run(
            &USAGE_READ_ON_TIMER,
            &[
                ("crates/aterm/src/lib.rs", lib),
                (
                    "crates/aterm/src/metrics.rs",
                    "fn tick() {\n    helper();\n}\n",
                ),
                (rel, helper),
            ],
        );
        found.into_iter().map(|f| f.path).collect::<Vec<_>>()
    };
    assert!(read("crates/aterm/src/rig.rs").is_empty());
    assert_eq!(
        read("crates/aterm/src/test_binary.rs"),
        ["crates/aterm/src/test_binary.rs"]
    );
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
        let bad =
            format!("fn tick() {{ helper(); }}\nfn helper() {{ Use::{constructor}(evidence); }}\n");
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
        found(
            "fn watcher() { loop { codex_row(); } }\nfn codex_row() { launch_read_use(a, b); }\n"
        )
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

/// Bodies of one name join across files, each word at its first spot: the earlier file's.
#[test]
fn a_word_keeps_its_first_spot_across_files() {
    let tick = "fn tick() {\n    launch_read_use(evidence);\n}\n";
    let late = "fn other() {}\nfn tick() {\n    launch_read_use(evidence);\n}\n";
    let spot = |files: &[(&str, &str)]| {
        let found = run(&USAGE_READ_ON_TIMER, files);
        found
            .iter()
            .map(|f| format!("{}:{}", f.path, f.line))
            .collect::<Vec<_>>()
    };
    let (a, b) = ("crates/aterm/src/a.rs", "crates/aterm/src/b.rs");
    assert_eq!(spot(&[(a, tick), (b, late)]), [format!("{a}:2")]);
    assert_eq!(spot(&[(b, late)]), [format!("{b}:3")]);
}
