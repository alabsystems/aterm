// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A GREY SUGGESTION IS NO PERSON'S DRAFT, AND NOTHING IS TYPED INTO A DRAFT INSIDE ITS GRACE. Under
//! a program that draws grey text a composer's first glyph is told by its cell wherever the cursor
//! stands (`Session::composer_draft`). Plain, it is a draft, which keeps its full grace from the
//! first read that says so and then goes to the policy's draft step. Grey with the cursor home on it,
//! it is a suggestion, typed over at once. Grey with the cursor anywhere else it says neither, since
//! a frame read before its program placed the cursor and a person's own text drawn grey read the
//! same: nothing is typed over it, no Enter is pressed on it, the wait's own steps look again, and
//! the host's guards hold as the door does. Nothing looks at a grace on the way, and a composer read
//! empty or grey with other words decides nothing inside one, on the same point or a new one, however
//! long and after whatever input, since a redraw stalled before its text reads the same. Once a read
//! finds gone the text a decision held the point on, a draft's grace or any other hold's, nothing is
//! typed for a whole grace from that read (`Session::see_draft`). Any other text is another draft,
//! which a person's edit and a window's reflow both read as, read on the composer or off it, and the
//! loop's own words are no person's draft (`Session::own_words`). A person's key still holds, a
//! person's grace keeps the host's wake, and a screen ticking over a draft or a suggestion asks its
//! cell no more for that.

use super::*;
use crate::supervise::policy::turn_end::{OWN_WORDS_STAND, PERSON_TYPING, SEARCH_DIVERTS};

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
/// the continuation is typed over the text at its first read; `composer_draft` asking the cell only
/// at column 2, as on `5ece62c7b3`, and an Enter is pressed on it.
#[test]
fn grey_text_with_the_cursor_past_it_waits_for_the_cursor() {
    let run = |homes: Option<Duration>| {
        let mut m = fenced(vec![busy_screen(), typed_in(SAID, HINT)]);
        m.dim_past = true;
        m.cursor_on_caret = true;
        m.cursor_col = Some(2 + HINT.chars().count());
        m.cursor_home_after = homes;
        m.stall_sleep = Some(Duration::from_secs(3600));
        m.exit_when = Some(acted);
        held_run("draft-past", m)
    };
    run(Some(HOME)).typed_at_the_step_after_home("homes", 1);
    run(None).left_it_alone("never homes");
}

/// MUSE CODE'S HINT, WHEREVER ITS FRAME LEFT THE CURSOR (`docs/measured/half-drawn-frame-2026-10-22.md`):
/// no DEC 2026 brackets, its ~2 KB bottom pane drawn in place and its cursor placed last
/// (`ESC[48;3H`), so a read inside the redraw finds its prompt hint whole with the cursor on the
/// footer's row, and a read of a frame before that with the cursor past the hint on the caret's row.
/// Its cell (Muse's grey `676c74`) says grey and its cursor says neither, so nothing is typed until
/// a wait step reads the cursor home; then the continuation is typed and the hint is never submitted.
/// NEGATIVE CONTROL: `cell_is_placeholder` ignoring the program's hint ink, and the hint is held as a
/// person's draft (`WAITING … a person is typing`, no write at the step after home).
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
        m.stall_sleep = Some(Duration::from_secs(3600));
        m.exit_when = Some(acted);
        let t = held_run("draft-muse-hint", m);
        t.typed_at_the_step_after_home(&format!("past {past}"), 1);
    }
}

/// A SUGGESTION READ IN PART IS NO DRAFT (the review of `d7b6e8aa23`): the first read catches
/// `keep going, then` with the cursor after it (a frame read before its program placed the cursor),
/// no key measured. That read says neither, and the frame's next read, the suggestion whole with the
/// cursor home, has the continuation typed over it, never an Enter on it.
/// NEGATIVE CONTROL: `composer_draft` asking the cell only at column 2, as on `5ece62c7b3`, and the
/// part is waited on as a person's draft; `see_draft` marking any other words as the text's leaving,
/// and the whole suggestion, which begins with the part, is waited on as well.
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
        m.stall_sleep = Some(Duration::from_secs(3600));
        held_run("draft-person-grey", m).left_it_alone(what);
    }
}

/// A DICTATION DRAWN GREY IS LEFT ALONE UNTIL IT STANDS PLAIN (the same review): the key that started
/// it long past its grace, the words growing under the cursor every 20 s, drawn dim, and then standing
/// plain. Nothing is typed or pressed while they grow, and the draft then keeps a full grace from the
/// read that says it typed before the draft step sends it.
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
    let plain = t.first_read_of(4);
    let acted = t.acted_at().unwrap_or_else(|| panic!("{}", t.evidence()));
    assert!(
        acted >= plain + Duration::from_secs(119),
        "acted {:?} after the words stood plain: {}",
        acted.saturating_duration_since(plain),
        t.evidence()
    );
    assert_eq!((t.enters(), t.typed()), (1, 0), "{}", t.evidence());
}

/// A DRAFT'S GRACE RUNS FROM THE FIRST READ THAT SAYS IT TYPED (the review of `367dfeea26`): the same
/// words, no key measured, on reads that could not say for a whole grace and more, then plain, as a
/// dictation that stops changing is. That read is the first sign of a person's text, so it is sent a
/// full grace from it, never on the reads before.
/// NEGATIVE CONTROLS: `cell_saw` keeping the first sight of text no read has said typed, and each
/// arm's Enter comes within 10 ms of the plain read; `cell_saw` leaving text only the rows had found
/// [`turn_end_loop::DraftCell::Unasked`] on a read that could not say, and the `grey past it` arm's does.
#[test]
fn a_drafts_grace_runs_from_the_first_read_that_says_it_typed() {
    type OnCaret = fn(&mut Mock, usize);
    let says_neither: [(&str, OnCaret); 3] = [
        ("its cell refused, the cursor home", |m, _| {
            m.cell_unread_on.insert(1);
            m.screen_cols.insert(1, 2);
        }),
        ("grey past it", |m, _| {
            m.cell_attrs_on.insert(1, GREY);
        }),
        ("grey, the cursor off the composer", |m, caret| {
            m.cell_attrs_on.insert(1, GREY);
            m.screen_cursors.insert(1, caret + 2);
            m.screen_cols.insert(1, 2);
        }),
    ];
    for (what, set) in says_neither {
        let shown = typed_in(SAID, DRAFT);
        let caret = shown
            .iter()
            .position(|row| row.starts_with('❯'))
            .expect("the caret's row");
        let mut m = fenced(vec![busy_screen(), shown.clone(), shown]);
        m.cell_attrs = "none";
        m.dim_past = true;
        m.cursor_on_caret = true;
        m.cursor_col = Some(2 + DRAFT.chars().count());
        set(&mut m, caret);
        m.timeline = Some((1, vec![Duration::ZERO, Duration::from_secs(125)]));
        m.stall_sleep = Some(Duration::from_secs(3600));
        m.exit_when = Some(acted);
        let t = held_run("draft-first-said", m);
        let plain = t.first_read_of(2);
        let enter = t
            .enter_at()
            .unwrap_or_else(|| panic!("{what}: {}", t.evidence()));
        assert!(
            enter >= plain + Duration::from_secs(119),
            "{what}: sent {:?} after the plain read: {}",
            enter.saturating_duration_since(plain),
            t.evidence()
        );
        assert_eq!((t.enters(), t.typed()), (1, 0), "{what}: {}", t.evidence());
    }
}

/// WHAT THE GLYPH'S CELL AND THE CURSOR SAY OF A COMPOSER'S TEXT, ARM BY ARM (`composer_draft`), and
/// which arms read a cell. Home (`cursor_home`) is the caret's row at column 2, where every real
/// suggestion a tape holds has it (`c8-cruise-placeholder`, 13 of 13 reads), or on a lone line the row
/// under, where a grid a resize left drawn one row high has it
/// (`docs/DESIGN-wall-resume-2026-09-28.md`). A dim first glyph on more rows than one is no draft by
/// its rows alone: Claude Code 2.1.318 cuts a long suggestion to one row and Muse Code 1.4.14 clips
/// its tip to one (`docs/measured/composer-cells-2026-11-03.md`), so this arm only keeps the rows from
/// calling grey text on more rows, which nothing measured draws, typed.
/// NEGATIVE CONTROL: `cursor_home` taking column 2 on any row but a later one of the composer, as on
/// `bd67622c8c`, and the rows above and under say suggestion.
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
    let cases: [Case<'_>; 19] = [
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
        ("grey, column 2 above", &lone, true, "dim", 2, 2, None, 1),
        (
            "grey, column 2 two rows under",
            &lone,
            true,
            "dim",
            CARET + 2,
            2,
            None,
            1,
        ),
        (
            "grey, two rows, column 2 above",
            &two,
            true,
            "dim",
            2,
            2,
            None,
            1,
        ),
        (
            "grey, two rows, column 2 under",
            &two,
            true,
            "dim",
            CARET + 2,
            2,
            None,
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
/// decides again until the grace from its first sight is out, each wait running its whole timeout on
/// the run's clock, a wait step (20 s) between reads, so nothing polls it; then the policy's draft step
/// sends it: one Enter, nothing typed over it.
/// NEGATIVE CONTROLS: `see_draft` dropping the same words read home, and each decision sees the
/// draft anew: five `WAITING`s, never sent; the policy's draft step without its `Submit` over
/// `Composer::Typed`, and the continuation is typed over the draft; every step of `wait_for_next`
/// capped at `HANDBACK_POLL`, about 60 reads.
#[test]
fn a_real_draft_is_held_its_full_grace_and_then_submitted() {
    let mut m = fenced(vec![busy_screen(), typed_in(SAID, DRAFT)]);
    m.cell_attrs = "none";
    m.cursor_on_caret = true;
    m.cursor_col = Some(2 + DRAFT.chars().count());
    m.cursor_home_after = Some(HOME);
    m.stall_sleep = Some(Duration::from_secs(3600));
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
    let reads = t.reads_between(seen);
    assert!(
        reads <= 120 / 20 + 4,
        "{reads} reads in the hold: {}",
        t.evidence()
    );
    assert_eq!(
        (t.enters(), t.typed()),
        (1, 0),
        "sent once, never typed over: {}",
        t.evidence()
    );
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

/// GREY TEXT OVER A HELD DRAFT DECIDES NOTHING EITHER (the review of `6337470849`): a draft no key
/// stamped holds the point; 20 s on, a read finds the program's own grey text with other words where
/// it stood (a cleared Claude Code composer shows its dim suggestion, `docs/measured/
/// claude-code-dialogs-2026-09-24.md`, Muse Code's idle composer its tip), the cursor home or off the
/// composer. It stands for a moment and the draft comes back, or it stands for good. No read tells a
/// redraw from a clear here either, so nothing is typed inside the draft's grace. The draft step
/// sends a draft that came back once its grace from its return is out, since other words let its first
/// sight go wherever the cursor stood. A draft that never did is typed over only once the grace from
/// the read that found it gone is out, and only with the cursor home: off the composer, grey for good
/// says neither, and is left alone.
/// NEGATIVE CONTROL: `see_draft` leaving no mark over text the point was held on, and the
/// continuation is typed 100 s from that read.
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
            let t = held(muse, off, Some(Duration::from_millis(400)));
            let from = t.first_read_of(3);
            let acted = t.acted_at().unwrap_or_else(|| panic!("{}", t.evidence()));
            assert!(
                acted >= from + Duration::from_secs(119),
                "{what}: acted {:?} into its grace: {}",
                acted.saturating_duration_since(from),
                t.evidence()
            );
            assert_eq!((t.enters(), t.typed()), (1, 0), "{what}: {}", t.evidence());
            let t = held(muse, off, None);
            if off {
                assert_eq!((t.enters(), t.typed()), (0, 0), "{what}: {}", t.evidence());
                continue;
            }
            let left = t.first_read_of(2);
            let typed = t.typed_at().unwrap_or_else(|| panic!("{}", t.evidence()));
            assert!(
                typed >= left + Duration::from_secs(119),
                "{what}: typed {:?} after the read that found it gone: {}",
                typed.saturating_duration_since(left),
                t.evidence()
            );
            assert_eq!((t.enters(), t.typed()), (0, 1), "{what}: {}", t.evidence());
        }
    }
}

