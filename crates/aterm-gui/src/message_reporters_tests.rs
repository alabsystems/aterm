// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

/// THE LOW-DISK ENTRY POINTS AT THE HOGS IT DOES NOT SHOW: two of up to
/// five are named, and the second says how many more `aterm harness disk`
/// lists. NEGATIVE CONTROL: two hogs carry no count, and none add no line.
#[test]
fn the_low_disk_entry_counts_the_hogs_it_does_not_show() {
    let more = |hogs: &[&str]| {
        let hogs: Vec<String> = hogs.iter().map(|h| (*h).to_string()).collect();
        super::disk_low_entry(7, 20, &hogs)
            .message()
            .expect("a valid entry")
            .detail
    };
    let four = more(&[
        "~/a 9 GiB (a)",
        "~/b 8 GiB (b)",
        "~/c 7 GiB (c)",
        "~/d 6 GiB (d)",
    ]);
    let named: Vec<_> = four
        .iter()
        .filter(|l| l.contains("Largest it cannot reclaim:"))
        .collect();
    assert_eq!(named.len(), 2, "{four:?}");
    assert!(
        named[1].ends_with(&format!("~/b 8 GiB (b); 2{}", super::AND_MORE)),
        "{four:?}"
    );
    let two = more(&["~/a 9 GiB (a)", "~/b 8 GiB (b)"]);
    assert!(!two.join("\n").contains("aterm harness disk"), "{two:?}");
    let none = more(&[]);
    assert!(!none.join("\n").contains("aterm harness disk"), "{none:?}");
    assert!(!none.join("\n").contains("Largest"), "{none:?}");
}

/// THE DISK-SETTINGS ENTRY SHOWS WHAT WAS NOT READ, AND COUNTS WHAT IT
/// DOES NOT SHOW: up to three of the reading's notes are each a line;
/// past three, two are shown and the last line counts the rest. NEGATIVE
/// CONTROL: three notes carry no count.
#[test]
fn the_disk_settings_entry_shows_the_notes_and_counts_the_rest() {
    use aterm_messages::exceptions::MORE_LINES;
    let notes: Vec<String> = (1..=5).map(|i| format!("note {i}")).collect();
    let detail = |n: usize| {
        super::disk_settings_entry(&notes[..n])
            .message()
            .expect("a valid entry")
            .detail
    };
    let shown = |lines: &[String]| {
        notes
            .iter()
            .filter(|note| lines.iter().any(|l| l == *note))
            .count()
    };
    let three = detail(MORE_LINES);
    assert_eq!(shown(&three), MORE_LINES, "{three:?}");
    assert!(
        !three.iter().any(|l| l.ends_with(super::NOTES_MORE)),
        "{three:?}"
    );
    let five = detail(5);
    assert_eq!(shown(&five), MORE_LINES - 1, "{five:?}");
    let rest = 5 - (MORE_LINES - 1);
    assert!(
        five.iter()
            .any(|l| *l == format!("{rest}{}", super::NOTES_MORE)),
        "{five:?}"
    );
}

/// A VOLUME SHORT WHILE CLEANUP IS OFF IS NOT THE PASSES' LINE (review
/// 30): no pass ran, so its line has its own key, a reason and a detail
/// that share nothing with the passes' line at the same figures, and the
/// settings' action (the same button and words as `disk.settings`), where
/// the passes' line has no button. Both read as the same title at the
/// same severity. NEGATIVE CONTROL: the passes' line names its hogs, and
/// the cleanup-off line, which no search fed, has nothing past its figures.
#[test]
fn a_volume_short_while_cleanup_is_off_is_not_the_passes_line() {
    let (free, floor) = (3, 17);
    let hogs = ["~/a 9 GiB (a)".to_string()];
    let passes = super::disk_low_entry(free, floor, &hogs);
    let off = super::disk_low_cleanup_off_entry(free, floor);
    let settings = super::disk_settings_entry(&["disk.apply is not read".to_string()]);
    let (p_title, p_why, p_then, p_detail) = entry_words(&passes);
    let (o_title, o_why, o_then, o_detail) = entry_words(&off);
    let (_, _, s_then, _) = entry_words(&settings);

    assert_ne!(off.key(), passes.key());
    assert_eq!(off.key(), super::DISK_LOW_CLEANUP_OFF_KEY);
    assert_eq!(passes.key(), super::DISK_LOW_KEY);
    assert_eq!(o_title, p_title);
    assert_eq!(
        off.message().expect("valid").severity,
        passes.message().expect("valid").severity
    );
    assert_ne!(o_why, p_why);
    assert!(
        o_detail.iter().all(|line| !p_detail.contains(line)),
        "{o_detail:?} / {p_detail:?}"
    );
    assert!(passes.mover().is_none());
    assert_eq!(off.mover(), settings.mover());
    assert_ne!(o_then, p_then);
    assert_eq!(o_then, s_then);
    // The figures, and nothing a search would have found.
    assert_eq!(o_detail.len(), 1, "{o_detail:?}");
    assert!(
        [free, floor]
            .iter()
            .all(|n| o_detail[0].contains(&format!("{n} GiB"))),
        "{o_detail:?}"
    );
    assert!(
        p_detail.iter().any(|l| l.contains(&hogs[0])),
        "{p_detail:?}"
    );
    // The records part the same way.
    let (p_rec, o_rec) = (
        super::disk_low_record(free, floor, &[]),
        super::disk_low_cleanup_off_record(free, floor),
    );
    assert_eq!(o_rec.title, p_rec.title);
    assert_ne!(o_rec.detail, p_rec.detail);
}

/// What the digest reads of `entry`: its title, its reason, its action and
/// the detail lines behind them.
fn entry_words(entry: &Entry) -> (String, String, String, Vec<String>) {
    let msg = entry
        .message()
        .unwrap_or_else(|fault| panic!("{}: {fault:?}", entry.key()));
    let aterm_messages::need::Need::Later(ask) = msg.need() else {
        panic!("{}: an entry is a Later ask", entry.key());
    };
    let (why, then) = (ask.why_words(), ask.then().words().to_string());
    assert_eq!(msg.detail[..2], [why.clone(), then.clone()]);
    (msg.title.clone(), why, then, msg.detail[2..].to_vec())
}

fn signed_out(labels: &[&str]) -> Entry {
    let labels: Vec<String> = labels.iter().map(|l| (*l).to_string()).collect();
    accounts_signed_out_entry(&labels).expect("a label is an entry")
}

/// A LABEL CANNOT FAULT THE SIGNED-OUT ENTRY: a label is the owner's own
/// word, and the entry's lints read words — the reason for a stem anywhere
/// in it, the action for a snooze that leads it and for a second action.
/// Labels that are or hold those words build the same title and the same
/// reason as any other, and an action that ENDS in the label. NEGATIVE
/// CONTROL: each of those labels, written where its lint reads (into the
/// reason; leading the action; inside it), faults the entry.
#[test]
fn a_label_cannot_fault_the_signed_out_entry() {
    use aterm_messages::exceptions::EntryFault;
    #[derive(Clone, Copy, Debug)]
    enum Written {
        Reason,
        Leading,
        Inside,
    }
    let (title, why, _, _) = entry_words(&signed_out(&["main"]));
    for (label, where_it_faults) in [
        ("soon", Written::Reason),
        ("retry", Written::Reason),
        ("soon-1", Written::Reason),
        ("x.retrying", Written::Reason),
        ("self-heal", Written::Reason),
        ("0-automatically", Written::Reason),
        ("wait", Written::Leading),
        ("not", Written::Leading),
        ("or", Written::Inside),
    ] {
        let (t, w, then, more) = entry_words(&signed_out(&[label]));
        assert_eq!(
            (&t, &w),
            (&title, &why),
            "{label}: named outside the action"
        );
        assert_eq!(then, format!("Run aterm harness accounts login {label}"));
        assert_eq!(more, [format!("Signed out: {label}")]);
        // NEGATIVE CONTROL: the word is one the lints know.
        let (reason, step) = match where_it_faults {
            Written::Reason => (
                format!("{label} is signed out, and only a browser sign-in restores it"),
                then.clone(),
            ),
            Written::Leading => (why.clone(), format!("{label} in with aterm harness")),
            Written::Inside => (why.clone(), format!("Sign in {label} under Details")),
        };
        let fault = Entry::new(
            Family::Accounts,
            "",
            "signed-out",
            &title,
            Why::OnlyYou(Reason::new(reason)),
            Then::step(step),
        )
        .message()
        .err();
        assert!(
            matches!(fault, Some(EntryFault::Ask(_))),
            "{label} written {where_it_faults:?}: {fault:?}"
        );
    }
}

/// THE LOGIN ACTION NAMES A LABEL WHOLE OR NOT AT ALL: the longest label the
/// action's cap holds (57 characters) ends it; at one more the action names
/// none and points at the detail, where the label stands whole. Only the
/// FIRST label decides. NEGATIVE CONTROL: the 58-character label written
/// into the action is cut, which is another account's command.
#[test]
fn the_login_action_names_a_label_whole_or_not_at_all() {
    use aterm_agent::harness::accounts::LABEL_MAX;
    use aterm_messages::need::STEP_CAP;
    let fits = "a".repeat(57);
    let over = format!("{fits}b");
    let (_, _, then, _) = entry_words(&signed_out(&[&fits]));
    assert_eq!(then, format!("{ACCOUNTS_LOGIN_STEP} {fits}"));
    assert_eq!(then.chars().count(), STEP_CAP, "57 is the cap's own edge");

    for long in [over.clone(), "z".repeat(LABEL_MAX)] {
        let (_, _, then, more) = entry_words(&signed_out(&[&long, "main"]));
        assert!(then.starts_with(ACCOUNTS_LOGIN_STEP), "{then}");
        assert!(!then.contains(&fits) && !then.contains("zz"), "{then}");
        assert!(!then.ends_with('\u{2026}'), "{then}");
        assert_eq!(more, [format!("Signed out: {long}, main")]);
    }
    // A long label that is not the first is not the action's.
    let (_, _, then, _) = entry_words(&signed_out(&["main", &over]));
    assert_eq!(then, format!("{ACCOUNTS_LOGIN_STEP} main"));

    // NEGATIVE CONTROL: written anyway, it is cut.
    let cut = Then::step(format!("{ACCOUNTS_LOGIN_STEP} {over}"));
    assert!(cut.words().ends_with('\u{2026}') && !cut.words().contains(&over));
}

/// THE SIGNED-OUT ENTRY IS ONE LINE HOWEVER MANY ARE SIGNED OUT: the title
/// counts them, the action names the first, and the detail names them in
/// the order given on at most three lines, none of them cut, counting the
/// ones it has no room for. Thirty-two labels of ordinary length are all
/// named; thirty-two of the longest a roster admits name eight. The first
/// label reads back from the detail ([`accounts_signed_out_first`]).
/// NEGATIVE CONTROL: no label is no entry, one label is one line with no
/// count, and the refused entry's words name no account.
#[test]
fn the_signed_out_entry_counts_in_its_title_and_names_in_its_detail() {
    use aterm_agent::harness::accounts::{LABEL_MAX, MAX_ACCOUNTS};
    // The labels the detail names, and the count it ends on.
    let named = |more: &[String]| -> (Vec<String>, usize) {
        let text = more.join(", ");
        let text = text.strip_prefix("Signed out: ").expect("the lead");
        let (labels, rest) = match text.rsplit_once(" and ") {
            Some((labels, tail)) => {
                let n = tail.strip_suffix(" more").expect("a count");
                (labels, n.parse().expect("a number"))
            }
            None => (text, 0),
        };
        (labels.split(", ").map(str::to_string).collect(), rest)
    };

    assert!(accounts_signed_out_entry(&[]).is_none());
    let one = signed_out(&["work-2"]);
    assert_eq!(one.key(), ACCOUNTS_SIGNED_OUT_KEY);
    assert_eq!(one.log_line_text(), Some("accounts need=signed-out n=1"));
    let (title, _, then, more) = entry_words(&one);
    assert_eq!(title, "A Claude account is signed out");
    assert_eq!(then, "Run aterm harness accounts login work-2");
    assert_eq!(more, ["Signed out: work-2"]);

    let three = signed_out(&["main", "work-2", "pool-a"]);
    assert_eq!(
        three.key(),
        ACCOUNTS_SIGNED_OUT_KEY,
        "the fact, not the set"
    );
    let (title, _, then, more) = entry_words(&three);
    assert_eq!(title, "3 Claude accounts are signed out");
    assert_eq!(then, "Run aterm harness accounts login main");
    assert_eq!(more, ["Signed out: main, work-2, pool-a"]);
    let detail = three.message().expect("an entry that passes").detail;
    assert_eq!(accounts_signed_out_first(&detail), Some("main"));
    let detail = one.message().expect("an entry that passes").detail;
    assert_eq!(accounts_signed_out_first(&detail), Some("work-2"));
    // NEGATIVE CONTROL: another entry's words name no account.
    let detail = accounts_refused_entry().message().expect("passes").detail;
    assert_eq!(accounts_signed_out_first(&detail), None);
    assert_eq!(accounts_signed_out_first(&detail[..2]), None);

    let ordinary: Vec<String> = (0..MAX_ACCOUNTS).map(|i| format!("work-{i:02}")).collect();
    let longest: Vec<String> = (0..MAX_ACCOUNTS)
        .map(|i| format!("{i:02}{}", "x".repeat(LABEL_MAX - 2)))
        .collect();
    for (labels, names) in [(&ordinary, MAX_ACCOUNTS), (&longest, 8)] {
        let entry = accounts_signed_out_entry(labels).expect("an entry");
        assert_eq!(entry.log_line_text(), Some("accounts need=signed-out n=32"));
        let detail = entry.message().expect("an entry that passes").detail;
        assert_eq!(
            accounts_signed_out_first(&detail),
            Some(labels[0].as_str()),
            "the account the action is for reads back, named in it or not"
        );
        let (title, _, _, more) = entry_words(&entry);
        assert_eq!(title, "32 Claude accounts are signed out");
        assert_eq!(more, accounts_signed_out_lines(labels), "no line is cut");
        assert!(more.len() <= aterm_messages::exceptions::MORE_LINES);
        let (said, rest) = named(&more);
        assert_eq!(said, labels[..names], "in the order given, each whole");
        assert_eq!(rest, MAX_ACCOUNTS - names);
    }
}

/// THE ACCOUNTS' TWO LINES ARE DIGEST LINES AND NOTHING LOUDER: each is an
/// `Info` entry under its own key of `harness.accounts.`, with no button
/// (its action is a command the owner runs), dismissed for a wait's span,
/// and with the one line a developer greps. The refused one says nothing
/// of why: that reason names the file's path. NEGATIVE CONTROL: the low-disk
/// entry is `Warn`, the severity that marks the batch a Problem.
#[test]
fn the_accounts_entries_are_info_lines_under_their_own_keys() {
    let refused = accounts_refused_entry();
    assert_eq!(refused.key(), ACCOUNTS_REFUSED_KEY);
    assert_eq!(
        refused.log_line_text(),
        Some("accounts need=roster-refused")
    );
    let (title, why, then, more) = entry_words(&refused);
    assert_eq!(title, "The Claude accounts list is refused");
    assert!(why.starts_with("accounts.toml is refused"), "{why}");
    assert_eq!(then, "Run aterm harness accounts to read why it is refused");
    assert!(more.is_empty(), "{more:?}");

    let signed_out = signed_out(&["main", "work-2"]);
    assert_ne!(signed_out.key(), refused.key());
    for entry in [&refused, &signed_out] {
        assert!(entry.key().starts_with(&Family::Accounts.prefix()));
        assert!(entry.mover().is_none());
        let msg = entry.message().expect("a valid entry");
        assert_eq!(msg.severity, Severity::Info, "{}", entry.key());
        assert!(msg.actions.is_empty(), "{:?}", msg.actions);
        let aterm_messages::need::Need::Later(ask) = msg.need() else {
            panic!("{}: an entry is a Later ask", entry.key());
        };
        assert_eq!(ask.resurface(), Resurface::WAIT);
    }
    // NEGATIVE CONTROL.
    let disk = disk_low_entry(7, 20, &[]).message().expect("a valid entry");
    assert_eq!(disk.severity, Severity::Warn);
}

/// A SUPERVISOR'S NOTE KEEPS ITS COMMAND (the review of 2026-09-30): a
/// frozen agent's warning, one line, was cut at the title's width before
/// the command that ends it. Its first line is the title now, and each
/// further line a detail line, whole. NEGATIVE CONTROL: a one-line note
/// is the title alone, as before.
#[test]
fn a_supervisor_note_puts_its_state_in_the_title_and_the_rest_below() {
    let sid = "s-b5cf2faabac5ce5127bd";
    let act = format!(
        "aterm leaves it running \u{2014} `aterm ctl @{sid} signal term` ends it (`signal \
         kill` if it survives), and aterm relaunches it on its conversation"
    );
    let text =
        format!("claude frozen: not reading input for 11m00s (1 B queued, rss 38.8 GB)\n{act}");
    let msg = super::supervisor_note(sid, "in tab 2", &text);
    assert_eq!(
        msg.title,
        "Tab 2: claude frozen: not reading input for 11m00s (1 B queued, rss 38.8 GB)"
    );
    assert!(msg.title.chars().count() <= aterm_messages::TITLE_CAP);
    assert_eq!(msg.detail, vec![act]);
    let one = super::supervisor_note(sid, "in tab 2", "codex holds on gpt-6 until 3pm");
    // The tab by its place where it fits, never the raw session id.
    assert_eq!(one.title, "Tab 2: codex holds on gpt-6 until 3pm");
    // A session no tab shows leads with nothing.
    let none = super::supervisor_note(sid, "in its tab", "codex holds on gpt-6 until 3pm");
    assert_eq!(none.title, "codex holds on gpt-6 until 3pm");
    assert!(one.detail.is_empty(), "{:?}", one.detail);
}

/// THE STALLED ROW NAMES THE PROCESSES THAT HOLD THE MOVE (2026-09-27: the
/// agent's notice named the shells under it, and the owner's row said only
/// `its own work runs` — the pids reached the owner in the give-up's
/// ledger row alone, after four notices). A line of its own, after the
/// tab and the move ([`super::STALL_MOVE_LINE`] stays where the host's
/// dedupe reads it) and before the shell spelling, still the last,
/// technical line (ruling 270): by pid, name and age — never a command,
/// the agent's own words — within the detail line's cap. NEGATIVE
/// CONTROL: nothing held, no line.
#[test]
fn a_stalled_upgrade_row_names_the_processes_that_hold_it() {
    use aterm_agent::harness::upgrade::Phase;
    use aterm_agent::harness::upgrade_drive::{HeldBy, Row};
    const NOW: u64 = 1_790_311_076;
    let shell = |pid: u32| HeldBy {
        pid,
        name: "zsh".into(),
        since: NOW - (5 * 86_400 + 4 * 3_600),
    };
    let row = Row {
        tab: "s-b5cf2faabac5ce5127bd".into(),
        from: "2.1.281".into(),
        to: "2.1.282".into(),
        phase: Phase::Announced {
            at_s: NOW - 3_600,
            asks: 2,
        },
        behind_since: NOW - 7 * 3_600,
        wait: "background".into(),
        held_by: vec![shell(63_492), shell(63_493)],
        ..Row::default()
    };
    let msg = super::agent_upgrade_stalled(&row, NOW, "in tab 2");
    assert_eq!(msg.detail.len(), 5, "{:?}", msg.detail);
    assert_eq!(
        msg.detail[3],
        "held by pid 63492 (zsh, 5d4h); pid 63493 (zsh, 5d4h)"
    );
    assert!(
        msg.detail[super::STALL_MOVE_LINE].starts_with("in tab 2 \u{00b7} "),
        "{:?}",
        msg.detail
    );
    assert!(
        msg.detail[4].starts_with("the same in any shell: "),
        "{:?}",
        msg.detail
    );
    let many = Row {
        held_by: (1..=9).map(|p| shell(4_000_000 + p)).collect(),
        ..row.clone()
    };
    let msg = super::agent_upgrade_stalled(&many, NOW, "in tab 2");
    assert!(msg.detail[3].ends_with("; and 4 more"), "{:?}", msg.detail);
    assert!(msg.detail[3].chars().count() <= aterm_messages::DETAIL_LINE_CAP);
    let free = Row {
        held_by: Vec::new(),
        ..row
    };
    let msg = super::agent_upgrade_stalled(&free, NOW, "in tab 2");
    assert_eq!(msg.detail.len(), 4, "{:?}", msg.detail);
    assert!(msg.detail.iter().all(|l| !l.starts_with("held by")));
}

/// ONLY AN INTERRUPT STANDS ON THE BAND (the owner, 2026-09-28: "there needs
/// to be some clear reason why the user must be engaged and what the user
/// should do ... don't continue to persist alerts to the user that are not
/// actionable"; the second reviews of 2026-09-29: this claim was type-checked
/// and never observed). For a roster of rows that covers every class the
/// classifier has, the reporter's entry is a standing warn row exactly where
/// [`Row::interrupt`] names a need, says that need's words, and never says
/// `Couldn't` of a wait; every other row is a record (`LogOnly`), or the
/// answer to the owner's own `Upgrade now` for [`ANSWER_MAX`]. The pure half
/// of the pre-landing drive (`plan-upgrade` 12.0): the host's half is
/// driven against a private instance.
#[test]
fn only_an_interrupt_stands_on_the_band() {
    use aterm_agent::harness::upgrade::{Agent, Phase, Request};
    use aterm_agent::harness::upgrade_drive::{Attention, HeldBy, Row};
    const NOW: u64 = 1_790_311_076;
    const DAY: u64 = 86_400;
    let held = |n: u32| -> Vec<HeldBy> {
        (0..n)
            .map(|i| HeldBy {
                pid: 4_000 + i,
                name: "zsh".into(),
                since: NOW - 3_600,
            })
            .collect()
    };
    let base = |agent: Agent| Row {
        session: "07422bc3-1be9-4875-9672-2eaf7b28aa21".into(),
        tab: "s-3e79b5bde172ad0088ad".into(),
        from: "2.1.283".into(),
        to: "2.1.284".into(),
        source: "managed".into(),
        agent,
        behind_since: NOW - 3 * DAY,
        minted_at: NOW - 3 * DAY,
        ..Row::default()
    };
    let mut roster: Vec<(&str, Row)> = Vec::new();
    for agent in [Agent::Claude, Agent::Codex] {
        let b = base(agent);
        roster.push((
            "a pending round nothing has visited",
            Row {
                phase: Phase::Pending,
                ..b.clone()
            },
        ));
        for wait in [
            "settling",
            "attended",
            "not-idle:busy",
            "background",
            "awaiting-ready",
            "announce-refused:changed",
            "announce-refused:held",
            "draft",
            "box",
            "limited",
            "login",
            "queued",
        ] {
            for age in [3_600, 30 * 3_600, 9 * DAY] {
                roster.push((
                    wait,
                    Row {
                        phase: Phase::Announced {
                            at_s: NOW - 60,
                            asks: 2,
                        },
                        wait: wait.into(),
                        wait_since: NOW - age,
                        behind_since: NOW - age,
                        minted_at: NOW - age,
                        held_by: held(16),
                        own_work_at: NOW - 60,
                        ..b.clone()
                    },
                ));
            }
        }
        roster.push((
            "a round that gave up and rests",
            Row {
                phase: Phase::Failed("unanswered".into()),
                streak_why: "unanswered".into(),
                stop_streak: 2,
                wait: "failed".into(),
                held_by: held(16),
                own_work_at: NOW - 60,
                ..b.clone()
            },
        ));
        roster.push((
            "a refusal",
            Row {
                phase: Phase::Failed("not-a-shell-job".into()),
                ..b.clone()
            },
        ));
        roster.push((
            "a suspended agent",
            Row {
                phase: Phase::Pending,
                wait: "not-foreground".into(),
                wait_since: NOW - 2 * 3_600,
                ..b.clone()
            },
        ));
        // The interrupts.
        roster.push((
            "the agent is gone",
            Row {
                phase: Phase::Failed("stale-exit".into()),
                exited_at: NOW - 600,
                ..b.clone()
            },
        ));
        roster.push((
            "a shutdown that hung",
            Row {
                phase: Phase::Exiting {
                    at_s: NOW - 20 * 60,
                },
                ..b.clone()
            },
        ));
        roster.push((
            "a relaunch that never reached a prompt",
            Row {
                phase: Phase::Relaunched {
                    at_s: NOW - 40 * 60,
                },
                exited_at: NOW - 41 * 60,
                ..b.clone()
            },
        ));
    }
    let mut standing = 0;
    for (what, row) in &roster {
        let msg = super::agent_upgrade_stalled(row, NOW, "in tab 2");
        let need = row.interrupt(NOW);
        let is_row = matches!(msg.hold, Hold::STANDING);
        assert_eq!(
            is_row,
            need.is_some(),
            "{what} ({:?}, {:?}): {:?} / {:?}",
            row.agent,
            row.wait,
            msg.hold,
            row.attention(NOW).level()
        );
        if let Some(need) = need {
            standing += 1;
            assert_eq!(msg.severity, Severity::Warn, "{what}");
            assert_eq!(msg.detail[0], need.why, "{what}");
            assert_eq!(msg.detail[1], need.then, "{what}");
            assert!(!msg.title.starts_with("Couldn't"), "{what}: {}", msg.title);
        } else {
            assert!(
                matches!(msg.hold, Hold::LogOnly),
                "{what}: a wait is a record: {:?}",
                msg.hold
            );
            assert!(
                !msg.title.starts_with("Couldn't upgrade") || row.phase != Phase::Pending,
                "{what}: {}",
                msg.title
            );
            assert!(
                !matches!(row.attention(NOW), Attention::Interrupt(_)),
                "{what}"
            );
        }
    }
    assert!(standing >= 6, "the roster holds the interrupts: {standing}");
    // The answer to the owner's own press: calm, for two minutes, no action.
    let asked = Row {
        phase: Phase::Announced {
            at_s: NOW - 60,
            asks: 2,
        },
        wait: "background".into(),
        wait_since: NOW - 3_600,
        request: Request::Now,
        held_by: held(16),
        own_work_at: NOW - 60,
        ..base(Agent::Claude)
    };
    let msg = super::agent_upgrade_stalled(&asked, NOW, "in tab 2");
    assert_eq!(msg.severity, Severity::Info);
    assert_eq!(msg.hold, Hold::For(ANSWER_MAX));
    assert_eq!(ANSWER_MAX, Duration::from_secs(120));
    assert!(msg.actions.is_empty(), "a decline is not an action");
}

/// "ASKED AGAIN EVERY 30 MINUTES" IS NEVER A FIXED PHRASE (design record
/// 2026-09-28, §1.4): tab #1's row said it for 28 hours in which nothing
/// asked. The words come from the record: when the latest notice was
/// typed, with its day once that is not today; while the re-ask is not
/// yet due, the time it becomes due, taken at the next break or idle
/// point; once due, only that the next point asks — never an interval;
/// and on the last notice, that the next point gives up. What would have
/// caught the old phrase: every form below says when it was last asked
/// and none says "every".
#[test]
fn the_asked_again_words_come_from_the_record_never_a_cadence() {
    use aterm_agent::harness::upgrade::{MAX_ASKS, REASK_S};
    // 2026-09-27 17:48:00 UTC; the zone at UTC and at PDT (-7 h).
    const AT: u64 = 1_790_531_280;
    let words = |noticed: u64, clock: u64, now: u64, offset: Option<i64>| {
        super::asked_again_words(noticed, clock, 2, now, offset)
    };
    assert_eq!(
        words(AT, AT, AT + 600, Some(0)),
        ", last asked 5:48 PM, asked again at its next break or idle point from 6:18 PM"
    );
    assert_eq!(
        words(AT, AT, AT + 600, Some(-7 * 3_600)),
        ", last asked 10:48 AM, asked again at its next break or idle point from 11:18 AM"
    );
    // Due: no promise of when.
    assert_eq!(
        words(AT, AT, AT + REASK_S, Some(0)),
        ", last asked 5:48 PM; asks again at its next break or idle point"
    );
    // A DAY ON (review of 2026-09-28): tab #1's notice, read 28 hours
    // later, is yesterday's — a bare `5:48 PM` under today's header read
    // as this afternoon. Further back, the day as a header says it.
    assert_eq!(
        words(AT, AT, AT + 28 * 3_600, Some(0)),
        ", last asked yesterday 5:48 PM; asks again at its next break or idle point"
    );
    assert!(
        words(AT, AT, AT + 3 * 86_400, Some(0)).starts_with(", last asked Sunday 5:48 PM;"),
        "{}",
        words(AT, AT, AT + 3 * 86_400, Some(0))
    );
    assert!(
        words(AT, AT, AT + 30 * 86_400, Some(0)).starts_with(", last asked Sun, 27 Sep, 5:48 PM;"),
        "{}",
        words(AT, AT, AT + 30 * 86_400, Some(0))
    );
    // No zone: said as ages, in the why line's units (`behind for 28 h`),
    // never the roster's `1d4h`.
    assert_eq!(
        words(AT, AT, AT + 600, None),
        ", last asked 10 min ago, asked again at its next break or idle point in 20 min or \
         later"
    );
    assert_eq!(
        words(AT, AT, AT + 28 * 3_600, None),
        ", last asked 28 h ago; asks again at its next break or idle point"
    );
    assert_eq!(
        words(AT, AT, AT + 3 * 86_400 + 600, None),
        ", last asked 3 days ago; asks again at its next break or idle point"
    );
    // Typed seconds ago: `just now`, never `1 min ago`.
    assert_eq!(
        words(AT, AT, AT + 5, None),
        ", last asked just now, asked again at its next break or idle point in 29 min or \
         later"
    );
    // Due within the minute: said as due, never `in 1 min or later`,
    // which would put it off.
    assert_eq!(
        words(AT, AT, AT + REASK_S - 30, None),
        ", last asked 29 min ago; asks again at its next break or idle point"
    );
    // An unknown notice time names none.
    assert_eq!(
        words(0, 0, AT, Some(0)),
        ", asked again at its next break or idle point"
    );
    // A LIMIT'S EPISODE MOVES THE CLOCK, NOT THE NOTICE (review of
    // 2026-09-28): typed at 17:48, the limit's episode closed at 21:58
    // and held the clock to then. The row names the notice's time, and
    // the re-ask's due time from the held clock.
    let held = AT + 4 * 3_600 + 600;
    assert_eq!(
        words(AT, held, held + 60, Some(0)),
        ", last asked 5:48 PM, asked again at its next break or idle point from 10:28 PM"
    );
    // THE LAST NOTICE (same review): the step past its clock is the
    // give-up, never a fifth notice.
    assert_eq!(
        super::asked_again_words(AT, AT, MAX_ASKS, AT + 600, Some(0)),
        ", last asked 5:48 PM (the last of 4 notices); gives up at its next break or \
         idle point from 6:18 PM"
    );
    assert_eq!(
        super::asked_again_words(AT, AT, MAX_ASKS, AT + REASK_S, Some(0)),
        ", last asked 5:48 PM (the last of 4 notices); gives up at its next break or \
         idle point"
    );
    for now in [AT + 600, AT + REASK_S, AT + 28 * 3_600] {
        for offset in [Some(0), None] {
            for asks in [1, MAX_ASKS] {
                let w = super::asked_again_words(AT, AT, asks, now, offset);
                assert!(!w.contains("every"), "{w}");
                assert!(w.contains("last asked"), "{w}");
                assert_eq!(w.contains("gives up"), asks == MAX_ASKS, "{w}");
            }
        }
    }
}

/// A LIMIT PAST ITS RESET IS NEVER TOLD TO BE WAITED OUT (the review of
/// 2026-09-29): the row's first line says the reset passed, and its
/// remedy line said "it moves once that ends" — the wait it named had
/// already ended, and the notice left in the tab holds every point. It
/// now names the one act that moves it: continuing the session in its
/// tab. NEGATIVE CONTROL: a limit whose reset is still ahead is not told
/// that (ruling 307: it ends by itself).
#[test]
fn a_limit_past_its_reset_names_continuing_the_session_never_waiting() {
    use aterm_agent::harness::upgrade::Phase;
    use aterm_agent::harness::upgrade_drive::Row;
    const NOW: u64 = 1_790_311_076;
    let row = |limit_until: u64| Row {
        tab: "s-b5cf2faabac5ce5127bd".into(),
        from: "2.1.281".into(),
        to: "2.1.282".into(),
        phase: Phase::Pending,
        wait: "limited".into(),
        limit_until,
        behind_since: NOW - 30 * 3_600,
        noticed_at: NOW - 30 * 3_600,
        ..Row::default()
    };
    let passed = row(NOW - 10 * 3_600);
    assert!(passed.limit_passed(NOW), "the fixture's reset has passed");
    let msg = super::agent_upgrade_stalled(&passed, NOW, "in tab 2");
    // The second line is the remedy's; the first says what holds it.
    let remedy = &msg.detail[1];
    assert!(
        remedy.contains("type in its tab to continue the session"),
        "{:?}",
        msg.detail
    );
    assert!(!remedy.contains("once that ends"), "{:?}", msg.detail);
    // The first line says the reset passed; the remedy does not say it again.
    assert!(msg.detail[0].contains("reset passed"), "{:?}", msg.detail);
    assert!(!remedy.contains("reset"), "{:?}", msg.detail);
    assert!(remedy.chars().count() <= aterm_messages::DETAIL_LINE_CAP);

    let ahead = row(NOW + 3_600);
    assert!(!ahead.limit_passed(NOW));
    let msg = super::agent_upgrade_stalled(&ahead, NOW, "in tab 2");
    assert!(
        msg.detail
            .iter()
            .all(|l| !l.contains("type in its tab to continue the session")),
        "{:?}",
        msg.detail
    );
}

