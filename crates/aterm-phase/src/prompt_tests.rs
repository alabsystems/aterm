// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::fixtures::*;
use super::*;

#[test]
fn a_boxs_first_row_is_its_own_never_a_transcript_row() {
    assert_eq!(prompt_box_first_row(&bash_one_row()), Some(2));
    assert_eq!(prompt_box_first_row(&bash_multi_row_with_note()), Some(0));
    let question = " Do you trust the files in this folder?";
    let transcript: Vec<String> = (0..40)
        .map(|i| format!("⏺ transcript row {i} done"))
        .collect();
    let under = |above: Vec<String>| {
        let mut r = above;
        r.push(String::new());
        r.extend(trust_box());
        r
    };
    for screen in [
        trust_box(),
        under(transcript.clone()),
        under(vec!["─".repeat(80)]),
        under(vec!["╰".to_string() + &"─".repeat(40) + "╯"]),
        under(vec!["✻ Worked for 3m 21s · done 8:50 PM".to_string()]),
        under(vec!["  ⎿  output under the gutter".to_string()]),
    ] {
        let first = prompt_box_first_row(&screen).expect("a live box");
        assert_eq!(screen[first], question, "{screen:?}");
    }
    let mut quoted = rows(&[
        "⏺ Here is the plan.",
        "",
        " Would you like to proceed with this plan?",
        "   │ step one",
        "   │",
        "   │ step two",
        "",
        " ❯ 1. Yes",
        "   2. No",
        "",
        " Esc to cancel",
    ]);
    quoted.extend(composer("  ? for shortcuts"));
    assert_eq!(prompt_box_first_row(&quoted), Some(2));
    let tall = under(transcript);
    let (a, _) = prompt_box_span(&tall).unwrap();
    assert_eq!(tall[a], question, "the span starts at the box's own row");
    assert_eq!(prompt_box_first_row(&rows(&["⏺ hello"])), None);
}

#[test]
fn a_one_row_bash_box_parses_command_description_and_four_options() {
    let p = parse_prompt(&bash_one_row()).expect("a prompt");
    assert_eq!(p.kind, PromptKind::Bash);
    assert_eq!(p.command, "git log --oneline -5");
    assert_eq!(p.description, "Show the five most recent commits");
    assert_eq!(p.options.len(), 4, "{:?}", p.options);
    assert_eq!(p.options[0], (1, "Yes".to_string()));
    assert_eq!(
        p.options[1],
        (2, "Yes, and don’t ask again for: git log *".to_string())
    );
    assert!(p.options[2].1.starts_with("Yes, and switch to auto mode"));
    assert_eq!(p.options[3], (4, "No".to_string()));
    assert!(p.notes.is_empty(), "{:?}", p.notes);
}

#[test]
fn a_multi_row_bash_box_joins_the_bars_skips_tips_and_reads_the_note() {
    let p = parse_prompt(&bash_multi_row()).expect("a prompt");
    assert_eq!(p.kind, PromptKind::Bash);
    assert_eq!(
        p.command,
        "cd ~/ay && git status --short --branch && git pull 2>&1 | tail -20"
    );
    assert_eq!(p.description, "Sync the checkout before verifying");
    assert_eq!(p.notes, vec!["This command requires approval".to_string()]);
    assert_eq!(
        p.options,
        vec![
            (1, "Yes".to_string()),
            (2, "Yes, and don’t ask again for: git pull *".to_string()),
            (3, "No".to_string()),
        ]
    );
}

#[test]
fn a_bar_led_description_and_a_bar_led_note_are_read_as_such() {
    let p = parse_prompt(&bash_multi_row_with_note()).expect("a prompt");
    assert_eq!(p.kind, PromptKind::Bash);
    assert!(
        p.command.starts_with("cd /work/trust-vc && S="),
        "{}",
        p.command
    );
    assert!(p.command.ends_with("sed 's#^t_mb/##'"), "{}", p.command);
    assert!(!p.command.contains("Extract three trees"), "{}", p.command);
    assert_eq!(
        p.description,
        "Extract three trees to scratch, format them identically, and diff for real content changes"
    );
    assert_eq!(p.notes.len(), 1, "{:?}", p.notes);
    assert!(
        p.notes[0].starts_with("Dangerous rm operation on possibly-empty variable path"),
        "{}",
        p.notes[0]
    );
    assert!(
        p.notes[0].ends_with("or use a literal path)"),
        "{}",
        p.notes[0]
    );
    assert_eq!(
        p.options,
        vec![(1, "Yes".to_string()), (2, "No".to_string())]
    );
}

#[test]
fn a_last_bar_row_that_reads_as_shell_stays_in_the_command() {
    let boxed = |last: &str| {
        let mut r = rows(&[
            " Bash command",
            "",
            "   │ cd ~/ay && git log --oneline",
            "   │ -5 | sort",
            &format!("   │ {last}"),
            "",
            " Do you want to proceed?",
            " ❯ 1. Yes",
            "   2. No",
            "",
            " Esc to cancel · Tab to amend",
        ]);
        r.extend(composer("  ? for shortcuts"));
        parse_prompt(&r).expect("a prompt")
    };
    for shell in [
        "| uniq -c",
        "-20 | tail",
        "echo Done with it",
        "Two words",
        "Three words $HERE",
        "Ends the (command)",
    ] {
        let p = boxed(shell);
        assert_eq!(
            p.command,
            format!("cd ~/ay && git log --oneline -5 | sort {shell}"),
            "{shell}"
        );
        assert_eq!(p.description, "", "{shell}");
    }
    let p = boxed("Show the recent commits");
    assert_eq!(p.command, "cd ~/ay && git log --oneline -5 | sort");
    assert_eq!(p.description, "Show the recent commits");

    assert!(reads_as_prose("Extract three trees to scratch"));
    assert!(!reads_as_prose("-20"));
    assert!(!reads_as_prose("extract three trees"));
    assert!(!reads_as_prose("Extract trees"));
    assert!(!reads_as_prose("Extract $S trees now"));

    let mut one = rows(&[
        " Bash command",
        "",
        "   │ Make the thing now",
        "",
        " Do you want to proceed?",
        " ❯ 1. Yes",
        "   2. No",
        "",
        " Esc to cancel · Tab to amend",
    ]);
    one.extend(composer("  ? for shortcuts"));
    let p = parse_prompt(&one).expect("a prompt");
    assert_eq!(p.command, "Make the thing now");
    assert_eq!(p.description, "");
}

#[test]
fn a_workflow_box_is_kind_workflow_with_its_description() {
    let p = parse_prompt(&workflow_box()).expect("a prompt");
    assert_eq!(p.kind, PromptKind::Workflow);
    assert_eq!(p.command, "");
    assert_eq!(
        p.description,
        "Fan out the 12 benchmark families to 4 agents and collect the standings table."
    );
    assert_eq!(p.options[0], (1, "Yes, run it".to_string()));
    assert_eq!(p.options[1], (2, "View raw script".to_string()));
    assert_eq!(p.options[2], (3, "No".to_string()));
}

#[test]
fn the_touch_box_is_bash_whatever_its_description_says() {
    for text in [BOX_BASH_TOUCH, BOX_BASH_TOUCH_BLINK] {
        let r = screen(text);
        let p = parse_prompt_v2(&r).expect("the box");
        assert_eq!(p.kind, PromptKind::Bash);
        assert_eq!(p.title, "Bash command");
        assert_eq!(p.command, "touch x");
        assert_eq!(
            p.command_rows,
            vec!["touch x".to_string(), "Create file x".to_string()]
        );
        assert_eq!(p.description_rows, 1);
        assert!(!p.gutter);
        assert_eq!(p.readings(), vec!["touch x", "touch x Create file x"]);
        assert_eq!(p.description, "Create file x");
        assert_eq!(p.select, Select::Digits);
        assert_eq!(p.options.len(), 3, "{:?}", p.options);
        assert_eq!(p.options[0].label, "Yes");
        assert_eq!(p.options[0].role, Role::Once);
        assert!(p.options[0].focused);
        assert!(
            p.options[1]
                .label
                .starts_with("Yes, and always allow access to /private/tmp/"),
            "{}",
            p.options[1].label
        );
        assert!(p.options[1].label.ends_with("work1 from this"));
        assert_eq!(p.options[1].role, Role::Persist);
        assert!(!p.options[1].focused);
        assert_eq!(p.options[2].label, "Nooject");
        assert_eq!(p.options[2].role, Role::Other);
        assert_eq!(p.focused().map(|o| o.n), Some(Some(1)));
        let cancel = p.cancel.as_ref().expect("a cancel hint");
        assert_eq!(cancel.key, "Esc");
        assert_eq!(cancel.verb, "cancel");
        assert_eq!(cancel.effect, CancelEffect::Reject);
        assert_eq!(r[p.span.0].trim(), "Bash command");
        assert_eq!(r[p.span.1].trim(), "Esc to cancel · Tab to amend");
    }
}

#[test]
fn a_read_file_description_does_not_make_a_bash_box_a_read_box() {
    let r: Vec<String> = screen(BOX_BASH_TOUCH)
        .into_iter()
        .map(|row| match row.as_str() {
            "   touch x" => "   wc -l /etc/hosts".to_string(),
            "   Create file x" => "   Read file /etc/hosts".to_string(),
            _ => row,
        })
        .collect();
    let p = parse_prompt_v2(&r).expect("the box");
    assert_eq!(p.kind, PromptKind::Bash);
    assert_eq!(p.command, "wc -l /etc/hosts");
    assert_eq!(p.description, "Read file /etc/hosts");
    let read = parse_prompt_v2(&read_box()).expect("the read box");
    assert_eq!(read.kind, PromptKind::Read);
    assert_eq!(read.path.as_deref(), Some("~/.ssh/config"));
    assert_eq!(read.with_role(Role::Session).map(|o| o.n), Some(Some(2)));
}

#[test]
fn the_edit_box_reads_its_path_and_the_mode_switch_option() {
    let p = parse_prompt_v2(&screen(BOX_EDIT)).expect("the box");
    assert_eq!(p.kind, PromptKind::Edit);
    assert_eq!(p.title, "Edit file");
    assert_eq!(p.command, "notes.txt");
    assert_eq!(p.path.as_deref(), Some("notes.txt"));
    assert!(p.command_rows.is_empty());
    let roles: Vec<Role> = p.options.iter().map(|o| o.role).collect();
    assert_eq!(roles, vec![Role::Once, Role::ModeSwitch, Role::Deny]);
    assert_eq!(p.cancel.map(|c| c.effect), Some(CancelEffect::Reject));
}

#[test]
fn the_auto_deny_countdown_is_not_a_note() {
    let r = screen(BOX_RM_AUTO_DENY);
    let p = parse_prompt_v2(&r).expect("the box");
    assert_eq!(p.kind, PromptKind::Bash);
    assert_eq!(p.notes.len(), 1, "{:?}", p.notes);
    assert!(
        p.notes[0].starts_with("Dangerous rm operation on possibly-empty variable path: $S/$1")
            && p.notes[0].ends_with("\"${S:?}\" or use a literal path)"),
        "{}",
        p.notes[0]
    );
    assert_eq!(
        p.auto_deny.as_deref(),
        Some(
            "Claude Code will automatically deny this request in 1:59, to avoid blocking \
             progress on an unattended session"
        )
    );
    assert_eq!(
        p.command_rows,
        [
            "S=/private/tmp/claude-502/scratch/e2e/work/tmp;",
            "for p in a b; do set -- $p; rm -rf $S/$1; done",
        ]
    );
    assert_eq!(parse_prompt(&r).expect("v1").notes.len(), 2);
    assert_eq!(
        p.options.iter().map(|o| o.role).collect::<Vec<_>>(),
        vec![Role::Once, Role::Deny]
    );

    assert_eq!(
        parse_prompt_v2(&screen(BOX_RM)).expect("box").auto_deny,
        None
    );
    let at = r
        .iter()
        .position(|row| row.contains("automatically deny"))
        .expect("the row");
    let mut spaced = r.clone();
    spaced.insert(at, String::new());
    let p = parse_prompt_v2(&spaced).expect("the box");
    assert_eq!(p.notes.len(), 1, "{:?}", p.notes);
    assert!(p.auto_deny.is_some());
    let mut other = r.clone();
    other[at] = " ⚠ Something else the vendor warns about".to_string();
    let p = parse_prompt_v2(&other).expect("the box");
    assert_eq!(p.auto_deny, None);
    assert!(
        p.notes[0].ends_with("⚠ Something else the vendor warns about"),
        "{:?}",
        p.notes
    );
}

#[test]
fn the_auto_deny_countdown_says_the_time_left() {
    use std::time::Duration;
    let left = |rows: &[String]| parse_prompt_v2(rows).expect("the box").auto_deny_left();
    let r = screen(BOX_RM_AUTO_DENY);
    assert_eq!(left(&r), Some(Duration::from_secs(119)));
    let measured = screen(BOX_RM_AUTO_DENY_2_1_301);
    let p = parse_prompt_v2(&measured).expect("the 2.1.301 box");
    assert_eq!(p.kind, PromptKind::Bash);
    assert_eq!(p.notes.len(), 1, "{:?}", p.notes);
    assert_eq!(p.auto_deny_left(), Some(Duration::from_secs(110)));
    let at = r
        .iter()
        .position(|row| row.contains("automatically deny"))
        .expect("the countdown");
    let ticked = |time: &str| {
        let mut t = r.clone();
        t[at] = t[at].replace("in 1:59,", &format!("in {time},"));
        t
    };
    for (time, secs) in [("1:44", 104), ("0:05", 5), ("0:00", 0), ("75:00", 4500)] {
        assert_eq!(
            left(&ticked(time)),
            Some(Duration::from_secs(secs)),
            "{time}"
        );
    }
    let mut wrapped = r.clone();
    wrapped[at] =
        " ⚠ Claude Code will automatically deny this request in 1:59, to avoid".to_string();
    wrapped.insert(
        at + 1,
        " blocking progress on an unattended session".to_string(),
    );
    assert_eq!(left(&wrapped), Some(Duration::from_secs(119)));

    assert_eq!(left(&screen(BOX_RM)), None, "no countdown");
    for time in [
        "about 2 minutes",
        "about 45 seconds",
        "1:5",
        "1:75",
        ":59",
        "soon",
    ] {
        assert!(
            parse_prompt_v2(&ticked(time))
                .expect("the box")
                .auto_deny
                .is_some(),
            "{time}: still the countdown row"
        );
        assert_eq!(left(&ticked(time)), None, "{time}");
    }
    let mut bare = r.clone();
    bare[at] = " ⚠ Claude Code will automatically deny this request in".to_string();
    assert_eq!(left(&bare), None, "no time at all");
}

/// 2.1.316 words the countdown `block this request in …, to avoid holding up an unattended
/// session`: its measured box still says its time left, its rm note is one note apart from
/// it, and a wrapped one is still the countdown, wrapped after its head or through its words.
/// The note above it is the box's foot note, read too with the box's head off the screen.
#[test]
fn the_countdown_worded_block_says_the_time_left() {
    use std::time::Duration;
    let r = screen(BOX_RM_AUTO_BLOCK_2_1_316);
    let p = parse_prompt_v2(&r).expect("the 2.1.316 box");
    assert_eq!(p.kind, PromptKind::Bash);
    assert_eq!(p.auto_deny_left(), Some(Duration::from_secs(119)));
    assert_eq!(p.notes.len(), 1, "{:?}", p.notes);
    assert!(p.rm_breaker().is_some(), "{:?}", p.notes);
    let note = "Dangerous rm operation on possibly-empty variable path: $S/$1 in `rm -rf \
                $S/$1` (bind $1 and rewrite its $S as \"${S:?}\" or use a literal path)";
    assert_eq!(p.notes, [note]);
    assert_eq!(p.foot_note(&r).as_deref(), Some(note));
    let whole = "Claude Code will automatically block this request in 1:59, to avoid holding \
                 up an unattended session";
    assert_eq!(p.auto_deny.as_deref(), Some(whole));
    let at = r
        .iter()
        .position(|row| row.contains("automatically block"))
        .expect("the countdown");
    for rows in [
        &[
            " ⚠ Claude Code will automatically block this request in 1:59, to avoid",
            " holding up an unattended session",
        ][..],
        &[
            " ⚠ Claude Code will automatically block this",
            " request in 1:59, to avoid holding up an",
            " unattended session",
        ][..],
        &[
            " ⚠ Claude Code will automatically block this request",
            " in 1:59, to avoid holding up an unattended",
            " session",
        ][..],
    ] {
        let mut wrapped = r.clone();
        wrapped.splice(at..=at, rows.iter().map(|s| (*s).to_string()));
        let p = parse_prompt_v2(&wrapped).expect("the wrapped box");
        assert_eq!(p.auto_deny.as_deref(), Some(whole), "{rows:?}");
        assert_eq!(
            p.auto_deny_left(),
            Some(Duration::from_secs(119)),
            "{rows:?}"
        );
        assert_eq!(p.notes, [note], "{rows:?}");
        assert!(p.rm_breaker().is_some(), "{rows:?}: {p:?}");
        assert_eq!(p.foot_note(&wrapped).as_deref(), Some(note), "{rows:?}");
    }

    let first = r
        .iter()
        .position(|row| row.starts_with(" │ S="))
        .expect("the command's first row");
    let cut = r[first..].to_vec();
    let p = parse_prompt_v2(&cut).expect("the box, its head cut");
    assert!(p.head_off_screen, "{p:?}");
    assert_eq!(p.foot_note(&cut).as_deref(), Some(note));
}

#[test]
fn a_countdown_the_model_wrote_is_no_countdown() {
    let fake = "Claude Code will automatically deny this request in 0:14, to avoid blocking \
                progress on an unattended session";
    let read = |rows: &[String]| parse_prompt_v2(rows).expect("the box");
    let without = |rows: &[String], at: &str| -> Vec<String> {
        let real = format!("⚠ Claude Code will automatically deny this request in {at},");
        rows.iter()
            .filter(|r| !r.trim_start().starts_with(&real))
            .cloned()
            .collect()
    };
    let swap = |rows: &[String], from: &str, to: &str| -> Vec<String> {
        assert_eq!(
            rows.iter().filter(|r| r.as_str() == from).count(),
            1,
            "{from}"
        );
        rows.iter()
            .map(|r| if r == from { to.to_string() } else { r.clone() })
            .collect()
    };

    let old = screen(BOX_RM_AUTO_DENY);
    assert!(read(&old).auto_deny.is_some_and(|t| t.contains("in 1:59,")));
    let old = swap(
        &old,
        "   Delete temporary directories",
        &format!("   ⚠ {fake}"),
    );
    let p = read(&old);
    assert!(
        p.auto_deny
            .as_deref()
            .is_some_and(|t| t.contains("in 1:59,")),
        "{p:?}"
    );
    assert_eq!(
        p.auto_deny_left(),
        Some(std::time::Duration::from_secs(119))
    );
    let p = read(&without(&old, "1:59"));
    assert_eq!(p.auto_deny, None, "{p:?}");
    assert_eq!(p.description, format!("⚠ {fake}"));

    let new = screen(BOX_RM_AUTO_DENY_2_1_301);
    assert!(read(&new).auto_deny.is_some_and(|t| t.contains("in 1:50,")));
    let described = swap(&new, " Delete temporary directories", &format!(" ⚠ {fake}"));
    let p = read(&described);
    assert!(
        p.auto_deny
            .as_deref()
            .is_some_and(|t| t.contains("in 1:50,")),
        "{p:?}"
    );
    let p = read(&without(&described, "1:50"));
    assert_eq!(p.auto_deny, None, "{p:?}");
    assert_eq!(p.description, format!("⚠ {fake}"));
    let command = new
        .iter()
        .find(|r| r.starts_with(" │ S="))
        .expect("the command row")
        .clone();
    let p = read(&without(
        &swap(&new, &command, &format!(" │ ⚠ {fake}")),
        "1:50",
    ));
    assert_eq!(p.auto_deny, None, "{p:?}");
    assert_eq!(p.command, format!("⚠ {fake}"));
    let bare = new
        .iter()
        .map(|r| r.replace("⚠ Claude Code will", "Claude Code will"))
        .collect::<Vec<_>>();
    assert_eq!(read(&bare).auto_deny, None, "no ⚠");

    let q = screen(QUESTION_ONE);
    let asked = swap(
        &q,
        "Which color scheme should the demo page use?",
        &format!("⚠ {fake}"),
    );
    let p = read(&asked);
    assert_eq!(p.kind, PromptKind::Question);
    assert_eq!(p.auto_deny, None, "{p:?}");

    let blocked = "Claude Code will automatically block this request in 0:14, to avoid \
                   holding up an unattended session";
    let real = "⚠ Claude Code will automatically block this request in 1:59,";
    let unblocked = |rows: &[String]| -> Vec<String> {
        rows.iter()
            .filter(|r| !r.trim_start().starts_with(real))
            .cloned()
            .collect()
    };
    let later = screen(BOX_RM_AUTO_BLOCK_2_1_316);
    let p = read(&later);
    assert_eq!(
        p.auto_deny_left(),
        Some(std::time::Duration::from_secs(119))
    );
    let notes = p.notes;
    let described = swap(
        &later,
        " Delete temporary directories",
        &format!(" ⚠ {blocked}"),
    );
    let p = read(&described);
    assert_eq!(
        p.auto_deny_left(),
        Some(std::time::Duration::from_secs(119)),
        "{p:?}"
    );
    assert_eq!(p.notes, notes, "{p:?}");
    let p = read(&unblocked(&described));
    assert_eq!(p.auto_deny, None, "{p:?}");
    assert_eq!(p.description, format!("⚠ {blocked}"));
    assert_eq!(p.notes, notes, "{p:?}");
    let command = later
        .iter()
        .find(|r| r.starts_with(" │ S="))
        .expect("the command row")
        .clone();
    let p = read(&unblocked(&swap(
        &later,
        &command,
        &format!(" │ ⚠ {blocked}"),
    )));
    assert_eq!(p.auto_deny, None, "{p:?}");
    assert_eq!(p.command, format!("⚠ {blocked}"));
    let noted = later
        .iter()
        .find(|r| r.starts_with(" │ path)"))
        .expect("the note's last row")
        .clone();
    let p = read(&unblocked(&swap(
        &later,
        &noted,
        &format!(" │ path) ⚠ {blocked}"),
    )));
    assert_eq!(p.auto_deny, None, "{p:?}");
    assert!(
        p.notes.len() == 1 && p.notes[0].ends_with(&format!("path) ⚠ {blocked}")),
        "the note keeps the forged words: {:?}",
        p.notes
    );
    let asked = swap(
        &q,
        "Which color scheme should the demo page use?",
        &format!("⚠ {blocked}"),
    );
    let p = read(&asked);
    assert_eq!(p.kind, PromptKind::Question);
    assert_eq!(p.auto_deny, None, "{p:?}");
}

#[test]
fn a_wrapped_auto_deny_countdown_is_one_countdown_not_a_note() {
    let r = screen(BOX_RM_AUTO_DENY);
    let at = r
        .iter()
        .position(|row| row.contains("automatically deny"))
        .expect("the row");
    let whole = "Claude Code will automatically deny this request in 1:59, to avoid blocking \
                 progress on an unattended session";
    for (head, tail) in [
        (
            " ⚠ Claude Code will automatically deny this request in 1:59, to avoid blocking progress on an",
            " unattended session",
        ),
        (
            " ⚠ Claude Code will automatically deny this request in 1:59, to avoid blocking",
            " progress on an unattended session",
        ),
    ] {
        let mut wrapped = r.clone();
        wrapped[at] = head.to_string();
        wrapped.insert(at + 1, tail.to_string());
        let p = parse_prompt_v2(&wrapped).expect("the box");
        assert_eq!(p.notes.len(), 1, "{:?}", p.notes);
        assert_eq!(p.auto_deny.as_deref(), Some(whole));
        assert_eq!(
            p.rm_breaker().map(|b| b.kind),
            Some(RmBreakerKind::EmptyVariable {
                in_substitution: false
            })
        );
        let mut noted = wrapped.clone();
        noted.insert(at + 2, String::new());
        noted.insert(at + 3, " This command requires approval".to_string());
        let p = parse_prompt_v2(&noted).expect("the box");
        assert_eq!(p.auto_deny, None);
        assert_eq!(p.notes.len(), 2, "{:?}", p.notes);
        assert!(p.notes[0].ends_with(whole), "{:?}", p.notes);
        assert_eq!(p.notes[1], "This command requires approval");
        assert_eq!(p.rm_breaker(), None, "two notes: no breaker's alone");
        let mut barred = wrapped.clone();
        barred.insert(
            at + 2,
            " │ Dangerous rm operation on critical path: /".to_string(),
        );
        let p = parse_prompt_v2(&barred).expect("the box");
        assert_eq!(p.auto_deny, None);
        assert!(
            p.notes.iter().any(|n| n.contains(whole)),
            "a note's rows: {:?}",
            p.notes
        );
    }
}