/// A NEW POINT READ GREY OFF THE COMPOSER KEEPS THE DRAFT (the review of `5ad7f68c8f`): a draft no key
/// stamped holds the point; 20 s on, a row above the composer moves, so the point's key moves, in a
/// frame whose composer reads grey, the cursor off it, and 400 ms on the draft is back. That read says
/// neither, so nothing is typed. Grey with the same words it costs the draft nothing: the draft step
/// sends it once the grace from its first sight is out, no later. Grey with other words is other text,
/// which keeps no first sight: the grace runs from the draft's return.
/// NEGATIVE CONTROLS: `composer_draft` taking grey for a suggestion wherever the cursor stands, and the
/// continuation is typed at 20 s, into the draft the program still holds; `cell_saw` dropping the
/// draft over a read that could not say, and the same words are sent at 140 s.
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
    let settled = |what: &str, t: &Held, from: usize| {
        let seen = t.first_read_of(from);
        let acted = t
            .acted_at()
            .unwrap_or_else(|| panic!("{what}: {}", t.evidence()));
        assert!(
            acted >= seen + Duration::from_secs(119) && acted < seen + Duration::from_secs(125),
            "{what}: acted {:?} after its read: {}",
            acted.saturating_duration_since(seen),
            t.evidence()
        );
        assert_eq!((t.enters(), t.typed()), (1, 0), "{what}: {}", t.evidence());
    };
    for (shown, from) in [(other, 3), (DRAFT, 1)] {
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
        settled(shown, &held_run("draft-new-point-grey", m), from);
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
    settled("Muse", &held_run("draft-new-point-grey-muse", m), 3);
}

/// A CURSOR OFF THE ROWS READ SAYS NOTHING, AS ONE OFF THE COMPOSER DOES (the review of `98b9c4d903`): a
/// draft no key stamped waits its grace; 20 s on a row above the composer moves, in a frame whose
/// composer reads grey with other words, the cursor off the composer or off the rows read (`text
/// tail=` over a taller window, the cursor above the tail), at column 40 or column 2; those words at
/// home or an empty composer 400 ms on; the draft back 1.1 s on. The other words let the draft's
/// first sight go wherever
/// the cursor stood, and no read says what became of the draft, so it is sent a grace from its return.
/// NEGATIVE CONTROL: `draft_read` answering by the column with the cursor off the rows read, as on
/// `98b9c4d903`, and the continuation is typed at 20.4 s, onto the words the program still holds.
#[test]
fn a_cursor_off_the_rows_read_says_nothing_of_other_words() {
    const MOVED: &str = "Stage 2 is in too.";
    let other = "/export saves the conversation to a text file";
    let past = 2 + DRAFT.chars().count();
    for (what, row, col, empty) in [
        ("off the composer, column 40", 0, 40, false),
        ("off the composer, column 2", 0, 2, false),
        ("off the rows read, column 40", 999, 40, false),
        ("off the rows read, column 2", 999, 2, false),
        ("off the composer, then empty", 0, 40, true),
    ] {
        let mut m = fenced(vec![
            busy_screen(),
            typed_in(SAID, DRAFT),
            typed_in(MOVED, other),
            if empty {
                ended(MOVED)
            } else {
                typed_in(MOVED, other)
            },
            typed_in(MOVED, DRAFT),
        ]);
        m.cell_attrs = "none";
        m.cursor_on_caret = true;
        m.screen_cols.insert(1, past);
        m.cell_attrs_on.insert(2, GREY);
        m.screen_cursors.insert(2, row);
        m.screen_cols.insert(2, col);
        if !empty {
            m.cell_attrs_on.insert(3, GREY);
        }
        m.screen_cols.insert(3, 2);
        m.screen_cols.insert(4, past);
        m.timeline = Some((
            1,
            vec![
                Duration::ZERO,
                Duration::from_secs(20),
                Duration::from_millis(20_400),
                Duration::from_millis(21_500),
            ],
        ));
        m.stall_sleep = Some(Duration::from_secs(3600));
        m.exit_when = Some(acted);
        let t = held_run("draft-off-the-rows", m);
        let back = t.first_read_of(4);
        let acted = t
            .acted_at()
            .unwrap_or_else(|| panic!("{what}: {}", t.evidence()));
        assert!(
            acted >= back + Duration::from_secs(119),
            "{what}: acted {:?} after the draft's return: {}",
            acted.saturating_duration_since(back),
            t.evidence()
        );
        assert_eq!((t.enters(), t.typed()), (1, 0), "{what}: {}", t.evidence());
    }
}

/// A NEW POINT READ GONE AT HOME KEEPS A DRAFT TOO (the review of `98b9c4d903`, on base as well): a
/// draft no key stamped waits its grace; 20 s on a row above the composer moves, in a frame whose
/// composer reads empty or grey at home with other words, or with words that begin with the draft
/// (the review of `367dfeea26`: its cell said typed, so it was no suggestion drawn in part); 1.5 s on
/// the draft is back. The policy decides again at that read, and the wait the draft's grace rested on
/// is gone, but the text the point was held on leaves its mark: the draft is sent a grace from its
/// return, and nothing is typed onto the words the program still holds.
/// NEGATIVE CONTROLS: `decide_to_act` keeping no text for a person's grace, and `see_draft` leaving no
/// mark over text the point was held on: each types the continuation at 20 s; `see_draft` taking any
/// words that begin with the text for a suggestion, whatever its cell said, as on `367dfeea26`, and
/// the third arm's continuation is typed at 20 s.
#[test]
fn a_new_point_read_gone_at_home_keeps_the_draft() {
    const MOVED: &str = "Stage 2 is in too.";
    let other = "/export saves the conversation to a text file";
    let begun = format!("{DRAFT}, then merge when ready");
    for (what, gone, attrs) in [
        ("empty", ended(MOVED), "none"),
        ("grey other words", typed_in(MOVED, other), GREY),
        ("grey words begun by it", typed_in(MOVED, &begun), GREY),
    ] {
        let mut m = fenced(vec![
            busy_screen(),
            typed_in(SAID, DRAFT),
            gone,
            typed_in(MOVED, DRAFT),
        ]);
        m.cell_attrs = "none";
        m.cursor_on_caret = true;
        m.screen_cols.insert(1, 2 + DRAFT.chars().count());
        m.cell_attrs_on.insert(2, attrs);
        m.screen_cols.insert(2, 2);
        m.screen_cols.insert(3, 2 + DRAFT.chars().count());
        m.timeline = Some((
            1,
            vec![
                Duration::ZERO,
                Duration::from_secs(20),
                Duration::from_millis(21_500),
            ],
        ));
        m.stall_sleep = Some(Duration::from_secs(3600));
        m.exit_when = Some(acted);
        let t = held_run("draft-new-point-home", m);
        let back = t.first_read_of(3);
        let acted = t
            .acted_at()
            .unwrap_or_else(|| panic!("{what}: {}", t.evidence()));
        assert!(
            acted >= back + Duration::from_secs(119),
            "{what}: acted {:?} after the draft's return: {}",
            acted.saturating_duration_since(back),
            t.evidence()
        );
        assert_eq!((t.enters(), t.typed()), (1, 0), "{what}: {}", t.evidence());
    }
}

/// What a scene sets on its run's Mock.
type SetUp = fn(&mut Mock);

/// One way a composer's read says neither, over screen 1 of a run: its cell refused, its first cell
/// read inside a frame, or grey with the cursor past the text.
const SAYS_NEITHER: [(&str, SetUp); 3] = [
    ("its cell refused", |m| m.cell_unreadable = true),
    ("a frame open", |m| m.cell_open_reads = 1),
    ("grey past it", |m| {
        m.cell_attrs = GREY;
        m.dim_past = true;
    }),
];

/// `screens` from screen 1 on, `at` their times, the cursor past `DRAFT` on its caret's row but on
/// screens whose text is gone, where it is home.
fn hold_run(tag: &str, screens: Vec<Vec<String>>, at: Vec<Duration>, set: SetUp) -> Held {
    let gone: Vec<usize> = screens
        .iter()
        .enumerate()
        .filter(|(_, rows)| !rows.iter().any(|r| *r == format!("❯ {DRAFT}")))
        .map(|(i, _)| i + 1)
        .collect();
    let mut all = vec![busy_screen()];
    all.extend(screens);
    let mut m = fenced(all);
    m.cell_attrs = "none";
    m.cursor_on_caret = true;
    m.cursor_col = Some(2 + DRAFT.chars().count());
    for i in gone {
        m.screen_cols.insert(i, 2);
        m.cell_attrs_on.insert(i, GREY);
    }
    set(&mut m);
    m.timeline = Some((1, at));
    m.stall_sleep = Some(Duration::from_secs(3600));
    m.exit_when = Some(acted);
    held_run(tag, m)
}

