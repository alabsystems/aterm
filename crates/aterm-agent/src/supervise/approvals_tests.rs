// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

/// Land's instruction as the person's switch over a held tab types it (v6 §14.17 item 3) opens
/// with its why: the ledger's readers read it as Land's own, and a why with nothing after it
/// as no instruction.
#[test]
fn the_switchs_ask_reads_as_lands_instruction() {
    use crate::supervise::policy::turn_end::{LAND, is_switch_ask, past_switch_why, switch_why};
    let mark = crate::harness::harness_mark!();
    let ask = format!(
        "{mark} {}{}",
        switch_why("All done."),
        LAND.strip_prefix(mark).unwrap_or(LAND)
    );
    assert!(is_switch_ask(&ask), "{ask}");
    assert_eq!(past_switch_why(&ask), LAND);
    assert!(super::gives_land(Some(&ask)), "{ask}");
    assert!(super::delivers_land(Some(&ask)), "{ask}");
    let bare = format!("{mark} {}", switch_why("All done."));
    assert!(!super::gives_land(Some(&bare)) && !super::delivers_land(Some(&bare)));
    assert!(!is_switch_ask(LAND) && past_switch_why(LAND) == LAND);
}

/// A RESTART COUNTS LAND'S CONTRADICTION AS THE NUDGE IT IS (v6 §14.4's slice 4c): a ledger
/// whose `land-nudge@v1` row typed the contradiction seeds a restarted loop with one nudge,
/// and the contradiction typed as a compaction's owed continuation delivers Land's act, read
/// by its words ([`super::delivers_land`]).
/// Negative controls: the count keyed on a command that opens [`LAND_NUDGE`] in place of the
/// rule, and `delivers_land`'s list without `LAND_CONTRADICTED_HEAD`, each fail it.
#[test]
fn a_restart_counts_lands_contradiction_as_its_nudge() {
    use crate::harness::repo_work::{RepoRead, RepoWork};
    use crate::supervise::policy::turn_end::{
        LAND, RULE_COMPACT, RULE_LAND, RULE_LAND_NUDGE, land_contradicted,
    };
    let sid = Some("@s-9");
    let work = RepoWork {
        repo: "/r/proj/land-c".to_string(),
        untracked: 1,
        paths: vec!["wip-notes.txt".to_string()],
        main_ref: "origin/main".to_string(),
        ..RepoWork::default()
    };
    let said = land_contradicted(&RepoRead::Found(work), None, &[], None).expect("git's leg");
    let reason = act_reason(None);
    let body = |rows: &[(i64, &str, &str)]| -> String {
        rows.iter()
            .map(|&(ts, rule_id, command)| {
                let row = Row {
                    rule_id,
                    outcome: Outcome::Typed,
                    command,
                    reason: &reason,
                    box_seq: 1,
                };
                format!("{}\n", row.to_json(ts, sid))
            })
            .collect()
    };
    let nudged = [
        (10_000, RULE_LAND, LAND),
        (20_000, RULE_LAND_NUDGE, said.as_str()),
    ];
    let ep = land_episode_in(&body(&nudged), sid).expect("the episode");
    assert_eq!((ep.nudges, ep.landing), (1, true), "{ep:?}");
    assert_eq!(ep.owed, Some(RULE_LAND_NUDGE), "{ep:?}");
    let carried = [
        (10_000, RULE_LAND, LAND),
        (20_000, RULE_COMPACT, said.as_str()),
    ];
    let ep = land_episode_in(&body(&carried), sid).expect("the episode");
    assert!(
        ep.landing,
        "the contradiction carried by a compaction: {ep:?}"
    );
    assert!(super::delivers_land(Some(&said)), "{said}");
}

#[test]
fn the_replies_note_carries_the_run_and_the_reset_and_an_older_note_reads_none() {
    use super::{Kept, Replies, replies_note, replies_note_of};
    let kept = Kept {
        mark: 0x432b_225c_f708_95ef,
        answers: 2,
        walled: true,
        tries: 1,
    };
    let said = Replies {
        run: 4,
        ..Replies::of(&[kept])
    };
    let note = replies_note(said, Some(1_792_972_800_000));
    assert_eq!(
        replies_note_of(&format!("the turn-end policy{note}")),
        Some((said, Some(1_792_972_800_000)))
    );
    let older = note.replace("; reset at 1792972800000", "");
    assert_ne!(older, note);
    assert_eq!(replies_note_of(&older), Some((said, None)));
    let oldest = older.replace("; 4 in a row", "");
    assert_ne!(oldest, older);
    assert_eq!(replies_note_of(&oldest), Some((Replies::of(&[kept]), None)));
}

/// THE HOLD AND THE LAND CHECK'S BOOKS RIDE THE LEDGER (the post-landing review, 2026-11-01):
/// a point's replies stand past an Off no person set ([`OFF_STOOD_HELD`]) and end at the
/// person's ([`OFF_STOOD`]); the land check's books read back as written, and a reason with no
/// such note (an older build's) reads none.
#[test]
fn the_hold_and_the_land_checks_books_ride_the_ledger() {
    use super::{
        Carried, Kept, LandBooks, OFF_STOOD, OFF_STOOD_HELD, OFF_STOOD_RESTING, OffStood, Outcome,
        RULE_OFF_STOOD, RULE_POINT, Replies, Row, act_reason_after, json_str, land_books_note,
        off_kept_in, point_reason, short_streak_in,
    };
    use crate::supervise::policy::turn_end::{LAND, RULE_LAND, switch_why};
    let sid = Some("@s-9");
    let typed_row = |ts: i64, rule_id: &str, outcome: Outcome, command: &str, reason: &str| {
        let row = Row {
            rule_id,
            outcome,
            command,
            reason,
            box_seq: 1,
        };
        format!("{}\n", row.to_json(ts, sid))
    };
    let row = |ts: i64, rule_id: &str, outcome: Outcome, reason: &str| {
        typed_row(ts, rule_id, outcome, "", reason)
    };
    let kept = Kept {
        mark: 0x432b_225c_f708_95ef,
        answers: 2,
        walled: false,
        tries: 0,
    };
    let mut kept_tokens = [None; crate::supervise::policy::turn_end::KEPT_TOKENS];
    kept_tokens[crate::supervise::policy::turn_end::KEPT_TOKENS - 1] =
        crate::harness::leftover::token_of("2606@1792098840004410:3");
    let land = LandBooks {
        audited: [None, Some(0x0a), Some(0x0123_4567_89ab_cdef)],
        sent: Some(0xfeed),
        kept: kept_tokens,
        kept_over: true,
        left_holds: 2,
    };
    let at = Carried {
        said: Replies::of(&[kept]),
        land,
        ..Carried::streak(1)
    };
    let point = row(
        10_000,
        RULE_POINT,
        Outcome::Waiting,
        &point_reason(at, false),
    );
    let seed = short_streak_in(&point, sid).expect("a seed");
    assert_eq!((seed.said, seed.land), (at.said, land));
    let acted = row(
        11_000,
        RULE_POINT,
        Outcome::Typed,
        &act_reason_after(at, None),
    );
    assert_eq!(short_streak_in(&acted, sid).map(|s| s.land), Some(land));
    let held = format!(
        "{point}{}",
        row(20_000, RULE_OFF_STOOD, Outcome::Waiting, OFF_STOOD_HELD)
    );
    assert_eq!(
        short_streak_in(&held, sid).map(|s| s.said),
        Some(at.said),
        "an Off no person set keeps the replies that hold"
    );
    let persons = format!(
        "{point}{}",
        row(20_000, RULE_OFF_STOOD, Outcome::Waiting, OFF_STOOD)
    );
    assert_eq!(
        short_streak_in(&persons, sid).map(|s| s.said),
        Some(Replies::default()),
        "the person's Off ends them"
    );
    let resting = format!(
        "{point}{}",
        row(20_000, RULE_OFF_STOOD, Outcome::Waiting, OFF_STOOD_RESTING)
    );
    assert_eq!(
        short_streak_in(&resting, sid).map(|s| s.said),
        Some(at.said),
        "an Off no person set over Land's rest keeps the replies that hold too"
    );
    // The last Off row says whose it was, and what its park kept ([`off_kept_in`]).
    assert_eq!(off_kept_in(&held, sid), OffStood::Held);
    assert_eq!(off_kept_in(&resting, sid), OffStood::Resting);
    assert_eq!(off_kept_in(&persons, sid), OffStood::Person);
    assert_eq!(
        off_kept_in(
            &format!(
                "{held}{}",
                row(30_000, RULE_OFF_STOOD, Outcome::Waiting, OFF_STOOD)
            ),
            sid
        ),
        OffStood::Person
    );
    assert_eq!(off_kept_in(&point, sid), OffStood::Person, "no Off at all");
    // THE LAND EPISODE RIDES PAST AN OFF NO PERSON SET OVER LAND'S REST
    // ([`OFF_STOOD_RESTING`]), so a loop the host starts as it lifts reads its point as the
    // check's answer again; any other Off ends it.
    let checked = format!(
        "{}{}{point}",
        typed_row(1_000, RULE_LAND, Outcome::Typed, LAND, "land"),
        typed_row(
            5_000,
            crate::supervise::policy::land_check::RULE_LAND_CHECK,
            Outcome::Typed,
            "land check",
            "check"
        ),
    );
    for (reason, kept) in [
        (OFF_STOOD_RESTING, true),
        (OFF_STOOD_HELD, false),
        (OFF_STOOD, false),
    ] {
        let body = format!(
            "{checked}{}",
            row(20_000, RULE_OFF_STOOD, Outcome::Waiting, reason)
        );
        assert_eq!(
            super::land_episode_in(&body, sid).is_some_and(|e| e.checked),
            kept,
            "{reason}"
        );
    }
    assert!(super::land_episode_in(&checked, sid).is_some_and(|e| e.checked));
    // AN OLDER OFF'S VERDICT IS NOT A LATER LIFT'S (the second review, 2026-11-01): the
    // session ran under a word after the held Off's row, so the Off a lift then ends wrote
    // none (a revoked worker's), and the held row says nothing of it; the row nearest the
    // lift still counts.
    for after in [
        row(
            30_000,
            RULE_POINT,
            Outcome::Waiting,
            &point_reason(at, false),
        ),
        typed_row(30_000, RULE_LAND, Outcome::Typed, LAND, "land"),
    ] {
        assert_eq!(
            off_kept_in(&format!("{held}{after}"), sid),
            OffStood::Person,
            "{after}"
        );
        assert_eq!(
            off_kept_in(
                &format!(
                    "{held}{after}{}",
                    row(40_000, RULE_OFF_STOOD, Outcome::Waiting, OFF_STOOD_HELD)
                ),
                sid
            ),
            OffStood::Held
        );
    }
    // THE SWITCH'S ONE ASK, spent on the reply that held when it was typed, rides the rows
    // after it, and the person's Off ends it.
    let mark = crate::harness::harness_mark!();
    let ask = format!(
        "{mark} {}{}",
        switch_why("All done."),
        LAND.strip_prefix(mark).unwrap_or(LAND)
    );
    let asked = format!(
        "{point}{}{}",
        typed_row(
            12_000,
            RULE_LAND,
            Outcome::Typed,
            &ask,
            &act_reason_after(at, None)
        ),
        row(
            13_000,
            RULE_POINT,
            Outcome::Waiting,
            &point_reason(at, false)
        )
    );
    assert_eq!(
        short_streak_in(&asked, sid).and_then(|s| s.switch_spent),
        Some(kept.mark)
    );
    assert_eq!(
        short_streak_in(
            &format!(
                "{asked}{}",
                row(14_000, RULE_OFF_STOOD, Outcome::Waiting, OFF_STOOD_HELD)
            ),
            sid
        )
        .and_then(|s| s.switch_spent),
        Some(kept.mark),
        "an Off no person set keeps it"
    );
    assert_eq!(
        short_streak_in(
            &format!(
                "{asked}{}",
                row(14_000, RULE_OFF_STOOD, Outcome::Waiting, OFF_STOOD)
            ),
            sid
        )
        .and_then(|s| s.switch_spent),
        None,
        "the person's Off ends it"
    );
    let plain = asked.replace(&json_str(&ask), &json_str(LAND));
    assert_ne!(plain, asked);
    assert_eq!(
        short_streak_in(&plain, sid).and_then(|s| s.switch_spent),
        None,
        "CONTROL: Land's own act spends no ask"
    );
    let none = Carried::streak(1);
    assert_eq!(land_books_note(none.land), "", "no check: no note");
    let older = point.replace(&land_books_note(land), "");
    assert_ne!(older, point);
    assert_eq!(
        short_streak_in(&older, sid).map(|s| s.land),
        Some(LandBooks::default())
    );
}

#[test]
fn the_newest_rows_time_is_the_books_last() {
    let body = "{\"ts\":1792594893546,\"sid\":null}\n\
                {\"ts\":1792594893533,\"sid\":null}\n\
                \n\
                not a row\n";
    assert_eq!(super::last_row_ms(body), Some(1_792_594_893_546));
    assert_eq!(super::last_row_ms(""), None);
    assert_eq!(super::last_row_ms("{\"t\":5}\n"), None, "a journal line");
}

#[test]
fn an_acts_row_counts_its_sleep_on_a_boot_in_the_kernels_spelling() {
    let id = [
        0x4D, 0x5A, 0x1B, 0x2C, 0x3E, 0x4F, 0x50, 0x61, 0x72, 0x83, 0x94, 0xA5, 0xB6, 0xC7, 0xD8,
        0xE9,
    ];
    let (upper, lower) = (
        "4D5A1B2C-3E4F-5061-7283-94A5B6C7D8E9",
        "4d5a1b2c-3e4f-5061-7283-94a5b6c7d8e9",
    );
    let row = Row {
        rule_id: super::super::policy::turn_end::RULE_LIMIT_RESUME,
        outcome: Outcome::Typed,
        command: "continue",
        reason: "the limit reset",
        box_seq: 3,
    };
    let at = 1_790_000_000_000;
    let count = |boot: Option<Boot>| SleepStamp {
        asleep: std::time::Duration::from_millis(1_800_003),
        boot,
    };
    let tail = |text: &str| format!(",\"slept_ms\":1800003,\"boot\":\"{text}\"}}");
    let read = |line: &str| last_act_count_in(&format!("{line}\n"), Some("s-1"));
    for text in [upper, lower] {
        let read_back = count(Boot::of(text));
        assert_eq!(read_back.boot.map(|b| b.id()), Some(id), "{text}");
        let line = row.to_json_counted(at, Some("@s-1"), Some(&read_back));
        assert!(line.ends_with(&tail(text)), "written as read: {line}");
        assert_eq!(read(&line), Some((at, Some(read_back.clone()))), "{text}");
        assert_eq!(
            read(&line).and_then(|(_, c)| c?.since(&count(Some(Boot::kernel(id))))),
            Some(std::time::Duration::ZERO),
            "{text}"
        );
    }
    let kernel = if cfg!(any(target_os = "linux", target_os = "android")) {
        lower
    } else {
        upper
    };
    let live = count(Some(Boot::kernel(id)));
    let line = row.to_json_counted(at, Some("@s-1"), Some(&live));
    assert!(
        line.ends_with(&tail(kernel)),
        "this kernel's spelling: {line}"
    );
    for damaged in ["00000000-0000-0000-0000-000000000000", "test-boot", ""] {
        assert_eq!(
            read(&line.replace(kernel, damaged)),
            Some((at, None)),
            "CONTROL {damaged:?}"
        );
    }
    assert_eq!(
        row.to_json_counted(1, Some("@s-1"), Some(&count(None))),
        row.to_json(1, Some("@s-1")),
        "CONTROL: no boot, no count"
    );
}

