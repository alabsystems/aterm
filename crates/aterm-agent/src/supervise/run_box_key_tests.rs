// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The review key of a box (`box_key`) and the one ask a box gets however the vendor redraws it,
// with negative controls. Included into `run_tests.rs`'s `mod engine::box_key`.

use aterm_phase::prompt::fixtures as f;

/// The measured rm box (Claude Code 2.1.283, 80x24) before the supervisor's `1`, and 5 ms
/// after it was taken: its countdown and one blank row wiped, the transcript reflowed above
/// it, the box still drawn.
const BEFORE: &str = include_str!("policy/fixtures/cap-taken-box-1-before.txt");
const REDRAWN: &str = include_str!("policy/fixtures/cap-taken-box-2-redrawn.txt");

/// `rows` as the point of a look: a box up.
fn prompt_turn(rows: Vec<String>) -> Turn {
    Turn {
        phase: Phase::Prompt,
        screen: Screen {
            rows,
            ..Screen::default()
        },
        timed_out: false,
    }
}

/// The review key of the box on `rows`, read by Claude Code's grammar.
fn claude_key(rows: &[String]) -> String {
    point_key(&aterm_phase::ClaudeReader, &prompt_turn(rows.to_vec()))
}

/// `rows` with every row equal to `from` replaced by the rows `to` — each
/// replacement asserted to have happened once.
fn swap_rows(rows: &[String], edits: &[(&str, &[&str])]) -> Vec<String> {
    let mut out = rows.to_vec();
    for (from, to) in edits {
        let at: Vec<usize> = out
            .iter()
            .enumerate()
            .filter(|(_, r)| r.as_str() == *from)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(at.len(), 1, "the row {from:?} once: {out:#?}");
        out.splice(at[0]..=at[0], to.iter().map(|r| (*r).to_string()));
    }
    out
}

/// [`BEFORE`] re-wrapped at another width, synthesized (no capture of one box at two widths
/// exists): its command row broken mid-token and at a space, its note and countdown broken
/// elsewhere, the transcript above re-wrapped, and `Listed 1 directory` scrolled in at the top.
fn rewrapped() -> Vec<String> {
    let mut out = rows(&["", "  Listed 1 directory", ""]);
    out.extend(swap_rows(
        &f::screen(BEFORE),
        &[
            (
                "⏺ Both hold only an empty keep/ subdirectory, with no files, so nothing is lost.",
                &[
                    "⏺ Both hold only an empty keep/ subdirectory, with no files, so",
                    "  nothing is lost.",
                ],
            ),
            (
                "  ⎿  $ S=/tmp/ahpP.PYJV/scratch && for p in a z; do set -- $p; rm -rf $S/$1;",
                &["  ⎿  $ S=/tmp/ahpP.PYJV/scratch && for p in a z; do set --"],
            ),
            ("     done", &["     $p; rm -rf $S/$1; done"]),
            (
                "   S=/tmp/ahpP.PYJV/scratch && for p in a z; do set -- $p; rm -rf $S/$1; done",
                &[
                    "   S=/tmp/ahpP.PY",
                    "   JV/scratch && for p in a z; do set --",
                    "   $p; rm -rf $S/$1; done",
                ],
            ),
            (
                " │ Dangerous rm operation on possibly-empty variable path: $S/$1 in `rm -rf",
                &[" │ Dangerous rm operation on possibly-empty variable path: $S/$1 in `rm -rf $S/$1`"],
            ),
            (
                " │ $S/$1` (bind $1 and rewrite its $S as \"${S:?}\" or use a literal path)",
                &[" │ (bind $1 and rewrite its $S as \"${S:?}\" or use a literal path)"],
            ),
            (
                " ⚠ Claude Code will automatically deny this request in 1:59, to avoid blocking",
                &[" ⚠ Claude Code will automatically deny this request in 1:59, to avoid"],
            ),
            (
                " progress on an unattended session",
                &[" blocking progress on an unattended session"],
            ),
        ],
    ));
    out
}

/// The measured 2.1.281 rm box with its auto-deny countdown at `left`.
fn ticking(left: &str) -> Vec<String> {
    f::screen(f::BOX_RM_AUTO_DENY)
        .into_iter()
        .map(|r| r.replace("request in 1:59,", &format!("request in {left},")))
        .collect()
}

/// [`ticking`] in a grid narrower than the countdown: its tail wrapped onto
/// a row of its own.
fn ticking_wrapped(left: &str) -> Vec<String> {
    let mut r = ticking(left);
    let i = r
        .iter()
        .position(|x| x.contains("automatically deny"))
        .expect("the countdown row");
    r[i] = r[i].replace(" progress on an unattended session", "");
    r.insert(i + 1, " progress on an unattended session".to_string());
    r
}

/// The measured Edit box (2.1.280) with the `❯` on option `n`.
fn edit_focused(n: u8) -> Vec<String> {
    let rows = f::screen(f::BOX_EDIT);
    rows.into_iter()
        .map(|r| {
            let t = r.trim_start_matches([' ', '❯']);
            if t.starts_with(&format!("{n}. ")) {
                format!(" ❯ {t}")
            } else if t.starts_with(|c: char| c.is_ascii_digit()) && t.contains(". ") {
                format!("   {t}")
            } else {
                r
            }
        })
        .collect()
}

/// The measured Edit box on `notes.txt` with another diff: the same path and question, another
/// hunk.
fn edit_other_diff() -> Vec<String> {
    swap_rows(
        &f::screen(f::BOX_EDIT),
        &[(" 1 -seed", &[" 4 -stem"]), (" 1 +sprout", &[" 4 +stalk"])],
    )
}

/// Every box handed over (`approve = "none"`): each box a look finds is an
/// `EVENT prompt`, a badge and an ask.
fn handing() -> SuperviseOpts {
    SuperviseOpts {
        policy: SupervisorConfig {
            approve: Approve::None,
            ..auto(30, None).policy
        },
        ..auto(30, None)
    }
}