/// TEXT A HOLD STANDS ON, READ GONE, DECIDES NOTHING FOR A GRACE (the review of `bd67622c8c`): a
/// composer whose read says neither holds the point, no key measured; 20 s on, a read finds it empty
/// or grey at home with other words, and 1.5 s on the text is back. No read tells that from a redraw
/// stalled before its text, as on a plain draft's grace, so nothing is typed. A read of the words
/// back that says typed has them sent a grace from their return; one that still says neither holds.
/// NEGATIVE CONTROL: `see_draft` leaving no mark over text the point was held on, and the continuation
/// is typed at 20 s, onto the words the program still holds.
#[test]
fn text_a_hold_stands_on_read_gone_decides_nothing_for_a_grace() {
    let times = vec![
        Duration::ZERO,
        Duration::from_secs(20),
        Duration::from_millis(21_500),
    ];
    let other = "/export saves the conversation to a text file";
    for (what, set) in SAYS_NEITHER {
        for gone in [ended(SAID), typed_in(SAID, other)] {
            let t = hold_run(
                "draft-hold-gone",
                vec![typed_in(SAID, DRAFT), gone, typed_in(SAID, DRAFT)],
                times.clone(),
                set,
            );
            let back = t.first_read_of(3);
            assert_eq!(t.typed(), 0, "{what}: {}", t.evidence());
            if let Some(enter) = t.enter_at() {
                assert!(
                    enter >= back + Duration::from_secs(119),
                    "{what}: sent {:?} after its return: {}",
                    enter.saturating_duration_since(back),
                    t.evidence()
                );
            }
        }
    }
}

/// A HOLD STANDS ON THE TEXT ITS LAST LOOK READ (the review of `bd67622c8c`): grey text past its cursor
/// makes the hold, no key measured; 10 s on, a look reads longer words past it; 10 s on, a read finds
/// a suggestion at home that begins with the first text and not with the second. Those words left, so
/// the continuation is typed a grace from that read, never over them at once.
/// NEGATIVE CONTROL: `draft_look` keeping the text the hold began on, and it is typed at that read.
#[test]
fn a_hold_stands_on_the_text_its_last_look_read() {
    let (first, edited) = ("keep going", "keep going fix the tests");
    let mut m = fenced(vec![
        busy_screen(),
        typed_in(SAID, first),
        typed_in(SAID, edited),
        typed_in(SAID, HINT),
    ]);
    m.cell_attrs = GREY;
    m.dim_past = true;
    m.cursor_on_caret = true;
    m.screen_cols.insert(1, 2 + first.chars().count());
    m.screen_cols.insert(2, 2 + edited.chars().count());
    m.screen_cols.insert(3, 2);
    m.timeline = Some((
        1,
        vec![
            Duration::ZERO,
            Duration::from_secs(10),
            Duration::from_secs(20),
        ],
    ));
    m.stall_sleep = Some(Duration::from_secs(3600));
    m.exit_when = Some(acted);
    let t = held_run("draft-hold-last-look", m);
    let left = t.first_read_of(3);
    let typed = t
        .acted_at()
        .unwrap_or_else(|| panic!("no act: {}", t.evidence()));
    assert!(
        typed >= left + Duration::from_secs(119),
        "typed {:?} after the read: {}",
        typed.saturating_duration_since(left),
        t.evidence()
    );
    assert_eq!((t.enters(), t.typed()), (0, 1), "{}", t.evidence());
}

/// A FRAME WITH NO COMPOSER BETWEEN A DRAFT AND A READ OF IT GONE KEEPS THE MARK (the review of
/// `367dfeea26`, on base as well): a draft no key stamped waits its grace; 60 s on a frame draws no
/// composer; 300 ms on a read finds the composer empty; and 300 ms on the draft is back, or never. That
/// frame says nothing of the draft, so the read after it leaves the mark it would have left: nothing is
/// typed there for a grace, and a draft back is sent a grace from its return.
/// NEGATIVE CONTROL: `hold_on` letting the text go at the frame with no composer, and the continuation
/// is typed at 60.3 s, onto the words the program still holds.
#[test]
fn a_frame_with_no_composer_keeps_the_mark_of_the_text_read_gone_after_it() {
    let past = 2 + DRAFT.chars().count();
    for back in [true, false] {
        let mut screens = vec![
            busy_screen(),
            typed_in(SAID, DRAFT),
            rows(&["Stage 1 is in.", "", "user@mac aterm % "]),
            ended(SAID),
        ];
        let mut at = vec![
            Duration::ZERO,
            Duration::from_secs(60),
            Duration::from_millis(60_300),
        ];
        if back {
            screens.push(typed_in(SAID, DRAFT));
            at.push(Duration::from_millis(60_600));
        }
        let mut m = fenced(screens);
        m.cell_attrs = "none";
        m.cursor_on_caret = true;
        m.screen_cols.insert(1, past);
        m.screen_cols.insert(2, 0);
        m.screen_cursors.insert(2, 2);
        m.screen_cols.insert(3, 2);
        m.screen_cols.insert(4, past);
        m.timeline = Some((1, at));
        m.stall_sleep = Some(Duration::from_secs(3600));
        m.exit_when = Some(acted);
        let t = held_run("draft-no-composer", m);
        let from = t.first_read_of(if back { 4 } else { 3 });
        let acted = t.acted_at().unwrap_or_else(|| panic!("{}", t.evidence()));
        assert!(
            acted >= from + Duration::from_secs(119),
            "back {back}: acted {:?} after that read: {}",
            acted.saturating_duration_since(from),
            t.evidence()
        );
        let expected = if back { (1, 0) } else { (0, 1) };
        assert_eq!((t.enters(), t.typed()), expected, "{}", t.evidence());
    }
}

/// TEXT A HOLD STOOD ON, GONE FOR GOOD, IS TYPED OVER A GRACE FROM ITS LEAVING: a composer whose read says
/// neither, or a person's line quoting a whole upgrade head (`Composer::Own`, asked about at once),
/// read empty from 20 s on. The continuation is typed 120 s from that read, never an Enter.
/// NEGATIVE CONTROL: as [`text_a_hold_stands_on_read_gone_decides_nothing_for_a_grace`]'s, and it is
/// typed at 20 s.
#[test]
fn text_a_hold_stood_on_gone_for_good_is_typed_over_a_grace_from_its_leaving() {
    let run = |tag: &str, text: &str, set: SetUp| {
        let mut m = fenced(vec![busy_screen(), typed_in(SAID, text), ended(SAID)]);
        m.cell_attrs = "none";
        m.cursor_on_caret = true;
        m.screen_cols.insert(1, 2 + text.chars().count());
        m.screen_cols.insert(2, 2);
        set(&mut m);
        m.timeline = Some((1, vec![Duration::ZERO, Duration::from_secs(20)]));
        m.stall_sleep = Some(Duration::from_secs(3600));
        m.exit_when = Some(acted);
        held_run(tag, m)
    };
    let quote = format!(
        "{}please also note this",
        crate::harness::upgrade::TELL_HEAD
    );
    let mut cases: Vec<(&str, &str, SetUp)> = vec![("a quote of a head", &quote, |_| {})];
    cases.extend(SAYS_NEITHER.map(|(what, set)| (what, DRAFT, set)));
    for (what, text, set) in cases {
        let t = run("draft-hold-gone-for-good", text, set);
        let left = t.first_read_of(2);
        let typed = t
            .typed_at()
            .unwrap_or_else(|| panic!("{what}: {}", t.evidence()));
        assert!(
            typed >= left + Duration::from_secs(119),
            "{what}: typed {:?} after the read that found it gone: {}",
            typed.saturating_duration_since(left),
            t.evidence()
        );
        assert_eq!((t.enters(), t.typed()), (0, 1), "{what}: {}", t.evidence());
    }
}

/// THE MARK COUNTS A SLEEP AS EVERY STAMP OF THE PERSON DOES (`Session::person_slept`, the review of
/// `98b9c4d903`): a hold's text read gone at 20 s, and the Mac asleep 600 s from 30 s on, far past any
/// grace. Nobody can have typed in the sleep, so the continuation is typed once the wake has settled,
/// 30 s on, as after any sleep.
/// NEGATIVE CONTROL: `person_slept` leaving `draft_left` on the awake clock, and it waits out the rest
/// of the grace after the wake, 110 s.
#[test]
fn a_sleep_past_the_marks_grace_lets_it_go_at_the_wake() {
    let mut m = fenced(vec![
        busy_screen(),
        typed_in(SAID, DRAFT),
        ended(SAID),
        ended(SAID),
    ]);
    m.cell_attrs = "none";
    m.cell_attrs_on.insert(1, GREY);
    m.dim_past = true;
    m.cursor_on_caret = true;
    m.screen_cols.insert(1, 2 + DRAFT.chars().count());
    m.screen_cols.insert(2, 2);
    m.screen_cols.insert(3, 2);
    m.sleeps_at.insert(3, 600);
    m.timeline = Some((
        1,
        vec![
            Duration::ZERO,
            Duration::from_secs(20),
            Duration::from_secs(30),
        ],
    ));
    m.stall_sleep = Some(Duration::from_secs(3600));
    m.exit_when = Some(acted);
    let t = held_run("draft-mark-slept", m);
    let wake = t.first_read_of(3);
    let typed = t.typed_at().unwrap_or_else(|| panic!("{}", t.evidence()));
    assert!(
        typed < wake + Duration::from_secs(60),
        "typed {:?} after the wake, 600 s past any grace: {}",
        typed.saturating_duration_since(wake),
        t.evidence()
    );
}

/// THE MARK LASTS UNTIL A TURN RUNS: text a hold stood on read gone, and a turn runs and ends inside
/// what would be its grace. The program took input, so no redraw is still to come: the continuation
/// is typed at the turn's end.
/// NEGATIVE CONTROL: `turn_end_at_point` keeping `draft_left` over a turn's work, and the continuation
/// waits out the rest of the grace.
#[test]
fn a_turn_that_ran_ends_a_held_texts_mark() {
    let mut m = fenced(vec![
        busy_screen(),
        typed_in(SAID, DRAFT),
        ended(SAID),
        busy_screen(),
        ended("Stage 2 is in too."),
    ]);
    m.cell_unreadable = true;
    m.cursor_on_caret = true;
    m.screen_cols.insert(1, 2 + DRAFT.chars().count());
    m.screen_cols.insert(2, 2);
    m.timeline = Some((
        1,
        vec![
            Duration::ZERO,
            Duration::from_secs(20),
            Duration::from_secs(25),
            Duration::from_secs(40),
        ],
    ));
    m.stall_sleep = Some(Duration::from_secs(3600));
    m.exit_when = Some(acted);
    let t = held_run("draft-hold-gone-turn", m);
    let over = t.first_read_of(4);
    let typed = t.typed_at().unwrap_or_else(|| panic!("{}", t.evidence()));
    assert!(
        typed < over + Duration::from_secs(5),
        "typed {:?} after the turn's end: {}",
        typed.saturating_duration_since(over),
        t.evidence()
    );
    assert_eq!((t.enters(), t.typed()), (0, 1), "{}", t.evidence());
}

