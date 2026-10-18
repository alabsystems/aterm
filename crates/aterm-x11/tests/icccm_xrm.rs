// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm-x11's ICCCM properties and resource database against x11rb 0.13.2
//! and x11rb-protocol 0.13.2, recorded in tests/recorded/: the property bytes
//! x11rb wrote and what it read from bytes it did not write, and the answer
//! x11rb-protocol's Database gave to each query of each database.
//!
//! Negative controls: every recording has a case; a hint one field away
//! encodes differently; a database with its precedence-deciding entry
//! dropped answers differently.

#![cfg(target_endian = "little")]

mod support;

use std::collections::BTreeMap;

use aterm_x11::icccm::{InitialState, Specified, WmClass, WmHints, WmSizeHints};
use aterm_x11::proto::xproto::GetPropertyReply;
use aterm_x11::xrm::Database;
use support::*;

const ICCCM: &str = "x11rb-0.13.2-icccm.tsv";
const XRM: &str = "x11rb-protocol-0.13.2-xrm.tsv";

fn full_size_hints() -> WmSizeHints {
    WmSizeHints {
        position: Some((Specified::Program, 10, -20)),
        size: Some((Specified::User, 800, 600)),
        min_size: Some((100, 50)),
        max_size: Some((4000, 3000)),
        size_increment: Some((9, 18)),
        aspect: Some(((1, 2), (3, 1))),
        base_size: Some((20, 40)),
        win_gravity: Some(10),
    }
}

fn full_hints() -> WmHints {
    WmHints {
        input: Some(true),
        initial_state: Some(InitialState::Iconic),
        icon_pixmap: Some(0x0060_0010),
        icon_window: None,
        icon_position: Some((-4, 8)),
        icon_mask: None,
        window_group: Some(0x0060_0001),
        urgent: true,
    }
}

fn render_size_hints(h: &WmSizeHints) -> String {
    let spec = |s: Specified| match s {
        Specified::User => "user",
        Specified::Program => "program",
    };
    let pair = |p: Option<(i32, i32)>| p.map_or("-".to_owned(), |(a, b)| format!("{a},{b}"));
    format!(
        "position={} size={} min={} max={} inc={} aspect={} base={} gravity={}",
        h.position
            .map_or("-".to_owned(), |(s, x, y)| format!("{}:{x},{y}", spec(s))),
        h.size
            .map_or("-".to_owned(), |(s, x, y)| format!("{}:{x},{y}", spec(s))),
        pair(h.min_size),
        pair(h.max_size),
        pair(h.size_increment),
        h.aspect
            .map_or("-".to_owned(), |((an, ad), (bn, bd))| format!(
                "{an},{ad}/{bn},{bd}"
            )),
        pair(h.base_size),
        h.win_gravity.map_or("-".to_owned(), |g| g.to_string()),
    )
}

fn render_hints(h: &WmHints) -> String {
    let opt = |v: Option<u32>| v.map_or("-".to_owned(), |v| v.to_string());
    format!(
        "input={} state={} icon_pixmap={} icon_window={} icon_position={} icon_mask={} group={} urgent={}",
        h.input.map_or("-".to_owned(), |v| b(v).to_string()),
        h.initial_state.map_or("-", |s| match s {
            InitialState::Normal => "normal",
            InitialState::Iconic => "iconic",
        }),
        opt(h.icon_pixmap),
        opt(h.icon_window),
        h.icon_position
            .map_or("-".to_owned(), |(x, y)| format!("{x},{y}")),
        opt(h.icon_mask),
        opt(h.window_group),
        b(h.urgent)
    )
}

fn class_reply(value: &[u8]) -> GetPropertyReply {
    GetPropertyReply {
        format: 8,
        type_: 31,
        bytes_after: 0,
        value_len: u32::try_from(value.len()).unwrap(),
        value: value.to_vec(),
    }
}