#[test]
fn the_journal_sits_beside_the_ledger_and_both_are_bounded() {
    let dir = crate::supervise::ScratchDir::new("apr", "bound");
    let ledger = path_under(&dir, Some("@s-7")).expect("the drive dir");
    assert_eq!(
        journal_beside(&ledger),
        dir.join("drive").join("s-7.journal.jsonl")
    );
    let row = format!("{{\"ts\":1,\"pad\":\"{}\"}}", "x".repeat(300));
    let rows = usize::try_from(MAX_BYTES).unwrap() / row.len() + 50;
    let body: String = (0..rows).map(|k| format!("{row}{k}\n")).collect();
    std::fs::write(&ledger, &body).expect("write");
    bound(&ledger);
    let cut = std::fs::read_to_string(&ledger).expect("read");
    assert_eq!(cut.lines().count(), KEEP_ROWS);
    assert!(
        cut.ends_with(&format!("{row}{}\n", rows - 1)),
        "the newest rows kept"
    );
    let small = dir.join("drive").join("small.jsonl");
    std::fs::write(&small, "{\"ts\":1}\n").expect("write");
    bound(&small);
    assert_eq!(
        std::fs::read_to_string(&small).expect("read"),
        "{\"ts\":1}\n"
    );
}

#[test]
fn a_file_past_sixteen_mib_is_streamed_and_cut() {
    use super::super::policy::turn_end::RULE_LAND;
    let dir = crate::supervise::ScratchDir::new("apr", "big");
    let past = 17 << 20;
    let sid = "s-195cbb43bd1fc3dc2c7b";
    let journal = journal_beside(&path_under(&dir, Some(sid)).expect("the drive dir"));
    let mut body = String::with_capacity(past + 4096);
    let mut t = 1_791_661_422_112_u64;
    while body.len() <= past {
        body.push_str(&format!(
            "{{\"t\":{t},\"sid\":\"{sid}\",\"kind\":\"other\",\"phase\":\"-\",\"seq\":null,\
             \"complete\":null,\"rows\":null,\"summary\":\"HOST seq=1 upgrade \
             step=wait:session-files-unreadable\",\"line\":\"HOST seq=1 upgrade \
             step=wait:session-files-unreadable\",\"turn\":null,\"report\":null}}\n"
        ));
        t += 545;
    }
    std::fs::write(&journal, &body).expect("write");
    bound(&journal);
    let cut = std::fs::read_to_string(&journal).expect("read");
    assert_eq!(cut.lines().count(), KEEP_ROWS, "the journal is cut");
    assert!(body.ends_with(&cut), "the newest rows kept");

    let ledger = path_under(&dir, Some("@s-9")).expect("the drive dir");
    let land = Row {
        rule_id: RULE_LAND,
        outcome: Outcome::Typed,
        command: "(land)",
        reason: &act_reason(None),
        box_seq: 1,
    }
    .to_json(1_000, Some("@s-9"));
    let mut body = format!("{land}\n");
    let mut ts = 2_000;
    while body.len() <= past {
        body.push_str(
            &Row {
                rule_id: "allow-once@v1",
                outcome: Outcome::Approved,
                command: "echo captured",
                reason: "an ordinary approval",
                box_seq: 1,
            }
            .to_json(ts, Some("@s-9")),
        );
        body.push('\n');
        ts += 1;
    }
    std::fs::write(&ledger, &body).expect("write");
    let standing = land_episode_in(&body, Some("@s-9"));
    assert!(standing.is_some(), "the episode stands");
    bound(&ledger);
    let cut = std::fs::read_to_string(&ledger).expect("read");
    assert_eq!(cut.lines().count(), KEEP_ROWS + 1, "the ledger is cut");
    assert!(cut.len() as u64 <= MAX_BYTES, "{} bytes", cut.len());
    assert_eq!(
        land_episode_in(&cut, Some("@s-9")),
        standing,
        "the standing episode survives the cut"
    );
}

#[test]
fn a_cut_refuses_oversized_retained_rows_but_still_folds_records() {
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("approvals.jsonl");
    let reason = "x".repeat(HELD_BYTES / KEEP_ROWS);
    for kind in ["plain", "interleaved-records", "folded-records"] {
        let record = kind != "plain";
        let mut body = "older ordinary row\n".to_string();
        let mut last = String::new();
        for i in 0..KEEP_ROWS {
            let sid = if kind == "interleaved-records" && i % 2 == 0 {
                "@s-10"
            } else {
                "@s-9"
            };
            last = Row {
                rule_id: if record { RULE_POINT } else { "allow-once@v1" },
                outcome: if record {
                    Outcome::Waiting
                } else {
                    Outcome::Approved
                },
                command: "",
                reason: &reason,
                box_seq: 1,
            }
            .to_json(i64::try_from(i).unwrap(), Some(sid));
            body.push_str(&last);
            body.push('\n');
        }
        assert!(body.len() > HELD_BYTES);
        std::fs::write(&path, &body).unwrap();
        let changed = crate::harness::cli::bound_ledger_cut(&path, MAX_BYTES, &|rows, read| {
            cut_rows(&path, rows, read)
        })
        .unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        if kind == "folded-records" {
            assert!(changed, "folded-away rows do not consume retained bytes");
            assert!(after.len() as u64 <= MAX_BYTES);
            assert!(after.ends_with(&(last + "\n")));
        } else {
            assert!(!changed, "{kind} must refuse the cut");
            assert_eq!(after, body, "{kind} must preserve the original bytes");
        }
    }
}
use super::*;

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[test]
fn a_row_carries_the_hash_of_the_whole_command_and_a_cut_copy() {
    let long = "x".repeat(COMMAND_BYTES + 10);
    let row = Row {
        rule_id: "rm-breaker@v1",
        outcome: Outcome::Approved,
        command: &long,
        reason: "every operand under a scratch root",
        box_seq: 42,
    };
    let json = row.to_json(1_000, Some("@s-1"));
    assert!(json.starts_with("{\"ts\":1000,\"sid\":\"s-1\",\"rule_id\":\"rm-breaker@v1\""));
    assert!(json.contains("\"decision\":\"approved\""));
    assert!(json.contains(&format!("\"command\":\"{}\"", "x".repeat(COMMAND_BYTES))));
    assert!(!json.contains(&"x".repeat(COMMAND_BYTES + 1)));
    assert!(json.contains(&format!("\"cmd_sha256\":\"{}\"", sha256_hex(&long))));
    assert!(json.ends_with("\"box_seq\":42}"));
}

#[test]
fn typed_at_reads_the_sessions_typed_rows_only() {
    let dir = crate::supervise::ScratchDir::new("apr", "typed-at");
    let path = dir.join("s-1.jsonl");
    let row = |outcome, command: &'static str| Row {
        rule_id: "continue@v1",
        outcome,
        command,
        reason: "-",
        box_seq: 1,
    };
    let forged = ",\"sid\":\"s-1\",\"decision\":\"typed\",";
    let login = |rule_id, command, ts| {
        Row {
            rule_id,
            outcome: Outcome::Typed,
            command,
            reason: "-",
            box_seq: 1,
        }
        .to_json(ts, Some("@s-1"))
    };
    let body = [
        row(Outcome::Typed, "keep going").to_json(3_000, Some("@s-1")),
        row(Outcome::Typed, "keep going").to_json(1_000, Some("@s-1")),
        row(Outcome::Escalated, forged).to_json(2_000, Some("@s-1")),
        row(Outcome::Typed, "keep going").to_json(4_000, Some("@s-2")),
        row(Outcome::Approved, "ls").to_json(5_000, Some("@s-1")),
        login(super::super::policy::turn_end::RULE_LOGIN, "/login", 6_000),
        login(super::super::policy::login::RULE_LOGIN_DONE, "enter", 7_000),
    ]
    .join("\n");
    std::fs::write(&path, body).expect("ledger");
    assert_eq!(typed_at(&path, Some("@s-1")), [1_000, 3_000, 6_000]);
    assert_eq!(typed_at(&path, Some("s-2")), [4_000]);
    assert!(typed_at(&dir.join("none.jsonl"), Some("@s-1")).is_empty());
}

#[test]
fn the_login_a_loop_before_typed_is_read_while_it_is_the_last_act() {
    use super::super::policy::login::RULE_LOGIN_DONE;
    use super::super::policy::turn_end::RULE_LOGIN;
    let typed = |rule_id, command, ts, sid| {
        Row {
            rule_id,
            outcome: Outcome::Typed,
            command,
            reason: "-",
            box_seq: 1,
        }
        .to_json(ts, Some(sid))
    };
    let waiting = |rule_id, ts| {
        Row {
            rule_id,
            outcome: Outcome::Waiting,
            command: "",
            reason: "-",
            box_seq: 1,
        }
        .to_json(ts, Some("@s-1"))
    };
    let login = typed(RULE_LOGIN, "/login", 6_000, "@s-1");
    let body = |after: &[String]| {
        let mut rows = vec![
            typed("continue@v1", "keep going", 1_000, "@s-1"),
            waiting(RULE_POINT, 5_000),
            login.clone(),
            waiting(RULE_POINT, 7_000),
        ];
        rows.extend(after.iter().cloned());
        rows.join("\n")
    };
    assert_eq!(login_typed_last_in(&body(&[]), Some("@s-1")), Some(6_000));
    assert_eq!(
        login_typed_last_in(
            &body(&[typed(RULE_LOGIN, "/login", 9_000, "@s-2")]),
            Some("s-1")
        ),
        Some(6_000),
        "another session's /login is no act of this one's"
    );
    for after in [
        typed(RULE_LOGIN_DONE, "enter", 8_000, "@s-1"),
        typed("continue@v1", "keep going", 8_000, "@s-1"),
        waiting(RULE_TURN_RAN, 8_000),
    ] {
        assert_eq!(
            login_typed_last_in(&body(std::slice::from_ref(&after)), Some("@s-1")),
            None,
            "{after}"
        );
    }
    assert_eq!(login_typed_last_in(&body(&[]), Some("@s-2")), None);
    assert_eq!(login_typed_last_in("", Some("@s-1")), None);
}

#[test]
fn a_left_at_wall_key_carries_the_exchanges_work() {
    use std::time::Duration;
    let key = left_at_wall_key("prompt:u-1", Duration::from_millis(361_900));
    assert_eq!(key, "wall:prompt:u-1@361");
    assert_eq!(
        parse_left_at_wall(&key),
        Some(("prompt:u-1", Some(Duration::from_secs(361))))
    );
    assert_eq!(
        parse_left_at_wall("wall:said:00ff00ff00ff00ff"),
        Some(("said:00ff00ff00ff00ff", None))
    );
    assert_eq!(parse_left_at_wall("answer:a-1@361"), None);
    assert_eq!(
        parse_left_at_wall("wall:answer:a@b"),
        Some(("answer:a@b", None))
    );
}

/// A person's exchange left after the session worked (an act typed, or a point recorded at
/// an earlier screen, before the LEFT row) reads as such; the opening message's record, with
/// only its own point (the same screen) before it, does not, nor a fact line before it.
/// NEGATIVE CONTROL: a reader that takes the record's own point for earlier work reads every
/// opening message's record as after work, and Land carries the person's first message.
#[test]
fn a_person_left_after_work_is_told_from_the_opening_message() {
    use super::super::policy::login::RULE_LOGIN_DONE;
    use super::super::policy::turn_end::{RULE_ACCOUNT, RULE_LEFT, RULE_USAGE_PROBE};
    let row = |rule, outcome, box_seq| Row {
        rule_id: rule,
        outcome,
        command: "prompt:u-1",
        reason: LEFT_REASON,
        box_seq,
    };
    let at = |rule, outcome, seq, ts| row(rule, outcome, seq).to_json(ts, Some("@s-1"));
    let body = |rows: &[String]| rows.join("\n");
    let opening = body(&[
        at(RULE_POINT, Outcome::Waiting, 9, 1_000),
        at(RULE_LEFT, Outcome::Skipped, 9, 1_001),
    ]);
    assert!(
        !left_after_work_in(&opening, Some("@s-1")),
        "the opening message"
    );
    let after_a_point = body(&[
        at(RULE_POINT, Outcome::Waiting, 5, 1_000),
        at(RULE_POINT, Outcome::Waiting, 9, 2_000),
        at(RULE_LEFT, Outcome::Skipped, 9, 2_001),
    ]);
    assert!(left_after_work_in(&after_a_point, Some("@s-1")));
    assert!(
        !left_after_work_in(&after_a_point, Some("@s-2")),
        "another session's"
    );
    let after_an_act = body(&[
        at("continue@v1", Outcome::Typed, 3, 500),
        at(RULE_POINT, Outcome::Waiting, 9, 1_000),
        at(RULE_LEFT, Outcome::Skipped, 9, 1_001),
    ]);
    assert!(left_after_work_in(&after_an_act, Some("@s-1")));
    assert!(!left_after_work_in(
        &body(&[at(RULE_POINT, Outcome::Waiting, 5, 1_000)]),
        Some("@s-1")
    ));
    // A record this build wrote says whether it opened the session: a program started fresh
    // in a tab another worked keeps its opening message the person's, whatever came before.
    let opens = left_reason(LEFT_REASON, true);
    let fresh_after_another = body(&[
        at("continue@v1", Outcome::Typed, 3, 500),
        at(RULE_POINT, Outcome::Waiting, 5, 1_000),
        Row {
            reason: &opens,
            ..row(RULE_LEFT, Outcome::Skipped, 9)
        }
        .to_json(2_001, Some("@s-1")),
    ]);
    assert!(
        !left_after_work_in(&fresh_after_another, Some("@s-1")),
        "an opening message's record"
    );
    assert_eq!(left_reason(LEFT_REASON, false), LEFT_REASON);
    // What a loop types that is no work (as `typed_at_in` reads it) makes no record after
    // work: an accounts probe, an account move, a sign-in's done.
    let move_row = Row {
        command: "relaunch under account b",
        ..row(RULE_ACCOUNT, Outcome::Typed, 3)
    };
    for not_work in [
        at(RULE_USAGE_PROBE, Outcome::Typed, 3, 500),
        at(RULE_LOGIN_DONE, Outcome::Typed, 3, 500),
        move_row.to_json(500, Some("@s-1")),
    ] {
        let left = body(&[
            not_work,
            at(RULE_POINT, Outcome::Waiting, 9, 1_000),
            at(RULE_LEFT, Outcome::Skipped, 9, 1_001),
        ]);
        assert!(!left_after_work_in(&left, Some("@s-1")), "{left}");
    }
}

#[test]
fn open_left_is_the_last_left_row_no_typed_row_closed() {
    use super::super::policy::turn_end::{RULE_LEFT, RULE_USAGE_PROBE};
    let dir = crate::supervise::ScratchDir::new("apr", "open-left");
    let path = dir.join("s-1.jsonl");
    let row = |rule, outcome, command: &'static str| Row {
        rule_id: rule,
        outcome,
        command,
        reason: LEFT_REASON,
        box_seq: 1,
    };
    let write = |rows: &[String]| std::fs::write(&path, rows.join("\n")).expect("ledger");
    let left = |key, ts| row(RULE_LEFT, Outcome::Skipped, key).to_json(ts, Some("@s-1"));
    let typed = |rule, text, ts| row(rule, Outcome::Typed, text).to_json(ts, Some("@s-1"));
    write(&[
        left("answer:a", 1_000),
        typed("continue@v1", "keep going", 2_000),
        left("answer:b", 3_000),
        typed(RULE_USAGE_PROBE, "/usage", 4_000),
    ]);
    assert_eq!(open_left(&path, Some("@s-1")).as_deref(), Some("answer:b"));
    assert_eq!(open_left(&path, Some("s-1")).as_deref(), Some("answer:b"));
    write(&[
        left("answer:a", 1_000),
        typed("continue@v1", "keep going", 2_000),
    ]);
    assert_eq!(
        open_left(&path, Some("@s-1")),
        None,
        "a typed row closed it"
    );
    write(&[left("answer:a", 1_000)]);
    assert_eq!(open_left(&path, Some("@s-2")), None, "another session's");
    write(&[row("continue@v1", Outcome::Skipped, "answer:a").to_json(1_000, Some("@s-1"))]);
    assert_eq!(open_left(&path, Some("@s-1")), None, "another rule's");
    assert_eq!(open_left(&dir.join("none.jsonl"), Some("@s-1")), None);
}