/// THE LOOP'S OWN LINE GONE IS NO PERSON'S TEXT GONE: its continuation left in the composer (its Enter
/// refused, no key measured), then read empty. A row above moved first, a new point with the line read
/// grey past its cursor, so a hold stands on it; or other grey words were read off the composer, on the
/// same point. No person's words were seen to leave, so no grace is waited: it is written again at
/// the read that finds it gone.
/// NEGATIVE CONTROLS: `hold_on` taking the loop's own line for the hold's text, and over the hold it
/// waits out a person's grace for nobody; `see_draft` marking that line when a read that could not
/// say lets it go, and over the other words it does.
#[test]
fn the_loops_own_line_gone_waits_no_grace() {
    const MOVED: &str = "Stage 2 is in too.";
    let line = "keep going";
    let other = "/export saves the conversation to a text file";
    let past = 2 + line.chars().count();
    for (what, between, at_end, row, col) in [
        ("a hold on it", typed_in(MOVED, line), MOVED, 5, past),
        ("other words off it", typed_in(SAID, other), SAID, 0, 40),
    ] {
        let mut m = fenced(vec![
            busy_screen(),
            ended(SAID),
            typed_in(SAID, line),
            between,
            ended(at_end),
        ]);
        m.gen_fence = true;
        m.sends_gen = true;
        m.help = FENCED_HELP.to_string();
        m.cursor_on_caret = true;
        m.cell_attrs = "none";
        m.dim_past = true;
        m.screen_cols.insert(2, past);
        m.cell_attrs_on.insert(3, GREY);
        m.screen_cursors.insert(3, row);
        m.screen_cols.insert(3, col);
        m.cell_attrs_on.insert(4, GREY);
        m.screen_cols.insert(4, 2);
        m.key_replies
            .push_back(ok("OK skipped reason=person seq=101\n"));
        m.stall_sleep = Some(Duration::from_secs(3600));
        m.exit_when = Some(|m| {
            m.requests
                .iter()
                .filter(|r| r.starts_with("turn ") || r.starts_with("send "))
                .count()
                >= 2
        });
        let t = held_run("draft-own-gone", m);
        assert_eq!(t.typed(), 2, "{what}: {}", t.evidence());
        assert_eq!(t.person_waits(), 0, "{what}: {}", t.evidence());
        let read = t.first_read_of(4);
        let last = t.m.read_times.last().map(|(_, at)| *at).expect("a read");
        assert!(
            last < read + Duration::from_secs(21),
            "{what}: still reading {:?} after the read that found it gone: {}",
            last.saturating_duration_since(read),
            t.evidence()
        );
    }
}

/// A WORKER FROZEN UNDER A COMPOSER THAT COULD NOT SAY IS HEARD (the review of `bd67622c8c`): grey text
/// with the cursor past it holds the point, and the worker stops reading its input after that hold's
/// first steps. Every other hold reads `status` at each wait step, and this one does too, so the freeze
/// is journaled at its first step past the stall's threshold.
/// NEGATIVE CONTROL: `wait_for_next` looking at the composer before its `stall_step`, as on
/// `bd67622c8c`, and the run reaches its deadline with no `FROZEN`.
#[test]
fn a_worker_frozen_under_a_composer_that_could_not_say_is_heard() {
    let mut m = fenced(vec![busy_screen(), typed_in(SAID, HINT)]);
    m.cell_attrs = GREY;
    m.dim_past = true;
    m.cursor_on_caret = true;
    m.cursor_col = Some(2 + HINT.chars().count());
    m.stall_sleep = Some(Duration::from_secs(3600));
    m.input = INPUT_CLEAR;
    m.input_after = vec![(2, INPUT_STALLED)];
    m.exit_when = Some(|m| m.status_reads >= 8);
    let t = held_run("draft-hold-frozen", m);
    assert_eq!((t.enters(), t.typed()), (0, 0), "{}", t.evidence());
    assert!(
        t.journal.iter().any(|l| l.starts_with("FROZEN ")),
        "{}",
        t.evidence()
    );
}

/// ONE SCREEN, ONE ANSWER (the review of `bd67622c8c`): the loop's `composer_draft` and Claude Code's
/// upgrade gate read every composer alike, over a lone line, one with an empty row under it and two
/// rows of text, dim or plain, the cursor on each kind of place: the gate calls it empty just where the
/// loop calls it a suggestion, and neither writes where the loop holds. The rows' own word
/// (`draft_read`), where they give one over grey text, is what the cell then says: none only where it
/// reads a suggestion, typed only where it reads neither. A cursor above the rows read is none to the
/// gate (`upgrade_drive`'s reader) and off them to the loop and the rows: all say neither.
/// NEGATIVE CONTROLS: `composer_is_empty` as on `bd67622c8c`, which took dim text for no draft with the
/// cursor off the composer and held dim text of two rows with the cursor home; [`cursor_home`] taking
/// no cursor for home, as the gate did, and the cursor above the rows differs; `draft_read` answering
/// by the column with the cursor off the rows read, as on `98b9c4d903`, and the rows say none there.
///
/// [`cursor_home`]: crate::supervise::screen::cursor_home
#[test]
fn the_loop_and_the_upgrade_gate_read_one_screen_alike() {
    let lone = typed_in(SAID, DRAFT);
    let (empty_under, _) = two_rows();
    let (two, _) = wrapped("please also update", "the changelog");
    for rows in [lone, empty_under, two] {
        let composer = Program::Claude
            .reader()
            .composer(&rows)
            .expect("a composer");
        let caret = composer.0;
        for attrs in ["dim", "none"] {
            for cursor in [
                (caret, 2),
                (caret, 30),
                (caret + 1, 2),
                (caret + 1, 4),
                (caret + 2, 2),
                (0, 2),
                (0, 40),
            ] {
                let mut m = Mock::new(vec![rows.clone()]);
                m.cell_attrs = attrs;
                m.dim_past = true;
                let screen = Screen {
                    rows: rows.clone(),
                    cursor_row: cursor.0,
                    cursor_col: cursor.1,
                    ..Screen::default()
                };
                let mut s = session(&mut m, None);
                let verdict = s.composer_draft(Some(&composer), true, None, &screen);
                let dim = crate::harness::upgrade::composer_dim_row(&rows, Some(cursor)).is_some()
                    && attrs == "dim";
                let empty = crate::harness::upgrade::composer_is_empty(&rows, Some(cursor), dim);
                assert_eq!(
                    empty,
                    verdict == Some(false),
                    "{attrs}, the cursor at {cursor:?}, rows {:?}: the loop says {verdict:?}",
                    composer.1,
                );
                if attrs == "dim" {
                    let by_rows = draft_read(Some(&composer), true, &screen);
                    assert!(
                        match by_rows {
                            Some(false) => verdict == Some(false),
                            Some(true) => verdict.is_none(),
                            None => true,
                        },
                        "the cursor at {cursor:?}, rows {:?}: the rows say {by_rows:?}, the cell \
                         {verdict:?}",
                        composer.1,
                    );
                }
            }
            let mut m = Mock::new(vec![rows.clone()]);
            m.cell_attrs = attrs;
            m.dim_past = true;
            let above = Screen {
                rows: rows.clone(),
                first: 5,
                cursor_row: 0,
                cursor_col: 2,
                ..Screen::default()
            };
            assert_eq!(above.cursor_index(), None);
            assert_eq!(
                draft_read(Some(&composer), true, &above),
                None,
                "{attrs}, rows {:?}",
                composer.1
            );
            let mut s = session(&mut m, None);
            let verdict = s.composer_draft(Some(&composer), true, None, &above);
            assert_ne!(verdict, Some(false), "{attrs}, rows {:?}", composer.1);
            assert!(
                !crate::harness::upgrade::composer_is_empty(&rows, None, attrs == "dim"),
                "{attrs}, rows {:?}",
                composer.1
            );
        }
    }
}

/// A PERSON'S SLASH DRAFT OVER A MENU ITS ENTER WOULD RUN HOLDS ITS GRACE AND STANDS AS THEIR
/// COMMAND (v6 §14.17.5.8): a person's `/config` over Claude Code's slash menu with
/// `/update-config` selected (its Enter would run the selection), no key measured, standing for
/// good, or redrawn the same at new seqs 20 s and 40 s on. Its grace is waited out at the wait's
/// pace, and past it the draft is the person's command whatever the menu selects: held quietly
/// (`HOLD … why=command`, once), never asked about (no `MENU_DIVERTS`) and never sent.
/// NEGATIVE CONTROL: the reading's command form ignored over a menu (`over_menu` alone, as before
/// the slice): the point is asked about, `MENU_DIVERTS`.
#[test]
fn a_slash_draft_over_a_menu_its_enter_would_run_holds_its_grace_and_stands_as_a_command() {
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
            fenced(vec![busy_screen(), at_end.clone()])
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
            "draft-held-command-redrawn"
        } else {
            "draft-held-command"
        };
        let t = held_run(tag, m);
        assert_eq!(t.person_waits(), 1, "{tag}: {}", t.evidence());
        let seen = t.first_read_of(1);
        assert!(
            !t.m.requests
                .iter()
                .any(|r| r.starts_with("meta set attention")),
            "{tag}: never asked about: {}",
            t.evidence()
        );
        assert_eq!(
            t.journal
                .iter()
                .filter(|l| l.starts_with("HOLD ") && l.contains(" why=command "))
                .count(),
            1,
            "{tag}: held as the person's command: {}",
            t.evidence()
        );
        let reads =
            t.m.read_times
                .iter()
                .filter(|(_, at)| (seen..=seen + Duration::from_secs(120)).contains(at))
                .count();
        assert!(
            reads <= 40,
            "{tag}: {reads} reads in a 120 s hold: {}",
            t.evidence()
        );
        assert_eq!((t.enters(), t.typed()), (0, 0), "{tag}: {}", t.evidence());
    }
}

/// A measured search screen ([`history_searches`]): its tag, its rows, its cursor, and its composer
/// row's first glyphs' `cell` replies by row and column.
type MeasuredSearch = (
    &'static str,
    Vec<String>,
    (usize, usize),
    Vec<((usize, usize), String)>,
);