/// A countdown wrapped across multiple rows is still recognized as the box countdown.
#[test]
fn a_countdown_wrapped_through_its_words_is_the_countdown() {
    let r = screen(BOX_RM_AUTO_DENY_2_1_301);
    let at = r
        .iter()
        .position(|row| row.contains("automatically deny"))
        .expect("the countdown");
    let whole = "Claude Code will automatically deny this request in 1:50, to avoid blocking \
                 progress on an unattended session";
    let wrapped = |rows: &[&str]| {
        let mut t = r.clone();
        t.splice(at..=at, rows.iter().map(|s| (*s).to_string()));
        t
    };
    let note = parse_prompt_v2(&r).expect("the box").notes;
    for rows in [
        &[
            " ⚠ Claude Code will automatically deny this",
            " request in 1:50, to avoid blocking progress on",
            " an unattended session",
        ][..],
        &[
            " ⚠ Claude Code will automatically deny this request",
            " in 1:50, to avoid blocking progress on an",
            " unattended session",
        ][..],
    ] {
        let t = wrapped(rows);
        let p = parse_prompt_v2(&t).expect("the box");
        assert_eq!(p.auto_deny.as_deref(), Some(whole), "{rows:?}");
        assert_eq!(
            p.auto_deny_left(),
            Some(std::time::Duration::from_secs(110)),
            "{rows:?}"
        );
        assert_eq!(p.notes, note, "{rows:?}");
        assert!(p.rm_breaker().is_some(), "{rows:?}: {p:?}");
        assert_eq!(p.foot_note(&t), Some(note[0].clone()), "{rows:?}");
    }

    let other = wrapped(&[
        " ⚠ Something else the vendor warns about, at",
        " some length",
    ]);
    let p = parse_prompt_v2(&other).expect("the box");
    assert_eq!(p.auto_deny, None, "{p:?}");
    assert!(
        p.notes[0].ends_with("⚠ Something else the vendor warns about, at some length"),
        "{:?}",
        p.notes
    );
    let bare = wrapped(&[
        " Claude Code will automatically deny this",
        " request in 1:50, to avoid blocking progress on",
        " an unattended session",
    ]);
    assert_eq!(parse_prompt_v2(&bare).expect("the box").auto_deny, None);
}

#[test]
fn a_boxs_foot_note_is_read_whole_over_its_countdown() {
    let r = screen(BOX_RM_AUTO_DENY);
    let whole = "Dangerous rm operation on possibly-empty variable path: $S/$1 in `rm -rf \
                 $S/$1` (bind $1 and rewrite its $S as \"${S:?}\" or use a literal path)";
    let note_of = |rows: &[String]| parse_prompt_v2(rows).expect("the box").foot_note(rows);
    assert_eq!(note_of(&r).as_deref(), Some(whole));
    let at = r
        .iter()
        .position(|row| row.contains("automatically deny"))
        .expect("the countdown");
    let mut wrapped = r.clone();
    wrapped[at] = " ⚠ Claude Code will automatically deny this request in 1:59, to avoid blocking"
        .to_string();
    wrapped.insert(at + 1, " progress on an unattended session".to_string());
    wrapped.insert(at + 2, String::new());
    assert_eq!(note_of(&wrapped).as_deref(), Some(whole));

    let first = r
        .iter()
        .position(|row| row.starts_with("   │ S="))
        .expect("the command's first row");
    let cut = r[first..].to_vec();
    let p = parse_prompt_v2(&cut).expect("the box, its head cut");
    assert!(p.head_off_screen, "{p:?}");
    assert!(p.notes.is_empty(), "{:?}", p.notes);
    assert_eq!(p.foot_note(&cut).as_deref(), Some(whole));

    let touch = screen(BOX_BASH_TOUCH);
    assert_eq!(note_of(&touch), None);
    let edit = screen(BOX_EDIT);
    assert_eq!(note_of(&edit), None);
    let mut narrower = r.clone();
    for k in 0..4 {
        narrower.insert(at + 1 + k, format!(" and on, row {k}"));
    }
    assert_eq!(note_of(&narrower).as_deref(), Some(whole));
    let mut tipped = r.clone();
    tipped.insert(at + 1, " Tip: something the vendor says".to_string());
    assert_eq!(note_of(&tipped), None);
}

#[test]
fn the_workflow_rm_unresolvable_box_reads_whole() {
    let r = screen(BOX_RM_UNRESOLVABLE_WORKFLOW);
    assert_eq!(crate::phase::worker_phase(&r), crate::phase::Phase::Prompt);
    let p = parse_prompt_v2(&r).expect("the box");
    assert_eq!(p.kind, PromptKind::Bash);
    assert_eq!(
        p.title,
        "Bash command · from the \"trust-vc-front-end-m1\" workflow"
    );
    assert!(p.gutter);
    assert_eq!(p.command_rows.len(), 6, "{:?}", p.command_rows);
    assert_eq!(
        p.command_rows[0],
        "cd /Users//user00/trust-vc-m1-notes/reader &&"
    );
    assert!(p.command_rows[5].ends_with("cat dumps/basic-$t.txt; done"));
    assert_eq!(p.description_rows, 0);
    assert_eq!(
        p.description,
        "Rerun all inputs and print VcIR for four cases"
    );
    assert_eq!(
        p.notes,
        [
            "Dangerous rm operation on statically-unresolvable target: /Users//user00/trust-vc/dumps/*"
        ]
    );
    assert_eq!(p.auto_deny, None);
    assert_eq!(
        p.rm_breaker(),
        Some(RmBreaker {
            command: RmCommand::Rm,
            kind: RmBreakerKind::Unresolvable,
            target: "/Users//user00/trust-vc/dumps/*".to_string(),
        })
    );
    let roles: Vec<(Option<u8>, Role, bool)> =
        p.options.iter().map(|o| (o.n, o.role, o.focused)).collect();
    assert_eq!(
        roles,
        vec![(Some(1), Role::Once, true), (Some(2), Role::Deny, false)]
    );
    assert_eq!(p.select, Select::Digits);
    assert_eq!(p.cancel.map(|c| c.effect), Some(CancelEffect::Reject));
}

#[test]
fn every_rm_breaker_note_form_is_the_breaker() {
    use RmBreakerKind::*;
    for (note, command, kind, target) in [
        (
            "Dangerous rm operation on possibly-empty variable path: $S/$1 in `rm -rf $S/$1` (bind $1 and rewrite its $S as \"${S:?}\" or use a literal path)",
            RmCommand::Rm,
            EmptyVariable {
                in_substitution: false,
            },
            "$S/$1 in `rm -rf $S/$1` (bind $1 and rewrite its $S as \"${S:?}\" or use a literal path)",
        ),
        (
            "Dangerous rm operation on possibly-empty variable path inside command substitution: $D/x in `rm -rf $D/x` (rewrite it as \"${D:?}/x\" or use a literal path)",
            RmCommand::Rm,
            EmptyVariable {
                in_substitution: true,
            },
            "$D/x in `rm -rf $D/x` (rewrite it as \"${D:?}/x\" or use a literal path)",
        ),
        (
            "Dangerous rm operation on possibly-empty variable path: ${…}/tmp (use a literal path: when the expansion is empty this removes /tmp)",
            RmCommand::Rm,
            EmptyVariable {
                in_substitution: false,
            },
            "${…}/tmp (use a literal path: when the expansion is empty this removes /tmp)",
        ),
        (
            "Dangerous rm operation on statically-unresolvable target: /w/dumps/*",
            RmCommand::Rm,
            Unresolvable,
            "/w/dumps/*",
        ),
        (
            "Dangerous rm operation on statically-unresolvable target: command substitution output",
            RmCommand::Rm,
            Unresolvable,
            "command substitution output",
        ),
        (
            "Dangerous rm operation on critical path: /usr",
            RmCommand::Rm,
            CriticalPath,
            "/usr",
        ),
        (
            "Dangerous rm operation on working directory or its ancestor: /w",
            RmCommand::Rm,
            WorkingDirectory,
            "/w",
        ),
        (
            "Dangerous rm operation on drive root: \\",
            RmCommand::Rm,
            DriveRoot,
            "\\",
        ),
        (
            "Dangerous rm operation on mounted drive path: /Volumes/zz-lab-none",
            RmCommand::Rm,
            MountedDrivePath,
            "/Volumes/zz-lab-none",
        ),
        (
            "Dangerous rm operation — too many command substitutions to analyze (65)",
            RmCommand::Rm,
            TooManySubstitutions,
            "65",
        ),
        (
            "Dangerous rm operation not ruled out — too many command substitutions to analyze (65)",
            RmCommand::Rm,
            TooManySubstitutions,
            "65",
        ),
        (
            "Dangerous rm operation not ruled out — some nested commands or function substitutions could not be analyzed",
            RmCommand::Rm,
            NestedUnanalyzed,
            "some nested commands or function substitutions could not be analyzed",
        ),
        (
            "Dangerous rm operation not ruled out — the shell may read this command differently than it was parsed",
            RmCommand::Rm,
            NotRuledOut,
            "the shell may read this command differently than it was parsed",
        ),
        (
            "Dangerous rmdir operation not ruled out — something new",
            RmCommand::Rmdir,
            NotRuledOut,
            "something new",
        ),
        (
            "Dangerous rmdir operation on statically-unresolvable target: /tmp/a/*",
            RmCommand::Rmdir,
            Unresolvable,
            "/tmp/a/*",
        ),
        (
            "Dangerous rmdir operation on critical path: /tmp",
            RmCommand::Rmdir,
            CriticalPath,
            "/tmp",
        ),
        (
            "This shell -c script runs rm and could not be checked",
            RmCommand::Rm,
            ShellScriptUnchecked,
            "This shell -c script runs rm and could not be checked",
        ),
        (
            "Dangerous rm operation in a shell -c script, on a target built from a value known only when it runs",
            RmCommand::Rm,
            ShellScriptValue,
            "in a shell -c script, on a target built from a value known only when it runs",
        ),
    ] {
        let b = rm_breaker_of(note).unwrap_or_else(|| panic!("{note}"));
        assert_eq!(
            (b.command, b.kind, b.target.as_str()),
            (command, kind, target),
            "{note}"
        );
    }
    for not in [
        "Dangerous rm operation detected: '/usr'",
        "Dangerous rm operation on critical path: /usr Dangerous rm operation on critical path: /etc",
        "Dangerous rm operation on something new: /x",
        "Dangerous rm operation on critical path: ",
        "Dangerous rm operation — too many command substitutions to analyze (lots)",
        "Dangerous rm operation not ruled out — too many command substitutions to analyze (lots)",
        "Dangerous rm operation not ruled out — too many command substitutions to analyze",
        "Dangerous rm operation not ruled out — some nested commands or function substitutions could not be analyzed (2)",
        "Dangerous rm operation — something new",
        "Dangerous rm operation — the shell may read this command differently than it was parsed",
        "Dangerous rm operation not ruled out — ",
        "Dangerous rm operation not ruled out —",
        "Dangerous rm operation not ruled out on critical path: /usr",
        "Dangerous rm operation on mounted drive path: ",
        "Dangerous rm operation on mounted drive: /Volumes/x",
        "Dangerous rm operation not ruled out some nested commands or function substitutions could not be analyzed",
        "This command requires approval",
        "Permission rule Bash(rm:*) requires confirmation for this command.",
        "This shell -c script runs rm and could not be checked (2)",
        "This shell -c script runs rmdir and could not be checked",
        "A permission rule denies a command in this shell -c script",
        "Dangerous rm operation in a shell -c script",
        "Dangerous rm operation in a shell -c script, on a target built from a value known only when it runs: /x",
    ] {
        assert_eq!(rm_breaker_of(not), None, "{not}");
    }
    let mut p = parse_prompt_v2(&screen(BOX_RM)).expect("the box");
    assert!(p.rm_breaker().is_some());
    p.notes.push("This command requires approval".to_string());
    assert_eq!(p.rm_breaker(), None);
}

#[test]
fn a_subagents_shell_c_rm_box_is_the_rm_breaker_and_a_deny_rule_the_owners() {
    let unchecked = "This shell -c script runs rm and could not be checked";
    let valued = "Dangerous rm operation in a shell -c script, on a target built from a value \
                  known only when it runs";
    for (row, note, kind) in [
        (
            format!(" {unchecked}"),
            unchecked,
            RmBreakerKind::ShellScriptUnchecked,
        ),
        (
            format!(" │ {unchecked}"),
            unchecked,
            RmBreakerKind::ShellScriptUnchecked,
        ),
        (
            format!(" │ {valued}"),
            valued,
            RmBreakerKind::ShellScriptValue,
        ),
    ] {
        let p = parse_prompt_v2(&shell_c_rm_box(&row)).expect("the box");
        assert_eq!(p.kind, PromptKind::Bash, "{row}");
        assert_eq!(
            p.origin(),
            Some(Origin::Subagent {
                name: Some("general-purpose".to_string())
            }),
            "{row}"
        );
        assert_eq!(p.base_title().as_deref(), Some("Bash command"), "{row}");
        assert!(p.command.starts_with("bash -c '"), "{row}: {p:?}");
        assert!(
            p.command.contains("rm -rf \"${P:?}/debug/incremental\""),
            "{row}: {p:?}"
        );
        assert_eq!(p.notes, [note], "{row}");
        let b = p.rm_breaker().unwrap_or_else(|| panic!("{row}: {p:?}"));
        assert_eq!((b.command, b.kind), (RmCommand::Rm, kind), "{row}");
        assert_eq!(p.owner_review(), None, "{row}");
        let roles: Vec<Role> = p.options.iter().map(|o| o.role).collect();
        assert_eq!(roles, [Role::Once, Role::Deny], "{row}");
    }
    let deny = "A permission rule denies a command in this shell -c script";
    let p = parse_prompt_v2(&shell_c_rm_box(&format!(" │ {deny}"))).expect("the box");
    assert_eq!(p.notes, [deny]);
    assert_eq!(p.rm_breaker(), None);
    let review = p.owner_review().expect("the owner's deny rule");
    assert_eq!(
        (review.kind, review.text.as_str()),
        (ReviewKind::DenyRule, deny)
    );
}

#[test]
fn a_bar_row_with_an_ellipsis_is_not_a_spinner_row() {
    let mut r = rows(&[
        "⏺ Removing tmp.",
        "",
        "─".repeat(120).as_str(),
        " Bash command",
        "",
        "   rm -rf \"$(git rev-parse --show-toplevel)\"/tmp",
        "   Remove tmp",
        "",
        " │ Dangerous rm operation on possibly-empty variable path: ${…}/tmp (use a literal path: when the expansion is empty this",
        " │ removes /tmp)",
        "",
        " Do you want to proceed?",
        " ❯ 1. Yes",
        "   2. No",
        "",
        " Esc to cancel · Tab to amend",
    ]);
    let p = parse_prompt_v2(&r).expect("the box");
    assert_eq!(p.kind, PromptKind::Bash);
    assert_eq!(p.title, "Bash command");
    assert_eq!(
        p.rm_breaker().map(|b| b.kind),
        Some(RmBreakerKind::EmptyVariable {
            in_substitution: false
        })
    );
    r[8] = " │ Running the whole suite…".to_string();
    r[9] = " │".to_string();
    let p = parse_prompt_v2(&r).expect("the box");
    assert_eq!(p.title, "Bash command");
    assert!(!crate::phase::is_glyph_row(" │ Running the whole suite…"));
    assert!(crate::phase::is_glyph_row("✻ Running the whole suite…"));
}

#[test]
fn the_trust_dialog_reads_its_folder_options_and_exit() {
    for (text, folder) in [
        (TRUST, "/work1"),
        (TRUST_AFTER_RESUMES, "/lv/work2"),
        (TRUST_2_1_308, "/at.lab0/proj"),
    ] {
        let r = screen(text);
        assert_eq!(crate::phase::worker_phase(&r), crate::phase::Phase::Prompt);
        let p = parse_prompt_v2(&r).expect("the dialog");
        assert_eq!(p.kind, PromptKind::Trust);
        assert_eq!(p.title, "Accessing workspace:");
        let path = p.path.clone().expect("the folder");
        assert!(
            path.starts_with("/private/tmp/") && path.ends_with(folder),
            "{path}"
        );
        assert_eq!(p.select, Select::ArrowsEnter);
        assert_eq!(p.options.len(), 2, "{:?}", p.options);
        assert_eq!(p.options[0].label, "No, exit");
        assert_eq!(p.options[0].role, Role::Exit);
        assert!(p.options[0].focused);
        assert_eq!(p.options[0].n, None);
        assert_eq!(p.options[1].label, "Yes, I trust this folder");
        assert_eq!(p.options[1].role, Role::Trust);
        assert!(!p.options[1].focused);
        assert_eq!(p.focused().map(|o| o.label.as_str()), Some("No, exit"));
        let cancel = p.cancel.expect("the footer's Esc");
        assert_eq!(cancel.effect, CancelEffect::Exit);
        assert!(!p.description.contains("I trust"), "{}", p.description);
        assert_eq!(
            prompt_box_span(&r).map(|(a, _)| r[a].trim()),
            Some("Accessing workspace:")
        );
    }
}

#[test]
fn the_lab_boxes_read_as_measured() {
    use Role::{Deny, ModeSwitch, Once, Persist};
    for (build, touch, write, edit, ask, hook) in [
        (
            "2.1.307",
            BOX_BASH_TOUCH_2_1_307,
            BOX_WRITE_CREATE_2_1_307,
            BOX_EDIT_2_1_307,
            BOX_BASH_ASK_RULE_2_1_307,
            BOX_BASH_HOOK_ASK_2_1_307,
        ),
        (
            "2.1.308",
            BOX_BASH_TOUCH_2_1_308,
            BOX_WRITE_CREATE_2_1_308,
            BOX_EDIT_2_1_308,
            BOX_BASH_ASK_RULE_2_1_308,
            BOX_BASH_HOOK_ASK_2_1_308,
        ),
        (
            "2.1.309",
            BOX_BASH_TOUCH_2_1_309,
            BOX_WRITE_CREATE_2_1_309,
            BOX_EDIT_2_1_309,
            BOX_BASH_ASK_RULE_2_1_309,
            BOX_BASH_HOOK_ASK_2_1_309,
        ),
    ] {
        for (what, text, kind, subject, description, roles, review) in [
            (
                "bash",
                touch,
                PromptKind::Bash,
                "touch x",
                "Create x",
                &[Once, Persist, ModeSwitch, Deny][..],
                None,
            ),
            (
                "write",
                write,
                PromptKind::Write,
                "w.txt",
                "",
                &[Once, ModeSwitch, Deny][..],
                None,
            ),
            (
                "edit",
                edit,
                PromptKind::Edit,
                "notes.txt",
                "",
                &[Once, ModeSwitch, Deny][..],
                None,
            ),
            (
                "ask rule",
                ask,
                PromptKind::Bash,
                "touch y",
                "Create y",
                &[Once, Deny][..],
                Some(ReviewKind::Rule),
            ),
            (
                "hook",
                hook,
                PromptKind::Bash,
                "touch z",
                "Create z",
                &[Once, Deny][..],
                Some(ReviewKind::Hook),
            ),
        ] {
            let what = format!("{build} {what}");
            let r = screen(text);
            assert_eq!(
                crate::phase::worker_phase(&r),
                crate::phase::Phase::Prompt,
                "{what}"
            );
            let p = parse_prompt_v2(&r).unwrap_or_else(|| panic!("{what}: the box"));
            assert_eq!(p.kind, kind, "{what}");
            if kind == PromptKind::Bash {
                assert_eq!(p.command, subject, "{what}");
                assert_eq!(p.description, description, "{what}");
                assert!(
                    p.readings().iter().all(|r| !r.contains("Tip:")),
                    "{what}: {:?}",
                    p.readings()
                );
            } else {
                assert_eq!(p.path.as_deref(), Some(subject), "{what}");
            }
            assert_eq!(
                p.options.iter().map(|o| o.role).collect::<Vec<_>>(),
                roles,
                "{what}: {:?}",
                p.options
            );
            assert_eq!(p.select, Select::Digits, "{what}");
            let first = p.focused().expect("the cursor");
            assert_eq!((first.n, first.label.as_str()), (Some(1), "Yes"), "{what}");
            assert_eq!(
                p.cancel.as_ref().map(|c| c.effect),
                Some(CancelEffect::Reject),
                "{what}"
            );
            assert_eq!(p.owner_review().map(|o| o.kind), review, "{what}");
            let v1 = parse_prompt(&r).unwrap_or_else(|| panic!("{what}: v1"));
            assert_eq!(v1.command, subject, "{what}");
            assert_eq!(
                v1.options.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
                (1..=u8::try_from(roles.len()).expect("a few")).collect::<Vec<_>>(),
                "{what}"
            );
            let (top, _) = prompt_box_span(&r).expect("the span");
            assert_eq!(parse_prompt_v2(&r[..top]), None, "{what}: cut above");
            assert_eq!(parse_prompt(&r[..top]), None, "{what}: cut above, v1");
        }
    }
    let nested_command = format!("X=1 if true; then echo{}; fi", " ${ echo a; }".repeat(17));
    let no_rm_command = format!("echo{}", " $(true)".repeat(65));
    for (what, text, command, kind, target) in [
        (
            "too many, rm",
            BOX_RM_TOO_MANY_2_1_307,
            None,
            RmBreakerKind::TooManySubstitutions,
            "65",
        ),
        (
            "too many, no rm",
            BOX_RM_TOO_MANY_NO_RM_2_1_307,
            Some(no_rm_command.as_str()),
            RmBreakerKind::TooManySubstitutions,
            "65",
        ),
        (
            "too many, no rm, bypass",
            BOX_RM_TOO_MANY_NO_RM_BYPASS_2_1_307,
            Some(no_rm_command.as_str()),
            RmBreakerKind::TooManySubstitutions,
            "65",
        ),
        (
            "nested, no rm",
            BOX_RM_NESTED_2_1_307,
            Some(nested_command.as_str()),
            RmBreakerKind::NestedUnanalyzed,
            "some nested commands or function substitutions could not be analyzed",
        ),
        (
            "mounted drive",
            BOX_RM_MOUNTED_DRIVE_2_1_307,
            Some("rm -rf /Volumes/zz-lab-none"),
            RmBreakerKind::MountedDrivePath,
            "/Volumes/zz-lab-none",
        ),
        (
            "2.1.308 too many, rm",
            BOX_RM_TOO_MANY_2_1_308,
            None,
            RmBreakerKind::TooManySubstitutions,
            "65",
        ),
        (
            "2.1.308 too many, no rm",
            BOX_RM_TOO_MANY_NO_RM_2_1_308,
            Some(no_rm_command.as_str()),
            RmBreakerKind::TooManySubstitutions,
            "65",
        ),
        (
            "2.1.308 mounted drive",
            BOX_RM_MOUNTED_DRIVE_2_1_308,
            Some("rm -rf /Volumes/zz-lab-none"),
            RmBreakerKind::MountedDrivePath,
            "/Volumes/zz-lab-none",
        ),
        (
            "2.1.308 too many, no rm, bypass",
            BOX_RM_TOO_MANY_NO_RM_BYPASS_2_1_308,
            Some(no_rm_command.as_str()),
            RmBreakerKind::TooManySubstitutions,
            "65",
        ),
        (
            "2.1.308 nested, no rm",
            BOX_RM_NESTED_2_1_308,
            Some(nested_command.as_str()),
            RmBreakerKind::NestedUnanalyzed,
            "some nested commands or function substitutions could not be analyzed",
        ),
        (
            "2.1.309 parse diverged, no rm",
            BOX_RM_PARSE_DIVERGED_2_1_309,
            Some("echo $("),
            RmBreakerKind::NotRuledOut,
            "the shell may read this command differently than it was parsed",
        ),
    ] {
        let r = screen(text);
        assert_eq!(
            crate::phase::worker_phase(&r),
            crate::phase::Phase::Prompt,
            "{what}"
        );
        let p = parse_prompt_v2(&r).unwrap_or_else(|| panic!("{what}: the rm box"));
        assert_eq!(p.kind, PromptKind::Bash, "{what}");
        match command {
            Some(c) => assert_eq!(p.command, c, "{what}"),
            None => assert!(p.command.ends_with("; rm -f zz-none"), "{}", p.command),
        }
        assert_eq!(
            p.options.iter().map(|o| o.role).collect::<Vec<_>>(),
            [Role::Once, Role::Deny],
            "{what}"
        );
        let b = p
            .rm_breaker()
            .unwrap_or_else(|| panic!("{what}: {:?}", p.notes));
        assert_eq!(
            (b.command, b.kind, b.target.as_str()),
            (RmCommand::Rm, kind, target),
            "{what}"
        );
        let (top, _) = prompt_box_span(&r).expect("the span");
        assert_eq!(parse_prompt_v2(&r[..top]), None, "{what}: cut above");
    }
    for text in [
        BOX_BASH_TOUCH_2_1_307,
        BOX_BASH_TOUCH_2_1_308,
        BOX_BASH_TOUCH_2_1_309,
        BOX_BASH_ASK_RULE_2_1_309,
        BOX_BASH_HOOK_ASK_2_1_309,
    ] {
        let plain = parse_prompt_v2(&screen(text)).expect("the box");
        assert_eq!(plain.rm_breaker(), None);
    }
    for text in [
        BOX_STATEMENT_FOLLOWS_16_2_1_307,
        BOX_STATEMENT_FOLLOWS_16_2_1_308,
    ] {
        let within = parse_prompt_v2(&screen(text)).expect("the box");
        assert_eq!(
            within.command,
            format!("X=1 if true; then echo{}; fi", " ${ echo a; }".repeat(16))
        );
        assert_eq!(within.notes.len(), 1, "{:?}", within.notes);
        assert!(
            within.notes[0].starts_with("statement directly follows another statement"),
            "{:?}",
            within.notes
        );
        assert_eq!(within.rm_breaker(), None);
    }
    for text in [PLAN_READY_2_1_307, PLAN_READY_2_1_308] {
        let r = screen(text);
        let p = parse_prompt_v2(&r).expect("the plan box");
        assert_eq!(p.kind, PromptKind::PlanExit);
        assert_eq!(
            p.options
                .iter()
                .map(|o| o.label.as_str())
                .collect::<Vec<_>>(),
            [
                "Yes, and use auto mode",
                "Yes, manually approve edits",
                "Tell Claude what to change"
            ]
        );
        assert_eq!(p.focused().map(|o| o.n), Some(Some(1)));
        let (top, _) = prompt_box_span(&r).expect("the span");
        assert_eq!(parse_prompt_v2(&r[..top]), None, "plan: cut above");
    }
}