#[test]
fn icccm_properties_are_x11rbs() {
    let rec = by_case(ICCCM);
    assert!(rec.len() >= 14, "the corpus is whole: {}", rec.len());
    let sparse = WmSizeHints {
        min_size: Some((200, 100)),
        size_increment: Some((7, 15)),
        base_size: Some((4, 6)),
        ..WmSizeHints::default()
    };
    let minimal = WmHints {
        input: Some(false),
        ..WmHints::default()
    };
    // Written by x11rb: the bytes, and the parse of them.
    let written: [(&str, Vec<u8>); 5] = [
        ("size_hints_full", full_size_hints().to_bytes()),
        ("size_hints_sparse", sparse.to_bytes()),
        ("size_hints_empty", WmSizeHints::default().to_bytes()),
        ("hints_full", full_hints().to_bytes()),
        ("hints_input", minimal.to_bytes()),
    ];
    let mut got = BTreeMap::new();
    let mut want = BTreeMap::new();
    for (case, bytes) in written {
        let row = &rec[case];
        assert_eq!(hex(&bytes), row[0], "{case}: the bytes x11rb wrote");
        want.insert(case.to_owned(), row[1].clone());
        let parsed = if case.starts_with("size") {
            WmSizeHints::parse(&bytes).map(|h| render_size_hints(&h))
        } else {
            WmHints::parse(&bytes).map(|h| render_hints(&h))
        };
        got.insert(case.to_owned(), parsed.unwrap_or_else(|_| "ERR".into()));
    }
    // Read by x11rb from bytes it did not write.
    for case in [
        "size_hints_legacy",
        "size_hints_truncated",
        "hints_legacy",
        "hints_bad_state",
    ] {
        let row = &rec[case];
        let bytes = unhex(&row[0]);
        want.insert(case.to_owned(), row[1].clone());
        let parsed = if case.starts_with("size") {
            WmSizeHints::parse(&bytes).map(|h| render_size_hints(&h))
        } else {
            WmHints::parse(&bytes).map(|h| render_hints(&h))
        };
        got.insert(case.to_owned(), parsed.unwrap_or_else(|_| "ERR".into()));
    }
    for case in [
        "class_both",
        "class_unterminated",
        "class_instance_only",
        "class_empty",
        "class_double_nul",
    ] {
        let row = &rec[case];
        want.insert(case.to_owned(), row[1].clone());
        let c = WmClass::from_reply(&class_reply(&unhex(&row[0])))
            .expect("STRING/8")
            .expect("present");
        got.insert(
            case.to_owned(),
            format!("instance={} class={}", hex(&c.instance), hex(&c.class)),
        );
    }
    let want_all: BTreeMap<_, _> = rec.iter().map(|(c, r)| (c.clone(), r[1].clone())).collect();
    same_cases(ICCCM, &want_all, &got);
    assert_eq!(want, got);
}

#[test]
fn a_hint_one_field_away_encodes_differently() {
    let rec = by_case(ICCCM);
    let mut h = full_size_hints();
    h.win_gravity = None;
    assert_ne!(hex(&h.to_bytes()), rec["size_hints_full"][0]);
    let mut h = full_hints();
    h.urgent = false;
    assert_ne!(hex(&h.to_bytes()), rec["hints_full"][0]);
}

#[test]
fn every_query_answers_as_x11rb_protocol_answered() {
    let rows = rows(XRM);
    let mut dbs = BTreeMap::new();
    let mut want = BTreeMap::new();
    for row in &rows {
        match row[0].as_str() {
            "db" => {
                dbs.insert(row[1].clone(), unhex(&row[2]));
            }
            "q" => {
                want.insert(format!("{} {}|{}", row[1], row[2], row[3]), row[4].clone());
            }
            other => panic!("{XRM}: row kind {other}"),
        }
    }
    assert!(dbs.len() >= 7 && want.len() >= 175, "the corpus is whole");
    let mut got = BTreeMap::new();
    for row in rows.iter().filter(|r| r[0] == "q") {
        let db = Database::parse(&dbs[&row[1]]);
        let answer = db
            .get(&row[2], &row[3])
            .map_or("-".to_owned(), |v| format!("={}", hex(v)));
        got.insert(format!("{} {}|{}", row[1], row[2], row[3]), answer);
    }
    same_cases(XRM, &want, &got);
    assert!(
        want.values().filter(|a| *a != "-").count() >= 25,
        "most queries find something"
    );
}

#[test]
fn dropping_the_deciding_entry_changes_the_answer() {
    let rows = rows(XRM);
    let data = rows
        .iter()
        .find(|r| r[0] == "db" && r[1] == "precedence")
        .map(|r| unhex(&r[2]))
        .expect("the precedence database");
    let full = Database::parse(&data);
    let text = String::from_utf8(data).unwrap();
    let without: String = text
        .lines()
        .filter(|l| !l.starts_with("Xft.dpi: 8"))
        .map(|l| format!("{l}\n"))
        .collect();
    let cut = Database::parse(without.as_bytes());
    assert_eq!(full.get_string("Xft.dpi", ""), Some("8"));
    assert_eq!(
        cut.get_string("Xft.dpi", ""),
        Some("3"),
        "the earlier tight entry decides once the later one is gone"
    );
}