/// THE STALLED ROW NAMES THE REMEDY THAT MOVES ITS KIND OF STALL (review
/// of 2026-09-25: `--now` was named for all five kinds, and it moves two —
/// a held-back agent is never reached, a refused or failed one stays
/// stopped). Each wording pinned, and `--now` named only where it works.
#[test]
fn a_stalled_upgrade_row_names_the_remedy_for_its_kind() {
    use aterm_agent::harness::upgrade::Phase;
    use aterm_agent::harness::upgrade_drive::Row;
    const NOW: u64 = 1_790_311_076;
    let tab = "s-b5cf2faabac5ce5127bd";
    let base = Row {
        tab: tab.into(),
        from: "2.1.281".into(),
        to: "2.1.282".into(),
        phase: Phase::Pending,
        behind_since: NOW - 60,
        noticed_at: NOW - 3_600,
        ..Row::default()
    };
    let remedy = |phase: Phase, wait: &str, behind: u64| {
        // Round 18 (D2/D4): a move still waiting says so; one that
        // stopped or cannot be reached could not be done. Ruling 423 (the
        // owner, 2026-09-28: `Couldn't upgrade Claude in tab 1 yet` read
        // as a failure with nothing to do): a wait is a RECORD titled as
        // one, and only what did not work says `Couldn't`.
        let waits = !matches!(phase, Phase::Failed(_)) && !wait.starts_with("terminal:");
        // Ruling 283: a round that stopped once (not refused) or gave
        // up asks again on its own — it retries later.
        let resting = matches!(&phase, Phase::Failed(why)
            if why != "not-a-shell-job" && !why.starts_with("argv:"));
        let msg = super::agent_upgrade_stalled(
            &Row {
                phase,
                wait: wait.into(),
                behind_since: NOW - behind,
                ..base.clone()
            },
            NOW,
            "in tab 2",
        );
        assert_eq!(
            msg.title,
            if resting {
                "Claude upgrade retries later in tab 2"
            } else if waits {
                "Claude upgrade waits in tab 2"
            } else {
                "Couldn't upgrade Claude in tab 2"
            },
            "{wait}"
        );
        assert_eq!(msg.detail.len(), 4, "{:?}", msg.detail);
        // Ruling 270: the tab in the person's words, the raw sid only on
        // the last, technical line.
        assert!(
            msg.detail[2].starts_with("in tab 2 \u{00b7} "),
            "{:?}",
            msg.detail
        );
        assert!(
            msg.detail[..3]
                .iter()
                .all(|l| !l.contains(tab) && !l.contains('`') || l.contains("codex resume")),
            "{:?}",
            msg.detail
        );
        (msg.detail[1].clone(), msg.detail[3].clone())
    };
    let shell =
        |words: &str| format!("the same in any shell: `aterm harness upgrade {tab} {words}`");
    // Overdue on a turn still running (ruling 380): hours on the ladder's
    // last rung, which `--now` is too — it is not named as moving it, and
    // never "at its next turn end" (a goal-mode session's turns never
    // end: the owner's tab, 2026-09-28).
    assert_eq!(
        remedy(Phase::Pending, "not-idle:busy", 7 * 3_600),
        (
            "Upgrade now does not move it past that wait: it moves once that ends; Skip \
             version keeps it on 2.1.281"
                .to_string(),
            shell("--skip")
        ),
        "overdue: the row's own buttons, the shell's spelling last"
    );
    // What `--now` still moves: the lane's own text left typed, tried
    // again at once.
    assert_eq!(
        remedy(Phase::Pending, "left-typed-backoff", 7 * 3_600),
        (
            "Upgrade now tries it again at its next pause; Skip This Version in the tab's \
             menu keeps it on 2.1.281"
                .to_string(),
            shell("--now` or `--skip")
        ),
        "overdue, left typed"
    );
    assert_eq!(
        remedy(Phase::Failed("unanswered".into()), "", 60),
        (
            "Upgrade now asks it again at its next pause; Skip This Version in the tab's \
             menu keeps it on 2.1.281"
                .to_string(),
            shell("--now` or `--skip")
        ),
        "gave up"
    );
    // A wait no rung passes (ruling 380, `blocked:*`): what is wrong, said
    // plainly, and the one way it moves — having blocked the move a
    // re-ask's interval, whatever other wait the last look found.
    let blocked = super::agent_upgrade_stalled(
        &Row {
            wait: "daemon-turn".into(),
            wait_since: NOW - 60,
            blocked: "no-shell-integration".into(),
            blocked_since: NOW - 1_800,
            agent: aterm_agent::harness::upgrade::Agent::Codex,
            ..base.clone()
        },
        NOW,
        "in tab 2",
    );
    assert_eq!(blocked.title, "Couldn't upgrade Codex in tab 2");
    // The owner's own shell setup: a digest line, a record until it exists.
    assert_eq!(
        (blocked.severity, blocked.hold),
        (Severity::Info, Hold::LogOnly)
    );
    assert!(
        blocked.detail[0].starts_with(
            "its tab's shell integration is not reaching aterm (as after an aterm update)"
        ),
        "{:?}",
        blocked.detail
    );
    assert_eq!(
        blocked.detail[1],
        "to move it now, quit Codex in its tab and resume it with the `codex resume` line it \
         prints; Skip version keeps it on 2.1.281"
    );
    let unreadable = super::agent_upgrade_stalled(
        &Row {
            wait: "screen-unreadable".into(),
            wait_since: NOW - 1_800,
            blocked: "screen-unreadable".into(),
            blocked_since: NOW - 1_800,
            agent: aterm_agent::harness::upgrade::Agent::Codex,
            ..base.clone()
        },
        NOW,
        "in tab 2",
    );
    assert_eq!(
        unreadable.detail[1],
        "to move it now, quit it in its tab and resume it there; Skip version keeps it on \
         2.1.281"
    );
    // The agent's own work under it (2026-09-26: a tab behind poll loops
    // that could never end read "it moves once that ends"). `--now` is not
    // named as moving it. The agent stopping the work does. When it was
    // last asked is the record's, and no cadence is promised (design
    // record 2026-09-28: "asked again every 30 minutes" stood over a tab
    // nothing had asked for 28 hours).
    for wait in ["background", "not-idle:shell"] {
        let working = remedy(
            Phase::Announced {
                at_s: NOW - 3_600,
                asks: 2,
            },
            wait,
            7 * 3_600,
        );
        let asked = super::asked_again_words(
            NOW - 3_600,
            NOW - 3_600,
            2,
            NOW,
            crate::presence::local_offset_s(),
        );
        assert!(
            asked.ends_with("; asks again at its next break or idle point"),
            "{asked}"
        );
        assert_eq!(
            working,
            (
                format!(
                    "Upgrade now cannot move it past its own work: it moves once that ends \
                     or the agent stops it{asked}; Skip version keeps it on 2.1.281"
                ),
                shell("--skip")
            ),
            "{wait}"
        );
        assert!(working.0.chars().count() <= aterm_messages::DETAIL_LINE_CAP);
        assert!(!working.0.contains("now moves"), "{working:?}");
        // The old fixed phrase is gone.
        assert!(!working.0.contains("every 30 minutes"), "{working:?}");
    }
    // Before any notice, nothing is said of asking again.
    let (pending, _) = remedy(Phase::Pending, "not-idle:shell", 7 * 3_600);
    assert!(
        pending.contains("its own work") && !pending.contains("asked again"),
        "{pending}"
    );
    // UNDER THE QUIET FLOW (`Row::quiet`, the review of 2026-09-30)
    // nothing asks an announced round again and nothing gives it up: no
    // re-ask and no give-up is promised of it. NEGATIVE CONTROL: the ask
    // flow's same row, above, promises the re-ask.
    let quiet = super::agent_upgrade_stalled(
        &Row {
            phase: Phase::Announced {
                at_s: NOW - 3_600,
                asks: 2,
            },
            wait: "background".into(),
            behind_since: NOW - 7 * 3_600,
            quiet: true,
            ..base.clone()
        },
        NOW,
        "in tab 2",
    );
    let said = quiet.detail.join("\n");
    assert!(said.contains("cannot move it past its own work"), "{said}");
    assert!(
        !said.contains("asks again") && !said.contains("gives up"),
        "{said}"
    );
    // A SPEND OR CREDIT WALL naming no reset (`Row::spend_held`, the
    // review of 2026-09-30): the owner's to lift, named with what lifts
    // it. NEGATIVE CONTROL: a usage limit's same row moves once it ends.
    let walled = |spend: bool| {
        super::agent_upgrade_stalled(
            &Row {
                wait: "limited".into(),
                wait_since: NOW - 7 * 3_600,
                limit_spend: spend,
                behind_since: NOW - 7 * 3_600,
                ..base.clone()
            },
            NOW,
            "in tab 2",
        )
        .detail
        .join("\n")
    };
    let spend = walled(true);
    assert!(spend.contains("add credits or raise the cap"), "{spend}");
    assert!(!spend.contains("once that ends"), "{spend}");
    let usage = walled(false);
    assert!(usage.contains("it moves once that ends"), "{usage}");
    assert!(!usage.contains("credits"), "{usage}");
    let pane = remedy(Phase::Pending, "terminal:tmux", 60);
    assert_eq!(
        pane,
        (
            "to move it, quit it in its tmux pane and resume it there; Skip version keeps \
             it on 2.1.281"
                .to_string(),
            shell("--skip")
        ),
        "held back"
    );
    for why in [
        "not-a-shell-job",
        "argv:--print",
        "no-resume",
        "resumed-elsewhere",
    ] {
        let by_hand = remedy(Phase::Failed(why.into()), "", 60);
        assert_eq!(
            by_hand,
            (
                "the upgrade asks it again after a rest; to move it sooner, quit it and \
                 resume it by hand; Skip version keeps it on 2.1.281"
                    .to_string(),
                shell("--skip")
            ),
            "{why}"
        );
    }
    assert!(
        !pane.0.contains("Upgrade now") && !pane.1.contains("--now"),
        "{pane:?}"
    );
}

/// RULING 283, THE AUDIT'S REAL CASE: a round that gave up, is resting,
/// and is more than six hours behind reads `overdue` while its step waits
/// `failed`. It asks again on its own, so it is a RECORD with no tab mark,
/// it offers `Upgrade now` (which re-arms it at once), and its words are
/// true: no "cannot move it", no bare `behind for 8h`, the next round's
/// time where the clock is known. NEGATIVE CONTROLS: the same stop
/// repeating, and a refusal, are records too (one digest line each: the
/// owner's rule of 2026-09-28 leaves only an agent the upgrade ended a
/// row) - and keep their own words.
#[test]
fn a_resting_round_that_gave_up_is_a_record_that_offers_upgrade_now() {
    use aterm_agent::harness::upgrade::Phase;
    use aterm_agent::harness::upgrade_drive::{Remedy, Row};
    use aterm_messages::UpgradeWord;
    const NOW: u64 = 1_790_311_076;
    let resting = Row {
        tab: "s-b5cf2faabac5ce5127bd".into(),
        from: "2.1.281".into(),
        to: "2.1.282".into(),
        phase: Phase::Failed("unanswered".into()),
        wait: "failed".into(),
        behind_since: NOW - 8 * 3_600,
        retry_at: NOW + 3_000,
        stop_streak: 1,
        streak_why: "unanswered".into(),
        ..Row::default()
    };
    assert_eq!(resting.stall(NOW).as_deref(), Some("overdue"));
    assert_eq!(resting.remedy(NOW), Some(Remedy::AskAgain));
    assert!(resting.asks_on_its_own(NOW));
    let msg = super::agent_upgrade_stalled(&resting, NOW, "in tab 2");
    assert_eq!(msg.title, "Claude upgrade retries later in tab 2");
    assert_eq!((msg.severity, msg.hold), (Severity::Info, Hold::LogOnly));
    assert_eq!(
        msg.detail[0],
        "behind for 8 h: it has not agreed to the move yet, so the upgrade rests, then asks \
         again"
    );
    assert!(
        msg.detail[1].contains("Upgrade now asks it"),
        "{:?}",
        msg.detail
    );
    if crate::presence::local_offset_s().is_some() {
        assert!(
            msg.detail[1].starts_with("it asks again on its own at "),
            "{:?}",
            msg.detail
        );
    }
    // Ruling 314 (day eight E2): the why AND the next step are the
    // sentence Settings ▸ Messages sets for a person; the move and the
    // shell spelling are its technical lines. A refused press restated
    // above them joins the sentence; a record with no move line, or no
    // tab's key (the loss row, the waiting record), keeps one line.
    let key = msg.key.as_deref();
    assert_eq!(
        super::sentence_lines(key, &msg.detail),
        2,
        "{:?}",
        msg.detail
    );
    let mut refused = msg.detail.clone();
    refused.insert(0, "the upgrade no longer takes that word".to_string());
    assert_eq!(super::sentence_lines(key, &refused), 3);
    assert_eq!(super::sentence_lines(None, &msg.detail), 1, "no key");
    assert_eq!(
        super::sentence_lines(Some(KEY_AGENT_UPGRADE), &msg.detail),
        1
    );
    assert_eq!(
        super::sentence_lines(key, &msg.detail[..2]),
        1,
        "no move line"
    );
    assert!(
        msg.detail
            .iter()
            .all(|l| !l.contains("cannot move") && !l.contains("does not move")),
        "{:?}",
        msg.detail
    );
    assert!(
        msg.actions.iter().any(|a| matches!(
            a,
            Intent::AgentUpgrade {
                word: UpgradeWord::Now,
                ..
            }
        )),
        "{:?}",
        msg.actions
    );

    // The same stop twice in a row is the person's, but no agent was
    // ended and nothing is lost by waiting a day: one digest line and a
    // record here, not a row.
    let repeating = Row {
        phase: Phase::Failed("no-resume".into()),
        wait: "failed".into(),
        stop_streak: 2,
        streak_why: "no-resume".into(),
        ..resting.clone()
    };
    let row = super::agent_upgrade_stalled(&repeating, NOW, "in tab 2");
    assert_eq!((row.severity, row.hold), (Severity::Info, Hold::LogOnly));
    assert_eq!(row.title, "Couldn't upgrade Claude in tab 2");
    // Day five, D25: it says when the next round is, as the first stop's
    // record did; D24: in a person's words, never `the harness`.
    assert!(
        row.detail[1]
            .starts_with("it stopped this way 2 times in a row; the upgrade asks it again "),
        "{:?}",
        row.detail
    );
    if crate::presence::local_offset_s().is_some() {
        assert!(
            row.detail[1].contains("asks it again at "),
            "{:?}",
            row.detail
        );
    }
    assert!(
        row.detail[..3]
            .iter()
            .chain(msg.detail[..3].iter())
            .all(|l| !l.contains("harness") && !l.contains("READY")),
        "{:?} {:?}",
        row.detail,
        msg.detail
    );
    // A refusal is met again by the next round: one digest line from the
    // first, a record here.
    let refused = Row {
        phase: Phase::Failed("not-a-shell-job".into()),
        stop_streak: 1,
        streak_why: "not-a-shell-job".into(),
        ..resting
    };
    let row = super::agent_upgrade_stalled(&refused, NOW, "in tab 2");
    assert_eq!(row.hold, Hold::LogOnly);
}

/// RULING 283: every way a move stops reads in a person's words — the
/// row's first line carries no raw stop word and nothing in parentheses
/// (it read `the move stopped (signal-refused)`).
#[test]
fn every_stop_reason_reads_in_a_persons_words() {
    use aterm_agent::harness::upgrade::{Agent, Phase};
    use aterm_agent::harness::upgrade_drive::Row;
    const NOW: u64 = 1_790_311_076;
    for agent in [Agent::Claude, Agent::Codex] {
        for why in [
            "signal-refused",
            "no-resume",
            "stale-exit",
            "shell-gone",
            "exited-before-continuing",
            "relaunch-refused",
            "resumed-elsewhere",
            "some-future-reason",
        ] {
            let row = Row {
                agent,
                tab: "s-b5cf2faabac5ce5127bd".into(),
                from: "2.1.281".into(),
                to: "2.1.282".into(),
                phase: Phase::Failed(why.into()),
                stop_streak: 2,
                streak_why: why.into(),
                behind_since: NOW - 60,
                ..Row::default()
            };
            let msg = super::agent_upgrade_stalled(&row, NOW, "in tab 2");
            let first = &msg.detail[0];
            assert!(
                !first.contains('(') && !first.contains(why) && !first.contains('`'),
                "{agent:?} {why}: {first}"
            );
        }
    }
}

/// RULING 283: a press that reaches a round which stopped between the
/// offer and the press is worded from the writer's typed refusal — the
/// tab by its place, the next round's time, the row's own `Skip version` —
/// never the CLI's sentence (a raw `s-…` id, backticked flags, a stop
/// word). NEGATIVE CONTROL: a refusal the window cannot type stays whole
/// behind Details, never the band's excerpt.
#[test]
fn a_press_on_a_round_that_stopped_is_worded_without_the_clis_sentence() {
    use aterm_agent::harness::upgrade::Agent;
    use aterm_messages::UpgradeWord;
    let why = "stopped:0:the upgrade in tab s-b5cf2faabac5ce5127bd stopped (no-resume): \
               `--now` re-arms only one that gave up";
    let msg = agent_upgrade_word_refused(
        UpgradeWord::Now,
        Agent::Claude,
        ("s-b5cf2faabac5ce5127bd", "2.1.282"),
        why,
        "in tab 2",
    );
    assert_eq!(msg.title, "Couldn't upgrade Claude in tab 2 now");
    assert!(msg.excerpt);
    // The remedy first (ruling 307).
    assert_eq!(
        msg.detail,
        [
            "retries soon",
            "the upgrade stopped before the press reached it"
        ]
    );
    for raw in ["s-b5", "`", "no-resume", "gave up", "("] {
        assert!(
            msg.detail.iter().all(|l| !l.contains(raw)),
            "{raw}: {:?}",
            msg.detail
        );
    }
    let other = agent_upgrade_word_refused(
        UpgradeWord::Now,
        Agent::Claude,
        ("s-1", "2.1.282"),
        "no upgrade is recorded for tab s-1; `aterm harness upgrade s-1 --dry-run` shows it",
        "in tab 2",
    );
    // A known kind of the writer's sentence is painted in a person's
    // words (ruling 307), the sentence whole behind them.
    assert!(other.excerpt);
    assert_eq!(other.detail[0], "no upgrade waits in that tab now");
    assert!(other.detail[1].contains("s-1"), "{:?}", other.detail);
    // NEGATIVE CONTROL: a sentence of no known kind stays behind Details.
    let unknown = agent_upgrade_word_refused(
        UpgradeWord::Now,
        Agent::Claude,
        ("s-1", "2.1.282"),
        "something else went wrong in tab s-1",
        "in tab 2",
    );
    assert!(
        !unknown.excerpt,
        "the CLI's own sentence waits behind Details"
    );
    // Another step held the lock: the row carries the press itself.
    let busy = agent_upgrade_word_refused(
        UpgradeWord::NotToday,
        Agent::Claude,
        ("s-1", "2.1.282"),
        "busy:another-sweep",
        "in tab 2",
    );
    assert_eq!(busy.title, "Couldn't postpone the upgrade in tab 2");
    assert_eq!(busy.detail[0], "press it again", "{:?}", busy.detail);
    assert_eq!(
        busy.actions,
        [Intent::AgentUpgrade {
            tab: "s-1".into(),
            to: "2.1.282".into(),
            word: UpgradeWord::NotToday,
        }]
    );
}

/// A REFUSED PRESS PAINTS ITS REMEDY AT 80 COLUMNS, whichever word was
/// pressed (ruling 307(a), review of round 21: every `Couldn't postpone
/// Claude's upgrade in tab 2` row painted its title alone at 80 while its
/// `Couldn't upgrade Claude in tab 2 now` siblings painted the remedy).
/// A target that changed says so and promises no move (`a newer build:
/// 2.1.283`, never `it moves to 2.1.283 now`); a round that stopped says
/// when it retries, at its widest clock words; a lock held elsewhere
/// paints the pressed capsule, the press being the remedy. NEGATIVE
/// CONTROL: the old 43-cell title paints no whole remedy at that width —
/// since ruling 402 only its lead clause, `a newer build`, never the
/// build it names.
#[test]
fn a_refused_press_paints_its_remedy_at_80_columns() {
    use aterm_agent::harness::upgrade::Agent;
    use aterm_messages::UpgradeWord;
    let painted = |msg: &Message| {
        let mut app = crate::App::headless_for_test();
        app.post_message(msg.clone());
        let row = app.band_presentation(80).rows[0].clone();
        (
            row.detail.map(|(_, d)| d),
            row.capsules
                .iter()
                .map(|c| c.text.clone())
                .collect::<Vec<_>>(),
        )
    };
    let refused = |word, why: &str| {
        agent_upgrade_word_refused(word, Agent::Claude, ("s-1", "2.1.282"), why, "in tab 2")
    };
    let stopped = "stopped:1790400000:the upgrade in tab s-1 stopped (no-resume): \
                   `--now` re-arms only one that gave up";
    for word in [UpgradeWord::Now, UpgradeWord::NotToday, UpgradeWord::Skip] {
        let stale = refused(word, "stale:2.1.283");
        assert_eq!(stale.detail[0], "a newer build: 2.1.283");
        assert!(
            stale.detail.iter().all(|l| !l.contains("moves")),
            "{:?}",
            stale.detail
        );
        assert_eq!(painted(&stale).0.as_deref(), Some("a newer build: 2.1.283"));
        let mut retry = refused(word, stopped);
        assert!(
            retry.detail[0].starts_with("retries "),
            "{:?}",
            retry.detail
        );
        "retries at 11:37 PM".clone_into(&mut retry.detail[0]);
        assert_eq!(painted(&retry).0.as_deref(), Some("retries at 11:37 PM"));
        let busy = refused(word, "busy:another-sweep");
        assert_eq!(
            painted(&busy).1.first().map(String::as_str),
            Some(word.label()),
            "{:?}",
            busy.title
        );
    }
    // NEGATIVE CONTROL: the title ruling 307 first chose.
    let mut old = refused(UpgradeWord::NotToday, "stale:2.1.283");
    "Couldn't postpone Claude's upgrade in tab 2".clone_into(&mut old.title);
    assert_eq!(painted(&old).0.as_deref(), Some("a newer build"));
}

/// AN UPGRADE STILL ASKING ON ITS OWN IS A RECORD (round 18, day four,
/// D5): an announced move on the agent's own work is asked again every
/// half hour and ends in a give-up of its own, so it stands on no glass.
/// So are the same wait before any notice and a turn still running, since
/// the classifier (the owner, 2026-09-28: a row only where they are
/// needed now); the one thing that stands is an agent the upgrade ended
/// (`only_an_interrupt_stands_on_the_band`).
#[test]
fn an_upgrade_still_asking_on_its_own_is_a_record() {
    use aterm_agent::harness::upgrade::Phase;
    use aterm_agent::harness::upgrade_drive::Row;
    const NOW: u64 = 1_790_311_076;
    let row = |phase: Phase, wait: &str| Row {
        tab: "s-b5cf2faabac5ce5127bd".into(),
        from: "2.1.281".into(),
        to: "2.1.282".into(),
        phase,
        wait: wait.into(),
        behind_since: NOW - 7 * 3_600,
        ..Row::default()
    };
    let asked = Phase::Announced {
        at_s: NOW - 600,
        asks: 1,
    };
    let msg = super::agent_upgrade_stalled(&row(asked.clone(), "background"), NOW, "in tab 2");
    assert_eq!(msg.hold, Hold::LogOnly);
    assert_eq!(msg.severity, Severity::Info);
    // A usage limit ends by itself before a notice as after one (ruling
    // 307): a record in either phase, never a warn row for days.
    for phase in [Phase::Pending, asked.clone()] {
        let limited = row(phase, "limited");
        assert!(limited.asks_on_its_own(NOW), "{:?}", limited.phase);
        let msg = super::agent_upgrade_stalled(&limited, NOW, "in tab 2");
        assert_eq!((msg.severity, msg.hold), (Severity::Info, Hold::LogOnly));
    }
    for (phase, wait) in [(Phase::Pending, "background"), (asked, "not-idle:busy")] {
        let msg = super::agent_upgrade_stalled(&row(phase, wait), NOW, "in tab 2");
        assert_eq!(msg.hold, Hold::LogOnly, "{wait}");
    }
}

/// NOTHING HOLDS IT NOW (2026-09-29, `upgrade::NEXT_POINT`): the watch's
/// read found the round's own next step due at a point nothing holds. A
/// first round seven hours behind is a row by the move clock, as every
/// first round is, and says what is true: its next step goes at its next
/// pause — never "`Upgrade now` does not move it past that wait: it moves
/// once that ends" of a wait that is not there. Said once, on the line a
/// grouped band also prints per tab; the remedy offers only the skip. A
/// round asking on its own under the same word is a record. NEGATIVE
/// CONTROL: a row under a real wait (a box) keeps that line.
#[test]
fn a_move_nothing_holds_is_said_so() {
    use aterm_agent::harness::upgrade::{NEXT_POINT, Phase};
    use aterm_agent::harness::upgrade_drive::Row;
    const NOW: u64 = 1_790_311_076;
    let row = |phase: Phase, wait: &str| Row {
        tab: "s-b5cf2faabac5ce5127bd".into(),
        from: "2.1.281".into(),
        to: "2.1.282".into(),
        phase,
        wait: wait.into(),
        behind_since: NOW - 7 * 3_600,
        ..Row::default()
    };
    let msg = super::agent_upgrade_stalled(&row(Phase::Pending, NEXT_POINT), NOW, "in tab 2");
    assert_eq!((msg.severity, msg.hold), (Severity::Info, Hold::LogOnly));
    assert_eq!(
        msg.detail[0], "behind for 7 h: its next step goes at its next pause",
        "{:?}",
        msg.detail
    );
    // Said once: the second line offers the skip, not the timing again.
    assert!(
        msg.detail[1].contains("keeps it on 2.1.281") && !msg.detail[1].contains("pause"),
        "{:?}",
        msg.detail
    );
    let boxed = super::agent_upgrade_stalled(&row(Phase::Pending, "box"), NOW, "in tab 2");
    assert!(
        boxed.detail[1].contains("it moves once that ends"),
        "{:?}",
        boxed.detail
    );
    let asked = row(
        Phase::Announced {
            at_s: NOW - 600,
            asks: 1,
        },
        NEXT_POINT,
    );
    assert!(asked.asks_on_its_own(NOW));
    let msg = super::agent_upgrade_stalled(&asked, NOW, "in tab 2");
    assert_eq!((msg.severity, msg.hold), (Severity::Info, Hold::LogOnly));
}

/// NOTHING HAS CHECKED IT (design record 2026-09-28, C10; the owner's
/// ruling of 2026-09-29, "with a grace after wake"): a record no look has
/// read for an hour past its due watch, the Mac awake past its grace, is a
/// ROW — over a round that would otherwise be a record, still asking on
/// its own — whose words say for how long and whose fault: aterm's, which
/// no word of the owner's mends, so no `Upgrade now`, and the shell line
/// names `--status`, which says when it was last looked at and by which
/// build. NEGATIVE CONTROL: the same record read within the wake grace is
/// the record it was, on the log alone.
#[test]
fn a_record_nothing_checks_is_said_as_aterms_own_fault() {
    use aterm_agent::harness::upgrade::Phase;
    use aterm_agent::harness::upgrade_drive::Row;
    use aterm_messages::UpgradeWord;
    const NOW: u64 = 1_790_311_076;
    let asked = Row {
        tab: "s-b5cf2faabac5ce5127bd".into(),
        from: "2.1.281".into(),
        to: "2.1.282".into(),
        phase: Phase::Announced {
            at_s: NOW - 600,
            asks: 1,
        },
        wait: "background".into(),
        behind_since: NOW - 7 * 3_600,
        watch_at: NOW - 2 * 3_600,
        looked_at: NOW - 3 * 3_600,
        awake_since: NOW - 86_400,
        ..Row::default()
    };
    let msg = super::agent_upgrade_stalled(&asked, NOW, "in tab 2");
    assert_eq!((msg.severity, msg.hold), (Severity::Info, Hold::LogOnly));
    // A record, whose title never says `Couldn't` of a wait (ruling 423).
    assert_eq!(msg.title, "Claude upgrade waits in tab 2");
    assert_eq!(
        msg.detail[0], "not checked for 3 h \u{2014} aterm's own fault",
        "{:?}",
        msg.detail
    );
    // Whose fault is the first line's; the second offers the skip alone,
    // never an act with nowhere to go ("report it").
    assert_eq!(
        msg.detail[1], "Skip version keeps it on 2.1.281",
        "{:?}",
        msg.detail
    );
    assert!(
        !msg.detail.iter().any(|l| l.contains("report")),
        "{:?}",
        msg.detail
    );
    assert!(
        msg.detail
            .last()
            .is_some_and(|l| l.ends_with("--status` or `--skip`")),
        "{:?}",
        msg.detail
    );
    assert!(
        !msg.actions.iter().any(|intent| matches!(
            intent,
            Intent::AgentUpgrade {
                word: UpgradeWord::Now,
                ..
            }
        )),
        "{:?}",
        msg.actions
    );
    let woke = Row {
        awake_since: NOW - 60,
        ..asked
    };
    assert!(woke.asks_on_its_own(NOW), "within the grace: a record");
    let msg = super::agent_upgrade_stalled(&woke, NOW, "in tab 2");
    assert_eq!((msg.severity, msg.hold), (Severity::Info, Hold::LogOnly));
    assert!(!msg.detail[0].contains("not checked"), "{:?}", msg.detail);
}

/// A TAB NO WORKER OF ATERM'S RUNS IN, BY SOMEONE ELSE'S HAND, IS NEVER
/// SAID AS ATERM'S OWN FAULT (the review of 2026-09-30, of C10): its agent
/// suspended with `^Z` (not in front), or its tab under another
/// supervisor's claim (`Row::aside`, which the owner's view sets). The
/// record nothing has stamped for three hours is told by what holds it —
/// "it is not in front in its tab", "another supervisor holds its tab" —
/// and by what moves it: `fg` in the tab, that supervisor letting go.
/// NEGATIVE CONTROL: the same record with no aside reads as aterm's own
/// fault ("not checked for 3 h — aterm's own fault"), as it must where
/// aterm's watch is at fault.
#[test]
fn a_tab_held_aside_is_said_by_whose_hand_holds_it() {
    use aterm_agent::harness::upgrade::Phase;
    use aterm_agent::harness::upgrade_drive::{CLAIMED, NOT_FOREGROUND, Row};
    const NOW: u64 = 1_790_311_076;
    let row = |aside: &str| Row {
        tab: "s-b5cf2faabac5ce5127bd".into(),
        from: "2.1.281".into(),
        to: "2.1.282".into(),
        phase: Phase::Pending,
        wait: if aside.is_empty() {
            "not-idle:busy"
        } else {
            aside
        }
        .into(),
        aside: aside.into(),
        behind_since: NOW - 7 * 3_600,
        watch_at: NOW - 2 * 3_600,
        looked_at: NOW - 3 * 3_600,
        awake_since: NOW - 86_400,
        ..Row::default()
    };
    let control = super::agent_upgrade_stalled(&row(""), NOW, "in tab 2");
    assert!(
        control.detail[0].ends_with("aterm's own fault"),
        "NEGATIVE CONTROL: {:?}",
        control.detail
    );
    for (aside, what, moves) in [
        (
            NOT_FOREGROUND,
            "it is not in front in its tab",
            "it moves once it is back in front in its tab (fg there); Skip version keeps it \
             on 2.1.281",
        ),
        (
            CLAIMED,
            "another supervisor holds its tab",
            "it moves once the supervisor holding its tab lets it go; Skip version keeps it \
             on 2.1.281",
        ),
    ] {
        let msg = super::agent_upgrade_stalled(&row(aside), NOW, "in tab 2");
        assert_eq!((msg.severity, msg.hold), (Severity::Info, Hold::LogOnly));
        assert!(msg.detail[0].ends_with(what), "{aside}: {:?}", msg.detail);
        assert_eq!(msg.detail[1], moves, "{aside}");
        assert!(
            msg.detail.iter().all(|l| !l.contains("fault")),
            "{aside}: {:?}",
            msg.detail
        );
    }
}

/// THE OWNER'S `Upgrade now` IS ANSWERED ON THE GLASS (2026-09-28: "when I
/// pressed 'update' in the claude version update button, nothing seemed
/// to happen?"). The press answers its row in place, and `--now` quiets
/// the stall for `NOW_QUIETS_S`; after that the host posts the tab's
/// stall again, and a move waiting on the agent's own work — which
/// `--now` does not waive — was a record posted and folded in the same
/// millisecond (messages.log, ids 64 and 66), withdrawing the answered
/// row under the same key. The owner's word still stood; nothing on the
/// glass said so. While it stands the answer is a calm notice that says
/// what it waits on and folds after [`ANSWER_MAX`] (the owner's rule: the
/// answer to their own press, with no action left in it, does not
/// persist); with no word it stays a record (D5).
///
/// FAILS WITHOUT THE FIX: the hurried row is `LogOnly`, as the unasked
/// one is.
#[test]
fn the_owners_upgrade_now_is_answered_on_the_glass() {
    use aterm_agent::harness::upgrade::{Phase, Request};
    use aterm_agent::harness::upgrade_drive::Row;
    const NOW: u64 = 1_790_311_076;
    let row = |request: Request| Row {
        tab: "s-b5cf2faabac5ce5127bd".into(),
        from: "2.1.280".into(),
        to: "2.1.284".into(),
        phase: Phase::Announced {
            at_s: NOW - 600,
            asks: 3,
        },
        wait: "background".into(),
        // Inside the move clock (`MOVE_BUDGET_S`): past it the round no
        // longer "asks on its own" (its words say so), and the answer to
        // the owner's press is the same (below).
        behind_since: NOW - 8 * 3_600,
        request,
        // Pressed longer ago than `--now` quiets a stall
        // (`NOW_QUIETS_S`, which is `REASK_S`): the stall is back, and so is
        // the host's row.
        request_at: NOW - aterm_agent::harness::upgrade::REASK_S - 60,
        ..Row::default()
    };
    let hurried = row(Request::Now);
    assert!(hurried.asks_on_its_own(NOW), "still the upgrade working");
    let msg = super::agent_upgrade_stalled(&hurried, NOW, "in tab 1");
    assert_eq!(
        (msg.severity, msg.hold),
        (Severity::Info, Hold::For(super::ANSWER_MAX))
    );
    assert!(
        msg.detail[1].contains("cannot move it past its own work"),
        "it says why the press did not move it: {:?}",
        msg.detail
    );
    // NEGATIVE CONTROL: no word from the owner, the same wait — a record.
    let msg = super::agent_upgrade_stalled(&row(Request::None), NOW, "in tab 1");
    assert_eq!((msg.severity, msg.hold), (Severity::Info, Hold::LogOnly));
}

/// THE OWNER'S PRESS IS ANSWERED, AND THE GIVE-UP AFTER IT IS THE DIGEST'S
/// (round six, F7, read against the owner's rule of 2026-09-28): tab #1,
/// three days behind its own never-ending work, the owner pressed
/// `Upgrade now`, the round gave up — which spends the word
/// (`St::give_up`) — and the next post under the tab's key was a log
/// record that replaced the standing answer. Round six made that record a
/// warn row with a mark, "asked or not"; an alert that persists where the
/// owner can do nothing new is the very thing the rule forbids. The press
/// is answered on the glass for [`ANSWER_MAX`] (they see it was heard and
/// why it cannot move yet), and what the give-up says about a tab three
/// days behind is ONE line in the digest, which they can read and
/// dismiss: the classifier's `own-work`.
///
/// FAILS WITHOUT THE CLASSIFIER: a warn row that stands for days.
#[test]
fn a_give_up_after_the_owners_press_is_the_digests_not_a_row() {
    use aterm_agent::harness::upgrade::{GAVE_UP, Phase, Request};
    use aterm_agent::harness::upgrade_drive::{Attention, Row};
    const NOW: u64 = 1_790_311_076;
    let hurried = Row {
        tab: "s-b5cf2faabac5ce5127bd".into(),
        from: "2.1.280".into(),
        to: "2.1.284".into(),
        phase: Phase::Announced {
            at_s: NOW - 600,
            asks: 3,
        },
        wait: "background".into(),
        behind_since: NOW - 3 * 86_400,
        request: Request::Now,
        request_at: NOW - aterm_agent::harness::upgrade::REASK_S - 60,
        ..Row::default()
    };
    let msg = super::agent_upgrade_stalled(&hurried, NOW, "in tab 1");
    assert_eq!(
        (msg.severity, msg.hold),
        (Severity::Info, Hold::For(super::ANSWER_MAX)),
        "the press is answered"
    );
    // What `St::give_up` does: the round stops, the word is spent.
    let gave_up = Row {
        phase: Phase::Failed(GAVE_UP.into()),
        streak_why: GAVE_UP.into(),
        stop_streak: 1,
        wait: "failed".into(),
        request: Request::None,
        request_at: 0,
        ..hurried
    };
    let msg = super::agent_upgrade_stalled(&gave_up, NOW, "in tab 1");
    assert_eq!(
        (msg.severity, msg.hold),
        (Severity::Info, Hold::LogOnly),
        "three days behind after the owner's press is a record"
    );
    assert_eq!(
        msg.title, "Claude upgrade retries later in tab 1",
        "and it says the truth of the record: the round goes on by itself at any age"
    );
    assert!(
        matches!(gave_up.attention(NOW), Attention::Digest(n) if n.kind == "own-work"),
        "and the day is ONE digest line: {:?}",
        gave_up.attention(NOW)
    );
}