#[test]
fn the_limit_options_dialog_reads_its_wait_row_by_label() {
    const STOP: &str = "Stop and wait for limit to reset";
    const CREDITS: &str = "Switch to usage credits";
    for (text, wait) in [
        (
            LIMIT_OPTIONS_DIALOG,
            "Wait here, then continue automatically at 12:23pm",
        ),
        (
            LIMIT_OPTIONS_WEEKLY,
            "Wait here, then continue automatically at Oct 10 at 2:19pm",
        ),
        (
            LIMIT_OPTIONS_26_2_1_300,
            "Wait here, then continue automatically at 7:16am",
        ),
        (
            LIMIT_OPTIONS_WEEKLY_26_2_1_300,
            "Wait here, then continue automatically at Oct 16 at 9:04am",
        ),
    ] {
        let r = screen(text);
        assert_eq!(crate::phase::worker_phase(&r), crate::phase::Phase::Prompt);
        let p = parse_prompt_v2(&r).expect("the dialog");
        assert_eq!((p.kind, p.head_off_screen), (PromptKind::UsageLimit, false));
        let read: Vec<(&str, Role)> = p
            .options
            .iter()
            .map(|o| (o.label.as_str(), o.role))
            .collect();
        assert_eq!(
            read,
            [
                (STOP, Role::Other),
                (wait, Role::AutoResume),
                ("Upgrade your plan", Role::Other)
            ]
        );
        assert_eq!(p.focused().map(|o| o.label.as_str()), Some(STOP));
        assert_eq!(p.command, wait);
        let small: Vec<String> = r.iter().map(|x| x.replace("  ❯ 1. ", "  › 1. ")).collect();
        assert_eq!(parse_prompt_v2(&small), Some(p.clone()));
    }
    let r = screen(LIMIT_OPTIONS_DIALOG);
    let title = r
        .iter()
        .position(|x| x == "  What do you want to do?")
        .expect("the title");
    let mut left = r.clone();
    left.insert(title + 1, " a row the Pane does not draw".to_string());
    let mut composed = r;
    composed.extend(composer("  ? for shortcuts"));
    for rows in [left, composed] {
        assert!(
            parse_prompt_v2(&rows).is_none_or(|p| p.kind != PromptKind::UsageLimit),
            "{rows:#?}"
        );
    }

    let d = limit_options_dialog(&["Keep the branch", "Drop it"], 0);
    assert_eq!(parse_prompt_v2(&d).map(|p| p.kind), Some(PromptKind::Other));
    for labels in [
        &["Upgrade to Max", "Add funds to continue with usage credits"][..],
        &["Upgrade your plan", CREDITS],
        &["Stop", "Ask your admin for more usage"],
    ] {
        let d = limit_options_dialog(labels, 0);
        let p = parse_prompt_v2(&d).expect("a box");
        assert_eq!(p.kind, PromptKind::Other, "{labels:?}");
        assert!(p.options.iter().all(|o| o.role != Role::AutoResume));
    }
    let mut d = limit_options_dialog(&[STOP, "Wait here, then continue automatically shortly"], 0);
    let title = d
        .iter()
        .position(|x| x.trim() == "What do you want to do?")
        .expect("the title");
    d[title] = " What would you like?".to_string();
    let p = parse_prompt_v2(&d).expect("a box");
    assert_eq!(p.kind, PromptKind::Other);
    assert!(p.options.iter().all(|o| o.role != Role::AutoResume));
    assert!(!is_wait_label("Don't continue automatically"));
    assert!(!is_wait_label("Wait here, then continue automatically"));
    assert!(!is_wait_label("Wait here, then continue automatically at "));
    assert!(!is_wait_label(
        "Wait here, then continue automatically soon"
    ));
}

#[test]
fn the_limit_dialog_in_the_fullscreen_panel_is_read_only_whole() {
    let r = screen(LIMIT_OPTIONS_PANEL);
    assert_eq!(
        parse_prompt_v2(&r).map(|p| p.kind),
        Some(PromptKind::UsageLimit)
    );
    let mut left = r.clone();
    left.insert(4, "  a row the panel does not draw".to_string());
    assert!(
        parse_prompt_v2(&left).is_none_or(|p| p.kind != PromptKind::UsageLimit),
        "{left:#?}"
    );
    let mut composed = r;
    composed.extend(
        [
            "─".repeat(131),
            "❯ ".to_string(),
            "─".repeat(131),
            "  ⏵⏵ auto mode on (shift+tab to cycle)".to_string(),
        ]
        .iter()
        .cloned(),
    );
    assert!(
        parse_prompt_v2(&composed).is_none_or(|p| p.kind != PromptKind::UsageLimit),
        "{composed:#?}"
    );
}

#[test]
fn a_box_whose_footer_wraps_is_read_whole() {
    let last = |r: &[String]| r.iter().rposition(|x| !x.trim().is_empty()).expect("a row");
    let r = screen(LIMIT_OPTIONS_34);
    assert_eq!(crate::phase::worker_phase(&r), crate::phase::Phase::Prompt);
    let p = parse_prompt_v2(&r).expect("the dialog");
    assert_eq!((p.kind, p.head_off_screen), (PromptKind::UsageLimit, false));
    let read: Vec<(&str, Role)> = p
        .options
        .iter()
        .map(|o| (o.label.as_str(), o.role))
        .collect();
    assert_eq!(
        read,
        [
            ("Stop and wait for limit to reset", Role::Other),
            (
                "Wait here, then continue automatically at 12:23pm",
                Role::AutoResume
            ),
            ("Upgrade your plan", Role::Other)
        ]
    );
    assert_eq!(p.span.1, last(&r));
    assert_eq!(
        p.footer.as_deref(),
        Some("Enter to confirm · Esc to cancel")
    );
    for (width, footer) in [
        (
            28,
            [
                "Enter to select · Tab/Arrow",
                "keys to navigate · ctrl+g to",
                "edit in VS Code · Esc to",
                "cancel",
            ],
        ),
        (11, [" Esc to", " cancel ·", " Tab to", " amend"]),
    ] {
        let rule = "─".repeat(width);
        let mut r = rows(&[&rule, ""]);
        r.extend(rows(&footer));
        assert_eq!(last_footer(&r), Some((2, 5)), "{footer:?}");
    }
}

#[test]
fn a_title_ink_wrapped_names_its_box() {
    let wait = "Wait here, then continue automatically at 12:23pm";
    let r = screen(LIMIT_OPTIONS_26);
    assert_eq!(crate::phase::worker_phase(&r), crate::phase::Phase::Prompt);
    let p = parse_prompt_v2(&r).expect("the 26-column dialog");
    assert_eq!(
        (p.kind, p.title.as_str()),
        (PromptKind::UsageLimit, crate::anchor!("limit.title"))
    );
    let read: Vec<(&str, Role)> = p
        .options
        .iter()
        .map(|o| (o.label.as_str(), o.role))
        .collect();
    assert_eq!(
        read,
        [
            ("Stop and wait for limit to reset", Role::Other),
            (wait, Role::AutoResume),
            ("Upgrade your plan", Role::Other)
        ]
    );
    assert_eq!(p.command, wait);

    let p = parse_prompt_v2(&screen(BOX_SKILL_44_2_1_300)).expect("the skill box");
    assert_eq!(
        (p.kind, p.title.as_str(), p.command.as_str()),
        (
            PromptKind::Skill,
            "Use skill \"anthropic-skills:built-in-browser\"?",
            "anthropic-skills:built-in-browser"
        )
    );
    let roles: Vec<Role> = p.options.iter().map(|o| o.role).collect();
    assert_eq!(roles, [Role::Once, Role::Persist, Role::Deny]);

    for (fixture, width) in [(HELD_MESSAGE_34_2_1_300, 34), (HELD_MESSAGE_50_2_1_300, 50)] {
        let p = parse_prompt_v2(&screen(fixture)).expect("the held message");
        assert_eq!(
            (p.kind, p.title.as_str()),
            (PromptKind::HeldMessage, crate::anchor!("held.title")),
            "{width}"
        );
        let options: Vec<(&str, Role, bool)> = p
            .options
            .iter()
            .map(|o| (o.label.as_str(), o.role, o.focused))
            .collect();
        assert_eq!(
            options,
            [
                (
                    "Deny — drop it and tell the sender it was declined",
                    Role::Deny,
                    true
                ),
                ("Deliver this message to Claude", Role::Other, false)
            ],
            "{width}"
        );
    }

    let rule = "─".repeat(24);
    let dashes = format!("  {}", "╌".repeat(22));
    let goal = rows(&[
        "⏺ Tool call done.",
        "",
        &rule,
        "  Claude proposes a",
        "  goal",
        &dashes,
        "   All tests in",
        "   test/auth pass",
        &dashes,
        "",
        "  Approving sets this",
        "  as the session goal.",
        "",
        "  ❯ Not now",
        "    Set this goal",
    ]);
    let p = parse_prompt_v2(&goal).expect("the goal");
    assert_eq!(
        (p.kind, p.title.as_str()),
        (PromptKind::GoalProposal, "Claude proposes a goal")
    );

    let p = parse_prompt_v2(&screen(BOX_BASH_26_2_1_295)).expect("the box");
    assert_eq!(
        (p.kind, p.title.as_str()),
        (PromptKind::Bash, "Bash command")
    );
    assert_eq!(p.options.len(), 4);
    let rule = "─".repeat(16);
    let dashes = "╌".repeat(16);
    let bash16 = rows(&[
        &rule,
        " Bash command",
        " Tip: auto mode",
        " handles these",
        " Create x",
        &dashes,
        " touch x",
        &dashes,
        " Do you want to",
        " proceed?",
        " ❯ 1. Yes",
        "   2. No",
        "",
        " Esc to cancel ·",
        " Tab to amend",
    ]);
    assert!(crate::wrap::continues(
        "Bash command",
        1,
        " Tip: auto mode",
        crate::wrap::room(1, 16)
    ));
    let p = parse_prompt_v2(&bash16).expect("the 16-column box");
    assert_eq!(
        (p.kind, p.title.as_str(), p.command.as_str()),
        (PromptKind::Bash, "Bash command", "touch x")
    );
    let rule = "─".repeat(40);
    let setup = rows(&[
        "⏺ Done.",
        "",
        &rule,
        " Remote Control is available for this",
        " session from claude.ai",
        "",
        " ❯ Enable Remote Control",
        "   Not now",
    ]);
    let p = parse_prompt_v2(&setup).expect("the setup dialog");
    assert_eq!(
        (p.kind, p.title.as_str()),
        (PromptKind::Other, "Remote Control is available for this")
    );
    for fixture in [TRUST, TRUST_BACKSTOP, HELD_MESSAGE_2_1_287] {
        let p = parse_prompt_v2(&screen(fixture)).expect("the box");
        assert_eq!(p.options.len(), 2, "{:?}", p.options);
    }
    let p = parse_prompt_v2(&screen(HELD_MESSAGE_2_1_287)).expect("the box");
    assert_eq!(p.options[1].label, "Deliver this message to Claude");
    for (fixture, widths, labels) in [
        (
            HELD_MESSAGE_2_1_287,
            54..=61,
            [
                "Deny — drop it and tell the sender it was declined",
                "Deliver this message to Claude",
            ],
        ),
        (
            TRUST_BACKSTOP,
            42..=46,
            [
                "No, continue without these permissions",
                "Yes, I trust this folder",
            ],
        ),
    ] {
        for width in widths {
            let r = rewrap_box(fixture, width);
            let whole = format!(" ❯ {}", labels[0]);
            let at = r.iter().position(|row| *row == whole).expect("whole");
            assert!(crate::wrap::continues_exactly(
                labels[0],
                0,
                3,
                &r[at + 1],
                width - 4
            ));
            let p = parse_prompt_v2(&r).expect("the box");
            let read: Vec<&str> = p.options.iter().map(|o| o.label.as_str()).collect();
            assert_eq!(read, labels, "{width}");
        }
    }
    assert_eq!(
        rewrap_box(HELD_MESSAGE_2_1_287, 34),
        screen(HELD_MESSAGE_34_2_1_300)
    );
    assert_eq!(
        rewrap_box(HELD_MESSAGE_2_1_287, 50),
        screen(HELD_MESSAGE_50_2_1_300)
    );
}

fn dialog_rows(fixture: &str) -> String {
    let r = screen(fixture);
    let top = r
        .iter()
        .rposition(|x| crate::phase::is_rule(x) && x.chars().count() > 40)
        .expect("the dialog's rule");
    r[top..].join("\n")
}

#[test]
fn a_label_ink_wrapped_at_a_name_or_inside_a_word_reads_whole() {
    type Read = (PromptKind, Vec<(String, Role, bool)>);
    let read = |r: &[String]| -> Read {
        let p = parse_prompt_v2(r).expect("the dialog");
        let o = p.options.iter();
        (
            p.kind,
            o.map(|o| (o.label.clone(), o.role, o.focused)).collect(),
        )
    };
    for (name, fixture, kind) in [
        (
            "held message",
            HELD_MESSAGE_2_1_287,
            PromptKind::HeldMessage,
        ),
        (
            "held message 2.1.282",
            HELD_MESSAGE,
            PromptKind::HeldMessage,
        ),
        ("computer use", COMPUTER_USE, PromptKind::ComputerUse),
        ("limit", LIMIT_OPTIONS_DIALOG, PromptKind::UsageLimit),
        ("weekly limit", LIMIT_OPTIONS_WEEKLY, PromptKind::UsageLimit),
    ] {
        let body = dialog_rows(fixture);
        let whole = read(&screen(&body));
        assert_eq!(whole.0, kind, "{name}");
        for width in 16..=120 {
            let r = rewrap_box(&body, width);
            assert_eq!(read(&r), whole, "{name} at {width}: {r:#?}");
        }
    }
    let r = rewrap_box(&dialog_rows(LIMIT_OPTIONS_DIALOG), 20);
    assert!(r.contains(&"    3. Upgrade your".to_string()), "{r:#?}");
    let p = parse_prompt_v2(&r).expect("the dialog");
    assert_eq!(p.options[2].label, "Upgrade your plan");
}

#[test]
fn a_plan_dialog_whose_hint_wraps_is_read() {
    let last = |r: &[String]| r.iter().rposition(|x| !x.trim().is_empty()).expect("a row");
    for text in [
        PLAN_READY_70_2_1_300,
        PLAN_READY_50_2_1_300,
        PLAN_READY_26_2_1_300,
    ] {
        let r = screen(text);
        assert_eq!(
            crate::phase::worker_phase(&r),
            crate::phase::Phase::Prompt,
            "{r:#?}"
        );
        let p = parse_prompt_v2(&r).expect("the dialog");
        assert_eq!(
            (p.kind, p.head_off_screen, p.title.as_str()),
            (PromptKind::PlanExit, false, "Ready to code?")
        );
        let read: Vec<(Option<u8>, &str, Role)> = p
            .options
            .iter()
            .map(|o| (o.n, o.label.as_str(), o.role))
            .collect();
        assert_eq!(
            read,
            [
                (Some(1), "Yes, and use auto mode", Role::ModeSwitch),
                (Some(2), "Yes, manually approve edits", Role::ModeSwitch),
                (Some(3), "Tell Claude what to change", Role::Other)
            ]
        );
        assert!(p.options[0].focused);
        assert_eq!(p.span.1, last(&r));
    }
}

#[test]
fn a_title_whole_on_its_row_takes_no_row_under_it() {
    let boxed = |w: usize, body: &[&str]| -> Vec<String> {
        let mut r = rows(&["❯ toolstart", "", "⏺ Working on it.", ""]);
        r.push("─".repeat(w));
        r.extend(body.iter().map(|b| {
            if *b == "╌" {
                "╌".repeat(w)
            } else {
                (*b).to_string()
            }
        }));
        r
    };
    let r = boxed(
        26,
        &[
            " Bash command",
            " Regenerate the lockfile",
            "╌",
            " touch w-forged",
            "╌",
            " Do you want to proceed?",
            " ❯ 1. Yes",
            "   2. No",
            "",
            " Esc to cancel · Tab to",
            " amend",
        ],
    );
    let p = parse_prompt_v2(&r).expect("the box");
    assert_eq!(p.title, "Bash command", "{r:#?}");
    assert_eq!(p.description, "Regenerate the lockfile");
    let long = |n: usize| {
        let stem = "/private/tmp/at.v4LY/proj/";
        let file = "notes.txt";
        format!(
            "{stem}{}{file}",
            "d/".repeat((n - stem.len() - file.len()) / 2)
        )
    };
    for (w, title, path, kind) in [
        (80, " Edit file", long(68), PromptKind::Edit),
        (50, " Edit file", long(38), PromptKind::Edit),
        (
            26,
            " Create file",
            "lab-write-d.txt".to_string(),
            PromptKind::Write,
        ),
    ] {
        let path_row = format!(" {path}");
        let r = boxed(
            w,
            &[
                title,
                &path_row,
                "╌",
                "  1 hello",
                "╌",
                " Do you want to proceed?",
                " ❯ 1. Yes",
                "   2. No",
                "",
                " Esc to cancel",
            ],
        );
        let p = parse_prompt_v2(&r).expect("the box");
        assert_eq!(
            (p.kind, p.title.as_str(), p.path.as_deref()),
            (kind, title.trim(), Some(path.as_str())),
            "{r:#?}"
        );
    }
}

#[test]
fn words_that_quote_the_footer_are_not_a_box() {
    let said = {
        let mut r = rows(&[
            "⏺ The watcher is armed. When the box appears, press Esc to cancel it or 1 to approve.",
            "",
        ]);
        r.extend(composer("  ? for shortcuts"));
        r
    };
    let bullet = {
        let mut r = rows(&[
            "⏺ Two ways out:",
            "  - 1 to approve",
            "  - Esc to cancel the run",
            "",
        ]);
        r.extend(composer("  ? for shortcuts"));
        r
    };
    let zsh = rows(&[
        "% grep -rn 'Esc to cancel' crates/aterm-phase/src | head -2",
        "crates/aterm-phase/src/prompt.rs:397:            \" Esc to cancel · Tab to amend\",",
        "crates/aterm-phase/src/prompt.rs:421:            \" Esc to cancel · Tab to amend\",",
        "% ",
    ]);
    let quoted = {
        let mut r = rows(&[
            "⏺ The worker's box read:",
            "   Do you want to proceed?",
            "   ❯ 1. Yes",
            "     2. No",
            "   Esc to cancel · Tab to amend",
            "",
        ]);
        r.extend(composer("  ? for shortcuts"));
        r
    };
    let no_options = {
        let mut r = rows(&["⏺ Done.", "", " Esc to cancel", ""]);
        r.extend(composer("  ? for shortcuts"));
        r
    };
    for (name, r) in [
        ("said", &said),
        ("bullet", &bullet),
        ("zsh", &zsh),
        ("quoted", &quoted),
        ("no options", &no_options),
    ] {
        assert_eq!(parse_prompt_v2(r), None, "{name}");
        assert_ne!(
            crate::phase::worker_phase(r),
            crate::phase::Phase::Prompt,
            "{name}"
        );
    }
    assert!(is_hint_footer(" Esc to cancel · Tab to amend"));
    assert!(is_hint_footer(" Enter to confirm · Esc to cancel"));
    assert!(is_hint_footer("  Esc to go back"));
    assert!(!is_hint_footer("  esc to interrupt"));
    assert!(!is_hint_footer(
        "⚠ Usage limit reached · continuing shortly · esc to cancel"
    ));
    assert!(!is_hint_footer("      Esc to cancel · Tab to amend"));
    assert!(!is_hint_footer(" Tab to amend"));
}

#[test]
fn an_unknown_box_is_titled_by_its_first_row_and_backs_out() {
    let mut r: Vec<String> = (0..40).map(|i| format!("⏺ transcript line {i}")).collect();
    r.extend(rows(&[
        "",
        " Select a model",
        " ❯ 1. Opus",
        "   2. Sonnet",
        "",
        " Enter to confirm · Esc to go back",
    ]));
    let p = parse_prompt_v2(&r).expect("a box");
    assert_eq!(p.kind, PromptKind::Other);
    assert_eq!(p.title, "Select a model");
    assert_eq!(p.cancel.map(|c| c.effect), Some(CancelEffect::Back));
    assert_eq!(prompt_box_span(&r).map(|(a, _)| a), Some(41));
    assert!(p.options.iter().all(|o| o.role == Role::Other));
    let last = r.len() - 1;
    r[last] = " Enter to select · Esc to go back".to_string();
    let p = parse_prompt_v2(&r).expect("a box");
    assert_eq!(p.kind, PromptKind::Question);
    assert_eq!(p.title, "Select a model");

    let mut o = rows(&[
        " Overwrite file",
        "",
        "   src/x.rs",
        "",
        " Do you want to overwrite x.rs?",
        " ❯ 1. Yes",
        "   2. No",
        "",
        " Esc to cancel · Tab to amend",
    ]);
    o.extend(composer("  ? for shortcuts"));
    let p = parse_prompt_v2(&o).expect("the box");
    assert_eq!(p.kind, PromptKind::Overwrite);
    assert_eq!(p.path.as_deref(), Some("src/x.rs"));
}

#[test]
fn roles_come_from_the_label_table() {
    let k = PromptKind::Bash;
    for (label, role) in [
        ("Yes", Role::Once),
        ("Yes, proceed", Role::Once),
        ("Yes, run it", Role::Once),
        ("Yes, and don’t ask again for: git log *", Role::Persist),
        (
            "Yes, and always allow access to /w from this project",
            Role::Persist,
        ),
        (
            "Yes, allow reading from /private/etc from this project",
            Role::Persist,
        ),
        (
            "Yes, and allow reading from ~/.ssh during this session",
            Role::Session,
        ),
        (
            "Yes, allow all edits during this session (shift+tab)",
            Role::ModeSwitch,
        ),
        (
            "Yes, and switch to auto mode · Claude edits, runs",
            Role::ModeSwitch,
        ),
        ("No", Role::Deny),
        ("No, and tell Claude what to do differently", Role::Deny),
        ("No, exit", Role::Exit),
        ("View raw script", Role::Other),
        ("Nooject", Role::Other),
        ("Yes, I trust this folder", Role::Other),
        (
            "No, and tell Claude what to do differently (esc)",
            Role::Deny,
        ),
        ("Deny (esc)", Role::Deny),
        (
            "Deny, and tell Claude what to do differently (esc)",
            Role::Deny,
        ),
        (
            "Deny — drop it and tell the sender it was declined",
            Role::Deny,
        ),
        (
            "Allow all actions on github.com for this session",
            Role::Session,
        ),
        ("Allow for this session (1 app)", Role::Session),
        ("Allow", Role::Other),
        ("Yes, enter plan mode", Role::ModeSwitch),
        ("Yes, manually approve edits", Role::ModeSwitch),
        (
            "Yes, clear context (12% used) and bypass permissions",
            Role::ModeSwitch,
        ),
        (
            "Yes, clear context (12% used) and use auto mode",
            Role::ModeSwitch,
        ),
        (
            "Yes, and switch to default (ask each time) for this session",
            Role::ModeSwitch,
        ),
        (
            "Yes, keep allowing reads outside the working directories",
            Role::Persist,
        ),
        (
            "Yes, and keep allowing any reads outside the working directories",
            Role::Persist,
        ),
        (
            "No, and block reads outside the working directories from now on",
            Role::Persist,
        ),
        ("No, and ask again next time", Role::Deny),
        ("Yes, but ask again next time", Role::Once),
        ("Yes, and remember this directory", Role::Persist),
        ("Don’t ask again", Role::Persist),
        ("No, continue without these permissions", Role::Deny),
        ("Not now", Role::Deny),
        ("Never mind", Role::Deny),
        ("Set this goal", Role::Other),
        ("Deliver this message to Claude", Role::Other),
        ("Yes (esc)", Role::Once),
        ("Continue without these settings", Role::Deny),
        ("Continue without browser tools", Role::Deny),
        ("Continue withoutfail", Role::Other),
        ("Continue with usage credits", Role::Other),
        ("Use this MCP server", Role::Persist),
        (
            "Use this and all future MCP servers in this project",
            Role::Persist,
        ),
        ("Continue without using this MCP server", Role::Persist),
        ("Enable selected", Role::Persist),
        ("Continue with browser tools", Role::Persist),
        ("Enable selected suites", Role::Other),
        ("Continue with browser tools for e2e", Role::Other),
        ("Apply to all future runs", Role::Other),
    ] {
        assert_eq!(role_of(label, k), role, "{label}");
    }
    for label in ["[✔] lab-probe-a", "Enable selected", "Yes", "No"] {
        assert_eq!(
            role_of(label, PromptKind::McpServer),
            Role::Persist,
            "{label}"
        );
    }
    assert_eq!(
        role_of("Yes, I trust this folder", PromptKind::Trust),
        Role::Trust
    );
    assert_eq!(role_of("Allow", PromptKind::Browser), Role::Once);
    assert_eq!(role_of("Allow", PromptKind::Tool), Role::Other);
    assert_eq!(
        role_of(
            "Allow all actions on x for this session",
            PromptKind::Browser
        ),
        Role::Session
    );
}