/// What one watch asked the manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Asked {
    /// `EVENT prompt` lines printed.
    prompts: usize,
    /// `meta set attention owner=supervisor …` requests (badges raised).
    sets: usize,
    /// `post to=@s-9 kind=ask --wait=0 …` requests (asks posted).
    posts: usize,
    /// The journal's `escalated` and `cleared` records.
    escalated: usize,
    cleared: usize,
}

impl Asked {
    /// `n` prompts, badges and asks; `n` escalations; `cleared` closes.
    fn each(n: usize, cleared: usize) -> Self {
        Self {
            prompts: n,
            sets: n,
            posts: n,
            escalated: n,
            cleared,
        }
    }
}

/// One watch over `m` under `opts`, the manager `@s-9` (the fabric
/// connected, the mock's default) and a journal of its own, `setup` run on
/// the session first: what it asked, and the lines it printed.
fn watched(
    tag: &str,
    m: &mut Mock,
    opts: &SuperviseOpts,
    setup: impl FnOnce(&mut Session<'_, Mock>),
) -> (Asked, Vec<String>) {
    let (dir, path) = journal_file(tag);
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..opts.clone()
    };
    let (lines, _) = watch_lines_with(m, &opts, |s| {
        s.set_manager(Some("@s-9".to_string()));
        setup(s);
    });
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    let kind = |k: &str| records.iter().filter(|r| r.kind == k).count();
    let asked = Asked {
        prompts: lines
            .iter()
            .filter(|l| l.starts_with("EVENT prompt "))
            .count(),
        sets: count(m, "meta set attention owner=supervisor "),
        posts: count(m, "post to=@s-9 kind=ask --wait=0 "),
        escalated: kind("escalated"),
        cleared: kind("cleared"),
    };
    (asked, lines)
}

/// [`watched`] over a fresh mock of `script` (it ends once the last screen
/// has stood two waits).
fn asked(tag: &str, script: Vec<Vec<String>>, opts: &SuperviseOpts) -> (Asked, Mock) {
    let mut m = Mock::new(true, script);
    m.vanish_after = Some(2);
    let (a, _) = watched(tag, &mut m, opts, |_| {});
    (a, m)
}

// ---- the key, pure ---------------------------------------------------------

/// The box key holds across every way the vendor redraws one box, each pair first proven a real
/// change of the rows: (a) the measured redraw; (b) a re-wrap at another width (synthesized);
/// (c) a ticking countdown, wrapped or not; (d) the Edit box's focus moved; (e) the trust
/// dialog's focus moved; (f) a hand-built tool box with a ticking countdown row.
#[test]
fn the_box_key_holds_across_a_redraw_a_rewrap_a_reflow_a_focus_move_and_a_ticking_countdown() {
    let same = |label: &str, a: &[String], b: &[String]| {
        assert_ne!(a, b, "{label}: the rows differ");
        let (ka, kb) = (claude_key(a), claude_key(b));
        assert!(ka.contains(" whole\n"), "{label}: read whole: {ka}");
        assert_eq!(ka, kb, "{label}");
    };
    let before = f::screen(BEFORE);
    same("(a) the measured redraw", &before, &f::screen(REDRAWN));
    let wrapped = rewrapped();
    let p = aterm_phase::parse_prompt_v2(&wrapped).expect("the re-wrapped box");
    assert_eq!(p.kind, PromptKind::Bash);
    assert!(p.auto_deny.is_some(), "its countdown read: {p:?}");
    same("(b) re-wrapped (synthesized)", &before, &wrapped);
    same("(b) re-wrapped vs redrawn", &f::screen(REDRAWN), &wrapped);
    same("(c) 1:59 -> 1:58", &ticking("1:59"), &ticking("1:58"));
    same("(c) 1:58 -> 1:57", &ticking("1:58"), &ticking("1:57"));
    same("(c) wrapped", &ticking("1:59"), &ticking_wrapped("1:56"));
    same(
        "(d) the edit box's focus",
        &f::screen(f::BOX_EDIT),
        &edit_focused(3),
    );
    let trust = f::screen(f::TRUST);
    let moved = swap_rows(
        &trust,
        &[
            (" ❯ No, exit", &["   No, exit"]),
            (
                "   Yes, I trust this folder",
                &[" ❯ Yes, I trust this folder"],
            ),
        ],
    );
    same("(e) the trust dialog's focus", &trust, &moved);
    let tool = |left: &str| {
        swap_rows(
            &f::screen(f::BOX_TOOL),
            &[(
                " Do you want to proceed?",
                &[
                    &format!(
                        " ⚠ Claude Code will automatically deny this request in {left}, to \
                         avoid blocking progress on an unattended session"
                    ),
                    " Do you want to proceed?",
                ],
            )],
        )
    };
    let p = aterm_phase::parse_prompt_v2(&tool("1:59")).expect("the tool box");
    assert!(
        p.auto_deny.is_some(),
        "the hand-built countdown is read: {p:?}"
    );
    same("(f) a tool box ticking", &tool("1:59"), &tool("1:58"));
}