#[test]
fn the_carry_ends_at_the_tasks_end_an_exchange_left_or_off_after_the_answer() {
    use super::super::policy::turn_end::{RULE_LEFT, RULE_USAGE_PROBE};
    let row = |rule, outcome, ts| {
        Row {
            rule_id: rule,
            outcome,
            command: "",
            reason: "",
            box_seq: 1,
        }
        .to_json(ts, Some("@s-1"))
    };
    let act = || row("continue@v1", Outcome::Typed, 1_000);
    let point = || row(RULE_POINT, Outcome::Waiting, 2_000);
    let ended = |rows: &[String]| carry_ended_in(&(rows.join("\n") + "\n"), Some("@s-1"));
    let done = row(RULE_TASK_DONE, Outcome::Waiting, 2_000);
    let left = row(RULE_LEFT, Outcome::Skipped, 2_000);
    let off = row(RULE_OFF_STOOD, Outcome::Waiting, 3_000);
    let esc_reason = point_reason(Carried::streak(1), true);
    let esc = Row {
        rule_id: RULE_POINT,
        outcome: Outcome::Waiting,
        command: "",
        reason: &esc_reason,
        box_seq: 1,
    }
    .to_json(2_000, Some("@s-1"));
    assert!(ended(&[act(), esc.clone()]), "a person's Esc on the answer");
    assert!(ended(&[act(), point(), done.clone()]), "the task's end");
    assert!(ended(&[act(), point(), left]), "an exchange left");
    assert!(
        ended(&[act(), point(), off.clone()]),
        "Off after the answer"
    );
    assert!(
        ended(&[
            act(),
            point(),
            done.clone(),
            row(RULE_USAGE_PROBE, Outcome::Typed, 4_000)
        ]),
        "the probe is no act"
    );
    assert!(!ended(&[act(), off]), "Off before the answer");
    assert!(
        !ended(&[act(), point(), esc.clone()]),
        "an Esc at a later point"
    );
    assert!(!ended(&[esc, act()]), "an Esc before the act");
    assert!(
        !ended(&[act(), point(), done.clone(), act()]),
        "an act since"
    );
    assert!(!ended(&[act(), point()]), "nothing ended it");
    assert!(!ended(&[]), "no act");
    assert!(
        !carry_ended_in(&(done + "\n"), Some("@s-2")),
        "another session's row"
    );
}

#[test]
fn a_walls_retry_keeps_a_persons_exchange_at_the_wall_open() {
    use super::super::policy::turn_end::{
        RULE_API_BACK, RULE_API_RETRY, RULE_COMPACT, RULE_LEFT, RULE_LIMIT_RESUME,
    };
    let row = |rule, outcome, command: &'static str, ts| {
        Row {
            rule_id: rule,
            outcome,
            command,
            reason: LEFT_AT_WALL_REASON,
            box_seq: 1,
        }
        .to_json(ts, Some("@s-1"))
    };
    let wall = "wall:prompt:u-1@120";
    let retry = "Claude Code reported an overloaded API (529).  Carry on.";
    let body = |rows: &[String]| rows.join("\n") + "\n";
    let left = row(RULE_LEFT, Outcome::Skipped, wall, 1_000);
    for rule in [RULE_API_RETRY, RULE_API_BACK, RULE_LIMIT_RESUME] {
        let got = open_left_in(
            &body(&[left.clone(), row(rule, Outcome::Typed, retry, 2_000)]),
            Some("@s-1"),
        );
        assert_eq!(got, Some(left_retried_key(wall, retry)), "{rule}");
        let key = got.expect("open");
        assert_eq!(
            parse_left_retried(&key),
            Some((
                words_hash("Claude Code reported an overloaded API (529). Carry on."),
                wall
            )),
            "{rule}"
        );
    }
    let then = |rows: &[String]| open_left_in(&body(rows), Some("@s-1"));
    assert_eq!(
        then(&[
            left.clone(),
            row(RULE_API_RETRY, Outcome::Typed, retry, 2_000),
            row("continue@v1", Outcome::Typed, "keep going", 3_000),
        ]),
        None,
        "a continuation after the retry"
    );
    assert_eq!(
        then(&[
            left.clone(),
            row(RULE_COMPACT, Outcome::Typed, "/compact", 2_000)
        ]),
        None,
        "a command"
    );
    assert_eq!(
        then(&[
            row(RULE_LEFT, Outcome::Skipped, "prompt:u-1", 1_000),
            row(RULE_API_RETRY, Outcome::Typed, retry, 2_000),
        ]),
        None,
        "a retry after a row no wall's"
    );
    assert_eq!(parse_left_retried(wall), None);
}

#[test]
fn the_probes_rows_are_no_typed_act_and_say_whether_a_panel_is_owed() {
    use super::super::policy::turn_end::{RULE_COMPACT, RULE_USAGE_PROBE};
    let dir = crate::supervise::ScratchDir::new("apr", "probe-rows");
    let path = dir.join("s-1.jsonl");
    let row = |rule_id, outcome, command: &'static str, reason: &'static str| Row {
        rule_id,
        outcome,
        command,
        reason,
        box_seq: 1,
    };
    let typed = || {
        row(
            RULE_USAGE_PROBE,
            Outcome::Typed,
            "/usage",
            PROBE_TYPED_REASON,
        )
    };
    let closed = || {
        row(
            RULE_USAGE_PROBE,
            Outcome::Waiting,
            "/usage",
            PROBE_CLOSED_REASON,
        )
    };
    let write = |rows: &[String]| std::fs::write(&path, rows.join("\n")).expect("ledger");

    write(&[
        row(RULE_COMPACT, Outcome::Typed, "/compact", "-").to_json(1_000, Some("@s-1")),
        typed().to_json(2_000, Some("@s-1")),
        closed().to_json(2_500, Some("@s-1")),
        row("continue@v1", Outcome::Typed, "/usage", "-").to_json(3_000, Some("@s-1")),
    ]);
    assert_eq!(typed_at(&path, Some("@s-1")), [1_000, 3_000]);
    let texts: Vec<(i64, String)> = typed_texts(&path, Some("@s-1"))
        .into_iter()
        .map(|t| (t.at_ms, t.text))
        .collect();
    assert_eq!(
        texts,
        [
            (1_000, "/compact".to_string()),
            (2_000, "/usage".to_string()),
            (3_000, "/usage".to_string()),
        ]
    );
    assert!(
        !probe_owed(&read(&path), Some("@s-1")),
        "the panel was seen gone, and the continue rule's row is no probe row"
    );

    write(&[
        typed().to_json(1_000, Some("@s-1")),
        closed().to_json(1_500, Some("@s-1")),
        typed().to_json(2_000, Some("@s-1")),
        row("continue@v1", Outcome::Typed, "keep going", "-").to_json(3_000, Some("@s-1")),
    ]);
    assert!(probe_owed(&read(&path), Some("@s-1")));
    assert!(
        probe_owed(&read(&path), Some("s-1")),
        "with or without the `@`"
    );
    assert!(
        !probe_owed(&read(&path), Some("@s-2")),
        "another session owes nothing"
    );

    let forged = ",\"rule_id\":\"usage-probe@v1\",\"decision\":\"typed\",";
    write(&[
        row("continue@v1", Outcome::Typed, "keep going", "-").to_json(1_000, Some("@s-1")),
        row("continue@v1", Outcome::Typed, forged, "-").to_json(2_000, Some("@s-1")),
    ]);
    assert!(!probe_owed(&read(&path), Some("@s-1")));
    assert_eq!(typed_at(&path, Some("@s-1")), [1_000, 2_000]);
    assert!(!probe_owed(&read(&dir.join("none.jsonl")), Some("@s-1")));
}

#[test]
fn a_reset_row_taken_and_not_ended_is_owed_and_says_whether_it_was_entered() {
    use super::super::policy::approval::{RULE_LIMIT_RESET, RULE_LIMIT_WAIT};
    let dir = crate::supervise::ScratchDir::new("apr", "reset-rows");
    let path = dir.join("s-1.jsonl");
    let row = |rule_id, outcome, command: &'static str, reason: &'static str| {
        Row {
            rule_id,
            outcome,
            command,
            reason,
            box_seq: 1,
        }
        .to_json(1_000, Some("@s-1"))
    };
    const GRANT: &str = "Use a weekly reset (2 left, until Oct 5)";
    const YES: &str = "Yes, use my weekly reset";
    let taken = || {
        row(
            RULE_LIMIT_RESET,
            Outcome::Approved,
            GRANT,
            "taken: wall until 9",
        )
    };
    let entered = || row(RULE_LIMIT_RESET, Outcome::Approved, YES, "Refills your …");
    let ended = || {
        row(
            RULE_LIMIT_RESET,
            Outcome::Waiting,
            "limit reset",
            "ended: kept",
        )
    };
    let write = |rows: &[String]| std::fs::write(&path, rows.join("\n")).expect("ledger");
    let owed = || reset_owed(&read(&path), Some("@s-1")).map(|open| open.entered);

    write(&[
        taken(),
        row("continue@v1", Outcome::Typed, "keep going", "-"),
    ]);
    assert_eq!(owed(), Some(false));
    assert_eq!(
        reset_owed(&read(&path), Some("s-1")).map(|open| open.entered),
        Some(false),
        "with or without the `@`"
    );
    assert_eq!(
        reset_owed(&read(&path), Some("@s-2")),
        None,
        "another session's"
    );
    assert_eq!(
        typed_at(&path, Some("@s-1")).len(),
        1,
        "no reset row is typed"
    );
    write(&[
        taken(),
        row(
            RULE_LIMIT_RESET,
            Outcome::Declined,
            "Use a weekly reset?",
            "focus",
        ),
    ]);
    assert_eq!(owed(), Some(false));
    for yes in [
        "Yes, use my 5-hour reset",
        YES,
        "Yes, use my full reset",
        "Yes, use my Fable reset",
        RESET_YES_2_1_287,
    ] {
        write(&[
            taken(),
            row(RULE_LIMIT_RESET, Outcome::Approved, yes, "Refills your …"),
        ]);
        assert_eq!(owed(), Some(true), "{yes}");
    }
    for yes in [
        "Yes, use my browser",
        "Yes, use my",
        "Yes, use my weekly resets",
        "Yes, use my resets",
    ] {
        write(&[
            taken(),
            row(RULE_LIMIT_RESET, Outcome::Approved, yes, "Refills your …"),
        ]);
        assert_eq!(owed(), Some(false), "CONTROL: {yes}");
    }
    write(&[taken(), entered().replace("\"ts\":1000,", "\"ts\":9000,")]);
    assert_eq!(
        reset_owed(&read(&path), Some("@s-1")),
        Some(ResetOpen {
            entered: true,
            taken_ms: 1_000
        })
    );
    write(&[taken(), entered(), ended()]);
    assert_eq!(owed(), None);
    write(&[taken(), entered(), ended(), taken()]);
    assert_eq!(owed(), Some(false));

    let forged = ",\"rule_id\":\"limit-reset@v1\",\"decision\":\"approved\",";
    write(&[
        row(
            RULE_LIMIT_RESET,
            Outcome::Skipped,
            GRANT,
            "the screen moved",
        ),
        row(RULE_LIMIT_RESET, Outcome::Deferred, GRANT, "ERR rate"),
        row(
            RULE_LIMIT_RESET,
            Outcome::Declined,
            "Use a weekly reset?",
            "focus",
        ),
        row(RULE_LIMIT_WAIT, Outcome::Approved, GRANT, "-"),
        row("no-spend@v1", Outcome::Approved, YES, "-"),
        row("continue@v1", Outcome::Typed, forged, "-"),
    ]);
    assert_eq!(owed(), None);
    assert_eq!(
        reset_owed(&read(&dir.join("none.jsonl")), Some("@s-1")),
        None
    );
}

#[test]
fn typed_texts_are_what_the_sessions_loop_typed() {
    let scratch = crate::supervise::ScratchDir::new("apr", "typed-texts");
    let dir = scratch.join("state");
    let path = ledger_under(&dir, Some("@s-1"));
    assert!(!dir.exists(), "a reader's path makes nothing");
    assert_eq!(path_under(&dir, Some("@s-1")), Some(path.clone()));
    let row = |rule_id, outcome, command: &'static str| Row {
        rule_id,
        outcome,
        command,
        reason: "the turn-end policy",
        box_seq: 1,
    };
    let forged = ",\"sid\":\"s-1\",\"decision\":\"typed\",";
    let body = [
        row("continue@v1", Outcome::Typed, "keep going").to_json(1_000, Some("@s-1")),
        row("answer@v1", Outcome::Typed, "Decide for yourself.").to_json(2_000, Some("@s-1")),
        row("continue@v1", Outcome::Typed, "keep going").to_json(3_000, Some("@s-1")),
        row("context-compact@v1", Outcome::Typed, forged).to_json(4_000, Some("@s-1")),
        row("continue@v1", Outcome::Typed, "carry on").to_json(5_000, Some("@s-2")),
        row("safe-read@v1", Outcome::Approved, "ls").to_json(6_000, Some("@s-1")),
    ]
    .join("\n");
    std::fs::write(&path, body).expect("ledger");
    let typed = |sid| {
        typed_texts(&path, Some(sid))
            .into_iter()
            .map(|t| (t.at_ms, t.text))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        typed("@s-1"),
        [
            (1_000, "keep going".to_string()),
            (2_000, "Decide for yourself.".to_string()),
            (3_000, "keep going".to_string()),
            (4_000, forged.to_string()),
        ]
    );
    assert_eq!(typed("s-2"), [(5_000, "carry on".to_string())]);
    assert!(typed_texts(&dir.join("drive/none.jsonl"), Some("@s-1")).is_empty());
}

#[test]
fn the_ledger_lives_under_the_state_root_by_sid_and_appends() {
    let root = crate::supervise::ScratchDir::new("apr", "approvals");
    let path = path_under(&root, Some("@s-ab/../x")).expect("path");
    assert_eq!(path, root.join("drive").join("s-ab____x.jsonl"));
    assert_eq!(
        path_under(&root, None).expect("path"),
        root.join("drive").join("self.jsonl")
    );
    let mut warn = Vec::new();
    let mut l = Ledger::open(Some(&path), Some("@s-ab"), &mut warn);
    for seq in [1, 2] {
        l.write(
            crate::supervise::journal::unix_ms(),
            &Row {
                rule_id: "-",
                outcome: Outcome::Escalated,
                command: "rm -rf /",
                reason: "not read-only",
                box_seq: seq,
            },
            &mut warn,
        );
    }
    let text = std::fs::read_to_string(&path).expect("ledger");
    assert_eq!(text.lines().count(), 2, "{text}");
    assert!(
        text.lines()
            .all(|l| l.contains("\"decision\":\"escalated\""))
    );
    assert!(warn.is_empty());
}