fn bash_box(command_rows: &[&str], options: &[&str]) -> Vec<String> {
    let mut r = rows(&[&"─".repeat(120), " Bash command", ""]);
    r.extend(rows(command_rows));
    r.extend(rows(&["", " Do you want to proceed?"]));
    r.extend(rows(options));
    r.extend(rows(&["", " Esc to cancel · Tab to amend"]));
    r
}

const PERSIST_OPTIONS: [&str; 3] = [
    " ❯ 1. Yes",
    "   2. Yes, and always allow access to /Users//x/proj from this project",
    "   3. No",
];

#[test]
fn a_description_that_reads_as_an_option_never_becomes_one() {
    let r = bash_box(&["   ls", "   2. Yes"], &PERSIST_OPTIONS);
    let p = parse_prompt_v2(&r).expect("the box");
    let numbered: Vec<(Option<u8>, &str, Role)> = p
        .options
        .iter()
        .map(|o| (o.n, o.label.as_str(), o.role))
        .collect();
    assert_eq!(numbered.len(), 3, "{numbered:?}");
    assert_eq!(p.with_role(Role::Once).map(|o| o.n), Some(Some(1)));
    assert_eq!(p.with_role(Role::Persist).map(|o| o.n), Some(Some(2)));
    assert!(p.readings().iter().any(|x| x.contains("2. Yes")));

    let mut noq = r.clone();
    noq.retain(|row| row.trim() != "Do you want to proceed?");
    let p = parse_prompt_v2(&noq).expect("the box");
    assert!(p.options.iter().any(|o| o.n == Some(2) && o.label == "Yes"));
    assert!(
        p.options.iter().all(|o| o.role == Role::Other),
        "{:?}",
        p.options
    );
    assert_eq!(p.with_role(Role::Once), None);

    for options in [
        &[" ❯ 1. Yes", "   1. Yes", "   2. No"][..],
        &[" ❯ 1. Yes", "   3. No"],
        &[" ❯ 1. Yes", " ❯ 2. No"],
        &[" ❯ 1. Yes", " Stray words", "   2. No"],
    ] {
        let p = parse_prompt_v2(&bash_box(&["   ls", "   List files"], options)).expect("the box");
        assert!(
            p.options.iter().all(|o| o.role == Role::Other),
            "{options:?}: {:?}",
            p.options
        );
    }
    let p =
        parse_prompt_v2(&bash_box(&["   ls", "   List files"], &PERSIST_OPTIONS)).expect("the box");
    let roles: Vec<Role> = p.options.iter().map(|o| o.role).collect();
    assert_eq!(roles, vec![Role::Once, Role::Persist, Role::Deny]);
}

#[test]
fn every_row_that_may_run_is_in_the_readings() {
    let two = [" ❯ 1. Yes", "   2. No"];
    for (cmd, tail) in [
        (
            &["   │ cat README.md", "   │ HOME=/tmp rm -rf ~/work"][..],
            "rm -rf ~/work",
        ),
        (
            &[
                "   │ find . -name '*.tmp' -print |",
                "   │ Xargs rm -rf build out dist",
            ],
            "Xargs rm",
        ),
        (
            &["   cat README.md", "   HOME=/tmp rm -rf ~/work"],
            "rm -rf",
        ),
        (&["   cat README.md", "", "   ; rm -rf ~/work"], "; rm -rf"),
        (
            &["   cat README.md", "   Tip: x; rm -rf ~/work"],
            "; rm -rf",
        ),
    ] {
        let p = parse_prompt_v2(&bash_box(cmd, &two)).expect("the box");
        assert_eq!(p.kind, PromptKind::Bash);
        assert!(
            p.command_rows.iter().any(|r| r.contains(tail)),
            "{cmd:?}: {:?}",
            p.command_rows
        );
        assert!(
            p.readings().iter().any(|r| r.contains(tail)),
            "{cmd:?}: {:?}",
            p.readings()
        );
    }
    let p = parse_prompt_v2(&bash_box(
        &["   │ cat README.md", "   │ HOME=/tmp rm -rf ~/work"],
        &two,
    ))
    .expect("the box");
    assert!(p.gutter);
    assert_eq!(p.command, "cat README.md");
    assert_eq!(p.description, "HOME=/tmp rm -rf ~/work");
    assert_eq!(p.description_rows, 1);
    assert_eq!(
        p.readings(),
        vec![
            "cat README.md",
            "cat README.md HOME=/tmp rm -rf ~/work",
            "cat README.md\nHOME=/tmp rm -rf ~/work",
        ]
    );
    let p = parse_prompt_v2(&bash_multi_row_with_note()).expect("the box");
    assert!(
        p.command_rows
            .last()
            .is_some_and(|r| r.starts_with("Extract three trees")),
        "{:?}",
        p.command_rows
    );
    assert!(
        p.readings()
            .iter()
            .any(|r| r.ends_with("real content changes"))
    );
    let p = parse_prompt_v2(&screen(BOX_RM)).expect("the box");
    assert_eq!(p.command_rows.len(), 2, "{:?}", p.command_rows);
    assert_eq!(p.notes.len(), 1);
}

#[test]
fn the_question_dialog_footers_are_footers() {
    for footer in [
        " Enter to select · Tab/Arrow keys to navigate · Esc to cancel",
        " Enter to select · ↑/↓ to navigate · Esc to cancel",
        " Enter to select · Tab/Arrow keys to navigate · ctrl+g to edit in VS Code · Esc to cancel",
        " ←/→ to switch · ↑/↓ to navigate · Enter to select · Esc to close",
    ] {
        assert!(is_hint_footer(footer), "{footer}");
        let r = rows(&[
            "⏺ I have two questions before I start.",
            "",
            "────────────────────────────────────────────────────────────",
            " ←  ☐ Scope  ☐ Tests  ✔ Submit  →",
            "",
            " Which scope should the refactor cover?",
            "",
            " ❯ 1. Small",
            "      Just the parser",
            "   2. Large",
            "      Everything under crates/",
            "   3. Type something.",
            " ────────────────────────────────────────",
            "   4. Chat about this",
            "",
            footer,
        ]);
        assert_eq!(
            crate::phase::worker_phase(&r),
            crate::phase::Phase::Prompt,
            "{footer}"
        );
        let p = parse_prompt_v2(&r).expect("the dialog");
        assert_eq!(p.kind, PromptKind::Question);
        let labels: Vec<&str> = p.options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "Small Just the parser",
                "Large Everything under crates/",
                "Type something.",
                "Chat about this"
            ]
        );
        assert!(p.options.iter().all(|o| o.role == Role::Other));
    }
    assert!(!is_hint_footer(" Enter to select · - Esc to cancel"));
    assert!(!is_hint_footer(" Press Esc to cancel"));
}

#[test]
fn a_box_under_a_multi_row_tool_output_is_a_box() {
    let box_rows = [
        "",
        " Bash command",
        "",
        "   git push",
        "   Push the branch",
        "",
        " Do you want to proceed?",
        " ❯ 1. Yes",
        "   2. No",
        "",
        " Esc to cancel · Tab to amend",
    ];
    let quote = format!("     {}1% until auto-compact", " ".repeat(77));
    for above in [
        vec![
            "⏺ Bash(aterm ctl @s-2 text)",
            "  ⎿  ✢ Booping…",
            quote.as_str(),
        ],
        vec!["⏺ Bash(ls)", "  ⎿  a", "     b", "     c"],
        vec!["⏺ A message that wraps", "  onto a second row."],
    ] {
        let mut r = rows(&above);
        r.extend(rows(&box_rows));
        r.extend(composer("  ? for shortcuts"));
        assert_eq!(
            parse_prompt(&r).map(|p| p.kind),
            Some(PromptKind::Bash),
            "{above:?}"
        );
        assert_eq!(crate::phase::worker_phase(&r), crate::phase::Phase::Prompt);
        let mut on = rows(&above);
        on.extend(rows(&box_rows[1..]));
        on.extend(composer("  ? for shortcuts"));
        assert_eq!(parse_prompt(&on), None, "{above:?}");
    }
}

#[test]
fn a_box_quoted_in_a_message_is_not_a_box() {
    let trust = [
        "⏺ For reference, the dialog looked like this:",
        "",
        "  Accessing workspace:",
        "",
        "  /Users//x/trusted/proj",
        "",
        "  ❯ No, exit",
        "    Yes, I trust this folder",
        "",
        "  Enter to confirm · Esc to cancel",
        "",
    ];
    let bash = [
        "⏺ It asked:",
        "",
        "  Bash command",
        "",
        "    ls -la",
        "    List files",
        "",
        "  Do you want to proceed?",
        "  ❯ 1. Yes",
        "    2. No",
        "",
        "  Esc to cancel · Tab to amend",
        "",
    ];
    for quoted in [&trust[..], &bash] {
        let mut r = rows(quoted);
        r.extend(composer("  ? for shortcuts"));
        assert_eq!(parse_prompt_v2(&r), None, "{quoted:?}");
        assert_ne!(
            crate::phase::worker_phase(&r),
            crate::phase::Phase::Prompt,
            "{quoted:?}"
        );
        let live: Vec<String> = quoted
            .iter()
            .map(|row| row.strip_prefix(' ').unwrap_or(row).to_string())
            .collect();
        assert!(parse_prompt_v2(&live).is_some(), "{live:?}");
    }
}

#[test]
fn a_box_quoted_in_the_users_message_is_not_a_box() {
    let bash = [
        "   Bash command",
        "     rm -rf ~/work",
        "   Do you want to proceed?",
        "   ❯ 1. Yes",
        "     2. No",
        "   Esc to cancel · Tab to amend",
    ];
    let tool = [
        "   Tool use",
        "     github - create_issue Tool: (MCP)",
        "   Do you want to proceed?",
        "     Yes",
        "   ❯ No",
        "   Esc to cancel · Tab to amend",
    ];
    let tail = |r: &mut Vec<String>| {
        r.extend(rows(&["", "✻ Thinking… (3s · ↓ 12 tokens)"]));
        r.extend(composer("  esc to interrupt"));
    };
    for quoted in [&bash[..], &tool] {
        let mut r = rows(&[
            "⏺ Done.",
            "",
            "❯ The harness showed me this, what should I do?",
        ]);
        r.extend(rows(quoted));
        tail(&mut r);
        assert_eq!(parse_prompt_v2(&r), None, "{quoted:?}");
        assert_ne!(
            crate::phase::worker_phase(&r),
            crate::phase::Phase::Prompt,
            "{quoted:?}"
        );
        for above in [
            vec!["❯ Run it."],
            vec![
                "❯ Run the cleanup that the plan in the",
                "  notes file describes.",
            ],
        ] {
            let mut live = rows(&above);
            live.push(String::new());
            live.extend(quoted.iter().map(|q| q[2..].to_string()));
            tail(&mut live);
            assert!(parse_prompt_v2(&live).is_some(), "{live:?}");
        }
    }
}

#[test]
fn a_box_quoted_under_an_indented_stop_row_is_not_a_box() {
    let bash = [
        " Bash command",
        "   rm -rf ~/work",
        " Do you want to proceed?",
        " ❯ 1. Yes",
        "   2. No",
        " Esc to cancel · Tab to amend",
    ];
    let tool = [
        " Tool use",
        "   github - create_issue Tool: (MCP)",
        " Do you want to proceed?",
        " ❯ Yes",
        "   No",
        " Esc to cancel · Tab to amend",
    ];
    let stops: [&[&str]; 6] = [
        &[
            "  ┌──────┬──────┐",
            "  │ a    │ b    │",
            "  └──────┴──────┘",
        ],
        &[
            "  ┌──────┬──────┐",
            "  │ a    │ b    │",
            "  └──────┴──────┘",
            "",
        ],
        &["  ╭───────────────╮"],
        &["  ✻ Cogitating… (5s)"],
        &["  * Waiting for the harness…"],
        &[" ╰───────────────╯"],
    ];
    let said = [
        "⏺ Here is what the harness showed:",
        "❯ The harness showed me this, what should I do?",
    ];
    for body in [&bash[..], &tool] {
        for stop in stops {
            for who in said {
                for (column, busy) in [("  ", true), (" ", true), ("  ", false)] {
                    let mut r = rows(&["⏺ Done.", "", who]);
                    r.extend(rows(stop));
                    r.extend(body.iter().map(|b| format!("{column}{b}")));
                    if busy {
                        r.extend(rows(&["", "✻ Thinking… (3s · ↓ 12 tokens)"]));
                        r.extend(composer("  esc to interrupt"));
                    } else {
                        r.push(String::new());
                        r.extend(composer("  ? for shortcuts"));
                    }
                    assert_eq!(parse_prompt_v2(&r), None, "{r:#?}");
                    let phase = crate::phase::worker_phase(&r);
                    assert_ne!(phase, crate::phase::Phase::Prompt, "{r:#?}");
                    let spinner = stop.iter().any(|s| crate::phase::is_glyph_row(s));
                    if !busy && !spinner {
                        assert_eq!(phase, crate::phase::Phase::Idle, "{r:#?}");
                    }
                }
            }
        }
    }
    for stop in [
        "─".repeat(120),
        "╰".to_string() + &"─".repeat(40) + "╯",
        "✻ Cogitating… (5s)".to_string(),
    ] {
        let mut live = rows(&["⏺ Done.", "", said[0]]);
        live.push(stop.clone());
        live.extend(rows(&bash));
        live.push(String::new());
        live.extend(composer("  ? for shortcuts"));
        assert!(parse_prompt_v2(&live).is_some(), "{live:#?}");
    }
    let mut live = rows(&["⏺ Done.", "", said[1], ""]);
    live.extend(rows(&bash));
    live.extend(rows(&["", "✻ Thinking… (3s · ↓ 12 tokens)"]));
    live.extend(composer("  esc to interrupt"));
    assert!(parse_prompt_v2(&live).is_some(), "{live:#?}");
}

#[test]
fn a_wrapped_trust_path_is_read_whole() {
    let full = screen(TRUST)
        .iter()
        .map(|r| r.trim())
        .find(|t| t.starts_with("/private/"))
        .expect("the folder")
        .to_string();
    for (path, cut) in [
        (full.as_str(), 40),
        (
            "/private/tmp/claude-502-evil/proj",
            "/private/tmp/claude-502".len(),
        ),
    ] {
        let r: Vec<String> = screen(TRUST)
            .into_iter()
            .flat_map(|row| {
                if row.trim().starts_with("/private/") {
                    vec![format!(" {}", &path[..cut]), format!(" {}", &path[cut..])]
                } else {
                    vec![row]
                }
            })
            .collect();
        let p = parse_prompt_v2(&r).expect("the dialog");
        assert_eq!(p.kind, PromptKind::Trust);
        assert_eq!(p.path.as_deref(), Some(path));
        assert_eq!(p.command, path);
        assert!(!p.description.contains("-evil"), "{}", p.description);
    }
}

#[test]
fn option_rows_need_a_number_a_dot_and_a_space() {
    assert_eq!(option_row(" ❯ 1. Yes", ARROW), Some((1, "Yes".to_string())));
    assert_eq!(
        option_row("   12. Many", ARROW),
        Some((12, "Many".to_string()))
    );
    assert_eq!(option_row("   2.5 GB free", ARROW), None);
    assert_eq!(option_row("   2158    -    let x = 1;", ARROW), None);
    assert_eq!(option_row("  1.Yes", ARROW), None);
    assert_eq!(option_row("  ❯ 3. No", ARROW), Some((3, "No".to_string())));
}

#[test]
fn the_small_pointer_is_a_cursor_only_in_the_usage_limit_dialog() {
    let screen = |title: &str| -> Vec<String> {
        [
            "",
            &"─".repeat(60),
            &format!(" {title}"),
            "",
            " › agent-3 is reading prompt.rs",
            "",
            " Enter to confirm · Esc to cancel",
        ]
        .iter()
        .map(|r| (*r).to_string())
        .collect()
    };
    assert_eq!(parse_prompt_v2(&screen("Background agents")), None);
    let p = parse_prompt_v2(&screen("What do you want to do?")).expect("a box");
    assert_eq!(p.kind, PromptKind::Other);
    assert_eq!(
        p.focused().map(|o| o.label.as_str()),
        Some("agent-3 is reading prompt.rs")
    );
    let mut d: Vec<String> = limit_options_dialog(&["Keep the branch", "Drop it"], 0)
        .iter()
        .map(|x| x.replace("  ❯ 1. ", "  › 1. "))
        .collect();
    let t = d
        .iter()
        .position(|x| x.trim() == "What do you want to do?")
        .expect("the title");
    d[t] = "  Pick one".to_string();
    let p = parse_prompt_v2(&d).expect("a box");
    let labels: Vec<&str> = p.options.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(labels, ["Drop it"]);
    assert!(
        p.options
            .iter()
            .all(|o| !o.focused && o.role == Role::Other)
    );
}

struct Want {
    text: &'static str,
    kind: PromptKind,
    title: &'static str,
    command: &'static str,
    path: Option<&'static str>,
    roles: &'static [Role],
    select: Select,
    cancel: Option<CancelEffect>,
    footer: bool,
}

#[test]
fn every_2_1_282_box_reads_as_its_own_kind() {
    use CancelEffect::Reject;
    use Role::*;
    let wants = [
        Want {
            text: BOX_BASH_SUBAGENT,
            kind: PromptKind::Bash,
            title: "Bash command · from the Explore agent",
            command: "git status --short",
            path: None,
            roles: &[Once, Persist, Deny],
            select: Select::Digits,
            cancel: Some(Reject),
            footer: true,
        },
        Want {
            text: BOX_BASH_RUNS_ON,
            kind: PromptKind::Bash,
            title: "Bash command (runs on m3)",
            command: "ls",
            path: None,
            roles: &[Once, Deny],
            select: Select::Digits,
            cancel: Some(Reject),
            footer: true,
        },
        Want {
            text: BOX_POWERSHELL,
            kind: PromptKind::PowerShell,
            title: "PowerShell command",
            command: "Get-ChildItem",
            path: None,
            roles: &[Once, Persist, Deny],
            select: Select::Digits,
            cancel: Some(Reject),
            footer: true,
        },
        Want {
            text: BOX_EDIT_NOTEBOOK,
            kind: PromptKind::Edit,
            title: "Edit notebook",
            command: "a.ipynb",
            path: Some("a.ipynb"),
            roles: &[Once, Deny],
            select: Select::Digits,
            cancel: Some(Reject),
            footer: true,
        },
        Want {
            text: BOX_EDIT_IDE,
            kind: PromptKind::Edit,
            title: "Opened changes in VS Code ⧉",
            command: "x.rs",
            path: Some("x.rs"),
            roles: &[Once, Deny],
            select: Select::Digits,
            cancel: Some(Reject),
            footer: true,
        },
        Want {
            text: BOX_FETCH,
            kind: PromptKind::Fetch,
            title: "Fetch",
            command: "url: https://docs.rs/x",
            path: None,
            roles: &[Once, Persist, Deny],
            select: Select::Digits,
            cancel: Some(Reject),
            footer: false,
        },
        Want {
            text: BOX_NETWORK,
            kind: PromptKind::NetworkRequest,
            title: "Network request outside of sandbox",
            command: "Host: registry.npmjs.org",
            path: None,
            roles: &[Once, Persist, Deny],
            select: Select::Digits,
            cancel: Some(Reject),
            footer: false,
        },
        Want {
            text: BOX_BROWSER,
            kind: PromptKind::Browser,
            title: "Claude in Chrome wants to click on github.com",
            command: "https://github.com/x",
            path: None,
            roles: &[Once, Session, Deny],
            select: Select::Digits,
            cancel: Some(Reject),
            footer: false,
        },
        Want {
            text: BOX_SKILL,
            kind: PromptKind::Skill,
            title: "Use skill \"deploy\"?",
            command: "deploy",
            path: None,
            roles: &[Once, Persist, Deny],
            select: Select::Digits,
            cancel: Some(Reject),
            footer: true,
        },
        Want {
            text: BOX_MONITOR,
            kind: PromptKind::Monitor,
            title: "Monitor",
            command: "tail -f build.log",
            path: None,
            roles: &[Once, Persist, Deny],
            select: Select::Digits,
            cancel: Some(Reject),
            footer: true,
        },
        Want {
            text: BOX_TOOL,
            kind: PromptKind::Tool,
            title: "Tool use",
            command: "github - create_issue Tool: (MCP)",
            path: None,
            roles: &[Once, Persist, Deny],
            select: Select::Digits,
            cancel: Some(Reject),
            footer: true,
        },
        Want {
            text: BOX_TOOL_DEFAULT_NO,
            kind: PromptKind::Tool,
            title: "Tool use",
            command: "Slack - send_message Tool: (MCP)",
            path: None,
            roles: &[Deny, Once],
            select: Select::ArrowsEnter,
            cancel: Some(Reject),
            footer: true,
        },
        Want {
            text: BOX_TOOL_CARD,
            kind: PromptKind::Tool,
            title: "Create an issue in alabsystems/aterm?",
            command: "title   Harness presses a quoted box",
            path: None,
            roles: &[Once, Persist, Deny],
            select: Select::Digits,
            cancel: Some(Reject),
            footer: true,
        },
        Want {
            text: TRUST_BACKSTOP,
            kind: PromptKind::Trust,
            title: "Accessing workspace:",
            command: "/Users//user00/aterm",
            path: Some("/Users//user00/aterm"),
            roles: &[Deny, Trust],
            select: Select::ArrowsEnter,
            cancel: Some(Reject),
            footer: true,
        },
        Want {
            text: COMPUTER_USE,
            kind: PromptKind::ComputerUse,
            title: "Computer Use wants to control these apps",
            command: "",
            path: None,
            roles: &[Deny, Session],
            select: Select::ArrowsEnter,
            cancel: Some(Reject),
            footer: true,
        },
        Want {
            text: READ_OUTSIDE_SETTING,
            kind: PromptKind::ReadOutsideSetting,
            title: "Read outside the working directories",
            command: "",
            path: None,
            roles: &[Persist, Persist, Deny],
            select: Select::Digits,
            cancel: Some(Reject),
            footer: true,
        },
        Want {
            text: HELD_MESSAGE,
            kind: PromptKind::HeldMessage,
            title: "Held message from another session",
            command: "",
            path: None,
            roles: &[Deny, Other],
            select: Select::ArrowsEnter,
            cancel: None,
            footer: false,
        },
        Want {
            text: PLAN_ENTER,
            kind: PromptKind::PlanEnter,
            title: "Enter plan mode?",
            command: "",
            path: None,
            roles: &[ModeSwitch, Deny],
            select: Select::ArrowsEnter,
            cancel: None,
            footer: false,
        },
        Want {
            text: PLAN_READY,
            kind: PromptKind::PlanExit,
            title: "Ready to code?",
            command: "",
            path: None,
            roles: &[ModeSwitch, ModeSwitch, Deny],
            select: Select::Digits,
            cancel: None,
            footer: false,
        },
        Want {
            text: GOAL_PROPOSAL,
            kind: PromptKind::GoalProposal,
            title: "Claude proposes a goal",
            command: "",
            path: None,
            roles: &[Deny, Other],
            select: Select::ArrowsEnter,
            cancel: None,
            footer: false,
        },
        Want {
            text: QUESTION_YES_NO,
            kind: PromptKind::Question,
            title: "☐ Migration",
            command: "",
            path: None,
            roles: &[Other, Other, Other, Other],
            select: Select::Digits,
            cancel: Some(Reject),
            footer: true,
        },
        Want {
            text: QUESTION_DO_YOU_WANT,
            kind: PromptKind::Question,
            title: "☐ Proceed",
            command: "",
            path: None,
            roles: &[Other, Other, Other, Other],
            select: Select::Digits,
            cancel: Some(Reject),
            footer: true,
        },
        Want {
            text: SETUP_AUTO_MODE_DEFAULT,
            kind: PromptKind::Other,
            title: "Make auto mode your default permission mode?",
            command: "",
            path: None,
            roles: &[ModeSwitch, Deny],
            select: Select::ArrowsEnter,
            cancel: None,
            footer: false,
        },
        Want {
            text: SETUP_CHROME_UPSELL,
            kind: PromptKind::Other,
            title: "Claude wants to use your browser",
            command: "",
            path: None,
            roles: &[Other, Deny, Persist],
            select: Select::ArrowsEnter,
            cancel: None,
            footer: false,
        },
        Want {
            text: SETUP_REMOTE_CONTROL,
            kind: PromptKind::Other,
            title: "Remote Control",
            command: "",
            path: None,
            roles: &[Other, Deny],
            select: Select::Digits,
            cancel: None,
            footer: false,
        },
        Want {
            text: SESSION_PAUSED,
            kind: PromptKind::Other,
            title: "Session paused",
            command: "",
            path: None,
            roles: &[Other, Other],
            select: Select::Digits,
            cancel: None,
            footer: false,
        },
    ];
    for w in wants {
        let r = screen(w.text);
        let name = w.title;
        let reading = crate::reader::read(Some("claude"), &r, None);
        assert_eq!(reading.phase, crate::phase::Phase::Prompt, "{name}");
        assert!(reading.phase_authoritative, "{name}");
        let p = reading.prompt.expect("the box");
        assert_eq!(p.kind, w.kind, "{name}");
        assert_eq!(p.title, w.title, "{name}");
        assert_eq!(p.command, w.command, "{name}");
        assert_eq!(p.path.as_deref(), w.path, "{name}");
        let roles: Vec<Role> = p.options.iter().map(|o| o.role).collect();
        assert_eq!(roles, w.roles, "{name}: {:?}", p.options);
        assert_eq!(p.select, w.select, "{name}");
        assert_eq!(p.cancel.as_ref().map(|c| c.effect), w.cancel, "{name}");
        assert_eq!(p.footer.is_some(), w.footer, "{name}");
        assert_eq!(r[p.span.0].trim(), w.title, "{name}");
        let first = usize::from(w.text == SETUP_CHROME_UPSELL);
        assert_eq!(
            p.focused().map(|o| o.row),
            Some(p.options[first].row),
            "{name}"
        );
    }
}