/// Negative controls: the key changes for another box however alike (another diff on one path,
/// a Write title, another command, note target, description, grant or url).
#[test]
fn the_box_key_changes_for_another_box() {
    let differ = |label: &str, a: &[String], b: &[String]| {
        assert!(
            aterm_phase::parse_prompt_v2(b).is_some(),
            "{label}: the other box is read"
        );
        assert_ne!(claude_key(a), claude_key(b), "{label}");
    };
    let edit = f::screen(f::BOX_EDIT);
    differ("another diff, one path", &edit, &edit_other_diff());
    differ(
        "a write, not an edit",
        &edit,
        &swap_rows(&edit, &[(" Edit file", &[" Write file"])]),
    );
    differ("another command", &rm_box("rm -rf a"), &rm_box("rm -rf b"));
    let rm = f::screen(f::BOX_RM);
    let note = rm
        .iter()
        .find(|r| r.contains("Dangerous rm operation"))
        .expect("the note row")
        .clone();
    differ(
        "the note's target",
        &rm,
        &swap_rows(&rm, &[(&note, &[&note.replacen("$S/$1", "$T/$1", 1)])]),
    );
    let touch = f::screen(f::BOX_BASH_TOUCH);
    let described = touch
        .iter()
        .find(|r| r.trim() == "Create file x")
        .expect("the description row")
        .clone();
    differ(
        "another description",
        &touch,
        &swap_rows(
            &touch,
            &[(&described, &[&described.replace("Create", "Make")])],
        ),
    );
    let tool = f::screen(f::BOX_TOOL);
    differ(
        "another grant",
        &tool,
        &swap_rows(
            &tool,
            &[(
                "   2. Yes, and don't ask again for github - create_issue commands in ~/aterm",
                &["   2. Yes, and don't ask again for github - create_issue commands in ~/other"],
            )],
        ),
    );
    let fetch = f::screen(f::BOX_FETCH);
    differ(
        "another url",
        &fetch,
        &swap_rows(
            &fetch,
            &[("   url: https://docs.rs/x", &["   url: https://docs.rs/y"])],
        ),
    );
}

/// The key is read by the loop's reader, never the screen's guess: the generic reader gives the
/// generic key where Claude Code's reads the box, and under `program=codex` Codex's exec box
/// served twice, its `Tip:` rows re-wrapped, is one point.
#[test]
fn the_box_key_is_the_readers() {
    let before = f::screen(BEFORE);
    let generic = point_key(&aterm_phase::GenericReader, &prompt_turn(before.clone()));
    assert!(
        generic.starts_with("prompt kind=other classify=- command=-\n"),
        "{generic}"
    );
    assert!(claude_key(&before).starts_with("prompt bash whole\n"));

    use aterm_phase::codex::fixtures as cx;
    let exec = f::screen(cx::BOX_EXEC);
    let tip = exec
        .iter()
        .position(|r| r.starts_with("  Tip: "))
        .expect("the tip");
    let mut retipped = exec.clone();
    retipped[tip..tip + 3].clone_from_slice(&rows(&[
        "  Tip: This is GPT-6, a new generation of intelligence. Astra is state-of-the-art in coding, computer use,",
        "  science, and professional work. Give it a hard problem, a half-formed idea, or anything you've been meaning",
        "  to build. See where it takes you.",
    ]));
    assert_ne!(exec, retipped, "the tip re-wrapped");
    let codex_key = |r: &[String]| point_key(&aterm_phase::CodexReader, &prompt_turn(r.to_vec()));
    assert!(
        codex_key(&exec).starts_with("prompt bash whole\n"),
        "{}",
        codex_key(&exec)
    );
    assert_eq!(codex_key(&exec), codex_key(&retipped));
    let mut m = Mock::new(true, vec![f::screen(cx::BUSY), exec, retipped]);
    m.program = "codex";
    m.vanish_after = Some(2);
    let (a, lines) = watched("box-key-codex", &mut m, &handing(), |_| {});
    assert_eq!(a, Asked::each(1, 0), "{lines:#?}\n{:#?}", m.requests);
}

// ---- the loop --------------------------------------------------------------

/// A box handed over, then redrawn and re-wrapped with no busy read between: one `EVENT
/// prompt`, one badge, one ask, no `CLEARED`.
#[test]
fn a_redrawn_box_is_asked_once() {
    let script = vec![
        busy_screen(),
        f::screen(BEFORE),
        f::screen(REDRAWN),
        f::screen(REDRAWN),
        rewrapped(),
        rewrapped(),
    ];
    let (a, m) = asked("box-key-redrawn", script, &handing());
    assert_eq!(a, Asked::each(1, 0), "{:#?}", m.requests);
}

/// A ticking auto-deny countdown, wrapped or not, is asked once, under the safe rules and
/// `approve = "none"` alike.
#[test]
fn a_ticking_countdown_is_asked_once() {
    let script = vec![
        busy_screen(),
        ticking("1:59"),
        ticking("1:58"),
        ticking("1:57"),
        ticking_wrapped("1:56"),
    ];
    for (tag, opts) in [("safe", auto(30, None)), ("none", handing())] {
        let (a, m) = asked("box-key-ticking", script.clone(), &opts);
        assert_eq!(a, Asked::each(1, 0), "{tag}: {:#?}", m.requests);
    }
}

/// Negative control: two Edit boxes on one path with different diffs, no busy read between,
/// are two asks, the first badge cleared. Run with the mock's own `await gone` and with the
/// server's (`gone_on_next`), where the path row never leaves and only the read after the
/// step that ran out sees the second box.
#[test]
fn two_edit_boxes_on_one_path_are_two_asks() {
    for servers in [false, true] {
        let script = vec![busy_screen(), f::screen(f::BOX_EDIT), edit_other_diff()];
        let mut m = Mock::new(true, script);
        m.gone_on_next = servers;
        // The first wait on the first box's path runs out (a timeout, not
        // the end); the session goes at the second box's second wait.
        m.vanish_after = Some(2);
        let (a, lines) = watched("box-key-two-edits", &mut m, &handing(), |_| {});
        assert_eq!(
            a,
            Asked::each(2, 1),
            "gone_on_next={servers}: {lines:#?}\n{:#?}",
            m.requests
        );
        if servers {
            assert!(
                m.gone_held >= 1,
                "the path held the wait: {:#?}",
                m.requests
            );
        }
    }
}

