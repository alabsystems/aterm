// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_results_run_names_its_scenario() {
    let cases = [
        ("results/s1-plain-01", Some("s1-plain")),
        ("results/c3-wall-12", Some("c3-wall")),
        ("results/s2-text-1.0-03", Some("s2-text-1.0")),
        ("results/s1-plain-1", None),
        ("results/s1-plain", None),
        ("tapes", None),
    ];
    for (run, want) in cases {
        assert_eq!(scenario_of(Path::new(run)).as_deref(), want, "{run}");
    }
    for name in ["s1-plain", "c1-approval", "s2-text-1.0"] {
        assert!(corpus_name(name), "{name}");
    }
    for name in ["", "S1", "-x", "a/b", "a b", ".hidden"] {
        assert!(!corpus_name(name), "{name:?}");
    }
}

#[test]
fn the_jails_profile_names_its_binary_and_tape_only() {
    let profile =
        jail_profile(Path::new("/b/aterm"), Path::new("/t/run.atape.txt")).expect("a profile");
    for rule in [
        "(deny network*)",
        "(deny file-write*)",
        "(deny process-fork)",
        "(deny process-exec)",
        "(allow process-exec (literal \"/b/aterm\"))",
        "(deny file-read-data)",
        "(allow file-read-data (literal \"/b/aterm\") (literal \"/t/run.atape.txt\") (literal \"/\"))",
    ] {
        assert!(profile.contains(rule), "{rule}\n{profile}");
    }
    assert!(!profile.contains('@'), "every hole filled: {profile}");
    assert!(jail_profile(Path::new("/b/a\"terminal"), Path::new("/t")).is_err());
}

#[test]
fn a_failed_run_is_not_promoted() {
    let owned = aterm_tempfile::tempdir().expect("owned scratch");
    let run = owned.path().join("s1-plain-01");
    std::fs::create_dir(&run).expect("a results run");
    for (summary, says) in [
        (r#"{"k": 1, "error": "no card"}"#, "error: no card"),
        (
            r#"{"exception": "the lab did not start"}"#,
            "exception: the lab did not start",
        ),
        ("{not json", "summary.json does not read"),
    ] {
        std::fs::write(run.join("summary.json"), summary).expect("a summary");
        let e = promote_into(&run, "s1-plain", true).expect_err(summary);
        assert!(e.contains(says), "{summary}: {e}");
    }
    std::fs::write(run.join("summary.json"), r#"{"k": 1, "error": ""}"#).expect("a summary");
    let e = promote_into(&run, "s1-plain", true).expect_err("no tape");
    assert!(e.contains("carries its end"), "{e}");
}

#[test]
fn a_tape_holding_fewer_names_what_it_lost() {
    let old = Census {
        approvals: 1,
        turn_ends: 1,
        acts: 2,
        ..Census::default()
    };
    let host_only = Census {
        turn_ends: 2,
        acts: 1,
        ..Census::default()
    };
    assert_eq!(lost(&old, &host_only), ["approval(s)", "act(s)"]);
    assert!(lost(&old, &old).is_empty());
    let more = Census {
        questions: 1,
        acts: 3,
        ..old
    };
    assert!(lost(&old, &more).is_empty(), "more of a kind loses nothing");
    assert_eq!(lost(&more, &old), ["question(s)", "act(s)"]);
    let wall = Census {
        turn_ends: 5,
        walls: 2,
        acts: 3,
        ..Census::default()
    };
    let again = Census {
        turn_ends: 4,
        ..wall
    };
    assert!(lost(&wall, &again).is_empty(), "{:?}", lost(&wall, &again));
    let no_retry = Census {
        walls: 0,
        acts: 1,
        ..again
    };
    assert_eq!(lost(&wall, &no_retry), ["wall(s)", "act(s)"]);
}