#[test]
fn powershell_reads_like_bash_and_the_notebook_names_its_file_in_the_question() {
    let p = parse_prompt_v2(&screen(BOX_POWERSHELL)).expect("the box");
    assert_eq!(p.command_rows, ["Get-ChildItem", "List"]);
    assert_eq!(p.description, "List");
    assert_eq!(p.readings(), ["Get-ChildItem", "Get-ChildItem List"]);
    let mut breaker = screen(BOX_RM);
    for row in &mut breaker {
        if row.trim() == "Bash command" {
            *row = " PowerShell command".to_string();
        }
    }
    let p = parse_prompt_v2(&breaker).expect("the box");
    assert_eq!(p.kind, PromptKind::PowerShell);
    assert_eq!(p.notes.len(), 1);
    assert_eq!(p.rm_breaker(), None);

    let r = screen(BOX_EDIT_NOTEBOOK);
    let p = parse_prompt_v2(&r).expect("the box");
    assert_eq!(p.path.as_deref(), Some("a.ipynb"));
    assert_eq!(
        question_file("Do you want to delete this cell from b.ipynb?").as_deref(),
        Some("b.ipynb")
    );
    assert_eq!(question_file("Do you want to proceed?"), None);
    assert_eq!(question_file("Do you want to create ?"), None);
    let p = parse_prompt_v2(&screen(BOX_EDIT)).expect("the control");
    assert_eq!(p.path.as_deref(), Some("notes.txt"));
}

#[test]
fn the_header_grammar_reads_every_origin() {
    let h = |t: &str| parse_header(t).unwrap_or_else(|| panic!("{t}"));
    let o = |t: &str| h(t).origin;
    let some = |s: &str| Some(s.to_string());
    assert_eq!(
        o("Bash command · from the \"trust-vc-front-end-m1\" workflow"),
        Some(Origin::Workflow {
            name: some("trust-vc-front-end-m1")
        })
    );
    assert_eq!(
        o("Bash command · from a workflow"),
        Some(Origin::Workflow { name: None })
    );
    assert_eq!(
        o("Bash command · from the Explore agent"),
        Some(Origin::Subagent {
            name: some("Explore")
        })
    );
    assert_eq!(
        o("Bash command · from a subagent"),
        Some(Origin::Subagent { name: None })
    );
    assert_eq!(
        o("Bash command · from a remote cloud agent"),
        Some(Origin::RemoteAgent)
    );
    assert_eq!(
        o("Bash command · from the ralph plugin"),
        Some(Origin::Plugin {
            name: some("ralph")
        })
    );
    assert_eq!(
        o("Bash command · from a plugin"),
        Some(Origin::Plugin { name: None })
    );
    assert_eq!(o("Bash command"), None);
    let x = h("Bash command · from the \"verify\" workflow                    2 of 5");
    assert_eq!(
        (x.base.as_str(), x.origin),
        (
            "Bash command",
            Some(Origin::Workflow {
                name: some("verify")
            })
        )
    );
    assert_eq!(
        h("Bash command (runs on m3)          1 of 2")
            .runs_on
            .as_deref(),
        Some("m3")
    );
    assert_eq!(h("Read file(s) 3 of 3").base, "Read file(s)");
    let x = h("Bash command (unsandboxed) · from a subagent");
    assert_eq!(
        (x.base.as_str(), x.unsandboxed, x.runs_on.as_deref()),
        ("Bash command", true, None)
    );
    let x = h("Bash command (runs on m3) · from the \"a · b\" workflow");
    assert_eq!(
        (x.base.as_str(), x.unsandboxed, x.runs_on.as_deref()),
        ("Bash command", false, Some("m3"))
    );
    assert_eq!(
        x.origin,
        Some(Origin::Workflow {
            name: some("a · b")
        })
    );
    assert_eq!(
        h("Edit file (runs on build-box)").runs_on.as_deref(),
        Some("build-box")
    );
    assert_eq!(h("Read file(s) · from a plugin").base, "Read file(s)");
    assert_eq!(
        h("PowerShell command (unsandboxed)").base,
        "PowerShell command"
    );
    for t in [
        "Bash command · from somewhere",
        "Bash command · from the  agent",
        "Bash command · from the \"x\" pipeline",
        "Bash command · Tab to amend",
    ] {
        assert_eq!(parse_header(t), None, "{t}");
    }
    let p = parse_prompt_v2(&screen(BOX_RM_UNRESOLVABLE_WORKFLOW)).expect("the box");
    assert_eq!(p.base_title().as_deref(), Some("Bash command"));
    assert_eq!(
        p.origin(),
        Some(Origin::Workflow {
            name: some("trust-vc-front-end-m1")
        })
    );
    assert!(!p.unsandboxed());
    assert_eq!(p.runs_on(), None);
    let p = parse_prompt_v2(&screen(BOX_BASH_RUNS_ON)).expect("the box");
    assert_eq!(p.runs_on().as_deref(), Some("m3"));
    assert_eq!(p.origin(), None);
    let p = parse_prompt_v2(&screen(BOX_BASH_SUBAGENT)).expect("the box");
    assert_eq!(
        p.origin(),
        Some(Origin::Subagent {
            name: some("Explore")
        })
    );
    for (title, kind) in [
        ("Monitor", Some(PromptKind::Monitor)),
        ("Monitor · from a subagent", Some(PromptKind::Monitor)),
        ("Monitor details", None),
        ("Fetch", Some(PromptKind::Fetch)),
        ("Fetching the docs", None),
        ("Fetch · from somewhere odd", None),
        ("Tool use", Some(PromptKind::Tool)),
        ("Tool usage", None),
        ("Tool use · from the github plugin", Some(PromptKind::Tool)),
        (
            "PowerShell command (unsandboxed)",
            Some(PromptKind::PowerShell),
        ),
        ("PowerShell commands", None),
        ("Use skill \"deploy\"?", Some(PromptKind::Skill)),
        ("Use skill \"\"?", None),
        ("Use this skill?", Some(PromptKind::Skill)),
        (
            "Claude in Chrome wants to navigate",
            Some(PromptKind::Browser),
        ),
        ("Claude in Chrome wants to", None),
        ("Opened changes in Zed ⧉", Some(PromptKind::Edit)),
        ("Opened changes in ⧉", None),
        ("Edit notebook", Some(PromptKind::Edit)),
        ("Enter plan mode?", Some(PromptKind::PlanEnter)),
        ("Exit plan mode?", Some(PromptKind::PlanExit)),
        ("Ready to code?", Some(PromptKind::PlanExit)),
        (
            "Held message from another session",
            Some(PromptKind::HeldMessage),
        ),
        ("Claude proposes a goal", Some(PromptKind::GoalProposal)),
        (
            "Computer Use wants to control these apps",
            Some(PromptKind::ComputerUse),
        ),
        (
            "Read outside the working directories",
            Some(PromptKind::ReadOutsideSetting),
        ),
        (
            "Network request outside of sandbox",
            Some(PromptKind::NetworkRequest),
        ),
        ("Bash command · from somewhere", Some(PromptKind::Bash)),
        ("Read file(s)", Some(PromptKind::Read)),
        (
            "New MCP server found in this project: lab-probe",
            Some(PromptKind::McpServer),
        ),
        (
            "New MCP server found in this project: a · from the x plugin",
            Some(PromptKind::McpServer),
        ),
        (
            "New MCP server found in this project: srv 1 of 2",
            Some(PromptKind::McpServer),
        ),
        (
            "2 new MCP servers found in this project",
            Some(PromptKind::McpServer),
        ),
        (
            "12 new MCP servers found in this project",
            Some(PromptKind::McpServer),
        ),
        ("new MCP servers found in this project", None),
        ("two new MCP servers found in this project", None),
        ("2 new MCP servers found in this project!", None),
        ("New MCP server found", None),
        (
            "Tool use                          1 of 2",
            Some(PromptKind::Tool),
        ),
        ("Fetch    2 of 5", Some(PromptKind::Fetch)),
        (
            "PowerShell command (unsandboxed)      12 of 14",
            Some(PromptKind::PowerShell),
        ),
        (
            "Held message from another session     1 of 2",
            Some(PromptKind::HeldMessage),
        ),
        (
            "Claude proposes a goal     2 of 3",
            Some(PromptKind::GoalProposal),
        ),
        ("Enter plan mode?   2 of 3", Some(PromptKind::PlanEnter)),
        ("Tool use   3 of 2", None),
        ("Tool use   1 of 1", None),
        ("Tool use   one of two", None),
        ("Tool usage   1 of 2", None),
        ("Monitor details   1 of 2", None),
    ] {
        assert_eq!(header_kind(title), kind, "{title}");
    }
    for text in [
        BOX_BASH_SUBAGENT,
        BOX_EDIT,
        BOX_TOOL,
        BOX_FETCH,
        BOX_MONITOR,
        HELD_MESSAGE,
        GOAL_PROPOSAL,
    ] {
        let plain = screen(text);
        let mut labelled = plain.clone();
        let rule = labelled.iter().position(|r| r.starts_with('─')).unwrap();
        let width = labelled[rule].chars().count();
        let title = labelled[rule + 1].trim_end().to_string();
        let gap = width - title.chars().count() - " 2 of 5 ".len();
        labelled[rule + 1] = format!("{title}{} 2 of 5 ", " ".repeat(gap));
        let (a, b) = (
            parse_prompt_v2(&plain).expect("the box"),
            parse_prompt_v2(&labelled).expect("the labelled box"),
        );
        assert_eq!(
            (a.kind, &a.title, a.header(), &a.options),
            (b.kind, &b.title, b.header(), &b.options),
            "{title}"
        );
    }
}

/// Claude Code 2.1.308 leaves its API-retry row standing under the `Session paused` dialog
/// that replaced its composer (measured 2026-10-26, a window tab that stood there for hours,
/// read as no box): the dialog is read over it, and the session's phase is the box's. The
/// same rows under a composer, or with a said row in the retry row's place, read no box.
#[test]
fn a_footerless_box_is_read_over_the_retry_row_it_left_standing() {
    let r = screen(SESSION_PAUSED_RETRY_ROW);
    let retry = r.iter().rposition(|x| !x.trim().is_empty()).unwrap();
    assert!(
        r[retry].contains("Waiting for API response"),
        "{}",
        r[retry]
    );
    let p = parse_prompt_v2(&r).expect("the box");
    assert_eq!(p.title, "Session paused");
    assert_eq!(p.select, Select::Digits);
    let labels: Vec<&str> = p.options.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(
        labels,
        ["Switch to Opus 4.8", "Edit prompt and retry with Opus 5.5"]
    );
    assert_eq!(p.focused().map(|o| o.row), Some(p.options[0].row));
    assert_eq!(p.span.1, retry - 2, "the box ends on its last option");
    assert_eq!(prompt_box_first_row(&r), Some(p.span.0));
    let reading = crate::reader::read(Some("claude"), &r, None);
    assert_eq!(reading.phase, crate::phase::Phase::Prompt);
    assert!(reading.phase_authoritative);

    let mut scrolled = r.clone();
    scrolled.push(String::new());
    scrolled.extend(composer("  ? for shortcuts"));
    assert_eq!(parse_prompt_v2(&scrolled), None, "+ composer");
    let mut went_on = r.clone();
    went_on[retry] = "⏺ Done.".to_string();
    assert_eq!(parse_prompt_v2(&went_on), None, "a said row in its place");
}

#[test]
fn a_footerless_box_is_read_at_the_live_bottom_only() {
    for text in [
        BOX_FETCH,
        BOX_NETWORK,
        BOX_BROWSER,
        HELD_MESSAGE,
        HELD_MESSAGE_2_1_287,
        PLAN_ENTER,
        PLAN_READY,
        GOAL_PROPOSAL,
        GOAL_PROPOSAL_2_1_292,
        SETUP_AUTO_MODE_DEFAULT,
        SETUP_CHROME_UPSELL,
        SETUP_REMOTE_CONTROL,
        SESSION_PAUSED,
    ] {
        let r = screen(text);
        let p = parse_prompt_v2(&r).expect("the box");
        let name = p.title.clone();
        assert_eq!(
            p.span.1,
            r.iter().rposition(|x| !x.trim().is_empty()).unwrap(),
            "{name}"
        );
        assert_eq!(prompt_box_first_row(&r), Some(p.span.0), "{name}");

        let mut quoted = rows(&["⏺ Bash(aterm ctl @s-2 text)", "  ⎿  (the worker's screen)"]);
        quoted.extend(r.iter().map(|x| format!("     {x}")));
        assert_eq!(parse_prompt_v2(&quoted), None, "{name} quoted");
        assert_ne!(
            crate::phase::worker_phase(&quoted),
            crate::phase::Phase::Prompt,
            "{name}"
        );

        let mut scrolled = r.clone();
        scrolled.push(String::new());
        scrolled.extend(composer("  ? for shortcuts"));
        assert_eq!(parse_prompt_v2(&scrolled), None, "{name} + composer");
        let mut went_on = r.clone();
        went_on.extend(rows(&["", "⏺ Done."]));
        assert_eq!(parse_prompt_v2(&went_on), None, "{name} + a said row");

        let ruleless: Vec<String> = r
            .iter()
            .filter(|x| !crate::phase::is_rule(x) || x.trim_end().chars().count() < 100)
            .cloned()
            .collect();
        assert_eq!(parse_prompt_v2(&ruleless), None, "{name} without its rule");

        let mut retitled = r.clone();
        retitled[p.span.0] = " Something else entirely".to_string();
        let other = parse_prompt_v2(&retitled).expect("a setup dialog");
        let proceed = crate::anchor!("plan.proceed");
        if text == PLAN_READY {
            assert_eq!(other.kind, PromptKind::PlanExit, "{name} retitled");
            assert_eq!(other.title, proceed, "{name} retitled");
        } else {
            assert_eq!(other.kind, PromptKind::Other, "{name} retitled");
        }
        retitled[p.span.0] = " Bash command".to_string();
        if text == PLAN_READY {
            let plan = parse_prompt_v2(&retitled).expect("the plan");
            assert_eq!(
                (plan.kind, plan.title.as_str()),
                (PromptKind::PlanExit, proceed)
            );
        } else {
            assert_eq!(parse_prompt_v2(&retitled), None, "{name} as Bash");
        }
    }
    let mut r = screen(BOX_FETCH);
    for row in &mut r {
        if row.trim_start().starts_with("2. ") {
            *row = row.replacen("2. ", "5. ", 1);
        }
    }
    assert_eq!(parse_prompt_v2(&r), None);
    let r: Vec<String> = screen(BOX_BASH_SUBAGENT)
        .into_iter()
        .filter(|x| !is_hint_footer(x))
        .collect();
    assert_eq!(parse_prompt_v2(&r), None);
    let tight = rows(&[
        "⏺ Working on it.",
        "",
        "─".repeat(120).as_str(),
        " Claude in Chrome wants to click on github.com",
        " ❯ 1. Allow",
        "   2. Allow all actions on github.com for this session",
        "   3. Deny (esc)",
    ]);
    let p = parse_prompt_v2(&tight).expect("the tight box");
    assert_eq!(p.kind, PromptKind::Browser);
    assert_eq!(p.span, (3, 6));
    assert_eq!(
        p.with_role(Role::Once).map(|o| o.label.as_str()),
        Some("Allow")
    );
    let p = parse_prompt_v2(&screen(PLAN_READY)).expect("the box");
    assert_eq!(p.options.len(), 3);
    assert_eq!(p.options[0].label, "Yes, and use auto mode");
    let p = parse_prompt_v2(&screen(HELD_MESSAGE_2_1_287)).expect("the box");
    assert_eq!(p.kind, PromptKind::HeldMessage);
    assert_eq!(
        p.options
            .iter()
            .map(|o| (o.role, o.label.as_str()))
            .collect::<Vec<_>>(),
        [
            (
                Role::Deny,
                "Deny — drop it and tell the sender it was declined"
            ),
            (Role::Other, "Deliver this message to Claude"),
        ]
    );
    let p = parse_prompt_v2(&screen(BOX_BROWSER)).expect("the box");
    let c = p.cancel.expect("the (esc) option");
    assert_eq!(
        (c.key.as_str(), c.verb.as_str(), c.effect),
        ("Esc", "Deny", CancelEffect::Reject)
    );
}

#[test]
fn a_question_is_never_a_confirmation() {
    for text in [QUESTION_YES_NO, QUESTION_DO_YOU_WANT] {
        let r = screen(text);
        let p = parse_prompt_v2(&r).expect("the dialog");
        assert_eq!(p.kind, PromptKind::Question);
        assert_eq!(p.with_role(Role::Once), None);
        assert!(p.options.iter().all(|o| o.role == Role::Other));
        assert_eq!(p.options[0].label, "Yes");
    }
    let r = screen(QUESTION_DO_YOU_WANT);
    let footer = r
        .iter()
        .rposition(|x| is_hint_footer(x))
        .expect("the footer");
    let chat = r
        .iter()
        .position(|x| x.contains("Chat about this"))
        .expect("chat");
    let other = r
        .iter()
        .position(|x| x.contains("Type something."))
        .expect("other");
    for (what, edit) in [
        (
            "select only",
            vec![(footer, " Enter to select · Esc to cancel")],
        ),
        (
            "navigate only",
            vec![(footer, " ↑/↓ to navigate · Esc to cancel")],
        ),
        (
            "type something only",
            vec![(footer, " Esc to cancel"), (chat, "")],
        ),
        ("chat only", vec![(footer, " Esc to cancel"), (other, "")]),
    ] {
        let mut x = r.clone();
        for (at, row) in edit {
            x[at] = row.to_string();
        }
        let p = parse_prompt_v2(&x).unwrap_or_else(|| panic!("{what}"));
        assert_eq!(p.kind, PromptKind::Question, "{what}");
        assert_eq!(p.with_role(Role::Once), None, "{what}");
    }
    let x: Vec<String> = r
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != chat && *i != other)
        .map(|(i, row)| {
            if i == footer {
                " Esc to cancel · Tab to amend".to_string()
            } else {
                row.replace("☐ Proceed", "Proceed")
            }
        })
        .collect();
    let p = parse_prompt_v2(&x).expect("the box");
    assert_ne!(p.kind, PromptKind::Question);
    assert_eq!(
        p.with_role(Role::Once).map(|o| o.label.as_str()),
        Some("Yes")
    );
}

#[test]
fn only_the_network_box_asks_at_column_three() {
    let r = screen(BOX_NETWORK);
    let p = parse_prompt_v2(&r).expect("the box");
    assert_eq!(
        p.question.map(|q| r[q].trim()),
        Some("Do you want to allow this connection?")
    );
    let mut card = rows(&[
        "",
        "─".repeat(120).as_str(),
        " github - list_issues",
        "",
        "   Do you want to see the issue list?",
        "",
        " ❯ 1. Yes",
        "   2. No",
        "",
        " Esc to cancel · Tab to amend",
    ]);
    let p = parse_prompt_v2(&card).expect("the box");
    assert_eq!(p.kind, PromptKind::Tool);
    assert!(p.tool_card());
    assert_eq!(p.question, None);
    assert_eq!(p.command, "Do you want to see the issue list?");
    card[4] = " Do you want to proceed?".to_string();
    let p = parse_prompt_v2(&card).expect("the box");
    assert_eq!(p.question, Some(4));
    assert_eq!(p.kind, PromptKind::Other);
}

#[test]
fn a_card_mode_tool_box_is_read_by_its_shape() {
    let card = screen(BOX_TOOL_CARD);
    let title = " Create an issue in alabsystems/aterm?";
    for t in [
        title,
        " github - create_issue",
        " github - create_issue · from a subagent",
        " github - create_issue · from the \"triage\" workflow",
    ] {
        let r: Vec<String> = card
            .iter()
            .map(|row| {
                if row == title {
                    t.to_string()
                } else {
                    row.clone()
                }
            })
            .collect();
        let p = parse_prompt_v2(&r).expect("the box");
        assert_eq!((p.kind, p.title.as_str()), (PromptKind::Tool, t.trim()));
        assert!(p.tool_card(), "{t}");
        assert_eq!(p.question, None);
        assert_eq!(p.cancel.map(|c| c.effect), Some(CancelEffect::Reject));
        assert_eq!(crate::phase::worker_phase(&r), crate::phase::Phase::Prompt);
    }
    assert!(!parse_prompt_v2(&screen(BOX_TOOL)).expect("box").tool_card());
    let swap = |from: &str, to: &str| -> Vec<String> {
        assert!(card.iter().any(|r| r == from), "{from}");
        card.iter()
            .map(|r| if r == from { to.to_string() } else { r.clone() })
            .collect()
    };
    let persist = "   2. Yes, and don't ask again for github - create_issue commands in ~/aterm";
    for (name, r) in [
        (
            "no amend",
            swap(" Esc to cancel · Tab to amend", " Esc to cancel"),
        ),
        (
            "a question",
            swap("   labels  bug, harness", " Do you want to proceed?"),
        ),
        (
            "an option y5n never draws",
            swap(persist, "   2. View raw input"),
        ),
        ("two yeses", swap(persist, "   2. Yes")),
        ("no No", swap("   3. No", "   3. Later")),
        (
            "an unknown suffix",
            swap(title, " Create an issue · in the background"),
        ),
    ] {
        let p = parse_prompt_v2(&r).expect("the box");
        assert_eq!(p.kind, PromptKind::Other, "{name}");
        assert!(!p.tool_card(), "{name}");
    }
    let q = parse_prompt_v2(&screen(QUESTION_YES_NO)).expect("the question");
    assert_eq!(q.kind, PromptKind::Question);
}

#[test]
fn a_box_taller_than_the_pane_is_read_with_its_head_off_screen() {
    let tail = |r: &[String], n: usize| r[r.len() - n..].to_vec();
    let incident = screen(BOX_RM_UNRESOLVABLE_WORKFLOW);
    let heredoc = tall_bash_box(62);
    let edit = tall_edit_box(80);
    let mut question = rows(&[
        "     the first option's description",
        "   2. Split the parser",
        "     one module per box kind",
        "   3. Type something.",
        " ────────────────────────────────────────",
        "   4. Chat about this",
        "",
        " Enter to select · ↑/↓ to navigate · Esc to cancel",
    ]);
    let cut = [
        ("incident, 14 rows", tail(&incident, 14), PromptKind::Other),
        ("incident, 17 rows", tail(&incident, 17), PromptKind::Other),
        ("heredoc, 49 rows", tail(&heredoc, 49), PromptKind::Other),
        ("edit, 49 rows", tail(&edit, 49), PromptKind::Other),
        ("edit, from its edge", tail(&edit, 88), PromptKind::Other),
        ("question, cut", question.clone(), PromptKind::Question),
    ];
    for (name, r, kind) in cut {
        let p = parse_prompt_v2(&r).unwrap_or_else(|| panic!("{name}: no box"));
        assert!(p.head_off_screen, "{name}");
        assert_eq!(p.kind, kind, "{name}");
        assert_eq!(p.span.1, r.len() - 1, "{name}");
        assert!(!p.tool_card(), "{name}");
        let reading = crate::reader::read(Some("claude"), &r, None);
        assert_eq!(reading.phase, crate::phase::Phase::Prompt, "{name}");
        assert!(reading.phase_authoritative, "{name}");
        let mut scrolled = r.clone();
        scrolled.extend(composer("  ? for shortcuts"));
        if crate::phase::leading_spaces(&r[0]) != 1 {
            assert_eq!(parse_prompt_v2(&scrolled), None, "{name}");
        }
    }
    for (name, r, kind, title) in [
        (
            "incident",
            incident.clone(),
            PromptKind::Bash,
            "Bash command · from the \"trust-vc-front-end-m1\" workflow",
        ),
        (
            "incident, 18 rows",
            tail(&incident, 18),
            PromptKind::Bash,
            "Bash command · from the \"trust-vc-front-end-m1\" workflow",
        ),
        (
            "heredoc",
            heredoc.clone(),
            PromptKind::Bash,
            "Bash command · from the \"fixtures\" workflow",
        ),
        ("edit", edit.clone(), PromptKind::Edit, "Edit file"),
    ] {
        let p = parse_prompt_v2(&r).unwrap_or_else(|| panic!("{name}: no box"));
        assert!(!p.head_off_screen, "{name}");
        assert_eq!(p.kind, kind, "{name}");
        assert_eq!(p.title, title, "{name}");
    }
    let whole = parse_prompt_v2(&heredoc).expect("the heredoc box");
    assert_eq!(whole.command_rows.len(), 62);
    assert_eq!(
        parse_prompt_v2(&edit).expect("the diff").path.as_deref(),
        Some("notes.txt")
    );
    question.insert(0, "   Which parser layout do you want?".to_string());
    let q = parse_prompt_v2(&question).expect("the question");
    assert_eq!(q.kind, PromptKind::Question);
    assert!(q.options.iter().all(|o| o.role == Role::Other));
}