/// Negative control: two Bash boxes with different commands, no busy read between, are two
/// asks.
#[test]
fn two_bash_boxes_with_different_commands_are_two_asks() {
    let script = vec![busy_screen(), rm_box("cmd-A"), rm_box("cmd-B")];
    let (a, m) = asked("box-key-two-commands", script, &handing());
    assert_eq!(a, Asked::each(2, 1), "{:#?}", m.requests);
}

/// The measured box's transcript with no box under it, no composer, no busy
/// row: what the screen shows a frame while Claude Code redraws (`writing`).
fn writing_frame() -> Vec<String> {
    let before = f::screen(BEFORE);
    let rule = before
        .iter()
        .position(|r| r.starts_with('─'))
        .expect("the rule over the box");
    before[..rule].to_vec()
}

/// A box redrawn through frames of output being written (no read of the worker's own busy
/// row) is asked once; with the worker's busy screen in their place it is asked twice, the
/// badge cleared at the busy read.
#[test]
fn a_box_redrawn_through_a_writing_frame_is_asked_once() {
    let writing = writing_frame();
    let claude = aterm_phase::ClaudeReader;
    assert!(
        !matches!(claude.phase(&writing), Phase::Busy | Phase::Prompt),
        "{:?}",
        claude.phase(&writing)
    );
    assert!(claude.composer(&writing).is_none());
    let box_rows = f::screen(BEFORE);
    for (between, want) in [(writing.clone(), 1), (busy_screen(), 2)] {
        let script = vec![
            busy_screen(),
            box_rows.clone(),
            between.clone(),
            between,
            box_rows.clone(),
        ];
        let mut m = Mock::new(true, script);
        m.idle_never = true;
        m.vanish_after = Some(2);
        let (a, lines) = watched("box-key-writing", &mut m, &handing(), |_| {});
        assert_eq!(
            a,
            Asked::each(want, want - 1),
            "{want}: {lines:#?}\n{:#?}",
            m.requests
        );
    }
}

/// Under a nonzero box settle the box is judged only on a read made after the settle (an
/// `await seq` before its badge), and its redraws after the ask are the same point: one ask.
/// The box stands until its badge is up (`attention_gates`), so the settle runs out on it.
#[test]
fn a_redrawn_box_under_the_settle_is_asked_once_after_the_settle() {
    let settle = Duration::from_millis(300);
    let script = vec![
        busy_screen(),
        f::screen(BEFORE),
        f::screen(REDRAWN),
        f::screen(REDRAWN),
    ];
    let mut m = Mock::new(true, script);
    m.attention_gates = vec![(1, 0)];
    m.stall_sleep = Some(settle);
    m.vanish_after = Some(3);
    let (a, lines) = watched("box-key-settle", &mut m, &handing(), |s| {
        s.set_box_settle(settle);
    });
    assert_eq!(a, Asked::each(1, 0), "{lines:#?}\n{:#?}", m.requests);
    let badge = m
        .requests
        .iter()
        .position(|r| r.starts_with("meta set attention owner=supervisor "))
        .expect("the badge");
    assert!(
        m.requests[..badge]
            .iter()
            .any(|r| r.starts_with("await seq ")),
        "judged after the settle: {:#?}",
        m.requests
    );
}

/// The same box asked again after work has the same key and still waits out the vendor's key
/// guard: the box settle counts from the read that first showed this mount of it. Pressed
/// under full power and handed over alike, each box's first read and its act have an `await
/// seq` between them.
#[test]
fn a_box_asked_again_after_work_waits_the_settle_again() {
    let settle = Duration::from_millis(300);
    // An `await seq` lies between a box's first read and its act.
    let settled_between = |m: &Mock, first_read: usize, act: usize| {
        m.requests[first_read..act]
            .iter()
            .any(|r| r.starts_with("await seq "))
    };
    // A box pressed, then the same box after a busy screen (the press seen
    // leaving): each sits until its own press.
    let mut m = fenced(vec![bash_one_row(), busy_screen(), bash_one_row()]);
    m.key_gates = vec![(0, 0), (2, 1)];
    m.stall_sleep = Some(settle);
    m.vanish_after = Some(1);
    let mut s = session(&mut m, None);
    s.set_box_settle(settle);
    let _ = s.supervise(&full_power_opts(None));
    drop(s);
    assert_eq!(m.presses().len(), 2, "{:#?}", m.requests);
    let presses: Vec<usize> = m
        .requests
        .iter()
        .enumerate()
        .filter(|(_, r)| r.starts_with("key "))
        .map(|(i, _)| i)
        .collect();
    let busy_read = m
        .reads
        .iter()
        .position(|(i, _)| *i == 1)
        .expect("the busy read");
    let texts: Vec<usize> = m
        .requests
        .iter()
        .enumerate()
        .filter(|(_, r)| r.starts_with("text "))
        .map(|(i, _)| i)
        .collect();
    let second_box_read = texts[busy_read + 1];
    assert!(
        settled_between(&m, texts[0], presses[0]),
        "the first box: {:#?}",
        m.requests
    );
    assert!(
        settled_between(&m, second_box_read, presses[1]),
        "the second box waited its own settle: {:#?}",
        m.requests
    );

    // Handed over, with the busy spell read by the turn's wait between: the
    // second box's badge only after its own settle.
    let mut m = Mock::new(
        true,
        vec![busy_screen(), bash_one_row(), busy_screen(), bash_one_row()],
    );
    m.attention_gates = vec![(1, 0), (3, 1)];
    m.stall_sleep = Some(settle);
    m.vanish_after = Some(2);
    let (a, lines) = watched("box-key-settle-again", &mut m, &handing(), |s| {
        s.set_box_settle(settle);
    });
    assert_eq!(a, Asked::each(2, 1), "{lines:#?}\n{:#?}", m.requests);
    let badges: Vec<usize> = m
        .requests
        .iter()
        .enumerate()
        .filter(|(_, r)| r.starts_with("meta set attention owner=supervisor "))
        .map(|(i, _)| i)
        .collect();
    let second_box_read = m
        .requests
        .iter()
        .enumerate()
        .filter(|(_, r)| r.starts_with("text "))
        .map(|(i, _)| i)
        .zip(&m.reads)
        .find(|(_, (script, _))| *script == 3)
        .map(|(i, _)| i)
        .expect("the second box read");
    assert!(
        settled_between(&m, second_box_read, badges[1]),
        "the second box waited its own settle: {:#?}",
        m.requests
    );
}

