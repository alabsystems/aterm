// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A GREY SUGGESTION IS NO PERSON'S DRAFT, AND NOTHING IS TYPED INTO A DRAFT INSIDE ITS GRACE. Under a
//! program that draws grey text a composer's first glyph is told by its cell wherever the cursor
//! stands (`Session::composer_draft`). Plain, it is a draft, which keeps its full grace from its first
//! read and then goes to the policy's draft step. Grey with the cursor home on it, it is a suggestion,
//! typed over at once. Grey with the cursor anywhere else it says neither, since a frame read before
//! its program placed the cursor and a person's own text drawn grey read the same: nothing is typed
//! over it, no Enter is pressed on it, the wait's own steps look again, and the host's guards hold as
//! the door does. Nothing looks at a grace on the way: a composer read empty or grey inside it decides
//! nothing, however long and after whatever input, since a redraw stalled before its text reads the
//! same. Any other text is another draft, which a person's edit and a window's reflow both read as, and
//! the loop's own words are no person's draft (`Session::own_words`). A person's key still holds, a
//! person's grace keeps the host's wake, and a screen ticking over a draft or a suggestion asks its
//! cell no more for that.

use super::*;
use crate::supervise::policy::turn_end::{MENU_DIVERTS, OWN_WORDS_STAND, PERSON_TYPING};

const SAID: &str = "Stage 1 is in.";
const HINT: &str = "keep going, then merge when ready";
const DRAFT: &str = "please also update the changelog";
/// When the cursor goes home after a screen's first read ([`Mock::cursor_home_after`]).
const HOME: Duration = Duration::from_secs(5);
/// What a program's grey text reads as a cell, by the Mock's `cell_attrs_on`.
const GREY: &str = "dim";

/// What a run did, and when, on the Mock's clock.
struct Held {
    m: Mock,
    lines: Vec<String>,
    journal: Vec<String>,
}

impl Held {
    fn person_waits(&self) -> usize {
        self.journal
            .iter()
            .filter(|l| l.starts_with("WAITING ") && l.ends_with(PERSON_TYPING))
            .count()
    }

    fn first_read_of(&self, index: usize) -> Instant {
        self.m
            .read_times
            .iter()
            .find(|(i, _)| *i == index)
            .map(|(_, at)| *at)
            .unwrap_or_else(|| panic!("screen {index} was read: {}", self.evidence()))
    }

    fn typed_at(&self) -> Option<Instant> {
        self.m.turn_times.first().copied().or(self.m.first_send_at)
    }

    fn enter_at(&self) -> Option<Instant> {
        self.m
            .key_times
            .iter()
            .find(|(r, _)| r.ends_with(" enter"))
            .map(|(_, at)| *at)
    }

    /// When the run first typed or pressed Enter.
    fn acted_at(&self) -> Option<Instant> {
        self.typed_at().into_iter().chain(self.enter_at()).min()
    }

    /// How many screens the run read from `from` to its first act.
    fn reads_between(&self, from: Instant) -> usize {
        let to = self
            .acted_at()
            .unwrap_or_else(|| panic!("{}", self.evidence()));
        self.m
            .read_times
            .iter()
            .filter(|(_, at)| (from..=to).contains(at))
            .count()
    }

    fn enters(&self) -> usize {
        self.m
            .requests
            .iter()
            .filter(|r| r.starts_with("key ") && r.ends_with(" enter"))
            .count()
    }

    fn typed(&self) -> usize {
        self.m
            .requests
            .iter()
            .filter(|r| r.starts_with("turn ") || r.starts_with("send "))
            .count()
    }

    fn evidence(&self) -> String {
        format!(
            "journal: {:#?}\nlines: {:#?}\nrequests: {:#?}",
            self.journal, self.lines, self.m.requests
        )
    }

    /// The run typed over at once, within a second of screen `index`'s first read, and pressed no
    /// Enter on what stood there.
    fn typed_at_once(&self, tag: &str, index: usize) {
        assert_eq!(self.person_waits(), 0, "{tag}: {}", self.evidence());
        assert_eq!(self.enters(), 0, "{tag}: {}", self.evidence());
        let typed = self
            .typed_at()
            .unwrap_or_else(|| panic!("{tag}: {}", self.evidence()));
        assert!(
            typed < self.first_read_of(index) + Duration::from_secs(1),
            "{tag}: typed {:?} after its first read: {}",
            typed.saturating_duration_since(self.first_read_of(index)),
            self.evidence()
        );
    }

    /// The run typed over only once the cursor went home, at the wait step after [`HOME`] that read
    /// it, and pressed no Enter on what stood there.
    fn typed_at_the_step_after_home(&self, tag: &str, index: usize) {
        assert_eq!(self.person_waits(), 0, "{tag}: {}", self.evidence());
        assert_eq!(self.enters(), 0, "{tag}: {}", self.evidence());
        let seen = self.first_read_of(index);
        let typed = self
            .typed_at()
            .unwrap_or_else(|| panic!("{tag}: {}", self.evidence()));
        assert!(
            typed >= seen + HOME && typed <= seen + Duration::from_secs(21),
            "{tag}: typed {:?} after its first read: {}",
            typed.saturating_duration_since(seen),
            self.evidence()
        );
    }

    /// The run wrote nothing and pressed no Enter, and the journal says what held it.
    fn left_it_alone(&self, tag: &str) {
        assert_eq!(
            (self.enters(), self.typed()),
            (0, 0),
            "{tag}: {}",
            self.evidence()
        );
        assert!(
            self.journal
                .iter()
                .any(|l| l.ends_with("why=composer-unknown")),
            "{tag}: {}",
            self.evidence()
        );
    }

    /// How many `cell` reads the run made.
    fn cells(&self) -> usize {
        self.m
            .requests
            .iter()
            .filter(|r| r.starts_with("cell "))
            .count()
    }
}

/// Run `m` under the owners' policy (Cruise, a person's grace of 120 s), its turns ending at once.
fn held_run(tag: &str, mut m: Mock) -> Held {
    let (dir, journal) = journal_file(tag);
    let opts = SuperviseOpts {
        journal: Some(journal.clone()),
        ..hosted(600)
    };
    assert_eq!(opts.policy.human_grace_s, 120);
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            min_work: Duration::ZERO,
            ..TurnEndTiming::default()
        });
    });
    let journal = journal_records(&journal)
        .0
        .into_iter()
        .map(|r| r.line)
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    Held { m, lines, journal }
}

/// Ends a run at its first write or Enter.
fn acted(m: &Mock) -> bool {
    m.requests.iter().any(|r| {
        r.starts_with("turn ")
            || r.starts_with("send ")
            || (r.starts_with("key ") && r.ends_with(" enter"))
    })
}

/// Ends a run at its first write, Enter or question to a person.
fn acted_or_asked(m: &Mock) -> bool {
    acted(m)
        || m.requests
            .iter()
            .any(|r| r.starts_with("meta set attention"))
}

/// `ended(SAID)` under six earlier rows, so that row 10, where the Mock leaves its cursor, is the
/// composer's top rule and the caret's row is 11, with `text` on it.
fn tall_with(text: &str) -> Vec<String> {
    let mut r = rows(&[
        "⏺ Read the plan.",
        "",
        "⏺ Ran the suite: 412 passed.",
        "",
        "⏺ Fixed the parser.",
        "",
    ]);
    r.extend(typed_in(SAID, text));
    assert_eq!(r[11], format!("❯ {text}"), "the caret's row: {r:#?}");
    r
}

/// Muse Code's mock over `screens`, fenced as a current host is, its cell drawn in Muse's typed ink.
fn muse_fenced(screens: Vec<Vec<String>>) -> Mock {
    let mut m = muse_mock(screens);
    m.gen_fence = true;
    m.sends_gen = true;
    m.human_seq = Some(7);
    m.human = vec!["null"];
    m.help = FENCED_HELP.to_string();
    m.cell_fg = "ccd3db";
    m
}

/// A row under Muse Code's composer on `rows`: where its redraw leaves the cursor until it places it
/// last.
fn muse_footer(rows: &[String]) -> usize {
    let caret = aterm_phase::muse::composer(rows).expect("the composer");
    (caret + 1..rows.len())
        .find(|&i| !rows[i].trim().is_empty() && !rows[i].starts_with('─'))
        .expect("a row under the composer")
}

/// THE 174640090a SHAPE, ITS CURSOR ON THE CARET'S ROW: a grey suggestion read with the cursor past
/// it, as a frame drawn before the program homes its cursor leaves it. Its cell is grey and its cursor
/// off it, which says neither: nothing is typed and no Enter pressed. The wait step after the cursor
/// went home reads a suggestion and the continuation is typed over it, never an Enter on it. A cursor
/// that never goes home is never acted on, and the journal says why.
/// NEGATIVE CONTROLS: `composer_draft` taking grey for a suggestion wherever the cursor stands, and
/// the run whose cursor never homes types over the text at once; `composer_draft` asking the cell only
/// at column 2, as on `5ece62c7b3`, and that run presses Enter on it at 120 s.
#[test]
fn grey_text_with_the_cursor_past_it_waits_for_the_cursor() {
    let run = |homes: Option<Duration>| {
        let mut m = fenced(vec![busy_screen(), typed_in(SAID, HINT)]);
        m.dim_past = true;
        m.cursor_on_caret = true;
        m.cursor_col = Some(2 + HINT.chars().count());
        m.cursor_home_after = homes;
        m.turn_releases = Some(1);
        m.stall_sleep = Some(Duration::from_secs(3600));
        m.exit_when = Some(acted);
        held_run("draft-past", m)
    };
    run(Some(HOME)).typed_at_the_step_after_home("homes", 1);
    run(None).left_it_alone("never homes");
}

/// THE 174640090a SHAPE: the suggestion drawn whole, the cursor still off the caret's row (where
/// `fabric_world`'s stand-in left it between its `cat` and its cursor placement), then home. The
/// continuation is typed over the suggestion once a wait step reads the cursor home, never an Enter
/// on it.
/// NEGATIVE CONTROL: as [`grey_text_with_the_cursor_past_it_waits_for_the_cursor`]'s, and the
/// continuation is typed at the read off the caret.
#[test]
fn a_suggestion_read_with_the_cursor_off_its_caret_waits_for_the_cursor() {
    let mut m = fenced(vec![busy_screen(), tall_with(HINT)]);
    m.dim_past = true;
    m.cursor_col = Some(40);
    m.cursor_home_after = Some(HOME);
    m.turn_releases = Some(1);
    m.stall_sleep = Some(Duration::from_secs(3600));
    m.exit_when = Some(acted);
    let t = held_run("draft-off-caret", m);
    t.typed_at_the_step_after_home("off its caret", 1);
    assert!(t.typed() == 1, "{}", t.evidence());
}

