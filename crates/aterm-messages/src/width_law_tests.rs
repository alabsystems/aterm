// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test-only: the width law. One intent reads one way across the band at
//! every width, whatever each row's title length.

use crate::center::MessageCenter;
use crate::glass::{
    CapsuleRole, Links, Presentation, RowKind, RowLayout, RowSpec, layout_row, layout_row_with,
};
use crate::log::MessageLog;
use crate::model::{
    Hold, Intent, Loads, Message, MessageId, Severity, UpgradeWord, WallStamp, tags,
};
use crate::need::{Ask, Reason, StandingWhy, Then};
use crate::text::char_width;
use crate::{Duration, ETA_SHORT_W, Instant, PROGRESS_GRACE};

fn stamp() -> WallStamp {
    WallStamp { unix_ms: 1_000 }
}

fn upgrade(word: UpgradeWord) -> Intent {
    Intent::AgentUpgrade {
        tab: "s-1".into(),
        to: "2.1.282".into(),
        word,
    }
}

fn band(msgs: Vec<Message>, cols: usize) -> Presentation {
    let now = Instant::now();
    let mut c = MessageCenter::new(MessageLog::empty(), now);
    for msg in msgs {
        c.post(msg, stamp(), now);
    }
    c.commit_rows(now, 3);
    c.presentation(cols, &char_width, None, Links::Painted)
}

/// The painted capsule texts of each row, the implicit link included.
fn labels(p: &Presentation) -> Vec<Vec<String>> {
    p.rows
        .iter()
        .map(|r| r.capsules.iter().map(|c| c.text.clone()).collect())
        .collect()
}

/// The row body opens Details at every width, so where dropping `Details ›`
/// alone fits the row, the authored capsule keeps its words (`Open
/// aterm.toml`, never `Edit` beside a sibling row that kept it). A row that
/// does not fit even so still takes the short forms.
#[test]
fn the_link_goes_before_an_authored_label_shortens() {
    let row = |title: &str| {
        Message::new(tags::CONFIG, Severity::Warn, title)
            .action(Intent::OpenConfigEditor { line: None })
    };
    let p = band(vec![row("Couldn't use a setting's value")], 60);
    assert_eq!(labels(&p), vec![vec!["Open aterm.toml".to_string()]]);
    // Wide enough for both: the link stays.
    let p = band(vec![row("Couldn't use a setting's value")], 80);
    assert_eq!(
        labels(&p),
        vec![vec![
            "Open aterm.toml".to_string(),
            "Details \u{203a}".to_string()
        ]]
    );
    // Too long even without the link: the short form.
    let p = band(
        vec![row("Couldn't apply the cursor trail's second pack now")],
        60,
    );
    assert_eq!(labels(&p), vec![vec!["Edit".to_string()]]);
}

/// A label one row at this width had to paint short is painted short on every
/// row carrying it. Alone, the waiting row keeps `Not today` (the control);
/// beneath a row that had to shorten it, it reads `Not now` too.
#[test]
fn one_intent_reads_one_way_across_the_band() {
    let waits = || {
        Message::new(
            tags::HARNESS,
            Severity::Warn,
            "Claude upgrade waits in tab 2",
        )
        .action(upgrade(UpgradeWord::Now))
        .action(upgrade(UpgradeWord::NotToday))
        .hold(Hold::STANDING)
    };
    let behind = Message::new(
        tags::HARNESS,
        Severity::Warn,
        "Couldn't upgrade Claude in tab 3 yet",
    )
    .action(upgrade(UpgradeWord::NotToday))
    .action(upgrade(UpgradeWord::Skip))
    .hold(Hold::STANDING);
    let alone = band(vec![waits()], 60);
    assert_eq!(
        labels(&alone),
        vec![vec!["Upgrade now".to_string(), "Not today".to_string()]],
        "the control"
    );
    let both = band(vec![waits(), behind], 60);
    let not_today: Vec<&str> = both
        .rows
        .iter()
        .flat_map(|r| r.capsules.iter())
        .filter(|c| c.full_label == UpgradeWord::NotToday.label())
        .map(|c| c.text.as_str())
        .collect();
    assert_eq!(not_today, vec!["Not now", "Not now"], "{:?}", labels(&both));
    // Only the forced label changes: `Upgrade now` keeps its words.
    assert!(
        both.rows.iter().any(|r| r
            .capsules
            .iter()
            .any(|c| c.text == "Upgrade now" && c.role != CapsuleRole::Details)),
        "{:?}",
        labels(&both)
    );
    // Wide enough for every long form, nothing is forced.
    let wide = band(
        vec![waits(), {
            Message::new(
                tags::HARNESS,
                Severity::Warn,
                "Couldn't upgrade Claude in tab 3 yet",
            )
            .action(upgrade(UpgradeWord::NotToday))
            .action(upgrade(UpgradeWord::Skip))
            .hold(Hold::STANDING)
        }],
        120,
    );
    assert!(
        wide.rows
            .iter()
            .flat_map(|r| r.capsules.iter())
            .all(|c| c.text == c.full_label),
        "{:?}",
        labels(&wide)
    );
}