// ---- someone answering the box, the window above it -------------------------

/// The measured subagent box (2.1.283, 149x62, a fresh pane) with the blank rows under it
/// trimmed, so it sits at the pane's bottom as in a full one. The rows right over it are the
/// main thread's: `⏺ The subagent is running your command now…` and the spinner `✻ Waiting for
/// 1 background agent to finish`.
fn subagent_box() -> Vec<String> {
    let mut r = f::screen(f::BOX_RM_SUBAGENT_FRESH_PANE);
    while r.last().is_some_and(|x| x.trim().is_empty()) {
        r.pop();
    }
    r
}

/// [`subagent_box`] with the main thread printing one row more over the spinner while the box
/// waits, hand-built (inferred, not captured), the pane's height kept: two rows off its top.
fn subagent_box_printed_over() -> Vec<String> {
    let mut r = subagent_box()[2..].to_vec();
    let spinner = r
        .iter()
        .position(|x| x.starts_with("✻ Waiting for 1 background agent"))
        .expect("the spinner row");
    r.splice(
        spinner..spinner,
        rows(&["⏺ Still waiting on the subagent's report.", ""]),
    );
    r
}

/// The measured preview dialog (2.1.282) with the focus moved to `2. Tabs` and the pane
/// following it, hand-built from the capture and the option's own `preview` text.
fn preview_moved() -> Vec<String> {
    let mut r = f::screen(f::QUESTION_PREVIEW);
    let at = r
        .iter()
        .position(|x| x.starts_with("❯ 1. Sidebar"))
        .expect("option 1");
    r[at] = r[at].replacen('❯', " ", 1);
    for (k, line) in [
        "+-------+-------+------+",
        "|General| Theme | Keys |",
        "+-------+-------+------+",
        "| Font size: 13        |",
        "+----------------------+",
    ]
    .iter()
    .enumerate()
    {
        let row = &r[at + 1 + k];
        let bar = row.find('│').expect("the pane's bar");
        r[at + 1 + k] = format!("{}│ {line:<40} │", &row[..bar]);
    }
    r[at + 1] = r[at + 1].replacen("  2. Tabs", "❯ 2. Tabs", 1);
    r
}

/// The measured Edit box with text typed into option 1, hand-built (the `Tab to amend` shape is
/// not captured): what counts is that the typing stays in the option rows.
fn edit_amended() -> Vec<String> {
    swap_rows(
        &f::screen(f::BOX_EDIT),
        &[(" ❯ 1. Yes", &[" ❯ 1. Yes, and keep the comment above it"])],
    )
}

/// A box someone is answering (a multi-select ticked, free text typed, a preview's focus
/// moved, an Edit box's focus moved or option amended, a subagent box printed over) is asked
/// once under the server's own `await gone`: a question or a command is not read while its row
/// stands, and a file box is read once a step, its request holding while its options change.
#[test]
fn a_box_someone_is_answering_is_asked_once() {
    let edit = f::screen(f::BOX_EDIT);
    for (tag, first, then, file) in [
        (
            "multi-select ticked",
            f::screen(f::QUESTION_MULTISELECT),
            f::screen(f::QUESTION_MULTISELECT_CHECKED),
            false,
        ),
        (
            "free text typed",
            f::screen(f::QUESTION_FREE_TEXT_FOCUSED),
            f::screen(f::QUESTION_FREE_TEXT_TYPED),
            false,
        ),
        (
            "preview focus moved",
            f::screen(f::QUESTION_PREVIEW),
            preview_moved(),
            false,
        ),
        ("edit focus moved", edit.clone(), edit_focused(3), true),
        ("edit amended", edit, edit_amended(), true),
        (
            "subagent box printed over",
            subagent_box(),
            subagent_box_printed_over(),
            false,
        ),
    ] {
        let claude = aterm_phase::ClaudeReader;
        let (a, b) = (
            claude.prompt(&first).expect("the box"),
            claude.prompt(&then).expect("the box, still"),
        );
        assert_eq!(a.kind, b.kind, "{tag}");
        assert_ne!(first, then, "{tag}: the rows differ");
        assert_eq!(
            box_anchor(&claude, &first),
            box_anchor(&claude, &then),
            "{tag}: one first row, so the wait on it runs out"
        );
        let mut m = Mock::new(true, vec![busy_screen(), first, then]);
        m.gone_on_next = true;
        m.vanish_after = Some(3);
        let (asked, lines) = watched("box-key-answering", &mut m, &handing(), |_| {});
        assert_eq!(
            asked,
            Asked::each(1, 0),
            "{tag}: {lines:#?}\n{:#?}",
            m.requests
        );
        assert!(m.gone_held >= 2, "{tag}: the row held: {:#?}", m.requests);
        let probes = m.after_gone.iter().filter(|p| **p).count();
        if file {
            assert!(
                probes >= 1 && m.reads.iter().any(|(i, _)| *i == 2),
                "{tag}: the step's read saw the box changed and passed it: {:#?}",
                m.requests
            );
        } else {
            assert_eq!(
                probes, 0,
                "{tag}: no read while it stands: {:#?}",
                m.requests
            );
        }
    }
}