/// MUSE CODE'S HINT, WHEREVER ITS FRAME LEFT THE CURSOR (`docs/measured/half-drawn-frame-2026-10-22.md`):
/// no DEC 2026 brackets, its ~2 KB bottom pane drawn in place and its cursor placed last
/// (`ESC[48;3H`), so a read inside the redraw finds its prompt hint whole with the cursor on the
/// footer's row, and a read of a frame before that with the cursor past the hint on the caret's row.
/// Its cell (Muse's grey `676c74`) says grey and its cursor says neither, so nothing is typed until
/// a wait step reads the cursor home; then the continuation is typed and the hint is never submitted.
/// NEGATIVE CONTROL: `cell_is_placeholder` ignoring the program's hint ink, and the hint is held as
/// a person's draft and then sent.
#[test]
fn a_muse_hint_waits_for_the_cursor_wherever_its_frame_left_it() {
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    let hint = "/export saves the conversation to a text file";
    for past in [false, true] {
        let hinted = muse_typed(hint);
        let caret = aterm_phase::muse::composer(&hinted).expect("the composer");
        let footer = muse_footer(&hinted);
        let mut m = muse_mock(vec![screen(u::BUSY_1_4_10), hinted]);
        m.screen_cursors
            .insert(1, if past { caret } else { footer });
        m.cursor_col = Some(if past { 2 + hint.chars().count() } else { 40 });
        m.cell_fg = "676c74";
        m.cell_attrs = "none";
        m.cursor_home_after = Some(HOME);
        m.turn_releases = Some(1);
        m.exit_when = Some(acted);
        let t = held_run("draft-muse-hint", m);
        t.typed_at_the_step_after_home(&format!("past {past}"), 1);
    }
}

/// A SUGGESTION READ IN PART IS NO DRAFT (the review of `d7b6e8aa23`): the first read catches
/// `keep going, then` with the cursor after it (a frame read before its program placed the cursor),
/// no key measured. That read says neither, and the frame's next read, the suggestion whole with the
/// cursor home, has the continuation typed over it, never an Enter on it.
/// NEGATIVE CONTROL: `composer_draft` asking the cell only at column 2, as on `5ece62c7b3`: the part
/// is waited on as a person's draft.
#[test]
fn a_suggestion_read_in_part_is_no_draft() {
    let part = "keep going, then";
    let mut m = Mock::new(vec![
        busy_screen(),
        typed_in(SAID, part),
        typed_in(SAID, HINT),
    ]);
    m.dim_past = true;
    m.cursor_on_caret = true;
    m.screen_cols.insert(1, 2 + part.chars().count());
    m.screen_cols.insert(2, 2);
    m.timeline = Some((1, vec![Duration::ZERO, Duration::from_millis(300)]));
    m.stall_sleep = Some(Duration::from_secs(3600));
    m.exit_when = Some(acted);
    let t = held_run("draft-hint-in-part", m);
    t.typed_at_once("in part", 1);
}

/// A SUGGESTION OVER AN EMPTY LATER ROW WAITS FOR THE CURSOR (the review of `d7b6e8aa23`): Claude
/// Code's one-row suggestion (dim) with an empty composer row drawn under it and the cursor on that
/// row (a redraw read before the row it left was cleared), no key measured. That reads as a person's
/// grey text over a Shift+Enter does, so nothing is typed and no Enter pressed; a wait step reads the
/// cursor home, and the continuation is typed over the suggestion, never an Enter on it.
/// NEGATIVE CONTROL: `cursor_home` without its later-row test, and the continuation is typed at the
/// first read.
#[test]
fn a_suggestion_over_an_empty_later_row_waits_for_the_cursor() {
    let mut r = typed_in(SAID, HINT);
    let caret = r
        .iter()
        .position(|row| *row == format!("❯ {HINT}"))
        .expect("the caret's row");
    r.insert(caret + 1, "  ".to_string());
    let mut m = fenced(vec![busy_screen(), r]);
    m.screen_cursors.insert(1, caret + 1);
    m.cursor_col = Some(2);
    m.cursor_home_after = Some(HOME);
    m.turn_releases = Some(1);
    m.stall_sleep = Some(Duration::from_secs(3600));
    m.exit_when = Some(acted);
    let t = held_run("draft-hint-later-row", m);
    t.typed_at_the_step_after_home("a later row", 1);
}

/// A PERSON'S TEXT DRAWN GREY IS NEVER TYPED OVER NOR SENT (the review of `5ad7f68c8f`). A program can
/// draw a person's own words grey: Claude Code 2.1.268 draws a voice transcript still forming
/// `dimColor` from the composer's first glyph, the cursor after it (read from its source, not run),
/// and a foreign write there drops the dictated words. So grey text with the cursor anywhere but home
/// on it, however many rows it has, is left alone, no key measured, for as long as it stands, and
/// the journal says what holds the point.
/// NEGATIVE CONTROL: `composer_draft` taking grey for a suggestion wherever the cursor stands, and
/// the continuation is typed over each at its first read.
#[test]
fn a_persons_text_drawn_grey_is_never_typed_over_nor_sent() {
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    let past = 2 + DRAFT.chars().count();
    let claude = |rows: Vec<String>, row: usize, col: usize| {
        let mut m = fenced(vec![busy_screen(), rows]);
        m.dim_past = true;
        m.cursor_row = Some(row);
        m.cursor_col = Some(col);
        m
    };
    let (empty_row, empty_caret) = two_rows();
    let (full_rows, full_caret) = wrapped("please also update", "the changelog");
    let on_caret = typed_in(SAID, DRAFT);
    let caret = on_caret
        .iter()
        .position(|row| *row == format!("❯ {DRAFT}"))
        .expect("the caret's row");
    let mut cases = vec![
        ("past its text", claude(on_caret, caret, past)),
        ("off the composer", claude(tall_with(DRAFT), 10, 40)),
        (
            "column 2 of an empty later row",
            claude(empty_row.clone(), empty_caret + 1, 2),
        ),
        (
            "column 4 of an empty later row",
            claude(empty_row, empty_caret + 1, 4),
        ),
        (
            "after two rows of it",
            claude(
                full_rows,
                full_caret + 1,
                2 + "the changelog".chars().count(),
            ),
        ),
    ];
    for (what, split) in [("Muse past its text", false), ("Muse on its footer", true)] {
        let drafted = muse_typed(DRAFT);
        let caret = aterm_phase::muse::composer(&drafted).expect("the composer");
        let footer = muse_footer(&drafted);
        let mut m = muse_fenced(vec![screen(u::BUSY_1_4_10), drafted]);
        m.cell_fg = "676c74";
        m.cell_attrs = "none";
        m.screen_cursors
            .insert(1, if split { footer } else { caret });
        m.cursor_col = Some(if split { 40 } else { past });
        cases.push((what, m));
    }
    for (what, mut m) in cases {
        m.turn_releases = Some(1);
        m.stall_sleep = Some(Duration::from_secs(3600));
        held_run("draft-person-grey", m).left_it_alone(what);
    }
}

/// A DICTATION DRAWN GREY IS LEFT ALONE UNTIL IT STANDS PLAIN (the same review): the key that started
/// it long past its grace, the words growing under the cursor every 20 s, drawn dim, and then standing
/// plain. Nothing is typed or pressed while they grow, and the draft then keeps a full grace from
/// their last change before the draft step sends it.
/// NEGATIVE CONTROL: as [`a_persons_text_drawn_grey_is_never_typed_over_nor_sent`]'s, and `keep going`
/// is written into the first words read.
#[test]
fn a_dictation_drawn_grey_is_left_alone_until_it_stands_plain() {
    let said = [
        "so what I want",
        "so what I want you to do next",
        "so what I want you to do next is this",
    ];
    let mut screens = vec![busy_screen()];
    screens.extend(said.iter().map(|s| typed_in(SAID, s)));
    screens.push(typed_in(SAID, said[2]));
    let mut m = fenced(screens);
    m.dim_past = true;
    m.cursor_on_caret = true;
    for (i, words) in said.iter().enumerate() {
        m.screen_cols.insert(i + 1, 2 + words.chars().count());
        m.human_on.insert(i + 1, "300000");
    }
    m.screen_cols.insert(4, 2 + said[2].chars().count());
    m.human_on.insert(4, "360000");
    m.cell_attrs_on.insert(4, "none");
    m.timeline = Some((
        1,
        vec![
            Duration::ZERO,
            Duration::from_secs(20),
            Duration::from_secs(40),
            Duration::from_secs(60),
        ],
    ));
    m.stall_sleep = Some(Duration::from_secs(3600));
    m.exit_when = Some(acted);
    let t = held_run("draft-dictation", m);
    let last = t.first_read_of(3);
    let acted = t.acted_at().unwrap_or_else(|| panic!("{}", t.evidence()));
    assert!(
        acted >= last + Duration::from_secs(119),
        "acted {:?} after the words last changed: {}",
        acted.saturating_duration_since(last),
        t.evidence()
    );
    assert_eq!((t.enters(), t.typed()), (1, 0), "{}", t.evidence());
}

/// WHAT THE GLYPH'S CELL AND THE CURSOR SAY OF A COMPOSER'S TEXT, ARM BY ARM (`composer_draft`), and
/// which arms read a cell. `cursor_home` takes no row but a later one of the composer: a grid a resize
/// left drawn one row high has the cursor a row under the caret's
/// (`docs/DESIGN-wall-resume-2026-09-28.md`), and every real suggestion a tape holds has it on the
/// caret's row at column 2 (`c8-cruise-placeholder`, 13 of 13 reads). A dim first glyph on more rows
/// than one is no draft by its rows alone: nothing measured keeps a long suggestion to one row.
#[test]
fn what_a_composers_cell_and_cursor_say() {
    /// What the case is, the composer's lines, whether its program draws grey text, what the glyph's
    /// cell answers, the cursor's row and column, the verdict, and how many cells it asks.
    type Case<'a> = (
        &'a str,
        &'a [&'a str],
        bool,
        &'static str,
        usize,
        usize,
        Option<bool>,
        usize,
    );
    const CARET: usize = 5;
    let lone = [DRAFT];
    let empty_under = [DRAFT, ""];
    let two = ["please also update", "the changelog"];
    let blank_first = ["", "the changelog"];
    let blank: [&str; 1] = [""];
    let cases: [Case<'_>; 16] = [
        ("no text", &blank, true, "dim", CARET, 2, Some(false), 0),
        (
            "a blank first row",
            &blank_first,
            true,
            "dim",
            CARET,
            2,
            Some(true),
            0,
        ),
        (
            "no grey text drawn",
            &lone,
            false,
            "dim",
            CARET,
            2,
            Some(true),
            0,
        ),
        ("plain, home", &lone, true, "none", CARET, 2, Some(true), 1),
        ("plain, past", &lone, true, "none", CARET, 30, Some(true), 1),
        ("plain, off", &lone, true, "none", 2, 40, Some(true), 1),
        (
            "plain, two rows",
            &two,
            true,
            "none",
            CARET + 1,
            15,
            Some(true),
            1,
        ),
        ("grey, home", &lone, true, "dim", CARET, 2, Some(false), 1),
        (
            "grey, a row under, off it",
            &lone,
            true,
            "dim",
            CARET + 1,
            2,
            Some(false),
            1,
        ),
        (
            "grey, column 2 above",
            &lone,
            true,
            "dim",
            2,
            2,
            Some(false),
            1,
        ),
        (
            "grey, two rows, home",
            &two,
            true,
            "dim",
            CARET,
            2,
            Some(false),
            1,
        ),
        ("grey, past", &lone, true, "dim", CARET, 30, None, 1),
        ("grey, off", &lone, true, "dim", 2, 40, None, 1),
        (
            "grey, later row, column 2",
            &empty_under,
            true,
            "dim",
            CARET + 1,
            2,
            None,
            1,
        ),
        (
            "grey, later row, column 4",
            &empty_under,
            true,
            "dim",
            CARET + 1,
            4,
            None,
            1,
        ),
        (
            "grey, two rows, past",
            &two,
            true,
            "dim",
            CARET + 1,
            15,
            None,
            1,
        ),
    ];
    let verdict = |m: &mut Mock, lines: &[&str], placeholder: bool, row: usize, col: usize| {
        let lines: Vec<String> = lines.iter().map(ToString::to_string).collect();
        let screen = Screen {
            rows: vec![String::new(); 20],
            cursor_row: row,
            cursor_col: col,
            ..Screen::default()
        };
        let mut s = session(m, None);
        s.composer_draft(Some(&(CARET, lines)), placeholder, None, &screen)
    };
    for (what, lines, placeholder, attrs, row, col, want, cells) in cases {
        let mut m = Mock::new(vec![ended(SAID)]);
        m.cell_attrs = attrs;
        m.dim_past = true;
        assert_eq!(
            verdict(&mut m, lines, placeholder, row, col),
            want,
            "{what}"
        );
        let read = m.requests.iter().filter(|r| r.starts_with("cell ")).count();
        assert_eq!(read, cells, "{what}: {:#?}", m.requests);
    }
    let mut refused = Mock::new(vec![ended(SAID)]);
    refused.cell_unreadable = true;
    assert_eq!(
        verdict(&mut refused, &lone, true, CARET, 2),
        None,
        "a cell refused"
    );
    let mut open = Mock::new(vec![ended(SAID)]);
    open.cell_open_reads = 1;
    assert_eq!(
        verdict(&mut open, &lone, true, CARET, 2),
        None,
        "a frame open"
    );
}