/// Forced short, a row that had room for `10 s left` paints `10s left`, and
/// the cells it saves buy nothing (its excerpt and stats are the ones it had)
/// at every width where it took the long form.
#[test]
fn forced_short_time_words_take_the_short_slot_and_spend_nothing() {
    let spec = RowSpec {
        kind: RowKind::Message(MessageId::FIRST),
        severity: Severity::Info,
        live: true,
        glyph: '\u{21e3}',
        title: "Downloading aterm v0.95.0",
        title_short: None,
        detail0: Some("from github.com over the office network, resumed where it stopped"),
        step: false,
        step_short: None,
        meter: Some((Some(500), "74 MB")),
        busy: false,
        animated: true,
        eta: true,
        load: None,
        load_slot: Loads::NONE,
        capsules: Vec::new(),
    };
    let text = |l: &crate::glass::RowLayout| {
        (
            l.detail.as_ref().map(|(_, d)| d.clone()),
            l.stats.as_ref().map(|(_, s)| s.clone()),
        )
    };
    // Every width where the row's own layout takes the long time words: the
    // forced short form paints `ETA_SHORT_W` and buys nothing with the rest.
    let mut long_widths = 0;
    let mut three_cells_matter = false;
    for cols in 30..=160 {
        let own = layout_row(&spec, cols, &char_width);
        if own.eta.is_none() || own.eta_short {
            continue;
        }
        long_widths += 1;
        let forced = layout_row_with(&spec, cols, &char_width, true);
        assert!(
            forced.eta.is_some() && forced.eta_short,
            "{cols}: {forced:?}"
        );
        assert_eq!(
            text(&forced),
            text(&own),
            "{cols}: the saved cells bought an extra"
        );
        // Non-vacuity: somewhere in the sweep the three cells the short form
        // saves would buy more excerpt or the stats, had they been spent.
        let wider = layout_row(&spec, cols + (crate::ETA_W - ETA_SHORT_W), &char_width);
        if !wider.eta_short && text(&wider) != text(&own) {
            three_cells_matter = true;
        }
    }
    assert!(
        long_widths > 20,
        "the sweep saw the long form: {long_widths}"
    );
    assert!(
        three_cells_matter,
        "the control: three cells change the row somewhere"
    );
}

/// The clause before the excerpt's first `: ` is a whole statement: it stands,
/// with no ellipsis, where the floor would drop the excerpt. Controls: a wider
/// row still cuts through the second clause; a lead of one word or under
/// twelve cells (`was`, `line 3`) is dropped; narrowing never shows more
/// words, nor brings the excerpt back once it went.
#[test]
fn a_whole_lead_clause_stands_where_the_floor_would_drop_the_excerpt() {
    let row = |why: &str| {
        Message::new(
            tags::HARNESS,
            Severity::Warn,
            "Couldn't upgrade Claude in tab 1 yet",
        )
        .line(why)
        .action(upgrade(UpgradeWord::NotToday))
        .action(upgrade(UpgradeWord::Skip))
        .hold(Hold::STANDING)
    };
    let excerpt = |why: &str, cols: usize| {
        band(vec![row(why)], cols).rows[0]
            .detail
            .as_ref()
            .map(|(_, d)| d.clone())
    };
    let why = "behind for 7 h: its turn is still running";
    // The link and both authored capsules stay whole at 100 columns.
    assert_eq!(
        labels(&band(vec![row(why)], 100)),
        vec![vec![
            "Not today".to_string(),
            "Skip version".to_string(),
            "Details \u{203a}".to_string()
        ]]
    );
    assert_eq!(excerpt(why, 100).as_deref(), Some("behind for 7 h"));
    // Wide enough for the floor: the cut through the second clause.
    let wide = excerpt(why, 103).expect("the floor's cut");
    assert!(
        wide.starts_with("behind for 7 h: ") && wide.ends_with('\u{2026}'),
        "{wide}"
    );
    // Too narrow even for the clause: dropped whole, never cut.
    assert_eq!(excerpt(why, 96), None);
    // A lead that says nothing alone is not offered.
    assert_eq!(excerpt("was: its turn is still running here", 100), None);
    assert_eq!(excerpt("line 3: expected a value after `=`", 100), None);
    // Narrowing: the words shown are the wider width's or fewer, and once
    // gone they stay gone.
    let bare = |s: &str| s.trim_end_matches('\u{2026}').to_string();
    let mut prev: Option<Option<String>> = None;
    let mut clause_widths = 0;
    for cols in (8..=200).rev() {
        let now = excerpt(why, cols);
        if now.as_deref() == Some("behind for 7 h") {
            clause_widths += 1;
        }
        if let Some(before) = &prev {
            match (before, &now) {
                (None, Some(d)) => panic!("{cols}: {d:?} came back"),
                (Some(p), Some(d)) => assert!(
                    bare(p).starts_with(&bare(d)),
                    "{cols}: {d:?} is not a narrowing of {p:?}"
                ),
                _ => {}
            }
        }
        prev = Some(now);
    }
    assert!(
        clause_widths >= 3,
        "the clause stood at {clause_widths} widths"
    );
}

/// Narrowing from 200 to 8 columns never shows more of an excerpt's words,
/// nor brings it back once it went; `excerpt` is the row's excerpt at a width.
fn narrowing_holds(excerpt: &dyn Fn(usize) -> Option<String>) {
    let bare = |s: &str| s.trim_end_matches('\u{2026}').to_string();
    let mut prev: Option<Option<String>> = None;
    for cols in (8..=200).rev() {
        let now = excerpt(cols);
        if let Some(before) = &prev {
            match (before, &now) {
                (None, Some(d)) => panic!("{cols}: {d:?} came back"),
                (Some(p), Some(d)) => assert!(
                    bare(p).starts_with(&bare(d)),
                    "{cols}: {d:?} is not a narrowing of {p:?}"
                ),
                _ => {}
            }
        }
        prev = Some(now);
    }
}