#[test]
fn a_cut_box_starting_at_its_question_or_an_option_reads_without_panic() {
    let tail = |r: &[String], n: usize| r[r.len() - n..].to_vec();
    let mut question = screen(QUESTION_YES_NO);
    question.resize(49, String::new());
    let incident = screen(BOX_RM_UNRESOLVABLE_WORKFLOW);
    let cuts = [
        (
            "question, last 40 of 49",
            tail(&question, 40),
            PromptKind::Question,
        ),
        (
            "question, from 2. No",
            tail(&question, 41),
            PromptKind::Question,
        ),
        (
            "incident, from the question",
            tail(&incident, 5),
            PromptKind::Other,
        ),
        (
            "incident, from 1. Yes",
            tail(&incident, 4),
            PromptKind::Other,
        ),
    ];
    for (name, r, kind) in cuts {
        let p = parse_prompt_v2(&r).unwrap_or_else(|| panic!("{name}: no box"));
        assert!(p.head_off_screen, "{name}");
        assert_eq!(p.kind, kind, "{name}");
        if kind == PromptKind::Question {
            assert!(p.options.iter().all(|o| o.role == Role::Other), "{name}");
        }
        let reading = crate::reader::read(Some("claude"), &r, None);
        assert!(reading.phase_authoritative, "{name}");
        assert_ne!(reading.phase, crate::phase::Phase::Idle, "{name}");
    }
}

#[test]
fn a_box_cut_at_its_question_is_no_tool_card() {
    let r = rows(&[
        "  ⎿  (tool output)",
        "   Print it",
        "",
        " Do you want to proceed?",
        " ❯ 1. Yes",
        "   2. Yes, and don't ask again for: echo *",
        "   3. No",
        "",
        " Esc to cancel · Tab to amend",
    ]);
    let p = parse_prompt_v2(&r).expect("a box");
    assert_eq!(p.title, "Do you want to proceed?");
    assert_eq!(p.kind, PromptKind::Other);
    assert!(!p.tool_card());
    let mut control = rows(&[&"─".repeat(120), " Bash command", "", "   │ echo \"hello"]);
    control.extend(r.iter().filter(|row| !row.contains('⎿')).cloned());
    assert_eq!(
        parse_prompt_v2(&control).expect("the box").kind,
        PromptKind::Bash
    );
}

#[test]
fn claudes_composer_under_a_table_is_no_footerless_dialog() {
    use crate::phase::{Phase, worker_phase};
    for (composer_row, footer) in [
        ("❯ Try \"refactor <filepath>\"", "  ? for shortcuts"),
        (
            "❯ Try \"refactor <filepath>\"",
            "  ⏵⏵ bypass permissions on (shift+tab to cycle)",
        ),
        ("❯ ", "  ? for shortcuts"),
    ] {
        let r = launch_under_a_table(composer_row, footer);
        assert_eq!(parse_prompt_v2(&r), None, "{r:#?}");
        assert_eq!(worker_phase(&r), Phase::Idle, "{r:#?}");
        let reading = crate::reader::read(Some("claude"), &r, None);
        assert_eq!(reading.phase, Phase::Idle);
        assert!(reading.prompt.is_none());
    }
    let scrolled =
        launch_under_a_table("❯ Try \"refactor <filepath>\"", "  ? for shortcuts").split_off(3);
    let mut tight = scrolled[..3].to_vec();
    tight.push(String::new());
    tight.extend_from_slice(&scrolled[scrolled.len() - 4..]);
    assert_eq!(parse_prompt_v2(&tight), None, "{tight:#?}");
    assert_eq!(worker_phase(&tight), Phase::Idle, "{tight:#?}");
    for draft in [
        &["❯ then add tests"][..],
        &["❯ 1. then add tests", "  2. and docs"],
        &["❯ "],
    ] {
        let r = first_turn_under_a_table(draft);
        assert_eq!(parse_prompt_v2(&r), None, "{r:#?}");
        assert_eq!(worker_phase(&r), Phase::Busy, "{r:#?}");
    }
    let menu = rows(&[
        &"─".repeat(120),
        " Choose a profile",
        "",
        " ❯ 1. dev",
        "   2. prod",
    ]);
    let apart = rows(&[
        &"─".repeat(60),
        " Rust                  100         2000",
        &"─".repeat(60),
        "% ./pick-profile",
        "",
        " ❯ 1. dev",
        "   2. prod",
    ]);
    assert_eq!(parse_prompt_v2(&apart), None, "{apart:#?}");
    let generic = crate::reader::read(None, &menu, None);
    assert!(!generic.phase_authoritative, "{generic:?}");
    assert_ne!(generic.program, crate::reader::Program::Claude);
}

#[test]
fn the_mcp_server_dialogs_are_named_by_their_titles_and_every_answer_is_kept() {
    use crate::phase::Phase;
    let one = screen(MCP_SERVER_ONE);
    let p = parse_prompt_v2(&one).expect("the one-server dialog");
    assert_eq!(p.kind, PromptKind::McpServer);
    assert_eq!(p.kind.name(), "mcp-server");
    assert_eq!(p.title, "New MCP server found in this project: lab-probe");
    assert_eq!(p.command, "lab-probe");
    assert!(
        p.description
            .starts_with("MCP servers may execute code or access system resources."),
        "{}",
        p.description
    );
    let read: Vec<(&str, Role, bool)> = p
        .options
        .iter()
        .map(|o| (o.label.as_str(), o.role, o.focused))
        .collect();
    assert_eq!(
        read,
        [
            ("Use this MCP server", Role::Persist, false),
            (
                "Use this and all future MCP servers in this project",
                Role::Persist,
                false
            ),
            (
                "Continue without using this MCP server",
                Role::Persist,
                true
            ),
        ]
    );
    assert_eq!(p.select, Select::ArrowsEnter);
    assert_eq!(
        p.footer.as_deref(),
        Some("Enter to confirm · Esc to cancel")
    );
    assert!(!p.head_off_screen);
    let v1 = parse_prompt(&one).expect("v1");
    assert_eq!((v1.kind, v1.command.as_str()), (p.kind, "lab-probe"));
    let two = screen(MCP_SERVER_TWO);
    let q = parse_prompt_v2(&two).expect("the dialog for several");
    assert_eq!(
        (q.kind, q.head_off_screen, q.title.as_str()),
        (
            PromptKind::McpServer,
            false,
            "2 new MCP servers found in this project"
        )
    );
    assert!(q.command.is_empty());
    let read: Vec<(&str, Role, bool)> = q
        .options
        .iter()
        .map(|o| (o.label.as_str(), o.role, o.focused))
        .collect();
    assert_eq!(
        read,
        [
            ("[✔] lab-probe-a", Role::Persist, true),
            ("[✔] lab-probe-b", Role::Persist, false),
            ("Enable selected", Role::Persist, false),
        ]
    );
    assert_eq!(q.select, Select::ArrowsEnter);
    assert_eq!(
        q.footer.as_deref(),
        Some("Space to select · Esc to reject all")
    );
    for (name, rows) in [("one", &one), ("two", &two)] {
        let reading = crate::reader::read(Some("claude"), rows, None);
        assert_eq!(reading.phase, Phase::Prompt, "{name}");
        assert!(reading.phase_authoritative, "{name}");
        assert_eq!(
            reading.prompt.map(|p| p.kind),
            Some(PromptKind::McpServer),
            "{name}"
        );
    }
    let asked: Vec<String> = one
        .iter()
        .map(|r| {
            r.replace(
                "Enter to confirm · Esc to cancel",
                "Enter to select · ↑/↓ to navigate · Esc to cancel",
            )
        })
        .collect();
    let p = parse_prompt_v2(&asked).expect("the dialog");
    assert_eq!(p.kind, PromptKind::McpServer);
    assert!(p.question_dialog.is_none());
    assert!(p.options.iter().all(|o| o.role == Role::Persist));
    let retitled: Vec<String> = one
        .iter()
        .map(|r| r.replace("New MCP server found in this project:", "New tool found:"))
        .collect();
    let p = parse_prompt_v2(&retitled).expect("still a box");
    assert_eq!(p.kind, PromptKind::Other);
    assert!(
        p.options.len() == 3 && p.options.iter().all(|o| o.role == Role::Persist),
        "{:?}",
        p.options
    );
    let retitled: Vec<String> = two
        .iter()
        .map(|r| r.replace("new MCP servers found in this project", "new tools found"))
        .collect();
    let rule = two
        .iter()
        .position(|r| r.starts_with('─'))
        .expect("the rule");
    for (name, rows) in [
        ("retitled", retitled),
        ("rule cut", two[rule + 1..].to_vec()),
        ("rule, title and subtitle cut", two[rule + 3..].to_vec()),
    ] {
        let q = parse_prompt_v2(&rows).expect("still a box");
        assert_eq!(
            (q.kind, q.head_off_screen),
            (PromptKind::Other, true),
            "{name}"
        );
        assert_eq!(
            q.options
                .iter()
                .find(|o| o.label == "Enable selected")
                .map(|o| o.role),
            Some(Role::Persist),
            "{name}: {:?}",
            q.options
        );
    }
}

#[test]
fn a_wrapped_unnumbered_option_is_read_as_one() {
    let [one, all, skip, submit] =
        ["mcp.use", "mcp.use_all", "mcp.skip", "mcp.submit"].map(crate::anchors::anchor_text);
    let (a, b) = ("[✔] lab-probe-a", "[✔] lab-probe-b");
    let three: &[&str] = &[one, all, skip];
    let expected: [Option<(PromptKind, &[&str])>; 27] = [
        Some((PromptKind::McpServer, three)),
        Some((PromptKind::McpServer, three)),
        Some((PromptKind::Other, &[skip])),
        Some((PromptKind::Other, &[skip])),
        Some((PromptKind::Other, &[all, skip])),
        Some((PromptKind::Other, &[one, all])),
        Some((PromptKind::Other, three)),
        None,
        Some((PromptKind::McpServer, &[a, b, submit])),
        Some((PromptKind::Other, &[a, submit])),
        Some((PromptKind::Other, &[one, all])),
        Some((PromptKind::Other, &[one, all])),
        Some((PromptKind::Other, three)),
        Some((PromptKind::Other, three)),
        Some((PromptKind::McpServer, three)),
        Some((PromptKind::McpServer, three)),
        Some((PromptKind::McpServer, three)),
        Some((PromptKind::Other, three)),
        Some((PromptKind::McpServer, three)),
        Some((PromptKind::McpServer, three)),
        Some((PromptKind::McpServer, three)),
        Some((PromptKind::McpServer, three)),
        Some((PromptKind::McpServer, three)),
        Some((PromptKind::McpServer, three)),
        Some((PromptKind::Other, &[all, skip])),
        Some((PromptKind::Other, &[all, skip])),
        Some((PromptKind::Other, &[a, b, submit])),
    ];
    for (text, want) in MCP_SERVER_NARROW
        .iter()
        .chain(&MCP_SERVER_DERIVED)
        .zip(expected)
    {
        let rows = screen(text);
        let name = text.lines().next().unwrap_or_default();
        let p = parse_prompt_v2(&rows);
        let read = p.as_ref().map(|p| {
            (
                p.kind,
                p.options
                    .iter()
                    .map(|o| o.label.as_str())
                    .collect::<Vec<_>>(),
            )
        });
        assert_eq!(read, want.map(|(k, l)| (k, l.to_vec())), "{name}");
        for o in p.iter().flat_map(|p| &p.options) {
            assert!(
                o.label.starts_with("[✔]") || o.role == Role::Persist,
                "{name}: {o:?}"
            );
        }
    }
}

#[test]
fn an_unnumbered_label_is_joined_in_the_room_at_either_edge() {
    let rows = screen(MCP_SERVER_NARROW[15]);
    let width = rule_width(&rows);
    assert_eq!(width, Some(54));
    let block = block_from(&rows, "Use this MCP server");
    let read = |width| -> Vec<(String, bool)> {
        unnumbered(&rows, &block, ARROW, width)
            .0
            .into_iter()
            .map(|o| (o.label, o.kept))
            .collect()
    };
    let [one, all, skip] = ["mcp.use", "mcp.use_all", "mcp.skip"].map(crate::anchors::anchor_text);
    let whole = |l: &str| (l.to_string(), true);
    assert_eq!(read(width), [whole(one), whole(all), whole(skip)]);

    let labels = |rows: &[String]| -> Vec<String> {
        parse_prompt_v2(rows)
            .expect("the dialog")
            .options
            .into_iter()
            .map(|o| o.label)
            .collect()
    };
    for (text, (width, head)) in TRUST_NARROW
        .iter()
        .zip([(27, "Yes, I trust this folder"), (20, "Yes, I trust this")])
    {
        let rows = screen(text);
        assert_eq!(rule_width(&rows), Some(width));
        let row = rows
            .iter()
            .find(|r| crate::text::trim(r) == head)
            .expect("the label's first row");
        assert_eq!(crate::question::cells(row), width, "to the edge");
        assert_eq!(labels(&rows), ["No, exit", "Yes, I trust this folder"]);
    }
    for width in 35..=70 {
        assert_eq!(
            labels(&rewrap_box(HELD_MESSAGE_2_1_287, width)),
            [
                "Deny — drop it and tell the sender it was declined",
                "Deliver this message to Claude"
            ],
            "{width}"
        );
    }

    assert_eq!(
        read(Some(56)),
        [
            whole(one),
            whole("Use this and all future MCP servers in this"),
            whole("project"),
            whole(skip)
        ]
    );
}

#[test]
fn inks_wrap_draws_the_measured_rows() {
    let block = |rows: &[String], first: usize, col: usize| -> Vec<String> {
        rows[first..]
            .iter()
            .take_while(|r| !r.trim().is_empty() && crate::phase::leading_spaces(r) == col)
            .map(|r| r.trim().to_string())
            .collect()
    };
    let title = "New MCP server found in this project: lab-probe";
    let body = "MCP servers may execute code or access system resources. All tool calls \
                require approval. Learn more in the MCP documentation.";
    let labels = ["mcp.use", "mcp.use_all", "mcp.skip"].map(crate::anchors::anchor_text);
    for k in [0, 1, 14, 15, 16, 18, 19, 20, 21, 22, 23] {
        let rows = screen(MCP_SERVER_NARROW[k]);
        let rule = rows.iter().position(|r| r.starts_with('─')).expect("rule");
        let width = crate::question::cells(&rows[rule]);
        let room = width - 4;
        let head = block(&rows, rule + 1, 2);
        assert_eq!(head, ink_wrap(title, room), "title at {width}");
        let at = rule + 1 + head.len() + 1;
        let text = block(&rows, at, 2);
        assert_eq!(text, ink_wrap(body, room), "body at {width}");
        let first = at + text.len() + 1;
        let drawn: Vec<String> = rows[first..]
            .iter()
            .take_while(|r| !r.trim().is_empty())
            .map(|r| {
                marked(r, ARROW)
                    .map_or(crate::text::trim(r), |(_, t)| t)
                    .to_string()
            })
            .collect();
        let want: Vec<String> = labels.iter().flat_map(|l| ink_wrap(l, room)).collect();
        assert_eq!(drawn, want, "labels at {width}");
        for line in [title, body].into_iter().chain(labels) {
            assert_eq!(crate::wrap::rows_in(line, room), ink_wrap(line, room).len());
        }
    }
    for text in TRUST_NARROW {
        let rows = screen(text);
        let rule = rows.iter().position(|r| r.starts_with('─')).expect("rule");
        let width = crate::question::cells(&rows[rule]);
        let path = rows
            .iter()
            .position(|r| r.trim_start().starts_with("/private/"))
            .expect("the path");
        assert_eq!(
            block(&rows, path, 1),
            ink_wrap("/private/tmp/at.6AMk/proj", width - 2)
        );
        let quick = rows
            .iter()
            .position(|r| r.trim_start().starts_with("Quick safety"))
            .expect("the body");
        assert_eq!(
            block(&rows, quick, 1),
            ink_wrap(
                "Quick safety check: Is this a project you created or one you trust? \
                 (Like your own code, a well-known open source project, or work from \
                 your team). If not, take a moment to review what's in this folder first.",
                width - 2
            )
        );
        let yes = rows
            .iter()
            .position(|r| r.trim_start().starts_with("Yes, I trust"))
            .expect("the label");
        assert_eq!(
            block(&rows, yes, 3),
            ink_wrap("Yes, I trust this folder", width - 3)
        );
    }
    let picker = screen(MODEL_PICKER_40COL);
    for (number, line) in [
        "Use the default model (currently Opus 5.5) · $4/$20 per Mtok",
        "Opus 5.5 · Best for everyday, complex tasks · $4/$20 per Mtok",
        "Fable 5.1 · Most capable for your hardest and longest-running tasks · $10/$50 per Mtok",
        "Sonnet 5.5 · Efficient for routine tasks · $2/$10 per Mtok",
        "Haiku 5.5 · Fastest for quick answers · $0.10/$0.50 per Mtok ($0.50/$2.50 for \
         prompts over 100k)",
    ]
    .into_iter()
    .enumerate()
    {
        let at = picker
            .iter()
            .position(|r| r.contains(&format!("{}. ", number + 1)))
            .expect("the option");
        let mut drawn = vec![picker[at].chars().skip(28).collect::<String>()];
        drawn.extend(block(&picker, at + 1, 28));
        assert_eq!(drawn, ink_wrap(line, 10), "{line}");
        assert_eq!(crate::wrap::rows_in(line, 10), drawn.len());
    }

    let rows = screen(MCP_SERVER_NARROW[21]);
    assert_eq!(
        block(&rows, 2, 2),
        ["New MCP server found in", "this project:", "lab-probe"]
    );
    assert_eq!(
        greedy(title, 23),
        ["New MCP server found in", "this project: lab-probe"]
    );
    let rows = screen(MCP_SERVER_NARROW[18]);
    let learn = rows
        .iter()
        .position(|r| r.trim() == "Learn more in")
        .expect("row");
    assert_eq!(crate::text::trim(&rows[learn + 1]), "the MCP");
    assert!(greedy(body, 17).iter().any(|r| r == "Learn more in the"));
}

#[test]
fn a_label_row_under_a_full_row_has_a_cell_less() {
    let (deny, deliver) = (
        "Deny — drop it and tell the sender it was declined",
        "Deliver this message to Claude",
    );
    let labels = |rows: &[String]| -> Option<Vec<String>> {
        parse_prompt_v2(rows).map(|p| p.options.into_iter().map(|o| o.label).collect())
    };
    let head_of = |piece: &str, label: &str| {
        label == piece
            || label
                .strip_prefix(piece)
                .is_some_and(|r| r.starts_with(' '))
    };
    for (fixture, width, col) in [(HELD_MESSAGE_2_1_287, 22, 3), (HELD_MESSAGE, 23, 4)] {
        let rows = rewrap_box_ink(fixture, width);
        let at = rows
            .iter()
            .position(|r| r.trim_start().starts_with("❯ Deny"))
            .expect("the Deny label");
        let drawn: Vec<&str> = rows[at..at + 3].iter().map(|r| r.trim_start()).collect();
        assert_eq!(
            drawn,
            ["❯ Deny — drop it and", "tell the sender", "it was declined"]
        );
        assert_eq!(crate::question::cells(&rows[at]), width - 1, "full");
        assert_eq!(crate::phase::leading_spaces(&rows[at + 2]), col);
        assert_eq!(
            labels(&rows),
            Some(vec![deny.to_string(), deliver.to_string()])
        );
        for j in 1..rows.len() {
            let half: Vec<String> = rows
                .iter()
                .enumerate()
                .map(|(i, r)| if i < j { r.clone() } else { String::new() })
                .collect();
            for label in labels(&half).into_iter().flatten() {
                assert!(
                    head_of(&label, deny) || head_of(&label, deliver),
                    "{width} columns, half drawn to {j}: {label:?}"
                );
            }
        }

        let wider: Vec<String> = rows
            .iter()
            .map(|r| match r.chars().last() {
                Some(c @ ('─' | '╌')) => format!("{r}{c}"),
                _ => r.clone(),
            })
            .collect();
        assert_eq!(
            labels(&wider),
            Some(vec![
                "Deny — drop it and tell the sender".to_string(),
                "it was declined".to_string(),
                deliver.to_string()
            ])
        );
    }
}

#[test]
fn a_row_at_or_past_the_room_gives_the_row_under_it_a_lead() {
    for (k, row, lead) in [
        (22, "Use this and all future MCP", 1),
        (20, "future MCP servers in", 1),
        (19, "future MCP servers", 0),
    ] {
        let rows = screen(MCP_SERVER_NARROW[k]);
        let room = rule_width(&rows).expect("the rule") - 5;
        let text = rows
            .iter()
            .map(|r| crate::text::trim(r))
            .find(|t| *t == row)
            .expect("the row");
        assert_eq!(
            crate::wrap::lead(crate::question::cells(text), room),
            lead,
            "{row:?} in {room}"
        );
    }
}

#[test]
fn a_rows_lead_is_carried_to_the_row_under_it() {
    let label = "Yes, and keep allowing any reads outside the working directories";
    let wrapped = ink_wrap(label, 13);
    assert_eq!(
        wrapped,
        [
            "Yes, and keep",
            "allowing any",
            "reads",
            "outside the",
            "working",
            "directories"
        ]
    );
    let rows: Vec<String> = wrapped
        .iter()
        .enumerate()
        .map(|(i, r)| format!("{}{r}", if i == 0 { "  ❯ " } else { "    " }))
        .chain(["    Not now".to_string()])
        .collect();
    let block: Vec<usize> = (0..rows.len()).collect();
    let read = |width| -> Vec<String> {
        unnumbered(&rows, &block, ARROW, Some(width))
            .0
            .into_iter()
            .map(|o| o.label)
            .collect()
    };
    assert_eq!(read(18), [label, "Not now"]);

    assert_eq!(
        read(19),
        [
            "Yes, and keep allowing any reads",
            "outside the working directories",
            "Not now"
        ]
    );
}

#[test]
fn an_option_opening_with_no_capital_under_another_reads_no_role() {
    let frame = |rows: &[&str]| -> Vec<String> { rows.iter().map(|r| r.to_string()).collect() };
    let roles = |rows: &[String]| -> Vec<(String, Role)> {
        parse_prompt_v2(rows)
            .expect("the box")
            .options
            .into_iter()
            .map(|o| (o.label, o.role))
            .collect()
    };
    let other = |labels: &[&str]| -> Vec<(String, Role)> {
        labels
            .iter()
            .map(|l| (l.to_string(), Role::Other))
            .collect()
    };
    let cjk = [
        "⏺ Working on it.",
        "  ⎿  (tool output)",
        "",
        "─────────────────────────────────",
        " Held message from another",
        " session",
        "",
        "  Another Claude session sent a",
        "  message: from @s-1",
        "",
        "  This session is not prompting",
        "  for permissions.",
        "",
        "  ❯ Deny — drop it and tell",
        "    那个 sender it was declined",
        "",
        "",
    ];
    // `🚀` is two cells wide, as the grid draws it: the rule is wide enough for it and its
    // space beside the label's last word a cell short of the edge, so it cannot be its rest.
    let emoji = [
        "⏺ Working on it.",
        "  ⎿  (tool output)",
        "",
        "──────────────────────────────────────────────────────────",
        " Held message from another session",
        "",
        "  Another Claude session sent a message: from @s-1",
        "",
        "  This session is not prompting for permissions.",
        "",
        "  ❯ Deny — drop it and tell the sender it was declined",
        "    🚀",
        "",
    ];
    let (cut, whole) = (
        "Deny — drop it and tell",
        "Deny — drop it and tell the sender it was declined",
    );
    for (rows, deny, rest, capital) in [
        (
            &cjk[..],
            cut,
            "那个 sender it was declined",
            "That sender it was declined",
        ),
        (&emoji[..], whole, "🚀", "Go"),
    ] {
        assert_eq!(roles(&frame(rows)), other(&[deny, rest]));
        let control: Vec<String> = rows.iter().map(|r| r.replace(rest, capital)).collect();
        assert_eq!(
            roles(&control),
            [
                (deny.to_string(), Role::Deny),
                (capital.to_string(), Role::Other)
            ]
        );
    }
    let always = |label: &str| {
        frame(&[
            "⏺ Working on it.",
            "",
            "────────────────────────────────────────",
            " Setting up Claude in Chrome",
            "",
            "  ✔ Install the extension",
            "  ✔ Connect to Chrome",
            "",
            &format!("  ❯ {label}"),
            "    Keep waiting for",
            "    now",
            "",
            "  Enter to confirm · Esc to cancel",
        ])
    };
    assert_eq!(
        roles(&always("Continue with tools, always on")),
        [
            ("Continue with tools, always on".to_string(), Role::Persist),
            ("Keep waiting for".to_string(), Role::Other),
            ("now".to_string(), Role::Other),
        ]
    );
    assert_eq!(
        roles(&always("Continue with tools, on")),
        other(&["Continue with tools, on", "Keep waiting for", "now"])
    );

    let rule = "─".repeat(60);
    let tool = |opts: [&str; 3]| -> Vec<String> {
        let mut rows: Vec<String> = [
            "⏺ Working on it.",
            "",
            &rule,
            " Tool use",
            "",
            "  Slack - send_message Tool: (MCP)",
            "",
            " Do you want to proceed?",
        ]
        .iter()
        .map(|r| r.to_string())
        .collect();
        rows.push(format!(" ❯ {}", opts[0]));
        rows.extend(opts[1..].iter().map(|o| format!("   {o}")));
        rows.extend(["".to_string(), " Esc to cancel · Tab to amend".to_string()]);
        rows
    };
    let session = "Yes, during this session";
    let kept = "Yes, and don't ask again for ls commands in";
    let mode = "Yes, allow all edits during this session (shift+tab)";
    let read = |pairs: [(&str, Role); 3]| -> Vec<(String, Role)> {
        pairs.iter().map(|&(l, r)| (l.to_string(), r)).collect()
    };
    assert_eq!(
        roles(&tool([session, kept, "no"])),
        read([
            (session, Role::Session),
            (kept, Role::Persist),
            ("no", Role::Other)
        ])
    );
    assert_eq!(
        roles(&tool([kept, mode, "no"])),
        read([
            (kept, Role::Persist),
            (mode, Role::ModeSwitch),
            ("no", Role::Other)
        ])
    );
    assert_eq!(
        roles(&tool([session, "Yes", "no"])),
        read([
            (session, Role::Session),
            ("Yes", Role::Other),
            ("no", Role::Other)
        ])
    );
    assert_eq!(
        roles(&tool([session, "Yes", "No"])),
        read([
            (session, Role::Session),
            ("Yes", Role::Once),
            ("No", Role::Deny)
        ])
    );

    for (fixture, width) in [(HELD_MESSAGE_2_1_287, 22), (HELD_MESSAGE, 23)] {
        let rows = rewrap_box_ink(fixture, width);
        let wider: Vec<String> = rows
            .iter()
            .map(|r| match r.chars().last() {
                Some(c @ ('─' | '╌')) => format!("{r}{c}"),
                _ => r.clone(),
            })
            .collect();
        let p = parse_prompt_v2(&wider).expect("the box");
        assert!(p.options.iter().any(|o| o.label == "it was declined"));
        assert!(
            p.options.iter().all(|o| o.role == Role::Other),
            "{:?}",
            p.options
        );
        let mut cut = 0;
        for j in 1..wider.len() {
            let half: Vec<String> = wider
                .iter()
                .enumerate()
                .map(|(i, r)| if i < j { r.clone() } else { String::new() })
                .collect();
            let Some(p) = parse_prompt_v2(&half) else {
                continue;
            };
            if p.options
                .iter()
                .skip(1)
                .any(|o| o.label.starts_with(char::is_lowercase))
            {
                cut += 1;
                assert!(
                    p.options.iter().all(|o| o.role == Role::Other),
                    "{width} columns, half drawn to {j}: {:?}",
                    p.options
                );
            }
        }

        assert!(
            cut > 0,
            "{width} columns: no half-drawn frame shows the rest alone"
        );

        let p = parse_prompt_v2(&rows).expect("the box");
        assert_eq!(p.options[0].role, Role::Deny, "{:?}", p.options);
    }
}