/// A REAL DRAFT STANDS ITS FULL GRACE, its caret moved home or not: typed text (no dim) with the
/// cursor after it, then the same rows with the cursor home on column 2, no key measured. Nothing
/// decides again until the grace from its first sight is out, and then the policy's draft step sends
/// it: one Enter, nothing typed over it.
/// NEGATIVE CONTROLS: `see_draft` dropping the same words read home, and each decision sees the
/// draft anew: five `WAITING`s, never sent; the policy's draft step without its `Submit` over
/// `Composer::Typed`, and the continuation is typed over the draft.
#[test]
fn a_real_draft_is_held_its_full_grace_and_then_submitted() {
    let mut m = fenced(vec![busy_screen(), typed_in(SAID, DRAFT)]);
    m.cell_attrs = "none";
    m.cursor_on_caret = true;
    m.cursor_col = Some(2 + DRAFT.chars().count());
    m.cursor_home_after = Some(HOME);
    m.turn_releases = Some(1);
    m.exit_when = Some(acted);
    let t = held_run("draft-real", m);
    assert_eq!(t.person_waits(), 1, "{}", t.evidence());
    let seen = t.first_read_of(1);
    let enter = t.enter_at().unwrap_or_else(|| panic!("{}", t.evidence()));
    assert!(
        enter >= seen + Duration::from_secs(120),
        "sent {:?} after it was first seen, inside its grace: {}",
        enter - seen,
        t.evidence()
    );
    assert_eq!(
        (t.enters(), t.typed()),
        (1, 0),
        "sent once, never typed over: {}",
        t.evidence()
    );
}

/// A DRAFT CLEARED HOLDS ITS GRACE, A KEY MEASURED OR NOT: the draft read, then cleared at the
/// next read by an input write no window stamped (another program's key). With the window's key
/// just measured the grace rests on the key; with none it rests on the draft, and a composer read
/// empty cannot be told from a redraw stalled before the draft's text, so neither is decided again
/// on the clear: the point waits the 120 s and only then is the continuation typed.
/// NEGATIVE CONTROL: a person's grace held as `draft_look` reads it (`draft_holds` true of
/// `Composer::Typed` at a `WaitUntil`), and the unkeyed run types the continuation at the clear's
/// read.
#[test]
fn a_draft_cleared_holds_its_grace_keyed_or_not() {
    for keyed in [false, true] {
        let mut m = fenced(vec![busy_screen(), typed_in(SAID, DRAFT), ended(SAID)]);
        m.cell_attrs = "none";
        m.cursor_on_caret = true;
        m.screen_cols.insert(1, 2 + DRAFT.chars().count());
        // A wait the screens no longer answer passes its own timeout on the run's clock, a step
        // of at most a second, so the 120 s grace is a few hundred round trips, not one per
        // millisecond of it.
        m.stall_sleep = Some(Duration::from_secs(1));
        if keyed {
            m.keyed_at_read = Some(1);
        }
        m.exit_when = Some(acted);
        let tag = if keyed {
            "draft-keyed"
        } else {
            "draft-unkeyed"
        };
        let t = held_run(tag, m);
        assert_eq!(t.person_waits(), 1, "{tag}: {}", t.evidence());
        let seen = t.first_read_of(1);
        let sent = t.typed_at().unwrap_or_else(|| panic!("{}", t.evidence()));
        assert!(
            sent >= seen + Duration::from_secs(119),
            "{tag}: typed {:?} after the draft was first read, inside its grace: {}",
            sent - seen,
            t.evidence()
        );
        assert_eq!(t.enters(), 0, "{tag}: {}", t.evidence());
    }
}

/// THE AGY L13 SHAPE (`docs/measured/forever-r11-2026-10-22.md`, `r11cv-agy`): the harness's own
/// act read half drawn and garbled at its echo (`typed, not submitted (the composer does not show
/// it as written)`), then whole. Those are the loop's own words, no person's draft: nothing waits
/// out a person's grace for nobody, and the policy's draft step finishes the act with its Enter
/// at the read that shows it whole.
/// NEGATIVE CONTROL: `own_words` answering false, and the loop's own act journals `WAITING … a
/// person is typing`.
#[test]
fn the_harnesss_own_half_echoed_act_is_no_persons_draft() {
    use aterm_phase::agy::fixtures as a;
    use aterm_phase::prompt::fixtures::screen;
    let ended = screen(a::END_OF_TURN);
    let caret = aterm_phase::agy::composer(&ended).expect("the composer");
    let with = |text: &str| {
        let mut r = ended.clone();
        r[caret] = format!("> {text}");
        r
    };
    let (dir, path) = journal_file("draft-own");
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..hosted(600)
    };
    let t = fenced_echo_under(
        vec![
            screen(a::BUSY),
            ended.clone(),
            with("keep goi7 ch"),
            with("keep going"),
        ],
        "agy",
        &opts,
        |m| {
            m.exit_when = Some(|m| {
                m.requests
                    .iter()
                    .any(|r| r.starts_with("key ") && r.ends_with(" enter"))
            });
        },
    );
    let journal: Vec<String> = journal_records(&path)
        .0
        .into_iter()
        .map(|r| r.line)
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        t.left_typed()
            .is_some_and(|a| a.contains("the composer does not show it as written")),
        "the echo read as the lab's was: {}",
        t.evidence()
    );
    assert!(
        !journal.iter().any(|l| l.ends_with(PERSON_TYPING)),
        "{journal:#?}\n{}",
        t.evidence()
    );
    let whole = t
        .first_read_of(3)
        .unwrap_or_else(|| panic!("{}", t.evidence()));
    let enter = t
        .enter_at()
        .unwrap_or_else(|| panic!("{journal:#?}\n{}", t.evidence()));
    assert!(
        enter >= whole && enter - whole < Duration::from_secs(10),
        "the act finished at the read that showed it whole, not {:?} after: {journal:#?}\n{}",
        enter.saturating_duration_since(whole),
        t.evidence()
    );
    assert_eq!(t.sends().len(), 1, "written once: {}", t.evidence());
}

/// A PERSON'S DRAFT FIRST READ WITH THE CURSOR OFF THE COMPOSER KEEPS ITS FULL GRACE: typed text
/// (no dim) read first with the cursor on the composer's top rule (a frame drawn before Claude
/// Code places its cursor), at the first two or three reads, then with the cursor after its text,
/// no key measured. The glyph's cell says typed, and the draft is seen from that read on, so the
/// point waits the person's grace from it and only then does the draft step send it.
/// NEGATIVE CONTROL: `composer_typed` never calling `cell_saw`, and the Enter goes at the first read,
/// no `WAITING … a person is typing` journaled.
#[test]
fn a_draft_first_read_off_the_composer_keeps_its_full_grace() {
    for off in [2, 3] {
        let mut m = fenced(vec![busy_screen(), tall_with(DRAFT)]);
        m.cell_attrs = "none";
        m.caret_after_reads = Some(off);
        m.screen_cols.insert(1, 2 + DRAFT.chars().count());
        m.turn_releases = Some(1);
        m.stall_sleep = Some(Duration::from_secs(1));
        m.exit_when = Some(acted);
        let t = held_run(&format!("draft-off-composer-{off}"), m);
        let seen = t.first_read_of(1);
        let acted = t.acted_at().unwrap_or_else(|| panic!("{}", t.evidence()));
        assert!(
            acted >= seen + Duration::from_secs(119),
            "off={off}: acted {:?} after the draft was first read: {}",
            acted - seen,
            t.evidence()
        );
        assert_eq!(t.person_waits(), 1, "off={off}: {}", t.evidence());
        assert_eq!(
            (t.enters(), t.typed()),
            (1, 0),
            "off={off}: the draft sent once, nothing typed over it: {}",
            t.evidence()
        );
    }
}

/// THE SAME IN MUSE CODE'S SPLIT FRAME: a person's draft (its cell plain, its colour Muse's typed
/// `ccd3db`, not the hint's `676c74`) read at every look inside Muse's bottom-pane redraw, the cursor
/// on the footer's row, no key measured. Its cell tells it from the hint
/// ([`a_muse_hint_is_no_draft_wherever_its_frame_left_the_cursor`]), so it is seen and held its grace
/// from its first read.
/// NEGATIVE CONTROL: as [`a_draft_first_read_off_the_composer_keeps_its_full_grace`]'s.
#[test]
fn a_muse_draft_read_inside_its_redraw_keeps_its_full_grace() {
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    let drafted = muse_typed(DRAFT);
    let footer = muse_footer(&drafted);
    let mut m = muse_fenced(vec![screen(u::BUSY_1_4_10), drafted]);
    m.screen_cursors.remove(&1);
    m.cursor_row = Some(footer);
    m.cursor_col = Some(40);
    m.cell_attrs = "none";
    m.turn_releases = Some(1);
    m.stall_sleep = Some(Duration::from_secs(1));
    m.exit_when = Some(acted);
    let t = held_run("draft-muse-real-split", m);
    let seen = t.first_read_of(1);
    let acted = t.acted_at().unwrap_or_else(|| panic!("{}", t.evidence()));
    assert!(
        acted >= seen + Duration::from_secs(119),
        "acted {:?} after the draft was first read: {}",
        acted - seen,
        t.evidence()
    );
    assert_eq!(t.person_waits(), 1, "{}", t.evidence());
    assert_eq!((t.enters(), t.typed()), (1, 0), "{}", t.evidence());
}

/// `typed_in(SAID, "hello")` with an empty second row under the caret's, and the caret's row.
fn two_rows() -> (Vec<String>, usize) {
    let mut r = typed_in(SAID, "hello");
    let caret = r
        .iter()
        .position(|row| row == "❯ hello")
        .expect("the caret's row");
    r.insert(caret + 1, "  ".to_string());
    (r, caret)
}