/// The measured screens of Claude Code 2.1.318's prompt-history search (Ctrl-R) open at a turn
/// end, each with its cursor and the cells of its composer row's first glyphs: its match in typed
/// ink (doc scene C), nothing (its query matching nothing over an empty composer), the grey
/// suggestion it opened over, and its match under a status line, where the search heads the
/// footer's second row; and its match and nothing in windows of 60, 44 and 26 columns and nothing at
/// 34, where its label wraps in its own column, by whole words or (at 26) within them.
fn history_searches() -> Vec<MeasuredSearch> {
    use aterm_phase::prompt::fixtures::{
        HISTORY_SEARCH_2_1_318, HISTORY_SEARCH_26COL_2_1_318, HISTORY_SEARCH_26COL_CELLS_2_1_318,
        HISTORY_SEARCH_44COL_2_1_318, HISTORY_SEARCH_44COL_CELLS_2_1_318,
        HISTORY_SEARCH_60COL_2_1_318, HISTORY_SEARCH_60COL_CELLS_2_1_318,
        HISTORY_SEARCH_CELLS_2_1_318, HISTORY_SEARCH_NO_MATCH_2_1_318,
        HISTORY_SEARCH_NO_MATCH_26COL_2_1_318, HISTORY_SEARCH_NO_MATCH_34COL_2_1_318,
        HISTORY_SEARCH_NO_MATCH_44COL_2_1_318, HISTORY_SEARCH_NO_MATCH_60COL_2_1_318,
        HISTORY_SEARCH_OVER_SUGGESTION_2_1_318, HISTORY_SEARCH_OVER_SUGGESTION_CELLS_2_1_318,
        HISTORY_SEARCH_UNDER_STATUSLINE_2_1_318, HISTORY_SEARCH_UNDER_STATUSLINE_CELLS_2_1_318,
        cells, cursor, screen,
    };
    [
        (
            "match",
            HISTORY_SEARCH_2_1_318,
            HISTORY_SEARCH_CELLS_2_1_318,
        ),
        ("no-match", HISTORY_SEARCH_NO_MATCH_2_1_318, ""),
        (
            "over-suggestion",
            HISTORY_SEARCH_OVER_SUGGESTION_2_1_318,
            HISTORY_SEARCH_OVER_SUGGESTION_CELLS_2_1_318,
        ),
        (
            "under-statusline",
            HISTORY_SEARCH_UNDER_STATUSLINE_2_1_318,
            HISTORY_SEARCH_UNDER_STATUSLINE_CELLS_2_1_318,
        ),
        (
            "60col",
            HISTORY_SEARCH_60COL_2_1_318,
            HISTORY_SEARCH_60COL_CELLS_2_1_318,
        ),
        ("no-match-60col", HISTORY_SEARCH_NO_MATCH_60COL_2_1_318, ""),
        (
            "44col",
            HISTORY_SEARCH_44COL_2_1_318,
            HISTORY_SEARCH_44COL_CELLS_2_1_318,
        ),
        ("no-match-44col", HISTORY_SEARCH_NO_MATCH_44COL_2_1_318, ""),
        ("no-match-34col", HISTORY_SEARCH_NO_MATCH_34COL_2_1_318, ""),
        (
            "26col",
            HISTORY_SEARCH_26COL_2_1_318,
            HISTORY_SEARCH_26COL_CELLS_2_1_318,
        ),
        ("no-match-26col", HISTORY_SEARCH_NO_MATCH_26COL_2_1_318, ""),
    ]
    .into_iter()
    .map(|(tag, text, cell_text)| {
        let at = cursor(text).expect("a measured cursor");
        let cells = cells(cell_text)
            .into_iter()
            .map(|(r, c, reply)| ((r, c), reply))
            .collect();
        (tag, screen(text), at, cells)
    })
    .collect()
}

/// THE PROMPT-HISTORY SEARCH IS NEVER PRESSED OR TYPED INTO (doc scene C, measured on 2.1.318): open
/// at a turn end over its match, which its Enter would send as an old prompt; over nothing, where
/// typing goes into its query; over a suggestion; over its match under a status line; and over its
/// match and nothing in narrow windows, where its label wraps in its own column. No key
/// measured, standing for good. Nothing is ever typed or pressed, and the point is asked about with
/// the search's own reason (`SEARCH_DIVERTS`) at once, never held as an unknown composer: the
/// search's row is no person's draft, so no person's grace waits on it.
/// NEGATIVE CONTROLS: `aterm_phase::phase::history_search` reading nothing, and each is held as an
/// unknown composer, asked about by no one (the cursor under the composer still keeps every key out,
/// `screen::cursor_under_composer`); the policy not reading the search (`TurnEndReading::of_slept`),
/// and the same; `Session::composer_typed` keeping the search's row as a draft seen, and each waits a
/// person's grace from the loop's own read. Without the cursor rule the search's rows are typed into
/// and its match entered where its label is not read (`a_search_in_a_narrow_window_is_never_pressed_
/// or_typed_into`).
#[test]
fn the_history_search_is_never_pressed_or_typed_into_and_is_asked_about() {
    let reason = &SEARCH_DIVERTS[..60];
    let outcomes: Vec<_> = history_searches()
        .into_iter()
        .map(|(tag, at_end, (row, col), cells)| {
            let mut m = fenced(vec![busy_screen(), at_end]);
            m.cursor_row = Some(row);
            m.cursor_col = Some(col);
            m.cells = cells.into_iter().collect();
            m.stall_sleep = Some(Duration::from_secs(3600));
            m.exit_when = Some(acted_or_asked);
            let t = held_run(&format!("history-search-{tag}"), m);
            let asked =
                t.m.requests
                    .iter()
                    .any(|r| r.starts_with("meta set attention") && r.contains(reason));
            let unknown = t
                .journal
                .iter()
                .any(|l| l.ends_with("why=composer-unknown"));
            let waited = t.journal.iter().any(|l| l.ends_with(PERSON_TYPING));
            (
                tag,
                (t.enters(), t.typed(), asked, unknown, waited),
                t.evidence(),
            )
        })
        .collect();
    let wrong: Vec<_> = outcomes
        .iter()
        .filter(|(_, got, _)| *got != (0, 0, true, false, false))
        .collect();
    assert!(
        wrong.is_empty(),
        "(enters, typed, asked with the search's reason, held as unknown, waited a person's grace) \
         of each: {:#?}\n{:#?}",
        outcomes
            .iter()
            .map(|(tag, got, _)| (tag, got))
            .collect::<Vec<_>>(),
        wrong
            .iter()
            .map(|(tag, _, evidence)| (tag, evidence))
            .collect::<Vec<_>>()
    );
}

/// A PERSON WHO LEAVES THE SEARCH HAS THE COMPOSER JUDGED AS USUAL: the search open at a turn end,
/// its query matching nothing over an empty composer (`claude-2.1.318-history-search-no-match`), no
/// key measured, standing 150 s, past a person's grace, then left with Esc, which keeps what stood
/// before it (measured, scene E of docs/measured/history-search-narrow-2026-11-04.md): the footer
/// back and the composer empty. Nothing is typed or pressed while the search stands; the
/// continuation is typed once it has gone, with no Enter.
/// NEGATIVE CONTROL: `Session::draft_look` keeping a diverted hold whatever it reads, and nothing is
/// typed after the search goes.
#[test]
fn a_person_who_leaves_the_search_has_the_composer_judged_as_usual() {
    let (_, open, (row, col), _) = history_searches().swap_remove(1);
    let mut left = open.clone();
    let footer = left.len() - 1;
    left[footer] = "  ⏵⏵ auto mode on (shift+tab to cycle)".to_string();
    let caret = left
        .iter()
        .rposition(|r| r.starts_with('❯'))
        .expect("the caret");
    let mut m = fenced(vec![busy_screen(), open, left]);
    m.screen_cursors.insert(1, row);
    m.screen_cols.insert(1, col);
    m.screen_cursors.insert(2, caret);
    m.screen_cols.insert(2, 2);
    m.timeline = Some((1, vec![Duration::ZERO, Duration::from_secs(150)]));
    m.stall_sleep = Some(Duration::from_secs(3600));
    m.exit_when = Some(acted);
    let t = held_run("history-search-left", m);
    let gone = t.first_read_of(2);
    let typed = t.typed_at().unwrap_or_else(|| panic!("{}", t.evidence()));
    assert!(
        typed >= gone,
        "typed while the search stood: {}",
        t.evidence()
    );
    assert_eq!((t.enters(), t.typed()), (0, 1), "{}", t.evidence());
}

/// Ends a run at its first write or Enter, or 600 s after its last screen was first read.
fn acted_or_600s_on(m: &Mock) -> bool {
    acted(m)
        || m.read_times
            .iter()
            .find(|(i, _)| *i + 1 == m.screens.len())
            .is_some_and(|(_, at)| run_since(m.waits.as_deref(), *at) >= Duration::from_secs(600))
}