/// THE STALLED ROW CARRIES THE OWNER'S WORDS THAT MOVE IT (gap #21: the
/// owner could steer an upgrade only by typing `aterm harness upgrade
/// <tab> --now|--defer|--skip` into another shell). Two capsules, for
/// THIS tab and THIS build: `Upgrade now` + `Not today` where `--now`
/// moves it (gave up — and a healthy wait, the waiting record's, which it
/// stands at the ladder's last rung), `Not today` + `Skip version` where
/// it would not (overdue on a turn still running: hours on that rung
/// already, ruling 380; waiting on the READY answer, in a pane, stopped for good, or already
/// hurried by the owner's `--now` — a second press would only arm a new
/// round); none once
/// a restart is under way or done, where the harness refuses a word. The
/// tab menu takes all three words, from the same table.
#[test]
fn a_stalled_upgrade_row_carries_the_words_that_move_its_kind() {
    use aterm_agent::harness::upgrade::Phase;
    use aterm_agent::harness::upgrade_drive::Row;
    use aterm_messages::UpgradeWord::{self, NotToday, Now, Skip};
    const NOW: u64 = 1_790_311_076;
    let tab = "s-b5cf2faabac5ce5127bd";
    let row = |phase: Phase, wait: &str, behind: u64| Row {
        tab: tab.into(),
        from: "2.1.281".into(),
        to: "2.1.282".into(),
        phase,
        wait: wait.into(),
        behind_since: NOW - behind,
        ..Row::default()
    };
    let capsules = |words: &[UpgradeWord]| -> Vec<Intent> {
        words
            .iter()
            .map(|&word| Intent::AgentUpgrade {
                tab: tab.into(),
                to: "2.1.282".into(),
                word,
            })
            .collect()
    };
    for (case, row, words) in [
        (
            "healthy",
            row(Phase::Pending, "not-idle:busy", 60),
            &[Now, NotToday, Skip][..],
        ),
        (
            "overdue",
            row(Phase::Pending, "not-idle:busy", 7 * 3_600),
            &[NotToday, Skip],
        ),
        (
            "gave up",
            row(Phase::Failed("unanswered".into()), "", 60),
            &[Now, NotToday, Skip],
        ),
        (
            "waits on READY",
            row(Phase::Pending, "awaiting-ready", 7 * 3_600),
            &[NotToday, Skip],
        ),
        (
            "in a pane",
            row(Phase::Pending, "terminal:tmux", 60),
            &[NotToday, Skip],
        ),
        (
            "stopped",
            row(Phase::Failed("no-resume".into()), "", 60),
            &[NotToday, Skip],
        ),
        (
            "hurried",
            Row {
                request: aterm_agent::harness::upgrade::Request::Now,
                request_at: NOW - 60,
                ..row(Phase::Pending, "not-idle:busy", 7 * 3_600)
            },
            &[NotToday, Skip],
        ),
        // Ruling 270: the word in force is not offered again — a
        // running deferral drops `Not today`, a skip of this build
        // leaves nothing; a deferral that ran out offers it again.
        (
            "deferred",
            Row {
                request: aterm_agent::harness::upgrade::Request::DeferUntil(NOW + 3_600),
                request_at: NOW - 60,
                ..row(Phase::Pending, "not-idle:busy", 7 * 3_600)
            },
            &[Now, Skip],
        ),
        (
            "deferral ran out",
            Row {
                request: aterm_agent::harness::upgrade::Request::DeferUntil(NOW - 1),
                ..row(Phase::Failed("no-resume".into()), "", 60)
            },
            &[NotToday, Skip],
        ),
        (
            "skipped",
            Row {
                request: aterm_agent::harness::upgrade::Request::Skip("2.1.282".into()),
                ..row(Phase::Failed("no-resume".into()), "", 60)
            },
            &[],
        ),
        ("under way", row(Phase::Exiting { at_s: NOW }, "", 60), &[]),
        ("done", row(Phase::Done, "", 60), &[]),
    ] {
        assert_eq!(agent_upgrade_words(&row, NOW), words, "{case}");
        assert_eq!(
            agent_upgrade_capsules(&row, NOW),
            capsules(&words[..words.len().min(2)]),
            "{case}"
        );
        if row.stall(NOW).is_some() {
            assert_eq!(
                agent_upgrade_stalled(&row, NOW, "in tab 2").actions,
                capsules(&words[..2]),
                "{case}: the row's capsules"
            );
        }
    }
    // The waiting record's single session is healthy: `Upgrade now` is
    // the accent chip, `Not today` the quiet one.
    let healthy = agent_upgrade_capsules(&row(Phase::Pending, "", 60), NOW);
    assert!(healthy[0].is_consequential() && !healthy[1].is_consequential());
}

use super::*;

/// THE SOCKETLESS HARNESS ROW KEEPS ITS FIX WHOLE (the audit of
/// 2026-09-30): cause, consequence and fix were one detail line, which
/// the line cap cut inside `--control-sock <path>` on the default macOS
/// folder of any home name of six letters or more. The fix is a line of
/// its own, and a person's words ([`sentence_lines`]). NEGATIVE CONTROL:
/// the old one-line macOS sentence for the same folder is clipped.
#[test]
fn the_socketless_harness_row_keeps_its_fix_whole_past_a_long_folder() {
    let cause = "/Users//someone/Library/Application Support/aterm can't be made private to \
                 you for a control socket";
    let msg = harness_without_socket(cause, Some(SOCKET_DIR_UNUSABLE_FIX));
    assert_eq!(
        msg.detail,
        vec![
            format!("{cause}, so {HARNESS_BOXES_UNANSWERED}"),
            SOCKET_DIR_UNUSABLE_FIX.to_string(),
        ]
    );
    assert_eq!(sentence_lines(msg.key.as_deref(), &msg.detail), 2);
    // A folder that fills the first line still leaves the fix whole.
    let deep = format!("/{}/aterm can't be made private to you", "x".repeat(300));
    let msg = harness_without_socket(&deep, Some(SOCKET_DIR_UNUSABLE_FIX));
    assert_eq!(msg.detail[1], SOCKET_DIR_UNUSABLE_FIX);
    // No fix, one line.
    let off = harness_without_socket("launched with the control socket off", None);
    assert_eq!(off.detail.len(), 1, "{:?}", off.detail);
    // The macOS fix, spelled out: Windows' shorter one fits the old line.
    let macos_fix = "set XDG_RUNTIME_DIR to a folder you own, or pass --control-sock <path>";
    let old = Message::new(tags::HARNESS, Severity::Warn, "t").line(format!(
        "{cause}, so {HARNESS_BOXES_UNANSWERED} \u{2014} {macos_fix}"
    ));
    assert!(old.detail[0].ends_with('\u{2026}'), "{:?}", old.detail);
}

fn sentences(msg: &Message) -> Vec<String> {
    std::iter::once(msg.title.clone())
        .chain(msg.detail.iter().cloned())
        .collect()
}

fn every_debugger_access_record() -> Vec<Message> {
    use crate::debugger_access::DebuggerRecord;
    [
        DebuggerRecord::On,
        DebuggerRecord::OnOutside,
        DebuggerRecord::Off,
        DebuggerRecord::WasOnNowOff,
    ]
    .into_iter()
    .map(debugger_access_record)
    .collect()
}

/// DEBUGGER ACCESS RECORDS (design §3.8): records, never rows — `LogOnly`,
/// one key, no actions, `Attention::Record` — with the undo on every "on"
/// record and only those.
#[test]
fn the_debugger_access_records_are_records_with_the_undo_only_when_on() {
    use crate::debugger_access::DebuggerRecord;
    for record in [
        DebuggerRecord::On,
        DebuggerRecord::OnOutside,
        DebuggerRecord::Off,
        DebuggerRecord::WasOnNowOff,
    ] {
        let msg = debugger_access_record(record);
        assert_eq!(msg.hold, Hold::LogOnly, "{msg:?}");
        assert_eq!(msg.key.as_deref(), Some(KEY_DEBUGGER_ACCESS));
        assert_eq!(msg.tag, tags::PRIVACY);
        assert!(msg.actions.is_empty(), "a record asks nothing: {msg:?}");
        assert_eq!(attention(&msg), Ok(Attention::Record));
        assert_eq!(
            msg.detail
                .iter()
                .any(|l| l.contains(aterm_containment::consent::DEBUGGER_ACCESS_REVERT)),
            matches!(record, DebuggerRecord::On | DebuggerRecord::OnOutside),
            "the undo rides every on record, and only those: {msg:?}"
        );
    }
    assert_eq!(
        debugger_access_record(DebuggerRecord::On).severity,
        Severity::Success
    );
}

/// Every message a launch or reload can post, from fixtures that stand
/// in for the real producers' words.
fn every_reporter_message() -> Vec<Message> {
    let mut warns = ConfigWarnings::default();
    warns.push(
        ConfigFamily::Keybindings,
        "config keybindings: skipping \"ctrl+x\": unknown action \"foo\"".into(),
    );
    warns.push(
        ConfigFamily::Keybindings,
        "config key_sequences: skipping \"ctrl+a b\": bad chord".into(),
    );
    warns.push(
        ConfigFamily::IgnoredKeys,
        "config line 3: unknown key \"windw_padding\" — did you mean \"window_padding\"?".into(),
    );
    warns.push(
        ConfigFamily::Restart,
        "columns/lines applies on next launch (resize the window to change size now)".into(),
    );
    warns.push(
        ConfigFamily::Restart,
        "gpu applies on next launch (the renderer backend is chosen at startup)".into(),
    );
    warns.push(
        ConfigFamily::Fonts,
        "config font_family_bold: \"Nope\" is not an admissible font (not found); ignored".into(),
    );
    crate::app_config::collect_key_notices(
        &mut warns,
        "game_font = \"chunky\"\nshow_hud = true\n[packages]\nenabled = true\n\
         auto_update = false\nseed_install = true\n",
    );
    let mut all = warns.into_messages();
    // Every family's title, singular and plural, so a re-word of any of
    // them meets the attention gate (audit 2026-09-24).
    for family in [
        ConfigFamily::Keybindings,
        ConfigFamily::IgnoredKeys,
        ConfigFamily::RetiredKeys,
        ConfigFamily::UnacceptedValues,
        ConfigFamily::CursorTrail,
        ConfigFamily::Fonts,
        ConfigFamily::Assets,
        ConfigFamily::Restart,
        ConfigFamily::SecureKeyboard,
    ] {
        for n in [1, 3] {
            let mut one = ConfigWarnings::default();
            for i in 0..n {
                one.push(family, format!("config sentence {i} for this family"));
            }
            all.extend(one.into_messages());
        }
    }
    all.push(crash_message(&crate::logging::CrashEvidence {
        path: std::path::PathBuf::from("/Users/_an/Library/Logs/aterm/crash-1-1.log.seen"),
        head: vec!["aterm-gui 0.1.0 crashed at unix 1.000".into()],
    }));
    all.push(killed_message(&crate::logging::KillEvidence {
        marker: std::path::PathBuf::from(
            "/Users/_an/Library/Logs/aterm/crash-marker-1-1-app.log.seen",
        ),
        log: std::path::PathBuf::from("/Users/_an/Library/Logs/aterm/aterm.log"),
    }));
    for (cause, fix) in [
        ("launched with the control socket off", None),
        (
            "no per-user runtime folder for a control socket",
            Some(SOCKET_DIR_UNSET_FIX),
        ),
        (
            "/run/user/501/aterm can't be made private to you for a control socket",
            Some(SOCKET_DIR_UNUSABLE_FIX),
        ),
        (
            "~/Library/Application Support/aterm can't be made private to you for a control \
             socket",
            Some(SOCKET_DIR_UNUSABLE_FIX),
        ),
        (
            "the --control-sock path is 120 bytes, over the 103-byte limit",
            Some("use a shorter path"),
        ),
    ] {
        all.push(harness_without_socket(cause, fix));
    }
    for class in [
        crate::crash_journal::DeathClass::Killed,
        crate::crash_journal::DeathClass::Signal,
        crate::crash_journal::DeathClass::Panic,
    ] {
        for reopened in [
            reopened_fixture(class),
            reopened_with_programs(class, &[]),
            reopened_with_programs(class, &[(1, "vim")]),
            reopened_with_programs(class, &[(0, "claude"), (1, "")]),
            reopened_with_agent(class, &[]),
            reopened_with_agent(class, &[(0, "vim")]),
        ] {
            for relaunching in [false, true] {
                all.push(journal_reopened_message(
                    &reopened,
                    None,
                    None,
                    Some(std::path::Path::new(
                        "/Users/_an/Library/Logs/aterm/aterm.log",
                    )),
                    relaunching,
                    PanesWithoutFolder::NONE,
                    0,
                ));
            }
        }
    }
    // A pane whose folder no shell could start in (audit #7 finding 48):
    // the reopened line that counts it, and each fault's row; and a pane
    // a program held whose folder is not here (audit #8).
    for (gone, unasked) in [(1, 0), (2, 0), (0, 1)] {
        all.push(journal_reopened_message(
            &reopened_fixture(crate::crash_journal::DeathClass::Killed),
            None,
            None,
            None,
            false,
            PanesWithoutFolder { gone, unasked },
            0,
        ));
    }
    for class in [
        crate::crash_journal::DeathClass::Killed,
        crate::crash_journal::DeathClass::Signal,
    ] {
        for reopened in [
            reopened_with_programs(class, &[]),
            reopened_with_agent(class, &[]),
        ] {
            all.push(journal_reopened_message(
                &reopened,
                None,
                None,
                None,
                true,
                PanesWithoutFolder {
                    gone: 1,
                    unasked: 0,
                },
                0,
            ));
        }
    }
    for fault in crate::spawn_folder::Fault::ALL {
        all.push(folder_row(fault, &named(&["/Users/_an/src/old"])));
        all.push(folder_row(fault, &named(&["/a", "/b"])));
    }
    for note in journal_note_fixtures() {
        all.push(journal_note_message(
            &note,
            Some(std::path::Path::new(
                "/Users/_an/Library/Logs/aterm/aterm.log",
            )),
        ));
    }
    {
        use aterm_agent::harness::relaunch::Outcome;
        let missed = [
            ("in tab 2".to_string(), Outcome::Cannot("shell-gone".into())),
            ("in tab 3".to_string(), Outcome::NotYet(String::new())),
        ];
        all.push(restored_agents_not_resumed(
            &missed[..1],
            Some(std::path::Path::new(
                "/Users/_an/Library/Logs/aterm/aterm.log",
            )),
        ));
        all.push(restored_agents_not_resumed(&missed, None));
    }
    all.push(launch_load_failure(
        "aterm.toml is not a valid configuration (expected `=` at line 3) — every \
         setting is running at its default. Fix /home/ana/.config/aterm.toml and it loads on \
         the next change.",
    ));
    all.push(cpu_renderer_no_effect(
        "background_opacity",
        "it has no translucent present path, so the window stays solid",
    ));
    all.push(backdrop_declined(
        "background_material is styling the title bar only",
        "this display stack refused the DirectComposition backdrop swapchain",
    ));
    all.push(gpu_lost());
    all.extend(integration_lost_fixtures());
    all.push(integration_rekey_waits("in tab 2", "s-0123456789abcdef"));
    all.push(a11y_publisher_dead("Server GUID mismatch", true));
    all.push(a11y_publisher_dead("Server GUID mismatch", false));
    all.push(presence_not_saved(
        "presence.band",
        "Presence Band",
        "the write was refused",
        false,
    ));
    all.push(presence_not_saved(
        "presence.rim",
        "Presence Rim",
        "the write could not be verified",
        true,
    ));
    all.push(fleet_hold(true));
    all.push(fleet_hold(false));
    all.push(hold_refused(true, "no session"));
    all.push(hold_refused(false, "no session"));
    all.push(fabric_failure(
        "document",
        "Couldn't open the document",
        &["no such file", "~/x.md"],
    ));
    all.push(fabric_failure(
        "on",
        "Couldn't start Fabric On",
        &["cannot spawn its thread"],
    ));
    all.push(fabric_failure(
        "inbox",
        "Couldn't open the inbox",
        &["no such file"],
    ));
    all.push(fabric_failure(
        "status",
        "Couldn't start Fabric Status",
        &["cannot spawn its thread"],
    ));
    all.push(fabric_status("on", "Fabric On finished", "", true));
    all.push(fabric_status(
        "off",
        "Couldn't run Fabric Off",
        "exited 2",
        false,
    ));
    all.push(serious_mode_feedback(
        "Serious Mode was not changed: aterm.toml changed first",
    ));
    all.push(serious_mode_feedback(
        "Serious Mode was saved but could not be applied: x",
    ));
    all.push(serious_mode_feedback(
        "Serious Mode was saved, but a newer aterm.toml edit now controls it",
    ));
    all.push(serious_mode_feedback(
        "Serious Mode may have been written but could not be verified; reload before \
         retrying: the disk generation moved",
    ));
    for head in [
        "Config observation was not valid TOML: expected `]`",
        "Robi was not dismissed: the setting was not saved",
        "Config reconciliation failed; queued changes were not written: io",
        "Manual saved aterm.toml, but its exact generation could not be admitted: x",
        "Config publication could not be verified: x",
        "Config was NOT saved: x",
        "Something new went wrong: x",
    ] {
        all.push(config_lane_error(head));
    }
    // The live agent upgrade's three (gap audit 2026-09-24): waiting (a
    // record), stalled (a row), done (a record).
    all.extend(agent_upgrade_waiting(
        "Claude Code",
        &["2.1.282", "2.1.282"],
    ));
    all.extend(agent_upgrade_waiting("Claude Code", &["2.1.282"]));
    all.extend(agent_upgrade_waiting(
        "Claude Code",
        &["2.1.282", "2.1.283"],
    ));
    all.extend(agent_upgrade_waiting("Codex", &["0.157.1"]));
    // One session's record (ruling 380), by its tab and in a second
    // window, Claude Code's longer product name included.
    for (agent, from, to) in [
        (
            aterm_agent::harness::upgrade::Agent::Codex,
            "0.157.1",
            "0.158.0",
        ),
        (
            aterm_agent::harness::upgrade::Agent::Claude,
            "2.1.282",
            "2.1.283",
        ),
    ] {
        for place in ["in tab 1", "in window 2, tab 11"] {
            all.push(agent_upgrade_waiting_in(
                &aterm_agent::harness::upgrade_drive::Row {
                    tab: "s-09205e59a0bbd30464d4".into(),
                    from: from.into(),
                    to: to.into(),
                    agent,
                    phase: aterm_agent::harness::upgrade::Phase::Pending,
                    behind_since: 1_790_572_868,
                    ..Default::default()
                },
                place,
            ));
        }
    }
    let upgrade = aterm_agent::harness::upgrade_drive::Row {
        tab: "s-b5cf2faabac5ce5127bd".into(),
        from: "2.1.281".into(),
        to: "2.1.282".into(),
        phase: aterm_agent::harness::upgrade::Phase::Failed("unanswered".into()),
        outcome: "claude restarted on 2.1.282 · model claude-opus-5-5".into(),
        ..Default::default()
    };
    all.push(agent_upgrade_stalled(&upgrade, 1_790_311_076, "in tab 2"));
    all.push(agent_upgrade_stalled(
        &aterm_agent::harness::upgrade_drive::Row {
            phase: aterm_agent::harness::upgrade::Phase::Pending,
            behind_since: 1_790_280_544,
            wait: "not-idle:busy".into(),
            ..upgrade.clone()
        },
        1_790_311_076,
        "in tab 2",
    ));
    // A Claude Code behind its own running work (2026-09-26), after a notice
    // and before one: the sentence guards read both remedy lines.
    for (phase, wait) in [
        (
            aterm_agent::harness::upgrade::Phase::Announced {
                at_s: 1_790_300_000,
                asks: 2,
            },
            "background",
        ),
        (
            aterm_agent::harness::upgrade::Phase::Pending,
            "not-idle:shell",
        ),
    ] {
        all.push(agent_upgrade_stalled(
            &aterm_agent::harness::upgrade_drive::Row {
                phase,
                behind_since: 1_790_280_544,
                wait: wait.into(),
                ..upgrade.clone()
            },
            1_790_311_076,
            "in tab 2",
        ));
    }
    // A record nothing has checked (design record C10): aterm's own
    // fault, said as such — the sentence guards read its remedy too.
    all.push(agent_upgrade_stalled(
        &aterm_agent::harness::upgrade_drive::Row {
            phase: aterm_agent::harness::upgrade::Phase::Pending,
            behind_since: 1_790_307_476,
            wait: "not-idle:busy".into(),
            watch_at: 1_790_311_076 - 2 * 3_600,
            looked_at: 1_790_311_076 - 3 * 3_600,
            awake_since: 1_790_311_076 - 86_400,
            ..upgrade.clone()
        },
        1_790_311_076,
        "in tab 2",
    ));
    all.push(agent_upgrade_done(&upgrade, "in tab 2"));
    // THE ROWS THAT STAND (ruling 423: the agent the upgrade ended, a
    // shutdown that hung, a relaunch that reached no prompt), the answer to
    // the owner's own `Upgrade now`, and aterm's own fault: every sentence
    // they say is read by the title, restart and glass-row guards.
    {
        use aterm_agent::harness::upgrade::{Phase, Request};
        const NOW: u64 = 1_790_311_076;
        for row in [
            aterm_agent::harness::upgrade_drive::Row {
                phase: Phase::Failed("stale-exit".into()),
                exited_at: NOW - 600,
                ..upgrade.clone()
            },
            aterm_agent::harness::upgrade_drive::Row {
                phase: Phase::Exiting { at_s: NOW - 1_200 },
                ..upgrade.clone()
            },
            aterm_agent::harness::upgrade_drive::Row {
                phase: Phase::Relaunched { at_s: NOW - 2_400 },
                ..upgrade.clone()
            },
            aterm_agent::harness::upgrade_drive::Row {
                phase: Phase::Announced {
                    at_s: NOW - 600,
                    asks: 2,
                },
                wait: "background".into(),
                request: Request::Now,
                request_at: NOW - 30,
                behind_since: NOW - 32 * 60,
                ..upgrade.clone()
            },
        ] {
            all.push(agent_upgrade_stalled(&row, NOW, "in tab 2"));
        }
        let unclassified = aterm_agent::harness::upgrade_drive::Row {
            phase: Phase::Pending,
            wait: "zzz-unclassified".into(),
            wait_since: NOW - 2 * 3_600,
            behind_since: NOW - 3 * 3_600,
            ..upgrade.clone()
        };
        if let aterm_agent::harness::upgrade_drive::Attention::Fault(need) =
            unclassified.attention(NOW)
        {
            all.push(agent_upgrade_fault(&unclassified, &need, "in tab 2"));
        }
    }
    // WHERE THE OWNER HAS MORE THAN ONE WINDOW the place names it (`in
    // window 2, tab 1`), a word longer: every title that names the tab —
    // a wait `Upgrade now` moves, one it does not (`… yet`), a stop, and
    // each refused word — still keeps the glass title rule, at two-digit
    // windows and tabs too.
    for place in [tab_place(Some(2), 1), tab_place(Some(12), 10)] {
        for (phase, wait) in [
            (
                aterm_agent::harness::upgrade::Phase::Pending,
                "not-idle:busy",
            ),
            (
                aterm_agent::harness::upgrade::Phase::Pending,
                "not-idle:shell",
            ),
            (
                aterm_agent::harness::upgrade::Phase::Announced {
                    at_s: 1_790_300_000,
                    asks: 2,
                },
                "background",
            ),
            (
                aterm_agent::harness::upgrade::Phase::Failed("not-a-shell-job".into()),
                "",
            ),
        ] {
            all.push(agent_upgrade_stalled(
                &aterm_agent::harness::upgrade_drive::Row {
                    phase,
                    wait: wait.into(),
                    behind_since: 1_790_280_544,
                    ..upgrade.clone()
                },
                1_790_311_076,
                &place,
            ));
        }
        for word in aterm_messages::UpgradeWord::ALL {
            all.push(agent_upgrade_word_refused(
                word,
                aterm_agent::harness::upgrade::Agent::Claude,
                ("s-1", "2.1.282"),
                "busy:another-sweep",
                &place,
            ));
        }
        all.push(agent_upgrade_done(&upgrade, &place));
    }
    // A stall's end, recorded under its row's key once the row was down
    // (review of 2026-09-27).
    let stalled = agent_upgrade_stalled(&upgrade, 1_790_311_076, "in tab 2");
    for end in [StallEnd::AsksAgain, StallEnd::MovesAgain, StallEnd::Left] {
        all.push(agent_upgrade_stall_over(
            upgrade.agent,
            stalled.key.as_deref().expect("keyed"),
            &stalled.detail,
            "in tab 2",
            end,
        ));
    }
    // The owner's word from the band (gap #21): what each word did, and
    // each kind of refusal.
    all.push(agent_upgrade_worded(
        &upgrade,
        aterm_messages::UpgradeWord::Now,
        "in tab 2",
        true,
    ));
    for word in aterm_messages::UpgradeWord::ALL {
        all.push(agent_upgrade_worded(&upgrade, word, "in tab 2", false));
        for why in [
            "busy:another-sweep",
            "stale:2.1.283",
            "the upgrade in tab s-b5cf2faabac5ce5127bd stopped for good (no-resume): `--now` \
             re-arms only one that gave up",
            // Ruling 283: a round that stopped between the offer and the
            // press, as the writer refuses it now (typed).
            "stopped:1790318276:the upgrade in tab s-b5cf2faabac5ce5127bd stopped \
             (no-resume): `--now` re-arms only one that gave up",
            "stopped:0:the upgrade in tab s-b5cf2faabac5ce5127bd stopped (signal-refused): \
             `--now` re-arms only one that gave up",
        ] {
            all.push(agent_upgrade_word_refused(
                word,
                aterm_agent::harness::upgrade::Agent::Claude,
                ("s-b5cf2faabac5ce5127bd", "2.1.282"),
                why,
                "in tab 2",
            ));
        }
    }
    // The Codex lane's (a daemon-mode client moved, one that named no
    // thread as it exited): its words pass every reporter guard too.
    let codex = aterm_agent::harness::upgrade_drive::Row {
        agent: aterm_agent::harness::upgrade::Agent::Codex,
        session: "codex-s-b5cf2faabac5ce5127bd".into(),
        from: "0.157.0".into(),
        to: "0.157.1".into(),
        phase: aterm_agent::harness::upgrade::Phase::Done,
        outcome: "codex on 0.157.1 · the same conversation resumed; its work never \
                  stopped, in the daemon"
            .into(),
        ..upgrade.clone()
    };
    all.push(agent_upgrade_done(&codex, "in tab 2"));
    all.push(agent_upgrade_stalled(
        &aterm_agent::harness::upgrade_drive::Row {
            phase: aterm_agent::harness::upgrade::Phase::Failed("no-resume-hint".into()),
            ..codex.clone()
        },
        1_790_311_076,
        "in tab 2",
    ));
    // The same, failed after its `/exit` ended the TUI (its record now
    // stands with no holder, and names `codex resume`), and a Codex
    // client whose daemon waits on a detached thread.
    all.push(agent_upgrade_stalled(
        &aterm_agent::harness::upgrade_drive::Row {
            phase: aterm_agent::harness::upgrade::Phase::Failed("no-resume-hint".into()),
            exited_at: 1_790_311_000,
            ..codex.clone()
        },
        1_790_311_076,
        "in tab 2",
    ));
    all.push(agent_upgrade_stalled(
        &aterm_agent::harness::upgrade_drive::Row {
            phase: aterm_agent::harness::upgrade::Phase::Pending,
            behind_since: 1_790_280_544,
            wait: "daemon-first:busy-thread".into(),
            ..codex.clone()
        },
        1_790_311_076,
        "in tab 2",
    ));
    // A Claude Code move that stopped after its SIGTERM ended the agent
    // (it names `claude --resume`), and moves under way that do not move
    // (stuck exiting, exited, relaunched), for both agents (2026-09-27).
    all.push(agent_upgrade_stalled(
        &aterm_agent::harness::upgrade_drive::Row {
            session: "0badf00d-1111-2222-3333-444455556666".into(),
            phase: aterm_agent::harness::upgrade::Phase::Failed("stale-exit".into()),
            exited_at: 1_790_311_000,
            ..upgrade.clone()
        },
        1_790_311_076,
        "in tab 2",
    ));
    for row in [&upgrade, &codex] {
        for (phase, exited_at) in [
            (
                aterm_agent::harness::upgrade::Phase::Exiting {
                    at_s: 1_790_310_000,
                },
                0,
            ),
            (
                aterm_agent::harness::upgrade::Phase::Exiting {
                    at_s: 1_790_310_000,
                },
                1_790_310_010,
            ),
            (
                aterm_agent::harness::upgrade::Phase::Relaunched {
                    at_s: 1_790_310_000,
                },
                1_790_310_010,
            ),
        ] {
            all.push(agent_upgrade_stalled(
                &aterm_agent::harness::upgrade_drive::Row {
                    phase,
                    exited_at,
                    behind_since: 1_790_309_000,
                    ..row.clone()
                },
                1_790_311_076,
                "in tab 2",
            ));
        }
    }
    all.extend(agent_upgrade_waiting(
        "Claude Code and Codex",
        &["2.1.282", "0.157.1"],
    ));
    for (phase, wait) in [
        (
            aterm_agent::harness::upgrade::Phase::Pending,
            "terminal:tmux",
        ),
        (
            aterm_agent::harness::upgrade::Phase::Failed("not-a-shell-job".into()),
            "",
        ),
        (
            aterm_agent::harness::upgrade::Phase::Failed("no-resume".into()),
            "",
        ),
    ] {
        all.push(agent_upgrade_stalled(
            &aterm_agent::harness::upgrade_drive::Row {
                phase,
                wait: wait.into(),
                behind_since: 1_790_311_000,
                ..upgrade.clone()
            },
            1_790_311_076,
            "in tab 2",
        ));
    }
    all.push(log_did_not_open("/x/y.log"));
    all.push(font_family_rejected(
        "font_family: \"Nope\" is not an admissible font",
    ));
    // R14–R22, the retired toast's reporters.
    all.extend(gesture_failures());
    all.push(file_access_question(
        &crate::consent_card::NeedWords::sample(),
    ));
    all.push(file_access_granted());
    all.extend(every_debugger_access_record());
    for posture in [
        aterm_update::which_copy::InstallPosture::MountedImage,
        aterm_update::which_copy::InstallPosture::Translocated,
    ] {
        all.push(install_posture(posture).expect("a posture to fix"));
    }
    for kind in [
        crate::connections::ConnectedSpawnKind::Controlled,
        crate::connections::ConnectedSpawnKind::Controller,
    ] {
        all.push(session_connection_created(
            &crate::connections::first_use_notice_text(kind),
        ));
    }
    all.push(session_connection_created(
        &crate::connections::first_use_connect_notice_text(true, false),
    ));
    all
}

/// A GESTURE FAILURE KEEPS THE WHOLE ERROR (design ruling 64): a 600-char
/// error — longer than one detail line — is every word behind Details, in
/// order, never clipped at the line cap.
#[test]
fn a_gesture_failure_keeps_the_whole_error() {
    let error = format!(
        "posix_spawn failed: {} (see https://example.invalid/aterm/spawn for the \
         limits); errno 35",
        "resource temporarily unavailable ".repeat(16)
    );
    assert!(error.chars().count() > aterm_messages::DETAIL_LINE_CAP);
    let m = new_tab_failed(&error);
    assert!(m.detail.len() > 1, "{:?}", m.detail);
    let normalized: String = error.split_whitespace().collect::<Vec<_>>().join(" ");
    let rejoined: String = m
        .detail
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(rejoined, normalized, "every word, in order");
    assert!(
        m.detail
            .iter()
            .all(|l| l.chars().count() <= aterm_messages::DETAIL_LINE_CAP)
    );
}

/// The folders a row names, as the fold keeps them.
fn named(dirs: &[&str]) -> crate::spawn_folder::Named {
    crate::spawn_folder::Named {
        dirs: dirs.iter().map(|dir| (*dir).to_owned()).collect(),
        unnamed: 0,
    }
}

/// THE MEMORY CEILING'S WARNING IS A ROW (owner direction 2026-09-29:
/// warn, do not restart): on the glass in the failure grammar (a Warn,
/// not a record), keyed per session so the next episode's replaces it,
/// its text — the resume line included — kept whole in the detail.
/// NEGATIVE CONTROL: the supervisor's information is a record, never a
/// row.
#[test]
fn a_supervisors_memory_warning_is_a_row_and_its_information_a_record() {
    let text = "Claude Code uses 215.1 GB, past aterm's memory ceiling of 46.0 GB; the Mac \
                has 60.0 GB available and is not short of memory; a warning only: nothing \
                is restarted; to restart it yourself: claude --resume 0badf00d";
    let row = super::supervisor_warning("s-1", "in tab 3", text);
    // The tab by its place, never the raw session id.
    assert_eq!(row.title, "Agent memory past its ceiling");
    assert_eq!(row.severity, Severity::Warn);
    assert_eq!(attention(&row), Ok(Attention::Failure));
    assert_eq!(row.key.as_deref(), Some("harness.memory.s-1"));
    let said = row.detail.join(" ");
    assert!(
        said.starts_with("Tab 3: Claude Code uses 215.1 GB"),
        "{said}"
    );
    assert!(!said.contains("s-1"), "{said}");
    assert!(said.ends_with("claude --resume 0badf00d"), "{said}");
    assert_ne!(
        super::supervisor_warning("s-2", "in tab 3", text).key,
        row.key,
        "per session"
    );
    // NEGATIVE CONTROL.
    let note = super::supervisor_note("s-1", "in tab 3", text);
    assert_eq!(attention(&note), Ok(Attention::Record));
}

