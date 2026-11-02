// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The judges of `lab harness-proof`, each held to a good harness's observations (it passes) and to
//! each broken harness's (it fails): one that never relaunches, relaunches a new session, moves
//! while a draft or a person's typing is present, swallows an exit status, never re-execs, or
//! claims where no contract 3 is served.

use super::*;

const S1: &str = "01a148f8-9301-4752-96db-fef456f88074";
const S2: &str = "9e6f2b2c-1d1e-4c7a-8b3f-2a4c5d6e7f80";
const V1: &str = "1.4.10-R6427.1";
const V2: &str = "1.4.13-R6996.1";

fn frame(version: &str, pid: u32, session: &str, start: u32) -> Frame {
    Frame {
        version: version.to_string(),
        pid,
        session: session.to_string(),
        start,
        composer: String::new(),
    }
}

fn claimed(holder: &str) -> ClaimSeen {
    ClaimSeen {
        program: Some("muse-alab".to_string()),
        holder: Some(holder.to_string()),
        proven: true,
    }
}

fn after<'a>(f: Option<&'a Frame>, leader: (u32, u32), seen: ClaimSeen) -> After<'a> {
    After {
        frame: f,
        leader: (Some(leader.0), Some(leader.1)),
        seen,
    }
}

fn held(versions: &str, scope: &str) -> Held {
    Held {
        versions: versions.to_string(),
        scope: scope.to_string(),
    }
}

/// The failing checks' labels, so a test says which check a broken harness fails.
fn failing(checks: &[Check]) -> Vec<&str> {
    checks
        .iter()
        .filter(|c| !c.ok)
        .map(|c| c.label.as_str())
        .collect()
}

/// Assert `checks` fail, and that one failing check's label holds `what`.
fn fails_on(checks: &[Check], what: &str) {
    assert!(!passed(checks), "a broken harness passed: {checks:?}");
    assert!(
        failing(checks).iter().any(|l| l.contains(what)),
        "no failing check names `{what}`: {:?}",
        failing(checks)
    );
}

#[test]
fn a_leg_with_no_checks_holds_nothing() {
    assert!(!passed(&[]));
    assert!(passed(&[check(true, "x")]));
    assert!(!passed(&[check(true, "x"), check(false, "y")]));
}

/// The stand-in's frame is read off aterm's screen by its footer and the composer above it; a
/// screen without its footer has no frame.
#[test]
fn the_stand_ins_frame_is_read_off_the_screen() {
    let screen = format!(
        "OK 40 text\n\n  Muse Code {V1}\n\n❯ hello\n\n◆ stand-in reply 1 (start 2)\n\n{rule}\n❯ a draft, not sent\n{rule}\n  standin · {V1} · pid 4242 · session {S1} · start 2\n",
        rule = "─".repeat(40)
    );
    let f = frame_of(&screen).expect("a frame");
    assert_eq!(
        f,
        Frame {
            composer: "a draft, not sent".to_string(),
            ..frame(V1, 4242, S1, 2)
        }
    );
    let empty = screen.replace("❯ a draft, not sent", "❯ ");
    assert_eq!(frame_of(&empty).map(|f| f.composer), Some(String::new()));
    assert_eq!(frame_of("$ muse-alab\nmuse-alab: not installed\n$ "), None);
    assert_eq!(
        frame_of(&format!(
            "  standin · {V1} · pid x · session {S1} · start 1"
        )),
        None,
        "a pid that is not one"
    );
    assert!(said_signal("…\nstandin: got SIGTERM\n$ ", "SIGTERM"));
    assert!(!said_signal("standin: got SIGHUP", "SIGTERM"));
}