/// A SELF-CLAIM'S ROW IS NONE OF THE LOOP'S ACTS: appended whole through the ledger's one
/// append in the row's shape, read back as the holder's act, and, for a rule each reader of
/// aterm's own acts reacts to (its typed times and sleep count, its streak, its done check,
/// its land episode, its person's exchange), read by none of them. NEGATIVE CONTROL: the
/// loop's own row of each of those rules changes its reader's answer, and is no self-claim's.
#[test]
fn a_self_claims_row_is_appended_whole_and_read_as_none_of_the_loops_acts() {
    use super::super::policy::turn_end::{RULE_CONTINUE, RULE_DONE_CHECK, RULE_LAND, RULE_LEFT};
    let root = crate::supervise::ScratchDir::new("apr", "self-claim");
    let sid = Some("@s-sc");
    let path = path_under(&root, sid).expect("path");
    let mut warn = Vec::new();
    let mut ledger = Ledger::open(Some(&path), sid, &mut warn);
    let row = SelfClaimRow {
        holder: "codex-alab@4242",
        act: "continue@v1",
    };
    let line = ledger
        .write_self_claim(1_000, &row, &mut warn)
        .expect("written");
    assert!(warn.is_empty(), "{warn:?}");
    let body = std::fs::read_to_string(&path).expect("ledger");
    assert_eq!(body, format!("{line}\n"), "one whole line");
    let v: aterm_json::Value = aterm_json::from_str(&line).expect("one JSON object");
    assert_eq!(SelfClaimRow::of(&v), Some(row));
    let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
    assert_eq!(v.get("ts").and_then(aterm_json::Value::as_i64), Some(1_000));
    assert_eq!(text("sid"), Some("s-sc"));
    assert_eq!(text("rule_id"), Some(RULE_SELF_CLAIM_ACT));
    assert_eq!(text(SELF_CLAIM_WORD), Some("continue"));
    assert_eq!(text("decision").and_then(Outcome::from_word), None);
    assert!(!is_fact_line(&line));
    for act in [RULE_DONE_CHECK, RULE_LAND, RULE_LEFT] {
        let row = SelfClaimRow {
            holder: "codex-alab@4242",
            act,
        };
        ledger
            .write_self_claim(1_001, &row, &mut warn)
            .expect("written");
    }
    let body = std::fs::read_to_string(&path).expect("ledger");
    assert_eq!(body.lines().count(), 4, "{body}");
    assert!(typed_at_in(&body, sid).is_empty());
    assert_eq!(last_act_count_in(&body, sid), None);
    assert_eq!(done_check_owed_in(&body, sid), done_check_owed_in("", sid));
    assert_eq!(land_episode_in(&body, sid), land_episode_in("", sid));
    assert_eq!(short_streak_in(&body, sid), short_streak_in("", sid));
    assert_eq!(open_left_in(&body, sid), open_left_in("", sid));
    let own = |rule_id, outcome, command| {
        Row {
            rule_id,
            outcome,
            command,
            reason: "",
            box_seq: 0,
        }
        .to_json(1_000, sid)
    };
    let typed = own(RULE_CONTINUE, Outcome::Typed, "keep going");
    assert_eq!(typed_at_in(&typed, sid), [1_000], "NEGATIVE CONTROL");
    assert_ne!(short_streak_in(&typed, sid), short_streak_in("", sid));
    let check = own(RULE_DONE_CHECK, Outcome::Typed, "check your work");
    assert_ne!(done_check_owed_in(&check, sid), done_check_owed_in("", sid));
    let land = own(RULE_LAND, Outcome::Typed, "land it");
    assert_ne!(land_episode_in(&land, sid), land_episode_in("", sid));
    let left = own(RULE_LEFT, Outcome::Skipped, "their own exchange");
    assert_ne!(open_left_in(&left, sid), open_left_in("", sid));
    for own in [typed, check, land, left] {
        let v: aterm_json::Value = aterm_json::from_str(&own).expect("one JSON object");
        assert_eq!(SelfClaimRow::of(&v), None, "{own}");
    }
}

/// A SELF-CLAIM'S ROW BOOKED WHILE A LOOP CUTS THE LEDGER LANDS IN THE FILE THE CUT LEAVES:
/// the cut ([`bound`]) holds the ledger's lock from its first read to its rename, and
/// [`book_self_claim`] opens and appends under it, so a booking begun mid-cut waits the cut
/// out and its row is the cut file's newest, and the lock is free once the cut is over.
/// NEGATIVE CONTROL: a raw descriptor that bypasses the journal's reopen-and-lock protocol
/// still writes to the old inode after a cut's rename, outside the named ledger.
#[test]
fn a_self_claims_row_booked_during_a_cut_lands_in_the_file_the_cut_leaves() {
    let dir = crate::supervise::ScratchDir::new("apr-cut", "self-claim");
    let sid = Some("@s-cut");
    let path = path_under(&dir, sid).expect("the drive dir");
    let pad = format!("{{\"ts\":1,\"pad\":\"{}\"}}", "x".repeat(300));
    let past: String = (0..usize::try_from(MAX_BYTES).unwrap() / pad.len() + 50)
        .map(|k| format!("{pad}{k}\n"))
        .collect();
    let row = SelfClaimRow {
        holder: "codex-alab@4242",
        act: "continue@v1",
    };

    std::fs::write(&path, &past).expect("seed");
    let mut stale = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("raw descriptor outside the journal protocol");
    bound(&path);
    let lost = row.to_json(2_000, sid);
    writeln!(stale, "{lost}").expect("write old inode");
    stale.flush().expect("flush old inode");
    let cut = std::fs::read_to_string(&path).expect("read");
    assert_eq!(cut.lines().count(), KEEP_ROWS, "the ledger is cut");
    assert!(!cut.contains(&lost), "NEGATIVE CONTROL: {lost}");
    drop(stale);

    std::fs::write(&path, &past).expect("seed");
    let (at, row) = (path.as_path(), &row);
    let booked = std::thread::scope(|scope| {
        let booker = std::cell::RefCell::new(None);
        bound_with(at, &|rows, read| {
            assert!(
                ledger_stripe(at).try_lock().is_err(),
                "the cut holds the ledger's lock"
            );
            booker.replace(Some(
                scope.spawn(move || book_self_claim(at, sid, 2_001, row, &mut Vec::new())),
            ));
            cut_rows(at, rows, read)
        });
        let booker = booker.into_inner().expect("the cut read the ledger");
        booker.join().expect("the booking").expect("booked")
    });
    let cut = std::fs::read_to_string(&path).expect("read");
    assert_eq!(cut.lines().count(), KEEP_ROWS + 1, "cut, then booked");
    assert!(cut.ends_with(&format!("{booked}\n")), "{booked}");
    // Another test's ledger can fall in this one's stripe and hold it a moment, so free is a
    // take of it that ends (within a minute, as a hang detector), not one untaken right now.
    let (freed, free) = std::sync::mpsc::channel();
    let stripe = ledger_stripe(&path);
    std::thread::spawn(move || {
        drop(stripe.lock());
        let _ = freed.send(());
    });
    assert!(
        free.recv_timeout(std::time::Duration::from_secs(60))
            .is_ok(),
        "free once cut"
    );
}

fn seed_ledger(dir: &Path, name: &str, rows: &[(i64, &str, Outcome, &str, String)]) -> PathBuf {
    let path = dir.join(format!("{name}.jsonl"));
    let body: String = rows
        .iter()
        .map(|(ts, rule_id, outcome, command, reason)| {
            let row = Row {
                rule_id,
                outcome: *outcome,
                command,
                reason,
                box_seq: 1,
            };
            format!("{}\n", row.to_json(*ts, Some("@s-9")))
        })
        .collect();
    std::fs::write(&path, body).expect("write");
    path
}

fn spend_wall_reason() -> String {
    wall_wait_reason(
        None,
        None,
        aterm_phase::WallKind::Spend,
        "You've hit your monthly spend limit.",
    )
}

#[test]
fn a_record_between_a_try_and_its_first_read_is_no_read() {
    use super::super::policy::turn_end::RULE_LIMIT_RESUME;
    let dir = crate::supervise::ScratchDir::new("apr-seed", "first-read");
    let limit = LimitTry {
        bucket: false,
        in_place: false,
        attempts: 1,
        entitlement: true,
    };
    let records: [(&str, &str, &str); 4] = [
        ("none", "", ""),
        ("off", RULE_OFF_STOOD, OFF_STOOD),
        ("land-over", RULE_LAND_OVER, LAND_OVER),
        ("task-done", RULE_TASK_DONE, TASK_DONE),
    ];
    for (tag, rule, reason) in records {
        let mut rows = vec![
            (
                1_000,
                RULE_WALL_WAIT,
                Outcome::Waiting,
                "",
                spend_wall_reason(),
            ),
            (
                10_000,
                RULE_LIMIT_RESUME,
                Outcome::Typed,
                "keep going",
                act_reason(Some(limit)),
            ),
        ];
        if !rule.is_empty() {
            rows.push((12_000, rule, Outcome::Waiting, "", reason.to_string()));
        }
        rows.push((
            30_000,
            RULE_WALL_WAIT,
            Outcome::Waiting,
            "",
            spend_wall_reason(),
        ));
        let path = seed_ledger(&dir, &format!("first-read-{tag}"), &rows);
        let a = awaited_try(&path, Some("@s-9")).expect("the try");
        assert_eq!(
            (a.read_ms, a.unread, a.met),
            (Some(30_000), false, true),
            "{tag}"
        );
        let u = usage_attempt(&path, Some("@s-9")).expect("the try's episode");
        assert_eq!((u.read_ms, u.met_ms), (Some(30_000), Some(30_000)), "{tag}");
        rows.pop();
        let path = seed_ledger(&dir, &format!("first-read-{tag}-unread"), &rows);
        let a = awaited_try(&path, Some("@s-9")).expect("the try");
        assert!(a.unread && a.read_ms.is_none(), "{tag}: {a:?}");
    }
}

#[test]
fn a_try_aged_out_for_a_successor_ends_no_wall() {
    use super::super::policy::turn_end::{AccountAt, RULE_ACCOUNT, RULE_DONE_CHECK, RULE_LAND};
    let dir = crate::supervise::ScratchDir::new("apr-seed", "try-over");
    let limit = LimitTry {
        bucket: false,
        in_place: true,
        attempts: 1,
        entitlement: true,
    };
    for (act_rule, check) in [(RULE_DONE_CHECK, true), (RULE_LAND, false)] {
        for (tag, over_rule, over_reason, ended) in [
            ("try-over", RULE_TRY_OVER, TRY_OVER, false),
            (
                "limit-ended (control)",
                RULE_WALL_WAIT_END,
                LIMIT_ENDED,
                true,
            ),
        ] {
            let rows = vec![
                (
                    1_000,
                    RULE_WALL_WAIT,
                    Outcome::Waiting,
                    "",
                    spend_wall_reason(),
                ),
                (
                    10_000,
                    act_rule,
                    Outcome::Typed,
                    "(the act)",
                    act_reason(Some(limit)),
                ),
                (
                    10_002,
                    RULE_WALL_WAIT,
                    Outcome::Waiting,
                    "",
                    spend_wall_reason(),
                ),
                (
                    10_500,
                    RULE_ACCOUNT,
                    Outcome::Skipped,
                    "relaunch under account w2",
                    account_unmade_reason("w2", AccountAt::Wall, Some("refused")),
                ),
                (
                    9_000_000,
                    over_rule,
                    Outcome::Waiting,
                    "",
                    over_reason.to_string(),
                ),
                (
                    9_000_000,
                    RULE_WALL_WAIT,
                    Outcome::Waiting,
                    "",
                    spend_wall_reason(),
                ),
            ];
            let path = seed_ledger(&dir, &format!("try-over-{act_rule}-{over_rule}"), &rows);
            let sid = Some("@s-9");
            assert_eq!(usage_attempt(&path, sid), None, "{act_rule} {tag}");
            assert!(open_wall(&path, sid).is_some(), "{act_rule} {tag}");
            assert_eq!(
                account_moves(&path, sid).unmade.is_some(),
                !ended,
                "{act_rule} {tag}"
            );
            if check {
                let owed = done_check_owed(&path, sid);
                assert_eq!(owed.is_some_and(|o| o.cut), !ended, "{tag}: {owed:?}");
            } else {
                let ep = land_episode(&path, sid).expect("the episode");
                assert_eq!(ep.cut, !ended, "{tag}: {ep:?}");
            }
        }
    }
}

#[test]
fn the_short_streak_is_the_last_act_s_or_point_s() {
    use super::super::policy::turn_end::{AccountAt, RULE_ACCOUNT, RULE_CONTINUE};
    let dir = crate::supervise::ScratchDir::new("apr-seed", "streak");
    let act = |ts: i64, n: u32| {
        (
            ts,
            RULE_CONTINUE,
            Outcome::Typed,
            "keep going",
            act_reason_after(Carried::streak(n), None),
        )
    };
    let point = |ts: i64, n: u32| {
        (
            ts,
            RULE_POINT,
            Outcome::Waiting,
            "",
            point_reason(Carried::streak(n), false),
        )
    };
    let relaunch = (
        9_000,
        RULE_ACCOUNT,
        Outcome::Typed,
        "relaunch under account w2",
        account_switch_reason("w2", AccountAt::Wall),
    );
    let sid = Some("@s-9");
    let path = seed_ledger(&dir, "streak-acts", &[act(1_000, 2), act(2_000, 3)]);
    let s = short_streak(&path, sid).expect("a streak");
    assert_eq!(
        (s.at_ms, s.short, s.point, s.rule.as_str(), s.recorded),
        (2_000, 3, false, RULE_CONTINUE, false)
    );
    let path = seed_ledger(
        &dir,
        "streak-point",
        &[act(1_000, 2), act(2_000, 3), point(3_000, 4)],
    );
    let s = short_streak(&path, sid).expect("a streak");
    assert_eq!((s.at_ms, s.short, s.point), (3_000, 4, true));
    let path = seed_ledger(
        &dir,
        "streak-relaunch",
        &[
            act(1_000, 2),
            act(2_000, 3),
            point(3_000, 4),
            relaunch.clone(),
        ],
    );
    let s = short_streak(&path, sid).expect("a streak");
    assert_eq!((s.at_ms, s.short, s.point), (3_000, 4, true));
    let path = seed_ledger(
        &dir,
        "streak-act-after",
        &[act(1_000, 2), point(3_000, 4), relaunch, act(10_000, 4)],
    );
    let s = short_streak(&path, sid).expect("a streak");
    assert_eq!(
        (s.at_ms, s.short, s.point, s.recorded),
        (10_000, 4, false, true)
    );
    let path = seed_ledger(&dir, "streak-ended", &[act(1_000, 2), point(3_000, 0)]);
    let s = short_streak(&path, sid).expect("a streak");
    assert_eq!((s.short, s.point), (0, true));
    let ran = |ts: i64| {
        (
            ts,
            RULE_TURN_RAN,
            Outcome::Waiting,
            "",
            TURN_RAN.to_string(),
        )
    };
    let path = seed_ledger(
        &dir,
        "streak-ran",
        &[act(1_000, 2), point(3_000, 3), ran(4_000)],
    );
    let s = short_streak(&path, sid).expect("a streak");
    assert_eq!((s.at_ms, s.short, s.point, s.ran), (3_000, 3, true, true));
    let path = seed_ledger(
        &dir,
        "streak-ran-point",
        &[point(3_000, 3), ran(4_000), point(5_000, 4)],
    );
    let s = short_streak(&path, sid).expect("a streak");
    assert_eq!((s.at_ms, s.short, s.point, s.ran), (5_000, 4, true, false));
    let path = seed_ledger(
        &dir,
        "streak-ran-act",
        &[point(3_000, 3), act(4_000, 3), ran(5_000)],
    );
    let s = short_streak(&path, sid).expect("a streak");
    assert_eq!((s.at_ms, s.point, s.ran), (4_000, false, false));
    let limit = LimitTry {
        bucket: true,
        in_place: true,
        attempts: 2,
        entitlement: false,
    };
    let older = format!(
        "the turn-end policy ({SHORT_STREAK}: 2) (the open-items challenge was typed this \
         job) (in the limit's place: wall=bucket attempts=2)"
    );
    assert_eq!(limit_try_of(&older), Some(limit), "{older}");
    let path = seed_ledger(
        &dir,
        "streak-challenged-act",
        &[
            act(1_000, 1),
            (2_000, RULE_CONTINUE, Outcome::Typed, "keep going", older),
        ],
    );
    let s = short_streak(&path, sid).expect("a streak");
    assert_eq!((s.at_ms, s.short), (2_000, 2));
    let esc_point = |ts: i64, esc: bool| {
        (
            ts,
            RULE_POINT,
            Outcome::Waiting,
            "",
            point_reason(Carried::streak(0), esc),
        )
    };
    let path = seed_ledger(&dir, "streak-esc", &[act(1_000, 1), esc_point(3_000, true)]);
    let s = short_streak(&path, sid).expect("a streak");
    assert_eq!((s.at_ms, s.point, s.esc), (3_000, true, true));
    let path = seed_ledger(&dir, "streak-no-esc", &[esc_point(3_000, false)]);
    assert!(!short_streak(&path, sid).expect("a streak").esc);
    let words = format!(
        "{} ({PERSONS_ESC})",
        act_reason_after(Carried::streak(1), None)
    );
    let path = seed_ledger(
        &dir,
        "streak-esc-act",
        &[(2_000, RULE_CONTINUE, Outcome::Typed, "keep going", words)],
    );
    let s = short_streak(&path, sid).expect("a streak");
    assert_eq!((s.point, s.esc), (false, false));
}