/// AUDIT #7 FINDING 48 — a shell that started in the home folder because
/// its folder was gone is ONE warning row, keyed, in the failure grammar,
/// saying where the shell is instead and naming the folder; several
/// folders are counted in the title and named one per line, and past the
/// line cap the rest are counted. A folder that could not be entered has
/// its own row and words. On the glass by the attention rule: the shell is
/// not where the person asked.
#[test]
fn a_shell_in_the_home_folder_is_one_row_naming_every_folder_per_fault() {
    use crate::spawn_folder::Fault;
    let one = folder_row(Fault::Missing, &named(&["/Users/_an/src/old"]));
    assert_eq!(one.title, "Couldn't find the folder");
    assert_eq!(one.severity, Severity::Warn);
    assert_eq!(
        one.detail,
        ["opened in your home folder instead of /Users/_an/src/old"]
    );
    assert_eq!(one.key.as_deref(), Some(KEY_FOLDER_NOT_FOUND));
    assert_eq!(attention(&one), Ok(Attention::Failure));

    let three = folder_row(Fault::Missing, &named(&["/a", "/b", "/c"]));
    assert_eq!(three.title, "Couldn't find 3 folders");
    assert_eq!(
        three.detail,
        ["opened in your home folder instead", "/a", "/b", "/c"]
    );
    assert_eq!(three.key, one.key, "one row, however many folders");
    assert_eq!(attention(&three), Ok(Attention::Failure));

    let many: Vec<String> = (0..40).map(|i| format!("/gone/{i}")).collect();
    let dirs: Vec<&str> = many.iter().map(String::as_str).collect();
    let row = folder_row(Fault::Missing, &named(&dirs));
    assert_eq!(row.title, "Couldn't find 40 folders");
    assert_eq!(row.detail.len(), DETAIL_LINES_CAP);
    assert_eq!(row.detail.last().map(String::as_str), Some("and 18 more"));

    let shut = folder_row(Fault::Shut, &named(&["/srv/locked"]));
    assert_eq!(shut.title, "Couldn't open the folder");
    assert_eq!(
        shut.detail,
        ["opened in your home folder instead of /srv/locked"]
    );
    assert_eq!(shut.key.as_deref(), Some(KEY_FOLDER_NOT_OPENED));
    assert_eq!(attention(&shut), Ok(Attention::Failure));
    let two = folder_row(Fault::Shut, &named(&["/a", "/b"]));
    assert_eq!(two.title, "Couldn't open 2 folders");
}

/// A ROW READS BACK AS THE FOLDERS IT NAMES — how a row carried across an
/// update keeps them when the next folder joins it: one folder, several,
/// past the line cap (the named ones and the count), a folder with a space
/// and one longer than a line. The negative control is a row of another
/// shape.
#[test]
fn a_folder_row_reads_back_as_the_folders_it_names() {
    use crate::spawn_folder::{Fault, Named};
    let long = format!("/deep/{}", "d".repeat(300));
    let many: Vec<String> = (0..40).map(|i| format!("/gone/{i}")).collect();
    let cases = [
        named(&["/Users/_an/src/old"]),
        named(&["/Users/_an/My Projects/old"]),
        named(&["/a", "/b", "/c"]),
        named(&[long.as_str()]),
        Named {
            dirs: many[..22].to_vec(),
            unnamed: 18,
        },
    ];
    for case in cases {
        for fault in Fault::ALL {
            assert_eq!(
                folder_row_named(&folder_row(fault, &case)),
                Some(case.clone())
            );
        }
    }
    let past_the_cap = named(&many.iter().map(String::as_str).collect::<Vec<_>>());
    let back = folder_row_named(&folder_row(Fault::Missing, &past_the_cap)).expect("reads");
    assert_eq!((back.dirs.len(), back.unnamed), (22, 18));
    assert_eq!(folder_row_named(&restored_tab_failed("x")), None);
}

/// AUDIT #7 FINDING 48 — the reopened layout's restored line COUNTS the
/// panes that could not open their folder instead of claiming every tab
/// came back in its folder; the rest of the row is unchanged. The
/// negative control is the line with none. A pane a program held whose
/// folder is not here (audit #8) is not counted, and the line then claims
/// no folders: its shell started in the default folder.
#[test]
fn the_reopened_row_counts_the_panes_that_could_not_open_their_folder() {
    use crate::crash_journal::DeathClass;
    let row = |gone, unasked| {
        journal_reopened_message(
            &reopened_fixture(DeathClass::Killed),
            None,
            None,
            None,
            false,
            PanesWithoutFolder { gone, unasked },
            0,
        )
    };
    assert_eq!(
        row(0, 0).detail[1],
        "2 tabs in 1 window restored in their folders from its crash journal"
    );
    assert_eq!(
        row(1, 0).detail[1],
        "2 tabs in 1 window restored from its crash journal; 1 pane could not open its folder"
    );
    assert_eq!(
        row(2, 0).detail[1],
        "2 tabs in 1 window restored from its crash journal; 2 panes could not open their \
         folders"
    );
    assert_eq!(
        row(0, 1).detail[1],
        "2 tabs in 1 window restored from its crash journal"
    );
    assert_eq!(
        row(1, 1).detail[1],
        "2 tabs in 1 window restored from its crash journal; 1 pane could not open its folder"
    );
    let (none, some) = (row(0, 0), row(1, 0));
    assert_eq!(
        (&none.title, none.severity, &none.detail[0], &none.key),
        (&some.title, some.severity, &some.detail[0], &some.key)
    );
}

/// AUDIT #8 — once a pane did not come back in its folder, the loss
/// sentence no longer says ONLY the scrollback was lost. Both shapes that said `only` — nothing ran, and
/// only a resumed agent ran — after a kill (the quiet record) and after
/// a crash (the row on glass). The negative control is each at 0.
#[test]
fn a_pane_without_its_folder_drops_only_from_the_loss_sentence() {
    use crate::crash_journal::DeathClass;
    for class in [DeathClass::Killed, DeathClass::Signal] {
        let nothing = |gone, unasked| {
            journal_reopened_message(
                &reopened_with_programs(class, &[]),
                None,
                None,
                None,
                false,
                PanesWithoutFolder { gone, unasked },
                0,
            )
        };
        assert_eq!(nothing(0, 0).detail[0], JOURNAL_NOTHING_RAN, "{class:?}");
        assert_eq!(
            nothing(1, 0).detail[0],
            "nothing was running in them: their scrollback did not survive",
            "{class:?}"
        );
        assert_eq!(
            nothing(0, 1).detail[0],
            JOURNAL_NOTHING_RAN_FOLDERLESS,
            "{class:?}: a held pane's folder that is not here"
        );
        assert_eq!(
            nothing(1, 0).detail[1],
            "2 tabs in 1 window restored from its crash journal; 1 pane could not open its \
             folder",
            "{class:?}"
        );
        let resumed = |gone, unasked| {
            journal_reopened_message(
                &reopened_with_agent(class, &[]),
                None,
                None,
                None,
                true,
                PanesWithoutFolder { gone, unasked },
                0,
            )
        };
        assert_eq!(
            resumed(0, 0).detail[0],
            format!(
                "aterm starts Claude again on its conversation in tab 2; \
                 {JOURNAL_ONLY_SCROLLBACK}"
            ),
            "{class:?}"
        );
        assert_eq!(
            resumed(1, 0).detail[0],
            "aterm starts Claude again on its conversation in tab 2; the scrollback did not \
             survive",
            "{class:?}"
        );
        let (none, some) = (nothing(0, 0), nothing(1, 0));
        assert_eq!(
            (&none.title, none.severity, none.hold, &none.key),
            (&some.title, some.severity, some.hold, &some.key),
            "{class:?}: only the words move"
        );
    }
}

/// Every R14 row, from the sentences their sites really pass.
fn gesture_failures() -> Vec<Message> {
    vec![
        new_tab_failed("fork: Resource temporarily unavailable (os error 35)"),
        new_window_failed("the pty could not be opened"),
        split_refused(
            "Split refused: this pane is 20x5 cells; a vertical split needs at least 20x7",
        ),
        split_refused("Split refused: this pane's size could not be measured"),
        split_failed("no event loop to host the new pane"),
        keystrokes_dropped(3),
        restore_stopped_early(),
        shell_lost_in_update("descriptor 9 was closed"),
        restored_tab_failed("exec failed"),
        input_refused("the paste is too large \u{2014} paste it in smaller pieces"),
        no_connection(true),
    ]
}

/// The fences the question's copy must keep, whichever form carries it (the
/// retired row, the digest entry): the owner's restart-phrase ruling
/// (`tools/grep_guard.sh` B12), the honesty rule that no coverage or scope
/// claim may ship while §7 S1 and S4 are unrun, and the ruling that
/// mitigating this annoyance is acceptable, so it may never read as
/// elimination.
fn assert_fda_copy_is_fence_clean<'a>(texts: impl Iterator<Item = &'a String>) {
    for text in texts {
        let lower = text.to_ascii_lowercase();
        // 1. The restart-phrase fence, in the shapes B12 scans for.
        for banned in [
            "restart",
            "relaunch",
            "re-launch",
            "reopen",
            "re-open",
            "quit and",
            "next launch",
            "reboot",
            "launch aterm again",
        ] {
            assert!(!lower.contains(banned), "{banned:?} in {text:?}");
        }
        // 2. No coverage claim and no scope sentence: which folders a
        //    grant reaches is S4's measurement and how far it reaches is
        //    S1's. `FDA_GRANTS` states neither: it does not say which
        //    prompts the switch silences or when a running process sees
        //    it; it says what enabling it lets programs DO, which Apple's
        //    Full Disk Access pane itself says ("data like Mail,
        //    Messages, Safari"), and that disclosure is the point of it.
        for unmeasured in [
            "cover",
            "documents",
            "desktop",
            "downloads",
            "volume",
            "icloud",
            "retires",
            "this process",
            "new processes",
            "every folder",
            "all folders",
        ] {
            assert!(!lower.contains(unmeasured), "{unmeasured:?} in {text:?}");
        }
        // 3. Mitigate, never eliminate (owner's ruling).
        for promise in [
            "never again",
            "no more",
            "eliminat",
            "not be interrupted",
            "never be interrupted",
            "without interruption",
            "stops the",
            "ends the",
        ] {
            assert!(!lower.contains(promise), "{promise:?} in {text:?}");
        }
    }
}

/// THE COPY OF THE ONE QUESTION THIS FEATURE ASKS UNPROMPTED (the fence
/// that held the retired card's caption, `notice.rs`, moved onto the
/// words themselves — design §7.1). Three fences at once: the owner's
/// restart-phrase ruling (`tools/grep_guard.sh` B12), the honesty
/// rule that no coverage or scope claim may ship while §7 S1 and S4 are
/// unrun, and the ruling that mitigating this annoyance is acceptable —
/// so it may never read as elimination. The title and `detail[0]` (the why) are
/// held under 75 characters together so the ask survives beside its two
/// capsules on an ordinary window, and the primary capsule's label opens
/// a pane without reading as granting anything.
#[test]
fn the_file_access_message_is_honest_and_fence_clean() {
    use crate::consent_card::{FDA_AFTER, FDA_DETAIL, FDA_GRANTS, FDA_TITLE, FDA_WHY};
    let words = crate::consent_card::NeedWords::sample();
    let msg = file_access_question(&words);
    assert_eq!(msg.title, FDA_TITLE, "the row carries the const");
    // WHY first, then what macOS did, then what the switch GRANTS, the
    // route, what aterm does next, and the hedge last.
    assert_eq!(msg.detail.len(), 6, "{:?}", msg.detail);
    assert_eq!(msg.detail[0], FDA_WHY);
    assert_eq!(msg.detail[1], words.event_line());
    assert_eq!(
        msg.detail[2], FDA_GRANTS,
        "the words say what the switch grants"
    );
    assert_eq!(
        msg.detail[3],
        crate::menu::privacy_settings_path_words(crate::menu::PrivacyPane::FullDiskAccess)
    );
    assert_eq!(msg.detail[4], FDA_AFTER);
    assert_eq!(msg.detail[5], FDA_DETAIL);
    assert_fda_copy_is_fence_clean(std::iter::once(&msg.title).chain(msg.detail.iter()));
    // 4. The owner's already-enabled switch explicitly possible: a failed
    //    probe does not read the switch (the hedge is the last line).
    assert!(FDA_DETAIL.contains("Full Disk Access"));
    assert!(FDA_DETAIL.contains("may already be enabled"));
    assert!(
        FDA_TITLE.chars().count() + FDA_WHY.chars().count() <= 75,
        "short enough to survive beside two capsules on an ordinary window"
    );
    assert!(
        FDA_TITLE.ends_with('?') && !msg.excerpt,
        "the title is the ask and the row paints it alone; the hedge is behind Details"
    );
    // 5. A decision: the primary opens a pane — its label grants nothing —
    //    and Not now answers; an Ask hold, the shared key, a quiet tone.
    assert_eq!(
        msg.actions,
        [
            Intent::OpenSystemPane {
                pane: PANE_FULL_DISK_ACCESS.to_string()
            },
            Intent::NotNow {
                decision: Decision::FileAccess
            }
        ]
    );
    let label = msg.actions[0].label().to_ascii_lowercase();
    for grant in ["grant", "allow", "enable", "turn on"] {
        assert!(!label.contains(grant), "{label:?} reads as granting");
    }
    assert_eq!(msg.actions[0].label(), "Open Settings");
    assert_eq!(msg.hold, Hold::asking(aterm_messages::HOLD_ASK));
    assert_eq!(msg.key.as_deref(), Some(KEY_FILE_ACCESS));
    assert_eq!(msg.severity, Severity::Info, "a question, never an alarm");
    assert_eq!(msg.tag, tags::PRIVACY);
    // 6. The route in words is behind Details, whole.
    assert!(
        msg.detail.iter().any(|l| l
            == crate::menu::privacy_settings_path_words(crate::menu::PrivacyPane::FullDiskAccess)),
        "{:?}",
        msg.detail
    );
}

/// THE FULL DISK ACCESS ENTRY SAYS THE CARD'S WORDS AND NO OTHERS
/// (design-exceptions §8.2, §9.2): its `what`, reason and action are the
/// card's own consts, imported and never retyped, its three `more` lines
/// are the grant, the follow-up and the hedge, and it keeps the same
/// fences as the row it replaced. One button (`Open Settings`, which opens
/// the pane and grants nothing), no `Not now` (the dismissal is the
/// decline), a look and never an alarm, the owner's own choice to hold
/// for a month. A regression (a verified grant later reads off) is a
/// different cause word, so a different fact and news once; the keys the
/// grant clears name exactly the two the builder can raise.
#[test]
fn the_file_access_entry_is_honest_and_fence_clean() {
    use crate::consent_card::{
        FDA_AFTER, FDA_DETAIL, FDA_GRANTS, FDA_THEN, FDA_WHAT, FDA_WHAT_DENIED, FDA_WHAT_REGRESSED,
        FDA_WHY, NeedCause, NeedWords,
    };
    let words = NeedWords::sample();
    let entry = file_access_entry(&words);
    assert_eq!(entry.key(), "privacy.fda.needed");
    let msg = entry.message().expect("the words pass the rule");
    assert_eq!(msg.title, FDA_WHAT, "the entry carries the const");
    // WHY first, then the one action, then what the switch GRANTS, what
    // aterm does next, and the hedge last.
    assert_eq!(
        msg.detail,
        [FDA_WHY, FDA_THEN, FDA_GRANTS, FDA_AFTER, FDA_DETAIL],
        "{:?}",
        msg.detail
    );
    assert_fda_copy_is_fence_clean(std::iter::once(&msg.title).chain(msg.detail.iter()));
    assert!(FDA_WHAT.chars().count() <= 48 && FDA_WHY.chars().count() <= 90);
    assert!(FDA_THEN.chars().count() <= 90 && !FDA_THEN.contains(" or "));
    // ONE button that opens the pane and grants nothing; no `Not now`.
    assert_eq!(
        msg.actions,
        [Intent::OpenSystemPane {
            pane: PANE_FULL_DISK_ACCESS.to_string()
        }]
    );
    let label = msg.actions[0].label().to_ascii_lowercase();
    for grant in ["grant", "allow", "enable", "turn on"] {
        assert!(!label.contains(grant), "{label:?} reads as granting");
    }
    assert_eq!(msg.actions[0].label(), "Open Settings");
    assert_eq!(msg.severity, Severity::Info, "a look, never an alarm");
    assert_eq!(msg.tag, tags::PRIVACY);
    assert!(
        matches!(
            msg.need(),
            aterm_messages::need::Need::Later(ask) if ask.resurface() == Resurface::CHOICE
        ),
        "a dismissal holds a month, then longer"
    );
    // The regression is another fact, titled as what happened.
    let regressed = file_access_entry(&NeedWords {
        regressed: true,
        ..words
    });
    assert_eq!(regressed.key(), "privacy.fda.regressed");
    let regressed_msg = regressed.message().expect("the words pass the rule");
    assert_eq!(regressed_msg.title, FDA_WHAT_REGRESSED);
    assert_fda_copy_is_fence_clean(std::iter::once(&regressed_msg.title));
    // A refused read is no prompt: its title says macOS refused aterm.
    let denied = file_access_entry(&NeedWords {
        cause: NeedCause::ProtectedRead { session: None },
        ..words
    });
    assert_eq!(denied.key(), "privacy.fda.needed");
    let denied_msg = denied.message().expect("the words pass the rule");
    assert_eq!(denied_msg.title, FDA_WHAT_DENIED);
    assert_fda_copy_is_fence_clean(std::iter::once(&denied_msg.title));
    assert_eq!(
        FDA_ENTRY_KEYS,
        [entry.key().as_str(), regressed.key().as_str()],
        "what a grant clears is exactly what the builder can raise"
    );
    // No key is shared with the retired row's, the record's or any row.
    assert_ne!(entry.key(), KEY_FILE_ACCESS);
}

/// R14 UNDER THE OWNER'S ATTENTION RULE (2026-09-23): a failure of the
/// person's own gesture is a row — Error, a short `HOLD_GESTURE` — with a
/// title of a few words and the WHOLE error behind Details, never cut.
/// No key: each is an event of its own.
#[test]
fn a_failed_gesture_is_a_short_row_with_the_whole_error_behind_details() {
    let long = "x".repeat(200);
    let msg = new_tab_failed(&long);
    assert_eq!(msg.title, "Couldn't open a new tab");
    assert_eq!(
        msg.detail,
        std::slice::from_ref(&long),
        "the error whole, not cut at 160"
    );
    assert!(
        !msg.excerpt,
        "a spawn's error does not change what the person does"
    );
    for msg in gesture_failures() {
        assert_eq!(msg.severity, Severity::Error, "{msg:?}");
        assert_eq!(msg.hold, Hold::For(HOLD_GESTURE), "{msg:?}");
        assert!(msg.key.is_none(), "an event, not a slot: {msg:?}");
        assert!(
            msg.title.split_whitespace().count() <= 5,
            "a few words: {:?}",
            msg.title
        );
        assert!(!msg.title.contains(':'), "no clause: {:?}", msg.title);
        assert!(!msg.detail.is_empty(), "the reason is behind Details");
    }
    let refused = split_refused(
        "Split refused: this pane is 20x5 cells; a vertical split needs at least 20x7",
    );
    assert_eq!(refused.title, "Couldn't split the pane");
    assert_eq!(
        refused.detail,
        [
            "pane 20\u{00d7}5, needs 20\u{00d7}7",
            "this pane is 20x5 cells; a vertical split needs at least 20x7"
        ],
        "the sizes are the excerpt; the sentence rides whole behind them"
    );
    assert_eq!(
        split_refused("Split refused: this pane's size could not be measured").detail,
        ["this pane's size could not be measured"],
        "another shape is its own excerpt"
    );
    assert_eq!(keystrokes_dropped(3).title, "Keystrokes dropped");
    assert_eq!(
        keystrokes_dropped(3).detail[0],
        "retype the last 3 keys",
        "what to do is the excerpt"
    );
    assert_eq!(keystrokes_dropped(1).detail[0], "retype the last key");
    assert!(keystrokes_dropped(3).excerpt);
    assert!(
        !restore_stopped_early().excerpt,
        "its line restates the title: behind Details"
    );
}

/// THE REST OF THE RETIRED TOAST UNDER THE ATTENTION RULE: a crippled
/// install is a row only when there is something to fix; the grant and the
/// connection disclosure are records that never touch the glass.
#[test]
fn the_rest_of_the_retired_toast_is_rows_only_where_there_is_something_to_fix() {
    // R21 — a row only where the person has something to fix.
    use aterm_update::which_copy::InstallPosture;
    assert!(install_posture(InstallPosture::Installed).is_none());
    assert!(install_posture(InstallPosture::NotABundle).is_none());
    let dmg = install_posture(InstallPosture::MountedImage).expect("a fix");
    assert_eq!(
        dmg.title, "Move aterm to Applications",
        "the instruction is the title"
    );
    assert_eq!(dmg.detail[0], "running from a mounted volume");
    assert!(
        dmg.detail[1].starts_with("Drag aterm to Applications and open it from there")
            && dmg.detail[1].contains("cannot update itself"),
        "the remedy whole: {:?}",
        dmg.detail
    );
    assert_eq!(
        dmg.detail.len(),
        2,
        "where and the remedy; the summary would say the first line again: {:?}",
        dmg.detail
    );
    assert_eq!(dmg.severity, Severity::Warn);
    assert_eq!(dmg.tag, tags::SYSTEM, "not an ALab tool (ruling 309)");
    assert!(
        dmg.actions.is_empty(),
        "no page of aterm's performs the fix"
    );
    assert_eq!(dmg.key.as_deref(), Some(KEY_INSTALL_POSTURE));
    let moved = install_posture(InstallPosture::Translocated).expect("a fix");
    assert_eq!(moved.title, "Move aterm to Applications");
    assert_eq!(moved.detail[0], "running from a temporary copy");
    // R17, R22 — records (R20 went with the sealed seed, Phase 5).
    for record in [
        file_access_granted(),
        session_connection_created(&crate::connections::first_use_notice_text(
            crate::connections::ConnectedSpawnKind::Controlled,
        )),
    ] {
        assert_eq!(record.hold, Hold::LogOnly, "{record:?}");
    }
    assert_eq!(file_access_granted().key.as_deref(), Some(KEY_FILE_ACCESS));
    let connected = session_connection_created(&crate::connections::first_use_notice_text(
        crate::connections::ConnectedSpawnKind::Controlled,
    ));
    assert_eq!(connected.title, "Session connection created");
    assert_eq!(connected.tag, tags::SESSION);
    assert!(
        connected.detail[0].starts_with("this session can now type into"),
        "{:?}",
        connected.detail
    );
    assert!(
        connected.detail[1].starts_with("Disconnect"),
        "{:?}",
        connected.detail
    );
}

/// The artifact's path is a detail line (whole, absolute) behind Details,
/// never the title — the title is the fact, the path is where to look —
/// and never on the glass (design ruling 74: `detail[0]` says a report
/// was saved, in words, and even that is not painted, ruling 145); the
/// head follows it; `Open log` carries the same path.
#[test]
fn the_crash_row_names_the_artifact_in_its_detail_not_its_title() {
    let path = "/Users/_an/Library/Logs/aterm/crash-signal-1-1.log.seen";
    let msg = crash_message(&crate::logging::CrashEvidence {
        path: std::path::PathBuf::from(path),
        head: vec!["aterm-gui 0.1.0 crashed".into(), "fatal signal 11".into()],
    });
    assert_eq!(msg.tag, tags::CRASH);
    assert_eq!(msg.severity, Severity::Error);
    assert_eq!(msg.title, "aterm crashed last time");
    assert!(!msg.title.contains('/'), "the title names no path");
    assert!(!msg.excerpt, "the row paints its title alone");
    assert_eq!(msg.detail[0], CRASH_EXCERPT, "the first line is words");
    assert!(
        !msg.detail[0].contains('/') && !msg.detail[0].contains(".log"),
        "the first line names no file: {}",
        msg.detail[0]
    );
    assert_eq!(msg.detail[1], format!("crash log at {path}"));
    assert_eq!(
        &msg.detail[2..],
        ["aterm-gui 0.1.0 crashed", "fatal signal 11"]
    );
    assert_eq!(
        msg.actions,
        [Intent::OpenPath {
            path: path.to_string()
        }]
    );
    assert_eq!(msg.hold, Hold::For(HOLD_LAUNCH));
    assert_eq!(msg.key.as_deref(), Some(KEY_CRASH));
}

/// The killed row says what happened without a path in its title, says
/// there is no crash report (the marker is empty — `Open log` goes to the
/// log, never to that empty file), and takes the crash row's slot.
#[test]
fn the_killed_row_opens_the_log_and_shares_the_crash_slot() {
    let log = "/Users/_an/Library/Logs/aterm/aterm.log";
    let marker = "/Users/_an/Library/Logs/aterm/crash-marker-9-9-app.log.seen";
    let msg = killed_message(&crate::logging::KillEvidence {
        marker: std::path::PathBuf::from(marker),
        log: std::path::PathBuf::from(log),
    });
    assert_eq!(msg.tag, tags::CRASH);
    assert_eq!(msg.severity, Severity::Warn);
    assert_eq!(msg.title, "aterm was stopped last time");
    assert_eq!(msg.detail[0], KILLED_EXCERPT);
    assert!(msg.detail.iter().any(|l| l.contains(log)));
    // The marker is how aterm knew, not something the reader acts on.
    assert!(!msg.detail.iter().any(|l| l.contains(marker)));
    assert_eq!(
        msg.actions,
        [Intent::OpenPath {
            path: log.to_string()
        }]
    );
    assert_eq!(msg.key.as_deref(), Some(KEY_CRASH));
}

/// A crash's and a kill's evidence, as the keeper's row receives them.
fn keeper_evidence_fixtures() -> (crate::logging::CrashEvidence, crate::logging::KillEvidence) {
    (
        crate::logging::CrashEvidence {
            path: std::path::PathBuf::from(
                "/Users/_an/Library/Logs/aterm/crash-signal-1-1.log.seen",
            ),
            head: vec!["aterm-gui 0.1.0 crashed".into()],
        },
        crate::logging::KillEvidence {
            marker: std::path::PathBuf::from(
                "/Users/_an/Library/Logs/aterm/crash-marker-9-9-app.log.seen",
            ),
            log: std::path::PathBuf::from("/Users/_an/Library/Logs/aterm/aterm.log"),
        },
    )
}

/// The keeper's recovery row takes the crash row's slot, so it says
/// where that row's evidence is — the crash log, else the killed run's
/// log — while `End sessions` stays its one action; and it never claims
/// a screen was redrawn, only that its program was asked to.
#[test]
fn the_keeper_row_keeps_the_evidence_of_the_row_it_replaces() {
    let (crash, killed) = keeper_evidence_fixtures();
    let bare = keeper_recovered_message(2, 1, None, None);
    assert_eq!(bare.key.as_deref(), Some(KEY_CRASH));
    assert_eq!(bare.actions, [Intent::EndRecovered]);
    assert_eq!(
        bare.detail,
        [
            "2 sessions kept running and were reattached",
            KEEPER_SCREENS,
            "1 session could not be reattached and was ended",
        ]
    );
    assert!(!KEEPER_SCREENS.contains("was redrawn"));
    let crashed = keeper_recovered_message(1, 0, Some(&crash), Some(&killed));
    assert_eq!(
        crashed.detail.last().map(String::as_str),
        Some("crash log at /Users/_an/Library/Logs/aterm/crash-signal-1-1.log.seen"),
        "a crash names its log, and wins over a kill"
    );
    assert_eq!(crashed.actions, [Intent::EndRecovered]);
    let stopped = keeper_recovered_message(1, 0, None, Some(&killed));
    assert_eq!(
        stopped.detail.last().map(String::as_str),
        Some("aterm's last lines are in /Users/_an/Library/Logs/aterm/aterm.log"),
        "named: after the screens line, `its` would read as a session's"
    );
}

/// A reopened crash journal of `class`: two tabs in one window.
fn reopened_fixture(class: crate::crash_journal::DeathClass) -> crate::crash_journal::Reopened {
    crate::crash_journal::Reopened {
        manifest: crate::crash_journal::fixtures::layout(&[("/a", "zsh"), ("/b", "vim")]),
        class,
        sources: vec![crate::crash_journal::JournalId { pid: 9, nanos: 9 }],
        writers: vec![9],
        handed: Vec::new(),
        programs: None,
        second_chance: false,
    }
}

/// The fixture with the journal saying which leaves ran a program (D16):
/// `(tab, program)` in its one window.
fn reopened_with_programs(
    class: crate::crash_journal::DeathClass,
    programs: &[(u32, &str)],
) -> crate::crash_journal::Reopened {
    crate::crash_journal::Reopened {
        programs: Some(
            programs
                .iter()
                .map(|(tab, program)| crate::crash_journal::LeafProgram {
                    window: 0,
                    tab: *tab,
                    program: (*program).to_string(),
                    agent: false,
                    again: false,
                })
                .collect(),
        ),
        ..reopened_fixture(class)
    }
}

/// [`reopened_with_programs`] with Claude hosted in tab 2 (ruling 293):
/// its leaf carries a relaunch record naming a conversation, and the
/// journal says the agent ran there.
fn reopened_with_agent(
    class: crate::crash_journal::DeathClass,
    programs: &[(u32, &str)],
) -> crate::crash_journal::Reopened {
    let mut reopened = reopened_with_programs(class, programs);
    let claude = crate::restore::AgentRestore {
        pid: 4242,
        start: "Sat Sep 27 01:02:03 2026".into(),
        program: "/opt/claude/bin/claude".into(),
        argv: vec!["/opt/claude/bin/claude".into()],
        session: Some("0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c".into()),
        cwd: "/b".into(),
        version: None,
        codex: None,
    };
    reopened
        .manifest
        .fill_agents(&|id| (id == 1).then(|| claude.clone()));
    if let Some(programs) = reopened.programs.as_mut() {
        programs.push(crate::crash_journal::LeafProgram {
            window: 0,
            tab: 1,
            program: "claude".to_string(),
            agent: true,
            again: false,
        });
    }
    reopened
}

/// RULING 293: a CRASH whose only program was an agent the relaunch
/// brings back keeps its row (aterm failed), in true words — aterm
/// starts Claude again, only the scrollback is gone; a journal that did not say what
/// ran still says the agent resumes in today's loss sentence. The row
/// that says restored agents did NOT come back names the tab and the
/// reason in a person's words, never a step word, and how to resume by
/// hand. NEGATIVE CONTROL: with the relaunch off, the crash names claude
/// as lost.
#[test]
fn a_crash_that_took_only_a_resumed_agent_says_so_and_a_failed_resume_is_named() {
    use crate::crash_journal::DeathClass;
    use aterm_agent::harness::relaunch::Outcome;
    let crashed = reopened_with_agent(DeathClass::Signal, &[]);
    let row = journal_reopened_message(
        &crashed,
        None,
        None,
        None,
        true,
        PanesWithoutFolder::NONE,
        0,
    );
    assert_eq!(row.severity, Severity::Error);
    assert_ne!(row.hold, Hold::LogOnly);
    assert_eq!(row.title, "Tabs restored after aterm crashed");
    assert_eq!(
        row.detail[0],
        "aterm starts Claude again on its conversation in tab 2; only the scrollback did not \
         survive"
    );
    let off = journal_reopened_message(
        &crashed,
        None,
        None,
        None,
        false,
        PanesWithoutFolder::NONE,
        0,
    );
    assert_eq!(off.title, "Tabs restored, claude lost");
    let older = crate::crash_journal::Reopened {
        programs: None,
        ..crashed.clone()
    };
    let row = journal_reopened_message(&older, None, None, None, true, PanesWithoutFolder::NONE, 0);
    // One sentence (ruling 314): the agent rides the loss line.
    assert_eq!(
        row.detail[0],
        format!("{JOURNAL_LOSS}; aterm starts Claude again on its conversation in tab 2")
    );
    assert!(row.detail[1..].iter().all(|l| !l.contains("starts Claude")));

    // BEHIND A QUIT LAYOUT (finding 48's review): the last quit's one
    // window opens first, so the reopened desk's tab 2 is in window 2 on
    // screen — named, and the quit's window said to have come back too.
    // RED before: `… in tab 2`, no window, nothing about the quit's.
    let behind = journal_reopened_message(
        &crashed,
        None,
        None,
        None,
        true,
        PanesWithoutFolder::NONE,
        1,
    );
    assert_eq!(
        behind.detail[0],
        "aterm starts Claude again on its conversation in tab 2 of window 2; only the \
         scrollback did not survive"
    );
    assert!(
        behind.detail.iter().any(|line| line
            == "the 1 window saved when aterm last quit came back too, first; this one is \
                window 2"),
        "{:?}",
        behind.detail
    );
    assert!(
        row.detail.iter().all(|line| !line.contains("last quit")),
        "no quit layout, no line: {:?}",
        row.detail
    );
    assert_eq!(
        journal_behind_quit_line(3, 2).as_deref(),
        Some(
            "the 3 windows saved when aterm last quit came back too, first; these are windows 4 to 5"
        )
    );

    assert_eq!(
        journal_resumed_line(&[(0, 0), (0, 2)], 1),
        "aterm starts Claude again on its conversations in tabs 1 and 3"
    );
    assert_eq!(
        journal_resumed_line(&[(0, 0), (1, 0)], 2),
        "aterm starts Claude again on its conversations in tab 1 of window 1 and tab 1 of \
         window 2"
    );
    assert_eq!(
        journal_resumed_line(&[(0, 0), (0, 1), (0, 2), (0, 3)], 1),
        "aterm starts Claude again on its conversations in 4 tabs"
    );
    assert_eq!(journal_resumed_line(&[], 1), "");

    let log = std::path::Path::new("/Users/_an/Library/Logs/aterm/aterm.log");
    let one = restored_agents_not_resumed(
        &[("in tab 2".to_string(), Outcome::Cannot("shell-gone".into()))],
        Some(log),
    );
    assert_eq!(one.title, "Couldn't resume Claude in tab 2");
    assert_eq!(attention(&one), Ok(Attention::Failure));
    assert_eq!(one.tag, tags::HARNESS);
    assert_eq!(
        one.detail[0],
        "its tab's shell had ended, so its conversation did not come back after aterm \
         stopped: type claude --resume there to pick it up again"
    );
    // DAY SIX, D31/D32: an agent that started and did not pick its
    // conversation up is said as that — the tab was ready — and the
    // remedy is in the plain sentence, not the technical block. A prompt
    // that refused the line is said as that too; a tab that never came
    // to its prompt stays "not ready" (the control).
    for step in ["failed:no-resume", "wait:resume"] {
        let started = restored_agents_not_resumed(
            &[("in tab 1".to_string(), Outcome::NotYet(step.into()))],
            Some(log),
        );
        assert_eq!(
            started.detail[0],
            "Claude started in the tab but did not pick its conversation up after aterm \
             stopped: type claude --resume there to pick it up again",
            "{step}"
        );
        assert!(
            started.detail[1..]
                .iter()
                .all(|l| !l.contains("--resume") && !l.contains("not ready")),
            "{:?}",
            started.detail
        );
    }
    assert!(
        not_resumed_why(&Outcome::NotYet("failed:relaunch:ERR-halted".into()))
            .starts_with("its prompt refused")
    );
    assert_eq!(
        not_resumed_why(&Outcome::NotYet("wait:shell-prompt".into())),
        "its tab was not ready for it in time"
    );
    let many = restored_agents_not_resumed(
        &[
            (
                "in tab 1".to_string(),
                Outcome::NotYet("wait:tab-not-live".into()),
            ),
            ("in tab 3".to_string(), Outcome::Left("not-exited".into())),
            ("in tab 4".to_string(), Outcome::Cannot("inert".into())),
        ],
        None,
    );
    assert_eq!(many.title, "Couldn't resume Claude in 3 tabs");
    assert_eq!(attention(&many), Ok(Attention::Failure));
    assert!(many.detail[0].ends_with("type claude --resume in each tab to pick one up again"));
    assert!(
        many.detail[1].starts_with("in tab 1, "),
        "{:?}",
        many.detail
    );
    for line in &many.detail {
        for word in ["wait:", "not-exited", "inert", "shell-gone", "(", "harness"] {
            assert!(!line.contains(word), "{word:?} in {line:?}");
        }
    }
}