/// [`BEFORE`]'s 2.1.283 layout (the vendor's echo of the pending call, `Running 1 shell
/// command…` over `⎿  $ <command>`, wrapped at a space as the capture wraps it) around a
/// 55-letter command, under `transcript`. Hand-built on the measured 80-column geometry.
fn suite_box(transcript: &[&str]) -> Vec<String> {
    let mut r = rows(transcript);
    r.extend(rows(&[
        "",
        "  Running 1 shell command…",
        "  ⎿  $ cargo test --workspace --all-targets -- --test-threads=1",
        "     integration_suite",
        "",
        "────────────────────────────────────────────────────────────────────────────────",
        " Bash command",
        "",
        "   cargo test --workspace --all-targets -- --test-threads=1 integration_suite",
        "   Run the integration suite on one thread",
        "",
        " Do you want to proceed?",
        " ❯ 1. Yes",
        "   2. No",
        "",
        " Esc to cancel · Tab to amend",
    ]));
    r
}

/// The same long command asked again after it failed (its error and the worker's next words
/// above the new echo, the busy spell unseen) is a new point: the [`ABOVE_LETTERS`] right over
/// the title are the echo alone, so the key counts the letters above it ([`before_the_echo`]).
#[test]
fn a_retried_long_command_is_asked_again() {
    let said = "⏺ The fix is in; running the integration suite on one thread.";
    let first = suite_box(&[said]);
    let again = suite_box(&[
        said,
        "",
        "  Ran 1 shell command",
        "  ⎿  error: 1 test of integration_suite timed out after 60s",
        "",
        "⏺ One test timed out under load; running the suite again.",
    ]);
    let letters = |r: &[String]| -> String {
        r.iter()
            .flat_map(|x| x.chars())
            .filter(|c| c.is_alphabetic())
            .collect()
    };
    let right_over_the_title = |r: &[String]| -> String {
        let title = r.iter().position(|x| x == " Bash command").expect("title");
        let above = letters(&r[..title]);
        let skip = above.chars().count().saturating_sub(ABOVE_LETTERS);
        above.chars().skip(skip).collect()
    };
    let echo = letters(&first[2..5]);
    assert!(echo.starts_with("Running") && echo.ends_with("integrationsuite"));
    assert_eq!(right_over_the_title(&first), right_over_the_title(&again));
    assert!(
        echo.ends_with(&right_over_the_title(&first)),
        "the window right over the title is the echo alone: {}",
        right_over_the_title(&first)
    );
    assert_ne!(claude_key(&first), claude_key(&again));
    assert_eq!(claude_key(&first), claude_key(&suite_box(&[said])));
    let mut m = Mock::new(true, vec![first, busy_screen(), again]);
    m.idle_skips_busy = true;
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines(
        &mut m,
        &SuperviseOpts {
            max: Duration::from_secs(30),
            policy: powerless(),
            ..SuperviseOpts::default()
        },
    );
    let got = decisions(&lines);
    assert!(
        got[0].starts_with("EVENT prompt kind=bash ")
            && got[0].ends_with(
                " command=cargo test --workspace --all-targets -- --test-threads=1 \
                 integration_suite"
            ),
        "{lines:#?}"
    );
    assert_eq!(got[1], got[0], "asked again: {lines:#?}");
    assert!(got[2].starts_with("EXIT session gone"), "{lines:#?}");
}

// ---- Tier-1: the loop bound to `SupervisorBoxAsk` ----------------------------

/// A screen of a script as `SupervisorBoxAsk` sees it.
#[derive(Debug, Clone, Copy)]
enum Seen {
    /// The worker's busy row: whatever request was up was answered, and the
    /// worker works.
    Busy,
    /// Output being written: no box, no composer, no busy row.
    Writing,
    /// Request `req` up, in drawing `form` (a redraw flips it).
    Box { req: i64, form: i64 },
}

/// The layout key the model's negative control replays: a prompt's review key built from the
/// box's rows verbatim (focus marker and countdown with them) and the words of the rows above
/// it one by one (`event_summary`, `status_row`, `prompt_box_span`, `push_words_above`).
/// Walked over the same reads, it must be refused.
fn layout_key(rows: &[String]) -> String {
    let turn = prompt_turn(rows.to_vec());
    let mut key = format!(
        "{} {}\n{}\n",
        turn.phase.name(),
        event_summary(&turn, &[]),
        status_row(rows).unwrap_or("")
    );
    let span = prompt_box_span(rows);
    let upto = match span {
        Some((a, _)) => a,
        None => last_said_index(rows).map_or(0, |k| k + 1),
    };
    push_words_above(&mut key, &rows[..upto]);
    if let Some((a, b)) = span {
        for r in &rows[a..=b] {
            key.push_str(r.trim_end());
            key.push('\n');
        }
    }
    key
}

/// The shipping review key of a box screen: `point_key` read by Claude
/// Code's grammar, the loop's reader for every script here.
fn shipping_key(rows: &[String]) -> String {
    claude_key(rows)
}

/// A review key over a screen's rows, as a walk binds the model to it
/// ([`walk_reads`]: [`shipping_key`], or [`layout_key`] for the replay).
type KeyOf<'k> = &'k dyn Fn(&[String]) -> String;