/// A DRAFT OF TWO ROWS, ITS CURSOR ON THE EMPTY SECOND (a Shift+Enter), no key measured: a cursor
/// on a later row of the composer stands in text, so the rows say typed (`draft_read`). The first
/// read sees it and the host's guards hold for it; its plain cell makes it a draft, and the point
/// waits the person's grace before the draft step sends it.
/// NEGATIVE CONTROL: `draft_read`'s later-row arm removed (the rows saying nothing there), and no
/// draft is seen at that read, which the host's guards read.
#[test]
fn a_two_row_draft_with_its_cursor_on_its_empty_row_is_seen() {
    let (rows, caret) = two_rows();
    let mut m = fenced(vec![rows.clone()]);
    m.cell_attrs = "none";
    m.screen_cursors.insert(0, caret + 1);
    m.cursor_col = Some(2);
    let opts = hosted(600);
    let mut s = virtual_session(&mut m, None);
    s.set_clock(TEST_NOW, PDT);
    let _ = s.screen().expect("a read");
    let now = s.fx.mono();
    let guards = s.occupancy_guards(now, &opts.policy, &rows);
    assert!(
        s.draft_seen.is_some(),
        "the draft is seen at its first read"
    );
    assert!(
        guards.draft && guards.person,
        "the host's guards hold for it"
    );
    let (rows, caret) = two_rows();
    let mut m = fenced(vec![busy_screen(), rows]);
    m.cell_attrs = "none";
    m.screen_cursors.insert(1, caret + 1);
    m.cursor_col = Some(2);
    m.turn_releases = Some(1);
    m.stall_sleep = Some(Duration::from_secs(1));
    m.exit_when = Some(acted);
    let t = held_run("draft-two-rows", m);
    let seen = t.first_read_of(1);
    let acted = t.acted_at().unwrap_or_else(|| panic!("{}", t.evidence()));
    assert!(
        acted >= seen + Duration::from_secs(119),
        "acted {:?} after the draft was first read: {}",
        acted - seen,
        t.evidence()
    );
    assert_eq!((t.enters(), t.typed()), (1, 0), "{}", t.evidence());
}

/// A COMPOSER READ EMPTY DECIDES NOTHING, FOR A MOMENT OR FOR GOOD: a draft no key stamped holds the
/// point; 20 s on, a read finds the composer drawn and empty. Drawn again 400 ms later (a redraw read
/// between the program's erase and its text), the draft is never typed over, and the draft step
/// sends it once its grace from its return is out. Empty for good (another writer cleared it), it is
/// still not decided again inside the draft's grace, as no read tells it from a redraw stalled
/// there: the continuation is typed once the grace from the draft's first sight is out.
/// NEGATIVE CONTROL: as [`a_draft_cleared_holds_its_grace_keyed_or_not`]'s, and Cruise's continuation
/// is typed on the empty read, a moment before the draft comes back.
#[test]
fn a_composer_read_empty_decides_nothing() {
    let run = |back: bool| {
        let mut screens = vec![busy_screen(), typed_in(SAID, DRAFT), ended(SAID)];
        let mut at = vec![Duration::ZERO, Duration::from_secs(20)];
        if back {
            screens.push(typed_in(SAID, DRAFT));
            at.push(Duration::from_millis(20_400));
        }
        let mut m = fenced(screens);
        m.cell_attrs = "none";
        m.cursor_on_caret = true;
        m.screen_cols.insert(1, 2 + DRAFT.chars().count());
        m.screen_cols.insert(2, 2);
        m.screen_cols.insert(3, 2 + DRAFT.chars().count());
        m.timeline = Some((1, at));
        m.stall_sleep = Some(Duration::from_secs(3600));
        m.exit_when = Some(acted);
        held_run(
            if back {
                "draft-blank-moment"
            } else {
                "draft-blank-stays"
            },
            m,
        )
    };
    let t = run(true);
    let back = t.first_read_of(3);
    let acted = t.acted_at().unwrap_or_else(|| panic!("{}", t.evidence()));
    assert!(
        acted >= back + Duration::from_secs(119),
        "acted {:?} after the draft was drawn again: {}",
        acted.saturating_duration_since(back),
        t.evidence()
    );
    assert_eq!(
        (t.enters(), t.typed()),
        (1, 0),
        "the draft sent once, nothing typed on the empty read: {}",
        t.evidence()
    );
    let t = run(false);
    let seen = t.first_read_of(1);
    let typed = t.typed_at().unwrap_or_else(|| panic!("{}", t.evidence()));
    assert!(
        typed >= seen + Duration::from_secs(119),
        "the cleared draft decided again {:?} after its first sight, inside its grace: {}",
        typed.saturating_duration_since(seen),
        t.evidence()
    );
    assert_eq!((t.enters(), t.typed()), (0, 1), "{}", t.evidence());
}

/// A PERSON'S GRACE READS THE SCREEN AT THE WAIT'S OWN PACE: a real draft stands its 120 s, no key
/// measured, each wait running its whole timeout on the run's clock, a wait step (20 s) between
/// reads; nothing polls it.
/// NEGATIVE CONTROL: every step of `wait_for_next` capped at `HANDBACK_POLL`, about 60 reads.
#[test]
fn a_persons_grace_reads_the_screen_at_the_waits_pace() {
    let mut m = fenced(vec![busy_screen(), typed_in(SAID, DRAFT)]);
    m.cell_attrs = "none";
    m.cursor_on_caret = true;
    m.cursor_col = Some(2 + DRAFT.chars().count());
    m.turn_releases = Some(1);
    m.stall_sleep = Some(Duration::from_secs(3600));
    m.exit_when = Some(acted);
    let t = held_run("draft-rest-pace", m);
    let seen = t.first_read_of(1);
    let acted = t.acted_at().unwrap_or_else(|| panic!("{}", t.evidence()));
    assert!(acted >= seen + Duration::from_secs(120), "{}", t.evidence());
    let reads = t.reads_between(seen);
    assert!(
        reads <= 120 / 20 + 4,
        "{reads} reads in the hold: {}",
        t.evidence()
    );
    assert_eq!((t.enters(), t.typed()), (1, 0), "{}", t.evidence());
}

/// A person's draft (no key measured) seen; 20 s on, a redraw is read with its composer drawn
/// empty for `gap`, then the draft again for good: Claude Code's frames, or Muse Code's unbracketed
/// pane.
fn stalled_redraw(tag: &str, muse: bool, gap: Duration) -> Held {
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    let mut m = if muse {
        muse_fenced(vec![
            screen(u::BUSY_1_4_10),
            muse_typed(DRAFT),
            muse_typed(""),
            muse_typed(DRAFT),
        ])
    } else {
        let mut m = fenced(vec![
            busy_screen(),
            typed_in(SAID, DRAFT),
            ended(SAID),
            typed_in(SAID, DRAFT),
        ]);
        m.cursor_on_caret = true;
        m
    };
    m.cell_attrs = "none";
    m.screen_cols.insert(1, 2 + DRAFT.chars().count());
    m.screen_cols.insert(2, 2);
    m.screen_cols.insert(3, 2 + DRAFT.chars().count());
    m.timeline = Some((
        1,
        vec![
            Duration::ZERO,
            Duration::from_secs(20),
            Duration::from_secs(20) + gap,
        ],
    ));
    m.stall_sleep = Some(Duration::from_secs(3600));
    m.exit_when = Some(acted);
    held_run(tag, m)
}

/// A STALLED REDRAW TYPES NOTHING OVER THE DRAFT (the reviews of `b6b17dd4e6` and `d7b6e8aa23`):
/// under load a program's writer stalls inside a redraw for longer than any settle rules out
/// (`docs/measured/half-drawn-frame-2026-10-22.md`), so a read can find the composer drawn empty
/// for seconds while the program still holds the person's draft. The point is not decided again,
/// nothing is typed into the program's draft, and the draft step sends it once its grace from its
/// return is out, as on Claude Code's frames so on Muse Code's unbracketed pane.
/// NEGATIVE CONTROL: as [`a_draft_cleared_holds_its_grace_keyed_or_not`]'s, and Cruise's continuation
/// is typed on the empty read (20 s), the draft drawn again a moment later.
#[test]
fn a_stalled_redraw_types_nothing_over_the_draft() {
    for muse in [false, true] {
        for gap_ms in [1_500_u64, 3_000] {
            let tag = format!(
                "draft-stalled-{}-{gap_ms}",
                if muse { "muse" } else { "claude" },
            );
            let t = stalled_redraw(&tag, muse, Duration::from_millis(gap_ms));
            let back = t.first_read_of(3);
            let acted = t.acted_at().unwrap_or_else(|| panic!("{}", t.evidence()));
            assert!(
                acted >= back + Duration::from_secs(119),
                "{tag}: acted {:?} after the draft was drawn again: {}",
                acted.saturating_duration_since(back),
                t.evidence()
            );
            assert_eq!(
                (t.enters(), t.typed()),
                (1, 0),
                "{tag}: the draft sent once, nothing typed over it: {}",
                t.evidence()
            );
        }
    }
}

/// GREY TEXT OVER A HELD DRAFT DECIDES NOTHING EITHER (the review of `6337470849`): a draft no key
/// stamped holds the point; 20 s on, a read finds the program's own grey text with other words where
/// it stood (a cleared Claude Code composer shows its dim suggestion, `docs/measured/
/// claude-code-dialogs-2026-09-24.md`, Muse Code's idle composer its tip), the cursor home or off the
/// composer. It stands for a moment and the draft comes back, or it stands for good. No read tells a
/// redraw from a clear here either, so nothing is typed inside the draft's grace. The draft step
/// sends a draft that came back once its grace is out: from its return where the grey read's cursor
/// was home, as that read let the draft go, from its first sight where it was off the composer, as
/// that read says nothing and the same words came back. A draft that never did is typed over only
/// once the grace from its first sight is out, and only with the cursor home: off the composer,
/// grey for good says neither, and is left alone.
/// NEGATIVE CONTROL: as [`a_draft_cleared_holds_its_grace_keyed_or_not`]'s, and the continuation is
/// typed at 20 s, over the draft the program still holds.
#[test]
fn grey_text_over_a_held_draft_decides_nothing() {
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    let other = "/export saves the conversation to a text file";
    let held = |muse: bool, off: bool, gap: Option<Duration>| {
        let typed = |text: &str| {
            if muse {
                muse_typed(text)
            } else {
                typed_in(SAID, text)
            }
        };
        let mut screens = vec![
            if muse {
                screen(u::BUSY_1_4_10)
            } else {
                busy_screen()
            },
            typed(DRAFT),
            typed(other),
        ];
        let mut at = vec![Duration::ZERO, Duration::from_secs(20)];
        if let Some(gap) = gap {
            screens.push(typed(DRAFT));
            at.push(Duration::from_secs(20) + gap);
        }
        let grey_caret = if muse {
            aterm_phase::muse::composer(&screens[2])
        } else {
            screens[2].iter().position(|r| r.starts_with('❯'))
        }
        .expect("the caret's row");
        let mut m = if muse {
            muse_fenced(screens)
        } else {
            let mut m = fenced(screens);
            m.cursor_on_caret = true;
            m
        };
        m.cell_attrs = "none";
        if muse {
            m.cell_fg_on.insert(2, "676c74");
        } else {
            m.cell_attrs_on.insert(2, GREY);
        }
        m.screen_cols.insert(1, 2 + DRAFT.chars().count());
        m.screen_cols.insert(2, if off { 40 } else { 2 });
        m.screen_cols.insert(3, 2 + DRAFT.chars().count());
        m.screen_cursors
            .insert(2, if off { grey_caret - 1 } else { grey_caret });
        m.timeline = Some((1, at));
        m.stall_sleep = Some(Duration::from_secs(3600));
        m.exit_when = Some(acted);
        held_run("draft-grey-over", m)
    };
    for muse in [false, true] {
        for off in [false, true] {
            let what = format!("muse {muse}, off {off}");
            for gap_ms in [400_u64, 1_500, 3_000] {
                let t = held(muse, off, Some(Duration::from_millis(gap_ms)));
                let from = t.first_read_of(if off { 1 } else { 3 });
                let acted = t.acted_at().unwrap_or_else(|| panic!("{}", t.evidence()));
                assert!(
                    acted >= from + Duration::from_secs(119),
                    "{what}, {gap_ms} ms: acted {:?} into its grace: {}",
                    acted.saturating_duration_since(from),
                    t.evidence()
                );
                assert_eq!(
                    (t.enters(), t.typed()),
                    (1, 0),
                    "{what}, {gap_ms} ms: {}",
                    t.evidence()
                );
            }
            let t = held(muse, off, None);
            if off {
                assert_eq!((t.enters(), t.typed()), (0, 0), "{what}: {}", t.evidence());
                continue;
            }
            let seen = t.first_read_of(1);
            let typed = t.typed_at().unwrap_or_else(|| panic!("{}", t.evidence()));
            assert!(
                typed >= seen + Duration::from_secs(119),
                "{what}: typed {:?} after the draft's first sight, inside its grace: {}",
                typed.saturating_duration_since(seen),
                t.evidence()
            );
            assert_eq!((t.enters(), t.typed()), (0, 1), "{what}: {}", t.evidence());
        }
    }
}