fn journal_note_fixtures() -> Vec<crate::crash_journal::Note> {
    let id = crate::crash_journal::JournalId { pid: 9, nanos: 9 };
    vec![
        crate::crash_journal::Note::Unreadable {
            id,
            class: crate::crash_journal::DeathClass::Killed,
            path: Some(std::path::PathBuf::from(
                "/Users/_an/Library/Application Support/aterm/journal-9-9.unreadable",
            )),
            error: "its layout is not one this build reads".into(),
        },
        crate::crash_journal::Note::Relapsed {
            id,
            class: crate::crash_journal::DeathClass::Signal,
            windows: 1,
            tabs: 2,
            lost: Some(crate::crash_journal::Lost {
                tabs: 1,
                named: vec!["Python".to_string()],
                unnamed: 0,
            }),
        },
        crate::crash_journal::Note::Relapsed {
            id,
            class: crate::crash_journal::DeathClass::Killed,
            windows: 1,
            tabs: 2,
            lost: Some(crate::crash_journal::Lost::default()),
        },
    ]
}

/// THE REOPENED LAYOUT'S ROW (PTY keeper P1) says what came back and what
/// did not — the tabs and their folders, never the programs or their
/// scrollback — in its title and its detail, takes the crash row's slot,
/// and carries the evidence the row it replaces would have: the crash log
/// and its head (an error), or the killed run's log (a warning).
#[test]
fn the_reopened_journal_row_says_what_came_back_and_what_did_not() {
    use crate::crash_journal::DeathClass;
    let path = "/Users/_an/Library/Logs/aterm/crash-7.log.seen";
    let crash = crate::logging::CrashEvidence {
        path: std::path::PathBuf::from(path),
        head: vec!["aterm-gui 0.1.0 crashed".into()],
    };
    let msg = journal_reopened_message(
        &reopened_fixture(DeathClass::Panic),
        Some(&crash),
        None,
        None,
        false,
        PanesWithoutFolder::NONE,
        0,
    );
    assert_eq!(msg.title, "Tabs restored, programs lost");
    assert_eq!(msg.severity, Severity::Error);
    assert_eq!(msg.key.as_deref(), Some(KEY_CRASH));
    assert_eq!(msg.tag, tags::CRASH);
    // The answer is the sentence (day five, D19); the class a detail.
    assert_eq!(msg.detail[0], JOURNAL_LOSS);
    assert_eq!(
        msg.detail[1],
        "2 tabs in 1 window restored in their folders from its crash journal"
    );
    assert_eq!(msg.detail[2], DeathClass::Panic.sentence());
    assert_eq!(msg.detail[3], format!("crash log at {path}"));
    assert_eq!(msg.detail[4], "aterm-gui 0.1.0 crashed");
    assert_eq!(
        msg.actions,
        [Intent::OpenPath {
            path: path.to_string()
        }]
    );

    let log = "/Users/_an/Library/Logs/aterm/aterm.log";
    let killed = crate::logging::KillEvidence {
        marker: std::path::PathBuf::from("/l/crash-marker-7-7-app.log.seen"),
        log: std::path::PathBuf::from(log),
    };
    let msg = journal_reopened_message(
        &reopened_fixture(DeathClass::Killed),
        None,
        Some(&killed),
        None,
        false,
        PanesWithoutFolder::NONE,
        0,
    );
    assert_eq!(msg.severity, Severity::Warn);
    assert_eq!(msg.tag, tags::SESSION, "a kill is no crash");
    assert!(msg.detail.iter().any(|line| line.contains(log)));
    assert_eq!(
        msg.actions,
        [Intent::OpenPath {
            path: log.to_string()
        }]
    );
    // A development start's kill has no kill row to carry: the log dir's
    // log is named instead.
    let msg = journal_reopened_message(
        &reopened_fixture(DeathClass::Killed),
        None,
        None,
        Some(std::path::Path::new(log)),
        false,
        PanesWithoutFolder::NONE,
        0,
    );
    assert_eq!(
        msg.actions,
        [Intent::OpenPath {
            path: log.to_string()
        }]
    );
    assert_eq!(attention(&msg), Ok(Attention::Failure));
}

/// D16 (ruling 282) — A QUIET RELAUNCH: a killed run whose tabs all sat at
/// their prompts lost nothing a person ran, so the reopened layout is a
/// RECORD that says so, never a held warning. The negative controls: a
/// program that ran keeps the warning and names itself, a crash keeps its
/// row whatever ran, and a journal that did not say (an older writer's)
/// reads exactly as before.
#[test]
fn a_relaunch_that_lost_nothing_is_a_record_and_a_lost_program_is_named() {
    use crate::crash_journal::DeathClass;
    let log = std::path::Path::new("/Users/_an/Library/Logs/aterm/aterm.log");
    let quiet = journal_reopened_message(
        &reopened_with_programs(DeathClass::Killed, &[]),
        None,
        None,
        Some(log),
        false,
        PanesWithoutFolder::NONE,
        0,
    );
    assert_eq!(quiet.title, "Tabs restored after aterm stopped");
    assert_eq!(quiet.severity, Severity::Info);
    assert_eq!(quiet.hold, Hold::LogOnly);
    assert_eq!(attention(&quiet), Ok(Attention::Record));
    assert_eq!(quiet.detail[0], JOURNAL_NOTHING_RAN);
    assert_eq!(quiet.tag, tags::SESSION);
    assert!(
        quiet.detail.iter().all(|line| !line.contains("programs")),
        "{:?}",
        quiet.detail
    );

    // Negative control: one tab ran vim — the warning stands and names it.
    let lost = journal_reopened_message(
        &reopened_with_programs(DeathClass::Killed, &[(1, "vim")]),
        None,
        None,
        Some(log),
        false,
        PanesWithoutFolder::NONE,
        0,
    );
    assert_eq!(lost.title, "Tabs restored, vim lost");
    assert_eq!(lost.severity, Severity::Warn);
    assert_eq!(lost.hold, Hold::For(HOLD_LAUNCH));
    assert_eq!(
        lost.detail[0],
        "vim was running in 1 of 2 tabs and did not survive, nor did the scrollback"
    );
    // Two tabs, one name unresolved: counted by tab, the known name kept.
    let both = journal_reopened_message(
        &reopened_with_programs(DeathClass::Killed, &[(0, "claude"), (1, "")]),
        None,
        None,
        Some(log),
        false,
        PanesWithoutFolder::NONE,
        0,
    );
    assert_eq!(both.title, "Tabs restored, programs lost");
    assert_eq!(
        both.detail[0],
        "claude and another program were running in both tabs and did not survive, nor \
         did the scrollback"
    );
    // A leaf the reopened layout does not hold counts for nothing.
    let stray = journal_reopened_message(
        &reopened_with_programs(DeathClass::Killed, &[(7, "vim")]),
        None,
        None,
        Some(log),
        false,
        PanesWithoutFolder::NONE,
        0,
    );
    assert_eq!(stray.hold, Hold::LogOnly);

    // A crash keeps its row (aterm failed), with true words.
    let crashed = journal_reopened_message(
        &reopened_with_programs(DeathClass::Panic, &[]),
        None,
        None,
        Some(log),
        false,
        PanesWithoutFolder::NONE,
        0,
    );
    assert_eq!(crashed.title, "Tabs restored after aterm crashed");
    assert_eq!(crashed.severity, Severity::Error);
    assert_eq!(crashed.hold, Hold::For(HOLD_LAUNCH));
    assert_eq!(crashed.detail[0], JOURNAL_NOTHING_RAN);
    // One program whose name is unknown: counted, not named.
    let unnamed = journal_reopened_message(
        &reopened_with_programs(DeathClass::Killed, &[(0, "")]),
        None,
        None,
        Some(log),
        false,
        PanesWithoutFolder::NONE,
        0,
    );
    assert_eq!(unnamed.title, "Tabs restored, a program lost");

    // An older journal (no programs said): today's words, today's weight.
    let older = journal_reopened_message(
        &reopened_fixture(DeathClass::Killed),
        None,
        None,
        Some(log),
        false,
        PanesWithoutFolder::NONE,
        0,
    );
    assert_eq!(older.title, "Tabs restored, programs lost");
    assert_eq!(older.severity, Severity::Warn);
    assert_eq!(older.detail[0], JOURNAL_LOSS);
}

#[test]
fn a_program_list_reads_as_a_person_would_say_it() {
    let names = |list: &[&str]| list.iter().map(|n| (*n).to_string()).collect::<Vec<_>>();
    assert_eq!(listed(&names(&["vim"]), 0), "vim");
    assert_eq!(listed(&names(&["vim", "claude"]), 0), "vim and claude");
    assert_eq!(
        listed(&names(&["vim", "claude", "less"]), 0),
        "vim, claude and less"
    );
    assert_eq!(
        listed(&names(&["a", "b", "c", "d", "e"]), 0),
        "a, b, c and 2 other programs"
    );
    assert_eq!(listed(&names(&[]), 1), "a program");
    assert_eq!(listed(&names(&[]), 2), "2 programs");
    assert_eq!(listed(&names(&["vim"]), 1), "vim and another program");
}

/// A crashed run's journal that was taken and NOT reopened is said, with
/// the reason — unreadable, or skipped by the brake — and, skipped, what it
/// held and what ran in it, and `Open log` (day five, D18: the row called a
/// kill a crash and named nothing). A crash files under `crash`, a second
/// stop under `session`.
#[test]
fn a_journal_not_reopened_is_said_with_its_reason() {
    let log = "/Users/_an/Library/Logs/aterm/aterm.log";
    let rows: Vec<Message> = journal_note_fixtures()
        .iter()
        .map(|note| journal_note_message(note, Some(std::path::Path::new(log))))
        .collect();
    assert_eq!(rows[0].title, "Couldn't restore your last tabs");
    assert!(rows[0].detail[0].contains("could not be read"));
    assert!(
        rows[0]
            .detail
            .iter()
            .any(|line| line.contains("journal-9-9.unreadable"))
    );
    assert_eq!(rows[1].title, "Couldn't restore tabs after a crash");
    assert_eq!(
        rows[1].detail[0],
        "2 tabs in 1 window were not restored: aterm crashed within 90 s of restoring them, \
         so they were skipped in case they caused it"
    );
    assert_eq!(rows[1].detail[1], "Python was running in 1 of 2 tabs");
    assert_eq!(rows[1].tag, tags::CRASH);
    assert_eq!(rows[2].title, "Couldn't restore tabs after two stops");
    assert!(
        rows[2].detail[0].contains("stopped twice in a row within 90 s"),
        "{:?}",
        rows[2].detail
    );
    assert_eq!(rows[2].detail[1], "nothing was running in them");
    assert_eq!(rows[2].tag, tags::SESSION);
    assert!(
        !rows[2].title.contains("crash") && !rows[2].detail[0].contains("crash"),
        "a kill is never called a crash"
    );
    for row in &rows {
        assert_eq!(
            row.actions,
            [Intent::OpenPath {
                path: log.to_string()
            }]
        );
        assert_eq!(row.severity, Severity::Warn);
        assert_eq!(row.key.as_deref(), Some(KEY_CRASH_JOURNAL));
        assert_eq!(attention(row), Ok(Attention::Failure));
    }
}

/// One message per family: a TERSE title — singular for one sentence, a
/// count for several — the first sentence's head as `detail[0]`, every
/// sentence whole behind it (the excerpt not repeated when it IS the
/// sentence), a repeat folded, past the line cap the roll-up, and the
/// family's key and the editor capsule on every one (design §10.3 C1–C9,
/// §10.5 H9; this replaces the ruling-29 head tests).
#[test]
fn config_family_titles_are_terse_and_the_first_sentence_is_the_excerpt() {
    let mut warns = ConfigWarnings::default();
    assert!(
        warns.clone().into_messages().is_empty(),
        "nothing collected, nothing posted"
    );
    warns.push(
        ConfigFamily::Keybindings,
        "config keybindings: skipping \"ctrl+x\": unknown action \"foo\"".into(),
    );
    warns.push(
        ConfigFamily::Keybindings,
        "config keybindings: skipping \"ctrl+x\": unknown action \"foo\"".into(),
    );
    warns.push(
        ConfigFamily::IgnoredKeys,
        "config line 3: unknown key \"windw_padding\"".into(),
    );
    warns.push(
        ConfigFamily::IgnoredKeys,
        "config line 9: unknown key \"colums\"".into(),
    );
    warns.push(
        ConfigFamily::Restart,
        "gpu applies on next launch (the renderer backend is chosen at startup)".into(),
    );
    assert_eq!(
        warns.sentences().collect::<Vec<_>>(),
        [
            "config keybindings: skipping \"ctrl+x\": unknown action \"foo\"",
            "config line 3: unknown key \"windw_padding\"",
            "config line 9: unknown key \"colums\"",
            "gpu applies on next launch (the renderer backend is chosen at startup)",
        ],
        "the echo is verbatim and in collection order, repeats folded"
    );
    assert_eq!(warns.told().len(), 4);
    let msgs = warns.into_messages();
    assert_eq!(msgs.len(), 3, "one per family");
    let kb = &msgs[0];
    assert_eq!(kb.title, "Keybinding skipped");
    assert_eq!(
        kb.detail,
        [
            "ctrl+x: no action foo",
            "\"ctrl+x\": unknown action \"foo\""
        ],
        "the chord is the subject, unquoted; the title already says skipped"
    );
    assert_eq!(kb.severity, Severity::Warn);
    assert_eq!(kb.key.as_deref(), Some("config.keybindings"));
    assert_eq!(kb.actions, [Intent::OpenConfigEditor { line: None }]);
    let keys = &msgs[1];
    assert_eq!(keys.title, "2 unknown settings");
    assert_eq!(
        keys.detail,
        [
            "line 3: unknown key \"windw_padding\"",
            "line 9: unknown key \"colums\""
        ]
    );
    let restart = &msgs[2];
    assert_eq!(restart.hold, Hold::LogOnly, "a waiting edit is a record");
    assert_eq!(restart.title, "GPU setting applies after restart");
    assert_eq!(
        restart.detail,
        ["gpu applies on next launch (the renderer backend is chosen at startup)"],
        "the sentence behind the plain title"
    );
    assert_eq!(restart.key.as_deref(), Some("config.restart"));

    // Every family's title, both forms.
    for (family, one, many) in [
        (
            ConfigFamily::Keybindings,
            "Keybinding skipped",
            "3 keybindings skipped",
        ),
        (
            ConfigFamily::IgnoredKeys,
            "Unknown setting",
            "3 unknown settings",
        ),
        (
            ConfigFamily::RetiredKeys,
            "Retired setting",
            "3 retired settings",
        ),
        (
            ConfigFamily::UnacceptedValues,
            "Couldn't use a setting's value",
            "Couldn't use 3 settings' values",
        ),
        (
            ConfigFamily::CursorTrail,
            "Couldn't apply the cursor trail",
            "3 errors in cursor trail settings",
        ),
        (
            ConfigFamily::Fonts,
            "Couldn't apply the font",
            "Couldn't apply 3 fonts",
        ),
        (
            ConfigFamily::Assets,
            "Couldn't load the image",
            "Couldn't load 3 images",
        ),
        (
            ConfigFamily::SecureKeyboard,
            "Couldn't change Secure Keyboard Entry",
            "Couldn't change Secure Keyboard Entry",
        ),
    ] {
        assert_eq!(family.title(1), one, "{family:?}");
        assert_eq!(family.title(3), many, "{family:?}");
    }

    // A font verdict's head sheds `; ignored`, and the sentence rides
    // whole behind it.
    let face = "f".repeat(20);
    let mut one = ConfigWarnings::default();
    one.push(
        ConfigFamily::Fonts,
        format!("config font_family: \"{face}\" is not an admissible font (not found); ignored"),
    );
    let msg = one.into_messages().remove(0);
    assert_eq!(msg.title, "Couldn't apply the font");
    // The excerpt in a person's words (ruling 306): no key, no quoting,
    // no `admissible`.
    assert_eq!(msg.detail[0], format!("no font named {face}"));
    assert_eq!(
        msg.detail[1],
        format!("font_family: \"{face}\" is not an admissible font (not found); ignored")
    );
    let font = font_family_rejected("font_family: \"Nope\" is not an admissible font");
    assert_eq!(font.title, "Couldn't apply the font");
    assert_eq!(
        font.detail,
        [
            "no font named Nope",
            "font_family: \"Nope\" is not an admissible font"
        ]
    );
    assert_eq!(font.key.as_deref(), Some("config.font-family"));

    // Past the engine's line cap the last line is the roll-up.
    let mut many = ConfigWarnings::default();
    many.extend(
        ConfigFamily::IgnoredKeys,
        (0..(DETAIL_LINES_CAP + 5)).map(|i| format!("config line {i}: unknown key \"k{i}\"")),
    );
    let msg = many.into_messages().remove(0);
    assert_eq!(
        msg.title,
        format!("{} unknown settings", DETAIL_LINES_CAP + 5)
    );
    assert_eq!(msg.detail.len(), DETAIL_LINES_CAP);
    assert_eq!(
        msg.detail[DETAIL_LINES_CAP - 1],
        format!("\u{2026} and 6 more \u{2014} {VALIDATOR_HINT}")
    );
    assert_eq!(
        msg.detail[DETAIL_LINES_CAP - 2],
        format!(
            "line {}: unknown key \"k{}\"",
            DETAIL_LINES_CAP - 2,
            DETAIL_LINES_CAP - 2
        )
    );
}

/// A RETIRED SPELLING IS NOT AN UNKNOWN KEY (review of the second
/// origin/main merge, 2026-09-23; design ruling 42). atpkg still reads
/// `[packages] auto_update = false` as `enabled = false`, and
/// `seed_install` as `auto_install` while that is unset. They are their
/// own family: a record that never reaches the glass, whose words say
/// what they are. The sentences are the real ones
/// (`app_config::collect_key_notices`, the launch's and every reload's
/// seam), so the pin cannot drift from the validator's words.
#[test]
fn a_retired_spelling_is_a_record_and_never_counted_as_an_unknown_key() {
    let src = "[packages]\nauto_update = false\nseed_install = false\n";
    let mut warns = ConfigWarnings::default();
    crate::app_config::collect_key_notices(&mut warns, src);
    let msgs = warns.into_messages();
    assert_eq!(
        msgs.len(),
        1,
        "one family, and it is not the ignored keys: {msgs:?}"
    );
    let retired = &msgs[0];
    // Both of the ignored-key family's wordings are false for a spelling that
    // is still READ: refusing only the old one would let this pass vacuously
    // the day the family was reworded.
    assert!(
        !retired.title.contains("nknown")
            && !retired.title.contains("not known to this build")
            && !retired.title.contains("no effect"),
        "a working opt-out called unknown or inert: {:?}",
        retired.title
    );
    assert_eq!(retired.title, "2 retired settings");
    assert_eq!(retired.key.as_deref(), Some("config.retired-keys"));
    assert_eq!(retired.hold, Hold::LogOnly, "a record, never on glass");
    assert_eq!(retired.severity, Severity::Info, "nothing is broken");
    assert!(
        retired
            .detail
            .iter()
            .any(|l| l.contains("it is read as `enabled = false`")),
        "{:?}",
        retired.detail
    );
    assert!(
        retired
            .detail
            .iter()
            .any(|l| l.contains("the old key is still read")),
        "{:?}",
        retired.detail
    );

    // Beside a typo, the deprecated `game_font` and the retired
    // `channel`: the typo is the one key the person's setting silently
    // misses, one row; `game_font` and `auto_update`, which still apply,
    // and `channel`, which a removed feature left behind, are the record
    // (design ruling 213: nothing pressed brings a removed feature back).
    let src = "game_font = \"chunky\"\nwindw_padding = 20\n\
               [packages]\nenabled = true\nauto_update = false\nchannel = \"stable\"\n";
    let mut warns = ConfigWarnings::default();
    crate::app_config::collect_key_notices(&mut warns, src);
    assert_eq!(
        warns.sentences().count(),
        4,
        "the stderr echo still says every one: {:?}",
        warns.told()
    );
    let msgs = warns.into_messages();
    assert_eq!(msgs.len(), 2, "{msgs:?}");
    let ignored = &msgs[0];
    assert_eq!(ignored.key.as_deref(), Some("config.ignored-keys"));
    // The warning's 45 s, now said by its class (a flash at `Span::Warn`).
    assert_eq!(ignored.hold, Hold::For(aterm_messages::HOLD_WARN));
    assert_eq!(ignored.severity, Severity::Warn);
    assert_eq!(ignored.title, "Misspelled setting");
    assert!(ignored.detail.iter().any(|l| l.contains("windw_padding")));
    assert!(
        ignored.detail.iter().all(|l| !l.contains("game_font")
            && !l.contains("auto_update")
            && !l.contains("packages.channel")),
        "{:?}",
        ignored.detail
    );
    let retired = &msgs[1];
    assert_eq!(retired.key.as_deref(), Some("config.retired-keys"));
    assert_eq!(retired.hold, Hold::LogOnly);
    assert!(
        retired
            .detail
            .iter()
            .any(|l| l.contains("packages.channel does nothing")),
        "{:?}",
        retired.detail
    );
    assert!(
        retired
            .detail
            .iter()
            .any(|l| l.contains("`game_font` is deprecated"))
    );
    assert!(
        retired
            .detail
            .iter()
            .any(|l| l.contains("it still keeps automatic updates off")),
        "{:?}",
        retired.detail
    );

    // `--validate-config` still lists every key that needs an edit.
    let all = crate::native_config_language::ignored_key_warnings(src);
    assert_eq!(all.len(), 4, "{all:?}");
}

/// THE COMMONEST CONFIG WARNING — one misspelled key: the title is terse
/// and the near-miss spelling is the excerpt (the key and its suggestion,
/// without the no-effect aside), with the whole sentence behind Details.
/// The sentence is the real one — `app_config::ignored_key_notices` over
/// the text a live edit would save — so the fixture cannot drift from the
/// validator's words.
#[test]
fn a_lone_ignored_key_warning_titles_its_head_and_keeps_the_sentence_whole() {
    let notices = crate::app_config::ignored_key_notices("windw_padding = 20\n");
    assert_eq!(notices.len(), 1, "{notices:?}");
    let whole = notices[0].strip_prefix("config ").expect("the prefix");
    let mut warns = ConfigWarnings::default();
    warns.extend(ConfigFamily::IgnoredKeys, notices.clone());
    let msg = warns.into_messages().remove(0);
    assert_eq!(msg.title, "Misspelled setting");
    assert_eq!(
        msg.detail[0], "windw_padding \u{2192} window_padding",
        "the key and its near miss; the line number is behind Details"
    );
    assert_eq!(msg.detail[1], whole, "the sentence survives whole");
    assert_eq!(msg.key.as_deref(), Some("config.ignored-keys"));
    // Once a clean reload resolves it, the log says it was fixed, under
    // ✓ Success, the warning's title its first detail line (ruling 265).
    assert_eq!(msg.finished.as_deref(), Some("Misspelled setting fixed"));
    let now = aterm_messages::Instant::now();
    let mut center = aterm_messages::MessageCenter::new(aterm_messages::MessageLog::empty(), now);
    let id = center
        .post(msg.clone(), aterm_messages::WallStamp { unix_ms: 1 }, now)
        .id;
    assert_eq!(
        center.resolve_key_prefix("config.", aterm_messages::Outcome::Ok, now),
        1
    );
    let rec = center.log().get(id).unwrap();
    assert_eq!(rec.title, "Misspelled setting fixed");
    assert_eq!(
        rec.detail.first().map(String::as_str),
        Some("Misspelled setting")
    );
    assert_eq!(
        (rec.severity, rec.glyph.ch()),
        (aterm_messages::Severity::Success, '\u{2713}')
    );

    // The plain form is the bare key.
    assert_eq!(
        unknown_key_excerpt("line 4: foo_bar is unknown to this build and has no effect")
            .as_deref(),
        Some("foo_bar")
    );
    assert_eq!(
        unknown_key_excerpt("line 3: unknown key \"windw_padding\"").as_deref(),
        None,
        "neither shape: the head at a seam instead"
    );
    // A near miss sheds its no-effect aside; the head is the LONGEST that
    // fits outside a parenthesis, so it keeps the dash and the suggestion.
    let key = "k".repeat(40);
    let near = format!("line 4: {key} \u{2014} did you mean \"x\"? (no effect in this build)");
    assert_eq!(
        excerpt_head(&near, EXCERPT_CAP),
        format!("line 4: {key} \u{2014} did you mean \"x\"?")
    );
    // The plain form has no seam: it is its own head.
    let plain = format!("line 4: {key} is unknown to this build and has no effect");
    assert_eq!(excerpt_head(&plain, EXCERPT_CAP), plain);
    // A sentence with no seam inside the cap is its own excerpt.
    let wide = "x".repeat(EXCERPT_CAP + 1);
    let long = format!("{wide} (aside); tail");
    assert_eq!(excerpt_head(&long, EXCERPT_CAP), long);
    assert_eq!(excerpt_head(" (aside) only", EXCERPT_CAP), " (aside) only");
}

/// Every title a reporter here mints fits the engine's title cap
/// without being clipped: a clipped title reads as a cut sentence.
#[test]
fn every_reporter_title_fits_the_title_cap() {
    for msg in every_reporter_message() {
        assert!(
            msg.title.chars().count() <= TITLE_CAP,
            "title over the cap: {:?}",
            msg.title
        );
        assert!(!msg.title.ends_with('\u{2026}'), "clipped: {:?}", msg.title);
        assert!(!msg.title.is_empty());
        assert!(
            msg.detail.iter().all(|l| !l.is_empty()),
            "no empty detail line: {msg:?}"
        );
    }
}

/// A CODEX row is named Codex, never Claude: the live upgrade's Codex
/// lane files its rows beside Claude Code's (`codex-<tab>`), and the
/// records were written when there was one agent.
#[test]
fn the_codex_lanes_records_name_codex() {
    use aterm_agent::harness::upgrade::{Agent, Phase};
    use aterm_agent::harness::upgrade_drive::Row;
    let codex = Row {
        agent: Agent::Codex,
        session: "codex-s-a".into(),
        tab: "s-a".into(),
        from: "0.157.0".into(),
        to: "0.157.1".into(),
        phase: Phase::Done,
        outcome: "codex on 0.157.1 · the same conversation resumed".into(),
        ..Row::default()
    };
    let done = agent_upgrade_done(&codex, "in tab 2");
    assert_eq!(done.title, "Codex moved onto 0.157.1");
    assert_eq!(done.detail, ["in tab 2", "the same conversation resumed"]);
    // A stop that repeats (ruling 283: a first one retries later, a record).
    let stalled = agent_upgrade_stalled(
        &Row {
            phase: Phase::Failed("no-resume-hint".into()),
            stop_streak: 2,
            streak_why: "no-resume-hint".into(),
            ..codex.clone()
        },
        1_790_311_076,
        "in tab 2",
    );
    assert_eq!(stalled.title, "Couldn't upgrade Codex in tab 2");
    assert!(
        stalled
            .detail
            .iter()
            .any(|l| l.contains("Codex 0.157.0 → 0.157.1")),
        "{:?}",
        stalled.detail
    );
    // It installs itself (ruling 380, the owner, 2026-09-28: "do I need
    // to do something? It's not clear."): nothing to do.
    let waiting = agent_upgrade_waiting("Codex", &["0.157.1"]).expect("a record");
    assert_eq!(waiting.title, "Codex 0.157.1 installs itself");
    assert!(waiting.detail[0].starts_with("nothing to do: "));
    // A mix names both products (review of 2026-09-26: "Newer agent
    // builds ready for 2 sessions" named neither), in the detail — the
    // title keeps to six words.
    let mixed =
        agent_upgrade_waiting("Claude Code and Codex", &["2.1.282", "0.157.1"]).expect("a record");
    assert_eq!(mixed.title, "Newer agent builds install themselves");
    assert_eq!(
        mixed.detail.first().map(String::as_str),
        Some("Claude Code and Codex sessions")
    );
    // A move that stopped after its `/exit` says where the conversation
    // is and what takes it back — not "quit it", which nothing runs to.
    let after_exit = agent_upgrade_stalled(
        &Row {
            phase: Phase::Failed("no-resume-hint".into()),
            exited_at: 1_790_311_000,
            ..codex.clone()
        },
        1_790_311_076,
        "in tab 2",
    );
    assert_eq!(after_exit.title, "Codex stopped in tab 2");
    assert!(
        after_exit
            .detail
            .contains(&"Resume it in that tab with this command".to_string())
            && after_exit.detail.contains(&"`codex resume`".to_string()),
        "{:?}",
        after_exit.detail
    );
    assert!(
        !after_exit.detail.iter().any(|l| l.contains("quit it")),
        "{:?}",
        after_exit.detail
    );
    // NEGATIVE CONTROL: a Claude row keeps every word it had.
    let claude = Row {
        agent: Agent::Claude,
        ..codex
    };
    assert_eq!(
        agent_upgrade_done(&claude, "in tab 2").title,
        "Claude Code moved onto 0.157.1"
    );
}

/// S1 AND S2 OF THE IN-FLIGHT REVIEW (2026-09-27). A Claude Code move that
/// stopped after its SIGTERM ended the agent names what takes the
/// conversation back — `claude --resume <conversation>` in the tab — never
/// "quit it": nothing runs to quit. A move under way that does not move
/// says it waits and that nothing is forced, offers no word (the harness
/// refuses one while a move is under way) and names `--status` in a
/// shell, never `--skip` or `--now`. NEGATIVE CONTROL: a Claude Code move
/// that stopped before any exit keeps the words it had.
#[test]
fn a_claude_move_stopped_after_its_exit_or_stuck_under_way_says_what_moves_it() {
    use aterm_agent::harness::upgrade::Phase;
    use aterm_agent::harness::upgrade_drive::Row;
    const NOW: u64 = 1_790_311_076;
    let session = "0badf00d-1111-2222-3333-444455556666";
    let tab = "s-b5cf2faabac5ce5127bd";
    let base = Row {
        session: session.into(),
        tab: tab.into(),
        from: "2.1.281".into(),
        to: "2.1.283".into(),
        behind_since: NOW - 600,
        ..Row::default()
    };
    let after_exit = agent_upgrade_stalled(
        &Row {
            phase: Phase::Failed("stale-exit".into()),
            exited_at: NOW - 60,
            ..base.clone()
        },
        NOW,
        "in tab 2",
    );
    // The one row that stands: why the owner, and the one action - the
    // sentence, and the command on a line of its own (ruling 423).
    assert!(
        after_exit
            .detail
            .contains(&"Resume it in that tab with this command".to_string())
            && after_exit
                .detail
                .contains(&format!("`claude --resume {session}`")),
        "{:?}",
        after_exit.detail
    );
    assert!(
        !after_exit
            .detail
            .iter()
            .any(|l| l.contains("quit it") || l.to_lowercase().contains("codex")),
        "{:?}",
        after_exit.detail
    );
    // Nothing runs in the tab to ask again: the tab's standing row, not
    // a round resting (ruling 283), and its why a person's words — no
    // raw stop word, no parenthesis, no backtick (ruling 284).
    assert_eq!(after_exit.title, "Claude stopped in tab 2");
    assert!(
        !after_exit.detail[0].contains("stale-exit") && !after_exit.detail[0].contains(['(', '`']),
        "{:?}",
        after_exit.detail
    );
    let before_exit = agent_upgrade_stalled(
        &Row {
            phase: Phase::Failed("signal-refused".into()),
            ..base.clone()
        },
        NOW,
        "in tab 2",
    );
    assert!(
        before_exit
            .detail
            .iter()
            .any(|l| l.contains("quit it and resume it by hand")),
        "{:?}",
        before_exit.detail
    );
    for phase in [
        Phase::Exiting { at_s: NOW - 400 },
        Phase::Relaunched { at_s: NOW - 400 },
    ] {
        let stuck = agent_upgrade_stalled(
            &Row {
                phase: phase.clone(),
                ..base.clone()
            },
            NOW,
            "in tab 2",
        );
        assert_eq!(
            stuck.detail[1],
            "no word moves it while it is under way: it goes on once what it waits on \
             ends, and nothing is forced",
            "{phase:?}"
        );
        assert_eq!(
            stuck.detail.last(),
            Some(&format!(
                "the same in any shell: `aterm harness upgrade {tab} --status`"
            )),
            "{phase:?}"
        );
        assert!(
            !stuck.detail[0].contains("stuck:") && !stuck.detail[0].contains(['(', '`']),
            "{phase:?}: {:?}",
            stuck.detail
        );
        assert!(stuck.actions.is_empty(), "{phase:?}: no word offered");
        assert!(
            !stuck.detail.iter().any(|l| l.contains("--skip")
                || l.contains("--now")
                || l.contains("Upgrade now")
                || l.contains("Skip version")),
            "{phase:?}: {:?}",
            stuck.detail
        );
    }
}

/// The value-level twin of `update_words::no_update_lane_source_prompts_a_
/// restart` over the reporters' words: no sentence asks for a restart
/// unless it carries one of the sanctioned anchors on the same line —
/// the restart-only keys (`columns/lines`, `gpu applies`), the GPU-lost
/// remedy (`open a new window or restart`), the dead publisher's retry,
/// the Windows backdrop.
#[test]
fn no_reporter_wording_prompts_a_restart() {
    let needles = ["restart", "relaunch", "reopen", "next launch", "reboot"];
    let anchors = [
        "columns/lines",
        "gpu applies",
        "open a new window or restart",
        "restart aterm to retry",
        "backdrop",
        // The restart-only record's title says WHEN the edit applies
        // (ruling 261: `GPU setting applies after restart`), never asks.
        "apply after restart",
        "applies after restart",
    ];
    let mut checked = 0usize;
    for msg in every_reporter_message() {
        for line in sentences(&msg) {
            checked += 1;
            let lower = line.to_lowercase();
            let prompts = needles.iter().any(|n| lower.contains(n));
            let anchored = anchors.iter().any(|a| lower.contains(a));
            assert!(!prompts || anchored, "asks for a restart: {line:?}");
        }
    }
    assert!(checked > 30, "the guard covers the reporters: {checked}");
    // The digest's entries, too: the upgrade's (every kind, every form of a
    // place) and the Full Disk Access need, in both causes.
    let mut entries: Vec<aterm_messages::exceptions::Entry> =
        crate::upgrade_exceptions_tests::every_digest_entry()
            .into_iter()
            .map(|(_, entry)| entry)
            .collect();
    for regressed in [false, true] {
        entries.push(file_access_entry(&crate::consent_card::NeedWords {
            regressed,
            ..crate::consent_card::NeedWords::sample()
        }));
    }
    for entry in entries {
        let msg = entry.message().expect("the words pass the rule");
        for line in sentences(&msg) {
            let lower = line.to_lowercase();
            let prompts = needles.iter().any(|n| lower.contains(n));
            let anchored = anchors.iter().any(|a| lower.contains(a));
            assert!(
                !prompts || anchored,
                "{} asks for a restart: {line:?}",
                entry.key()
            );
        }
    }
}