/// A row whose excerpt names each tab and how long (`tab 1 for 7 h · tab 3 for
/// 2 h`) has no `: `; its first ` · ` piece is a whole statement too and
/// stands where the floor would drop the excerpt. Controls: the cut through
/// the second piece at 103, dropped at 90, a first piece under twelve cells
/// not offered, and narrowing monotone.
#[test]
fn a_first_piece_stands_where_the_floor_would_drop_the_excerpt() {
    let row = |why: &str| {
        Message::new(
            tags::HARNESS,
            Severity::Warn,
            "Couldn't upgrade Claude in 2 tabs yet",
        )
        .line(why)
        .action(upgrade(UpgradeWord::Now))
        .action(upgrade(UpgradeWord::NotToday))
        .hold(Hold::STANDING)
    };
    let excerpt = |why: &str, cols: usize| {
        band(vec![row(why)], cols).rows[0]
            .detail
            .as_ref()
            .map(|(_, d)| d.clone())
    };
    let why = "tab 1 for 7 h \u{b7} tab 3 for 2 h";
    assert_eq!(
        labels(&band(vec![row(why)], 100)),
        vec![vec![
            "Upgrade now".to_string(),
            "Not today".to_string(),
            "Details \u{203a}".to_string()
        ]]
    );
    assert_eq!(excerpt(why, 100).as_deref(), Some("tab 1 for 7 h"));
    let wide = excerpt(why, 103).expect("the floor's cut");
    assert!(
        wide.starts_with("tab 1 for 7 h \u{b7} ") && wide.ends_with('\u{2026}'),
        "{wide}"
    );
    assert_eq!(excerpt(why, 90), None);
    assert_eq!(excerpt("tab 1 \u{b7} tab 3 for 2 hours now", 100), None);
    narrowing_holds(&|cols| excerpt(why, cols));
}

/// A cut that says no word past the lead clause (`behind for 26 h:…`, the
/// joint dangling) paints the clause whole instead; one column wider the cut
/// adds `its` and stands. The same holds for a ` · ` piece.
#[test]
fn a_cut_that_adds_no_word_to_the_lead_is_the_lead() {
    let one = |why: &str, cols: usize| {
        let msg = Message::new(
            tags::HARNESS,
            Severity::Warn,
            "Couldn't upgrade Claude in tab 1 yet",
        )
        .line(why)
        .action(upgrade(UpgradeWord::NotToday))
        .action(upgrade(UpgradeWord::Skip))
        .hold(Hold::STANDING);
        band(vec![msg], cols).rows[0]
            .detail
            .as_ref()
            .map(|(_, d)| d.clone())
    };
    let why = "behind for 26 h: its turn is still running";
    assert_eq!(one(why, 103).as_deref(), Some("behind for 26 h"));
    assert_eq!(one(why, 102).as_deref(), Some("behind for 26 h"));
    let wider = one(why, 104).expect("the cut");
    assert!(wider.starts_with("behind for 26 h: its"), "{wider}");
    narrowing_holds(&|cols| one(why, cols));
    let two = |why: &str, cols: usize| {
        let msg = Message::new(
            tags::HARNESS,
            Severity::Warn,
            "Couldn't upgrade Claude in 2 tabs yet",
        )
        .line(why)
        .action(upgrade(UpgradeWord::Now))
        .action(upgrade(UpgradeWord::NotToday))
        .hold(Hold::STANDING);
        band(vec![msg], cols).rows[0]
            .detail
            .as_ref()
            .map(|(_, d)| d.clone())
    };
    let why = "tab 1 for 26 h \u{b7} tab 3 for 2 h";
    assert_eq!(two(why, 103).as_deref(), Some("tab 1 for 26 h"));
    narrowing_holds(&|cols| two(why, cols));
}

/// What a tab's login notice says happened (its reason, the second line).
const LOST: &str = "Claude Code's login expired, so it cannot answer prompts.";

/// A tab's notice as its host raises it: a now ask whose row leads with its
/// one step, then its reason, `Show tab 1` its button.
fn step_first(title: &str, step: &str) -> Message {
    Message::new(tags::HARNESS, Severity::Warn, title)
        .line(step)
        .line(LOST)
        .action(Intent::ShowTab { tab: 1, window: 1 })
        .now(Ask::standing(
            StandingWhy::OnlyYou(Reason::new(LOST)),
            Then::step(step),
        ))
        .key("harness.resume-ask.s-1.nr")
}

/// The same ask with no lines of its own: [`Message::now`] leads with the
/// reason, so its excerpt is no step.
fn reason_first(title: &str, step: &str) -> Message {
    Message::new(tags::HARNESS, Severity::Warn, title)
        .action(Intent::ShowTab { tab: 1, window: 1 })
        .now(Ask::standing(
            StandingWhy::OnlyYou(Reason::new(LOST)),
            Then::step(step),
        ))
        .key("harness.resume-ask.s-1.nr")
}

/// `msg` alone on the band at `cols`, once its reveal's grace is over.
fn revealed(msg: Message, cols: usize) -> RowLayout {
    revealed_with(msg, cols, &char_width)
}

/// [`revealed`], its cells measured by `width`.
fn revealed_with(msg: Message, cols: usize, width: &dyn Fn(&str) -> usize) -> RowLayout {
    let now = Instant::now();
    let mut c = MessageCenter::new(MessageLog::empty(), now);
    c.post(msg, stamp(), now);
    let later = now + PROGRESS_GRACE + Duration::from_millis(1);
    let _ = c.settle(later, true);
    c.commit_rows(later, 3);
    let mut p = c.presentation(cols, width, None, Links::Painted);
    assert_eq!(p.rows.len(), 1, "one row on the band");
    p.rows.remove(0)
}

fn excerpt_of(row: &RowLayout) -> Option<String> {
    row.detail.as_ref().map(|(_, d)| d.clone())
}

/// The cells `words` cut after one more whole word than `shown` keeps would
/// take ([`crate::glass`]'s whole-word cut): `None` where `shown` is not a
/// cut of `words` or keeps its last word already.
fn next_cut_cells(shown: &str, words: &str) -> Option<usize> {
    let stem = shown.strip_suffix('\u{2026}')?;
    let rest = words
        .strip_prefix(stem)?
        .trim_start_matches([' ', ',', ';', ':', '.', '\u{b7}']);
    let word = rest.split(' ').next()?;
    let more = &rest[word.len()..];
    let next = format!("{stem} {word}");
    Some(char_width(&next) + usize::from(!more.trim().is_empty()))
}

