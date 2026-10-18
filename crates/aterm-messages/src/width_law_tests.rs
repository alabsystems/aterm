// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test-only: the width law. One intent reads one way across the band at
//! every width, whatever each row's title length.

use crate::center::MessageCenter;
use crate::glass::{
    CapsuleRole, Links, Presentation, RowKind, RowSpec, layout_row, layout_row_with,
};
use crate::log::MessageLog;
use crate::model::{
    Hold, Intent, Loads, Message, MessageId, Severity, UpgradeWord, WallStamp, tags,
};
use crate::text::char_width;
use crate::{ETA_SHORT_W, Instant};

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
        detail0: Some("from github.com over the office network, resumed where it stopped"),
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