/// The feedback rows: a terse title, the cause (or what to do) as
/// `detail[0]`, a gesture's short hold; the pins that read the old
/// sentences as titles ("The GPU was lost", "accessibility OFF", "has no
/// effect on the CPU renderer" on glass) are re-pinned here.
#[test]
fn feedback_sentences_split_into_a_title_and_a_cause() {
    let msg = serious_mode_feedback(
        "Serious Mode was not changed: save conflict; current aterm.toml could not be admitted: x",
    );
    assert_eq!(msg.title, "Couldn't change Serious Mode");
    assert_eq!(
        msg.detail,
        [
            "save conflict",
            "current aterm.toml could not be admitted: x"
        ]
    );
    assert_eq!(msg.key.as_deref(), Some(KEY_SERIOUS_MODE));
    assert_eq!(msg.hold, Hold::For(HOLD_GESTURE));
    let conflict = serious_mode_feedback(
        "Serious Mode was not changed because aterm.toml changed first; its current value was kept.",
    );
    assert_eq!(conflict.detail[0], "aterm.toml changed first");
    let unverified = serious_mode_feedback(
        "Serious Mode may have been written but could not be verified; reload before retrying: gen moved",
    );
    assert_eq!(unverified.title, "Couldn't confirm Serious Mode");
    assert_eq!(unverified.detail[0], "reload before retrying");
    let msg = presence_not_saved(
        "presence.rim",
        "Presence Rim",
        "aterm.toml changed first; its current value was kept",
        false,
    );
    assert_eq!(msg.title, "Couldn't save Presence Rim");
    assert_eq!(msg.severity, Severity::Error);
    assert_eq!(msg.hold, Hold::For(HOLD_GESTURE));
    assert_eq!(
        msg.key.as_deref(),
        Some("fabric.presence-save.presence.rim")
    );
    let maybe = presence_not_saved("presence.band", "Presence Band", "io", true);
    assert_eq!(maybe.title, "Couldn't confirm Presence Band was saved");
    assert_eq!(maybe.severity, Severity::Warn);
    let msg = config_lane_error("Robi was not dismissed: the setting was not saved");
    assert_eq!(msg.title, "Couldn't dismiss Robi");
    assert_eq!(msg.detail, ["the setting was not saved"]);
    assert_eq!(msg.hold, Hold::For(HOLD_GESTURE), "Robi's is a gesture");
    let unknown = config_lane_error("Something new went wrong: the cause");
    assert_eq!(unknown.title, "Couldn't change a setting");
    assert_eq!(unknown.detail, ["Something new went wrong: the cause"]);
    assert!(
        !unknown.excerpt,
        "someone else's words stay behind Details (ruling 306)"
    );
    let msg = launch_load_failure(
        "aterm.toml could not be read at launch (permission denied (os error 13)) \u{2014} every setting is running at its default.",
    );
    assert_eq!(msg.title, "Couldn't read your settings");
    assert_eq!(msg.detail[0], "permission denied (os error 13)");
    // The consequence is the notice's own tail, said once (audit
    // 2026-09-24): no line of its own before the whole sentence.
    assert_eq!(
        msg.detail
            .iter()
            .filter(|l| l.contains("every setting is running at its default"))
            .count(),
        1,
        "{:?}",
        msg.detail
    );
    assert_eq!(msg.hold, Hold::For(HOLD_LAUNCH));
    assert_eq!(msg.key.as_deref(), Some(KEY_LAUNCH_LOAD));
    let invalid = launch_load_failure(
        "aterm.toml is not a valid configuration (expected `=` at line 3) \u{2014} the window's settings except [harness] are at their defaults.",
    );
    assert_eq!(invalid.title, "Couldn't load your settings");
    assert_eq!(invalid.detail[0], "a mistake on line 3", "ruling 306");
    let lineless = launch_load_failure(
        "aterm.toml is not a valid configuration (expected `=`) \u{2014} the window's settings except [harness] are at their defaults.",
    );
    assert!(!lineless.excerpt, "no line named: the title alone");
    let standing = gpu_lost();
    assert_eq!(standing.title, "GPU lost");
    assert_eq!(
        standing.detail,
        [
            "windows opened for the backdrop cannot redraw",
            "open a new window or restart aterm to get a visible one back"
        ]
    );
    assert_eq!(standing.hold, Hold::STANDING);
    assert_eq!(standing.actions, [Intent::NewWindow]);
    let dead = a11y_publisher_dead("bus gone", true);
    assert_eq!(dead.title, "Screen reader access lost");
    assert_eq!(dead.hold, Hold::STANDING, "a screen reader was attached");
    assert_eq!(
        a11y_publisher_dead("bus gone", false).hold,
        Hold::LogOnly,
        "nobody was listening: a record, not an FYI on the glass"
    );
    assert_eq!(
        dead.detail,
        ["restart aterm to retry", "bus gone"],
        "the retry alone on its line, the reason behind it; never the title again"
    );
    let cpu = cpu_renderer_no_effect("background_material", "no compositor");
    assert_eq!(cpu.key.as_deref(), Some("render.cpu.background_material"));
    assert_eq!(cpu.hold, Hold::LogOnly, "a disclosure is a record");
    assert_eq!(
        cpu.actions,
        [Intent::OpenSettings {
            route: "/appearance".to_string()
        }]
    );
    // A Hold press on a held session asked for what already is: a
    // record. A Release the fleet refused failed: a gesture row, titled
    // with the outcome.
    let held = fleet_hold(true);
    assert_eq!(held.title, "Session already held");
    assert_eq!(held.hold, Hold::LogOnly);
    assert_eq!(held.detail, ["the fleet holds it"]);
    let kept = fleet_hold(false);
    assert_eq!(kept.title, "Couldn't release the hold");
    assert_eq!(kept.severity, Severity::Warn);
    assert_eq!(kept.hold, Hold::For(HOLD_GESTURE));
    assert_eq!(kept.detail, ["the fleet holds it"]);
    assert!(kept.excerpt);
    assert_eq!(kept.key.as_deref(), Some(KEY_FABRIC_HOLD));
    assert_eq!(hold_refused(true, "x").title, "Couldn't hold the session");
    assert_eq!(hold_refused(false, "x").title, "Couldn't lift the hold");
    assert_eq!(
        hold_refused(true, "x").key.as_deref(),
        Some(KEY_FABRIC_HOLD)
    );
    // The lost bridge's advisory (2026-09-29): a warning that names what
    // did NOT happen and the switch that would have made it.
    let lost = fabric_lost_advisory(2);
    assert_eq!(lost.key.as_deref(), Some(KEY_FABRIC_HOLD));
    assert_eq!(lost.title, "Fabric bridge lost");
    assert!(
        lost.detail[0].starts_with("2 sessions it governed keep running, not halted")
            && lost.detail[0].contains("hold_on_bridge_lost = true"),
        "{:?}",
        lost.detail
    );
    assert!(fabric_lost_advisory(1).detail[0].starts_with("1 session it governed"));
    let failed = fabric_status("off", "Couldn't run Fabric Off", "exited 2", false);
    assert_eq!(failed.severity, Severity::Error);
    assert_eq!(failed.detail, ["exited 2"]);
    assert!(!failed.excerpt, "an exit code rides behind Details");
    let overridden = serious_mode_feedback(
        "Serious Mode was saved, but a newer aterm.toml edit now controls it.",
    );
    assert_eq!(overridden.title, "Couldn't apply Serious Mode");
    assert_eq!(
        overridden.detail,
        ["aterm.toml sets it"],
        "what overrode it"
    );
    let done = fabric_status("off", "Fabric Off finished", "", true);
    assert_eq!(done.hold, Hold::LogOnly, "a clean exit is a record");
    assert_eq!(log_did_not_open("/x/y.log").title, "Couldn't open the log");
    assert_eq!(log_did_not_open("/x/y.log").hold, Hold::For(HOLD_GESTURE));
}

/// A DIAGNOSTIC IS ONE PROBLEM, NOT ONE LINE (upstream e7dc1feee's
/// `a_multi_line_diagnostic_is_painted_as_the_rows_it_actually_has`, ported
/// onto R13). A rejected `aterm.toml` reaches the config lane as a `toml`
/// parse error with its caret diagram: one message, a terse title, the
/// parser's first row as the excerpt and its caret diagram behind Details.
/// The error string comes from the REAL parser, not a literal, so the test
/// cannot pass because the shape it assumes has changed.
#[test]
fn a_multi_line_diagnostic_is_one_message_with_the_rows_it_actually_has() {
    let error = "x = [bad\n"
        .parse::<aterm_toml::edit::DocumentMut>()
        .expect_err("a malformed document")
        .to_string();
    assert!(
        error.contains('\n'),
        "this test is about a multi-line diagnostic: {error:?}"
    );
    let msg = config_lane_error(&format!("Config observation was not valid TOML: {error}"));
    // ONE PROBLEM, ONE MESSAGE: a terse title, and the diagnostic's rows
    // are its detail lines — several, not one.
    assert_eq!(msg.title, "Couldn't read your settings");
    assert_eq!(msg.key.as_deref(), Some(KEY_CONFIG_LANE));
    assert!(msg.detail.len() > 1, "{:?}", msg.detail);
    // NO CONTROL CHARACTER REACHES A LINE, and no line is blank.
    for line in &msg.detail {
        assert!(
            line.chars().all(|ch| !ch.is_control()),
            "a control character took a cell: {line:?}"
        );
        assert!(!line.trim().is_empty(), "a blank row: {:?}", msg.detail);
    }
    // The band's excerpt is the finding in a person's words (ruling
    // 306), and the parser's own opening sentence and its caret row are
    // DIFFERENT lines after it, the caret row kept whole with its
    // alignment.
    assert!(
        msg.detail[0].starts_with("line 1: missing ") || msg.detail[0] == "a mistake on line 1",
        "{:?}",
        msg.detail
    );
    assert!(!msg.detail[0].contains('`'), "{:?}", msg.detail);
    assert!(
        msg.detail[1].starts_with("TOML parse error"),
        "{:?}",
        msg.detail
    );
    let caret = error
        .lines()
        .find(|row| row.contains('^'))
        .expect("the parser's caret row")
        .trim_end();
    let at = msg
        .detail
        .iter()
        .position(|line| line == caret)
        .unwrap_or_else(|| panic!("the caret row {caret:?} is no line: {:?}", msg.detail));
    assert!(at > 0, "the caret must not share a line with the sentence");
    // A control byte that survives the split is spelled, not dropped.
    let hostile = config_lane_error("Config observation was not valid TOML: bad\u{1b}[31m\tx");
    assert_eq!(hostile.detail, ["bad\u{00b7}[31m\u{00b7}x"]);
    // A one-line sentence: the terse title and its cause.
    let one = config_lane_error("Robi was not dismissed: the lane dropped it");
    assert_eq!(
        (one.title.as_str(), one.detail.as_slice()),
        (
            "Couldn't dismiss Robi",
            ["the lane dropped it".to_string()].as_slice()
        )
    );
    // Every head of design §10.3 C22's map.
    for (head, title) in [
        (
            "Config observation was not valid TOML",
            "Couldn't read your settings",
        ),
        ("Robi was not dismissed", "Couldn't dismiss Robi"),
        (
            "Config reconciliation failed; queued changes were not written",
            "Couldn't save settings changes",
        ),
        (
            "Manual saved aterm.toml, but its exact generation could not be admitted",
            "Couldn't apply saved settings",
        ),
        (
            "Config was saved, but its exact disk generation could not be admitted",
            "Couldn't apply saved settings",
        ),
        (
            "Config publication could not be verified",
            "Couldn't confirm settings saved",
        ),
        ("Config was NOT saved", "Couldn't save a settings change"),
    ] {
        assert_eq!(
            config_lane_error(&format!("{head}: the cause")).title,
            title,
            "{head}"
        );
    }
}

/// A PERSON'S SETTINGS ▸ PACKAGES VERB (design ruling 224): every row it
/// can show — the verb's own, and queued at the store lock — is PROGRESS
/// under the owner's rule (live, busy: the comet and the elapsed clock),
/// titled in a few words in the lane's one noun, keyed to the lane's pass
/// so the plan's reads restate it, revealed only after the progress grace
/// (a 1–4 s no-op never flashes a row), with no capsule, and ending in
/// its own finished words.
#[test]
fn a_persons_packages_rows_are_progress_after_the_grace() {
    let verbs = [
        (
            PackagesVerb::Check,
            "Checking ALab tools",
            "ALab tools checked",
        ),
        (
            PackagesVerb::Install,
            "Installing ALab tools",
            "ALab tools installed",
        ),
        (
            PackagesVerb::Remove,
            "Removing ALab tools",
            "ALab tools removed",
        ),
    ];
    for (verb, title, finished) in verbs {
        let row = packages_verb_row(verb);
        let waiting = packages_waiting_row(verb);
        assert_eq!(row.title, title);
        assert_eq!(waiting.title, WAITING_FOR_ALAB);
        for msg in [&row, &waiting] {
            assert_eq!(attention(msg), Ok(Attention::Progress), "{}", msg.title);
            assert!(aterm_messages::text::title_words(&msg.title) <= 6);
            assert_eq!(msg.reveal_after, Some(aterm_messages::PROGRESS_GRACE));
            assert_eq!(msg.key.as_deref(), Some(crate::toolchain_words::KEY_PASS));
            assert!(msg.meter.as_ref().is_some_and(|m| m.busy), "busy");
            assert!(msg.actions.is_empty(), "work in flight is no decision");
            assert!(!msg.excerpt, "the title alone");
            assert_eq!(msg.finished_title(), finished);
            assert!(matches!(msg.hold, Hold::Live { .. }));
        }
        // The wait's own bound, with a margin: the row outlives a queue
        // that could last the whole 30 minutes.
        assert!(matches!(
            waiting.hold,
            Hold::Live { stale_after } if stale_after
                > Duration::from_secs(crate::ATPKG_WAIT_LOCK_SECS)
        ));
    }
    assert_eq!(PackagesVerb::Check.finished_work(), "ALab tools updated");
    assert_eq!(
        PackagesVerb::Install.finished_work(),
        "ALab tools installed"
    );
    // The machine apply is a sub-second local edit: no row.
    assert_eq!(
        crate::app_native::person_verb(&crate::native_app::PackagesRequest::MachineApply),
        None
    );
    assert_eq!(
        crate::app_native::person_echo(true, true),
        aterm_messages::EchoKind::Fault
    );
    assert_eq!(
        crate::app_native::person_echo(false, false),
        aterm_messages::EchoKind::Vanish
    );
    assert_eq!(
        crate::app_native::person_echo(false, true),
        aterm_messages::EchoKind::Complete
    );
}

/// A STOP ON WORK IN FLIGHT (design ruling 232): the paste row carries
/// the one decision a progress row may — `Stop paste`, which stops that
/// row's own work — and stays PROGRESS; the rewrap row carries none; any
/// other consequential press on a moving row is refused.
#[test]
fn a_progress_row_may_carry_only_the_stop_of_its_own_work() {
    use aterm_messages::waits::{paste_row, rewrap_row};
    let pasting = paste_row(4, 1, 1_100_000, 4_200_000, aterm_messages::PROGRESS_GRACE);
    assert_eq!(attention(&pasting), Ok(Attention::Progress));
    assert_eq!(pasting.actions, [Intent::StopPaste { session: 4 }]);
    assert!(pasting.actions.iter().all(Intent::stops_work));
    let rewrapping = rewrap_row(4, 1_200_000, 3_400_000, aterm_messages::PROGRESS_GRACE);
    assert_eq!(attention(&rewrapping), Ok(Attention::Progress));
    assert!(rewrapping.actions.is_empty());
    let mut install_mid_flight = pasting.clone();
    install_mid_flight.actions = vec![Intent::ApplyUpdate { build: 9 }];
    assert_eq!(
        attention(&install_mid_flight),
        Err("a decision on work in flight")
    );
    // A navigation stays allowed beside work (it decides nothing).
    let mut navigation = rewrapping;
    navigation.actions = vec![Intent::OpenSettings {
        route: "/messages".into(),
    }];
    assert_eq!(attention(&navigation), Ok(Attention::Progress));
}

/// The band's config row at 60, 80 and 100 columns: at 60 the title and
/// `Open aterm.toml` take the row, at 80 the correction alone fits, from
/// 100 the whole correction.
const PINNED_MISSPELLED_ROWS: [(&str, Option<&str>); 3] = [
    ("Misspelled setting", None),
    ("Misspelled setting", Some("\u{2192} window_padding")),
    (
        "Misspelled setting",
        Some("windw_padding \u{2192} window_padding"),
    ),
];

/// THE OVERDUE ROW SAYS HOW LONG (round 38, day ten, D3; ruling 402): the
/// day's step 8, one Claude tab seven hours behind on a turn still running,
/// through the agent's own row ([`agent_upgrade_stalled_group`]). Its words
/// lead with `behind for 7 h`. Since the owner's rule of 2026-09-28 (a wait is
/// not a failure, and nothing persists that the owner cannot act on) the
/// title is the wait's (`Claude upgrade waits in tab 1`) and the row is a
/// record: it takes no band row, whatever the width.
#[test]
fn the_overdue_upgrade_row_says_how_long_at_100_columns() {
    use aterm_agent::harness::upgrade::Phase;
    use aterm_agent::harness::upgrade_drive::Row;
    const NOW: u64 = 1_790_311_076;
    let row = Row {
        tab: "s-b5cf2faabac5ce5127bd".into(),
        from: "2.1.280".into(),
        to: "2.1.283".into(),
        phase: Phase::Pending,
        wait: "not-idle:busy".into(),
        behind_since: NOW - 7 * 3_600,
        noticed_at: NOW - 3_600,
        ..Row::default()
    };
    let msg = super::agent_upgrade_stalled_group(&[(&row, "in tab 1".to_string())], NOW)
        .expect("one tab's row");
    assert_eq!(msg.title, "Claude upgrade waits in tab 1");
    assert!(
        msg.detail[0].starts_with("behind for 7 h: "),
        "{:?}",
        msg.detail
    );
    // A wait is a RECORD (owner rules, 2026-09-28: no alert persists that the
    // owner cannot act on): the words say how long, and the band shows no row.
    let mut app = crate::App::headless_for_test();
    app.post_message(msg);
    let p = app.band_presentation(100);
    assert!(p.rows.is_empty(), "a wait takes no band row: {:?}", p.rows);
}

/// THE AGENT'S ROW FOR SEVERAL TABS SAYS HOW LONG TOO (round 38's
/// review, finding 1; ruling 404): two Claude tabs, seven and two hours
/// behind on turns still running, are the agent's ONE overdue line. Its
/// first line, `tab 1 for 7 h · tab 3 for 2 h`, names the tabs most behind
/// first. Since the seal it is a RECORD, whatever the width (nothing posts
/// it: `no_tab_joins_an_agents_group_row`).
#[test]
fn the_agents_overdue_row_for_two_tabs_says_how_long_at_100_columns() {
    use aterm_agent::harness::upgrade::Phase;
    use aterm_agent::harness::upgrade_drive::Row;
    const NOW: u64 = 1_790_311_076;
    let row = |tab: &str, hours: u64| Row {
        tab: tab.into(),
        from: "2.1.280".into(),
        to: "2.1.283".into(),
        phase: Phase::Pending,
        wait: "not-idle:busy".into(),
        behind_since: NOW - hours * 3_600,
        noticed_at: NOW - 3_600,
        ..Row::default()
    };
    let (one, three) = (
        row("s-b5cf2faabac5ce5127bd", 7),
        row("s-c6d0a3bbcbd6df6238ce", 2),
    );
    let msg = super::agent_upgrade_stalled_group(
        &[
            (&one, "in tab 1".to_string()),
            (&three, "in tab 3".to_string()),
        ],
        NOW,
    )
    .expect("the agent's row");
    assert_eq!(msg.title, "Couldn't upgrade Claude in 2 tabs yet");
    assert_eq!(msg.detail[0], "tab 1 for 7 h \u{b7} tab 3 for 2 h");
    // Its class is a RECORD (a standing row nobody could act on was the
    // owner's row 46, ruling 363): the words say how long, the band shows
    // no row, and the door counts nothing.
    assert!(matches!(msg.need(), aterm_messages::need::Need::Record));
    let mut app = crate::App::headless_for_test();
    app.post_message(msg);
    let p = app.band_presentation(100);
    assert!(
        p.rows.is_empty(),
        "the agent's row is a record: {:?}",
        p.rows
    );
}

/// THE BAND'S CONFIG ROW (ruling 261): a misspelled key reads
/// `Misspelled setting`, its excerpt the correction — `windw_padding →
/// window_padding` where it fits whole, the correction alone behind its
/// arrow where it does not (ruling 306: a bare `window_padding` named the
/// right spelling as the misspelled one), never the typo. Pinned at 60 and 80 columns, from the validator's
/// real sentence.
#[test]
fn the_misspelled_setting_row_is_pinned_at_60_and_80_columns() {
    let notices = crate::app_config::ignored_key_notices("windw_padding = 20\n");
    let mut warns = ConfigWarnings::default();
    warns.extend(ConfigFamily::IgnoredKeys, notices);
    let msg = warns.into_messages().remove(0);
    assert_eq!(msg.title, "Misspelled setting");
    let mut app = crate::App::headless_for_test();
    app.post_message(msg);
    let row = |cols: usize| {
        let p = app.band_presentation(cols);
        let r = p.rows[0].clone();
        (r.title.1, r.detail.map(|(_, d)| d))
    };
    assert_eq!(
        vec![row(60), row(80), row(100)],
        PINNED_MISSPELLED_ROWS
            .iter()
            .map(|(t, d)| (t.to_string(), d.map(str::to_string)))
            .collect::<Vec<_>>()
    );
}

/// THE TITLE LINT (design ruling 261): the owner's "short plain English,
/// no jargon", held over every title aterm's own builders write — this
/// module's, the toolchain and update lanes', the paste and rewrap rows and
/// the strain row. A glass title is at most six words (ruling 76) and a
/// record's at most eight (the log reads it whole; a record may name a
/// measured span); every title starts with a capital or a count (the brand
/// `aterm` may lead in its own case), and carries no parenthesis, no colon
/// and no file extension (`aterm.toml` is "your settings"). A Warn or Error
/// that is not a question or a decision says what went wrong in ONE grammar:
/// `Couldn't …`, or it names the loss (`GPU lost`, `Keystrokes dropped`,
/// `Misspelled setting`).
#[test]
fn every_title_is_short_plain_and_states_what_happened() {
    const LOSS: [&str; 15] = [
        // A shutdown the upgrade asked for that hung, and an agent it
        // restarted that never reached a prompt (ruling 423's interrupts),
        // and aterm's own faults in an upgrade: a record that made no
        // progress, a wait aterm cannot classify, a relaunch that never
        // settled.
        "exiting",
        "ready",
        "stuck",
        "moved",
        "settled",
        "lost",
        "dropped",
        "crashed",
        "skipped",
        "misspelled",
        "unknown",
        "errors",
        "full",
        "slowed",
        "stopped",
    ];
    let mut all = every_reporter_message();
    use crate::toolchain_words as tw;
    all.extend([
        tw::announced("installing 10 ALab program(s) over the network (about 3 GB on disk)"),
        tw::deferred("the network is metered"),
        tw::installed("claude 2.1.280"),
        tw::ended("3 programs updated", true),
        tw::failed("trust: disk full"),
        tw::install_failed("trust: disk full"),
        tw::not_installed("nothing to install"),
        tw::unusable(
            "ALab publishes no build for this machine's architecture (x86_64-apple-darwin)",
        ),
        tw::first_run_short(tw::FirstRunShort::Failed, "No space left on device"),
        tw::first_run_short(tw::FirstRunShort::Nothing, "seed unusable"),
        tw::appnotice(
            tags::TOOLCHAIN,
            "aterm pkg install claude: 2.1.281 installed",
        ),
        tw::managed_current(
            "claude 2.1.280 (Anthropic latest)",
            0,
            true,
            crate::toolchain_words::HookDialect::Zsh,
        )
        .expect("a record"),
        tw::managed_current(
            "claude 2.1.280 (Anthropic latest); codex 0.156.0 (OpenAI latest)",
            0,
            true,
            crate::toolchain_words::HookDialect::Zsh,
        )
        .expect("a record"),
    ]);
    all.extend(tw::machine_settings(&format!(
        "spotlight-noindex 73 dir(s) migrated; {}",
        atpkg::machine::UNIVERSAL_CONTROL_ENTRY
    )));
    for verb in [
        PackagesVerb::Check,
        PackagesVerb::Install,
        PackagesVerb::Remove,
    ] {
        all.push(packages_verb_row(verb));
        all.push(packages_waiting_row(verb));
    }
    all.push(aterm_messages::waits::paste_row(
        3,
        1,
        1_100_000,
        4_200_000,
        Duration::ZERO,
    ));
    all.push(aterm_messages::waits::rewrap_row(
        3,
        1_200_000,
        3_400_000,
        Duration::ZERO,
    ));
    // The update lane's rows too (ruling 309: `Update didn't finish` and
    // `Update didn't install` read apart from every `Couldn't …` beside
    // them, and no lint read them) — the health warnings included since
    // ruling 311 keyed their heal and latch on their kind, not their words.
    all.extend(
        every_lane_message()
            .into_iter()
            .filter(|m| m.tag == tags::UPDATE),
    );
    let mut failures = Vec::new();
    for msg in &all {
        let t = msg.title.as_str();
        let words = aterm_messages::text::title_words(t);
        let cap = if msg.hold == Hold::LogOnly { 8 } else { 6 };
        let mut fault = Vec::new();
        if words > cap {
            fault.push(format!("{words} words (at most {cap})"));
        }
        let first = t.chars().next().unwrap_or(' ');
        if !(first.is_uppercase() || first.is_ascii_digit() || t.starts_with("aterm ")) {
            fault.push("does not start with a capital".to_string());
        }
        if t.contains(['(', ')']) {
            fault.push("a parenthesis".to_string());
        }
        if t.contains(':') {
            fault.push("a colon".to_string());
        }
        let bytes = t.as_bytes();
        let extension = t.match_indices('.').any(|(at, _)| {
            at > 0
                && bytes[at - 1].is_ascii_alphanumeric()
                && t[at + 1..]
                    .split(|c: char| !c.is_ascii_alphanumeric())
                    .next()
                    .is_some_and(|ext| {
                        (2..=8).contains(&ext.len()) && ext.bytes().all(|b| b.is_ascii_lowercase())
                    })
        });
        if extension {
            fault.push("a file extension".to_string());
        }
        // A question, a decision, work in flight — or a row whose title
        // IS its remedy (`Move aterm to Applications`): what to do,
        // which the failure grammar would bury.
        let decision = msg.is_ask()
            || msg.actions.iter().any(Intent::is_consequential)
            || matches!(msg.hold, Hold::Live { .. })
            || t.starts_with("Move ");
        let lower = t.to_lowercase();
        if matches!(msg.severity, Severity::Warn | Severity::Error)
            && !decision
            && !t.starts_with("Couldn't ")
            && !lower
                .split(|c: char| !c.is_alphanumeric())
                .any(|w| LOSS.contains(&w))
        {
            fault.push("a failure that neither says Couldn't nor names a loss".to_string());
        }
        if !fault.is_empty() {
            failures.push(format!("{t:?} ({:?}): {}", msg.severity, fault.join(", ")));
        }
    }
    assert!(failures.is_empty(), "titles:\n{}", failures.join("\n"));
}

/// THE OWNER'S RULE, OVER EVERY BUILDER (design §10.1, ruling 76): every
/// message a reporter here posts is progress, a decision, a failure or a
/// record — never a confirmation, an FYI or progress with no indicator on
/// the glass — and every glass title is terse.
#[test]
fn every_glass_message_earns_its_row() {
    let mut classes = std::collections::BTreeMap::new();
    let all = every_reporter_message();
    // A key a removed feature left behind (`show_hud`) is a RECORD in
    // the retired family, never the ignored-key row (design ruling 213).
    let removed = all
        .iter()
        .find(|m| m.detail.iter().any(|l| l.contains("show_hud does nothing")))
        .expect("the removed key is reported");
    assert_eq!(
        removed.key.as_deref(),
        Some(ConfigFamily::RetiredKeys.key())
    );
    assert_eq!(attention(removed), Ok(Attention::Record));
    // A record is never painted, so its first sentence is not repeated
    // as a cut excerpt above itself (audit 2026-09-24).
    let restart = all
        .iter()
        .find(|m| m.key.as_deref() == Some(ConfigFamily::Restart.key()))
        .expect("the restart record");
    assert_eq!(
        restart.detail.first().map(String::as_str),
        Some("columns/lines applies on next launch (resize the window to change size now)"),
        "{:?}",
        restart.detail
    );
    for msg in all {
        let class =
            attention(&msg).unwrap_or_else(|why| panic!("{why}: {:?} ({:?})", msg.title, msg.hold));
        *classes.entry(format!("{class:?}")).or_insert(0usize) += 1;
    }
    // This module's builders are decisions, failures and records; its one
    // progress row (the admin install) went with the OS-installer protocols
    // (2026-09-24), and the live-work rows are `toolchain_words`', whose
    // own test holds them to the rule as `Progress` — as
    // `a_persons_packages_rows_are_progress_after_the_grace` holds the
    // Settings verbs' rows (ruling 224).
    for class in ["Decision", "Failure", "Record"] {
        assert!(
            classes.contains_key(class),
            "the fixture exercises every class it builds: {classes:?}"
        );
    }
    assert!(!classes.contains_key("Progress"), "{classes:?}");
    // The rule refuses what it must.
    let fyi = Message::new(tags::SYSTEM, Severity::Info, "Something happened");
    assert_eq!(attention(&fyi), Err("an FYI on glass"));
    let done = Message::new(tags::SYSTEM, Severity::Success, "Done");
    assert_eq!(attention(&done), Err("a confirmation on glass"));
    let blind = Message::new(tags::SYSTEM, Severity::Info, "Working").hold(Hold::Live {
        stale_after: Duration::from_secs(30),
    });
    assert_eq!(attention(&blind), Err("progress with no indicator"));
    let clause = Message::new(tags::SYSTEM, Severity::Warn, "It broke: here is why");
    assert_eq!(attention(&clause), Err("a clause in a glass title"));
    let long = Message::new(
        tags::SYSTEM,
        Severity::Warn,
        "one two three four five six seven",
    );
    assert_eq!(attention(&long), Err("a glass title over six words"));
    let sentence = Message::new(tags::SYSTEM, Severity::Warn, "It broke.");
    assert_eq!(attention(&sentence), Err("a sentence for a glass title"));
    // Words, not tokens: a route's `▸` and a name's `&` spend none.
    assert_eq!(
        aterm_messages::text::title_words("Open Privacy & Security \u{25b8} Full Disk Access"),
        6
    );
    // A measured LEVEL is Progress on the strain row alone (ruling 208).
    let level = |tag: Tag, key: &str| {
        Message::new(tag, Severity::Info, "Typing slowed by yes in tab 2")
            .key(key)
            .hold(Hold::Live {
                stale_after: aterm_messages::STALE_STRAIN,
            })
            .meter(aterm_messages::Meter::level(900, "7.1 of 8 cores"))
    };
    assert_eq!(
        attention(&level(tags::SYSTEM, aterm_messages::STRAIN_KEY)),
        Ok(Attention::Progress)
    );
    assert_eq!(
        attention(&level(tags::UPDATE, aterm_messages::STRAIN_KEY)),
        Err("a level off the strain row")
    );
    assert_eq!(
        attention(&level(tags::SYSTEM, "system.other")),
        Err("a level off the strain row")
    );
}

/// THE DOOR, OVER EVERY BUILDER (owner rule 2026-09-28; review 2026-09-29):
/// each message a reporter here or in a lane can post goes through a fresh
/// center. None is RECORDED or CUT by the door (`demotions() == 0`: a class
/// that could not stand, or a hold no class earned, is a defect). The
/// roster of builders written the old way is EMPTY: the old spelling of a
/// persistent hold is a compile error (the seal, `Persist`), so a new
/// builder either earns its hold from an `Ask` or is a record.
#[test]
fn every_reporter_message_survives_the_door() {
    let now = std::time::Instant::now();
    let stamp = aterm_messages::WallStamp {
        unix_ms: 1_790_000_000_000,
    };
    let mut c = aterm_messages::MessageCenter::new(aterm_messages::MessageLog::empty(), now);
    for msg in every_reporter_message()
        .into_iter()
        .chain(every_lane_message())
    {
        let title = msg.title.clone();
        c.post(msg, stamp, now);
        assert_eq!(
            c.demotions(),
            0,
            "the door recorded {title:?} ({:?}): a class that cannot stand",
            c.last_demoted()
        );
    }
}