/// A NEW POINT READ GREY OFF THE COMPOSER KEEPS THE DRAFT (the review of `5ad7f68c8f`): a draft no key
/// stamped holds the point; 20 s on, a row above the composer moves, so the point's key moves, in a
/// frame whose composer reads grey with other words or the same, the cursor off it, and 400 ms on the
/// draft is back. That read says neither, so it costs the draft nothing: nothing is typed, and the
/// draft step sends it once the grace from its first sight is out, no later.
/// NEGATIVE CONTROLS: `composer_draft` taking grey for a suggestion wherever the cursor stands, and the
/// continuation is typed at 20 s, into the draft the program still holds; `cell_saw` dropping the
/// draft over a read that could not say, and it is sent at 140 s, the draft's grace started again.
#[test]
fn a_new_point_read_grey_off_the_composer_keeps_the_draft() {
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    const MOVED: &str = "Stage 2 is in too.";
    let other = "/export saves the conversation to a text file";
    let times = vec![
        Duration::ZERO,
        Duration::from_secs(20),
        Duration::from_millis(20_400),
    ];
    let cols = |m: &mut Mock| {
        m.cell_attrs = "none";
        m.cursor_on_caret = true;
        m.screen_cols.insert(1, 2 + DRAFT.chars().count());
        m.screen_cols.insert(2, 40);
        m.screen_cols.insert(3, 2 + DRAFT.chars().count());
        m.stall_sleep = Some(Duration::from_secs(3600));
        m.exit_when = Some(acted);
    };
    let settled = |what: &str, t: &Held| {
        let seen = t.first_read_of(1);
        let acted = t
            .acted_at()
            .unwrap_or_else(|| panic!("{what}: {}", t.evidence()));
        assert!(
            acted >= seen + Duration::from_secs(119) && acted < seen + Duration::from_secs(125),
            "{what}: acted {:?} after the draft's first read: {}",
            acted.saturating_duration_since(seen),
            t.evidence()
        );
        assert_eq!((t.enters(), t.typed()), (1, 0), "{what}: {}", t.evidence());
    };
    for shown in [other, DRAFT] {
        let mut m = fenced(vec![
            busy_screen(),
            typed_in(SAID, DRAFT),
            typed_in(MOVED, shown),
            typed_in(MOVED, DRAFT),
        ]);
        cols(&mut m);
        m.cell_attrs_on.insert(2, GREY);
        m.screen_cursors.insert(2, 0);
        m.timeline = Some((1, times.clone()));
        settled(shown, &held_run("draft-new-point-grey", m));
    }
    let moved = |text: &str| {
        let mut r = muse_typed(text);
        let caret = aterm_phase::muse::composer(&r).expect("the composer");
        let above = (0..caret - 1)
            .rev()
            .find(|&i| !r[i].trim().is_empty())
            .expect("a row above");
        r[above] = format!("◆ {MOVED}");
        r
    };
    let grey = moved(other);
    let footer = muse_footer(&grey);
    let mut m = muse_fenced(vec![
        screen(u::BUSY_1_4_10),
        muse_typed(DRAFT),
        grey,
        moved(DRAFT),
    ]);
    cols(&mut m);
    m.cell_fg_on.insert(2, "676c74");
    m.screen_cursors.remove(&1);
    m.screen_cursors.insert(2, footer);
    m.timeline = Some((1, times));
    settled("Muse", &held_run("draft-new-point-grey-muse", m));
}

/// A PERSON'S DRAFT OVER A MENU ITS ENTER WOULD RUN HOLDS ITS GRACE AND IS ASKED ABOUT: a person's
/// `/config` over Claude Code's slash menu with `/update-config` selected (its Enter would run the
/// selection: `Composer::Diverted`), no key measured, standing for good, or redrawn the same at new
/// seqs 20 s and 40 s on. Its grace is waited out at the wait's pace, the point is asked about
/// (`MENU_DIVERTS`) once it is out, and nothing is ever sent.
/// NEGATIVE CONTROL: the policy reading a draft over a menu as `Composer::Typed` (`over_menu`
/// ignored), and the draft step sends it with its Enter on the selection.
#[test]
fn a_draft_over_a_menu_its_enter_would_run_holds_its_grace_and_is_asked_about() {
    use aterm_phase::prompt::fixtures::{
        SLASH_MENU_CONFIG_2_1_268, SLASH_MENU_CONFIG_DOWN_CELLS_2_1_268, cells, screen,
    };
    let mut at_end = screen(SLASH_MENU_CONFIG_2_1_268);
    at_end[3] = format!("⏺ {SAID}");
    at_end[4] = "✻ Worked for 12m 2s · done 4:24 PM".to_string();
    let caret = at_end
        .iter()
        .position(|r| r.starts_with('❯') && r.trim_end().ends_with("/config"))
        .expect("the caret row");
    for redrawn in [false, true] {
        let mut m = if redrawn {
            let mut m = fenced(vec![
                busy_screen(),
                at_end.clone(),
                at_end.clone(),
                at_end.clone(),
            ]);
            m.timeline = Some((
                1,
                vec![
                    Duration::ZERO,
                    Duration::from_secs(20),
                    Duration::from_secs(40),
                ],
            ));
            m
        } else {
            let mut m = fenced(vec![busy_screen(), at_end.clone()]);
            m.turn_releases = Some(1);
            m
        };
        m.cursor_row = Some(caret);
        m.cursor_col = Some(2 + "/config".chars().count());
        m.cells = cells(SLASH_MENU_CONFIG_DOWN_CELLS_2_1_268)
            .into_iter()
            .map(|(r, c, reply)| ((r, c), reply))
            .collect();
        m.stall_sleep = Some(Duration::from_secs(3600));
        m.exit_when = Some(acted_or_asked);
        let tag = if redrawn {
            "draft-held-diverted-redrawn"
        } else {
            "draft-held-diverted"
        };
        let t = held_run(tag, m);
        assert_eq!(t.person_waits(), 1, "{tag}: {}", t.evidence());
        let seen = t.first_read_of(1);
        let told =
            t.m.attention_at
                .unwrap_or_else(|| panic!("{tag}: asked about: {}", t.evidence()));
        assert!(
            told >= seen + Duration::from_secs(119),
            "{tag}: asked {:?} after the draft was first read, inside its grace: {}",
            told.saturating_duration_since(seen),
            t.evidence()
        );
        let reason = &MENU_DIVERTS[..60];
        assert!(
            t.m.requests
                .iter()
                .any(|r| r.starts_with("meta set attention") && r.contains(reason)),
            "{tag}: {}",
            t.evidence()
        );
        let reads =
            t.m.read_times
                .iter()
                .filter(|(_, at)| (seen..=told).contains(at))
                .count();
        assert!(
            reads <= 40,
            "{tag}: {reads} reads in a 120 s hold: {}",
            t.evidence()
        );
        assert_eq!((t.enters(), t.typed()), (0, 0), "{tag}: {}", t.evidence());
    }
}

/// ATERM'S OWN UPGRADE WORDS ARE NO PERSON'S DRAFT (the review of `b6b17dd4e6`): a line headed by any
/// of its upgrade words (`upgrade::own_words`, the policy's `Composer::Own`) standing typed in the
/// composer, no key measured. Nobody typed it, so no person's grace waits 120 s on it: the policy's
/// draft step asks about it (`OWN_WORDS_STAND`) at once and presses nothing. A person's draft quoting a
/// whole head reads the same, and no read tells them apart: that person is asked about their line a
/// grace early, with nothing written and no Enter, as the other half of the rule forbids holding
/// aterm's own words as theirs. A key the window measured holds it (the policy's person).
/// NEGATIVE CONTROLS: `own_words` without its `upgrade::own_words` arm, and the point waits `WAITING …
/// a person is typing` for a person who typed nothing; `upgrade::own_words` without any one head,
/// and that head's line does.
#[test]
fn aterms_own_upgrade_words_are_asked_about_with_no_persons_grace() {
    use crate::harness::upgrade::{ANNOUNCE_HEAD, RELEASE_HEAD, TELL_HEAD};
    for head in [ANNOUNCE_HEAD, RELEASE_HEAD, TELL_HEAD] {
        let line = format!("{head}Claude Code 2.1.400 is installed");
        let mut m = fenced(vec![busy_screen(), typed_in(SAID, &line)]);
        m.cell_attrs = "none";
        m.cursor_on_caret = true;
        m.cursor_col = Some(2 + line.chars().count());
        m.turn_releases = Some(1);
        m.stall_sleep = Some(Duration::from_secs(3600));
        m.exit_when = Some(acted_or_asked);
        let t = held_run("draft-upgrade-words", m);
        assert_eq!(t.person_waits(), 0, "{head}: {}", t.evidence());
        let seen = t.first_read_of(1);
        let told =
            t.m.attention_at
                .unwrap_or_else(|| panic!("{head}: asked about: {}", t.evidence()));
        assert!(
            told < seen + Duration::from_secs(10),
            "{head}: asked {:?} after the words were first read: {}",
            told.saturating_duration_since(seen),
            t.evidence()
        );
        let reason = &OWN_WORDS_STAND[..40];
        assert!(
            t.m.requests
                .iter()
                .any(|r| r.starts_with("meta set attention") && r.contains(reason)),
            "{head}: {}",
            t.evidence()
        );
        assert_eq!((t.enters(), t.typed()), (0, 0), "{head}: {}", t.evidence());
    }
}