/// AN OLD PROMPT THE SEARCH LEFT UNEDITED IS ASKED ABOUT AND NEVER ENTERED (finding 2 of the review
/// of the draft-hold work): Esc accepts the search's match in Claude Code 2.1.318 and Muse Code
/// 1.4.14 alike, so a person backing out of the search leaves the old prompt in the composer, in typed
/// ink, the cursor home (measured, `claude-2.1.318-history-search-left`,
/// `muse-1.4.14-R7309.1-history-search-left`, each with its cells; the search's frames before it are
/// those screens under the search's measured row, the cursor after its query). Nobody typed it, so
/// the point is asked about (`RECALL_STANDS`) and nothing is entered or typed for 600 s, in a
/// 12-column window too, where the match's label stands two cells wide round a blank row
/// (measured, `claude-2.1.318-history-search-12col`, `-left-12col`). A query that matched nothing
/// over a person's draft leaves the draft (scene CD, measured), which is theirs: entered once its
/// grace is out. And a person's edit of the old prompt makes it theirs: asked about while it stood
/// unedited, it is entered a grace after the edit.
/// NEGATIVE CONTROLS: `Session::see_draft` not keeping what the search showed, and the old prompt is
/// entered once its grace is out (scenes CM and CR, measured on `0d3f571d04`); `Searched::seen`
/// keeping the search's last text whatever the composer held before, and the person's own draft is
/// asked about and never entered; `Session::left_composer` ignoring what the search left, and the old
/// prompt is entered; `phase::history_search` ending its footer at the first blank row, and the
/// 12-column match is entered (lab scene CM at 12 columns, measured on `cdef270607`).
#[test]
fn an_old_prompt_the_search_left_unedited_is_asked_about_and_never_entered() {
    use crate::supervise::policy::turn_end::RECALL_STANDS;
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::{
        HISTORY_SEARCH_12COL_2_1_318, HISTORY_SEARCH_LEFT_2_1_318,
        HISTORY_SEARCH_LEFT_12COL_2_1_318, HISTORY_SEARCH_LEFT_12COL_CELLS_2_1_318,
        HISTORY_SEARCH_LEFT_CELLS_2_1_318, cells, cursor, screen,
    };
    let measured = |text: &str| -> Vec<((usize, usize), String)> {
        cells(text)
            .into_iter()
            .map(|(r, c, reply)| ((r, c), reply))
            .collect()
    };
    // Claude Code: the left frame as measured; the search's frame is it with the search's row in
    // place of the footer's first, the cursor after its query.
    let left = screen(HISTORY_SEARCH_LEFT_2_1_318);
    let (caret, home) = cursor(HISTORY_SEARCH_LEFT_2_1_318).expect("a measured cursor");
    assert_eq!(home, 2);
    let footer = caret + 2;
    let mode = "  ⏵⏵ auto mode on (shift+tab to cycle)";
    assert_eq!(left[footer], mode);
    let searching = |label: &str, text: &str| {
        let mut rows = left.clone();
        rows[caret] = format!("❯ {text}").trim_end().to_string();
        rows[footer] = format!("  {label}  {}", mode.trim_start());
        rows
    };
    let at = |rows: &[String], text: &str| {
        let mut rows = rows.to_vec();
        rows[caret] = format!("❯ {text}").trim_end().to_string();
        rows
    };
    let mut busy_draft = busy_screen();
    let busy_caret = busy_draft
        .iter()
        .position(|r| r.starts_with('❯'))
        .expect("the caret");
    let own = "my own words here";
    busy_draft[busy_caret] = format!("❯ {own}");
    let old = "alpha old prompt one";
    let edited = format!("{old} today");
    // (tag, screens, cursors by screen, the screen whose first read a grace runs from, cells, entered)
    type Arm = (
        &'static str,
        Vec<Vec<String>>,
        Vec<(usize, usize)>,
        Option<usize>,
        Vec<((usize, usize), String)>,
        bool,
    );
    let arms: Vec<Arm> = vec![
        (
            "match over nothing, Esc",
            vec![
                busy_screen(),
                searching("search prompts: alpha", old),
                left.clone(),
            ],
            vec![(0, 2), (footer, 23), (caret, 2)],
            None,
            measured(HISTORY_SEARCH_LEFT_CELLS_2_1_318),
            false,
        ),
        (
            "match over a draft, Esc",
            vec![
                busy_draft.clone(),
                searching("search prompts: alpha", old),
                left.clone(),
            ],
            vec![(busy_caret, 2 + own.len()), (footer, 23), (caret, 2)],
            None,
            measured(HISTORY_SEARCH_LEFT_CELLS_2_1_318),
            false,
        ),
        (
            "nothing over a draft, Esc",
            vec![
                busy_draft.clone(),
                searching("no matching prompt: zzq", own),
                at(&left, own),
            ],
            vec![
                (busy_caret, 2 + own.len()),
                (footer, 25),
                (caret, 2 + own.len()),
            ],
            None,
            Vec::new(),
            true,
        ),
        (
            "match over nothing, Esc, then edited",
            vec![
                busy_screen(),
                searching("search prompts: alpha", old),
                left.clone(),
                at(&left, &edited),
            ],
            vec![(0, 2), (footer, 23), (caret, 2), (caret, 2 + edited.len())],
            Some(3),
            measured(HISTORY_SEARCH_LEFT_CELLS_2_1_318),
            true,
        ),
    ];
    let recall_asked = |t: &Held| {
        t.m.requests
            .iter()
            .any(|r| r.starts_with("meta set attention") && r.contains(&RECALL_STANDS[..60]))
    };
    let mut outcomes = Vec::new();
    for (tag, screens, cursors, timed, cells, entered) in arms {
        let n = screens.len();
        let mut m = fenced(screens);
        for (i, (row, col)) in cursors.into_iter().enumerate() {
            m.screen_cursors.insert(i, row);
            m.screen_cols.insert(i, col);
        }
        m.cell_attrs = "none";
        m.cells = cells.into_iter().collect();
        let mut offsets = vec![Duration::ZERO, Duration::from_secs(150)];
        if n == 4 {
            offsets.push(Duration::from_secs(300));
        }
        m.timeline = Some((1, offsets));
        m.stall_sleep = Some(Duration::from_secs(3600));
        m.exit_when = Some(acted_or_600s_on);
        let t = held_run(&format!("history-search-recall-{tag}"), m);
        let got = (t.enters(), t.typed(), recall_asked(&t));
        // The recall edited later is asked about while it stands unedited, then sent a grace
        // after the edit, the person's own draft.
        let want = match (entered, timed) {
            (false, _) => (0, 0, true),
            (true, None) => (1, 0, false),
            (true, Some(_)) => (1, 0, true),
        };
        if let Some(from) = timed.filter(|_| got == want) {
            let enter = t.enter_at().expect("the Enter");
            let from = t.first_read_of(from);
            assert!(
                enter >= from + Duration::from_secs(119),
                "{tag}: entered {:?} after the edit: {}",
                enter.saturating_duration_since(from),
                t.evidence()
            );
        }
        outcomes.push((tag, got, want, t.evidence()));
    }
    // A grey suggestion the old prompt repeats, the person at the keys while it stands: the search
    // opened over it with no query (measured, `claude-2.1.318-history-search-over-suggestion`, its
    // glyphs' cells `999999 … dim`) held no draft before it, so the match Esc leaves is the
    // search's, though it reads the same words.
    let grey: std::collections::BTreeMap<(usize, usize), String> = [(2, 'a'), (3, 'l')]
        .into_iter()
        .map(|(col, glyph)| ((caret, col), format!("OK {glyph} 999999 111318 dim")))
        .collect();
    let mut m = fenced(vec![
        busy_screen(),
        left.clone(),
        searching("search prompts: ", old),
        searching("search prompts: alpha", old),
        left.clone(),
    ]);
    for (i, (row, col)) in [(0, 2), (caret, 2), (footer, 18), (footer, 23), (caret, 2)]
        .into_iter()
        .enumerate()
    {
        m.screen_cursors.insert(i, row);
        m.screen_cols.insert(i, col);
    }
    m.cell_attrs = "none";
    m.cells = measured(HISTORY_SEARCH_LEFT_CELLS_2_1_318)
        .into_iter()
        .collect();
    m.cells_on = [(1, grey.clone()), (2, grey)].into_iter().collect();
    // The person's keys: Ctrl-R over the suggestion, then the query.
    m.human_on = [(1, "200"), (2, "200"), (3, "200")].into_iter().collect();
    m.timeline = Some((
        1,
        vec![
            Duration::ZERO,
            Duration::from_millis(500),
            Duration::from_secs(2),
            Duration::from_secs(152),
        ],
    ));
    m.stall_sleep = Some(Duration::from_secs(3600));
    m.exit_when = Some(acted_or_600s_on);
    let t = held_run("history-search-recall-over-suggestion", m);
    outcomes.push((
        "a suggestion the match repeats, Esc",
        (t.enters(), t.typed(), recall_asked(&t)),
        (0, 0, true),
        t.evidence(),
    ));
    // Claude Code at 12 columns, its match's label two cells wide round a blank row (measured,
    // `claude-2.1.318-history-search-12col`, then the frame Esc left, `-left-12col`, with its
    // cells): lab scene CM pressed Enter on it where the label went unread.
    {
        let open = screen(HISTORY_SEARCH_12COL_2_1_318);
        let left = screen(HISTORY_SEARCH_LEFT_12COL_2_1_318);
        let mut m = fenced(vec![busy_screen(), open, left]);
        for (i, text) in [
            (1, HISTORY_SEARCH_12COL_2_1_318),
            (2, HISTORY_SEARCH_LEFT_12COL_2_1_318),
        ] {
            let (row, col) = cursor(text).expect("a measured cursor");
            m.screen_cursors.insert(i, row);
            m.screen_cols.insert(i, col);
        }
        m.cell_attrs = "none";
        m.cells = measured(HISTORY_SEARCH_LEFT_12COL_CELLS_2_1_318)
            .into_iter()
            .collect();
        m.timeline = Some((1, vec![Duration::ZERO, Duration::from_secs(150)]));
        m.stall_sleep = Some(Duration::from_secs(3600));
        m.exit_when = Some(acted_or_600s_on);
        let t = held_run("history-search-recall-12col", m);
        outcomes.push((
            "12 columns: match over nothing, Esc",
            (t.enters(), t.typed(), recall_asked(&t)),
            (0, 0, true),
            t.evidence(),
        ));
    }
    // Muse Code: the same, its search's row between the bottom rule and the footer: scoped at 144
    // columns, its scope dropped at 26 (measured, `muse-1.4.14-R7309.1-history-search-26col`; the
    // frame Esc left, `-left-26col`, with its cells: the search's frame is it with that row over
    // the footer, row for row the measured one).
    let wide = screen(u::HISTORY_SEARCH_LEFT);
    let (caret, _) = cursor(u::HISTORY_SEARCH_LEFT).expect("cursor");
    let mut open = wide.clone();
    open.remove(0);
    open.insert(
        caret + 1,
        "  [everywhere] search prompts: alpha".to_string(),
    );
    let narrow = screen(u::HISTORY_SEARCH_LEFT_26COL);
    let (narrow_caret, _) = cursor(u::HISTORY_SEARCH_LEFT_26COL).expect("cursor");
    let mut narrow_open = narrow.clone();
    narrow_open.insert(narrow_caret + 2, "  search prompts: alpha".to_string());
    narrow_open.pop();
    for (tag, screens, (row, col), left_at, left_cells) in [
        (
            "muse: match over nothing, Esc",
            vec![screen(u::BUSY_1_4_10), open, wide],
            (caret + 1, 36),
            caret,
            u::HISTORY_SEARCH_LEFT_CELLS,
        ),
        (
            "muse at 26 columns: match over nothing, Esc",
            vec![screen(u::BUSY_1_4_10), narrow_open, narrow],
            (narrow_caret + 2, 23),
            narrow_caret,
            u::HISTORY_SEARCH_LEFT_26COL_CELLS,
        ),
    ] {
        let mut m = muse_fenced(screens);
        m.screen_cursors.insert(1, row);
        m.screen_cols.insert(1, col);
        m.screen_cursors.insert(2, left_at);
        m.screen_cols.insert(2, 2);
        m.cell_attrs = "none";
        m.cells = measured(left_cells).into_iter().collect();
        m.timeline = Some((1, vec![Duration::ZERO, Duration::from_secs(150)]));
        m.stall_sleep = Some(Duration::from_secs(3600));
        m.exit_when = Some(acted_or_600s_on);
        let t = held_run("history-search-recall-muse", m);
        outcomes.push((
            tag,
            (t.enters(), t.typed(), recall_asked(&t)),
            (0, 0, true),
            t.evidence(),
        ));
    }
    // A read inside Muse Code's unbracketed redraw: the person's draft whole, the cursor still on
    // the footer (docs/measured/half-drawn-frame-2026-10-22.md), then placed. No search's row
    // stands, so no search opened: the draft is theirs, entered a grace after the read that said it
    // typed.
    let (short, whole) = ("fix the bu", "fix the bug");
    let drafted = muse_typed(whole);
    let caret = aterm_phase::muse::composer(&drafted).expect("the composer");
    let mut m = muse_fenced(vec![
        screen(u::BUSY_1_4_10),
        muse_typed(short),
        drafted.clone(),
        drafted.clone(),
    ]);
    for (i, (row, col)) in [
        (caret, 2 + short.len()),
        (muse_footer(&drafted), 40),
        (caret, 2 + whole.len()),
    ]
    .into_iter()
    .enumerate()
    {
        m.screen_cursors.insert(i + 1, row);
        m.screen_cols.insert(i + 1, col);
    }
    m.cell_attrs = "none";
    m.timeline = Some((
        1,
        vec![
            Duration::ZERO,
            Duration::from_secs(1),
            Duration::from_secs(2),
        ],
    ));
    m.stall_sleep = Some(Duration::from_secs(3600));
    m.exit_when = Some(acted_or_600s_on);
    let t = held_run("history-search-muse-half-drawn", m);
    let got = (t.enters(), t.typed(), recall_asked(&t));
    if got == (1, 0, false) {
        let enter = t.enter_at().expect("the Enter");
        let from = t.first_read_of(3);
        assert!(
            enter >= from + Duration::from_secs(119),
            "muse half-drawn: entered {:?} after the read that said typed: {}",
            enter.saturating_duration_since(from),
            t.evidence()
        );
    }
    outcomes.push((
        "muse: a draft read inside the redraw, the cursor on the footer",
        got,
        (1, 0, false),
        t.evidence(),
    ));
    let wrong: Vec<_> = outcomes
        .iter()
        .filter(|(_, got, want, _)| got != want)
        .collect();
    assert!(
        wrong.is_empty(),
        "(enters, typed, asked) of each: {:#?}\n{:#?}",
        outcomes
            .iter()
            .map(|(tag, got, want, _)| (tag, got, want))
            .collect::<Vec<_>>(),
        wrong
            .iter()
            .map(|(tag, _, _, evidence)| (tag, evidence))
            .collect::<Vec<_>>()
    );
}