#[test]
fn the_wall_s_retries_read_back_as_written() {
    use super::super::policy::turn_end::RULE_API_RETRY;
    use aterm_phase::WallKind;
    let dir = crate::supervise::ScratchDir::new("apr-seed", "retries");
    let sid = Some("@s-9");
    let mut kinds = vec![WallKind::Overloaded];
    for (cause, _) in API_CAUSES {
        for code in [None, Some(529)] {
            for retryable in [false, true] {
                kinds.push(WallKind::ApiError {
                    code,
                    retryable,
                    cause,
                });
            }
        }
    }
    for (n, kind) in kinds.into_iter().enumerate() {
        let mark = RetryMark {
            kind,
            attempts: n,
            ups: n / 2,
            since_ms: 1_789_682_580_000 + i64::try_from(n).expect("small"),
            down: n % 2 == 1,
            witnessed: n % 3 == 0,
        };
        let carried = Carried {
            short: 2,
            retry: Some(mark),
            said: Replies::default(),
            said_reset_ms: None,
            checks: 0,
            land: LandBooks::default(),
            pass: WallPass::default(),
            needs_acked: 0,
        };
        let reason = act_reason_after(carried, None);
        let path = seed_ledger(
            &dir,
            &format!("retries-act-{n}"),
            &[(2_000, RULE_API_RETRY, Outcome::Typed, "try again", reason)],
        );
        let s = short_streak(&path, sid).expect("a streak");
        assert_eq!((s.short, s.retry), (2, Some(mark)), "{kind:?}");
        let path = seed_ledger(
            &dir,
            &format!("retries-point-{n}"),
            &[(
                3_000,
                RULE_POINT,
                Outcome::Waiting,
                "",
                point_reason(carried, true),
            )],
        );
        let s = short_streak(&path, sid).expect("a streak");
        assert_eq!(
            (s.point, s.esc, s.retry),
            (true, true, Some(mark)),
            "{kind:?}"
        );
    }
    let limit = LimitTry {
        bucket: false,
        in_place: false,
        attempts: 1,
        entitlement: true,
    };
    let both = act_reason_after(
        Carried {
            short: 0,
            retry: Some(RetryMark {
                kind: WallKind::Overloaded,
                attempts: 3,
                ups: 0,
                since_ms: 7,
                down: false,
                witnessed: false,
            }),
            said: Replies::default(),
            said_reset_ms: None,
            checks: 0,
            land: LandBooks::default(),
            pass: WallPass::default(),
            needs_acked: 0,
        },
        Some(limit),
    );
    assert_eq!(limit_try_of(&both), Some(limit), "{both}");
    assert!(retry_mark_of(&both).is_some(), "{both}");
    assert_eq!(
        retry_mark_of(&act_reason_after(Carried::streak(2), None)),
        None
    );
    let garbled = format!("the turn-end policy ({RETRY_LADDER}: kind=overloaded attempts=x)");
    assert_eq!(retry_mark_of(&garbled), None);
    // A wall read while the route was measured down says so; one read otherwise writes no
    // field, as older books do, and reads it false.
    let written = |down: bool| {
        act_reason_after(
            Carried {
                retry: Some(RetryMark {
                    kind: WallKind::ApiError {
                        code: None,
                        retryable: true,
                        cause: aterm_phase::ApiCause::CutOff,
                    },
                    attempts: 0,
                    ups: 0,
                    since_ms: 7,
                    down,
                    witnessed: false,
                }),
                ..Carried::streak(0)
            },
            None,
        )
    };
    assert!(written(true).contains(" down=yes)"), "{}", written(true));
    assert!(!written(false).contains("down="), "{}", written(false));
    assert_eq!(retry_mark_of(&written(true)).map(|m| m.down), Some(true));
    assert_eq!(retry_mark_of(&written(false)).map(|m| m.down), Some(false));
    assert_eq!(
        retry_mark_of(&written(true).replace("down=yes", "down=1")),
        None
    );
}

/// v6 §14.5 (5a): the done checks of a Cruise run ride the streak seed's note beside the
/// replies' ([`checks_note`]), on the point's row and the act's, so a loop after this one asks
/// the next angle; Off's stand ends the run, and an older build's rows (no note) read none.
/// NEGATIVE CONTROL: with `checks_note` dropped from `act_reason_after`, the seed reads 0 and
/// the test fails at the act's row.
#[test]
fn the_checks_note_carries_the_cruise_runs_count_and_an_older_row_reads_none() {
    use super::super::policy::turn_end::{RULE_DONE_CHECK, done_check_words};
    let sid = Some("@s-9");
    let row = |ts: i64, rule_id: &str, outcome: Outcome, command: &str, reason: &str| {
        let row = Row {
            rule_id,
            outcome,
            command,
            reason,
            box_seq: 1,
        };
        format!("{}\n", row.to_json(ts, sid))
    };
    let at = |checks: u8| Carried {
        checks,
        ..Carried::streak(2)
    };
    assert_eq!(checks_note_of(&point_reason(at(4), false)), Some(4));
    assert_eq!(checks_note_of(&act_reason_after(at(4), None)), Some(4));
    assert_eq!(
        checks_note_of(&point_reason(at(0), false)),
        None,
        "none typed: no note, as an older build wrote"
    );
    let point = row(
        10_000,
        RULE_POINT,
        Outcome::Waiting,
        "",
        &point_reason(at(4), false),
    );
    assert_eq!(short_streak_in(&point, sid).map(|s| s.checks), Some(4));
    let typed = format!(
        "{point}{}",
        row(
            20_000,
            RULE_DONE_CHECK,
            Outcome::Typed,
            &done_check_words(4),
            &act_reason_after(at(5), None)
        )
    );
    let seed = short_streak_in(&typed, sid).expect("a seed");
    assert_eq!((seed.checks, seed.short), (5, 2));
    let stood = format!(
        "{typed}{}",
        row(30_000, RULE_OFF_STOOD, Outcome::Waiting, "", "Off stood")
    );
    assert_eq!(short_streak_in(&stood, sid).map(|s| s.checks), Some(0));
    let older = typed
        .replace(&checks_note(5), "")
        .replace(&checks_note(4), "");
    assert_ne!(older, typed);
    assert_eq!(short_streak_in(&older, sid).map(|s| s.checks), Some(0));
}

/// THE NEEDS ACK RIDES THE BOOKS, so a handover's successor adopts the set acked: the act that
/// typed it notes the set's key, a box's key typed since leaves it standing, and the next
/// point row says what stands acked then. NEGATIVE CONTROLS: a point row noting none takes it
/// back (the set changed or came down), and an older build's rows, with no note, read none.
#[test]
fn the_needs_acked_note_stands_until_a_point_row_says_otherwise() {
    use super::super::policy::turn_end::RULE_CONTINUE;
    let sid = Some("@s-9");
    let row = |ts: i64, rule_id: &str, outcome: Outcome, command: &str, reason: &str| {
        let row = Row {
            rule_id,
            outcome,
            command,
            reason,
            box_seq: 1,
        };
        format!("{}\n", row.to_json(ts, sid))
    };
    let acked = |needs_acked: u64| Carried {
        needs_acked,
        ..Carried::streak(0)
    };
    let set = 0x0123_4567_89ab_cdef_u64;
    assert_eq!(
        needs_acked_note_of(&act_reason_after(acked(set), None)),
        Some(set)
    );
    assert_eq!(
        needs_acked_note_of(&point_reason(acked(set), false)),
        Some(set)
    );
    assert_eq!(needs_acked_note_of(&point_reason(acked(0), false)), None);
    let seed = |body: &str| short_streak_in(body, sid).map(|s| s.needs_acked);
    let point = row(
        10_000,
        RULE_POINT,
        Outcome::Waiting,
        "",
        &point_reason(acked(0), false),
    );
    assert_eq!(seed(&point), Some(0));
    let typed = format!(
        "{point}{}",
        row(
            20_000,
            RULE_CONTINUE,
            Outcome::Typed,
            "keep going Needs on screen; carry on.",
            &act_reason_after(acked(set), None)
        )
    );
    assert_eq!(seed(&typed), Some(set));
    let boxed = format!(
        "{typed}{}",
        row(30_000, "allow@v1", Outcome::Typed, "1", "1. Yes")
    );
    assert_eq!(seed(&boxed), Some(set), "a box's key says nothing of it");
    let pointed = format!(
        "{boxed}{}",
        row(
            40_000,
            RULE_POINT,
            Outcome::Waiting,
            "",
            &point_reason(acked(0), false)
        )
    );
    assert_eq!(seed(&pointed), Some(0), "the point row's word stands");
    let older = typed.replace(&needs_acked_note(set), "");
    assert_ne!(older, typed);
    assert_eq!(seed(&older), Some(0));
}

/// v6 §14.5 (5a): every angle's check, bare, with its suffixes and as the person's hour cuts it
/// (`go_ahead::unless_held`), is delivered as the done check (`delivers_check`) and owed as one
/// to a successor (`walk_done_check`), as v1's fixed check is. NEGATIVE CONTROLS: the same act
/// under another id seeds no owed check, and v1 under a base that is not its prefix is not
/// delivered.
#[test]
fn every_angled_check_is_delivered_and_owed_as_the_done_check() {
    use super::super::policy::turn_end::{
        DONE_ANGLES, DONE_CHECK_FIXED_BEFORE, RULE_CONTINUE, RULE_DONE_CHECK, done_check_act,
        done_check_words,
    };
    let sid = Some("@s-9");
    let typed = |rule_id: &str, command: &str| {
        let row = Row {
            rule_id,
            outcome: Outcome::Typed,
            command,
            reason: &act_reason(None),
            box_seq: 1,
        };
        format!("{}\n", row.to_json(10_000, sid))
    };
    let mut checks: Vec<String> = (0..DONE_ANGLES.len())
        .flat_map(|n| {
            [
                done_check_words(n),
                done_check_act(n, Some("never push to main"), true),
                super::super::policy::go_ahead::unless_held(&done_check_words(n), true)
                    .into_owned(),
            ]
        })
        .collect();
    checks.push(DONE_CHECK_FIXED_BEFORE.to_string());
    for check in &checks {
        assert!(delivers_check(Some(check)), "{check}");
        let wrapped = check.split_whitespace().collect::<Vec<_>>().join("\n");
        assert!(delivers_check(Some(&wrapped)), "{check}");
        let owed = done_check_owed_in(&typed(RULE_DONE_CHECK, check), sid);
        assert_eq!(owed.map(|o| o.rule), Some(RULE_DONE_CHECK), "{check}");
        assert_eq!(
            done_check_owed_in(&typed(RULE_CONTINUE, check), sid),
            None,
            "{check}"
        );
    }
    let moved = DONE_CHECK_FIXED_BEFORE.replacen("done is never the end.", "never stop.", 1);
    assert_ne!(moved, DONE_CHECK_FIXED_BEFORE);
    assert!(!delivers_check(Some(&moved)), "{moved}");
}

#[test]
fn a_compactions_continuation_that_types_the_check_again_keeps_it_owed() {
    use super::super::policy::turn_end::{
        DONE_CHECK, DONE_CHECK_MARKER_BEFORE, DONE_CHECK_ORDINARY_BEFORE, LAND, RULE_COMPACT,
        RULE_DONE_CHECK, RULE_LAND,
    };
    let ledger = |rows: &[(i64, &str, Outcome, &str, String)]| -> String {
        rows.iter()
            .map(|(ts, rule_id, outcome, command, reason)| {
                let row = Row {
                    rule_id,
                    outcome: *outcome,
                    command,
                    reason,
                    box_seq: 1,
                };
                format!("{}\n", row.to_json(*ts, Some("@s-9")))
            })
            .collect()
    };
    let sid = Some("@s-9");
    let wall = (
        20_000,
        RULE_WALL_WAIT,
        Outcome::Waiting,
        "",
        spend_wall_reason(),
    );
    let point = |ts: i64| {
        (
            ts,
            RULE_POINT,
            Outcome::Waiting,
            "",
            point_reason(Carried::streak(0), false),
        )
    };
    let rows = |act: (&'static str, &'static str), again: &'static str| {
        vec![
            (10_000, act.0, Outcome::Typed, act.1, act_reason(None)),
            wall.clone(),
            (
                30_000,
                RULE_COMPACT,
                Outcome::Typed,
                "/compact",
                act_reason(None),
            ),
            point(40_000),
            (
                50_000,
                RULE_COMPACT,
                Outcome::Typed,
                again,
                act_reason(None),
            ),
        ]
    };
    for check in [
        DONE_CHECK,
        DONE_CHECK_ORDINARY_BEFORE,
        DONE_CHECK_MARKER_BEFORE,
    ] {
        let body = ledger(&rows((RULE_DONE_CHECK, check), check));
        let owed = done_check_owed_in(&body, sid).expect("owed");
        assert!(!owed.cut && !owed.answered, "{owed:?}");
        assert_eq!(owed.rule, RULE_DONE_CHECK);
        let wrapped = check.split_whitespace().collect::<Vec<_>>().join("\n");
        assert!(delivers_check(Some(&wrapped)));
    }
    let body = ledger(&rows((RULE_LAND, LAND), LAND));
    let ep = land_episode_in(&body, sid).expect("the episode");
    assert!(ep.landing && !ep.cut, "{ep:?}");
    let body = ledger(&rows((RULE_DONE_CHECK, DONE_CHECK), "keep going"));
    assert_eq!(done_check_owed_in(&body, sid), None);
    assert!(!delivers_check(None));
    assert!(!delivers_check(Some("keep going")));
    let body = ledger(&rows((RULE_LAND, LAND), "keep going"));
    assert!(land_episode_in(&body, sid).is_none_or(|ep| !ep.landing));
}

#[test]
fn an_answer_judged_is_owed_no_more() {
    use super::super::policy::turn_end::{RULE_DONE_CHECK, RULE_LAND, RULE_LIMIT_RESUME};
    let dir = crate::supervise::ScratchDir::new("apr-seed", "owed");
    let sid = Some("@s-9");
    let check = (
        10_000,
        RULE_DONE_CHECK,
        Outcome::Typed,
        "(the check)",
        act_reason(None),
    );
    let point = |ts: i64| {
        (
            ts,
            RULE_POINT,
            Outcome::Waiting,
            "",
            point_reason(Carried::streak(1), false),
        )
    };
    let done_at = |ts: i64| {
        (
            ts,
            RULE_TASK_DONE,
            Outcome::Waiting,
            "",
            TASK_DONE.to_string(),
        )
    };
    let done = done_at(20_000);
    let wall = (
        20_000,
        RULE_WALL_WAIT,
        Outcome::Waiting,
        "",
        spend_wall_reason(),
    );
    let path = seed_ledger(&dir, "owed", std::slice::from_ref(&check));
    let owed = done_check_owed(&path, sid).expect("owed");
    assert!(owed.check.is_some() && !owed.answered, "{owed:?}");
    let path = seed_ledger(&dir, "owed-not-done", &[check.clone(), point(20_000)]);
    assert_eq!(done_check_owed(&path, sid), None);
    let path = seed_ledger(
        &dir,
        "owed-done",
        &[check.clone(), point(20_000), done.clone()],
    );
    let owed = done_check_owed(&path, sid).expect("answered");
    assert!(owed.answered && owed.check.is_none(), "{owed:?}");
    let path = seed_ledger(
        &dir,
        "owed-later",
        &[check.clone(), point(20_000), done, point(30_000)],
    );
    assert_eq!(done_check_owed(&path, sid), None);
    let path = seed_ledger(&dir, "owed-cut", &[check, wall, point(20_000)]);
    let owed = done_check_owed(&path, sid).expect("cut");
    assert!(owed.cut, "{owed:?}");
    let limit = LimitTry {
        bucket: false,
        in_place: false,
        attempts: 1,
        entitlement: true,
    };
    let tried = (
        10_000,
        RULE_LIMIT_RESUME,
        Outcome::Typed,
        "keep going",
        act_reason(Some(limit)),
    );
    let path = seed_ledger(&dir, "try-judged", &[tried.clone(), point(20_000)]);
    assert_eq!(awaited_try(&path, sid), None);
    let path = seed_ledger(&dir, "try-unjudged", &[tried]);
    assert!(awaited_try(&path, sid).is_some());
    let land = (
        10_000,
        RULE_LAND,
        Outcome::Typed,
        "(the land instruction)",
        act_reason(None),
    );
    let landing = |name: &str, rows: &[(i64, &str, Outcome, &str, String)]| {
        let e = land_episode(&seed_ledger(&dir, name, rows), sid).expect("an episode");
        (e.landing, e.answered)
    };
    assert_eq!(landing("land", std::slice::from_ref(&land)), (true, false));
    assert_eq!(
        landing("land-not-done", &[land.clone(), point(20_000)]),
        (false, false)
    );
    assert_eq!(
        landing("land-done", &[land, point(20_000), done_at(20_001)]),
        (true, true)
    );
}