#[test]
fn status_who_blocks_and_the_log_are_read_as_aterm_writes_them() {
    let st = "OK sid=s-1 program=muse-alab agent=- supervisor=muse-alab@500 supervisor_proven=1 autopilot=cruise";
    assert_eq!(claim_seen(st), claimed("muse-alab@500"));
    let none = claim_seen("OK program=- supervisor=- supervisor_proven=0");
    assert_eq!(none, ClaimSeen::default());

    let who = "OK 2\n0 s-a driving=- watchers=0 turns=0 live nonce=00 fgpgid=77\n1 s-b driving=- watchers=0 turns=0 live nonce=11 fgpgid=-\n";
    assert_eq!(fgpgid(who, "s-a"), Some(77));
    assert_eq!(fgpgid(who, "s-b"), None);
    assert_eq!(fgpgid(who, "s-c"), None);

    let log = "1792253947.285 9 INFO h: harness @s-a: muse-alab 0.1.0 (muse 1.4.10-R6427.1) holds the session under contract 3, scope=relaunch,upgrade; aterm's built-in engine stands down from exactly that\n\
               1792253948.000 9 INFO h: harness @s-b: grok-alab 0.1.0 (grok 1.0) holds the session under contract 3, scope=relaunch; aterm's built-in engine stands down from exactly that\n\
               1792253949.000 9 INFO h: harness @s-a: relaunch pid=12 tab=s-a session=x from=- to=- step=wait:turn-open\n\
               1792253950.000 9 INFO h: harness @s-a: relaunch pid=12 tab=s-a session=x from=- to=- step=typed\n\
               1792253951.000 9 INFO h: harness @s-a: upgrade pid=12 tab=s-a session=x from=a to=b step=skip:self-claimed\n\
               1792253952.000 9 INFO h: harness @s-b: upgrade pid=13 tab=s-b session=y from=a to=b step=terminated\n";
    assert_eq!(
        held_lines(log, "s-a"),
        [held(
            "muse-alab 0.1.0 (muse 1.4.10-R6427.1)",
            "relaunch,upgrade"
        )]
    );
    assert_eq!(held_lines(log, "s-c"), []);
    assert_eq!(
        builtin_acts(log, "s-a", "relaunch").len(),
        1,
        "the wait is no act"
    );
    assert_eq!(builtin_acts(log, "s-a", "upgrade"), Vec::<String>::new());
    assert_eq!(builtin_acts(log, "s-b", "upgrade").len(), 1);

    let blocks = blocks_of(
        r#"OK {"blocks":[{"id":1,"state":"complete","exit":130,"cmdline":"muse-alab"},{"id":2,"state":"complete","exit":0,"cmdline":" ( cd -- '/w' && exec '/p/agents/muse' 'resume' '01a148f8-9301-4752-96db-fef456f88074' )"},{"id":3,"state":"executing","exit":null,"cmdline":"muse-alab"}]}"#,
    );
    assert_eq!(blocks.len(), 3);
    assert_eq!(
        exit_of(&blocks, "muse-alab"),
        None,
        "the newest run is still executing; the older run's 130 is not its exit"
    );
    assert_eq!(
        exit_of(&blocks[..2], "muse-alab"),
        Some(130),
        "the newest, complete"
    );
    assert_eq!(exit_of(&blocks, "make"), None);
    assert!(resume_typed(&blocks, S1));
    assert!(!resume_typed(&blocks, S2));
    assert_eq!(blocks_of("ERR no such session"), []);
}