/// Walks the real loop's reads of one script (`Mock::reads`: the script index of each `text`
/// read, in order) on `model`, and, given `key`, binds the real loop to it read by read:
///
/// - a read straight after an `await gone` on the box's row that ran out (`Mock::after_gone`)
///   is the wait's own read, never a look: no action fires, no ask may follow it, and it must
///   end the wait exactly when the model says another request is up (the real
///   `file_request_key` of what it read against that of the box last looked at);
/// - a busy read is `Answer` (a request up) then `Work`; a writing read is `Writing`; any other
///   box read is `NextBox` (another request: the next the model draws) or `Redraw`, then
///   `Look`, and before the `Look` the model's `key == handed` must hold exactly when `key` of
///   this screen equals `key` of the box last looked at;
/// - the asks the model counts at a read must be the `post … kind=ask` the real loop sent
///   between that read and the next.
///
/// Every invariant is checked at every state. `key` `None` walks the model alone (the dials'
/// negative controls). `Ok` the asks the model counts; `Err` the first thing broken, its
/// message starting with `key`, `request`, `asks`, `order`, or the invariant's name.
fn walk_reads(
    model: &aterm_spec::derive::Model,
    script: &[(Vec<String>, Seen)],
    m: &Mock,
    key: Option<KeyOf<'_>>,
) -> Result<i64, String> {
    let texts: Vec<usize> = m
        .requests
        .iter()
        .enumerate()
        .filter(|(_, r)| r.starts_with("text "))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(texts.len(), m.reads.len(), "every read is a `text`");
    assert_eq!(m.after_gone.len(), m.reads.len(), "every read is marked");
    let asks_at: Vec<usize> = m
        .requests
        .iter()
        .enumerate()
        .filter(|(_, r)| r.starts_with("post to=@s-9 kind=ask "))
        .map(|(i, _)| i)
        .collect();
    let mut st = model.init_state();
    // The asks of the requests already past, and the screen of the last look
    // at a box.
    let mut past = 0;
    let mut looked: Option<&[String]> = None;
    let fire = |st: &mut std::collections::BTreeMap<&'static str, i64>, a: &str| {
        if model.fire(a, st) {
            Ok(())
        } else {
            Err(format!("order: the model refused {a} at {st:?}"))
        }
    };
    for (k, &(i, _)) in m.reads.iter().enumerate() {
        let (rows, seen) = (&script[i].0, script[i].1);
        let until = texts.get(k + 1).copied().unwrap_or(m.requests.len());
        let posted = asks_at
            .iter()
            .filter(|&&at| texts[k] < at && at < until)
            .count();
        let posted = i64::try_from(posted).expect("asks");
        if m.after_gone[k] {
            let Seen::Box { req, .. } = seen else {
                return Err(format!("order: the wait's read {k} shows no box"));
            };
            let last = looked.ok_or_else(|| format!("order: the wait's read {k} before a look"))?;
            if key.is_some() {
                let request = |r: &[String]| {
                    file_request_key(&aterm_phase::ClaudeReader, &prompt_turn(r.to_vec()))
                };
                let same = request(rows) == request(last) && request(rows).is_some();
                if same != (req == st["req"]) {
                    return Err(format!(
                        "request: the wait's read {k} took request {req} for {} the one up \
                         ({st:?})",
                        if same { "" } else { "other than" }
                    ));
                }
                if posted != 0 {
                    return Err(format!("asks: {posted} posted after the wait's read {k}"));
                }
            }
            continue;
        }
        let before = past + st["asks"];
        match seen {
            Seen::Busy => {
                if st["req"] > 0 {
                    fire(&mut st, "Answer")?;
                }
                fire(&mut st, "Work")?;
            }
            Seen::Writing => fire(&mut st, "Writing")?,
            Seen::Box { req, form } => {
                if st["req"] != req {
                    if st["seen"] + 1 != req {
                        return Err(format!("order: request {req} drawn out of order at {st:?}"));
                    }
                    past += st["asks"];
                    fire(&mut st, "NextBox")?;
                } else if st["form"] != form {
                    fire(&mut st, "Redraw")?;
                }
                if let Some(key) = key {
                    let real = looked.is_some_and(|last| key(last) == key(rows));
                    if real != (st["key"] == st["handed"]) {
                        return Err(format!(
                            "key: read {k} (script {i}) is {} the box last looked at by the \
                             key, and the model says otherwise at {st:?}",
                            if real { "the same as" } else { "not" }
                        ));
                    }
                }
                fire(&mut st, "Look")?;
                looked = Some(rows);
            }
        }
        for inv in &model.invariants {
            if !model.check_invariant(inv.name, &st) {
                return Err(format!("{} broken at {st:?}", inv.name));
            }
        }
        let asked = past + st["asks"] - before;
        if key.is_some() && asked != posted {
            return Err(format!(
                "asks: the model asks {asked} at read {k} (script {i}), the loop posted {posted}"
            ));
        }
    }
    Ok(past + st["asks"])
}

/// One script of the Tier-1 bind: its screens as the model sees each; whether `await idle`
/// never latches; whether `await gone` is the server's (`gone_on_next`); whether
/// [`layout_key`] must be refused walking its reads; and the dial that must break an invariant
/// walking them.
struct Walked {
    tag: &'static str,
    screens: Vec<(Vec<String>, Seen)>,
    idle_never: bool,
    servers_gone: bool,
    layout_refused: bool,
    caught: Option<(&'static str, &'static str)>,
}