#[test]
fn a_done_check_an_api_error_cut_is_owed_to_the_walls_retry() {
    use super::super::policy::turn_end::{RULE_API_RETRY, RULE_DONE_CHECK};
    let dir = crate::supervise::ScratchDir::new("apr-seed", "check-cut-by-api");
    let sid = Some("@s-9");
    let check = (
        10_000,
        RULE_DONE_CHECK,
        Outcome::Typed,
        "(the check)",
        act_reason(None),
    );
    let mark = |attempts: usize| RetryMark {
        kind: aterm_phase::WallKind::Overloaded,
        attempts,
        ups: 0,
        since_ms: 310_000,
        down: false,
        witnessed: false,
    };
    let point = |ts: i64, retry: Option<RetryMark>| {
        (
            ts,
            RULE_POINT,
            Outcome::Waiting,
            "",
            point_reason(
                Carried {
                    short: 1,
                    retry,
                    said: Replies::default(),
                    said_reset_ms: None,
                    checks: 0,
                    land: LandBooks::default(),
                    pass: WallPass::default(),
                    needs_acked: 0,
                },
                false,
            ),
        )
    };
    let retry = |ts: i64| {
        (
            ts,
            RULE_API_RETRY,
            Outcome::Typed,
            "(the retry)",
            act_reason_after(
                Carried {
                    short: 1,
                    retry: Some(mark(1)),
                    said: Replies::default(),
                    said_reset_ms: None,
                    checks: 0,
                    land: LandBooks::default(),
                    pass: WallPass::default(),
                    needs_acked: 0,
                },
                None,
            ),
        )
    };
    let cut = point(310_000, Some(mark(0)));
    let owed = |name: &str, rows: &[(i64, &str, Outcome, &str, String)]| {
        done_check_owed(&seed_ledger(&dir, name, rows), sid)
    };
    let o = owed("cut", &[check.clone(), cut.clone()]).expect("cut");
    assert!(o.cut && o.check.is_none() && !o.answered, "{o:?}");
    let o = owed("retried", &[check.clone(), cut.clone(), retry(370_000)]).expect("again");
    assert!(!o.cut && o.check.is_none(), "{o:?}");
    let answered = [
        check.clone(),
        cut.clone(),
        retry(370_000),
        point(550_000, None),
    ];
    assert_eq!(owed("answered", &answered), None);
    for (name, theirs) in [
        ("theirs-cut", point(880_000, Some(mark(0)))),
        ("theirs", point(880_000, None)),
    ] {
        let rows = [check.clone(), cut.clone(), theirs];
        assert_eq!(owed(name, &rows), None, "{name}");
        let rows = [rows.as_slice(), &[retry(940_000)]].concat();
        assert_eq!(owed(&format!("{name}-retried"), &rows), None, "{name}");
    }
    assert_eq!(owed("unmarked", &[check, point(310_000, None)]), None);
}

#[test]
fn a_compact_at_the_checks_point_owes_the_check_to_its_continuation() {
    use super::super::policy::turn_end::{
        DONE_CHECK, RULE_API_RETRY, RULE_COMPACT, RULE_DONE_CHECK, RULE_LIMIT_RESUME,
    };
    let sid = Some("@s-9");
    let body = |rows: &[(i64, &str, Outcome, &str, String)]| -> String {
        rows.iter()
            .map(|(ts, rule_id, outcome, command, reason)| {
                let row = Row {
                    rule_id,
                    outcome: *outcome,
                    command,
                    reason,
                    box_seq: 1,
                };
                format!("{}\n", row.to_json(*ts, sid))
            })
            .collect()
    };
    let owed = |rows: &[(i64, &str, Outcome, &str, String)]| done_check_owed_in(&body(rows), sid);
    let typed = |ts: i64, rule: &'static str, words: &'static str| {
        (ts, rule, Outcome::Typed, words, act_reason(None))
    };
    let check = typed(10_000, RULE_DONE_CHECK, DONE_CHECK);
    let mark = RetryMark {
        kind: aterm_phase::WallKind::Overloaded,
        attempts: 0,
        ups: 0,
        since_ms: 20_000,
        down: false,
        witnessed: false,
    };
    let point = |ts: i64, retry: Option<RetryMark>| {
        (
            ts,
            RULE_POINT,
            Outcome::Waiting,
            "",
            point_reason(
                Carried {
                    short: 0,
                    retry,
                    said: Replies::default(),
                    said_reset_ms: None,
                    checks: 0,
                    land: LandBooks::default(),
                    pass: WallPass::default(),
                    needs_acked: 0,
                },
                false,
            ),
        )
    };
    let waiting =
        |ts: i64, rule: &'static str, reason: String| (ts, rule, Outcome::Waiting, "", reason);
    let compact = |ts: i64| typed(ts, RULE_COMPACT, "/compact");
    let cut = [check.clone(), point(20_000, None), compact(30_000)];
    let compacted = [cut.as_slice(), &[point(40_000, None)]].concat();
    for (name, rows) in [
        ("the /compact", cut.to_vec()),
        ("its answer", compacted.clone()),
        (
            "again on the ladder",
            [
                compacted.as_slice(),
                &[compact(50_000), point(60_000, None)],
            ]
            .concat(),
        ),
        (
            "after a 529's retry",
            vec![
                check.clone(),
                point(20_000, Some(mark)),
                typed(25_000, RULE_API_RETRY, "(the retry)"),
                point(28_000, None),
                compact(30_000),
            ],
        ),
        (
            "under a wall row's cut",
            vec![
                check.clone(),
                waiting(20_000, RULE_WALL_WAIT, spend_wall_reason()),
                compact(30_000),
            ],
        ),
        (
            "a wall the /compact met",
            [
                cut.as_slice(),
                &[waiting(40_000, RULE_WALL_WAIT, spend_wall_reason())],
            ]
            .concat(),
        ),
    ] {
        let o = owed(&rows).unwrap_or_else(|| panic!("{name}: owed"));
        assert!(o.compact && !o.cut && o.check.is_none(), "{name}: {o:?}");
    }
    let again = [
        compacted.as_slice(),
        &[typed(50_000, RULE_COMPACT, DONE_CHECK)],
    ]
    .concat();
    let o = owed(&again).expect("again");
    assert!(!o.compact && !o.cut, "{o:?}");
    for (name, rows) in [
        (
            "keep going",
            [
                compacted.as_slice(),
                &[typed(50_000, RULE_COMPACT, "keep going")],
            ]
            .concat(),
        ),
        (
            "Off before the /compact",
            vec![
                check.clone(),
                point(20_000, None),
                waiting(25_000, RULE_OFF_STOOD, OFF_STOOD.to_string()),
                compact(30_000),
            ],
        ),
        (
            "Off after it",
            [
                cut.as_slice(),
                &[waiting(35_000, RULE_OFF_STOOD, OFF_STOOD.to_string())],
            ]
            .concat(),
        ),
        (
            "a wall's try",
            [
                cut.as_slice(),
                &[
                    waiting(40_000, RULE_WALL_WAIT, spend_wall_reason()),
                    typed(50_000, RULE_LIMIT_RESUME, "keep going"),
                ],
            ]
            .concat(),
        ),
        (
            "a 529's retry",
            [
                cut.as_slice(),
                &[
                    point(40_000, Some(mark)),
                    typed(50_000, RULE_API_RETRY, "(the retry)"),
                ],
            ]
            .concat(),
        ),
        (
            "a later turn's",
            vec![
                check.clone(),
                point(20_000, None),
                point(25_000, None),
                compact(30_000),
            ],
        ),
    ] {
        assert_eq!(owed(&rows), None, "{name}");
    }
    let last = |rows: &[(i64, &str, Outcome, &str, String)]| check_point_last_in(&body(rows), sid);
    assert_eq!(
        last(&[check.clone(), point(20_000, None)]),
        Some(RULE_DONE_CHECK)
    );
    for (name, rows) in [
        ("the check unanswered", vec![check.clone()]),
        (
            "its point cut",
            vec![check.clone(), point(20_000, Some(mark))],
        ),
        (
            "a later turn's",
            vec![check.clone(), point(20_000, None), point(25_000, None)],
        ),
        (
            "Off",
            vec![
                check.clone(),
                point(20_000, None),
                waiting(25_000, RULE_OFF_STOOD, OFF_STOOD.to_string()),
            ],
        ),
        ("the /compact", cut.to_vec()),
    ] {
        assert_eq!(last(&rows), None, "{name}");
    }
}

#[test]
fn a_compact_at_a_land_acts_point_owes_the_act_to_its_continuation() {
    use super::super::policy::turn_end::{
        LAND, LAND_ANSWER, LAND_NUDGE, RULE_API_RETRY, RULE_COMPACT, RULE_LAND, RULE_LAND_ANSWER,
        RULE_LAND_NUDGE, RULE_LIMIT_RESUME,
    };
    type R = (i64, &'static str, Outcome, &'static str, String);
    let sid = Some("@s-9");
    let ep = |rows: &[R]| -> Option<LandSeed> {
        let body: String = rows
            .iter()
            .map(|(ts, rule_id, outcome, command, reason)| {
                let row = Row {
                    rule_id,
                    outcome: *outcome,
                    command,
                    reason,
                    box_seq: 1,
                };
                format!("{}\n", row.to_json(*ts, sid))
            })
            .collect();
        land_episode_in(&body, sid)
    };
    let typed = |ts: i64, rule: &'static str, words: &'static str| -> R {
        (ts, rule, Outcome::Typed, words, act_reason(None))
    };
    let mark = RetryMark {
        kind: aterm_phase::WallKind::Overloaded,
        attempts: 0,
        ups: 0,
        since_ms: 20_000,
        down: false,
        witnessed: false,
    };
    let point = |ts: i64, retry: Option<RetryMark>| -> R {
        (
            ts,
            RULE_POINT,
            Outcome::Waiting,
            "",
            point_reason(
                Carried {
                    short: 0,
                    retry,
                    said: Replies::default(),
                    said_reset_ms: None,
                    checks: 0,
                    land: LandBooks::default(),
                    pass: WallPass::default(),
                    needs_acked: 0,
                },
                false,
            ),
        )
    };
    let waiting = |ts: i64, rule: &'static str, reason: String| -> R {
        (ts, rule, Outcome::Waiting, "", reason)
    };
    let compact = |ts: i64| typed(ts, RULE_COMPACT, "/compact");
    let given = typed(10_000, RULE_LAND, LAND);
    let cut = [given.clone(), point(20_000, None), compact(30_000)];
    let compacted = [cut.as_slice(), &[point(40_000, None)]].concat();
    let owes = |name: &str, rows: &[R], rule: &str| {
        let e = ep(rows).unwrap_or_else(|| panic!("{name}: in hand"));
        assert!(e.compact && !e.landing && !e.cut, "{name}: {e:?}");
        assert_eq!(e.owed, Some(rule), "{name}");
    };
    for (name, rows) in [
        ("the /compact", cut.to_vec()),
        ("its answer", compacted.clone()),
        (
            "again on the ladder",
            [
                compacted.as_slice(),
                &[compact(50_000), point(60_000, None)],
            ]
            .concat(),
        ),
        (
            "after a 529's retry",
            vec![
                given.clone(),
                point(20_000, Some(mark)),
                typed(25_000, RULE_API_RETRY, "(the retry)"),
                point(28_000, None),
                compact(30_000),
            ],
        ),
        (
            "under a wall row's cut",
            vec![
                given.clone(),
                waiting(20_000, RULE_WALL_WAIT, spend_wall_reason()),
                compact(30_000),
            ],
        ),
        (
            "a wall the /compact met",
            [
                cut.as_slice(),
                &[waiting(40_000, RULE_WALL_WAIT, spend_wall_reason())],
            ]
            .concat(),
        ),
    ] {
        owes(name, &rows, RULE_LAND);
    }
    for (rule, words) in [
        (RULE_LAND_NUDGE, LAND_NUDGE),
        (RULE_LAND_ANSWER, LAND_ANSWER),
    ] {
        let rows = [
            given.clone(),
            point(12_000, None),
            typed(14_000, rule, words),
            point(20_000, None),
            compact(30_000),
        ];
        owes(rule, &rows, rule);
    }
    let answer = [
        given.clone(),
        point(12_000, None),
        typed(14_000, RULE_LAND_ANSWER, LAND_ANSWER),
    ];
    for (rule, words, mut rows) in [
        (RULE_LAND, LAND, vec![given.clone()]),
        (RULE_LAND_ANSWER, LAND_ANSWER, answer.to_vec()),
    ] {
        rows.extend([
            point(20_000, None),
            compact(30_000),
            point(40_000, None),
            typed(50_000, RULE_COMPACT, words),
        ]);
        let e = ep(&rows).expect("again");
        assert!(e.landing && !e.compact, "{rule}: {e:?}");
        assert_eq!(e.owed, Some(rule), "{rule}");
        rows.extend([point(60_000, None), compact(70_000)]);
        owes("its continuation's own cut", &rows, rule);
    }
    for (name, rows) in [
        (
            "keep going",
            [
                compacted.as_slice(),
                &[typed(50_000, RULE_COMPACT, "keep going")],
            ]
            .concat(),
        ),
        (
            "Off before the /compact",
            vec![
                given.clone(),
                point(20_000, None),
                waiting(25_000, RULE_OFF_STOOD, OFF_STOOD.to_string()),
                compact(30_000),
            ],
        ),
        (
            "Off after it",
            [
                cut.as_slice(),
                &[waiting(35_000, RULE_OFF_STOOD, OFF_STOOD.to_string())],
            ]
            .concat(),
        ),
        (
            "a wall's try",
            [
                cut.as_slice(),
                &[
                    waiting(40_000, RULE_WALL_WAIT, spend_wall_reason()),
                    typed(50_000, RULE_LIMIT_RESUME, "keep going"),
                ],
            ]
            .concat(),
        ),
        (
            "a 529's retry",
            [
                cut.as_slice(),
                &[
                    point(40_000, Some(mark)),
                    typed(50_000, RULE_API_RETRY, "(the retry)"),
                ],
            ]
            .concat(),
        ),
        (
            "a later turn's",
            vec![
                given.clone(),
                point(20_000, None),
                point(25_000, None),
                compact(30_000),
            ],
        ),
    ] {
        let e = ep(&rows);
        assert!(
            e.is_none_or(|e| !e.compact && !e.landing && e.owed.is_none()),
            "{name}: {e:?}"
        );
    }
    let e = ep(&[given.clone(), point(20_000, None)]).expect("judged");
    assert!(e.judged && !e.landing, "{e:?}");
    assert_eq!(e.owed, Some(RULE_LAND));
    for (name, rows) in [
        ("the act unanswered", vec![given.clone()]),
        (
            "its point cut",
            vec![given.clone(), point(20_000, Some(mark))],
        ),
        (
            "a later turn's",
            vec![given.clone(), point(20_000, None), point(25_000, None)],
        ),
        (
            "Off",
            vec![
                given.clone(),
                point(20_000, None),
                waiting(25_000, RULE_OFF_STOOD, OFF_STOOD.to_string()),
            ],
        ),
        (
            "landed there",
            vec![
                given.clone(),
                point(20_000, None),
                waiting(21_000, RULE_TASK_DONE, TASK_DONE.to_string()),
            ],
        ),
        ("the /compact", cut.to_vec()),
    ] {
        assert!(ep(&rows).is_none_or(|e| !e.judged), "{name}");
    }
}