#[test]
fn options_described_beside_their_labels_are_read_by_their_labels() {
    let picker = screen(MODEL_PICKER_40COL);
    let block = block_from(&picker, "❯ 1. Default");
    assert_eq!(beside_column(&picker, &block, 2, ARROW), Some(28));
    let mcp = screen(MCP_SERVER_NARROW[0]);
    let block = block_from(&mcp, "Use this MCP server");
    assert_eq!(beside_column(&mcp, &block, 2, ARROW), None);
    let (with, without) = (
        crate::anchor!("setup.chrome_continue"),
        "Continue without browser tools",
    );
    let expected: [&[(&str, Role)]; 5] = [
        &[(without, Role::Deny)],
        &[("Keep waiting", Role::Other), (without, Role::Deny)],
        &[(with, Role::Persist), (without, Role::Deny)],
        &[
            ("Keep waiting", Role::Other),
            ("Continue without brow…", Role::Other),
        ],
        &[
            ("Continue with browser…", Role::Other),
            ("Continue without brow…", Role::Other),
        ],
    ];
    for ((phase, text), want) in CHROME_SETUP.iter().zip(expected) {
        let p = parse_prompt_v2(&screen(text)).expect("the panel");
        let read: Vec<(&str, Role)> = p
            .options
            .iter()
            .map(|o| (o.label.as_str(), o.role))
            .collect();
        assert_eq!(read, want, "{phase}");
    }
}

fn block_from(rows: &[String], head: &str) -> Vec<usize> {
    let first = rows
        .iter()
        .position(|r| r.trim_start().starts_with(head))
        .expect("the first option");
    (first..rows.len())
        .take_while(|&i| !rows[i].trim().is_empty())
        .collect()
}

#[test]
fn an_answer_kept_for_good_is_read_from_its_rows() {
    let [one, skip] = ["mcp.use", "mcp.skip"].map(crate::anchors::anchor_text);
    let rows = screen(MCP_SERVER_NARROW[13]);
    let lines: Vec<&str> = block_from(&rows, "Use this MCP server")
        .into_iter()
        .map(|i| marked(&rows[i], ARROW).map_or(crate::text::trim(&rows[i]), |(_, t)| t))
        .collect();
    assert_eq!(lines.len(), 5);
    assert_eq!(kept_rows(&lines, &[0, 1]), [true, true]);
    assert_eq!(kept_rows(&lines, &[0, 1, 2, 3, 4]), [true; 5]);
    assert_eq!(kept_rows(&lines[..2], &[0, 1]), [true, true]);
    assert_eq!(kept_rows(&lines[3..4], &[0]), [true]);
    let wide: Vec<String> = screen(MCP_SERVER_NARROW[15])
        .iter()
        .map(|r| {
            if r.starts_with('─') {
                format!("{r}──")
            } else {
                r.replace("New MCP server found in this project:", "New tool found:")
            }
        })
        .collect();
    let p = parse_prompt_v2(&wide).expect("the box");
    assert_eq!(p.kind, PromptKind::Other);
    let read: Vec<(&str, Role)> = p
        .options
        .iter()
        .map(|o| (o.label.as_str(), o.role))
        .collect();
    assert_eq!(
        read,
        [
            (one, Role::Persist),
            ("Use this and all future MCP servers in this", Role::Persist),
            ("project", Role::Persist),
            (skip, Role::Persist)
        ]
    );
    let chrome = parse_prompt_v2(&screen(CHROME_SETUP[2].1)).expect("the panel");
    let read: Vec<(&str, bool, Role)> = chrome
        .options
        .iter()
        .map(|o| (o.label.as_str(), o.kept, o.role))
        .collect();
    assert_eq!(
        read,
        [
            (crate::anchor!("setup.chrome_continue"), true, Role::Persist),
            ("Continue without browser tools", false, Role::Deny)
        ]
    );
    assert_eq!(spelled("[✔] lab-probe-a"), "lab-probe-a");
    assert_eq!(
        kept_rows(&[lines[3], "Keep waiting"], &[0, 1]),
        [false, false]
    );
    assert_eq!(
        kept_rows(&["Keep waiting", "Continue"], &[0, 1]),
        [false, false]
    );
    let two = parse_prompt_v2(&screen(MCP_SERVER_NARROW[9])).expect("the box");
    let kept: Vec<bool> = two.options.iter().map(|o| o.kept).collect();
    assert_eq!(kept, [false, true]);
}

#[test]
fn a_label_cut_short_is_read_so_under_its_selected_mark() {
    let picker = screen(MODEL_PICKER_40COL);
    let label = |n: u8| -> String {
        let (_, label) = picker
            .iter()
            .find_map(|r| option_row(r, ARROW).filter(|(k, _)| *k == n))
            .expect("the option");
        label.split("  ").next().unwrap_or_default().to_string()
    };
    assert_eq!(label(1), "Default (recomme… ✔");
    assert!(cut_short(&label(1)));
    assert_eq!(label(2), "Opus");
    assert!(!cut_short(&label(2)));
    assert!(!cut_short("Opus ✔"));
}

#[test]
fn an_mcp_server_name_keeps_a_literal_position_suffix() {
    for name in ["lab-probe", "srv 3 of 2", "srv 1 of 2"] {
        let rows: Vec<String> = screen(MCP_SERVER_ONE)
            .into_iter()
            .map(|row| row.replace("lab-probe", name))
            .collect();
        let p = parse_prompt_v2(&rows).expect("the complete MCP dialog");
        assert_eq!(p.kind, PromptKind::McpServer, "{name}");
        assert!(!p.head_off_screen, "{name}");
        assert!(
            p.options.len() == 3 && p.options.iter().all(|o| o.role == Role::Persist),
            "{name}: {:?}",
            p.options
        );
        assert_eq!(
            (p.title, p.command),
            (
                format!("New MCP server found in this project: {name}"),
                name.to_string(),
            ),
            "the actual parser retains the full server name: {name}"
        );
    }
}

#[test]
fn the_settings_errors_continue_without_is_its_refusal() {
    let rows = screen(SETTINGS_ERROR);
    let p = parse_prompt_v2(&rows).expect("the dialog");
    assert_eq!(
        (p.kind, p.title.as_str(), p.select),
        (PromptKind::Other, "Settings Error", Select::Digits)
    );
    let read: Vec<(Option<u8>, &str, Role)> = p
        .options
        .iter()
        .map(|o| (o.n, o.label.as_str(), o.role))
        .collect();
    assert_eq!(
        read,
        [
            (Some(1), "Fix with Claude", Role::Other),
            (Some(2), "Exit and fix manually", Role::Other),
            (Some(3), "Continue without these settings", Role::Deny),
        ]
    );
    assert_eq!(
        p.cancel.as_ref().map(|c| (c.verb.as_str(), c.effect)),
        Some(("cancel", CancelEffect::Exit))
    );
    let edits = [
        ("Settings Error", "Settings Warning"),
        (
            "Files with errors are skipped entirely, not just the invalid settings.",
            "The values listed above were skipped; the rest of the file is in effect.",
        ),
        ("1. Fix with Claude", "1. Continue"),
        ("2. Exit and fix manually", "2. Fix with Claude"),
        (
            "3. Continue without these settings",
            "3. Exit and fix manually",
        ),
    ];
    let warning: Vec<String> = rows
        .iter()
        .map(|r| {
            edits
                .iter()
                .fold(r.clone(), |r, (from, to)| r.replace(from, to))
        })
        .collect();
    let p = parse_prompt_v2(&warning).expect("the dialog");
    assert_eq!(p.title, "Settings Warning");
    let labels: Vec<&str> = p.options.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(
        labels,
        ["Continue", "Fix with Claude", "Exit and fix manually"]
    );
    assert_eq!(
        p.cancel.as_ref().map(|c| (c.verb.as_str(), c.effect)),
        Some(("cancel", CancelEffect::Reject))
    );
}

#[test]
fn the_model_switch_confirmation_is_read_by_its_title_under_the_panel_rule() {
    use crate::phase::Phase;
    for (name, text, subject) in [
        ("model", MODEL_SWITCH, "switch to Sonnet 5"),
        ("inline", MODEL_SWITCH_INLINE, "switch to Opus 5.5"),
        ("effort", EFFORT_SWITCH, "switch to high"),
        ("hook", MODEL_SWITCH_HOOK, "switch to Sonnet 5"),
    ] {
        let rows = screen(text);
        let p = parse_prompt_v2(&rows).unwrap_or_else(|| panic!("{name}: no box"));
        assert_eq!(p.kind, PromptKind::ModelSwitch, "{name}");
        assert_eq!(p.options[0].label, format!("Yes, {subject}"), "{name}");
        assert!(p.command.is_empty(), "{name}");
        assert_eq!(p.select, Select::Digits, "{name}");
        let roles: Vec<(Option<u8>, Role)> = p.options.iter().map(|o| (o.n, o.role)).collect();
        assert_eq!(
            roles,
            [(Some(1), Role::Once), (Some(2), Role::Deny)],
            "{name}"
        );
        assert!(p.options[0].focused, "{name}");
        assert!(p.cancel.is_none(), "{name}: it draws no key hints");
        assert!(!p.head_off_screen, "{name}");
        let reading = crate::reader::read(Some("claude"), &rows, None);
        assert_eq!(reading.phase, Phase::Prompt, "{name}");
        assert!(reading.phase_authoritative, "{name}");
    }
    let rows = screen(MODEL_SWITCH);
    let p = parse_prompt_v2(&rows).expect("the measured box");
    assert_eq!(p.title, "Switch model?");
    let rule = rows
        .iter()
        .position(|r| r.starts_with('\u{2594}'))
        .expect("the panel rule");
    let title = rule + 1;
    let mut bare = rows.clone();
    for r in bare.iter_mut().take(rule) {
        r.clear();
    }
    let p = parse_prompt_v2(&bare).expect("the box with the transcript gone");
    assert_eq!(
        (p.kind, p.head_off_screen),
        (PromptKind::ModelSwitch, false)
    );
    let mut toast = rows.clone();
    toast[rule] = format!("{} Update available \u{2594}", "\u{2594}".repeat(100));
    assert_eq!(
        parse_prompt_v2(&toast).map(|p| p.kind),
        Some(PromptKind::ModelSwitch)
    );
    let mut hook = screen(MODEL_SWITCH_HOOK);
    let body = hook
        .iter()
        .position(|r| r.trim().starts_with("Model switches"))
        .expect("the hook's text");
    hook.insert(body + 1, "   Switch model?".to_string());
    hook.insert(body + 2, "   \u{23bf} per the cost dashboard".to_string());
    let p = parse_prompt_v2(&hook).expect("the hook's form with odd text");
    assert_eq!(p.kind, PromptKind::ModelSwitch);
    assert_eq!(hook[p.span.0 - 1].chars().next(), Some('\u{2594}'));
    let mut dash = rows.clone();
    dash[rule] = "\u{2500}".repeat(124);
    let mut col_one = rows.clone();
    col_one[title] = format!(" {}", rows[title].trim());
    let mut said = rows.clone();
    said.insert(title + 1, "\u{23fa} Switching now.".to_string());
    let mut framed = rows.clone();
    framed.extend(composer("  ? for shortcuts"));
    let mut inline_panel = screen(MODEL_SWITCH_INLINE);
    let dash_at = inline_panel
        .iter()
        .rposition(|r| crate::phase::is_rule(r))
        .expect("the inline rule");
    inline_panel[dash_at] = "\u{2594}".repeat(100);
    for (why, r) in [
        ("an inline title under a `▔` rule", inline_panel),
        ("a `─` rule", dash),
        ("the title at column one", col_one),
        ("a said row inside", said),
        ("the composer under it", framed),
    ] {
        assert!(
            parse_prompt_v2(&r).is_none_or(|p| p.kind != PromptKind::ModelSwitch),
            "{why}: {r:#?}"
        );
    }
}

#[test]
fn a_footerless_box_whose_head_is_cut_is_read_with_its_head_off_screen() {
    use crate::phase::Phase;
    let tail = |r: &[String], n: usize| r[r.len() - n..].to_vec();
    for (name, text, kind) in [
        ("network", BOX_NETWORK, PromptKind::NetworkRequest),
        ("browser", BOX_BROWSER, PromptKind::Browser),
        ("held", HELD_MESSAGE, PromptKind::HeldMessage),
        ("goal", GOAL_PROPOSAL, PromptKind::GoalProposal),
        ("plan-enter", PLAN_ENTER, PromptKind::PlanEnter),
        ("setup-auto", SETUP_AUTO_MODE_DEFAULT, PromptKind::Other),
        ("chrome-upsell", SETUP_CHROME_UPSELL, PromptKind::Other),
        ("remote-control", SETUP_REMOTE_CONTROL, PromptKind::Other),
        ("session-paused", SESSION_PAUSED, PromptKind::Other),
    ] {
        let whole = screen(text);
        let rule = whole
            .iter()
            .position(|r| crate::phase::is_rule(r))
            .expect("rule");
        let p = parse_prompt_v2(&whole).unwrap_or_else(|| panic!("{name}: whole"));
        assert_eq!(p.kind, kind, "{name}");
        assert!(!p.head_off_screen, "{name}");
        let block = whole
            .iter()
            .rposition(|r| !r.trim().is_empty())
            .and_then(|last| (0..=last).rev().find(|&i| whole[i].trim().is_empty()))
            .expect("the option block's top")
            + 1;
        let shortest = whole.len() - block;
        let title_first = whole.len() - (rule + 1);
        assert!(shortest < title_first, "{name}");
        for n in shortest..=title_first {
            let r = tail(&whole, n);
            let at = format!("{name}, last {n}");
            let p = parse_prompt_v2(&r).unwrap_or_else(|| panic!("{at}: no box {r:#?}"));
            assert!(p.head_off_screen, "{at}");
            assert_eq!(p.kind, PromptKind::Other, "{at}");
            assert!(!p.tool_card(), "{at}");
            let top = r.iter().position(|row| !row.trim().is_empty());
            assert_eq!(Some(p.span.0), top, "{at}");
            let reading = crate::reader::read(Some("claude"), &r, None);
            assert_eq!(reading.phase, Phase::Prompt, "{at}");
            assert!(reading.phase_authoritative, "{at}");
            for above in ["⏺ Working on it.", "% ls", &"─".repeat(120)] {
                let mut under = vec![above.to_string()];
                under.extend(r.iter().cloned());
                let got = parse_prompt_v2(&under);
                assert!(
                    got.is_none_or(|p| !p.head_off_screen),
                    "{at} under {above:?}"
                );
            }
            let mut framed = composer("  ? for shortcuts");
            framed.extend(r.iter().cloned());
            assert_eq!(parse_prompt_v2(&framed), None, "{at}, composer over it");
        }
        let r = tail(&whole, title_first + 1);
        let p = parse_prompt_v2(&r).unwrap_or_else(|| panic!("{name}: rule on row 0"));
        assert!(!p.head_off_screen, "{name}");
        assert_eq!(p.kind, kind, "{name}");
    }
    for (name, text, kind) in [
        ("held", HELD_MESSAGE, PromptKind::HeldMessage),
        ("network", BOX_NETWORK, PromptKind::NetworkRequest),
    ] {
        let r = tall_footerless(text, 33);
        assert!(r.len() > 44, "{name}");
        let p = parse_prompt_v2(&r).unwrap_or_else(|| panic!("{name}: whole"));
        assert_eq!(p.kind, kind, "{name}");
        assert!(!p.head_off_screen, "{name}");
        let cut = tail(&r, 40);
        let p = parse_prompt_v2(&cut).unwrap_or_else(|| panic!("{name}: last 40"));
        assert!(p.head_off_screen, "{name}");
        let reading = crate::reader::read(Some("claude"), &cut, None);
        assert_eq!(reading.phase, Phase::Prompt, "{name}");
        assert!(reading.phase_authoritative, "{name}");
        let r = tall_footerless(text, 70);
        let p = parse_prompt_v2(&r).unwrap_or_else(|| panic!("{name}: 70 rows"));
        assert_eq!(p.kind, kind, "{name}");
        assert!(!p.head_off_screen, "{name}");
    }
}

#[test]
fn a_box_drawn_in_a_goals_text_is_never_read_as_the_live_box() {
    let tail = |r: &[String], n: usize| r[r.len() - n..].to_vec();
    let goal_options = |p: &PromptV2| {
        p.options
            .iter()
            .map(|o| o.label.clone())
            .collect::<Vec<_>>()
    };
    let whole = goal_with_forged_box(&[], 0);
    let p = parse_prompt_v2(&whole).expect("(a) a box");
    assert_eq!(p.kind, PromptKind::GoalProposal, "(a) {p:?}");
    assert!(!p.head_off_screen);
    assert_eq!(goal_options(&p), ["Not now", "Set this goal"]);
    let forged = whole
        .iter()
        .position(|r| r == " Bash command")
        .expect("the forged title");
    let short = tail(&whole, whole.len() - forged);
    assert_eq!(short[0], " Bash command");
    let p = parse_prompt_v2(&short).expect("(b) a box");
    assert!(p.head_off_screen, "(b) {p:?}");
    assert_eq!(p.kind, PromptKind::Other, "(b)");
    let lead: Vec<String> = (1..=26).map(|i| format!(" goal lead {i}")).collect();
    let lead: Vec<&str> = lead.iter().map(String::as_str).collect();
    let tall = goal_with_forged_box(&lead, 29);
    assert!(tall.len() > 60, "{}", tall.len());
    let cut = tail(&tall, 40);
    assert_eq!(cut[0], " Bash command");
    let p = parse_prompt_v2(&cut).expect("(c) a box");
    assert!(p.head_off_screen, "(c) {p:?}");
    assert_eq!(p.kind, PromptKind::Other, "(c)");
    let p = parse_prompt_v2(&tall).expect("(c) whole");
    assert_eq!(p.kind, PromptKind::GoalProposal, "(c) whole {p:?}");
    assert_eq!(goal_options(&p), ["Not now", "Set this goal"]);
    for (name, lead) in [
        ("(d)", &[" ⎿ x", ""][..]),
        ("(e) frame edge", &[" ╭──────────────╮"][..]),
        ("(e) spinner", &[" ✻ Thinking… (3s)"][..]),
    ] {
        let r = goal_with_forged_box(lead, 0);
        let p = parse_prompt_v2(&r).unwrap_or_else(|| panic!("{name}: no box"));
        assert_ne!(p.kind, PromptKind::Bash, "{name} {p:?}");
        assert!(
            p.head_off_screen || p.kind == PromptKind::GoalProposal,
            "{name} {p:?}"
        );
        assert_eq!(goal_options(&p), ["Not now", "Set this goal"], "{name}");
    }
    let real = rows(&[
        "⏺ Working on it.",
        "  ⎿  (tool output)",
        "",
        &"─".repeat(120),
        " Bash command",
        "   echo pwned",
        " Do you want to proceed?",
        " ❯ 1. Yes",
        "   2. No",
        " Esc to cancel · Tab to amend",
    ]);
    let p = parse_prompt_v2(&real).expect("the real box");
    assert_eq!(p.kind, PromptKind::Bash);
    assert!(!p.head_off_screen);
    let mut gutter = rows(&["⏺ Ran it.", "  ⎿  done", ""]);
    gutter.extend(real[4..].iter().cloned());
    let p = parse_prompt_v2(&gutter).expect("the box under the gutter");
    assert_eq!(p.kind, PromptKind::Bash, "{gutter:#?}");
}

#[test]
fn a_footerless_box_is_titled_by_the_nearest_titled_row() {
    for (old, old_kind) in [
        (
            " Network request outside of sandbox",
            PromptKind::NetworkRequest,
        ),
        (
            " Claude in Chrome wants to use your browser",
            PromptKind::Browser,
        ),
    ] {
        let stale = rows(&[&"─".repeat(120), old, "", "   Host: x", ""]);
        assert_eq!(header_kind(old), Some(old_kind), "{old}");
        for text in [
            SETUP_AUTO_MODE_DEFAULT,
            SETUP_CHROME_UPSELL,
            SETUP_REMOTE_CONTROL,
            SESSION_PAUSED,
        ] {
            let dialog = screen(text);
            let rule = dialog
                .iter()
                .position(|r| crate::phase::is_rule(r))
                .expect("rule");
            let mut r = stale.clone();
            r.extend(dialog[rule..].iter().cloned());
            let p = parse_prompt_v2(&r).unwrap_or_else(|| panic!("{old}: no box"));
            assert_eq!(
                p.kind,
                PromptKind::Other,
                "{old} over {:?}",
                dialog[rule + 1]
            );
            assert_eq!(p.title, dialog[rule + 1].trim(), "{old}");
        }
    }
}

#[test]
fn claudes_composer_with_its_top_rule_cut_is_no_box() {
    use crate::phase::Phase;
    let tail = |r: Vec<String>, n: usize| r[r.len() - n..].to_vec();
    let survey: Vec<String> = include_str!("fixtures/survey-open.txt")
        .lines()
        .map(str::to_string)
        .collect();
    let bg12: Vec<String> = include_str!("fixtures/wait_bg12.out")
        .lines()
        .filter(|l| !l.starts_with("exit="))
        .map(str::to_string)
        .collect();
    let rule = "─".repeat(120);
    for (name, r, phase) in [
        ("survey, last 3", tail(survey, 3), Phase::Idle),
        ("wait_bg12, last 3", tail(bg12, 3), Phase::Busy),
        (
            "one-line draft",
            rows(&["❯ fix the parser yourself", &rule, "  ? for shortcuts"]),
            Phase::Idle,
        ),
        (
            "three-line draft",
            rows(&[
                "❯ line one",
                "  line two",
                "  line three",
                &rule,
                "  ? for shortcuts",
            ]),
            Phase::Idle,
        ),
    ] {
        assert!(r[0].starts_with('❯'), "{name}: {:?}", r[0]);
        assert_eq!(parse_prompt_v2(&r), None, "{name}");
        let reading = crate::reader::read(Some("claude"), &r, None);
        assert!(reading.prompt.is_none(), "{name}: {reading:?}");
        assert_eq!(reading.phase, phase, "{name}");
    }
}