/// THE CLAIM: the foreground leader's proven claim, logged with exactly its scope, passes; an
/// unrostered harness, a claim by another pid (the child's), an unproven one, a wrong scope or no
/// logged claim fails.
#[test]
fn the_claim_judge_wants_the_leaders_proven_claim_with_its_scope() {
    let scope = "relaunch,upgrade";
    let log = [held("muse-alab 0.1.0 (muse 1.4.10-R6427.1)", scope)];
    let good = claim(
        "muse-alab",
        scope,
        true,
        &claimed("muse-alab@500"),
        Some(500),
        &log,
    );
    assert!(passed(&good), "{good:?}");

    fails_on(
        &claim(
            "muse-alab",
            scope,
            false,
            &claimed("muse-alab@500"),
            Some(500),
            &log,
        ),
        "roster",
    );
    fails_on(
        &claim(
            "muse-alab",
            scope,
            true,
            &claimed("muse-alab@501"),
            Some(500),
            &log,
        ),
        "supervisor=muse-alab@500",
    );
    let unproven = ClaimSeen {
        proven: false,
        ..claimed("muse-alab@500")
    };
    fails_on(
        &claim("muse-alab", scope, true, &unproven, Some(500), &log),
        "proven",
    );
    for wrong in ["relaunch", "relaunch,upgrade,turn-end"] {
        fails_on(
            &claim(
                "muse-alab",
                scope,
                true,
                &claimed("muse-alab@500"),
                Some(500),
                &[held("muse-alab 0.1.0 (muse 1)", wrong)],
            ),
            "scope=relaunch,upgrade",
        );
    }
    fails_on(
        &claim(
            "muse-alab",
            scope,
            true,
            &claimed("muse-alab@500"),
            Some(500),
            &[held("grok-alab 0.1.0 (grok 1)", scope)],
        ),
        "logged the claim",
    );
    fails_on(
        &claim(
            "muse-alab",
            scope,
            true,
            &ClaimSeen::default(),
            Some(500),
            &[],
        ),
        "supervisor=",
    );
    fails_on(
        &claim(
            "muse-alab",
            scope,
            true,
            &claimed("muse-alab@500"),
            None,
            &log,
        ),
        "supervisor=",
    );
}

/// THE CRASH RELAUNCH: a new child on the same session as its next start, the harness's pid and
/// claim kept and the built-in lane quiet, passes; a harness that never relaunches, brings the same
/// pid back, opens a new session, skips the start count, exits and is started again, loses its
/// claim, or leaves the relaunch to aterm's built-in lane, fails.
#[test]
fn the_relaunch_judge_fails_a_harness_that_never_relaunches_or_relaunches_wrong() {
    let id = "muse-alab";
    let before = frame(V1, 600, S1, 1);
    let next = frame(V1, 601, S1, 2);
    let held_claim = || claimed("muse-alab@500");
    let good = relaunch(
        id,
        &before,
        &after(Some(&next), (500, 500), held_claim()),
        false,
    );
    assert!(passed(&good), "{good:?}");

    fails_on(
        &relaunch(id, &before, &after(None, (500, 500), held_claim()), false),
        "a new child",
    );
    fails_on(
        &relaunch(
            id,
            &before,
            &after(Some(&before), (500, 500), held_claim()),
            false,
        ),
        "a new child",
    );
    let fresh = frame(V1, 601, S2, 1);
    fails_on(
        &relaunch(
            id,
            &before,
            &after(Some(&fresh), (500, 500), held_claim()),
            false,
        ),
        "same session",
    );
    let skipped = frame(V1, 601, S1, 3);
    fails_on(
        &relaunch(
            id,
            &before,
            &after(Some(&skipped), (500, 500), held_claim()),
            false,
        ),
        "next start",
    );
    fails_on(
        &relaunch(
            id,
            &before,
            &after(Some(&next), (500, 502), claimed("muse-alab@502")),
            false,
        ),
        "kept its pid",
    );
    fails_on(
        &relaunch(
            id,
            &before,
            &after(Some(&next), (500, 500), ClaimSeen::default()),
            false,
        ),
        "claim still stands",
    );
    fails_on(
        &relaunch(
            id,
            &before,
            &after(Some(&next), (500, 500), held_claim()),
            true,
        ),
        "built-in relaunch lane stood down",
    );
}