/// Every lane's rows beside this module's: the update flow, its records
/// and outcomes, the toolchain lane's rows and records, a person's
/// Settings ▸ Packages verbs and the session waits — the census the
/// marks are held over (ruling 302).
fn every_lane_message() -> Vec<Message> {
    use crate::toolchain_words as tw;
    use crate::update_words as uw;
    use aterm_update::Progress as P;
    let mut all = every_reporter_message();
    all.extend([
        uw::progress(
            &P::Downloading {
                version: "0.95.0".into(),
                bytes_done: 37_000_000,
                bytes_total: 74_000_000,
            },
            None,
            "",
        ),
        uw::progress(
            &P::Verifying {
                version: "0.95.0".into(),
            },
            None,
            "",
        ),
        uw::staged("0.95.0", 7, Some(uw::ApplyPosture::Automatic)),
        uw::staged("0.95.0", 7, Some(uw::ApplyPosture::ManualByConfig)),
        uw::download_failed("0.95.0", "zip sha256 mismatch"),
        uw::download_postponed("on battery"),
        uw::download_withheld("0.95.0", "held: build 7 is quarantined"),
        uw::installing("0.95.0"),
        uw::finishing("0.95.0"),
        uw::landed("0.95.0", 7, 0, None),
        uw::switch_started(Some("0.95.0"), "0.94.0"),
        uw::switch_stopped(Some("0.95.0"), "0.94.0", true, "typing"),
        uw::switch_stopped(Some("0.95.0"), "0.94.0", false, "child died"),
        uw::outcome("Update waiting", "x", Severity::Info),
        uw::outcome("Couldn't finish the update", "x", Severity::Warn),
        uw::needs_install(
            &crate::app_update_screen::update_installed_title(Some("0.95.0")),
            uw::INSTALL_FROM_MENU,
            Severity::Info,
            7,
        ),
        uw::needs_install("Couldn't install aterm v0.95.0", "x", Severity::Warn, 7),
        uw::needs_install(
            "Couldn't install aterm v0.95.0",
            uw::TRIES_AGAIN,
            Severity::Warn,
            7,
        ),
        uw::failed("Couldn't finish the update", "the helper exited 3", false),
        uw::health_warning(
            uw::HealthKind::Download,
            aterm_update::health_failing_title("pipeline"),
            "3 checks",
        ),
        uw::health_warning(
            uw::HealthKind::Stalled,
            uw::CHECKER_STALLED_TITLE,
            "the update check stopped answering 50 min ago while checking for a new \
             version; aterm started a fresh one in its place.",
        ),
        uw::health_recovered("Couldn't download updates", "since Aug 27"),
        uw::scrollback_lost(12_345, 3),
        tw::announced("installing 10 ALab program(s) over the network (about 3 GB on disk)"),
        tw::deferred("the network is metered"),
        tw::installed("claude 2.1.280"),
        tw::ended("3 programs updated", true),
        tw::failed("trust: disk full"),
        tw::install_failed("trust: disk full"),
        tw::not_installed("nothing to install"),
        aterm_messages::waits::paste_row(3, 1, 1_100_000, 4_200_000, Duration::ZERO),
        aterm_messages::waits::rewrap_row(3, 1_200_000, 3_400_000, Duration::ZERO),
    ]);
    all.extend(tw::machine_settings("spotlight-noindex 73 dir(s) migrated"));
    for verb in [
        PackagesVerb::Check,
        PackagesVerb::Install,
        PackagesVerb::Remove,
    ] {
        all.push(packages_verb_row(verb));
        all.push(packages_waiting_row(verb));
    }
    all
}

/// THE MARK SAYS WHAT THE ENTRY IS (design rulings 302–304), over every
/// lane's builders: a RECORD and a FAILURE wear their severity's mark —
/// `✕` for an Error (round 21: `aterm crashed last time` wore the
/// warning's `⚠` in an error's red), `⚠` for a Warn, `ℹ` and `✓` for the
/// quiet two — never the working `↻` of a row still moving (a finished
/// `Tabs restored after aterm stopped` read as a restore under way); a
/// DECISION that warns wears its severity's mark, and an Info one `ℹ`, or
/// `⇣` for the one build waiting on `Install now` (ruling 303: `aterm vX
/// is ready` wore `✓` from one builder and a still `↻` from the other);
/// WORK IN FLIGHT wears its activity's mark from one table — `⇣` a thing
/// moving in, `↻` a check or a rewrap, `⏸` a wait, `⊖` a removal (ruling
/// 304: the `·` read as a bullet), `↑` a thing going out.
#[test]
fn every_mark_says_what_the_entry_is() {
    let faults: Vec<String> = every_lane_message()
        .into_iter()
        .filter(|msg| msg.key.as_deref() != Some(aterm_messages::STRAIN_KEY))
        .filter(|msg| !mark_fits(msg))
        .map(|msg| {
            format!(
                "{:?} ({:?}, {:?}): {}",
                msg.title,
                msg.severity,
                msg.hold,
                msg.glyph.ch()
            )
        })
        .collect();
    assert!(faults.is_empty(), "marks:\n{}", faults.join("\n"));
    // NEGATIVE CONTROLS, through the lint's own predicate (review of
    // round 21: the control built a row and never asked the rule): each
    // class the rulings name, wearing the mark they retired.
    let crash = Message::new(tags::CRASH, Severity::Error, "aterm crashed last time")
        .glyph(Glyph::or_fallback('\u{26a0}'));
    assert!(!mark_fits(&crash), "a crash in a warning's triangle");
    assert!(mark_fits(
        &crash.clone().glyph(Severity::Error.default_glyph())
    ));
    let relaunch = Message::new(tags::CRASH, Severity::Info, "aterm reopened quietly")
        .hold(Hold::LogOnly)
        .glyph(Glyph::or_fallback('\u{21bb}'));
    assert!(
        !mark_fits(&relaunch),
        "a finished record wearing the working mark"
    );
    assert!(mark_fits(
        &relaunch.clone().glyph(Severity::Info.default_glyph())
    ));
    let rewrap = aterm_messages::waits::rewrap_row(3, 1_200_000, 3_400_000, Duration::ZERO);
    assert!(mark_fits(&rewrap), "the control's row as built");
    let still = rewrap.glyph(Severity::Info.default_glyph());
    assert!(!mark_fits(&still), "work in flight wearing a record's mark");
}

/// The mark lint's rule for one entry ([`every_mark_says_what_the_entry_is`]).
fn mark_fits(msg: &Message) -> bool {
    const MOVING: [char; 5] = ['\u{21e3}', '\u{21bb}', '\u{23f8}', '\u{2296}', '\u{2191}'];
    let mark = msg.glyph.ch();
    let own = msg.severity.default_glyph().ch();
    let moving = matches!(msg.hold, Hold::Live { .. })
        && msg
            .meter
            .as_ref()
            .is_some_and(|m| m.busy || m.fill_permille.is_some());
    let decision = msg.is_ask() || msg.actions.iter().any(Intent::is_consequential);
    if moving {
        MOVING.contains(&mark)
    } else if msg.hold == Hold::LogOnly || msg.severity != Severity::Info || !decision {
        mark == own
    } else {
        mark == '\u{2139}'
            || (mark == '\u{21e3}'
                && msg
                    .actions
                    .iter()
                    .any(|i| matches!(i, Intent::ApplyUpdate { .. })))
    }
}

/// The restart-only family and the renderer's disclosures are RECORDS
/// (design §10.3 C8, C13, C14): nothing broke, nothing presses.
#[test]
fn the_restart_family_and_the_render_notes_are_records() {
    let mut warns = ConfigWarnings::default();
    warns.push(
        ConfigFamily::Restart,
        "columns/lines applies on next launch (resize the window to change size now)".into(),
    );
    warns.push(
        ConfigFamily::Restart,
        "gpu applies on next launch (the renderer backend is chosen at startup)".into(),
    );
    let restart = warns.into_messages().remove(0);
    assert_eq!(restart.hold, Hold::LogOnly);
    assert_eq!(attention(&restart), Ok(Attention::Record));
    for record in [
        cpu_renderer_no_effect("background_opacity", "no translucent present path"),
        backdrop_declined(
            "background_material is styling the title bar only",
            "this display stack refused the DirectComposition backdrop swapchain",
        ),
    ] {
        assert_eq!(record.hold, Hold::LogOnly, "{}", record.title);
    }
}

/// C9's DEFECT FIX: the Secure Keyboard Entry refusal was one sentence with
/// an 18-space run where a line continuation was lost; it is one clean
/// sentence now, and its excerpt is the state the person set.
#[test]
fn secure_keyboard_refusal_has_no_space_run() {
    let mut warns = ConfigWarnings::default();
    warns.push(
        ConfigFamily::SecureKeyboard,
        "secure_keyboard_entry: the OS refused the change (OSStatus -25293) \u{2014} \
         Secure Keyboard Entry is NOT on"
            .into(),
    );
    let msg = warns.into_messages().remove(0);
    assert_eq!(msg.title, "Couldn't change Secure Keyboard Entry");
    assert_eq!(msg.detail[0], "Secure Keyboard Entry is NOT on");
    assert!(msg.detail[1].contains("OSStatus -25293"));
    assert!(
        msg.detail.iter().all(|l| !l.contains("  ")),
        "{:?}",
        msg.detail
    );
}

/// The lost-integration rows the corpus holds: one tab under each
/// remedy, two tabs in two windows under both, and more tabs than the row
/// names a line each.
fn integration_lost_fixtures() -> Vec<Message> {
    use crate::integration_notice::Heal;
    let tab = |place: &str, sid: &str, heal| LostTab {
        place: place.to_string(),
        sid: sid.to_string(),
        heal,
    };
    let claude = Heal::AgentMoved {
        agent: "Claude Code",
    };
    let mut sets = vec![
        vec![tab("in tab 2", "s-0123456789abcdef", claude)],
        vec![tab(
            "in window 2, tab 1",
            "s-0123456789abcdef",
            Heal::NewTab,
        )],
        vec![
            tab("in window 1, tab 2", "s-0123456789abcdef", claude),
            tab("in window 2, tab 10", "s-fedcba9876543210", Heal::NewTab),
        ],
    ];
    sets.push(
        (1..=14)
            .map(|i| tab(&format!("in tab {i}"), &format!("s-{i:016x}"), claude))
            .collect(),
    );
    sets.iter()
        .filter_map(|tabs| integration_lost(tabs))
        .collect()
}

/// DESIGN (a) OF THE 2026-09-29 REVIEW (rulings 270, 300): the row names
/// each tab by its PLACE, never by the raw `s-<hex>` a person never sees —
/// the session id is on the technical line alone, where the successor
/// reads it back. The words a person reads (the title and the sentence
/// lines, [`sentence_lines`]) and every per-tab line hold no session id,
/// and every title keeps the glass title rule. And no line carries a
/// space run (the `AgentMoved` remedy once held 18 spaces where its line
/// continuation lost the `\`), the rekey record's included.
#[test]
fn the_lost_integration_row_names_tabs_by_place_never_by_session_id() {
    for msg in integration_lost_fixtures() {
        assert!(
            sentences(&msg).iter().all(|l| !l.contains("  ")),
            "{:?}",
            msg.detail
        );
        let key = msg.key.as_deref();
        assert_eq!(key, Some(KEY_INTEGRATION_LOST), "one key for the one row");
        assert!(matches!(msg.hold, Hold::Standing(_)), "{:?}", msg.hold);
        assert_eq!(msg.severity, Severity::Warn);
        assert_eq!(msg.finished.as_deref(), Some(INTEGRATION_BACK));
        assert_eq!(
            aterm_messages::text::glass_title_fault(&msg.title),
            None,
            "{:?}",
            msg.title
        );
        assert!(!msg.title.contains("s-"), "{:?}", msg.title);
        let (technical, person): (Vec<&String>, Vec<&String>) = msg
            .detail
            .iter()
            .partition(|l| super::split_integration_sessions(l).is_some());
        assert!(!technical.is_empty(), "{:?}", msg.detail);
        // One tab's list is singular, several tabs' plural.
        let one = msg.title.starts_with("Tab ");
        assert!(
            technical.iter().all(|l| {
                l.contains(super::INTEGRATION_SESSION) == one
                    && l.contains(INTEGRATION_SESSIONS) == !one
            }),
            "{technical:?}"
        );
        assert!(
            msg.detail
                .last()
                .is_some_and(|l| super::split_integration_sessions(l).is_some()),
            "the session ids close the detail: {:?}",
            msg.detail
        );
        for line in &person {
            assert!(!line.contains("s-0") && !line.contains("s-f"), "{line:?}");
            assert!(
                !line.contains("detail=") && !line.contains("OSC"),
                "{line:?}"
            );
        }
        // The names, the count of the unnamed rest where there is one,
        // and the remedy.
        let counted = msg.detail[1].starts_with("and ");
        assert_eq!(
            sentence_lines(key, &msg.detail),
            2 + usize::from(counted),
            "{:?}",
            msg.detail
        );
        assert!(
            person[0].contains("tab "),
            "the first line names the tabs: {:?}",
            person[0]
        );
        assert!(!integration_lost_sessions(&msg.detail).is_empty());
    }
    let many = integration_lost_fixtures().pop().expect("the long one");
    assert_eq!(many.title, "14 tabs lost their shell integration");
    // One remedy for all 14: said once, the rest counted on a line of its
    // own, and no per-tab line repeats it.
    assert!(
        many.detail[0].starts_with("tab 1 \u{00b7} tab 2 ")
            && many.detail[0].contains("tab 12 \u{2014} "),
        "{:?}",
        many.detail
    );
    assert_eq!(many.detail[1], "and 2 more tabs", "{:?}", many.detail);
    // Places long enough that the names reach the line cap: the count
    // still stands.
    let long = (1..=14)
        .map(|i| LostTab {
            place: format!("in window 10, tab {}", 10 + i),
            sid: format!("s-{i:016x}"),
            heal: crate::integration_notice::Heal::NewTab,
        })
        .collect::<Vec<_>>();
    let long = integration_lost(&long).expect("14 tabs");
    assert_eq!(long.detail[1], "and 2 more tabs", "{:?}", long.detail);
    // Mixed remedies: the second line promises a list, and the list is
    // the agent tabs alone (none for the tab only a new tab heals).
    let mixed = integration_lost_fixtures().swap_remove(2);
    assert!(mixed.detail[1].ends_with("anew:"), "{:?}", mixed.detail);
    let per_tab = |m: &Message| {
        m.detail
            .iter()
            .filter(|l| l.ends_with(" \u{00b7} Claude Code"))
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(per_tab(&mixed), ["window 1, tab 2 \u{00b7} Claude Code"]);
    // An agent tab past the first line's naming cap is still listed.
    let mut late = (1..=13)
        .map(|i| LostTab {
            place: format!("in tab {i}"),
            sid: format!("s-{i:016x}"),
            heal: crate::integration_notice::Heal::NewTab,
        })
        .collect::<Vec<_>>();
    late.push(LostTab {
        place: "in tab 14".into(),
        sid: format!("s-{:016x}", 14),
        heal: crate::integration_notice::Heal::AgentMoved {
            agent: "Claude Code",
        },
    });
    let late = integration_lost(&late).expect("14 tabs");
    // The count of the unnamed rest follows the names it continues, never
    // the agent list, where it would read as more agent tabs.
    assert_eq!(late.detail[1], "and 2 more tabs", "{:?}", late.detail);
    assert!(late.detail[2].ends_with("anew:"), "{:?}", late.detail);
    assert_eq!(late.detail[3], "tab 14 \u{00b7} Claude Code");
    assert_eq!(per_tab(&late), ["tab 14 \u{00b7} Claude Code"]);
    assert!(
        late.detail[4..]
            .iter()
            .all(|l| !l.starts_with("and ") || super::split_integration_sessions(l).is_some()),
        "{:?}",
        late.detail
    );
    // More agent tabs than the list names: the rest counted as agent tabs.
    let crowd = (1..=15)
        .map(|i| LostTab {
            place: format!("in tab {i}"),
            sid: format!("s-{i:016x}"),
            heal: if i == 1 {
                crate::integration_notice::Heal::NewTab
            } else {
                crate::integration_notice::Heal::AgentMoved {
                    agent: "Claude Code",
                }
            },
        })
        .collect::<Vec<_>>();
    let crowd = integration_lost(&crowd).expect("15 tabs");
    assert_eq!(crowd.detail[1], "and 3 more tabs", "{:?}", crowd.detail);
    assert!(
        crowd.detail[2].contains("14 also come back"),
        "{:?}",
        crowd.detail
    );
    assert_eq!(per_tab(&crowd).len(), INTEGRATION_TABS_NAMED);
    assert_eq!(
        crowd.detail[3 + INTEGRATION_TABS_NAMED],
        "and 2 more agent tabs",
        "{:?}",
        crowd.detail
    );
    assert_eq!(
        many.detail
            .iter()
            .filter(|l| l.contains("next starts"))
            .count(),
        1,
        "{:?}",
        many.detail
    );
    // Every id whole, however many lines they take.
    let sids = integration_lost_sessions(&many.detail);
    assert_eq!(sids.len(), 14, "{:?}", many.detail);
    assert!(sids.iter().all(|s| s.len() == 18), "{sids:?}");
    assert!(
        many.detail
            .iter()
            .all(|l| l.chars().count() <= aterm_messages::DETAIL_LINE_CAP),
        "{:?}",
        many.detail
    );
    // A one-tab row an older parent carried said `sessions`: still read.
    let carried = vec!["it reads `status integration=degraded` \u{2014} sessions s-0a".into()];
    assert_eq!(
        integration_lost_sessions(&carried),
        vec!["s-0a".to_string()]
    );
    let rec = integration_rekey_waits("in tab 2", "s-0123456789abcdef");
    assert_eq!(rec.hold, Hold::LogOnly);
    assert!(rec.detail[0].starts_with("in tab 2:"), "{:?}", rec.detail);
    assert!(
        sentences(&rec).iter().all(|l| !l.contains("  ")),
        "{:?}",
        rec.detail
    );
}

/// A glued row is the sentence and the diagram sharing one line — the shape
/// the engine's `\n`-deleting sanitizer produced: `…column 11  |1 | font_px =`.
fn glued(line: &str) -> bool {
    line.contains("column") && line.contains('|')
}

/// THE LAUNCH PATH KEEPS THE ROWS TOO. An `aterm.toml` that is invalid AT
/// LAUNCH — every setting running at its default, the highest-stakes case —
/// went through one `Message::line`, so the band, the Details page and the
/// screen reader all read the caret diagram glued inline and the 240-char cut
/// fell mid-word in the consequence. The lane path kept e7dc1feee's split;
/// this one lost it when the banner became messages. Real parser, real notice.
#[test]
fn an_invalid_aterm_toml_at_launch_keeps_its_caret_diagram_in_rows() {
    let path = std::path::Path::new("/home/someone/.config/aterm/aterm.toml");
    let error = aterm_toml::from_str::<crate::app_config::Config>("font_px = \n")
        .err()
        .expect("a malformed aterm.toml");
    let notice = crate::app_config::launch_config_notice(
        path,
        crate::app_config::LaunchConfigProblem::Invalid(&error),
    );
    assert!(
        notice.contains('\n'),
        "this is about a multi-line notice: {notice:?}"
    );

    let msg = launch_load_failure(&notice);
    assert_eq!(
        msg.title, "Couldn't load your settings",
        "terse, and says it is broken"
    );
    assert!(msg.detail.len() > 1, "{:?}", msg.detail);
    for line in &msg.detail {
        assert!(!glued(line), "the diagram is glued to the words: {line:?}");
        assert!(line.chars().all(|ch| !ch.is_control()), "{line:?}");
    }
    assert!(
        msg.detail
            .iter()
            .any(|line| line.starts_with(char::is_whitespace) && line.contains('^')),
        "the caret row keeps its leading whitespace, so Details sets it as a diagram: {:?}",
        msg.detail
    );
    assert!(
        msg.detail
            .last()
            .is_some_and(|line| line.ends_with("and it loads on the next change.")),
        "the consequence arrives whole, not cut mid-word: {:?}",
        msg.detail
    );
    // A one-line problem is unchanged: the notice is its only line.
    let unreadable = crate::app_config::launch_config_notice(
        path,
        crate::app_config::LaunchConfigProblem::Unreadable(&"permission denied"),
    );
    let unreadable_msg = launch_load_failure(&unreadable);
    assert_eq!(unreadable_msg.title, "Couldn't read your settings");
    assert_eq!(
        unreadable_msg.detail.last(),
        Some(&unreadable),
        "the notice is its own last line: {:?}",
        unreadable_msg.detail
    );
}

/// A TRAIL PACK THAT DOES NOT PARSE KEEPS ITS ROWS, alone or in a crowd. Its
/// sentence embeds `aterm_toml`'s caret diagram, and the config families
/// handed it to one `Message::line` too. In a count, rows are what the
/// budget spends: counting one diagram as one line let the engine's cap drop
/// the roll-up, and a list that looks complete while it is not is the
/// defect the roll-up exists to prevent.
#[test]
fn a_trail_pack_parse_error_keeps_its_rows_and_the_roll_up_survives() {
    let error = aterm_toml::from_str::<aterm_toml::Value>("trail = [\n")
        .expect_err("a malformed manifest")
        .to_string();
    assert!(error.contains('\n'), "{error:?}");
    let sentence = format!(
        "cursor_trail_packs[0] \"sparkle.toml\" invalid (TOML schema error: {error}); skipping"
    );

    // Alone: the family's short title (ruling 146), the head as the
    // excerpt, and every row of the sentence behind it.
    let lone = config_family_message(ConfigFamily::CursorTrail, std::slice::from_ref(&sentence));
    assert_eq!(lone.title, "Couldn't apply the cursor trail");
    assert_eq!(
        lone.detail[0],
        "cursor_trail_packs[0] \"sparkle.toml\" invalid"
    );
    assert!(lone.detail.len() > 2, "{:?}", lone.detail);
    for line in &lone.detail {
        assert!(!glued(line), "the diagram is glued to the words: {line:?}");
    }
    assert!(
        lone.detail
            .iter()
            .any(|line| line.trim_start().starts_with('|') && line.contains('^')),
        "the caret row is a line of its own: {:?}",
        lone.detail
    );

    // Enough of them that their ROWS overflow a message, though their count
    // (eight sentences) would not have.
    let many: Vec<String> = (0..8).map(|_| sentence.clone()).collect();
    let msg = config_family_message(ConfigFamily::CursorTrail, &many);
    assert!(msg.detail.len() <= DETAIL_LINES_CAP, "{}", msg.detail.len());
    assert!(
        msg.detail
            .last()
            .is_some_and(|line| line.starts_with("\u{2026} and ")),
        "the roll-up must survive the rows it rolls up: {:?}",
        msg.detail.last()
    );
    for line in &msg.detail {
        assert!(!glued(line), "{line:?}");
    }
}

/// UNDER ONE TAB'S KEY, THE SAME STALL IS THE SAME BY ITS WORDS (the
/// owner's report of 2026-09-27: a restart posted two tabs' stalls the
/// owner had already read): wherever that tab now stands (the place on
/// the move line is not compared), and an overdue one whatever its age.
/// Two tabs' stalls are told apart by their titles (ruling 275: `…in tab
/// 2`), never the sid. NEGATIVE CONTROLS: another stall and another
/// target are other stalls; so is a row before ruling 270 (the move
/// alone on its line) whose words differ, while one whose words are the
/// same is the same stall.
#[test]
fn a_stall_is_the_same_by_its_words_wherever_its_tab_now_stands() {
    use aterm_agent::harness::upgrade::Phase;
    use aterm_agent::harness::upgrade_drive::Row;
    const NOW: u64 = 1_790_529_236;
    // A refusal: a stall a new round meets again. (A round that gave up
    // is no stall since main's 2026-09-27 rest-and-re-arm.)
    let refused = |tab: &str| Row {
        tab: tab.into(),
        from: "2.1.280".into(),
        to: "2.1.283".into(),
        phase: Phase::Failed("not-a-shell-job".into()),
        ..Row::default()
    };
    let (a, b) = ("s-c543f4e0edd3439e5791", "s-0df27d3bf4b3b704621e");
    let row_a = agent_upgrade_stalled(&refused(a), NOW, "in tab 2");
    let row_b = agent_upgrade_stalled(&refused(b), NOW, "in tab 5");
    assert_ne!(row_a.title, row_b.title, "the band tells them apart");
    assert_eq!(row_a.detail[0], row_b.detail[0], "the same stall's words");
    assert!(
        row_a.detail[0].contains("foreground job"),
        "{:?}",
        row_a.detail
    );
    assert!(
        row_a.detail[..STALL_MOVE_LINE + 1]
            .iter()
            .all(|l| !l.contains(a)),
        "never the sid there: {:?}",
        row_a.detail
    );
    assert_eq!(
        row_a.detail[STALL_MOVE_LINE],
        "in tab 2 \u{00b7} Claude Code 2.1.280 \u{2192} 2.1.283"
    );

    // THE SAME STALL, wherever its tab now stands.
    let moved = agent_upgrade_stalled(&refused(a), NOW, "in its tab");
    assert!(same_stall(&row_a.detail, &moved.detail));
    // AND WHEREVER A REFUSED PRESS PUT ITS LINES (ruling 307): the row is
    // restated with the refusal above the stall's lines, and the record it
    // retires into is what a restart reads back.
    let mut restated = vec!["press it again: another step held the upgrade's lock".to_string()];
    restated.extend(row_a.detail.iter().cloned());
    assert!(same_stall(&restated, &row_a.detail), "{restated:?}");
    assert!(same_stall(&row_a.detail, &restated));
    let over = agent_upgrade_stall_over(
        aterm_agent::harness::upgrade::Agent::Claude,
        "harness.upgrade.s-a",
        &restated,
        "in tab 2",
        StallEnd::AsksAgain,
    );
    assert_eq!(over.title, "Claude upgrade asks again in tab 2");
    assert_eq!(
        over.detail,
        [
            format!("was: {}", row_a.detail[0]),
            row_a.detail[STALL_MOVE_LINE].clone()
        ]
    );
    // …in another window too: the place names the window once the owner
    // has two, and two tabs first in their own windows read apart.
    let windowed = |tab: &str, window: usize| {
        agent_upgrade_stalled(&refused(tab), NOW, &tab_place(Some(window), 1))
    };
    assert!(same_stall(&row_a.detail, &windowed(a, 2).detail));
    assert!(same_stall(&windowed(a, 1).detail, &windowed(a, 12).detail));
    assert_eq!(
        windowed(a, 2).detail[STALL_MOVE_LINE],
        "in window 2, tab 1 \u{00b7} Claude Code 2.1.280 \u{2192} 2.1.283"
    );
    assert_eq!(
        windowed(b, 2).title,
        "Couldn't upgrade Claude in window 2, tab 1"
    );
    assert_ne!(windowed(a, 1).title, windowed(b, 2).title);
    let overdue = |age: u64| {
        agent_upgrade_stalled(
            &Row {
                phase: Phase::Pending,
                behind_since: NOW - age,
                wait: "not-idle:busy".into(),
                ..refused(a)
            },
            NOW,
            "in tab 2",
        )
    };
    assert!(
        overdue(7 * 3_600).detail[0].starts_with("behind for "),
        "{:?}",
        overdue(7 * 3_600).detail
    );
    assert!(same_stall(
        &overdue(7 * 3_600).detail,
        &overdue(9 * 3_600).detail
    ));
    // NEGATIVE CONTROLS.
    assert!(
        !same_stall(&row_a.detail, &overdue(7 * 3_600).detail),
        "another stall"
    );
    let newer = agent_upgrade_stalled(
        &Row {
            to: "2.1.284".into(),
            ..refused(a)
        },
        NOW,
        "in tab 2",
    );
    assert!(!same_stall(&row_a.detail, &newer.detail), "another target");

    // A ROW BEFORE RULING 270: the move alone on its line.
    let unplaced = |detail: &[String]| -> Vec<String> {
        let mut d = detail.to_vec();
        d[STALL_MOVE_LINE] = d[STALL_MOVE_LINE]
            .split_once(" \u{00b7} ")
            .map(|(_, moved)| moved.to_string())
            .expect("placed");
        d
    };
    let old = unplaced(&row_a.detail);
    assert!(same_stall(&old, &row_a.detail), "{old:?}");
    assert!(same_stall(&row_a.detail, &old));
    assert!(same_stall(
        &unplaced(&overdue(7 * 3_600).detail),
        &overdue(9 * 3_600).detail
    ));
    // NEGATIVE CONTROLS: another stall, another target, in that layout
    // too; and words that differ.
    assert!(!same_stall(
        &unplaced(&overdue(7 * 3_600).detail),
        &row_a.detail
    ));
    assert!(!same_stall(&unplaced(&newer.detail), &row_a.detail));
    let mut reworded = old.clone();
    reworded[0] = "the move stopped (not-a-shell-job)".to_string();
    assert!(!same_stall(&reworded, &row_a.detail), "{reworded:?}");
    // A move line whose head is no place is compared whole.
    let mut odd = row_a.detail.clone();
    odd[STALL_MOVE_LINE] = format!("tab two \u{00b7} {}", old[STALL_MOVE_LINE]);
    assert!(!same_stall(&odd, &row_a.detail), "{odd:?}");
    for head in [
        "in window two, tab 1",
        "in window 2 tab 1",
        "in window 2, tab",
    ] {
        let mut odd = row_a.detail.clone();
        odd[STALL_MOVE_LINE] = format!("{head} \u{00b7} {}", old[STALL_MOVE_LINE]);
        assert!(!same_stall(&odd, &row_a.detail), "{odd:?}");
    }
}

/// ONE TAB'S UPGRADE KEY, AND NOTHING ELSE (review of 2026-09-27): the
/// carried-row sweep (`upgrade_host::post_upgrade_stalls`) withdraws
/// every live row this filter matches that its view does not own, so a
/// match too wide would take other families' rows down. A tab's key —
/// this build's, cleaned as it prints, or an earlier build's — names its
/// tab; the waiting record's bare key, an empty tab, the finished moves'
/// `done`, a key that only begins with the family's word, and every
/// other family's key name none.
#[test]
fn a_tabs_upgrade_key_names_its_tab_and_no_other_key_does() {
    assert_eq!(agent_upgrade_key_tab("harness.upgrade.s-x"), Some("s-x"));
    let key = agent_upgrade_key("s-c543f4e0edd3439e5791");
    assert_eq!(agent_upgrade_key_tab(&key), Some("s-c543f4e0edd3439e5791"));
    for other in [
        KEY_AGENT_UPGRADE,
        "harness.upgrade.",
        "harness.upgrade.done",
        "harness.upgradex.s-x",
        "harness.upgrades.s-x",
        "harness.s-x",
        "harness.relaunch.s-x",
        "fabric.x",
        "",
    ] {
        assert_eq!(agent_upgrade_key_tab(other), None, "{other:?}");
    }
}

/// A PLACE THAT NAMES THE WINDOW (the owner with more than one window:
/// `in window 2, tab 1`) keeps every title that names the tab within the
/// glass title rule — six words, 48 characters. A title the window's word
/// would push past it names the place after a comma (`Couldn't upgrade
/// Claude yet, window 2, tab 1`: the title lint allows no brackets); every
/// title that fits keeps its words. NEGATIVE CONTROLS: the one-window place
/// never takes the comma form, and the same words placed plainly break the
/// rule.
#[test]
fn a_place_that_names_the_window_keeps_the_titles_short() {
    use aterm_agent::harness::upgrade::{Agent, Phase};
    use aterm_agent::harness::upgrade_drive::Row;
    use aterm_messages::UpgradeWord;
    const NOW: u64 = 1_790_529_236;
    let row = |phase: Phase, wait: &str| Row {
        tab: "s-c543f4e0edd3439e5791".into(),
        from: "2.1.280".into(),
        to: "2.1.283".into(),
        phase,
        wait: wait.into(),
        behind_since: NOW - 7 * 3_600,
        ..Row::default()
    };
    // Behind the agent's own work before any notice: a record, whose title
    // is a wait's, never a failure's.
    let waits = row(Phase::Pending, "not-idle:shell");
    assert!(!waits.asks_on_its_own(NOW), "not yet asking on its own");
    let place = tab_place(Some(2), 1);
    assert_eq!(place, "in window 2, tab 1");
    assert_eq!(tab_place(None, 3), "in tab 3");
    let title = |row: &Row, place: &str| agent_upgrade_stalled(row, NOW, place).title;
    assert_eq!(title(&waits, "in tab 1"), "Claude upgrade waits in tab 1");
    assert_eq!(
        title(&waits, &place),
        "Claude upgrade waits in window 2, tab 1"
    );
    // The one row that stands names the window too.
    let ended = Row {
        exited_at: NOW - 600,
        ..row(Phase::Failed("stale-exit".into()), "")
    };
    assert_eq!(title(&ended, &place), "Claude stopped in window 2, tab 1");
    assert_eq!(
        title(&row(Phase::Failed("not-a-shell-job".into()), ""), &place),
        "Couldn't upgrade Claude in window 2, tab 1"
    );
    let refused = |word| {
        agent_upgrade_word_refused(word, Agent::Claude, ("s-1", "2.1.283"), "busy:x", &place).title
    };
    assert_eq!(
        refused(UpgradeWord::Now),
        "Couldn't upgrade Claude now, window 2, tab 1"
    );
    assert_eq!(
        refused(UpgradeWord::NotToday),
        "Couldn't postpone the upgrade, window 2, tab 1"
    );
    assert_eq!(
        refused(UpgradeWord::Skip),
        "Couldn't skip Claude 2.1.283 in window 2, tab 1"
    );
    for title in [
        title(&waits, &place),
        refused(UpgradeWord::Now),
        refused(UpgradeWord::NotToday),
        refused(UpgradeWord::Skip),
        title(&waits, &tab_place(Some(12), 10)),
    ] {
        assert_eq!(
            aterm_messages::text::glass_title_fault(&title),
            None,
            "{title:?}"
        );
    }
    // NEGATIVE CONTROLS.
    assert_eq!(title(&waits, "in tab 12"), "Claude upgrade waits in tab 12");
    assert_eq!(
        title(&waits, "in its tab"),
        "Claude upgrade waits in its tab"
    );
    assert!(
        aterm_messages::text::glass_title_fault(&format!("Couldn't upgrade Claude {place} yet"))
            .is_some(),
        "placed plainly, the window's word breaks the rule"
    );
}

/// ONE SESSION'S UPGRADE, WAITING, IS A RECORD WORDED BY WHAT HOLDS IT
/// (ruling 380; the owner, 2026-09-28: "What is this alert about 'Codex
/// upgrade waits in tab 1'??? that is confusing to me. do I need to do
/// something? It's not clear. I want upgrades to be applied
/// automatically." — and the review of that day: the record promised "the
/// first pause in your typing" of the incident's goal, which its daemon's
/// turns held). The incident's own row, ten minutes behind: titled by its
/// tab, `Info` in the log with no glass and no tab mark. Where only the
/// ladder's comfort holds it (its screen settling), its sentence is the
/// answer — nothing to do, when it starts, and from when a moment with the
/// agent idle and nobody typing is enough (the ladder's Land rung on the
/// person's clock, or `two hours behind` where the clock is not known);
/// where this tab's own goal holds it, that goal and when it moves; where
/// a turn aterm cannot place holds it, a turn that may be this tab's own
/// (a subagent of this conversation) or another Codex session's — never
/// presumed either, never a goal to pause here — said whole, unclipped. Then the tab and the move. Never `waits`,
/// never `next turn end`, never the pause in the person's typing. A place
/// that would break the title rule leaves the title (the move line keeps
/// it). NEGATIVE CONTROL: the same session six hours behind on a turn
/// still running is no longer healthy — a stall, and its row says it could
/// not be done yet; six hours on another session's turn, its row names
/// that and no goal to pause.
#[test]
fn one_waiting_upgrade_is_a_record_worded_by_what_holds_it() {
    use aterm_agent::harness::upgrade::{Agent, Phase};
    use aterm_agent::harness::upgrade_drive::Row;
    const PENDING: u64 = 1_790_572_868;
    let now = PENDING + 600;
    let row = Row {
        tab: "s-09205e59a0bbd30464d4".into(),
        from: "0.157.1".into(),
        to: "0.158.0".into(),
        agent: Agent::Codex,
        phase: Phase::Pending,
        behind_since: PENDING,
        wait: "settling".into(),
        wait_since: PENDING,
        ..Row::default()
    };
    assert_eq!(row.stall(now), None, "the ladder still has rungs to go");
    assert_eq!(row.lands_by(), Some(PENDING + 7_200));
    let msg = agent_upgrade_waiting_in(&row, "in tab 1");
    assert_eq!(msg.title, "Codex 0.158.0 installs itself in tab 1");
    assert_eq!(aterm_messages::text::glass_title_fault(&msg.title), None);
    assert_eq!((msg.severity, msg.hold), (Severity::Info, Hold::LogOnly));
    let from = crate::presence::local_offset_s().map_or_else(
        || "two hours behind".to_string(),
        |offset| aterm_messages::words::clock_words((PENDING + 7_200) * 1000, offset),
    );
    assert_eq!(
        msg.detail,
        vec![
            format!(
                "nothing to do: it starts on its own at a quiet moment in the tab, and from \
                 {from} as soon as it is idle and nobody has typed there for 20 seconds"
            ),
            "in tab 1 \u{00b7} Codex 0.157.1 \u{2192} 0.158.0".to_string(),
        ]
    );
    // This tab's own goal (the kernel named its thread): what holds it,
    // and when it moves.
    let goal = agent_upgrade_waiting_in(
        &Row {
            wait: "goal".into(),
            quiet: false,
            ..row.clone()
        },
        "in tab 1",
    );
    // The goal pause (the owner's decision of 2026-09-28), under
    // `upgrade_ask = true`: aterm pauses the goal for a moment at the Land
    // rung and resumes it; nothing to do.
    assert_eq!(
        goal.detail[0],
        format!(
            "a goal is running in this tab's Codex; from {from} aterm pauses the goal for a \
             moment to install the new Codex, and resumes it right after: nothing to do"
        )
    );
    // Under the default quiet flow no pause is made: the goal holds it
    // while it runs.
    let quiet = agent_upgrade_waiting_in(
        &Row {
            wait: "goal".into(),
            quiet: true,
            ..row.clone()
        },
        "in tab 1",
    );
    assert_eq!(
        quiet.detail[0],
        "a goal is running in this tab's Codex; it moves once the goal stops: `/goal pause` \
         in this tab stops it"
    );
    // The goal paused for the move: said so, nothing to do.
    let held = agent_upgrade_waiting_in(
        &Row {
            wait: "goal-held".into(),
            goal_held_since: PENDING + 7_300,
            ..row.clone()
        },
        "in tab 1",
    );
    assert_eq!(
        held.detail[0],
        "aterm paused its goal for a moment to install the new Codex, and resumes it right \
         after: nothing to do"
    );
    // A turn aterm cannot place: this conversation's subagent or another
    // session's, never presumed either, never a goal here.
    let unplaced = agent_upgrade_waiting_in(
        &Row {
            wait: "daemon-busy".into(),
            ..row.clone()
        },
        "in tab 1",
    );
    assert!(
        unplaced.detail[0].starts_with(
            "a turn is running on its Codex daemon that may be this tab's own (a subagent of \
             this conversation) or another Codex session's"
        ),
        "{:?}",
        unplaced.detail
    );
    for msg in [&msg, &goal, &held, &unplaced] {
        for line in std::iter::once(&msg.title).chain(&msg.detail) {
            assert!(
                !line.contains("waits")
                    && !line.contains("turn end")
                    && !line.contains("pause in your typing"),
                "{line}"
            );
        }
    }
    assert!(!unplaced.detail[0].contains("goal") && !unplaced.detail[0].contains("pause"));
    // A place that breaks the title rule leaves the title, not the record.
    let far = agent_upgrade_waiting_in(
        &Row {
            agent: Agent::Claude,
            from: "2.1.282".into(),
            to: "2.1.283".into(),
            ..row.clone()
        },
        "in window 2, tab 11",
    );
    assert_eq!(far.title, "Claude Code 2.1.283 installs itself");
    assert!(far.detail[1].starts_with("in window 2, tab 11 \u{00b7} "));
    // NEGATIVE CONTROL: six hours behind, the turn still running.
    let late = PENDING + 6 * 3_600;
    let overdue = Row {
        wait: "not-idle:busy".into(),
        ..row.clone()
    };
    assert_eq!(overdue.stall(late).as_deref(), Some("overdue"));
    let stalled = agent_upgrade_stalled(&overdue, late, "in tab 1");
    // A wait is a record titled as one (ruling 423), never a failure.
    assert_eq!(stalled.title, "Codex upgrade waits in tab 1");
    assert_eq!(
        stalled.detail[0],
        "behind for 6 h: its turn is still running"
    );
    // Six hours on a turn aterm cannot place: named so — a subagent of
    // this conversation or another session's, neither presumed — no goal
    // to pause.
    let other = agent_upgrade_stalled(
        &Row {
            wait: "daemon-busy".into(),
            ..row.clone()
        },
        late,
        "in tab 1",
    );
    assert!(
        other.detail[0].contains("a subagent of this conversation")
            && other.detail[0].ends_with("or another Codex session's")
            && !other.detail[0].contains("most often"),
        "{:?}",
        other.detail
    );
    for line in &other.detail {
        assert!(!line.contains("goal") && !line.contains("pause"), "{line}");
    }
    // Six hours on its own goal under `upgrade_ask = true` (something
    // kept the pause off): aterm pauses it once nothing else holds the
    // tab, and pausing it here moves it now.
    let own = agent_upgrade_stalled(
        &Row {
            wait: "goal".into(),
            quiet: false,
            ..row.clone()
        },
        late,
        "in tab 1",
    );
    assert!(
        own.detail[1].starts_with(
            "aterm pauses the goal for a moment to move it once nothing else holds the tab, \
             and resumes it after; pausing it in this tab moves it now"
        ),
        "{:?}",
        own.detail
    );
    // Under the default quiet flow aterm makes no pause: none is
    // promised.
    let quiet = agent_upgrade_stalled(
        &Row {
            wait: "goal".into(),
            quiet: true,
            ..row.clone()
        },
        late,
        "in tab 1",
    );
    assert!(
        quiet.detail[1]
            .starts_with("it moves once the goal stops: `/goal pause` in this tab stops it; "),
        "{:?}",
        quiet.detail
    );
    // A GOAL ATERM PAUSED AND HAS NOT RESUMED: a row of its own, worded as
    // the wait it is, with the one hand step, and no word of the
    // upgrade's offered. NEGATIVE CONTROL: the move done and nothing left
    // paused — no row.
    let left = Row {
        phase: aterm_agent::harness::upgrade::Phase::Done,
        goal_left_since: PENDING + 9_000,
        ..row.clone()
    };
    assert_eq!(left.stall(late).as_deref(), Some("goal-paused"));
    let msg = agent_upgrade_stalled(&left, late, "in tab 1");
    assert_eq!(msg.title, "Codex goal still paused in tab 1");
    assert_eq!((msg.severity, msg.hold), (Severity::Warn, Hold::STANDING));
    assert!(
        msg.detail[0].contains("has not been able to resume it yet")
            && msg.detail[1] == "type /goal resume in the tab to go on with the goal now",
        "{:?}",
        msg.detail
    );
    assert!(agent_upgrade_words(&left, late).is_empty());
    assert!(agent_upgrade_capsules(&left, late).is_empty());
    // LEFT PAUSED ON PURPOSE, its thread fallen into a sandbox: its own
    // words, the relaunch its hand step, no word of the upgrade's.
    let sandboxed = Row {
        wait: "goal-sandboxed".into(),
        ..left.clone()
    };
    assert_eq!(sandboxed.stall(late).as_deref(), Some("goal-sandboxed"));
    let msg = agent_upgrade_stalled(&sandboxed, late, "in tab 1");
    assert_eq!(msg.title, "Codex goal left paused in tab 1");
    assert!(
        msg.detail[0].contains("fell into a sandbox")
            && msg.detail[1].contains("--dangerously-bypass-approvals-and-sandbox"),
        "{:?}",
        msg.detail
    );
    assert!(agent_upgrade_words(&sandboxed, late).is_empty());
    assert!(agent_upgrade_capsules(&sandboxed, late).is_empty());
    let done = Row {
        phase: aterm_agent::harness::upgrade::Phase::Done,
        ..row
    };
    assert_eq!(done.stall(late), None);
}

/// A NOTICE WAITING BEHIND A FULL QUEUE IS TOLD HOW IT MOVES (review of
/// 2026-09-27: the row said `Upgrade now` does not move it and that it
/// "moves once that ends", of a limit over by every word, while nothing of
/// the upgrade's own ever ended the wait). An overdue upgrade waiting
/// `queued`: its words name the wait, its remedy says the agent's next
/// run moves it — typing in its tab, or `Upgrade now`, which types the
/// question once more — and promises no period of its own (ruling 307);
/// `Upgrade now` is
/// offered, and the record says the upgrade waits (Remedy::Now); the
/// queue's rest is a day at most, so nothing is a row. NEGATIVE CONTROL: waiting on the READY
/// answer, `Upgrade now` is not offered and the remedy says it does not
/// move it.
#[test]
fn a_full_queues_row_says_the_next_turn_or_upgrade_now_moves_it() {
    use aterm_agent::harness::upgrade::Phase;
    use aterm_agent::harness::upgrade_drive::{Remedy, Row};
    use aterm_messages::UpgradeWord;
    const NOW: u64 = 1_790_529_236;
    let waiting = |wait: &str| Row {
        tab: "s-c543f4e0edd3439e5791".into(),
        from: "2.1.280".into(),
        to: "2.1.283".into(),
        phase: Phase::Announced { at_s: NOW, asks: 1 },
        behind_since: NOW - 30 * 86_400,
        wait: wait.into(),
        ..Row::default()
    };
    let queued = waiting("queued");
    assert_eq!(queued.remedy(NOW), Some(Remedy::Now));
    assert!(
        !queued.asks_on_its_own(NOW),
        "the person's to move, and 30 days behind"
    );
    let msg = agent_upgrade_stalled(&queued, NOW, "in tab 2");
    assert_eq!(msg.title, "Claude upgrade waits in tab 2");
    assert_eq!(msg.hold, Hold::LogOnly);
    // A person's words, not the upgrade's own (ruling 307: `its notice
    // waits unread` read as a band row, and `30d` as a code).
    assert_eq!(
        msg.detail[0],
        "behind for 30 days: it has not read the upgrade's question yet"
    );
    let remedy = &msg.detail[1];
    for words in [
        "moves when Claude next runs",
        "type in its tab",
        UpgradeWord::Now.label(),
        "once more",
        "waiting longer each time, up to a day",
    ] {
        assert!(remedy.contains(words), "{words}: {remedy}");
    }
    assert!(!remedy.contains("once that ends"), "{remedy}");
    // A person's words (ruling 307): never the session's own; and no one
    // interval it does not keep (main's growing rest).
    assert!(!remedy.contains("session"), "{remedy}");
    assert!(!remedy.contains("every 2h30m"), "no one interval: {remedy}");
    assert!(remedy.len() <= aterm_messages::DETAIL_LINE_CAP, "{remedy}");
    assert!(agent_upgrade_words(&queued, NOW).contains(&UpgradeWord::Now));
    // ITS ANSWER SAYS WHAT THE PRESS DID (ruling 307): the question typed
    // once more — never `the wait for a quiet tab is waived`, of a session
    // already idle. NEGATIVE CONTROL: any other wait keeps those words.
    let asked = agent_upgrade_worded(&queued, UpgradeWord::Now, "in tab 2", true);
    assert_eq!(asked.title, "Claude is asked again in tab 2");
    assert!(asked.detail[0].contains("once more"), "{:?}", asked.detail);
    assert!(!asked.detail[0].contains("waived"), "{:?}", asked.detail);
    let hurried = agent_upgrade_worded(&queued, UpgradeWord::Now, "in tab 2", false);
    assert!(hurried.detail[0].contains("waived"), "{:?}", hurried.detail);
    // Never "waits to go idle" (the audit of 2026-09-30): under the word
    // aterm pauses a goal-mode Codex's goal, whose turns never end.
    assert!(!hurried.detail[0].contains("idle"), "{:?}", hurried.detail);
    // NEGATIVE CONTROL.
    let ready = waiting("not-ready");
    assert_eq!(ready.remedy(NOW), Some(Remedy::Waits));
    let msg = agent_upgrade_stalled(&ready, NOW, "in tab 2");
    assert!(
        msg.detail[1].contains("does not move it"),
        "{:?}",
        msg.detail
    );
    assert!(!agent_upgrade_words(&ready, NOW).contains(&UpgradeWord::Now));
}

// ---- round 44: the second adversarial hunt's host repros -------------

/// Overdue rows of `n` Claude tabs, each with a real-length tab id
/// (`s-` and 20 hex digits).
fn r44_overdue_tabs(
    n: u64,
    now: u64,
    phase: &aterm_agent::harness::upgrade::Phase,
) -> Vec<aterm_agent::harness::upgrade_drive::Row> {
    (0..n)
        .map(|i| aterm_agent::harness::upgrade_drive::Row {
            session: format!("conv-{i}"),
            tab: format!("s-{i:020x}"),
            from: "2.1.283".into(),
            to: "2.1.284".into(),
            source: "managed".into(),
            phase: phase.clone(),
            behind_since: now - 25 * 3_600 - i * 60,
            wait: "queued".into(),
            wait_since: now - 25 * 3_600,
            ..Default::default()
        })
        .collect()
}

/// host-upgrade-R44-UP-3 — ruling 363: the agent's row's last line carries
/// EVERY stall it stands for (each tab's id and target), which
/// `agent_upgrade_group_members` reads back after a restart so a row the
/// owner put down is not posted again. Six tabs with real ids must read
/// back six stalls.
#[test]
fn r44_up3_an_agents_row_of_six_tabs_names_every_stall_it_stands_for() {
    const NOW: u64 = 1_790_311_076;
    let rows = r44_overdue_tabs(6, NOW, &aterm_agent::harness::upgrade::Phase::Pending);
    let members: Vec<(&aterm_agent::harness::upgrade_drive::Row, String)> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| (r, format!("in tab {}", i + 1)))
        .collect();
    let msg = super::agent_upgrade_stalled_group(&members, NOW).expect("a row");
    let named = super::agent_upgrade_group_members(&msg.detail);
    assert_eq!(named.len(), 6, "{:?} -> {named:?}", msg.detail.last());
}