/// Whether `shown` is the title `words` at its floor: cut only after a whole
/// word ([`crate::glass::tests::whole_words_of`]), in at most
/// [`crate::TITLE_MIN`] cells, and one word more would not fit them.
fn at_floor(shown: &str, words: &str) -> bool {
    crate::glass::tests::whole_words_of(shown, words)
        && char_width(shown) <= crate::TITLE_MIN
        && next_cut_cells(shown, words).is_none_or(|next| next > crate::TITLE_MIN)
}

/// AN ASK'S ONE STEP WINS OVER ITS TITLE (the review of the owner's notices,
/// 2026-10-20: at 864 px the band cut `Type /login in the tab and sign in
/// with main's own…`, and at 514 px it showed the title and `Show tab 1` and
/// dropped the step). Wide, every word stands; narrower, `Details ›` goes and
/// the title elides while the step stays whole. NEGATIVE CONTROLS: the same
/// ask led by its reason keeps its whole title and drops that excerpt, and
/// the same lines on a row that asks nothing drop the step.
#[test]
fn an_asks_step_stands_whole_where_the_title_can_pay_for_it() {
    let title = "Claude's login expired in tab 1";
    let step = "Type /login in the tab and sign in again.";
    let wide = revealed(step_first(title, step), 120);
    assert_eq!(wide.title.1, title);
    assert_eq!(excerpt_of(&wide).as_deref(), Some(step));
    assert_eq!(
        wide.capsules
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>(),
        ["Show tab 1", "Details \u{203a}"]
    );
    let narrow = revealed(step_first(title, step), 70);
    assert_eq!(excerpt_of(&narrow).as_deref(), Some(step), "{narrow:?}");
    assert!(narrow.title.1.ends_with('\u{2026}'), "{narrow:?}");
    assert!(title.starts_with(narrow.title.1.trim_end_matches('\u{2026}')));
    // NEGATIVE CONTROL: led by its reason, the excerpt is an extra.
    let reason = revealed(reason_first(title, step), 70);
    assert_eq!(reason.title.1, title, "{reason:?}");
    assert_eq!(excerpt_of(&reason), None, "{reason:?}");
    // NEGATIVE CONTROL: the step's words on a row that asks for nothing.
    let plain = Message::new(tags::HARNESS, Severity::Warn, title)
        .line(step)
        .action(Intent::ShowTab { tab: 1, window: 1 })
        .hold(Hold::STANDING);
    let plain = band(vec![plain], 70).rows.remove(0);
    assert_eq!(excerpt_of(&plain), None, "{plain:?}");
}

/// A step longer than the band can hold beside the shortest title is cut, the
/// title at its floor ([`crate::TITLE_MIN`]), and never dropped while the
/// excerpt's floor fits; narrowing never shows more of it, nor brings it
/// back once it went. NEGATIVE CONTROL: one column more than the whole row
/// needs paints it whole.
#[test]
fn an_asks_step_too_long_for_any_title_is_cut_not_dropped() {
    let title = "Claude usage limit reached in tab 1";
    let step = "Run aterm harness accounts renew pool-with-a-rather-long-label";
    let narrow = revealed(step_first(title, step), 70);
    let cut = excerpt_of(&narrow).expect("the step stands");
    assert!(cut.ends_with('\u{2026}'), "{cut}");
    assert!(step.starts_with(cut.trim_end_matches('\u{2026}')), "{cut}");
    assert!(at_floor(&narrow.title.1, title), "{narrow:?}");
    narrowing_holds(&|cols| excerpt_of(&revealed(step_first(title, step), cols)));
    // Whole once the title and the capsules fit beside it.
    let whole = (70..=200)
        .find(|&cols| excerpt_of(&revealed(step_first(title, step), cols)).as_deref() == Some(step))
        .expect("a width shows it whole");
    assert!(whole > 70);
    assert_eq!(
        excerpt_of(&revealed(step_first(title, step), whole + 1)).as_deref(),
        Some(step)
    );
}

/// The band's words in the place of a command it cannot show whole (the
/// harness's `COMMAND_UNDER_DETAILS`).
const SHORT: &str = "Run the command under Details.";

/// A tab's notice led by its `step`, with its band `short` form where it has
/// one, `Show tab 1` its button.
fn step_or_short(title: &str, step: &str, short: Option<&str>) -> Message {
    let then = Then::step(step);
    let then = match short {
        Some(words) => then.or_short(words),
        None => then,
    };
    Message::new(tags::HARNESS, Severity::Warn, title)
        .line(step)
        .line(LOST)
        .action(Intent::ShowTab { tab: 1, window: 1 })
        .now(Ask::standing(StandingWhy::OnlyYou(Reason::new(LOST)), then))
        .key("harness.resume-ask.s-1.lr")
}