/// A PERSON'S PART OF AN UPGRADE HEAD IS THEIRS (the review of `6337470849`): `[a`, `[aterm` and
/// `[aterm harness]` are a person's few glyphs as much as they are the start of aterm's line, no key
/// measured. Each keeps its grace; only then does the policy, which takes a part of a head for
/// aterm's own (`upgrade::own_draft`), ask about it. Nothing is pressed.
/// NEGATIVE CONTROL: `own_words` asking `upgrade::own_draft`, and each is asked about 5 ms after its
/// first read, inside the person's grace.
#[test]
fn a_persons_part_of_an_upgrade_head_keeps_its_grace() {
    for draft in ["[a", "[aterm", "[aterm harness]"] {
        let mut m = fenced(vec![busy_screen(), typed_in(SAID, draft)]);
        m.cell_attrs = "none";
        m.cursor_on_caret = true;
        m.cursor_col = Some(2 + draft.chars().count());
        m.turn_releases = Some(1);
        m.stall_sleep = Some(Duration::from_secs(3600));
        m.exit_when = Some(acted_or_asked);
        let t = held_run("draft-upgrade-part", m);
        assert_eq!(t.person_waits(), 1, "{draft:?}: {}", t.evidence());
        let seen = t.first_read_of(1);
        let told =
            t.m.attention_at
                .unwrap_or_else(|| panic!("{draft:?}: asked about: {}", t.evidence()));
        assert!(
            told >= seen + Duration::from_secs(119),
            "{draft:?}: asked {:?} after its first read: {}",
            told.saturating_duration_since(seen),
            t.evidence()
        );
        assert_eq!(
            (t.enters(), t.typed()),
            (0, 0),
            "{draft:?}: {}",
            t.evidence()
        );
    }
}

/// `typed_in(SAID, first)` with a second composer row `  second` under the caret's, and the
/// caret's row.
fn wrapped(first: &str, second: &str) -> (Vec<String>, usize) {
    let mut r = typed_in(SAID, first);
    let caret = r
        .iter()
        .position(|row| *row == format!("❯ {first}"))
        .expect("the caret's row");
    r.insert(caret + 1, format!("  {second}"));
    (r, caret)
}

/// A DRAFT REFLOWED ONLY STRETCHES ITS HOLD: a person's draft soft-wrapped over two rows, its caret
/// home or after its text, no key measured; 20 s on the window is made wider and the same words reflow
/// onto one row. No read tells that from a person's edit ([`a_persons_edit_of_a_drafts_whitespace_starts_a_new_grace`]),
/// so the reflowed read is a draft seen from that read on: the host's guards hold across it and the
/// point waits a grace from it, a full one, before the draft step sends it.
/// NEGATIVE CONTROL: `see_draft` keeping no draft on other text, and the host's guards hold nothing
/// over the reflowed read.
#[test]
fn a_draft_reflowed_only_stretches_its_hold() {
    let (two, caret) = wrapped("please also update", "the changelog");
    let one = typed_in(SAID, DRAFT);
    let mut m = fenced(vec![two.clone(), one.clone()]);
    m.cell_attrs = "none";
    m.screen_cursors.insert(0, caret + 1);
    m.screen_cols.insert(0, 2 + "the changelog".chars().count());
    m.cursor_on_caret = true;
    m.screen_cols.insert(1, 2 + DRAFT.chars().count());
    let opts = hosted(600);
    let mut s = virtual_session(&mut m, None);
    s.set_clock(TEST_NOW, PDT);
    let _ = s.screen().expect("the two rows");
    assert!(s.draft_seen.is_some(), "the draft seen at its first read");
    let _ = s.screen().expect("the reflowed row");
    let now = s.fx.mono();
    let guards = s.occupancy_guards(now, &opts.policy, &one);
    assert!(
        guards.draft,
        "the host's draft guard holds across the reflow"
    );

    for past in [false, true] {
        let mut m = fenced(vec![busy_screen(), two.clone(), one.clone()]);
        m.cell_attrs = "none";
        m.screen_cursors
            .insert(1, if past { caret + 1 } else { caret });
        m.cursor_on_caret = true;
        if past {
            m.screen_cols.insert(1, 2 + "the changelog".chars().count());
            m.screen_cols.insert(2, 2 + DRAFT.chars().count());
        } else {
            m.cursor_col = Some(2);
        }
        m.timeline = Some((1, vec![Duration::ZERO, Duration::from_secs(20)]));
        m.stall_sleep = Some(Duration::from_secs(3600));
        m.exit_when = Some(acted);
        let t = held_run("draft-reflowed", m);
        let reflowed = t.first_read_of(2);
        let acted = t.acted_at().unwrap_or_else(|| panic!("{}", t.evidence()));
        assert!(
            acted >= reflowed + Duration::from_secs(119),
            "past {past}: acted {:?} after the reflow was read: {}",
            acted.saturating_duration_since(reflowed),
            t.evidence()
        );
        assert_eq!((t.enters(), t.typed()), (1, 0), "{}", t.evidence());
    }
}

/// A PERSON'S EDIT OF A DRAFT'S WHITESPACE STARTS A NEW GRACE (the reviews of `6337470849` and
/// `5ad7f68c8f`): a draft read past its cursor; 110 s on, no key measured, the same words with a space
/// taken out on as many rows, a Shift+Enter between two words, or one at its end. Each is an edit: the
/// Enter waits a grace from the edit's read, not from the first sight.
/// NEGATIVE CONTROLS: `cell_saw` and `see_draft` keeping first sight over `squashed` text, and each
/// goes at 120 s, 10 s after the edit; over `squashed` text on another number of rows, as `cac0754387`
/// did, and both Shift+Enter arms do.
#[test]
fn a_persons_edit_of_a_drafts_whitespace_starts_a_new_grace() {
    let (between, between_caret) = wrapped("please also", "update the changelog");
    let (at_its_end, end_caret) = {
        let mut r = typed_in(SAID, DRAFT);
        let caret = r
            .iter()
            .position(|row| *row == format!("❯ {DRAFT}"))
            .expect("the caret's row");
        r.insert(caret + 1, "  ".to_string());
        (r, caret)
    };
    let cases = [
        (
            "a space out",
            tall_with("please also update thechangelog"),
            10,
            40,
        ),
        (
            "a Shift+Enter between words",
            between,
            between_caret + 1,
            2 + "update the changelog".chars().count(),
        ),
        ("a Shift+Enter at its end", at_its_end, end_caret + 1, 2),
    ];
    for (what, edited, row, col) in cases {
        let first = if what == "a space out" {
            tall_with(DRAFT)
        } else {
            typed_in(SAID, DRAFT)
        };
        let caret = first
            .iter()
            .position(|r| *r == format!("❯ {DRAFT}"))
            .expect("the caret's row");
        let mut m = fenced(vec![busy_screen(), first, edited]);
        m.cell_attrs = "none";
        m.screen_cursors.insert(1, caret);
        m.screen_cols.insert(1, 2 + DRAFT.chars().count());
        m.screen_cursors.insert(2, row);
        m.screen_cols.insert(2, col);
        m.timeline = Some((1, vec![Duration::ZERO, Duration::from_secs(110)]));
        m.stall_sleep = Some(Duration::from_secs(3600));
        m.exit_when = Some(acted);
        let t = held_run("draft-edit-space", m);
        let edit = t.first_read_of(2);
        let acted = t
            .acted_at()
            .unwrap_or_else(|| panic!("{what}: {}", t.evidence()));
        assert!(
            acted >= edit + Duration::from_secs(119),
            "{what}: acted {:?} after the edit was read: {}",
            acted.saturating_duration_since(edit),
            t.evidence()
        );
        assert_eq!((t.enters(), t.typed()), (1, 0), "{what}: {}", t.evidence());
    }
}

/// A READ OFF THE COMPOSER'S ROWS SAYS NOTHING OF A DRAFT (`draft_read`, `fabric_world`'s
/// `174640090a`): a lone line, the cursor on a row above the composer or under it, as a program that
/// places its cursor last is read between the two. A read of that sees no draft, so the host's guards
/// hold nothing over a suggestion so read, and it costs a draft already seen nothing.
/// NEGATIVE CONTROLS: `draft_read`'s off-composer arm answering `Some(true)`, as on `5ece62c7b3`, or
/// either half of its condition gone, and the off read stands as a draft.
#[test]
fn a_read_off_the_composers_rows_says_nothing_of_a_draft() {
    let shown = typed_in(SAID, HINT);
    let caret = shown
        .iter()
        .position(|row| *row == format!("❯ {HINT}"))
        .expect("the caret's row");
    for off in [caret - 1, caret + 1] {
        let mut m = fenced(vec![shown.clone(), shown.clone()]);
        m.cell_attrs = "none";
        m.screen_cursors.insert(0, off);
        m.screen_cols.insert(0, 40);
        m.screen_cursors.insert(1, caret);
        m.screen_cols.insert(1, 2 + HINT.chars().count());
        let mut s = virtual_session(&mut m, None);
        s.set_clock(TEST_NOW, PDT);
        let _ = s.screen().expect("the read off the composer");
        assert!(s.draft_seen.is_none(), "row {off}: {:?}", s.draft_seen);
        let _ = s.screen().expect("the read past the line");
        let seen = s.draft_seen.clone().expect("a draft seen");

        let mut m = fenced(vec![shown.clone(), shown.clone()]);
        m.cell_attrs = "none";
        m.screen_cursors.insert(0, caret);
        m.screen_cols.insert(0, 2 + HINT.chars().count());
        m.screen_cursors.insert(1, off);
        m.screen_cols.insert(1, 40);
        let mut s = virtual_session(&mut m, None);
        s.set_clock(TEST_NOW, PDT);
        let _ = s.screen().expect("the read past the line");
        let held = s.draft_seen.clone().expect("a draft seen");
        let _ = s.screen().expect("the read off the composer");
        assert_eq!(s.draft_seen, Some(held), "row {off}: kept, over {seen:?}");
    }
}

/// A DRAFT FIRST READ WITH ITS CARET HOME IS SEEN BY ITS CELL (pre-existing on `595b785510`, the
/// review of `b6b17dd4e6`): a person's draft first read with the cursor at column 2 on the
/// caret's row (a Home pressed, an Esc's restored prompt), its cell plain, no key measured. The
/// policy's read of the cell says typed, and the draft is seen from that read on, so the point waits
/// the person's grace before the draft step sends it.
/// NEGATIVE CONTROL: `composer_typed` never calling `cell_saw`, and the draft is sent at its first
/// read, no `WAITING … a person is typing` journaled.
#[test]
fn a_draft_first_read_with_its_caret_home_keeps_its_full_grace() {
    let mut m = fenced(vec![busy_screen(), typed_in(SAID, DRAFT)]);
    m.cell_attrs = "none";
    m.cursor_on_caret = true;
    m.cursor_col = Some(2);
    m.turn_releases = Some(1);
    m.stall_sleep = Some(Duration::from_secs(1));
    m.exit_when = Some(acted);
    let t = held_run("draft-first-home", m);
    assert_eq!(t.person_waits(), 1, "{}", t.evidence());
    let seen = t.first_read_of(1);
    let acted = t.acted_at().unwrap_or_else(|| panic!("{}", t.evidence()));
    assert!(
        acted >= seen + Duration::from_secs(119),
        "acted {:?} after the draft was first read: {}",
        acted - seen,
        t.evidence()
    );
    assert_eq!((t.enters(), t.typed()), (1, 0), "{}", t.evidence());
}