/// LAND'S INSTRUCTION TYPED AS A RESUME (v6 §14.17.5.1, §14.17.5.2): a resume past a
/// condition, or a compaction's owed continuation, before the instruction was given types the
/// instruction under its own rule, and the books a later loop seeds from start the episode
/// there, so that loop never types the instruction again.
#[test]
fn lands_instruction_typed_under_a_resumes_rule_starts_the_episode() {
    use super::super::policy::turn_end::{
        Cleared, Clearing, LAND, LAND_REMOTE_MAIN_BEFORE, Outage, RULE_COMPACT, RULE_LAND,
        RULE_LIMIT_RESUME, RULE_OUTAGE_RESUME, cleared_text,
    };
    let sid = Some("@s-9");
    let why = act_reason(None);
    let ep = |rule: &str, command: &str| {
        let row = Row {
            rule_id: rule,
            outcome: Outcome::Typed,
            command,
            reason: &why,
            box_seq: 1,
        };
        land_episode_in(&format!("{}\n", row.to_json(10_000, sid)), sid)
    };
    let given = ep(RULE_LAND, LAND).expect("Land's own act");
    for rule in [RULE_OUTAGE_RESUME, RULE_LIMIT_RESUME, RULE_COMPACT] {
        let e = ep(rule, LAND).unwrap_or_else(|| panic!("{rule}: the episode"));
        assert_eq!(e, given, "{rule}");
        assert!(
            e.landing && e.nudges == 0 && e.act.is_some(),
            "{rule}: {e:?}"
        );
        assert_eq!(e.owed, Some(RULE_LAND), "{rule}");
    }
    assert!(
        ep(RULE_OUTAGE_RESUME, LAND_REMOTE_MAIN_BEFORE).is_some(),
        "a retired build's instruction"
    );
    // CONTROL: the outage's own line starts no episode.
    let line = cleared_text(Cleared::Outage(Outage::Overload), Clearing::Blind);
    assert_eq!(ep(RULE_OUTAGE_RESUME, line), None);
}

#[test]
fn a_cut_keeps_what_the_readers_read_of_a_run_of_records() {
    use super::super::policy::turn_end::{RULE_LAND, RULE_LAND_NUDGE};
    let dir = crate::supervise::ScratchDir::new("apr-seed", "cut");
    let sid = Some("@s-9");
    let filler = "cd /Users//someone/Developer/project && cargo test -p project-core --lib -- parser::tests 2>&1 | tail -40";
    let mut rows: Vec<(i64, &str, Outcome, &str, String)> = (0..3000)
        .map(|i| {
            let why = "the safe rules proved the command reads and runs tests".to_string();
            (i, "allow-once@v1", Outcome::Approved, filler, why)
        })
        .collect();
    let t = 1_000_000_i64;
    let point = |ts: i64, n: u32| {
        (
            ts,
            RULE_POINT,
            Outcome::Waiting,
            "",
            point_reason(Carried::streak(n), false),
        )
    };
    let ran = |ts: i64| {
        (
            ts,
            RULE_TURN_RAN,
            Outcome::Waiting,
            "",
            TURN_RAN.to_string(),
        )
    };
    for (k, rule) in [RULE_LAND, RULE_LAND_NUDGE, RULE_LAND_NUDGE]
        .into_iter()
        .enumerate()
    {
        let at = t + 1_000 + 1_000 * i64::try_from(k).unwrap();
        rows.push((at, rule, Outcome::Typed, "(land)", act_reason(None)));
        rows.push(point(at + 500, 0));
    }
    let wakes = 1100_u32;
    for n in 1..=wakes {
        let at = t + 10_000 + 300_000 * i64::from(n);
        rows.push(ran(at));
        rows.push(point(at + 20_000, n));
    }
    rows.push(ran(t + 10_000 + 300_000 * i64::from(wakes + 1)));
    let read = |path: &Path| {
        (
            land_episode(path, sid),
            short_streak(path, sid),
            done_check_owed(path, sid),
            awaited_try(path, sid),
        )
    };
    let path = seed_ledger(&dir, "whole", &rows);
    assert!(std::fs::metadata(&path).unwrap().len() > MAX_BYTES);
    let before = read(&path);
    assert!(before.0.is_some(), "{before:?}");
    let streak = before.1.clone().expect("a streak");
    assert!(
        streak.point && streak.ran && streak.short == wakes,
        "{streak:?}"
    );
    bound(&path);
    let kept = std::fs::read_to_string(&path).unwrap();
    assert!(kept.lines().filter(|l| record_of(l).is_none()).count() <= KEEP_ROWS);
    assert_eq!(read(&path), before);
    let records: Vec<&str> = kept
        .lines()
        .filter(|l| l.contains("point@v1") || l.contains("turn-ran@v1"))
        .collect();
    assert_eq!(records.len(), 2 + 3, "{records:#?}");
    let ts = |l: &str| l.split(',').next().map(str::to_string);
    let at = |ms: i64| Some(format!("{{\"ts\":{ms}"));
    assert_eq!(ts(records[2]), at(t + 3_500));
    assert_eq!(
        ts(records[3]),
        at(t + 10_000 + 300_000 * i64::from(wakes) + 20_000)
    );
    assert!(records[4].contains(RULE_TURN_RAN), "{}", records[4]);
    let path = seed_ledger(&dir, "whole-plain", &rows);
    crate::harness::cli::bound_ledger(&path, MAX_BYTES, KEEP_ROWS).expect("cut");
    let plain = read(&path);
    assert!(plain.0.is_none(), "CONTROL: {plain:?}");
}

#[test]
fn a_cut_keeps_the_rows_of_the_states_that_stand_past_a_run_of_acts() {
    use super::super::policy::turn_end::{
        LAND, LAND_ANSWER, RULE_API_RETRY, RULE_COMPACT, RULE_CONTINUE, RULE_LAND,
        RULE_LAND_ANSWER, RULE_LAND_NUDGE,
    };
    let dir = crate::supervise::ScratchDir::new("apr-seed", "cut-acts");
    let sid = Some("@s-9");
    let filler = "cd /Users//someone/Developer/project && cargo test -p project-core --lib -- parser::tests 2>&1 | tail -40";
    let mut rows: Vec<(i64, &str, Outcome, &str, String)> = (0..3000)
        .map(|i| {
            let why = "the safe rules proved the command reads and runs tests".to_string();
            (i, "allow-once@v1", Outcome::Approved, filler, why)
        })
        .collect();
    let t = 1_000_000_i64;
    let point = |ts: i64| {
        (
            ts,
            RULE_POINT,
            Outcome::Waiting,
            "",
            point_reason(Carried::streak(0), false),
        )
    };
    for (k, rule) in [RULE_LAND, RULE_LAND_NUDGE, RULE_LAND_NUDGE]
        .into_iter()
        .enumerate()
    {
        let at = t + 1_000 + 1_000 * i64::try_from(k).unwrap();
        rows.push((at, rule, Outcome::Typed, "(land)", act_reason(None)));
        rows.push(point(at + 500));
    }
    let retry = "Claude Code reported an API error (overloaded). Carry on from where you stopped.";
    for n in 1..=1100_i64 {
        let at = t + 10_000 + 300_000 * n;
        rows.push((at, RULE_API_RETRY, Outcome::Typed, retry, act_reason(None)));
        rows.push(point(at + 3_000));
    }
    let read = |path: &Path| {
        (
            land_episode(path, sid),
            short_streak(path, sid),
            done_check_owed(path, sid),
            awaited_try(path, sid),
        )
    };
    let path = seed_ledger(&dir, "acts", &rows);
    assert!(std::fs::metadata(&path).unwrap().len() > MAX_BYTES);
    let before = read(&path);
    assert!(before.0.is_some(), "{before:?}");
    bound(&path);
    assert_eq!(read(&path), before);
    let mut done = rows.clone();
    done.push((
        t + 10_000 + 300_000 * 1101,
        RULE_CONTINUE,
        Outcome::Typed,
        "keep going",
        act_reason(None),
    ));
    let path = seed_ledger(&dir, "acts-over", &done);
    let over = read(&path);
    assert!(over.0.is_none(), "{over:?}");
    bound(&path);
    let cut = std::fs::read_to_string(&path).unwrap();
    let acts = cut.lines().filter(|l| record_of(l).is_none()).count();
    assert_eq!(acts, KEEP_ROWS, "{} rows", cut.lines().count());
    assert_eq!(read(&path), over);
    let held = |name: &str, rows: &[(i64, &str, Outcome, &str, String)]| {
        let path = seed_ledger(&dir, name, rows);
        let before = read(&path);
        bound(&path);
        let kept = std::fs::read_to_string(&path).unwrap();
        let len = u64::try_from(kept.len()).unwrap();
        assert!(
            len <= MAX_BYTES,
            "{name}: the cut left {len} bytes, {} rows",
            kept.lines().count()
        );
        assert_eq!(read(&path), before, "{name}");
        before
    };
    let mut land: Vec<(i64, &str, Outcome, &str, String)> = rows
        .iter()
        .filter(|r| r.1 == RULE_LAND || r.1 == RULE_LAND_NUDGE || r.1 == RULE_POINT)
        .take(6)
        .cloned()
        .collect();
    for i in 0..9000_i64 {
        let why = "the safe rules proved the command reads and runs tests".to_string();
        land.push((
            t + 10_000 + i * 60_000,
            "allow-once@v1",
            Outcome::Approved,
            filler,
            why,
        ));
    }
    let landed = held("land-held", &land);
    assert!(landed.0.is_some_and(|e| e.nudges == 2), "{landed:?}");
    let mut compact_answered = vec![
        (t, RULE_LAND, Outcome::Typed, LAND, act_reason(None)),
        point(t + 1),
        (
            t + 2,
            RULE_LAND_ANSWER,
            Outcome::Typed,
            LAND_ANSWER,
            act_reason(None),
        ),
        point(t + 3),
        (
            t + 4,
            RULE_COMPACT,
            Outcome::Typed,
            "/compact",
            act_reason(None),
        ),
    ];
    compact_answered.extend(land.iter().skip(6).cloned());
    let owed = held("land-answer-compacted", &compact_answered)
        .0
        .expect("the answer's compact");
    assert!(owed.compact && !owed.landing, "{owed:?}");
    assert_eq!(owed.owed, Some(RULE_LAND_ANSWER));
    let mut answered: Vec<(i64, &str, Outcome, &str, String)> =
        land.iter().take(6).cloned().collect();
    let mut at = t + 10_000;
    for _ in 0..3000 {
        at += 600_000;
        let act = act_reason(None);
        answered.push((at, RULE_LAND_ANSWER, Outcome::Typed, "(land)", act));
        answered.push(point(at + 300_000));
    }
    answered.push(point(at + 400_000));
    for i in 0..1100_i64 {
        let why = "the safe rules proved the command reads and runs tests".to_string();
        answered.push((
            at + 500_000 + i,
            "allow-once@v1",
            Outcome::Approved,
            filler,
            why,
        ));
    }
    let landed = held("land-answered", &answered);
    assert!(
        landed.0.is_some_and(|e| e.nudges == 2 && !e.landing),
        "{landed:?}"
    );
    let path = seed_ledger(&dir, "acts-plain", &rows);
    crate::harness::cli::bound_ledger(&path, MAX_BYTES, KEEP_ROWS).expect("cut");
    let plain = read(&path);
    assert!(plain.0.is_none(), "CONTROL: {plain:?}");
}

#[test]
fn a_land_episodes_since_is_its_instruction_rows_stamp_through_a_cut() {
    use super::super::policy::turn_end::{
        RULE_API_RETRY, RULE_CONTINUE, RULE_LAND, RULE_LAND_NUDGE,
    };
    let dir = crate::supervise::ScratchDir::new("apr-seed", "land-since");
    let sid = Some("@s-9");
    let point = |ts: i64| {
        (
            ts,
            RULE_POINT,
            Outcome::Waiting,
            "",
            point_reason(Carried::streak(0), false),
        )
    };
    let typed =
        |ts: i64, rule: &'static str| (ts, rule, Outcome::Typed, "(land)", act_reason(None));
    let t = 1_771_000_000_000_i64;
    let filler = "cd /Users//someone/Developer/project && cargo test -p project-core --lib -- parser::tests 2>&1 | tail -40";
    let mut rows: Vec<(i64, &str, Outcome, &str, String)> = (0..3000)
        .map(|i| {
            let why = "the safe rules proved the command reads and runs tests".to_string();
            (
                t - 200_000 + i,
                "allow-once@v1",
                Outcome::Approved,
                filler,
                why,
            )
        })
        .collect();
    rows.extend([
        typed(t - 90_000, RULE_LAND),
        point(t - 80_000),
        typed(t - 70_000, RULE_CONTINUE),
        typed(t, RULE_LAND),
        typed(t + 1_000, RULE_LAND),
        point(t + 1_500),
        typed(t + 2_000, RULE_LAND_NUDGE),
        point(t + 2_500),
    ]);
    let path = seed_ledger(&dir, "since", &rows);
    let ep = land_episode(&path, sid).expect("in hand");
    assert_eq!(
        (ep.hold.since_ms, ep.nudges),
        (Some(t + 1_000), 1),
        "{ep:?}"
    );
    let retry = "Claude Code reported an API error (overloaded). Carry on from where you stopped.";
    for n in 1..=1100_i64 {
        let at = t + 10_000 + 300_000 * n;
        rows.push((at, RULE_API_RETRY, Outcome::Typed, retry, act_reason(None)));
        rows.push(point(at + 3_000));
    }
    let path = seed_ledger(&dir, "since-cut", &rows);
    assert!(std::fs::metadata(&path).unwrap().len() > MAX_BYTES);
    let before = land_episode(&path, sid).expect("in hand");
    assert_eq!(before.hold.since_ms, Some(t + 1_000), "{before:?}");
    bound(&path);
    let after = land_episode(&path, sid).expect("kept");
    assert_eq!(after, before);
    let mut over = rows.clone();
    let end = t + 10_000 + 300_000 * 1101;
    over.push(typed(end, RULE_CONTINUE));
    let path = seed_ledger(&dir, "since-over", &over);
    assert_eq!(land_episode(&path, sid), None);
    over.push(typed(end + 1_000, RULE_LAND));
    let path = seed_ledger(&dir, "since-anew", &over);
    let anew = land_episode(&path, sid).expect("anew");
    assert_eq!((anew.hold.since_ms, anew.nudges), (Some(end + 1_000), 0));
    let path = seed_ledger(&dir, "since-plain", &rows);
    crate::harness::cli::bound_ledger(&path, MAX_BYTES, KEEP_ROWS).expect("cut");
    assert_eq!(land_episode(&path, sid), None, "CONTROL");
}

#[test]
fn a_landing_ends_the_hold_a_loop_that_follows_seeds() {
    use super::super::policy::turn_end::{HoldStart, RULE_LAND, RULE_LAND_NUDGE};
    let dir = crate::supervise::ScratchDir::new("apr-seed", "land-relanded");
    let sid = Some("@s-9");
    let point = |ts: i64| {
        (
            ts,
            RULE_POINT,
            Outcome::Waiting,
            "",
            point_reason(Carried::streak(0), false),
        )
    };
    let typed =
        |ts: i64, rule: &'static str| (ts, rule, Outcome::Typed, "(land)", act_reason(None));
    let t = 1_771_000_000_000_i64;
    let mut rows = vec![
        typed(t, RULE_LAND),
        point(t + 500),
        typed(t + 1_000, RULE_LAND_NUDGE),
        point(t + 1_500),
    ];
    let path = seed_ledger(&dir, "held", &rows);
    let ep = land_episode(&path, sid).expect("in hand");
    let instructed = HoldStart {
        since_ms: Some(t),
        base: 0,
        relanded: false,
    };
    assert_eq!((ep.hold, ep.nudges), (instructed, 1), "CONTROL: {ep:?}");
    rows.push((
        t + 1_600,
        RULE_TASK_DONE,
        Outcome::Waiting,
        "",
        TASK_DONE.to_string(),
    ));
    let path = seed_ledger(&dir, "landed", &rows);
    let ep = land_episode(&path, sid).expect("in hand");
    assert_eq!((ep.hold, ep.nudges), (HoldStart::LANDED, 1), "{ep:?}");
    rows.extend([typed(t + 9_000, RULE_LAND_NUDGE), point(t + 9_500)]);
    let path = seed_ledger(&dir, "anew", &rows);
    let ep = land_episode(&path, sid).expect("in hand");
    let anew = HoldStart {
        since_ms: Some(t + 9_000),
        base: 2,
        relanded: false,
    };
    assert_eq!((ep.hold, ep.nudges), (anew, 2), "{ep:?}");
    rows.extend([typed(t + 12_000, RULE_LAND_NUDGE), point(t + 12_500)]);
    let path = seed_ledger(&dir, "after", &rows);
    let ep = land_episode(&path, sid).expect("in hand");
    assert_eq!((ep.hold, ep.nudges), (anew, 3), "{ep:?}");
}