/// Tier-1 for `SupervisorBoxAsk`: the real `Session` over each scripted server (S1-S10), every
/// read bound as [`walk_reads`] says, every invariant at every state, and every request a
/// script draws looked at. Negative controls: S1 and S2 walked with [`layout_key`] are refused
/// on the key, and the model alone breaks its invariant under `Drawing = 1` (S1), `Coarse = 1`
/// (S3) and `WritingWorks = 1` (S4).
#[test]
fn tier1_the_box_is_asked_once_per_request() {
    use Seen::{Box as B, Busy, Writing};
    let model = aterm_spec::derive::supervisor_box_ask_model();
    let edit_a = f::screen(f::BOX_EDIT);
    let edit_b = edit_other_diff();
    let writing = writing_frame();
    let redrawn = |tag, first: Vec<String>, then: Vec<String>| Walked {
        tag,
        screens: vec![
            (busy_screen(), Busy),
            (first, B { req: 1, form: 0 }),
            (then, B { req: 1, form: 1 }),
        ],
        idle_never: false,
        servers_gone: true,
        layout_refused: false,
        caught: None,
    };
    let scripts = vec![
        Walked {
            tag: "S1 the incident's redraw",
            screens: vec![
                (busy_screen(), Busy),
                (f::screen(BEFORE), B { req: 1, form: 0 }),
                (f::screen(REDRAWN), B { req: 1, form: 1 }),
                (f::screen(REDRAWN), B { req: 1, form: 1 }),
                (rewrapped(), B { req: 1, form: 0 }),
                (rewrapped(), B { req: 1, form: 0 }),
            ],
            idle_never: false,
            servers_gone: false,
            layout_refused: true,
            caught: Some(("Drawing", "OneAskPerRequest")),
        },
        Walked {
            tag: "S2 the ticking countdown",
            screens: vec![
                (busy_screen(), Busy),
                (ticking("1:59"), B { req: 1, form: 0 }),
                (ticking("1:58"), B { req: 1, form: 1 }),
                (ticking("1:57"), B { req: 1, form: 0 }),
                (ticking_wrapped("1:56"), B { req: 1, form: 1 }),
            ],
            idle_never: false,
            servers_gone: false,
            layout_refused: true,
            caught: None,
        },
        Walked {
            tag: "S3 two edits on one path, the second again after work",
            screens: vec![
                (busy_screen(), Busy),
                (edit_a.clone(), B { req: 1, form: 0 }),
                (edit_b.clone(), B { req: 2, form: 0 }),
                (busy_screen(), Busy),
                (edit_b.clone(), B { req: 3, form: 0 }),
            ],
            idle_never: false,
            servers_gone: false,
            layout_refused: false,
            caught: Some(("Coarse", "EveryLookedRequestIsAsked")),
        },
        Walked {
            tag: "S4 redrawn through writing frames",
            screens: vec![
                (busy_screen(), Busy),
                (f::screen(BEFORE), B { req: 1, form: 0 }),
                (writing.clone(), Writing),
                (writing, Writing),
                (f::screen(BEFORE), B { req: 1, form: 0 }),
            ],
            idle_never: true,
            servers_gone: false,
            layout_refused: false,
            caught: Some(("WritingWorks", "OneAskPerRequest")),
        },
        Walked {
            tag: "S5 two edits on one path under the server's gone",
            screens: vec![
                (busy_screen(), Busy),
                (edit_a.clone(), B { req: 1, form: 0 }),
                (edit_b, B { req: 2, form: 0 }),
            ],
            idle_never: false,
            servers_gone: true,
            layout_refused: false,
            caught: Some(("Coarse", "EveryLookedRequestIsAsked")),
        },
        redrawn("S6 the edit box arrowed through", edit_a, edit_focused(3)),
        redrawn(
            "S7 the multi-select ticked",
            f::screen(f::QUESTION_MULTISELECT),
            f::screen(f::QUESTION_MULTISELECT_CHECKED),
        ),
        redrawn(
            "S8 the free text typed",
            f::screen(f::QUESTION_FREE_TEXT_FOCUSED),
            f::screen(f::QUESTION_FREE_TEXT_TYPED),
        ),
        redrawn(
            "S9 the preview's focus moved",
            f::screen(f::QUESTION_PREVIEW),
            preview_moved(),
        ),
        redrawn(
            "S10 the subagent box printed over",
            subagent_box(),
            subagent_box_printed_over(),
        ),
    ];
    for Walked {
        tag,
        screens,
        idle_never,
        servers_gone,
        layout_refused,
        caught,
    } in scripts
    {
        let mut m = Mock::new(true, screens.iter().map(|(r, _)| r.clone()).collect());
        m.idle_never = idle_never;
        m.gone_on_next = servers_gone;
        m.vanish_after = Some(if servers_gone { 3 } else { 2 });
        let (a, lines) = watched("box-key-tier1", &mut m, &handing(), |_| {});
        let shipping: KeyOf<'_> = &shipping_key;
        let model_asks = walk_reads(&model, &screens, &m, Some(shipping))
            .unwrap_or_else(|e| panic!("{tag}: {e}\n{:#?}\n{:#?}", m.reads, m.requests));
        assert!(
            model_asks > 0,
            "{tag}: a walk that asked nothing proves nothing"
        );
        // Every request the script draws is looked at (the wait's own reads aside), else a
        // request the loop never read would pass unasked and unseen by the model too.
        let request_of = |i: usize| match screens[i].1 {
            B { req, .. } => Some(req),
            Busy | Writing => None,
        };
        let drawn: std::collections::BTreeSet<i64> =
            (0..screens.len()).filter_map(request_of).collect();
        let looked: std::collections::BTreeSet<i64> = m
            .reads
            .iter()
            .zip(&m.after_gone)
            .filter(|(_, probe)| !**probe)
            .filter_map(|((i, _), _)| request_of(*i))
            .collect();
        assert_eq!(
            looked, drawn,
            "{tag}: every request drawn is looked at: {:#?}",
            m.requests
        );
        let n = usize::try_from(model_asks).expect("asks");
        assert_eq!(
            (a.posts, a.prompts),
            (n, n),
            "{tag}: {lines:#?}\n{:#?}",
            m.requests
        );
        if layout_refused {
            let layout: KeyOf<'_> = &layout_key;
            match walk_reads(&model, &screens, &m, Some(layout)) {
                Err(e) => assert!(e.starts_with("key: "), "{tag}, the old layout key: {e}"),
                Ok(n) => panic!("{tag}: the old layout key is not refused ({n} asks)"),
            }
        }
        if let Some((dial, invariant)) = caught {
            let dialled = aterm_spec::interp::with_consts(&model, &[(dial, 1)]);
            match walk_reads(&dialled, &screens, &m, None) {
                Err(e) => assert!(e.starts_with(invariant), "{tag}, {dial} = 1: {e}"),
                Ok(n) => panic!("{tag}: {dial} = 1 is not caught by the walk ({n} asks)"),
            }
        }
    }
}