/// A COMPOSER WHOSE CELL COULD NOT SAY IS LOOKED AT AGAIN (the reviews of `6337470849`): the
/// decision's read of the glyph's cell comes from a frame still open (` frame=open`), with no key
/// measured and no content seq moving after it. That read decides nothing, and the next wait step's
/// read looks again: a plain cell is a draft, which keeps its grace from its first sight and is then
/// sent; a dim one with the cursor home is a suggestion, typed over at that step. A host that refuses
/// the cell for good is never written to, its screen read at the wait's pace, no faster, and the
/// journal says what holds the point.
/// NEGATIVE CONTROLS: `wait_for_next` without its `Composer::Unknown` step, and the first two never
/// act: no read moves the point past a cell that came back; `turn_end_write` without its
/// `composer-unknown` arm, and the journal says only `why=policy`.
#[test]
fn a_composer_whose_cell_could_not_say_is_looked_at_again() {
    let run = |attrs: &'static str, col: usize| {
        let mut m = fenced(vec![busy_screen(), typed_in(SAID, HINT)]);
        m.dim_past = true;
        m.cell_attrs = attrs;
        m.cell_open_reads = 1;
        m.cursor_on_caret = true;
        m.cursor_col = Some(col);
        m.turn_releases = Some(1);
        m.stall_sleep = Some(Duration::from_secs(3600));
        m.exit_when = Some(acted);
        held_run("draft-cell-open", m)
    };
    let t = run("none", 2 + HINT.chars().count());
    let seen = t.first_read_of(1);
    let acted = t.acted_at().unwrap_or_else(|| panic!("{}", t.evidence()));
    assert!(
        acted >= seen + Duration::from_secs(119),
        "a draft sent {:?} after its first sight: {}",
        acted - seen,
        t.evidence()
    );
    assert_eq!((t.enters(), t.typed()), (1, 0), "{}", t.evidence());
    let t = run("dim", 2);
    let seen = t.first_read_of(1);
    let typed = t.typed_at().unwrap_or_else(|| panic!("{}", t.evidence()));
    assert!(
        typed > seen && typed - seen <= Duration::from_secs(21),
        "a suggestion typed over {:?} after its first read: {}",
        typed.saturating_duration_since(seen),
        t.evidence()
    );
    assert_eq!((t.enters(), t.typed()), (0, 1), "{}", t.evidence());

    let mut m = fenced(vec![busy_screen(), typed_in(SAID, HINT)]);
    m.cell_unreadable = true;
    m.cursor_on_caret = true;
    m.cursor_col = Some(2 + HINT.chars().count());
    m.turn_releases = Some(1);
    m.stall_sleep = Some(Duration::from_secs(3600));
    let t = held_run("draft-cell-refused", m);
    t.left_it_alone("refused");
    let reads = t.m.read_times.len();
    assert!(
        reads <= 2 * (600 / 20) + 4,
        "{reads} reads over the run's 600 s: {}",
        t.evidence()
    );
}

/// A PERSON'S GRACE KEEPS THE WAKE (the review of `6337470849`): on a host whose `await agent` works,
/// a real draft's grace parks on it, as any wait at an idle point does, so a turn that starts wakes
/// the loop at once and no ticking screen makes each of its moves a read. The draft is sent once its
/// grace is out.
/// NEGATIVE CONTROL: `agent_wake` answering `None` while a person's draft is seen, and no `await
/// agent` is armed in the hold: every content move is a read until the grace is out.
#[test]
fn a_persons_grace_keeps_the_wake_on_a_host_that_has_one() {
    let mut m = fenced(vec![busy_screen(), typed_in(SAID, DRAFT)]);
    m.agent_pushes = true;
    m.cell_attrs = "none";
    m.cursor_on_caret = true;
    m.cursor_col = Some(2 + DRAFT.chars().count());
    m.turn_releases = Some(1);
    m.stall_sleep = Some(Duration::from_secs(3600));
    m.exit_when = Some(acted);
    let t = held_run("draft-wake", m);
    assert_eq!(t.person_waits(), 1, "{}", t.evidence());
    let seen = t.first_read_of(1);
    let acted = t.acted_at().unwrap_or_else(|| panic!("{}", t.evidence()));
    assert!(acted >= seen + Duration::from_secs(119), "{}", t.evidence());
    let requests = &t.m.requests;
    let read_of_the_draft = requests
        .iter()
        .enumerate()
        .filter(|(_, r)| r.starts_with("text "))
        .nth(1)
        .map(|(i, _)| i);
    let enter = requests.iter().position(|r| r.ends_with(" enter"));
    assert!(
        read_of_the_draft
            .zip(enter)
            .is_some_and(|(from, to)| requests[from..to]
                .iter()
                .any(|r| r.starts_with("await agent "))),
        "the grace parks on the wake: {}",
        t.evidence()
    );
    assert_eq!((t.enters(), t.typed()), (1, 0), "{}", t.evidence());
}

/// A CELL READ FROM A LATER FRAME WRITES NOTHING (the review of `6337470849`): the rows show a person's
/// plain draft, its caret home, and its glyph's cell, read after them, answers from a frame since
/// drawn, dim. That read takes the text for grey and the continuation goes to be typed, but its write
/// carries the generation of the rows it judged (`send if-gen=`), which that frame moved past, so the
/// host writes nothing. The next read finds the draft plain, and it keeps its grace.
/// NEGATIVE CONTROL: `type_fenced`'s `send` without its `if-gen=`, and the continuation is typed over
/// the draft, inside its grace.
#[test]
fn a_cell_read_from_a_later_frame_writes_nothing() {
    let mut m = fenced(vec![busy_screen(), typed_in(SAID, DRAFT)]);
    m.help = FENCED_HELP.to_string();
    m.human = vec!["null"];
    m.human_seq = Some(7);
    m.cell_attrs = "none";
    m.cell_later_frames = 2;
    m.cursor_on_caret = true;
    m.cursor_col = Some(2);
    m.turn_releases = Some(1);
    m.stall_sleep = Some(Duration::from_secs(1));
    m.exit_when = Some(|m| {
        m.requests
            .iter()
            .any(|r| r.starts_with("key ") && r.ends_with(" enter"))
    });
    let t = held_run("draft-later-frame", m);
    assert!(
        t.journal
            .iter()
            .any(|l| l.contains("the screen moved before the write")),
        "the fence refused the write: {}",
        t.evidence()
    );
    let seen = t.first_read_of(1);
    let enter = t.enter_at().unwrap_or_else(|| panic!("{}", t.evidence()));
    assert!(
        enter >= seen + Duration::from_secs(119),
        "sent {:?} after the draft was first read: {}",
        enter - seen,
        t.evidence()
    );
    assert_eq!(t.enters(), 1, "{}", t.evidence());
}

/// `text` standing in the composer at a break of the agent's own background work, and the caret's row.
fn at_break(text: &str) -> (Vec<String>, usize) {
    let mut r = rows(&[
        "⏺ Waiting for the build.",
        "",
        "✻ Crunched for 9m 27s · done 11:07 AM · 1 shell still running",
        "",
    ]);
    r.extend(composer("  ⏵⏵ auto mode on · 1 shell · ← for agents"));
    let caret = r
        .iter()
        .position(|row| row == "❯")
        .expect("the caret's row");
    r[caret] = format!("❯ {text}");
    (r, caret)
}

/// THE HOST'S GUARDS AT A BREAK HOLD WHAT THE GLYPH'S CELL SAYS (the reviews of `6337470849` and
/// `5ad7f68c8f`): a break of the agent's own background work, a lone line in the composer, no key
/// measured, no decision before it, the cursor off the composer (a frame read before its program
/// placed it), past the line or home on it. Plain, it is a person's draft whichever: the guards hold
/// and the host's fact is held. Dim with the cursor home it is a suggestion: the guards hold nothing,
/// and the fact goes. Dim with the cursor anywhere else it says neither: the guards hold, as the door
/// reads no verdict as a draft, and nothing goes over a person's text.
/// NEGATIVE CONTROLS: `host_steps_in_background` without its `composer_for_guards`, and the guards hold
/// nothing over the plain line off the composer; `cell_saw` leaving a read that could not say no draft
/// seen, and they hold nothing over the dim line off it.
#[test]
fn a_breaks_guards_hold_what_the_glyphs_cell_says() {
    let (at_break, caret) = at_break(DRAFT);
    let cursors = [
        ("off the composer", 0, 40, false),
        ("past the line", caret, 2 + DRAFT.chars().count(), false),
        ("home on the line", caret, 2, true),
    ];
    for (attrs, plain) in [("none", true), ("dim", false)] {
        for (at, row, col, home) in cursors {
            let draft = plain || !home;
            let host = Arc::new(TellingHost::default());
            let mut m = Mock::new(vec![at_break.clone()]);
            m.cell_attrs = attrs;
            m.dim_past = true;
            m.cursor_row = Some(row);
            m.cursor_col = Some(col);
            let opts = hosted(600);
            let mut s = session(&mut m, Some("@s-1".to_string()));
            s.set_clock(TEST_NOW, PDT);
            s.stall_host = Some(Arc::clone(&host) as Arc<dyn IdleHost>);
            s.setup.background_settle = Duration::ZERO;
            let screen = s.screen().expect("the break");
            let mut since = Some(Instant::now());
            s.host_steps_in_background(&screen, &mut since, Some(&opts.policy), &mut Alone)
                .expect("the break's step");
            let now = s.fx.mono();
            let guards = s.occupancy_guards(now, &opts.policy, &screen.rows);
            let told = host.held.lock().unwrap().clone();
            assert_eq!(
                (guards.draft, guards.person),
                (draft, draft),
                "{attrs}, {at}: the guards"
            );
            assert_eq!(
                s.turn_end.fact_untaken(),
                !draft,
                "{attrs}, {at}: the host's fact, told held={told:?}"
            );
        }
    }
}

/// A BREAK'S CELL THAT COULD NOT SAY IS ASKED AGAIN: a suggestion, its cursor home, whose cell first
/// answers from a frame still open. The guards hold at that step, as for any read that could not say,
/// and let go at the next step over the same composer, when the cell speaks.
/// NEGATIVE CONTROL: `composer_for_guards` remembering a read that could not say, and the guards hold
/// the suggestion until its grace is out.
#[test]
fn a_breaks_cell_that_could_not_say_is_asked_again() {
    let (shown, caret) = at_break(HINT);
    let mut m = Mock::new(vec![shown]);
    m.cell_attrs = "dim";
    m.dim_past = true;
    m.cell_open_reads = 1;
    m.cursor_row = Some(caret);
    m.cursor_col = Some(2);
    let opts = hosted(600);
    let mut s = session(&mut m, Some("@s-1".to_string()));
    s.set_clock(TEST_NOW, PDT);
    s.setup.background_settle = Duration::ZERO;
    let screen = s.screen().expect("the break");
    let mut since = Some(Instant::now());
    s.host_steps_in_background(&screen, &mut since, Some(&opts.policy), &mut Alone)
        .expect("the break's step");
    assert!(s.draft_seen.is_some(), "a read that could not say holds");
    s.host_steps_in_background(&screen, &mut since, Some(&opts.policy), &mut Alone)
        .expect("the break's next step");
    assert!(s.draft_seen.is_none(), "the cell spoke: {:?}", s.draft_seen);
}