/// A STEP THAT MUST NOT BE CUT GIVES WAY TO ITS SHORT FORM (the review of
/// the band, 2026-10-20: at 514 px the band read `Run aterm harness accounts
/// renew…`, a command with the account it names cut off). Where the step
/// stands whole beside the title it does; where it would be cut, its short
/// form stands in its place, whole; only on the narrowest rows is the short
/// form itself cut — the step never is, at any width, even one shorter than
/// its short form. NEGATIVE CONTROL: the same step with no short form is cut
/// at 70 columns, its command and all.
#[test]
fn a_step_with_a_short_form_is_whole_or_replaced_never_cut() {
    let title = "Claude usage limit reached in tab 1";
    let long = "Run aterm harness accounts renew pool-with-a-rather-long-label";
    let short = SHORT;
    let ask = |step: &str, short: Option<&str>| step_or_short(title, step, short);
    for step in [long, "Run aterm harness accounts."] {
        let (mut whole, mut replaced) = (0, 0);
        for cols in 20..=200 {
            match excerpt_of(&revealed(ask(step, Some(short)), cols)) {
                Some(shown) if shown == step => whole += 1,
                Some(shown) if shown == short => replaced += 1,
                Some(shown) => {
                    let said = shown.trim_end_matches('\u{2026}');
                    assert!(
                        short.starts_with(said) && !said.contains("aterm"),
                        "{step} at {cols}: a cut of the step: {shown:?}"
                    );
                }
                None => {}
            }
        }
        assert!(whole > 0, "{step}: {whole} whole, {replaced} replaced");
        assert!(
            step != long || replaced > 0,
            "{step}: {whole} whole, {replaced} replaced"
        );
    }
    assert_eq!(
        excerpt_of(&revealed(ask(long, Some(short)), 70)).as_deref(),
        Some(short)
    );
    // NEGATIVE CONTROL: no short form, the command is cut.
    let cut = excerpt_of(&revealed(ask(long, None), 70)).expect("the step stands");
    assert!(
        cut.ends_with('\u{2026}') && cut.contains("aterm harness"),
        "{cut}"
    );
}

/// A STEP'S SHORT FORM GIVES BACK THE CELLS THE WHOLE STEP WOULD HAVE TAKEN
/// (the review of the band, ox4-3: with a 24-character label, at 80 and 70
/// columns the band read `Claude usag… · Run the command under Details.`,
/// then some 25 blank cells before `Tab 1`, and no `Details ›`, the capsule
/// the short form sends the reader to). Where the short form stands in the
/// step's place the head is fitted around it from the top, so `Details ›`
/// stands where it fits, the title takes the cells it can — longer than its
/// floor — and a title is cut only where the cells beside it would not hold
/// its next word; where its whole-word floor leaves the whole step room, the
/// step stands whole. NEGATIVE CONTROL: at the wide band the whole step
/// stands and the same check holds; the same step with no short form is cut
/// beside the title at its floor, as a step with no short form always is.
#[test]
fn a_steps_short_form_gives_back_the_cells_the_step_took() {
    let title = "Claude usage limit reached in tab 1";
    let step = "Run aterm harness accounts renew client-acme-research-024";
    // The blank cells between the excerpt and the first capsule, past the
    // clearance a capsule always has.
    let blank = |row: &RowLayout| {
        let (at, words) = row.detail.as_ref().expect("an excerpt");
        let end = at + char_width(words);
        let first = row.capsules.first().expect("Show tab 1").col;
        first.saturating_sub(end + crate::BEFORE_CAPSULES)
    };
    // The title's floor is the whole words that fit its cells, so beside it
    // the whole step fits sooner than beside a cut of a word: at 80 columns
    // it stands whole, the title at its floor (the step outranks the
    // title's words past the floor), and at 70 the short form stands.
    let mut replaced = 0;
    for cols in [70, 80] {
        let row = revealed(step_or_short(title, step, Some(SHORT)), cols);
        let shown = excerpt_of(&row);
        if shown.as_deref() == Some(step) {
            assert!(at_floor(&row.title.1, title), "{cols}: {row:?}");
        } else {
            assert_eq!(shown.as_deref(), Some(SHORT), "{cols}: {row:?}");
            replaced += 1;
            let details = row.capsules.iter().any(|c| c.text == "Details \u{203a}");
            assert!(
                details || char_width(&row.title.1) > crate::TITLE_MIN,
                "{cols}: the title at its floor and no Details: {row:?}"
            );
        }
        // A title cut after a whole word may leave fewer blank cells than
        // its next word would take, never as many.
        if row.title.1 != title {
            let next = next_cut_cells(&row.title.1, title).expect("a cut of the title");
            assert!(
                char_width(&row.title.1) + blank(&row) < next,
                "{cols}: a cut title beside blank cells its next word fits: {row:?}"
            );
        }
    }
    assert!(replaced > 0, "the short form stood at neither width");
    // NEGATIVE CONTROL: the whole step at the wide band.
    let wide = revealed(step_or_short(title, step, Some(SHORT)), 120);
    assert_eq!(excerpt_of(&wide).as_deref(), Some(step), "{wide:?}");
    // NEGATIVE CONTROL: no short form, the step is cut beside the title at
    // its floor.
    let cut = revealed(step_or_short(title, step, None), 70);
    assert!(at_floor(&cut.title.1, title), "{cut:?}");
    assert!(
        excerpt_of(&cut).is_some_and(|d| d.ends_with('\u{2026}')),
        "{cut:?}"
    );
}