#[test]
fn a_gutter_row_in_a_live_box_body_never_reads_idle() {
    use crate::phase::Phase;
    let rule = "─".repeat(120);
    let tall = |head: &[&str], body: usize, foot: &[&str]| {
        let mut r = rows(head);
        r.iter_mut().for_each(|x| *x = x.replace("RULE", &rule));
        r.extend((0..body).map(|k| format!("   line {k}")));
        r.extend(rows(foot));
        r
    };
    let bash_foot = [
        "   ⏺ Bash(ls)",
        "     ⎿  a.txt",
        "",
        "   ⏺ Done.",
        "   EOF",
        "   Write the transcript fixture",
        "",
        " Do you want to proceed?",
        " ❯ 1. Yes",
        "   2. No",
        "",
        " Esc to cancel · Tab to amend",
    ];
    let bash_head = [
        "⏺ Writing the fixture.",
        "",
        "RULE",
        " Bash command",
        "",
        "   cat > fixture.txt <<'EOF'",
    ];
    let heredoc = tall(&bash_head, 0, &bash_foot);
    let tall_heredoc = tall(&bash_head, 30, &bash_foot);
    let mut col3 = bash_foot.to_vec();
    col3.splice(0..2, ["   ⎿  a.txt"]);
    let heredoc3 = tall(&bash_head, 0, &col3);
    let skill = tall(
        &[
            "⏺ Working on it.",
            "",
            "RULE",
            " Use skill \"deploy\"?",
            " Claude may use instructions, code, or files from this Skill.",
            "",
            "   Deploys the app",
            "   ⎿ see notes",
            "",
            "   more",
        ],
        0,
        &[
            "",
            " Do you want to proceed?",
            " ❯ 1. Yes",
            "   2. Yes, and don't ask again for deploy in ~/aterm",
            "   3. No",
            "",
            " Esc to cancel · Tab to amend",
        ],
    );
    let question = tall(
        &[
            "⏺ One question before I start.",
            "",
            "RULE",
            " ☐ Output",
            "",
            " Which transcript format should the fixture use?",
            "",
            " ❯ 1. Gutter",
            "      ⎿  rows under a tool call",
            "",
            "   2. Plain",
            "   3. Type something.",
        ],
        0,
        &["", " Enter to select · ↑/↓ to navigate · Esc to cancel"],
    );
    let tail40 = |r: &[String]| r[r.len().saturating_sub(40)..].to_vec();
    for (name, r, kind) in [
        ("heredoc", heredoc.clone(), PromptKind::Bash),
        ("heredoc, column 3", heredoc3, PromptKind::Bash),
        ("tall heredoc", tall_heredoc.clone(), PromptKind::Bash),
        (
            "tall heredoc, 40 rows",
            tail40(&tall_heredoc),
            PromptKind::Other,
        ),
        ("skill", skill, PromptKind::Skill),
        ("question", question, PromptKind::Question),
    ] {
        let p = parse_prompt_v2(&r).unwrap_or_else(|| panic!("{name}: no box"));
        assert_eq!(p.kind, kind, "{name}: {p:?}");
        assert_eq!(p.head_off_screen, kind == PromptKind::Other, "{name}");
        assert_eq!(p.span.1, r.len() - 1, "{name}");
        let reading = crate::reader::read(Some("claude"), &r, None);
        assert_eq!(reading.phase, Phase::Prompt, "{name}");
        assert!(reading.phase_authoritative, "{name}");
        assert_eq!(crate::phase::worker_phase(&r), Phase::Prompt, "{name}");
        for n in 5..=r.len() {
            let t = r[r.len() - n..].to_vec();
            let reading = crate::reader::read(Some("claude"), &t, None);
            assert_ne!(reading.phase, Phase::Idle, "{name}, last {n}: {reading:?}");
        }
    }
    let cut = rows(&[
        "  ⎿  a.txt",
        "",
        "   ⏺ Done.",
        "   EOF",
        "",
        " Do you want to proceed?",
        " ❯ 1. Yes",
        "   2. No",
        "",
        " Esc to cancel · Tab to amend",
    ]);
    let p = parse_prompt_v2(&cut).expect("the cut box");
    assert!(p.head_off_screen);
    assert_eq!(p.kind, PromptKind::Other);
    assert_eq!(crate::phase::worker_phase(&cut), Phase::Prompt);
    let mut solid = heredoc.clone();
    let at = solid.iter().position(|x| x.contains('⎿')).unwrap();
    solid.remove(at + 1);
    assert_eq!(
        parse_prompt_v2(&solid).expect("solid").kind,
        PromptKind::Bash
    );
    let mut over = rows(&["⏺ Bash(ls)", "  ⎿  a.txt", ""]);
    over.extend(heredoc[2..].iter().cloned());
    assert_eq!(parse_prompt_v2(&over).expect("over").kind, PromptKind::Bash);
    for (name, mut quoted) in [("cut", cut), ("heredoc body", heredoc[5..].to_vec())] {
        quoted.extend(composer("  ? for shortcuts"));
        assert_eq!(parse_prompt_v2(&quoted), None, "{name}");
    }
}

#[test]
fn a_plan_taller_than_the_tail_is_still_a_plan() {
    let rule = "─".repeat(120);
    let mut plan = rows(&[
        "⏺ Working on it.",
        "",
        &rule,
        " Ready to code?",
        "",
        " Here is Claude's plan:",
        "",
    ]);
    plan.extend((1..=50).map(|k| format!("   {k}. Step {k}")));
    plan.extend(rows(&[
        "",
        &rule,
        " Claude has written up a plan and is ready to execute. Would you like to proceed?",
        "",
        " ❯ 1. Yes, and use auto mode",
        "   2. Yes, manually approve edits",
        "   3. No, keep planning · shift+tab to approve with this feedback",
        "",
        " ctrl+g to edit in VS Code · ~/.claude/plans/x.md",
    ]));
    let whole = parse_prompt_v2(&plan).expect("the plan");
    assert_eq!(whole.kind, PromptKind::PlanExit);
    assert_eq!(whole.title, "Ready to code?");
    let tail = plan[plan.len() - 40..].to_vec();
    assert!(!tail.iter().any(|r| r.contains("Ready to code?")));
    let cut = parse_prompt_v2(&tail).expect("the plan's tail");
    assert_eq!(cut.kind, PromptKind::PlanExit, "{cut:?}");
    assert_eq!(cut.title, crate::anchor!("plan.proceed"));
    assert_eq!(
        crate::reader::read(Some("claude"), &tail, None).phase,
        crate::phase::Phase::Prompt
    );
}

#[test]
fn the_plan_question_and_hint_ink_wrapped_are_one_dialog() {
    let proceed = crate::anchor!("plan.proceed");
    let r = screen(PLAN_APPROVAL_60_2_1_300);
    let p = parse_prompt_v2(&r).expect("the 60-column dialog");
    assert_eq!(p.kind, PromptKind::PlanExit);
    assert_eq!(p.title, "Ready to code?");
    assert_eq!(p.select, Select::Digits);
    let labels: Vec<&str> = p.options.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "Yes, and use auto mode",
            "Yes, manually approve edits",
            "No, keep planning · shift+tab to approve with this feedback"
        ]
    );
    assert_eq!(p.span.1, r.len() - 1, "the hint's last row");
    let read = crate::reader::read(Some("claude"), &r, None);
    assert_eq!(read.phase, crate::phase::Phase::Prompt);
    assert!(read.phase_authoritative);

    let r = screen(PLAN_APPROVAL_TALL_80_2_1_300);
    assert!(!r.iter().any(|x| x.contains("Ready to code?")));
    let p = parse_prompt_v2(&r).expect("the 80-column dialog");
    assert_eq!((p.kind, p.title.as_str()), (PromptKind::PlanExit, proceed));
    assert_eq!(p.options.len(), 3);
    assert_eq!(crate::worker_phase(&r), crate::Phase::Prompt);

    let mut quoted = screen(PLAN_APPROVAL_TALL_80_2_1_300);
    quoted.extend(composer("  ? for shortcuts"));
    assert_eq!(parse_prompt_v2(&quoted), None);
    let mut said = rows(&[
        "⏺ It asks: Claude has written up a plan and is ready to execute. Would you like",
        "  to proceed?",
        "",
    ]);
    said.extend(composer("  ? for shortcuts"));
    assert_eq!(parse_prompt_v2(&said), None);
    let mut shell = screen(PLAN_APPROVAL_60_2_1_300);
    shell.push("% ".to_string());
    assert_eq!(parse_prompt_v2(&shell), None);
    for (fixture, title) in [(PLAN_APPROVAL, proceed), (PLAN_READY, "Ready to code?")] {
        let p = parse_prompt_v2(&screen(fixture)).expect("the plan");
        assert_eq!((p.kind, p.title.as_str()), (PromptKind::PlanExit, title));
    }
    let rule = "─".repeat(140);
    let wide = rows(&[
        "⏺ Working on it.",
        "",
        &rule,
        " Ready to code?",
        "",
        " Here is Claude's plan:",
        "",
        "   1. Do the thing",
        "",
        &rule,
        &format!(" {proceed}"),
        "",
        " ❯ 1. Yes, and use auto mode",
        "   2. Yes, manually approve edits",
        "   3. No, keep planning · shift+tab to approve with this feedback",
        "",
        " ctrl+g to edit in Vim · ~/.claude/plans/staged-sniffing-newell.md",
    ]);
    let p = parse_prompt_v2(&wide).expect("the 140-column dialog");
    assert_eq!(
        (p.kind, p.title.as_str()),
        (PromptKind::PlanExit, "Ready to code?")
    );
}

#[test]
fn a_plan_approvals_label_ink_wrapped_is_its_label() {
    assert_eq!(
        rewrap_box(PLAN_APPROVAL_140_2_1_300, 60),
        screen(PLAN_APPROVAL_60_2_1_300)
    );
    for width in 20..=120 {
        let r = rewrap_box(PLAN_APPROVAL_140_2_1_300, width);
        let p = parse_prompt_v2(&r).unwrap_or_else(|| panic!("{width}: {r:#?}"));
        assert_eq!(p.kind, PromptKind::PlanExit, "{width}");
        let read: Vec<(&str, Role)> = p
            .options
            .iter()
            .map(|o| (o.label.as_str(), o.role))
            .collect();
        assert_eq!(
            read,
            [
                ("Yes, and use auto mode", Role::ModeSwitch),
                ("Yes, manually approve edits", Role::ModeSwitch),
                (
                    "No, keep planning · shift+tab to approve with this feedback",
                    Role::Deny
                )
            ],
            "{width}"
        );
    }
    let p = parse_prompt_v2(&screen(PLAN_APPROVAL)).expect("the plan");
    assert_eq!(p.options[2].label, "Tell Claude what to change");
}

#[test]
fn the_owners_ask_rule_or_hook_is_read_off_the_box() {
    let fetch = screen(BOX_FETCH);
    let fq = fetch
        .iter()
        .position(|r| r.trim().starts_with("Do you want to allow Claude to fetch"))
        .expect("the question");
    let with = |rows: &[String], at: usize, add: &[&str]| -> Vec<String> {
        let mut r = rows[..at].to_vec();
        r.extend(add.iter().map(|x| (*x).to_string()));
        r.extend(rows[at..].iter().cloned());
        r
    };
    let ruled = with(
        &fetch,
        fq,
        &[
            " │ Permission rule WebFetch(domain:docs.rs) requires confirmation for this tool.",
            " /permissions to update rules",
            "",
        ],
    );
    let r = parse_prompt_v2(&ruled)
        .and_then(|p| p.owner_review())
        .expect("the owner's review");
    assert_eq!(r.kind, ReviewKind::Rule);
    let hooked = with(
        &fetch,
        fq,
        &[
            " Hook PreToolUse:WebFetch requires confirmation for this tool.",
            "",
        ],
    );
    let r = parse_prompt_v2(&hooked)
        .and_then(|p| p.owner_review())
        .expect("the owner's review");
    assert_eq!(r.kind, ReviewKind::Hook);

    for (name, text) in [
        ("bash", BOX_BASH_TOUCH),
        ("fetch", BOX_FETCH),
        ("edit", BOX_EDIT),
        ("skill", BOX_SKILL),
        ("rm auto-deny", BOX_RM_AUTO_DENY),
        ("rm auto-deny 2.1.301", BOX_RM_AUTO_DENY_2_1_301),
        ("rm auto-block 2.1.316", BOX_RM_AUTO_BLOCK_2_1_316),
        ("question", QUESTION_TABS_INCIDENT),
        ("review", QUESTION_REVIEW),
        ("bash 2.1.287", BOX_BASH_2_1_287),
        ("read 2.1.287", BOX_BASH_READ_2_1_287),
        ("multiline", BOX_BASH_MULTILINE_2_1_287),
        ("rm breaker 2.1.287", BOX_RM_BREAKER_2_1_287),
        ("classifier fallback", BOX_CLASSIFIER_FALLBACK_2_1_287),
        ("fetch 2.1.287", BOX_FETCH_2_1_287),
        ("write 2.1.287", BOX_WRITE_CREATE_2_1_287),
        ("write under a hook", BOX_WRITE_HOOK_ASK_2_1_287),
    ] {
        let p = parse_prompt_v2(&screen(text)).expect("the box");
        assert_eq!(p.owner_review(), None, "{name}: {p:?}");
    }

    let mut stacked = screen(BOX_CLASSIFIER_FALLBACK_2_1_287);
    let blank = stacked
        .iter()
        .rposition(|r| r.trim() == "Do you want to proceed?")
        .expect("the question")
        - 1;
    assert!(stacked[blank].trim().is_empty());
    stacked.splice(
        blank..blank,
        [
            " Permission rule Bash(touch:*) requires confirmation for this command.",
            " /permissions to update rules",
        ]
        .map(str::to_string),
    );
    let r = parse_prompt_v2(&stacked)
        .and_then(|p| p.owner_review())
        .expect("the owner's review behind the classifier's");
    assert_eq!(r.kind, ReviewKind::Rule);
}

#[test]
fn a_bracketed_box_is_read_by_its_dashes_not_by_a_column() {
    let bash = |text: &str| {
        let p = parse_prompt_v2(&screen(text)).expect("the box");
        assert_eq!(p.kind, PromptKind::Bash);
        assert_eq!(crate::worker_phase(&screen(text)), crate::Phase::Prompt);
        assert!(p.notes.iter().all(|n| !n.contains('╌')), "{:?}", p.notes);
        p
    };
    for (text, command, description, gutter) in [
        (BOX_BASH_2_1_287, &["rm -rf ./lab-doomed"][..], "ls", false),
        (
            BOX_BASH_READ_2_1_287,
            &["wc -l /etc/hosts"],
            "Count the lines of the hosts file",
            false,
        ),
        (
            BOX_BASH_MULTILINE_2_1_287,
            &["touch lab-m1.txt", "touch lab-m2.txt", "# a comment line"],
            "Create two witness files",
            true,
        ),
        (
            BOX_BASH_ASK_RULE_2_1_287,
            &["touch lab-witness-b.txt"],
            "Create a witness file",
            false,
        ),
        (
            BOX_BASH_HOOK_ASK_2_1_287,
            &["touch lab-witness-c.txt"],
            "Create a witness file",
            false,
        ),
        (
            BOX_CLASSIFIER_FALLBACK_2_1_287,
            &["python3 -c \"print(2)\""],
            "Print two",
            false,
        ),
        (
            BOX_BASH_BACKGROUND_AGENT_2_1_292,
            &["touch sub-x"],
            "Create sub-x",
            false,
        ),
    ] {
        let p = bash(text);
        assert_eq!(p.command_rows, command, "{text:.60}");
        assert_eq!(p.description, description);
        assert_eq!((p.description_rows, p.gutter), (0, gutter));
        assert_eq!(p.readings()[0], command.join(" "));
    }
    let note = "ctrl+x ctrl+k twice to stop background agents";
    let mut footers = vec![
        format!(" Esc to cancel · Tab to amend · {note}"),
        format!(
            " Esc to cancel · Tab to amend · {}",
            note.replace("twice", "again")
        ),
        format!(" Esc to cancel · {note}"),
    ];
    let whole = format!(" Esc to cancel · Tab to amend · {note}");
    footers.extend(
        (31..whole.chars().count())
            .map(|n| format!("{}…", whole.chars().take(n).collect::<String>())),
    );
    for footer in &footers {
        let mut rows = screen(BOX_BASH_BACKGROUND_AGENT_2_1_292);
        *rows.last_mut().expect("the footer") = footer.clone();
        let p = bash(&rows.join("\n"));
        assert_eq!(
            crate::worker_phase(&rows),
            crate::Phase::Prompt,
            "{footer:?}"
        );
        assert_eq!(p.command_rows, ["touch sub-x"], "{footer:?}");
        assert_eq!(
            p.cancel.as_ref().map(|c| c.effect),
            Some(CancelEffect::Reject),
            "{footer:?}"
        );
    }
    let p = bash(BOX_BASH_BACKGROUND_AGENT_STACKED_2_1_292);
    assert_eq!(p.command_rows, ["touch sub-a"]);
    assert_eq!(
        p.options.iter().map(|o| o.role).collect::<Vec<_>>(),
        [Role::Once, Role::Persist, Role::ModeSwitch, Role::Deny]
    );
    assert_eq!(p.base_title().as_deref(), Some("Bash command"));
    for not in [
        format!(" {note}"),
        " Esc to cancel · whatever the vendor draws · ctrl+x ctrl+k twice to stop background agents"
            .to_string(),
        " Esc to cancel · Tab to amend · ctrl+x ctrl+k ctrl+j ctrl+l ctrl+o twice to stop background agents"
            .to_string(),
    ] {
        assert!(!is_hint_footer(&not), "{not:?}");
    }
    let p = bash(BOX_BASH_ASK_RULE_2_1_287);
    assert_eq!(
        p.notes,
        [
            "Permission rule Bash(touch:*) requires confirmation for this command. /permissions to update rules"
        ]
    );
    let p = bash(BOX_RM_BREAKER_2_1_287);
    assert!(p.command_rows[0].starts_with("S=/private/tmp/at-nonexistent-lab;"));
    assert_eq!(p.notes.len(), 1, "{:?}", p.notes);
    assert!(p.rm_breaker().is_some(), "{:?}", p.notes);

    for text in [BOX_FETCH_2_1_287, BOX_FETCH_ASK_RULE_2_1_287] {
        let r = screen(text);
        assert_eq!(crate::worker_phase(&r), crate::Phase::Prompt);
        let p = parse_prompt_v2(&r).expect("the fetch box");
        assert_eq!(p.kind, PromptKind::Fetch);
        assert!(
            p.command.starts_with("url: http://127.0.0.1:"),
            "{}",
            p.command
        );
        assert_eq!(p.select, Select::Digits);
        assert_eq!(
            p.options.iter().map(|o| o.role).collect::<Vec<_>>(),
            [Role::Once, Role::Persist, Role::Deny]
        );
        assert_eq!(p.cancel.map(|c| c.effect), Some(CancelEffect::Reject));
    }

    let mut forged = screen(BOX_BASH_2_1_287);
    let at = forged
        .iter()
        .position(|r| r.trim() == "rm -rf ./lab-doomed")
        .expect("the command");
    forged.insert(at, format!(" {}", "╌".repeat(40)));
    let p = bash_of(&forged);
    assert_eq!(p.command_rows.len(), 2, "{:?}", p.command_rows);
    assert!(
        p.readings()
            .iter()
            .all(|r| r.contains("rm -rf ./lab-doomed"))
    );
}

#[test]
fn a_read_boxs_call_names_its_path() {
    for (text, call) in [
        (BOX_READ_OUTSIDE_2_1_292, " Read(/etc/hosts)"),
        (BOX_READ_OUTSIDE_2_1_284, "  Read(/etc/hosts)"),
    ] {
        let p = parse_prompt_v2(&screen(text)).expect("the box");
        assert_eq!(p.kind, PromptKind::Read);
        assert_eq!(p.path.as_deref(), Some("/etc/hosts"), "{call}");
        assert_eq!(p.owner_review(), None, "{p:?}");
        assert_eq!(
            p.options.iter().map(|o| o.role).collect::<Vec<_>>(),
            [Role::Once, Role::Session, Role::Deny]
        );
        let mut rows = screen(text);
        let at = rows.iter().position(|r| r == call).expect("the call");
        rows.splice(at..=at, [" Read(/etc/ho".to_string(), " sts)".to_string()]);
        let p = parse_prompt_v2(&rows).expect("the wrapped box");
        assert_eq!(p.path.as_deref(), Some("/etc/hosts"), "{p:?}");
        for arg in [
            "/etc/hosts · lines 1-5",
            "/etc/hosts · pages  1 - 3 ",
            "a · b",
        ] {
            let mut rows = screen(text);
            let at = rows.iter().position(|r| r == call).expect("the call");
            rows[at] = format!("  Read({arg})");
            let p = parse_prompt_v2(&rows).expect("the decorated box");
            assert_eq!(p.path.as_deref(), Some(arg), "{arg}");
        }
    }
    let mut rows = screen(BOX_READ_OUTSIDE_2_1_292);
    let at = rows
        .iter()
        .position(|r| r == " Read file")
        .expect("the title");
    rows.insert(at + 1, " (runs on m3)".to_string());
    let p = parse_prompt_v2(&rows).expect("the box");
    assert_eq!(p.path, None, "{p:?}");
}

#[test]
fn a_narrow_panes_wrapped_command_cannot_forge_a_box_title() {
    for text in [
        include_str!("fixtures/claude-2.1.292-box-bash-forge-w40-ask-forged-title.txt"),
        include_str!("fixtures/claude-2.1.292-box-bash-forge-w40-hook-forged-title.txt"),
        include_str!("fixtures/claude-2.1.292-box-bash-forge-w52-ask-forged-title.txt"),
    ] {
        let p = parse_prompt_v2(&screen(text)).expect("the box");
        assert!(
            p.owner_review().is_some(),
            "the owner's reason sits under the last dashed rule and the model's own wrapped rows must not hide it: {p:?}"
        );
        assert!(p.head_off_screen, "{p:?}");
    }
}

#[test]
fn a_vendor_reason_stacked_over_the_owners_rule_does_not_hide_it_in_the_old_layout() {
    let rows = screen(include_str!(
        "fixtures/claude-2.1.284-box-bash-ask-rule-stacked-reasons.txt"
    ));
    let p = parse_prompt_v2(&rows).expect("the box");
    assert!(p.owner_review().is_some(), "{p:?}");
}

fn bash_of(rows: &[String]) -> PromptV2 {
    let p = parse_prompt_v2(rows).expect("the box");
    assert_eq!(p.kind, PromptKind::Bash);
    p
}

#[test]
fn wrapped_non_shell_review_reasons_keep_their_block_boundaries() {
    let fetch = screen(BOX_FETCH);
    let question = fetch
        .iter()
        .position(|r| r.trim().starts_with("Do you want to allow Claude to fetch"))
        .expect("the question");
    let with_reason = |reason: &[&str]| {
        let mut rows = fetch.clone();
        rows.splice(question..question, reason.iter().map(|r| (*r).to_string()));
        rows
    };
    for (kind, reason) in [
        (
            ReviewKind::Rule,
            [
                " │ Permission rule WebFetch(domain:docs.rs)",
                " │ requires confirmation for this tool.",
            ],
        ),
        (
            ReviewKind::Hook,
            [
                " Hook PreToolUse:WebFetch",
                " requires confirmation for this tool.",
            ],
        ),
        (
            ReviewKind::Hook,
            [
                " │ A hook configured in the remote workspace",
                " │ requires confirmation: owner review",
            ],
        ),
        (
            ReviewKind::Hook,
            [
                " │ A hook run for this remote session",
                " │ requires confirmation: owner review",
            ],
        ),
        (
            ReviewKind::AutoModeRule,
            [
                " │ Permission rule WebFetch(domain:docs.rs) overrides auto",
                " │ mode for this tool.",
            ],
        ),
    ] {
        let p = parse_prompt_v2(&with_reason(&reason)).expect("the fetch box");
        let review = p.owner_review().expect("the wrapped owner review");
        assert_eq!(review.kind, kind);
        assert_eq!(review.text, reason.map(unbarred).join(" "));
    }

    for reason in [
        vec![
            " │ Permission rule WebFetch(domain:docs.rs)",
            "",
            " │ requires confirmation for this tool.",
        ],
        vec![
            " │ Permission rule WebFetch(domain:docs.rs)",
            " │",
            " │ requires confirmation for this tool.",
        ],
        vec![
            " │ Permission rule WebFetch(domain:docs.rs)",
            "   quoted body text",
            " │ requires confirmation for this tool.",
        ],
        vec![
            "   Permission rule WebFetch(domain:docs.rs)",
            "   requires confirmation for this tool.",
        ],
    ] {
        let p = parse_prompt_v2(&with_reason(&reason)).expect("the fetch box");
        assert_eq!(
            p.owner_review(),
            None,
            "separate or quoted rows: {reason:?}"
        );
    }
    let mut quoted = with_reason(&[
        " │ Permission rule WebFetch(domain:docs.rs)",
        " │ requires confirmation for this tool.",
    ]);
    quoted.extend(composer("  ? for shortcuts"));
    assert_eq!(
        parse_prompt_v2(&quoted),
        None,
        "a transcript is no live box"
    );
}

#[test]
fn the_plan_approval_is_a_box_without_a_footer() {
    let r = screen(PLAN_APPROVAL);
    assert_eq!(crate::worker_phase(&r), crate::Phase::Prompt);
    let p = parse_prompt_v2(&r).expect("the plan approval");
    assert_eq!(p.kind, PromptKind::PlanExit);
    assert!(
        p.title.ends_with("Would you like to proceed?"),
        "{}",
        p.title
    );
    let labels: Vec<&str> = p.options.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "Yes, auto-accept edits",
            "Yes, manually approve edits",
            "Tell Claude what to change"
        ]
    );
    assert!(p.options[0].affirms() && !p.options[2].affirms());

    for dialog in [ASK_SUBMIT, PLAN_APPROVAL] {
        let mut quoted = screen(dialog);
        quoted.extend(composer("  ? for shortcuts"));
        assert_eq!(parse_prompt_v2(&quoted), None, "a composer under it");
        let mut said = screen(dialog);
        said.push("⏺ Done.".to_string());
        assert_eq!(parse_prompt_v2(&said), None, "words under it");
    }
}