/// THE ANSWER TO A WORD ON AN AGENT'S ROW (the audit of 2026-09-30): the
/// answer said `… in 3 tabs when idle` — false of a goal-mode Codex,
/// whose goal the word pauses — and `the wait for a quiet tab is waived`
/// of tabs already idle with their question unread in a full queue, or
/// whose round gave up: those are asked once more, and move once the
/// agent agrees. Each arm a glass title and whole lines (a line past the
/// cap is clipped to an ellipsis), for either agent and a dozen tabs.
#[test]
fn the_answer_to_upgrade_now_on_an_agents_row_says_what_it_did() {
    use aterm_agent::harness::upgrade::Agent;
    use aterm_messages::UpgradeWord;
    let moves = super::agent_upgrade_group_worded(Agent::Claude, UpgradeWord::Now, 3, 0);
    assert_eq!(moves.title, "Claude moves in 3 tabs");
    assert_eq!(
        moves.detail[0],
        "the wait for a quiet tab is waived; each moves at its next pause, never over \
         typing, a draft, a dialog or running work"
    );
    let asked = super::agent_upgrade_group_worded(Agent::Claude, UpgradeWord::Now, 3, 3);
    assert_eq!(asked.title, "Claude is asked again in 3 tabs");
    assert_eq!(
        asked.detail[0],
        "the upgrade's question is typed once more; each moves once Claude agrees"
    );
    let mixed = super::agent_upgrade_group_worded(Agent::Claude, UpgradeWord::Now, 3, 1);
    assert_eq!(mixed.title, "Claude moves in 2 tabs");
    assert_eq!(
        mixed.detail[0],
        "each moves at its next pause; in 1 more the upgrade's question is typed once more, \
         and it moves once Claude agrees"
    );
    let one = super::agent_upgrade_group_worded(Agent::Codex, UpgradeWord::Now, 1, 0);
    assert_eq!(one.title, "Codex moves in 1 tab");
    assert!(
        one.detail[0].contains("; it moves at its next pause"),
        "{:?}",
        one.detail
    );
    for agent in [Agent::Claude, Agent::Codex] {
        for (n, asked) in [(1, 0), (1, 1), (3, 0), (3, 1), (3, 3), (12, 11)] {
            for word in UpgradeWord::ALL {
                let msg = super::agent_upgrade_group_worded(agent, word, n, asked);
                assert!(
                    aterm_messages::text::glass_title_fault(&msg.title).is_none(),
                    "{}",
                    msg.title
                );
                for line in &msg.detail {
                    assert!(!line.contains("idle"), "{line}");
                    // `Message::line` clips at the cap: what a line past
                    // it leaves behind is the ellipsis.
                    assert!(!line.ends_with('\u{2026}'), "clipped: {line}");
                }
            }
        }
    }
}

/// AN AGENT'S ROW NAMES `--now` IN ITS SHELL LINE ONLY WHERE A TAB TAKES
/// IT (the audit of 2026-09-30): two tabs whose waits no word moves past
/// (a turn running, the agent's own shell) had no `Upgrade now` button,
/// and the shell line told the owner to run `--now` all the same — a word
/// recorded that moves nothing. NEGATIVE CONTROL: tabs whose question
/// waits unread take it, and the line names it.
#[test]
fn an_agents_row_names_now_in_its_shell_line_only_where_a_tab_takes_it() {
    use aterm_messages::UpgradeWord;
    const NOW: u64 = 1_790_311_076;
    let rows: Vec<aterm_agent::harness::upgrade_drive::Row> =
        r44_overdue_tabs(2, NOW, &aterm_agent::harness::upgrade::Phase::Pending)
            .into_iter()
            .zip(["not-idle:busy", "not-idle:shell"])
            .map(|(r, wait)| aterm_agent::harness::upgrade_drive::Row {
                wait: wait.into(),
                ..r
            })
            .collect();
    let group = |rows: &[aterm_agent::harness::upgrade_drive::Row]| {
        let members: Vec<(&aterm_agent::harness::upgrade_drive::Row, String)> = rows
            .iter()
            .enumerate()
            .map(|(i, r)| (r, format!("in tab {}", i + 1)))
            .collect();
        super::agent_upgrade_stalled_group(&members, NOW).expect("a row")
    };
    let held = group(&rows);
    assert!(
        !held
            .actions
            .iter()
            .any(|i| i.label() == UpgradeWord::Now.label()),
        "{:?}",
        held.actions
    );
    let shell = held.detail.last().expect("the shell line");
    assert!(
        shell.starts_with("the same in any shell: `aterm harness upgrade <tab> --skip`"),
        "{shell}"
    );
    assert!(!shell.contains("--now"), "{shell}");
    // NEGATIVE CONTROL.
    let queued = group(&r44_overdue_tabs(
        2,
        NOW,
        &aterm_agent::harness::upgrade::Phase::Pending,
    ));
    assert!(
        queued
            .detail
            .last()
            .expect("the shell line")
            .starts_with("the same in any shell: `aterm harness upgrade <tab> --now` or `--skip`"),
        "{:?}",
        queued.detail
    );
    // MIXED: one tab takes it, one does not — `--now` is named for the
    // one alone, and the list still reads back both.
    let mut mixed = r44_overdue_tabs(2, NOW, &aterm_agent::harness::upgrade::Phase::Pending);
    mixed[1].wait = "not-idle:shell".into();
    let mixed_row = group(&mixed);
    let (now_line, rest) = mixed_row.detail.split_last().expect("lines");
    assert_eq!(
        now_line,
        &format!(
            "`aterm harness upgrade <tab> --now` only for {}",
            mixed[0].tab
        )
    );
    let shell = rest.last().expect("the shell line");
    assert!(
        shell.starts_with("the same in any shell: `aterm harness upgrade <tab> --skip`"),
        "{shell}"
    );
    assert_eq!(
        super::agent_upgrade_group_members(&mixed_row.detail).len(),
        2
    );
}

/// AN AGENT'S ROW LISTS EACH TAB AS THE COMMAND BESIDE IT TAKES IT (the
/// audit of 2026-09-30): the list after `aterm harness upgrade <tab>`
/// read `s-aaa@2.1.284`, the window's comparison key, and pasted in it
/// named no tab (`no upgrade waits in that tab now`). Now each id stands
/// alone, its build said once after the run of tabs bound for it
/// (`s-aaa, s-bbb (to 2.1.284)`), and every stall reads back — across
/// wrapped lines, with two builds — as does a list an earlier build
/// wrote.
#[test]
fn an_agents_row_lists_each_tab_as_the_command_takes_it() {
    const NOW: u64 = 1_790_311_076;
    let mut rows = r44_overdue_tabs(8, NOW, &aterm_agent::harness::upgrade::Phase::Pending);
    rows[1].to = "2.1.285".into();
    rows[4].to = "2.1.285".into();
    let members: Vec<(&aterm_agent::harness::upgrade_drive::Row, String)> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| (r, format!("in tab {}", i + 1)))
        .collect();
    let msg = super::agent_upgrade_stalled_group(&members, NOW).expect("a row");
    let listed: Vec<&String> = msg
        .detail
        .iter()
        .filter(|l| l.contains(super::GROUP_TABS))
        .collect();
    assert!(listed.len() > 1, "wraps: {listed:#?}");
    for line in &listed {
        assert!(!line.contains('@'), "{line}");
        assert!(
            line.chars().count() <= aterm_messages::DETAIL_LINE_CAP,
            "{line}"
        );
        // Each line ends with the build its last tabs are bound for.
        assert!(line.ends_with(')'), "{line}");
    }
    let mut read: Vec<(String, String)> = super::agent_upgrade_group_members(&msg.detail);
    let mut want: Vec<(String, String)> =
        rows.iter().map(|r| (r.tab.clone(), r.to.clone())).collect();
    read.sort();
    want.sort();
    assert_eq!(read, want);
    // A tab bound for no build is read with none, whatever follows it.
    rows[7].to = String::new();
    let members: Vec<(&aterm_agent::harness::upgrade_drive::Row, String)> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| (r, format!("in tab {}", i + 1)))
        .collect();
    let msg = super::agent_upgrade_stalled_group(&members, NOW).expect("a row");
    let read = super::agent_upgrade_group_members(&msg.detail);
    assert!(
        read.contains(&(rows[7].tab.clone(), String::new())),
        "{read:?} from {:#?}",
        msg.detail
    );
    assert_eq!(read.len(), 8);
    // One tab left: its id alone, then its build.
    let one = super::agent_upgrade_stalled_group(&members[..1], NOW).expect("a row");
    let shell = one.detail.last().expect("its shell line");
    assert!(
        shell.ends_with(&format!(" \u{2014} tabs {} (to 2.1.284)", rows[0].tab)),
        "{shell}"
    );
    assert_eq!(
        super::agent_upgrade_group_members(&one.detail),
        [(rows[0].tab.clone(), "2.1.284".to_string())]
    );
    // What an earlier build wrote reads back the same.
    let legacy = [
        "the same in any shell: `aterm harness upgrade <tab> --now` or `--skip` \u{2014} tabs \
         s-aaa@2.1.284, s-bbb@2.1.285"
            .to_string(),
        "also \u{2014} tabs s-ccc@".to_string(),
    ];
    assert_eq!(
        super::agent_upgrade_group_members(&legacy),
        [
            ("s-aaa".to_string(), "2.1.284".to_string()),
            ("s-bbb".to_string(), "2.1.285".to_string()),
            ("s-ccc".to_string(), String::new()),
        ]
    );
}

/// host-upgrade-R44-UP-6 — ruling 380 (i) and the builder's own doc: the
/// agent's row's title takes the most serious of its tabs' OWN words.
/// Tabs whose rounds gave up, past the move clock, each read `Couldn't
/// upgrade Claude in tab N` (no `yet`); their agent's row must not say
/// `… yet` either.
#[test]
fn r44_up6_an_agents_row_says_yet_only_where_its_tabs_rows_do() {
    use aterm_agent::harness::upgrade::{GAVE_UP, Phase};
    const NOW: u64 = 1_790_311_076;
    let rows: Vec<aterm_agent::harness::upgrade_drive::Row> =
        r44_overdue_tabs(2, NOW, &Phase::Failed(GAVE_UP.to_string()))
            .into_iter()
            .map(|r| aterm_agent::harness::upgrade_drive::Row {
                retry_at: NOW + 3_600,
                release: GAVE_UP.to_string(),
                wait: String::new(),
                ..r
            })
            .collect();
    let members: Vec<(&aterm_agent::harness::upgrade_drive::Row, String)> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| (r, format!("in tab {}", i + 1)))
        .collect();
    for (row, place) in &members {
        let own = super::agent_upgrade_stalled(row, NOW, place);
        assert!(!own.title.ends_with(" yet"), "its own row: {}", own.title);
    }
    let group = super::agent_upgrade_stalled_group(&members, NOW).expect("a row");
    assert!(
        !group.title.ends_with(" yet"),
        "the agent's row says `yet` where none of its tabs' rows does: {}",
        group.title
    );
}

/// AN AGENT'S ROW SAYS WHICH TABS `UPGRADE NOW` ONLY ASKS AGAIN (the
/// review of 2026-09-30): its button line said `Upgrade now moves all 3
/// at their next pauses` of tabs whose question waited unread behind a
/// full queue and of rounds that gave up, where the word only types the
/// question once more — as each tab's own row, and the answer to the
/// press, say. NEGATIVE CONTROL: tabs backing off their own typed text
/// move at their next pauses, on the same line.
#[test]
fn an_agents_row_says_which_tabs_upgrade_now_only_asks_again() {
    use aterm_agent::harness::upgrade::{GAVE_UP, Phase};
    use aterm_agent::harness::upgrade_drive::Row;
    const NOW: u64 = 1_790_311_076;
    let gave_up = |r: &Row| Row {
        phase: Phase::Failed(GAVE_UP.to_string()),
        retry_at: NOW + 3_600,
        release: GAVE_UP.to_string(),
        wait: String::new(),
        ..r.clone()
    };
    let backing_off = |r: &Row| Row {
        wait: "left-typed-backoff".into(),
        ..r.clone()
    };
    let does = |rows: &[Row]| -> String {
        let members: Vec<(&Row, String)> = rows
            .iter()
            .enumerate()
            .map(|(i, r)| (r, format!("in tab {}", i + 1)))
            .collect();
        let msg = super::agent_upgrade_stalled_group(&members, NOW).expect("a row");
        msg.detail[1].clone()
    };
    let queued = r44_overdue_tabs(3, NOW, &Phase::Pending);
    let [a, b, c] = [&queued[0], &queued[1], &queued[2]];
    assert!(super::upgrade_now_asks_again(a));
    assert!(super::upgrade_now_asks_again(&gave_up(b)));
    assert!(!super::upgrade_now_asks_again(&backing_off(c)));
    // Two waiting on their question, one given up: each asked once more.
    let said = does(&[a.clone(), gave_up(b), c.clone()]);
    assert!(
        said.starts_with("Upgrade now asks all 3 once more; "),
        "{said}"
    );
    // One backing off moves at its next pause; the other two are asked.
    let said = does(&[backing_off(a), gave_up(b), c.clone()]);
    assert!(
        said.starts_with(
            "Upgrade now moves tab 1 at its next pause and asks 2 of them once more; "
        ),
        "{said}"
    );
    // NEGATIVE CONTROL.
    let said = does(&[backing_off(a), backing_off(b), backing_off(c)]);
    assert!(
        said.starts_with("Upgrade now moves all 3 at their next pauses; "),
        "{said}"
    );
    assert!(!said.contains("once more"), "{said}");
    // UNDER THE QUIET FLOW (round eight, #4) a round that gave up asks
    // nothing once re-armed: the word moves it at its next pause, and its
    // own row says the next round moves it, never that it asks again.
    let quiet = Row {
        quiet: true,
        ..gave_up(b)
    };
    assert!(!super::upgrade_now_asks_again(&quiet));
    let said = does(&[backing_off(a), quiet.clone()]);
    assert!(
        said.starts_with("Upgrade now moves both at their next pauses; "),
        "{said}"
    );
    let own = super::agent_upgrade_stalled(&quiet, NOW, "in tab 2");
    let words = own.detail.join(" ");
    assert!(!words.contains("asks"), "{words}");
    assert!(words.contains("moves it"), "{words}");
}

/// A QUIET ROUND ITS AGENT'S OWN WORK GAVE UP ON OFFERS NO `UPGRADE NOW`
/// (round eight, #4's review): the quiet flow gives a round up only once
/// the agent's own work has held its restart past the re-ask window, and
/// `Upgrade now` re-arms a round the quiet restart's gate holds on that
/// same work (`background`, whatever the owner said). The row said "it
/// moves on its own at a quiet moment …; Upgrade now moves it at its next
/// pause" and offered the button, which moved nothing, and then read
/// "Upgrade now does not move it past that wait". Now it is a wait on its
/// own work — the work seen since its notice, or running under the agent
/// at the last look, named on the row — with no `Upgrade now`, on its own
/// row and on its agent's. NEGATIVE CONTROL: a quiet give-up no work held
/// (a status that only lagged, which `--now` reads past) keeps the button
/// and its words.
#[test]
fn a_quiet_round_its_own_work_gave_up_on_offers_no_upgrade_now() {
    use aterm_agent::harness::upgrade::{GAVE_UP, Phase};
    use aterm_agent::harness::upgrade_drive::{HeldBy, Remedy, Row};
    use aterm_messages::UpgradeWord;
    const NOW: u64 = 1_790_311_076;
    let tabs = r44_overdue_tabs(2, NOW, &Phase::Pending);
    let gave_up = |r: &Row| Row {
        phase: Phase::Failed(GAVE_UP.to_string()),
        retry_at: NOW + 3_600,
        release: GAVE_UP.to_string(),
        wait: "failed".into(),
        quiet: true,
        ..r.clone()
    };
    let seen = Row {
        work_seen: true,
        ..gave_up(&tabs[1])
    };
    let running = Row {
        held_by: vec![HeldBy {
            pid: 4242,
            name: "zsh".into(),
            since: NOW - 7_200,
        }],
        ..gave_up(&tabs[1])
    };
    for (row, name) in [(&seen, "work seen"), (&running, "work running")] {
        assert!(row.rests_on_own_work(), "{name}");
        assert_eq!(row.remedy(NOW), Some(Remedy::Waits), "{name}");
        assert!(
            !super::agent_upgrade_words(row, NOW).contains(&UpgradeWord::Now),
            "{name}"
        );
        let own = super::agent_upgrade_stalled(row, NOW, "in tab 2");
        let words = own.detail.join(" ");
        assert!(
            words.contains("Upgrade now cannot move it past its own work"),
            "{name}: {words}"
        );
        assert!(
            !words.contains("moves it at its next pause"),
            "{name}: {words}"
        );
        assert!(
            own.actions.iter().all(|intent| !matches!(
                intent,
                Intent::AgentUpgrade {
                    word: UpgradeWord::Now,
                    ..
                }
            )),
            "{name}: {:?}",
            own.actions
        );
        let members = [(&tabs[0], "in tab 1".to_string()), (row, "in tab 2".into())];
        let line = super::agent_upgrade_stalled_group(&members, NOW)
            .expect("a row")
            .detail[1]
            .clone();
        assert!(
            line.starts_with("Upgrade now asks tab 1 once more; the other one moves once"),
            "{name}: {line}"
        );
    }
    assert!(
        running
            .held_words(NOW)
            .is_some_and(|words| words.contains("zsh")),
        "what holds it is named"
    );
    // NEGATIVE CONTROL: no work seen nor running — a status that lagged.
    let lagged = gave_up(&tabs[1]);
    assert!(!lagged.rests_on_own_work());
    assert_eq!(lagged.remedy(NOW), Some(Remedy::AskAgain));
    assert!(super::agent_upgrade_words(&lagged, NOW).contains(&UpgradeWord::Now));
    let words = super::agent_upgrade_stalled(&lagged, NOW, "in tab 2")
        .detail
        .join(" ");
    assert!(words.contains("moves it at its next pause"), "{words}");
    // NEGATIVE CONTROL: the ask flow's give-up, its own work seen, is
    // asked again by the word, as ever.
    let asked = Row {
        quiet: false,
        ..seen.clone()
    };
    assert!(!asked.rests_on_own_work());
    assert_eq!(asked.remedy(NOW), Some(Remedy::AskAgain));
}

/// A QUIET GIVE-UP `UPGRADE NOW` DOES MOVE KEEPS THE BUTTON (round eight,
/// the review of #4's review). Its work seen since the notice, each of
/// these read "Upgrade now cannot move it past its own work: it moves once
/// that ends" with no button, and neither moves when that work ends:
/// a CODEX round (its lane re-arms a resting round only when the rest is
/// over, whatever the TUI's work does), and a Claude Code round whose
/// shells ended while its status still says `shell` over an idle screen
/// (the end of the work re-arms only through a gate that never reads that
/// lag past). The owner's `--now` moves both: the quiet restart goes at
/// the first idle point with nothing under the agent, the lag read past on
/// the owner's word. NEGATIVE CONTROL: the Claude Code round while its
/// shells still run is a wait on them.
#[test]
fn a_quiet_give_up_upgrade_now_does_move_keeps_its_button() {
    use aterm_agent::harness::upgrade::{Agent, GAVE_UP, Phase};
    use aterm_agent::harness::upgrade_drive::{HeldBy, Remedy, Row};
    use aterm_messages::UpgradeWord;
    const NOW: u64 = 1_790_311_076;
    let tabs = r44_overdue_tabs(2, NOW, &Phase::Pending);
    let gave_up = Row {
        phase: Phase::Failed(GAVE_UP.to_string()),
        retry_at: NOW + 3_600,
        release: GAVE_UP.to_string(),
        wait: "failed".into(),
        quiet: true,
        work_seen: true,
        ..tabs[1].clone()
    };
    let codex = Row {
        agent: Agent::Codex,
        ..gave_up.clone()
    };
    let lagging = Row {
        wait: "status-stale:shell".into(),
        ..gave_up.clone()
    };
    for (row, name) in [(&codex, "codex"), (&lagging, "status lags")] {
        assert_eq!(row.remedy(NOW), Some(Remedy::AskAgain), "{name}");
        assert!(
            super::agent_upgrade_words(row, NOW).contains(&UpgradeWord::Now),
            "{name}: the button that moves it"
        );
        let words = super::agent_upgrade_stalled(row, NOW, "in tab 2")
            .detail
            .join(" ");
        assert!(
            words.contains("moves it at its next pause"),
            "{name}: {words}"
        );
        assert!(
            !words.contains("cannot move it past its own work"),
            "{name}: {words}"
        );
    }
    // NEGATIVE CONTROL: the shells still run under the Claude Code agent.
    let running = Row {
        held_by: vec![HeldBy {
            pid: 4242,
            name: "zsh".into(),
            since: NOW - 7_200,
        }],
        ..gave_up
    };
    assert_eq!(running.remedy(NOW), Some(Remedy::Waits));
    assert!(!super::agent_upgrade_words(&running, NOW).contains(&UpgradeWord::Now));
}