/// A STEP CUT SHORT OF ITS FIRST ACTION SAYS IT IS CUT (the review of the
/// band, ox4-8: at 70 columns `Sign in to client-acme-research-024: clear the
/// draft, then type /login.` read `Sign in to client-acme-research-024`, a
/// complete-looking instruction to sign in, the draft and `/login` gone with
/// no mark). At every width a sentence step that is not shown whole ends in
/// `…`, its lead clause alone included, and narrowing never shows more of
/// it. NEGATIVE CONTROL: the same words as a statement (a row that asks for
/// nothing) stand as their whole lead clause with no mark at some width, as
/// a reason's lead does.
#[test]
fn a_step_cut_short_of_its_first_action_ends_in_an_ellipsis() {
    let title = "Claude's login expired in tab 1";
    let lead = "Sign in to client-acme-research-024";
    let step = "Sign in to client-acme-research-024: clear the draft, then type /login.";
    let mut lead_alone = 0;
    for cols in 20..=200 {
        let Some(shown) = excerpt_of(&revealed(step_first(title, step), cols)) else {
            continue;
        };
        if shown == step {
            continue;
        }
        assert!(
            shown.ends_with('\u{2026}'),
            "{cols}: a cut with no mark: {shown:?}"
        );
        let said = shown.trim_end_matches('\u{2026}').trim_end_matches(':');
        assert!(step.starts_with(said), "{cols}: {shown:?}");
        if said == lead {
            lead_alone += 1;
        }
    }
    assert!(
        lead_alone > 0,
        "premise: some width shows the lead clause alone"
    );
    narrowing_holds(&|cols| excerpt_of(&revealed(step_first(title, step), cols)));
    // NEGATIVE CONTROL: as a statement, the lead clause stands bare.
    let statement = |cols: usize| {
        let msg = Message::new(tags::HARNESS, Severity::Warn, title)
            .line(step)
            .action(upgrade(UpgradeWord::NotToday))
            .action(upgrade(UpgradeWord::Skip))
            .hold(Hold::STANDING);
        band(vec![msg], cols).rows[0]
            .detail
            .as_ref()
            .map(|(_, d)| d.clone())
    };
    assert!(
        (20..=200).any(|cols| statement(cols).as_deref() == Some(lead)),
        "the control: a statement's lead clause stands bare"
    );
}

/// What the title of `row` reads as, against its whole `title` and its
/// `short` form: 2 whole, 1 the short form whole, 0 the short form cut.
/// `None`: neither (a cut of the whole title, or other words).
fn title_tier(row: &RowLayout, title: &str, short: &str) -> Option<u8> {
    let shown = row.title.1.as_str();
    let said = shown.trim_end_matches('\u{2026}');
    if shown == title {
        Some(2)
    } else if shown == short {
        Some(1)
    } else if shown.ends_with('\u{2026}') && !said.is_empty() && short.starts_with(said) {
        Some(0)
    } else {
        None
    }
}

/// `step_or_short`'s ask with its title's short form.
fn titled(title: &str, short_title: &str, step: &str, short: Option<&str>) -> Message {
    step_or_short(title, step, short).short_title(short_title)
}

/// A TITLE READS WHOLE, AS ITS SHORT FORM WHOLE, OR AS ITS SHORT FORM CUT
/// (the review of the band, 2026-10-20: at the default window the band read
/// `Claude's lo…` and `Claude usag…`, stubs that said nothing of what
/// happened). At every width from 20 to 200 columns, for a step with and
/// without its own short form, beside a short label and a 24-character one,
/// the row's title is one of the three; as the row narrows the form never
/// widens again, save at two widths: where the step gives way to its short
/// form, and where the step goes, both of which fit the head anew (the short
/// title may stand whole again there, the whole title never); where the step
/// goes, the cells it took are the head's again, `Details ›` among them
/// where it fits beside the capsules' long words (a re-elided title left
/// them blank beside `Tab 1`: `Login expired`, 16 blank cells, no step and
/// no way to it at 47 columns); the step keeps its
/// place, cut only beside a title at its floor; the whole title stands at
/// the wide band and the short form at the default one, whole beside the
/// short label's step; the spoken title is the whole title at every width.
/// NEGATIVE CONTROL: the same row with no short title is a cut of its whole
/// title at 70 columns.
#[test]
fn a_title_reads_whole_short_or_short_cut_never_as_a_stub_of_itself() {
    let title = "Claude's login expired in tab 1";
    let short = "Login expired";
    for label in ["main", "client-acme-research-024"] {
        let commands = [
            (
                format!("Type /login in the tab and sign in as {label}."),
                None,
            ),
            (
                format!("Run aterm harness accounts renew {label}"),
                Some(SHORT),
            ),
        ];
        for (step, step_short) in &commands {
            let row = |cols| revealed(titled(title, short, step, *step_short), cols);
            let (mut wholes, mut shorts, mut drops) = (0, 0, 0);
            let mut prev = None::<(u8, bool, bool)>;
            for cols in (20..=200).rev() {
                let r = row(cols);
                let tier = title_tier(&r, title, short)
                    .unwrap_or_else(|| panic!("{label} {cols}: a stub of the title: {r:?}"));
                assert_eq!(r.full_title, title, "{cols}: the spoken title");
                let shown = excerpt_of(&r);
                if shown.as_ref().is_some_and(|d| d != step && d != SHORT) {
                    assert!(
                        at_floor(&r.title.1, title) || at_floor(&r.title.1, short),
                        "{label} {cols}: the step cut beside a title above its floor: {r:?}"
                    );
                }
                wholes += usize::from(tier == 2);
                shorts += usize::from(tier == 1);
                // The widths where the step gives way to its short form, and
                // where it goes, fit the head anew: the short title may stand
                // whole again there, the whole title never.
                let replaced = shown.as_deref() == Some(SHORT);
                let stands = r.detail.is_some();
                let dropped = !stands && prev.is_some_and(|(_, _, stood)| stood);
                assert!(
                    prev.is_none_or(
                        |(p, was, _)| tier <= p || ((replaced && !was) || dropped) && tier < 2
                    ),
                    "{label} {cols}: the title widened as the row narrowed: {r:?}"
                );
                if dropped {
                    drops += 1;
                    // The head fitted anew: `Details ›` where it fits beside
                    // the capsules' long words (it goes before they shorten).
                    let details = r.capsules.iter().any(|c| c.text == "Details \u{203a}");
                    let long = r.capsules.iter().all(|c| c.text == c.full_label);
                    let end = r.title.0 + char_width(&r.title.1);
                    let blank = r.capsules[0]
                        .col
                        .saturating_sub(end + crate::BEFORE_CAPSULES);
                    let link = char_width("Details \u{203a}") + 2 + crate::CAPSULE_GAP;
                    assert!(
                        details || long && blank < link,
                        "{label} {cols}: the step's cells left blank, no way to it: {r:?}"
                    );
                }
                prev = Some((tier, replaced, stands));
            }
            assert!(
                wholes > 0 && shorts > 0,
                "{label}: {wholes} whole, {shorts} short"
            );
            assert_eq!(drops, 1, "{label}: the step goes once");
            assert_eq!(row(200).title.1, title, "{label}: the wide band");
            let default = row(80);
            assert_eq!(
                title_tier(&default, title, short).map(|t| t < 2),
                Some(true)
            );
            assert!(excerpt_of(&default).is_some(), "{label} at 80: {default:?}");
            if label == "main" {
                assert_eq!(default.title.1, short, "{label} at 80: {default:?}");
            }
        }
    }
    // NEGATIVE CONTROL: no short title, the whole title is cut.
    let step = "Type /login in the tab and sign in as main.";
    let plain = revealed(step_or_short(title, step, None), 70);
    let said = plain.title.1.trim_end_matches('\u{2026}');
    assert!(
        plain.title.1.ends_with('\u{2026}') && title.starts_with(said),
        "{plain:?}"
    );
    assert_eq!(title_tier(&plain, title, short), None, "{plain:?}");
}