/// NO MOVE WHILE A DRAFT OR A PERSON'S TYPING IS PRESENT: every read the old child, with the draft
/// standing, passes; a harness that moves while a draft is present (one read shows the new build),
/// a window with no reads, a read with no frame, or a draft that went, fails.
#[test]
fn the_hold_judge_fails_a_harness_that_moves_while_a_draft_is_present() {
    let before = Frame {
        composer: "lab draft".to_string(),
        ..frame(V1, 600, S1, 1)
    };
    let still = || Some(before.clone());
    let good = held_still(
        "a draft is present",
        &before,
        &[still(), still(), still()],
        Some("lab draft"),
    );
    assert!(passed(&good), "{good:?}");
    let person = held_still("a person is typing", &before, &[still(), still()], None);
    assert!(passed(&person), "{person:?}");

    let moved_mid = Some(Frame {
        composer: "lab draft".to_string(),
        ..frame(V2, 601, S1, 2)
    });
    fails_on(
        &held_still(
            "a draft is present",
            &before,
            &[still(), moved_mid.clone(), still()],
            Some("lab draft"),
        ),
        "no move while a draft is present",
    );
    fails_on(
        &held_still("a person is typing", &before, &[still(), moved_mid], None),
        "no move while a person is typing",
    );
    fails_on(
        &held_still("a draft is present", &before, &[], Some("lab draft")),
        "no move",
    );
    fails_on(
        &held_still(
            "a draft is present",
            &before,
            &[still(), None],
            Some("lab draft"),
        ),
        "no move",
    );
    let cleared = Some(frame(V1, 600, S1, 1));
    fails_on(
        &held_still(
            "a draft is present",
            &before,
            &[still(), cleared],
            Some("lab draft"),
        ),
        "stood in the composer",
    );
}

/// THE MOVE: the child back on the new build as a new process on the same session, the harness's
/// pid and claim kept and the built-in lane quiet, passes; a harness that never moves, moves onto
/// a new session, exits to move, or leaves the move to aterm, fails.
#[test]
fn the_move_judge_wants_the_new_build_on_the_same_session_under_the_same_harness() {
    let id = "muse-alab";
    let before = frame(V1, 600, S1, 2);
    let onto = frame(V2, 610, S1, 3);
    let good = moved(
        id,
        &before,
        V2,
        &after(Some(&onto), (500, 500), claimed("muse-alab@500")),
        false,
    );
    assert!(passed(&good), "{good:?}");

    fails_on(
        &moved(
            id,
            &before,
            V2,
            &after(Some(&before), (500, 500), claimed("muse-alab@500")),
            false,
        ),
        "moved onto the new build",
    );
    fails_on(
        &moved(
            id,
            &before,
            V2,
            &after(None, (500, 500), claimed("muse-alab@500")),
            false,
        ),
        "moved onto the new build",
    );
    let fresh = frame(V2, 610, S2, 1);
    fails_on(
        &moved(
            id,
            &before,
            V2,
            &after(Some(&fresh), (500, 500), claimed("muse-alab@500")),
            false,
        ),
        "same session",
    );
    fails_on(
        &moved(
            id,
            &before,
            V2,
            &after(Some(&onto), (500, 520), claimed("muse-alab@520")),
            false,
        ),
        "kept its pid",
    );
    fails_on(
        &moved(
            id,
            &before,
            V2,
            &after(Some(&onto), (500, 500), claimed("muse-alab@500")),
            true,
        ),
        "built-in upgrade lane stood down",
    );
}

/// SIGNAL AND EXIT PROPAGATION: the group's SIGINT reaches the child and both run on; the shell
/// sees the child's exit, and the child the forwarded signal, with nothing relaunched; a harness
/// that dies of the SIGINT, swallows a status, never forwards a signal or relaunches after an
/// intended end, fails.
#[test]
fn the_signal_and_exit_judges_want_the_childs_status_and_no_relaunch() {
    let before = frame(V1, 600, S1, 1);
    let good = interrupted(
        &before,
        &after(Some(&before), (500, 500), claimed("muse-alab@500")),
        true,
    );
    assert!(passed(&good), "{good:?}");
    fails_on(
        &interrupted(
            &before,
            &after(Some(&before), (500, 500), claimed("muse-alab@500")),
            false,
        ),
        "got SIGINT",
    );
    let gone = After {
        frame: None,
        leader: (Some(500), None),
        seen: ClaimSeen::default(),
    };
    fails_on(&interrupted(&before, &gone, true), "kept running");
    fails_on(&interrupted(&before, &gone, true), "kept its pid");

    assert!(passed(&ended("an exit 130", Some(130), 130, None, false)));
    assert!(passed(&ended(
        "SIGTERM",
        Some(143),
        143,
        Some(("SIGTERM", true)),
        false
    )));
    fails_on(&ended("an exit 130", Some(0), 130, None, false), "exit 130");
    fails_on(&ended("an exit 130", None, 130, None, false), "exit 130");
    fails_on(
        &ended("SIGTERM", Some(143), 143, Some(("SIGTERM", false)), false),
        "got SIGTERM",
    );
    fails_on(
        &ended("SIGHUP", Some(129), 129, Some(("SIGHUP", true)), true),
        "nothing was relaunched",
    );
}