/// THE UPGRADE GATE NEVER READS THE SEARCH AS AN EMPTY COMPOSER (`upgrade::composer_is_empty`, which
/// the Claude upgrade's typing and the window's own `/` commands go through): over every measured
/// search of `history_searches`, each with its measured cursor and its first glyph's ink.
/// NEGATIVE CONTROL: `screen::composer_holds_no_draft` without its cursor rule
/// (`cursor_under_composer`), and over nothing it reads empty.
#[test]
fn the_upgrade_gate_never_reads_the_history_search_as_an_empty_composer() {
    for (tag, rows, at, cells) in history_searches() {
        let dim = cells.iter().any(|(_, reply)| reply.ends_with(" dim"));
        assert!(
            !crate::harness::upgrade::composer_is_empty(&rows, Some(at), dim),
            "{tag}"
        );
    }
}

/// A SEARCH LEFT AT A BREAK LEAVES NO DRAFT BEHIND (the review of `09eff949d8`): a break of the
/// agent's own background work, the prompt-history search open over an empty composer, its query
/// matching nothing and the cursor on its row (as `claude-2.1.318-history-search-no-match` draws it),
/// no key measured; then left (Esc), the composer empty and the cursor home. While the search stands
/// the door's read at a write finds no composer it could type into; once it has gone nothing holds
/// the break's lines as a draft.
/// NEGATIVE CONTROLS: `Session::composer_draft` without the cursor rule (`Screen::cursor_under`), and
/// the door reads the search's empty row as an empty composer to type into; `Session::composer_typed`
/// keeping the search's row as a draft seen, and the break's guards hold an empty draft after the
/// search has gone.
#[test]
fn a_search_left_at_a_break_leaves_no_draft_behind() {
    let (closed, caret) = at_break("");
    let footer = closed.len() - 1;
    let mut open = closed.clone();
    open[footer] = format!("  no matching prompt: zzq  {}", closed[footer].trim_start());
    let mut m = Mock::new(vec![open, closed]);
    m.cell_attrs = "none";
    m.dim_past = true;
    m.screen_cursors.insert(0, footer);
    m.screen_cols.insert(0, 27);
    m.screen_cursors.insert(1, caret);
    m.screen_cols.insert(1, 2);
    let opts = hosted(600);
    let mut s = session(&mut m, Some("@s-1".to_string()));
    s.set_clock(TEST_NOW, PDT);
    s.setup.background_settle = Duration::ZERO;
    let mut since = Some(Instant::now());
    let searching = s.screen().expect("the search");
    s.host_steps_in_background(&searching, &mut since, Some(&opts.policy), &mut Alone)
        .expect("the break's step");
    assert!(
        s.door_draft(&searching),
        "the door's read of the search: no empty composer"
    );
    let left = s.screen().expect("the search left");
    s.host_steps_in_background(&left, &mut since, Some(&opts.policy), &mut Alone)
        .expect("the break's next step");
    let guards = s.occupancy_guards(s.fx.mono(), &opts.policy, &left.rows);
    assert_eq!(
        (guards.draft, s.door_draft(&left)),
        (false, false),
        "held after the search went: seen {:?}",
        s.draft_seen
    );
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

/// A COMPOSER WHOSE CELL COULD NOT SAY IS LOOKED AT AGAIN (the reviews of `6337470849`): the
/// decision's read of the glyph's cell comes from a frame still open (` frame=open`), with no key
/// measured and no content seq moving after it. That read decides nothing, and the next wait step's
/// read looks again: a plain cell is a draft, which keeps its grace from that read and is then
/// sent; a dim one with the cursor home is a suggestion, typed over at that step. A host that refuses
/// the cell for good is never written to, its screen read once per wait step, no faster, and the
/// journal says what holds the point.
/// NEGATIVE CONTROLS: `wait_for_next` without its `Composer::Unknown` step, and the first two never
/// act: no read moves the point past a cell that came back; `turn_end_write` without its
/// `composer-unknown` arm, and the journal says only `why=policy`; `moved_past` reading the screen on
/// that step too, and 61 reads over the run's 600 s, the look's own read being the next one.
#[test]
fn a_composer_whose_cell_could_not_say_is_looked_at_again() {
    let run = |attrs: &'static str, col: usize| {
        let mut m = fenced(vec![busy_screen(), typed_in(SAID, HINT)]);
        m.dim_past = true;
        m.cell_attrs = attrs;
        m.cell_open_reads = 1;
        m.cursor_on_caret = true;
        m.cursor_col = Some(col);
        m.stall_sleep = Some(Duration::from_secs(3600));
        m.exit_when = Some(acted);
        held_run("draft-cell-open", m)
    };
    let t = run("none", 2 + HINT.chars().count());
    let seen = t.first_read_of(1);
    let acted = t.acted_at().unwrap_or_else(|| panic!("{}", t.evidence()));
    assert!(
        acted >= seen + Duration::from_secs(139),
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
    m.stall_sleep = Some(Duration::from_secs(3600));
    let t = held_run("draft-cell-refused", m);
    t.left_it_alone("refused");
    let reads = t.m.read_times.len();
    assert!(
        reads <= 600 / 20 + 4,
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

/// THE GUARDS' MEMORY IS OF THE CURSOR TOO (`composer_for_guards`, the review of `bd67622c8c`): a
/// suggestion its cursor is home on, the guards holding nothing; the same words, the cursor moved off
/// home by its column alone or its row alone, as a person who took the suggestion or a redraw leaves
/// it. The next step asks the cell again, which says neither, and the guards hold.
/// NEGATIVE CONTROLS: `composer_for_guards` remembering no column, and the column's move holds
/// nothing; no row, and the row's.
#[test]
fn a_breaks_guards_ask_again_when_the_cursor_moves_off_a_suggestion() {
    let (shown, caret) = at_break(HINT);
    for (what, from, to) in [
        ("its column", (caret + 1, 2), (caret + 1, 40)),
        ("its row", (caret, 2), (0, 2)),
    ] {
        let mut m = Mock::new(vec![shown.clone(), shown.clone()]);
        m.cell_attrs = "dim";
        m.dim_past = true;
        m.screen_cursors.insert(0, from.0);
        m.screen_cols.insert(0, from.1);
        m.screen_cursors.insert(1, to.0);
        m.screen_cols.insert(1, to.1);
        let opts = hosted(600);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(TEST_NOW, PDT);
        s.setup.background_settle = Duration::ZERO;
        let mut since = Some(Instant::now());
        let first = s.screen().expect("the break");
        s.host_steps_in_background(&first, &mut since, Some(&opts.policy), &mut Alone)
            .expect("the break's step");
        let now = s.fx.mono();
        let guards = s.occupancy_guards(now, &opts.policy, &first.rows);
        assert_eq!((guards.draft, guards.person), (false, false), "{what}");
        let moved = s.screen().expect("the cursor moved");
        s.host_steps_in_background(&moved, &mut since, Some(&opts.policy), &mut Alone)
            .expect("the break's next step");
        let now = s.fx.mono();
        let guards = s.occupancy_guards(now, &opts.policy, &moved.rows);
        assert_eq!((guards.draft, guards.person), (true, true), "{what}");
    }
}

/// A turn's end at screen 1, and `rows` redrawn every second for `span` seconds.
fn ticking(rows: &[String], span: u32) -> Mock {
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
}

/// A SCREEN TICKING OVER A COMPOSER ASKS ITS CELL NO MORE FOR IT (the review of `5ad7f68c8f`): a footer
/// that ticks every second moves the content seq, so each move is a read. At an idle point over a real
/// draft no survey is drawn and nothing else reads the draft, so no read of the 120 s asks the cell
/// but the decision's. At a break, the same composer and cursor are asked about once: a draft, and a
/// suggestion its cursor is home on, of one row or wrapped onto two.
/// NEGATIVE CONTROLS: `survey` asking the cell with no survey drawn and none in hand, and the idle
/// point's cells count its reads (135); `composer_for_guards` without its memory, and the break's do
/// (145); `draft_read` saying typed of two rows with the cursor home, and the two-row suggestion
/// flips between seen and let go at every read (145).
#[test]
fn a_screen_ticking_over_a_composer_asks_its_cell_no_more_for_it() {
    let mut m = ticking(&typed_in(SAID, DRAFT), 130);
    m.cell_attrs = "none";
    m.cursor_on_caret = true;
    m.cursor_col = Some(2 + DRAFT.chars().count());
    m.exit_when = Some(acted);
    let t = held_run("draft-tick-idle", m);
    assert!(t.m.read_times.len() > 100, "it ticked: {}", t.evidence());
    assert!(t.cells() <= 4, "{} cells: {}", t.cells(), t.evidence());
    assert_eq!((t.enters(), t.typed()), (1, 0), "{}", t.evidence());

    let (wrapped_hint, wrapped_caret) = at_break(HINT);
    let mut wrapped_hint = wrapped_hint;
    wrapped_hint.insert(wrapped_caret + 1, "  and then the rest of it".to_string());
    for (what, shown, attrs, col) in [
        (
            "a draft",
            at_break(DRAFT).0,
            "none",
            2 + DRAFT.chars().count(),
        ),
        ("a suggestion", at_break(HINT).0, "dim", 2),
        ("a suggestion of two rows", wrapped_hint, "dim", 2),
    ] {
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

/// A HOLD ASKS EACH READ ITS CELL ONCE (the review of `98b9c4d903`): grey text with the cursor past it,
/// for good, a screen ticking every second, under a host that wants its steps. The hold's look reads
/// the cell of each read, and the host's step before its guards asks nothing more of that read.
/// NEGATIVE CONTROL: `host_steps_here` asking for its guards under a hold too, and the cells outnumber
/// the reads, 263 to 204.
#[test]
fn a_hold_asks_each_read_its_cell_once() {
    let clock = virtual_clock();
    let host = Arc::new(IdleTeller {
        clock: Arc::clone(&clock),
        steps: std::sync::Mutex::new(Vec::new()),
    });
    let mut m = ticking(&typed_in(SAID, HINT), 60);
    m.cell_attrs = GREY;
    m.dim_past = true;
    m.cursor_on_caret = true;
    m.cursor_col = Some(2 + HINT.chars().count());
    m.exit_at_request = Some(600);
    let (dir, journal) = journal_file("draft-hold-one-cell");
    let opts = SuperviseOpts {
        journal: Some(journal),
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        ..hosted(600)
    };
    let _ = record_watch_on(&mut m, &opts, clock, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            min_work: Duration::ZERO,
            ..TurnEndTiming::default()
        });
    });
    let _ = std::fs::remove_dir_all(&dir);
    let count = |head: &str| m.requests.iter().filter(|r| r.starts_with(head)).count();
    let (reads, cells) = (count("text "), count("cell "));
    assert!(reads > 50, "it ticked: {reads} reads");
    assert!(
        cells <= reads,
        "{cells} cells over {reads} reads: {:#?}",
        m.requests
    );
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
        s.own_line = Some(OwnLine::of(own, OwnBy::Line, None));
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

/// THE DOOR READS A COMPOSER ONCE, ASKING A CELL ONLY WHERE THE ROWS CANNOT SAY (finding 4 of the
/// review of the draft-hold work): `left_seen` read the composer by its rows (`draft_read`) and again
/// by the door's read (`Session::door_draft`), and the door's guards read it that second way alone,
/// which asked the glyph's cell over a line the rows already said was typed. One read now serves both,
/// asking what the measured looks need (`docs/measured/composer-cells-2026-11-03.md`): a typed line,
/// the cursor past it, no cell; the same line home under Claude Code's drawn caret, two (inverse, then
/// plain: typed); a suggestion home, two (inverse, then dim: none); an empty composer, none; the
/// prompt-history search, none (its cursor under the composer, `claude-2.1.318-history-search-no-
/// match`). `left_seen` says what the door's read says and asks what it asks.
/// NEGATIVE CONTROL: `door_draft` without its `draft_read` arm, and the typed line with the cursor past
/// it asks its cell at each read.
#[test]
fn the_doors_read_asks_a_cell_only_where_the_rows_cannot_say() {
    use crate::supervise::facts::LeftLook;
    use aterm_phase::prompt::fixtures::{HISTORY_SEARCH_NO_MATCH_2_1_318, cursor, screen};
    type Look<'a> = (
        &'a str,
        Vec<String>,
        (usize, usize),
        Vec<((usize, usize), &'a str)>,
        bool,
        usize,
    );
    let (empty, caret) = at_break("");
    let typed = at_break("hello there").0;
    let suggested = at_break("run the whole test suite again").0;
    let looks: [Look<'_>; 5] = [
        (
            "typed, the cursor past it",
            typed.clone(),
            (caret, 13),
            Vec::new(),
            true,
            0,
        ),
        (
            "typed, home",
            typed,
            (caret, 2),
            vec![
                ((caret, 2), "OK h 111318 d0d0d0 inverse"),
                ((caret, 3), "OK e d0d0d0 111318 none"),
            ],
            true,
            2,
        ),
        (
            "a suggestion, home",
            suggested,
            (caret, 2),
            vec![
                ((caret, 2), "OK r 111318 d0d0d0 inverse"),
                ((caret, 3), "OK u 999999 111318 dim"),
            ],
            false,
            2,
        ),
        ("empty", empty, (caret, 2), Vec::new(), false, 0),
        (
            "the search",
            screen(HISTORY_SEARCH_NO_MATCH_2_1_318),
            cursor(HISTORY_SEARCH_NO_MATCH_2_1_318).expect("a measured cursor"),
            Vec::new(),
            true,
            0,
        ),
    ];
    let mut outcomes = Vec::new();
    for (look, rows, (row, col), cells, draft, asked) in looks {
        let mut m = Mock::new(vec![rows]);
        m.screen_cursors.insert(0, row);
        m.screen_cols.insert(0, col);
        m.cell_attrs = "none";
        m.cells = cells
            .into_iter()
            .map(|(at, reply)| (at, reply.to_string()))
            .collect();
        let mut s = session(&mut m, Some("@s-1".to_string()));
        let scr = s.screen().expect("the look");
        let asked_now = |s: &Session<'_, Mock>| {
            s.ctl
                .requests
                .iter()
                .filter(|r| r.split_whitespace().find(|w| !w.starts_with('@')) == Some("cell"))
                .count()
        };
        let before = asked_now(&s);
        let door = s.door_draft(&scr);
        let door_cells = asked_now(&s) - before;
        let left = match s.left_seen(&scr, false) {
            LeftLook::Read { draft, .. } => draft,
            other => panic!("{look}: {other:?}"),
        };
        let left_cells = asked_now(&s) - before - door_cells;
        outcomes.push((
            look,
            (door, left, door_cells, left_cells),
            (draft, Some(draft), asked, asked),
        ));
    }
    let wrong: Vec<_> = outcomes
        .iter()
        .filter(|(_, got, want)| got != want)
        .collect();
    assert!(
        wrong.is_empty(),
        "(door, left_seen, the door's cells, left_seen's cells): {outcomes:#?}"
    );
}

/// A SEARCH IN A NARROW WINDOW IS NEVER PRESSED OR TYPED INTO (finding 5 of the review of the
/// draft-hold work, and finding 7's Claude Code under 22 columns): measured in the real-Claude lab on
/// 2.1.318 at 22, 14 and 12 columns (`claude-2.1.318-history-search-22col`, `-14col`,
/// `-no-match-14col`, `-no-match-12col`, each with its cursor), the search's label wraps down its own
/// column, and the no-match frames at 14 and 12 columns have a blank row inside it, the space
/// between two of its words, which `phase::history_search` reads across. Its
/// cursor stands under the composer's bottom rule, as on every measured search and on no other measured
/// screen (`screen::cursor_under_composer`), so the composer reads as neither an empty one to type into
/// nor a person's draft to send: nothing is pressed or typed for 600 s, and the upgrade's gate reads no
/// empty composer.
/// NEGATIVE CONTROL: `Session::composer_draft` without the cursor rule, and the 12-column search's
/// empty row reads as an empty composer: the loop's continuation is typed into the search.
#[test]
fn a_search_in_a_narrow_window_is_never_pressed_or_typed_into() {
    use aterm_phase::prompt::fixtures::{
        HISTORY_SEARCH_14COL_2_1_318, HISTORY_SEARCH_22COL_2_1_318,
        HISTORY_SEARCH_NO_MATCH_12COL_2_1_318, HISTORY_SEARCH_NO_MATCH_14COL_2_1_318, cursor,
        screen,
    };
    let mut outcomes = Vec::new();
    for (tag, text) in [
        ("22col", HISTORY_SEARCH_22COL_2_1_318),
        ("14col", HISTORY_SEARCH_14COL_2_1_318),
        ("no-match-14col", HISTORY_SEARCH_NO_MATCH_14COL_2_1_318),
        ("no-match-12col", HISTORY_SEARCH_NO_MATCH_12COL_2_1_318),
    ] {
        let rows = screen(text);
        let at = cursor(text).expect("a measured cursor");
        let gate = crate::harness::upgrade::composer_is_empty(&rows, Some(at), false);
        let mut m = fenced(vec![busy_screen(), rows]);
        m.screen_cursors.insert(1, at.0);
        m.screen_cols.insert(1, at.1);
        m.cell_attrs = "none";
        m.timeline = Some((1, vec![Duration::ZERO]));
        m.stall_sleep = Some(Duration::from_secs(3600));
        m.exit_when = Some(acted_or_600s_on);
        let t = held_run(&format!("narrow-search-{tag}"), m);
        outcomes.push((tag, (t.enters(), t.typed(), gate), t.evidence()));
    }
    let wrong: Vec<_> = outcomes
        .iter()
        .filter(|(_, got, _)| *got != (0, 0, false))
        .collect();
    assert!(wrong.is_empty(), "(enters, typed, the gate): {wrong:#?}");
}

/// MUSE CODE'S SEARCH IN A NARROW WINDOW IS NEVER PRESSED OR TYPED INTO (finding 7 of the review of
/// the draft-hold work): measured on Muse Code 1.4.14 at 34, 26 and 22 columns
/// (`muse-1.4.14-R7309.1-history-search-no-match-34col`, `-26col`, `-no-match-22col`, each with its
/// cursor), the search's row drops its scope, or is cut from the left at 22. Muse's reader takes that
/// lone row between the bottom rule and the footer for the search's (`muse::is_lone_search_row`), so
/// the point asks the search's own question at once (`SEARCH_DIVERTS`) and nothing is pressed or
/// typed for 600 s (the upgrade's gate on the same frames is
/// `the_gate_reads_the_cursor_as_the_loop_does`'s).
/// NEGATIVE CONTROL: `is_lone_search_row` taking the scoped row alone, and the frames read as none:
/// held, asked about by no one.
#[test]
fn a_muse_search_in_a_narrow_window_is_never_pressed_or_typed_into() {
    use crate::supervise::policy::turn_end::SEARCH_DIVERTS;
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::{cursor, screen};
    let mut outcomes = Vec::new();
    for (tag, text) in [
        ("no-match-34col", u::HISTORY_SEARCH_NO_MATCH_34COL),
        ("26col", u::HISTORY_SEARCH_26COL),
        ("no-match-22col", u::HISTORY_SEARCH_NO_MATCH_22COL),
    ] {
        let rows = screen(text);
        let at = cursor(text).expect("a measured cursor");
        let mut m = muse_fenced(vec![screen(u::BUSY_1_4_10), rows]);
        m.screen_cursors.insert(1, at.0);
        m.screen_cols.insert(1, at.1);
        m.cell_attrs = "none";
        m.timeline = Some((1, vec![Duration::ZERO]));
        m.stall_sleep = Some(Duration::from_secs(3600));
        m.exit_when = Some(acted_or_600s_on);
        let t = held_run(&format!("narrow-muse-search-{tag}"), m);
        let asked =
            t.m.requests
                .iter()
                .any(|r| r.starts_with("meta set attention") && r.contains(&SEARCH_DIVERTS[..60]));
        outcomes.push((tag, (t.enters(), t.typed(), asked), t.evidence()));
    }
    let wrong: Vec<_> = outcomes
        .iter()
        .filter(|(_, got, _)| *got != (0, 0, true))
        .collect();
    assert!(wrong.is_empty(), "(enters, typed, asked): {wrong:#?}");
}