/// BESIDE A STEP'S SHORT FORM THE SHORT TITLE BUYS `Details ›` (the review
/// of the band, ox4-3, at the default window with a 24-character label):
/// `Run the command under Details.` sends the reader to a capsule, so the
/// short title stands in the whole one's place before that capsule goes,
/// and at 70 and 80 columns `Show tab 1` keeps its long words. NEGATIVE CONTROL: the same row
/// with no short title spends those cells on the title's words and drops
/// `Details ›`.
#[test]
fn beside_a_steps_short_form_the_short_title_keeps_details() {
    let title = "Claude usage limit reached in tab 1";
    let step = "Run aterm harness accounts renew client-acme-research-024";
    let caps = |row: &RowLayout| {
        row.capsules
            .iter()
            .map(|c| c.text.clone())
            .collect::<Vec<_>>()
    };
    for cols in [70, 80] {
        let row = revealed(titled(title, "Usage limit", step, Some(SHORT)), cols);
        assert_eq!(row.title.1, "Usage limit", "{cols}: {row:?}");
        assert_eq!(excerpt_of(&row).as_deref(), Some(SHORT), "{cols}: {row:?}");
        assert_eq!(caps(&row)[0], "Show tab 1", "{cols}: {row:?}");
    }
    let default = revealed(titled(title, "Usage limit", step, Some(SHORT)), 80);
    assert_eq!(
        caps(&default),
        ["Show tab 1", "Details \u{203a}"],
        "{default:?}"
    );
    // NEGATIVE CONTROL: no short title, the whole title's words take the
    // cells and the link goes.
    let plain = revealed(step_or_short(title, step, Some(SHORT)), 80);
    assert!(
        !caps(&plain).iter().any(|c| c == "Details \u{203a}"),
        "{plain:?}"
    );
}

/// A short title wider than the whole one, or one on a row that has room for
/// the whole title, never shows: the whole title is what a row reads where
/// it fits. NEGATIVE CONTROL: the same short title where the whole one does
/// not fit stands in its place.
#[test]
fn a_short_title_stands_only_where_the_whole_one_does_not_fit() {
    let step = "Type /login in the tab and sign in again.";
    let wide = revealed(
        titled("Claude's login expired", "Login expired", step, None),
        200,
    );
    assert_eq!(wide.title.1, "Claude's login expired", "{wide:?}");
    let longer = revealed(
        titled("Login gone", "Claude's login expired", step, None),
        30,
    );
    assert!(
        "Login gone".starts_with(longer.title.1.trim_end_matches('\u{2026}')),
        "{longer:?}"
    );
    // NEGATIVE CONTROL: where the whole title does not fit, the short one.
    let narrow = revealed(
        titled("Claude's login expired", "Login expired", step, None),
        70,
    );
    assert_eq!(narrow.title.1, "Login expired", "{narrow:?}");
}

/// A SHORT TITLE GIVES BACK THE CELLS THE WHOLE TITLE TOOK (the review of
/// the band, 2026-10-20: at the default window `Login expired · Finish
/// signing in in your browser.` stood beside 17 blank cells and `Tab 1`):
/// where the whole title gives way, the head is fitted again around the
/// short one from the top, so `Show tab 1` keeps its words and `Details ›`
/// stands where it fits, as beside a step's short form (ox4-3). NEGATIVE
/// CONTROL: the same row with no short title spends those cells on its
/// title's words, and its capsule is short.
#[test]
fn a_short_title_gives_back_the_cells_the_whole_title_took() {
    let title = "Claude's login expired in tab 1";
    // The row the review imaged, at the default window: a step too long for
    // the whole title beside it.
    let step = "Finish signing in in your browser.";
    let cols = 80;
    let caps = |row: &RowLayout| {
        row.capsules
            .iter()
            .map(|c| c.text.clone())
            .collect::<Vec<_>>()
    };
    let row = revealed(titled(title, "Login expired", step, None), cols);
    assert_eq!(row.title.1, "Login expired", "{row:?}");
    assert_eq!(excerpt_of(&row).as_deref(), Some(step), "{row:?}");
    assert_eq!(caps(&row), ["Show tab 1", "Details \u{203a}"], "{row:?}");
    // NEGATIVE CONTROL: no short title.
    let plain = revealed(step_or_short(title, step, None), cols);
    assert!(plain.title.1.ends_with('\u{2026}'), "{plain:?}");
    assert_eq!(caps(&plain), ["Tab 1"], "{plain:?}");
}