/// THE HARNESS'S OWN RE-EXEC: the same pid on the new image, claimed again with the new version,
/// the child running on, passes; a harness that never re-execs, exits and is restarted, loses its
/// claim, or restarts its child, fails.
#[test]
fn the_reexec_judge_wants_the_same_pid_on_the_new_image_and_the_child_kept() {
    let id = "muse-alab";
    let child = frame(V2, 610, S1, 3);
    let next = "0.1.0+gabcdef012";
    let log = [
        held("muse-alab 0.1.0 (muse 1.4.13-R6996.1)", "relaunch,upgrade"),
        held(
            &format!("muse-alab {next} (muse 1.4.13-R6996.1)"),
            "relaunch,upgrade",
        ),
    ];
    let a = after(Some(&child), (500, 500), claimed("muse-alab@500"));
    let good = reexec(id, next, true, &log, (&child, Some(&child)), &a);
    assert!(passed(&good), "{good:?}");

    fails_on(
        &reexec(id, next, false, &log, (&child, Some(&child)), &a),
        "new build's image",
    );
    fails_on(
        &reexec(id, next, true, &log[..1], (&child, Some(&child)), &a),
        "made again with the new version",
    );
    let restarted = after(Some(&child), (500, 530), claimed("muse-alab@530"));
    fails_on(
        &reexec(id, next, true, &log, (&child, Some(&child)), &restarted),
        "kept its pid",
    );
    let lost = after(Some(&child), (500, 500), ClaimSeen::default());
    fails_on(
        &reexec(id, next, true, &log, (&child, Some(&child)), &lost),
        "claim still stands",
    );
    let relaunched = frame(V2, 611, S1, 4);
    fails_on(
        &reexec(id, next, true, &log, (&child, Some(&relaunched)), &a),
        "ran on through it",
    );
}

/// THE NEGATIVE CONTROL: where no contract 3 is served, the vendor in the harness's place
/// (program and leader the stand-in's), unclaimed, passes; a harness that stays (its own name,
/// its own pid leading) or claims anyway fails. The built-in lanes then take the session: their
/// resume line and the vendor back on its session pass; no line, or a new session, fails.
#[test]
fn the_negative_control_wants_a_pass_through_the_built_in_lanes_take() {
    let standin = frame(V1, 700, S1, 1);
    let through = ClaimSeen {
        program: Some("muse".to_string()),
        holder: None,
        proven: false,
    };
    let good = passed_through("muse", &through, Some(700), Some(&standin));
    assert!(passed(&good), "{good:?}");

    let stayed = ClaimSeen {
        program: Some("muse-alab".to_string()),
        ..ClaimSeen::default()
    };
    fails_on(
        &passed_through("muse", &stayed, Some(500), Some(&standin)),
        "program muse",
    );
    fails_on(
        &passed_through("muse", &through, Some(500), Some(&standin)),
        "took the harness's place",
    );
    let claimed_anyway = ClaimSeen {
        holder: Some("muse-alab@700".to_string()),
        ..through.clone()
    };
    fails_on(
        &passed_through("muse", &claimed_anyway, Some(700), Some(&standin)),
        "nothing claimed",
    );
    fails_on(
        &passed_through("muse", &through, Some(700), None),
        "took the harness's place",
    );

    let back = frame(V1, 701, S1, 2);
    assert!(passed(&builtin_took(&standin, true, Some(&back))));
    fails_on(
        &builtin_took(&standin, false, Some(&back)),
        "typed the resume line",
    );
    fails_on(
        &builtin_took(&standin, true, Some(&frame(V1, 701, S2, 1))),
        "same session",
    );
    fails_on(&builtin_took(&standin, true, None), "same session");
}