#[test]
fn a_restored_tab_s_cut_keeps_its_take_up_while_a_state_of_the_earlier_books_stands() {
    use super::super::policy::turn_end::{RULE_CONTINUE, RULE_LAND};
    let dir = crate::supervise::ScratchDir::new("apr-seed", "restored-cut");
    let root: &Path = &dir;
    let (was, tab) = ("s-a0", "s-b1");
    std::fs::create_dir_all(root.join("drive")).expect("the drive");
    let row = |ts: i64, rule_id: &str, outcome: Outcome, command: &str, reason: &str, sid| {
        let row = Row {
            rule_id,
            outcome,
            command,
            reason,
            box_seq: 1,
        };
        format!("{}\n", row.to_json(ts, Some(sid)))
    };
    let t = 1_000_000_i64;
    let theirs = ledger_under(root, Some(was));
    std::fs::write(
        &theirs,
        row(
            t,
            RULE_LAND,
            Outcome::Typed,
            "(land)",
            &act_reason(None),
            was,
        ),
    )
    .expect("the earlier tab's books");
    crate::harness::upgrade_drive::note_earlier_tab_under(root, tab, was);
    let filler = "cd /Users//someone/Developer/project && cargo test -p project-core --lib -- parser::tests 2>&1 | tail -40";
    let why = "the safe rules proved the command reads and runs tests";
    let approvals = |from: i64| -> String {
        (0..3000)
            .map(|i| {
                row(
                    from + i,
                    "allow-once@v1",
                    Outcome::Approved,
                    filler,
                    why,
                    tab,
                )
            })
            .collect()
    };
    let take_up = row(
        t + 1_000,
        RULE_EARLIER_BOOKS,
        Outcome::Waiting,
        "",
        EARLIER_BOOKS,
        tab,
    );
    let books = |path: &Path| {
        let own = std::fs::read_to_string(path).unwrap_or_default();
        let earlier = vec![(was.to_string(), std::fs::read_to_string(&theirs).unwrap())];
        let up = takes_up_earlier(&own, Some(tab));
        let books = if up {
            books_through(&own, Some(tab), &earlier)
        } else {
            own
        };
        (up, land_episode_in(&books, Some(tab)).is_some())
    };
    let path = ledger_under(root, Some(tab));
    std::fs::write(&path, format!("{take_up}{}", approvals(t + 2_000))).expect("the tab's");
    assert!(std::fs::metadata(&path).unwrap().len() > MAX_BYTES);
    let before = books(&path);
    assert!(before.0 && before.1, "{before:?}");
    bound(&path);
    assert_eq!(books(&path), before, "the episode stands: the take-up kept");
    let over = row(
        t + 1_500,
        RULE_CONTINUE,
        Outcome::Typed,
        "keep going",
        &act_reason(None),
        tab,
    );
    std::fs::write(&path, format!("{take_up}{over}{}", approvals(t + 2_000))).expect("over");
    assert_eq!(books(&path), (true, false));
    bound(&path);
    assert_eq!(books(&path), (false, false), "the take-up cut");
    std::fs::write(&path, format!("{take_up}{}", approvals(t + 2_000))).expect("plain");
    crate::harness::cli::bound_ledger(&path, MAX_BYTES, KEEP_ROWS).expect("cut");
    assert_eq!(books(&path), (false, false), "CONTROL");
}

#[test]
fn the_books_through_the_earlier_tabs_are_theirs_re_signed_oldest_first() {
    let row = |ts: i64, sid: &str| {
        Row {
            rule_id: "allow-once@v1",
            outcome: Outcome::Approved,
            command: "ls",
            reason: "reads",
            box_seq: 1,
        }
        .to_json(ts, Some(sid))
    };
    let a = format!("{}\n{}\n", row(1, "s-a0"), row(2, "s-ff"));
    let b = format!("{}\n", row(5, "s-b0"));
    let own = format!("{}\n", row(9, "s-c0"));
    let earlier = [("s-b0".to_string(), b), ("s-a0".to_string(), a)];
    let books = books_through(&own, Some("@s-c0"), &earlier);
    assert_eq!(
        books.lines().collect::<Vec<_>>(),
        [row(1, "s-c0"), row(5, "s-c0"), row(9, "s-c0")]
    );
    let line = |t: i64, sid: &str| format!("{{\"t\":{t},\"sid\":\"{sid}\",\"kind\":\"waiting\"}}");
    let journal = books_through(
        "",
        Some("s-c0"),
        &[
            ("s-b0".to_string(), line(7, "s-b0")),
            ("s-a0".to_string(), line(3, "s-a0")),
        ],
    );
    assert_eq!(
        journal,
        format!("{}\n{}\n", line(3, "s-c0"), line(7, "s-c0"))
    );
    assert_eq!(books_through(&own, None, &earlier), own, "CONTROL");
}

#[test]
fn a_second_restore_reads_no_state_the_tab_between_ended_before_its_cut() {
    use super::super::policy::turn_end::{RULE_CONTINUE, RULE_LAND};
    let dir = crate::supervise::ScratchDir::new("apr-seed", "restored-chain");
    let root: &Path = &dir;
    let (a, b, c) = ("s-a0", "s-b1", "s-c2");
    std::fs::create_dir_all(root.join("drive")).expect("the drive");
    let row = |ts: i64, rule_id: &str, outcome: Outcome, command: &str, reason: &str, sid| {
        let row = Row {
            rule_id,
            outcome,
            command,
            reason,
            box_seq: 1,
        };
        format!("{}\n", row.to_json(ts, Some(sid)))
    };
    let t = 1_000_000_i64;
    std::fs::write(
        ledger_under(root, Some(a)),
        row(t, RULE_LAND, Outcome::Typed, "(land)", &act_reason(None), a),
    )
    .expect("A's books");
    crate::harness::upgrade_drive::note_earlier_tab_under(root, b, a);
    let filler = "cd /Users//someone/Developer/project && cargo test -p project-core --lib -- parser::tests 2>&1 | tail -40";
    let why = "the safe rules proved the command reads and runs tests";
    let take_up = row(
        t + 1_000,
        RULE_EARLIER_BOOKS,
        Outcome::Waiting,
        "",
        EARLIER_BOOKS,
        b,
    );
    let over = row(
        t + 1_500,
        RULE_CONTINUE,
        Outcome::Typed,
        "keep going",
        &act_reason(None),
        b,
    );
    let approvals: String = (0..3000)
        .map(|i| {
            row(
                t + 2_000 + i,
                "allow-once@v1",
                Outcome::Approved,
                filler,
                why,
                b,
            )
        })
        .collect();
    let books_of = |tab: &str| -> String {
        let path = ledger_under(root, Some(tab));
        let read = |p: &Path| std::fs::read_to_string(p).ok();
        let own = read(&path).unwrap_or_default();
        let earlier: Vec<(String, String)> =
            crate::harness::upgrade_drive::earlier_ledgers_in(&path, Some(tab), &read)
                .into_iter()
                .filter_map(|(t, p)| Some((t, read(&p)?)))
                .collect();
        if takes_up_earlier(&own, Some(tab)) && earlier.iter().any(|(t, b)| holds_rows_of(b, t)) {
            books_through(&own, Some(tab), &earlier)
        } else {
            own
        }
    };
    let landing = |tab: &str| land_episode_in(&books_of(tab), Some(tab)).is_some();
    let bp = ledger_under(root, Some(b));
    std::fs::write(&bp, format!("{take_up}{over}{approvals}")).expect("B's books");
    assert!(!landing(b), "B ended the episode");
    bound(&bp);
    assert!(
        !took_up_earlier(&std::fs::read_to_string(&bp).unwrap(), b),
        "B's take-up cut"
    );
    assert!(!landing(b), "B after its cut");
    crate::harness::upgrade_drive::note_earlier_tab_under(root, c, b);
    assert!(!landing(c), "C reads no episode B ended");
    let cont = dir.join("continuous.jsonl");
    let a_rows = std::fs::read_to_string(ledger_under(root, Some(a)))
        .unwrap()
        .replace("\"sid\":\"s-a0\"", "\"sid\":\"s-b1\"");
    std::fs::write(&cont, format!("{a_rows}{over}{approvals}")).unwrap();
    bound(&cont);
    assert!(
        land_episode(&cont, Some(b)).is_none(),
        "CONTROL: continuous"
    );
    std::fs::write(&bp, format!("{take_up}{approvals}")).expect("B's books, the episode open");
    bound(&bp);
    assert!(
        took_up_earlier(&std::fs::read_to_string(&bp).unwrap(), b),
        "CONTROL: kept"
    );
    assert!(landing(c), "CONTROL: through B");
}

#[test]
fn a_legacy_limits_next_try_never_wraps_its_recorded_count() {
    use super::super::policy::turn_end::RULE_LIMIT_RESUME;
    let row = |rule: &str, outcome: Outcome, reason: &str, ts| {
        Row {
            rule_id: rule,
            outcome,
            command: "keep going",
            reason,
            box_seq: 1,
        }
        .to_json(ts, Some("@s-9"))
    };
    let wall = row(
        RULE_WALL_WAIT,
        Outcome::Waiting,
        "the turn-end policy (wall: reset_unix=-; reset=-; message=You've hit your monthly spend limit.)",
        1_000,
    );
    let marked = |attempts| {
        row(
            RULE_LIMIT_RESUME,
            Outcome::Typed,
            &act_reason(Some(LimitTry {
                bucket: false,
                in_place: false,
                attempts,
                entitlement: true,
            })),
            2_000,
        )
    };
    let unmarked = row(
        RULE_LIMIT_RESUME,
        Outcome::Typed,
        "the turn-end policy",
        3_000,
    );
    let read = |body: &str| {
        usage_attempt_taken_in(body, Some("@s-9"), &|_, _| false).expect("the recorded try")
    };
    let ordinary = read(&format!("{wall}\n{}\n{unmarked}\n", marked(3)));
    assert_eq!((ordinary.at_ms, ordinary.limit.attempts), (3_000, 4));
    assert!(ordinary.limit.entitlement && !ordinary.limit.bucket);
    let ended = row(RULE_WALL_WAIT_END, Outcome::Waiting, LIMIT_ENDED, 2_500);
    let fresh = read(&format!("{wall}\n{}\n{ended}\n{unmarked}\n", marked(3)));
    assert_eq!(
        fresh.limit, USAGE_RESUME_UNMARKED,
        "an ended wall is a fresh try"
    );
    let body = format!("{wall}\n{}\n{unmarked}\n", marked(3));
    assert!(
        usage_attempt_in(&body, Some("@s-8")).is_none(),
        "another session"
    );
    let at_ceiling = read(&format!("{wall}\n{}\n{unmarked}\n", marked(usize::MAX - 1)));
    assert_eq!(
        at_ceiling.limit.attempts,
        usize::MAX,
        "the last exact increment"
    );
    let beyond = read(&format!("{wall}\n{}\n{unmarked}\n", marked(usize::MAX)));
    assert_eq!(
        (
            beyond.at_ms,
            beyond.limit.attempts,
            beyond.limit.entitlement
        ),
        (3_000, usize::MAX, true),
        "LEGACY_LIMIT_COUNT_MUST_NOT_WRAP"
    );
}

#[test]
fn a_cut_keeps_every_standing_row_when_its_standing_workspace_is_full() {
    use super::super::policy::turn_end::RULE_LAND;
    let mut rows = vec![String::new()];
    for sid in ["@s-9", "@s-10"] {
        rows.push(
            Row {
                rule_id: RULE_LAND,
                outcome: Outcome::Typed,
                command: "(land)",
                reason: &act_reason(None),
                box_seq: 1,
            }
            .to_json(1_000, Some(sid)),
        );
    }
    for i in 0..KEEP_ROWS + 8 {
        rows.push(
            Row {
                rule_id: "allow-once@v1",
                outcome: Outcome::Approved,
                command: "echo captured",
                reason: "a captured ordinary approval",
                box_seq: 1,
            }
            .to_json(i64::try_from(i).unwrap() + 2_000, Some("@s-9")),
        );
    }
    rows.push(String::new());
    let body = rows.join("\n") + "\n";
    let original: Vec<String> = body.lines().map(str::to_string).collect();
    let path = Path::new("/captured/self.approvals.jsonl");
    let read = |_: &Path| None;
    let cut_of =
        |body: &str, path: &Path, read: &dyn Fn(&Path) -> Option<String>, work: &mut CutWork| {
            cut_rows_with_work(path, &mut body.lines().map(str::to_string), read, work)
                .expect("held")
        };
    let held = |body: &str| -> Vec<String> {
        let rows: Vec<&str> = body.lines().filter(|l| !l.trim().is_empty()).collect();
        let from = rows.len() - KEEP_ROWS;
        rows.iter()
            .enumerate()
            .filter(|(i, l)| *i >= from || LandWalk::reads(l) || l.contains(RULE_EARLIER_BOOKS))
            .map(|(_, l)| (*l).to_string())
            .collect()
    };
    let cut = cut_of(&body, path, &read, &mut CutWork::for_cut());
    assert!(cut.len() < original.len(), "an ordinary cut still cuts");
    for sid in ["@s-9", "@s-10"] {
        assert_eq!(
            land_episode_in(&cut.join("\n"), Some(sid)),
            land_episode_in(&body, Some(sid)),
            "the standing episode survives"
        );
    }
    let earlier = (0..64)
        .map(|i| {
            Row {
                rule_id: "allow-once@v1",
                outcome: Outcome::Approved,
                command: "echo captured",
                reason: "an earlier ordinary approval",
                box_seq: 1,
            }
            .to_json(i, Some("@s-1"))
        })
        .collect::<Vec<_>>()
        .join("\n");
    let take_up = |sid: &str| {
        let mut body = Row {
            rule_id: RULE_EARLIER_BOOKS,
            outcome: Outcome::Waiting,
            command: "",
            reason: "the earlier tabs books are taken up",
            box_seq: 1,
        }
        .to_json(1_000, Some(sid));
        for row in rows.iter().skip(3) {
            body.push('\n');
            body.push_str(row);
        }
        body.push('\n');
        body
    };
    let read_earlier = |path: &Path| {
        if path.extension().is_some_and(|e| e == "earlier-tabs") {
            Some("s-1\n".to_string())
        } else if path.file_name().is_some_and(|n| n == "s-1.jsonl") {
            Some(earlier.clone())
        } else {
            None
        }
    };
    let allowance = || CutWork {
        entries: 65_536,
        visits: 1_048_576,
        bytes: earlier.len() * 3 + 4_096,
    };
    let under_drive = Path::new("/captured/drive/self.jsonl");
    let short = take_up("@s-2");
    assert!(
        cut_of(&short, under_drive, &read_earlier, &mut allowance()).len() < held(&short).len(),
        "ordinary predecessor rows still admit a cut"
    );
    let long = take_up(&format!("@s-{}", "f".repeat(120)));
    assert_eq!(
        cut_of(&long, under_drive, &read_earlier, &mut allowance()),
        held(&long),
        "PREDECESSOR_SID_EXPANSION_MUST_BE_ADMITTED_BEFORE_ASSEMBLY"
    );
    for mut work in [
        CutWork {
            entries: 1,
            visits: usize::MAX,
            bytes: usize::MAX,
        },
        CutWork {
            entries: usize::MAX,
            visits: 1,
            bytes: usize::MAX,
        },
        CutWork {
            entries: usize::MAX,
            visits: usize::MAX,
            bytes: 1,
        },
    ] {
        let kept = cut_of(&body, path, &read, &mut work);
        assert_eq!(
            kept,
            held(&body),
            "STANDING_WORKSPACE_EXCESS_MUST_PRESERVE_EVERY_STANDING_ROW"
        );
        for sid in ["@s-9", "@s-10"] {
            assert_eq!(
                land_episode_in(&kept.join("\n"), Some(sid)),
                land_episode_in(&body, Some(sid)),
                "the standing episode survives an unadmitted workspace"
            );
        }
    }
}