/// A TITLE IS CUT ONLY AFTER A WHOLE WORD (the review of the band over
/// Settings: at 22 columns `Keybinding skipped` read `⚠ Keybindin…` beside
/// `Edit`, a word cut mid-way and nothing to say the rest was a press away).
/// For every row the band lays out — the width law's fixtures and motion
/// fixtures, the config families' titles beside `Open aterm.toml`, a tab's
/// notice led by its step, a title with a short form — at every width from
/// 22 to 42 columns (the band's columns at 1× and at 2× text alike), under
/// the char measure and a two-cell one, the title reads whole, as its words
/// (or its short form's) cut after a whole word with `…`, or not at all; the
/// spoken title is whole. At 22 columns the keybinding row keeps
/// `Keybinding…` beside `Edit`. NEGATIVE CONTROL: the cut the band painted,
/// `Keybindin…`, fails the same check.
#[test]
fn a_title_is_cut_only_after_a_whole_word() {
    use crate::glass::tests::{fixtures, motion_fixtures, whole_words_of};
    let two_cell = |s: &str| -> usize {
        s.chars()
            .map(|c| if c.is_ascii_uppercase() { 2 } else { 1 })
            .sum()
    };
    let honest = |shown: &str, title: &str, short: Option<&str>| {
        shown.is_empty()
            || whole_words_of(shown, title)
            || short.is_some_and(|short| whole_words_of(shown, short))
    };
    assert!(!honest("Keybindin\u{2026}", "Keybinding skipped", None));
    for (name, spec) in fixtures().into_iter().chain(motion_fixtures()) {
        for cols in 22..=42 {
            for width in [&char_width as &dyn Fn(&str) -> usize, &two_cell] {
                let row = layout_row(&spec, cols, width);
                assert!(
                    honest(&row.title.1, spec.title, spec.title_short),
                    "{name}@{cols}: {:?}",
                    row.title.1
                );
            }
        }
    }
    let config = |title: &str| {
        Message::new(tags::CONFIG, Severity::Warn, title)
            .line("skipping \"cmd+shift+k\": unknown action")
            .action(Intent::OpenConfigEditor { line: None })
    };
    let mut rows: Vec<(Message, &str, Option<&str>)> = [
        "Keybinding skipped",
        "3 keybindings skipped",
        "Unknown setting",
        "Misspelled setting",
        "Retired setting",
        "Couldn't use a setting's value",
        "Couldn't apply the cursor trail",
        "3 errors in cursor trail settings",
        "Couldn't apply the font",
        "Couldn't load 2 images",
        "A setting applies after restart",
        "Couldn't change Secure Keyboard Entry",
    ]
    .into_iter()
    .map(|title| (config(title), title, None))
    .collect();
    let notice = "Claude usage limit reached in tab 1";
    rows.push((
        step_first(notice, "Run aterm harness accounts renew main"),
        notice,
        None,
    ));
    rows.push((
        step_first(notice, "Run aterm harness accounts renew main").short_title("Usage limit"),
        notice,
        Some("Usage limit"),
    ));
    for (msg, title, short) in rows {
        for cols in 22..=42 {
            let row = revealed(msg.clone(), cols);
            assert!(
                honest(&row.title.1, title, short),
                "{title}@{cols}: {:?}",
                row.title.1
            );
            assert_eq!(row.full_title, title, "{title}@{cols}: spoken whole");
        }
    }
    let narrow = revealed(config("Keybinding skipped"), 22);
    assert_eq!(narrow.title.1, "Keybinding\u{2026}", "{narrow:?}");
    assert_eq!(
        narrow
            .capsules
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>(),
        ["Edit"],
        "{narrow:?}"
    );
}

/// AN ALL-CJK TITLE STANDS ON THE BAND AT EVERY NARROW WIDTH: a config
/// row and a notice whose titles have no space (Japanese, Chinese) or few
/// (Korean), at every width from 22 to 42 columns (the band's columns at 1×
/// and 2× text), measured in cells (a CJK character is two) and by chars:
/// the title is whole or cut between its characters with `…`, never empty,
/// never cut before a closer, and spoken whole. NEGATIVE CONTROL: cut only
/// at spaces, the Japanese and Chinese titles were left out of the row at
/// every width here.
#[test]
fn an_all_cjk_title_stands_on_the_band_at_every_narrow_width() {
    use crate::glass::tests::whole_words_of;
    let cells = |s: &str| aterm_grapheme::str_width(s);
    let titles = [
        "設定ファイルを読み込めませんでした",
        "无法保存设置文件，请检查权限",
        "설정 파일을 저장할 수 없습니다",
        "「テーマ」が見つかりません。",
    ];
    for title in titles {
        let config = Message::new(tags::CONFIG, Severity::Warn, title)
            .line("skipping \"cmd+shift+k\": unknown action")
            .action(Intent::OpenConfigEditor { line: None });
        for msg in [
            config,
            step_first(title, "Run aterm harness accounts renew main"),
        ] {
            for cols in 22..=42 {
                for width in [&char_width as &dyn Fn(&str) -> usize, &cells] {
                    let row = revealed_with(msg.clone(), cols, width);
                    let shown = &row.title.1;
                    assert!(!shown.is_empty(), "{title}@{cols}: left out ({row:?})");
                    assert!(whole_words_of(shown, title), "{title}@{cols}: {shown:?}");
                    // A stop the cut left trailing went with it.
                    let stem = shown.trim_end_matches('\u{2026}');
                    let next = title[stem.len()..]
                        .trim_start_matches(['\u{3001}', '\u{3002}', '\u{ff0c}'])
                        .chars()
                        .next();
                    assert!(
                        !matches!(next, Some('」' | '。' | '，')),
                        "{title}@{cols}: {shown:?} leaves {next:?} to start the rest"
                    );
                    assert_eq!(row.full_title, title, "{title}@{cols}: spoken whole");
                }
            }
        }
    }
}