/// A BREAK'S DRAFT THAT WENT AND CAME BACK IS ASKED ABOUT AGAIN: a person's draft with its caret home,
/// which its rows alone never say, held by a break's step; a read finds the composer empty (a redraw
/// or another writer), and the next finds the same text and cursor. The next step asks the cell
/// again, and the guards hold it again.
/// NEGATIVE CONTROL: `composer_for_guards` remembering the text and cursor alone, and the guards hold
/// nothing over the draft that came back.
#[test]
fn a_breaks_draft_that_went_and_came_back_is_asked_again() {
    let (shown, caret) = at_break(DRAFT);
    let (empty, _) = at_break("");
    let mut m = Mock::new(vec![shown.clone(), empty, shown]);
    m.cell_attrs = "none";
    m.dim_past = true;
    m.cursor_row = Some(caret);
    m.cursor_col = Some(2);
    let opts = hosted(600);
    let mut s = session(&mut m, Some("@s-1".to_string()));
    s.set_clock(TEST_NOW, PDT);
    s.setup.background_settle = Duration::ZERO;
    let mut since = Some(Instant::now());
    let first = s.screen().expect("the break");
    s.host_steps_in_background(&first, &mut since, Some(&opts.policy), &mut Alone)
        .expect("the break's step");
    assert!(s.draft_seen.is_some(), "the cell says typed");
    let _ = s.screen().expect("the empty read");
    assert!(s.draft_seen.is_none(), "{:?}", s.draft_seen);
    let back = s.screen().expect("the draft back");
    assert!(s.draft_seen.is_none(), "the rows cannot say");
    s.host_steps_in_background(&back, &mut since, Some(&opts.policy), &mut Alone)
        .expect("the break's next step");
    assert!(s.draft_seen.is_some(), "the cell says typed again");
}

/// A SCREEN TICKING OVER A COMPOSER ASKS ITS CELL NO MORE FOR IT (the review of `5ad7f68c8f`): a footer
/// that ticks every second moves the content seq, so each move is a read. At an idle point over a real
/// draft no survey is drawn and nothing else reads the draft, so no read of the 120 s asks the cell
/// but the decision's. At a break, the same composer and cursor are asked about once: a draft, and a
/// suggestion its cursor is home on.
/// NEGATIVE CONTROLS: `survey` asking the cell with no survey drawn and none in hand, and the idle
/// point's cells count its reads (135); `composer_for_guards` without its memory, and the break's do
/// (145).
#[test]
fn a_screen_ticking_over_a_composer_asks_its_cell_no_more_for_it() {
    let ticking = |rows: &[String], span: u32| {
        let mut screens = vec![busy_screen()];
        screens.extend((0..=span).map(|_| rows.to_vec()));
        let mut m = fenced(screens);
        m.timeline = Some((
            1,
            (0..=span)
                .map(|k| Duration::from_secs(u64::from(k)))
                .collect(),
        ));
        m.stall_sleep = Some(Duration::from_secs(3600));
        m
    };
    let mut m = ticking(&typed_in(SAID, DRAFT), 130);
    m.cell_attrs = "none";
    m.cursor_on_caret = true;
    m.cursor_col = Some(2 + DRAFT.chars().count());
    m.exit_when = Some(acted);
    let t = held_run("draft-tick-idle", m);
    assert!(t.m.read_times.len() > 100, "it ticked: {}", t.evidence());
    assert!(t.cells() <= 4, "{} cells: {}", t.cells(), t.evidence());
    assert_eq!((t.enters(), t.typed()), (1, 0), "{}", t.evidence());

    for (what, text, attrs, col) in [
        ("a draft", DRAFT, "none", 2 + DRAFT.chars().count()),
        ("a suggestion", HINT, "dim", 2),
    ] {
        let (shown, _) = at_break(text);
        let mut m = ticking(&shown, 120);
        m.cell_attrs = attrs;
        m.dim_past = true;
        m.cursor_on_caret = true;
        m.cursor_col = Some(col);
        m.exit_at_request = Some(6000);
        let t = held_run("draft-tick-break", m);
        assert!(
            t.m.read_times.len() > 100,
            "{what}: it ticked: {}",
            t.evidence()
        );
        assert!(
            t.cells() <= 3,
            "{what}: {} cells: {}",
            t.cells(),
            t.evidence()
        );
    }
}

/// What the host's fact was told at each of a point's steps ([`IdlePoint::held`]).
#[derive(Debug)]
struct IdleTeller {
    clock: Arc<crate::supervise::fx::ScriptedClock>,
    steps: std::sync::Mutex<Vec<(Instant, bool)>>,
}

impl IdleHost for IdleTeller {
    fn wants(&self) -> bool {
        true
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn at_idle_on(&self, point: IdlePoint) -> Option<HostStep> {
        let at = run_now(Some(&self.clock));
        self.steps.lock().unwrap().push((at, point.held.is_some()));
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
}

/// THE HOST'S GUARDS AT A POINT WITH NOTHING DUE LET A SUGGESTION GO (the review of `5ad7f68c8f`): the
/// turn over, an empty composer, no continuation due; `text` then drawn with the cursor past it (a
/// frame read before its program placed the cursor), and home 300 ms on in a frame of its own, or 30 s
/// on with no content moving at all. No decision reads that composer, so the host's step asks the cell
/// before its guards. A suggestion is let go at the first step that sees the cursor home; a person's draft
/// keeps its grace, its cursor past it or home on it from the start (a Home pressed), which its rows
/// alone cannot say.
/// NEGATIVE CONTROLS: `host_steps_here` without its `composer_for_guards`, and the suggestion holds
/// the host's fact its whole 120 s; `composer_for_guards` called only over a draft the rows have seen,
/// and the draft home never holds it.
#[test]
fn the_hosts_guards_at_a_point_with_nothing_due_let_a_suggestion_go() {
    let run = |text: &str, attrs: &'static str, framed: bool, past: bool| {
        let clock = virtual_clock();
        let host = Arc::new(IdleTeller {
            clock: Arc::clone(&clock),
            steps: std::sync::Mutex::new(Vec::new()),
        });
        let mut screens = vec![busy_screen(), ended(SAID), typed_in(SAID, text)];
        let mut times = vec![Duration::ZERO, Duration::from_secs(5)];
        if framed {
            screens.push(typed_in(SAID, text));
            times.push(Duration::from_millis(5_300));
        }
        let mut m = fenced(screens);
        m.cell_attrs = attrs;
        m.dim_past = true;
        m.cursor_on_caret = true;
        m.screen_cols.insert(1, 2);
        m.screen_cols
            .insert(2, if past { 2 + text.chars().count() } else { 2 });
        if framed {
            m.screen_cols.insert(3, 2);
        } else {
            m.cursor_home_after = Some(Duration::from_secs(30));
        }
        m.timeline = Some((1, times));
        m.stall_sleep = Some(Duration::from_secs(3600));
        m.exit_at_request = Some(3000);
        let (dir, journal) = journal_file("draft-idle-guards");
        let mut opts = SuperviseOpts {
            journal: Some(journal),
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..hosted(400)
        };
        opts.policy.continue_policy = false;
        let _ = record_watch_on(&mut m, &opts, clock, |s| {
            s.set_turn_end_timing(TurnEndTiming {
                min_work: Duration::ZERO,
                ..TurnEndTiming::default()
            });
        });
        let _ = std::fs::remove_dir_all(&dir);
        let drawn = m
            .read_times
            .iter()
            .find(|(i, _)| *i == 2)
            .map(|(_, at)| *at)
            .expect("the text was read");
        let steps = host.steps.lock().unwrap().clone();
        let cells = m.requests.iter().filter(|r| r.starts_with("cell ")).count();
        (steps, drawn, cells)
    };
    for framed in [true, false] {
        let (steps, drawn, cells) = run(HINT, "dim", framed, true);
        let found = drawn + Duration::from_secs(if framed { 21 } else { 51 });
        let held_past = steps
            .iter()
            .filter(|(at, held)| *held && *at > found)
            .count();
        assert_eq!(held_past, 0, "framed {framed}: {steps:?} from {drawn:?}");
        assert!(
            steps.iter().any(|(at, held)| !*held && *at > found),
            "framed {framed}: {steps:?} from {drawn:?}"
        );
        assert!(cells <= 12, "framed {framed}: {cells} cells");
        for past in [true, false] {
            let (steps, drawn, _) = run(DRAFT, "none", framed, past);
            assert!(
                steps
                    .iter()
                    .any(|(at, held)| *held && *at > drawn + Duration::from_secs(100)),
                "framed {framed}, past {past}: a draft keeps its grace: {steps:?} from {drawn:?}"
            );
            assert!(
                !steps
                    .iter()
                    .any(|(at, held)| *held && *at > drawn + Duration::from_secs(121)),
                "framed {framed}, past {past}: and no longer: {steps:?} from {drawn:?}"
            );
        }
    }
}

/// A PERSON'S DRAFT THAT BEGINS THE LOOP'S OWN LINE IS THEIRS (the review of `d7b6e8aa23`): the
/// loop's line `keep going, then merge when ready` left unsent, and a read of the composer finding
/// `keep going` or `k`, no key measured. Only the line itself, whitespace aside, is the loop's own
/// words; a part of it can be a person's draft, and keeps its grace (`Session::person_at`). A line
/// still drawing is its echo's to wait out (`the_harnesss_own_half_echoed_act_is_no_persons_draft`).
/// A line held in custody is the loop's own words too.
/// NEGATIVE CONTROLS: `own_words` taking a prefix of the line as the loop's own
/// (`squashed(own).starts_with(squashed(shown))`), and `keep going` and `k` lose their grace; without
/// its custody arm, and the line held reads as a person's.
#[test]
fn a_persons_draft_that_begins_the_loops_own_line_is_theirs() {
    use crate::supervise::run::turn_end_loop::{Custody, OwnBy, OwnLine};
    let own = "keep going, then merge when ready";
    for (seen, persons) in [
        (own, false),
        ("keep  going,  then merge when ready", false),
        ("keep going", true),
        ("k", true),
    ] {
        let mut m = fenced(vec![typed_in(SAID, seen)]);
        m.cursor_on_caret = true;
        m.cursor_col = Some(2 + seen.chars().count());
        let mut s = virtual_session(&mut m, None);
        s.set_clock(TEST_NOW, PDT);
        s.own_line = Some(OwnLine::of(own, OwnBy::Line));
        let _ = s.screen().expect("the read of the composer");
        assert!(s.draft_seen.is_some(), "{seen:?}: the read sees it");
        assert_eq!(
            s.person_at().is_some(),
            persons,
            "{seen:?} under the line left {own:?}"
        );
        let mut m = fenced(vec![typed_in(SAID, seen)]);
        m.cursor_on_caret = true;
        m.cursor_col = Some(2 + seen.chars().count());
        let mut s = virtual_session(&mut m, None);
        s.set_clock(TEST_NOW, PDT);
        s.custody = Some(Custody::of(own, s.fx.mono()));
        let _ = s.screen().expect("the read of the composer");
        assert_eq!(
            s.person_at().is_some(),
            persons,
            "{seen:?} under the line held {own:?}"
        );
    }
}
