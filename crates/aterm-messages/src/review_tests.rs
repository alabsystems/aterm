// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test-only: junk-tolerance and flood nets from the 2026-09-22 adversarial
//! review — a deterministic PRNG drives the codec, every small decoder, both
//! wire parsers, the text shapers and the words at random and at extremes,
//! and a 6000-step flood asserts every bound after every step. The
//! defect-pinning tests from the same review live beside their code.

use crate::center::{MessageCenter, Outcome};
use crate::glass::Links;
use crate::glass::tests::{chars, render};
use crate::log::tests::as_decoded;
use crate::log::{LogLine, LogRecord, LogState, MessageLog, Retired};
use crate::model::{
    ActionIndex, Glyph, Hold, Intent, Message, MessageId, Meter, Origin, Restatement, Severity,
    Tag, WallStamp, every_intent, tags,
};
use crate::text::{sanitize, shape_detail, shape_path, trim_words, wrap};
use crate::wire::{NoticeRequest, PostRequest, WireGate, apply, parse_read_args};
use crate::words::{abbreviate_paths_in, date_words, home_abbreviate, relative_words, stamp_words};
use crate::{
    DETAIL_LINE_CAP, DETAIL_LINES_CAP, Duration, Instant, LOG_CAP, MAX_ACTIONS, MAX_LIVE, MAX_ROWS,
    PENDING_PERSIST_CAP, TITLE_CAP,
};

/// xorshift64 — deterministic, no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.next() % 100 < pct
    }
}

/// Letters, joints, paths, escapes, controls, US, wide and zero-width
/// characters, the codec's own separators — everything a hostile reporter
/// or a corrupt file could carry.
const ALPHABET: &[&str] = &[
    "a",
    "b",
    "c",
    "x",
    " ",
    " ",
    " ",
    "`",
    " \u{00b7} ",
    "/",
    "~/",
    "\\",
    "\t",
    "\n",
    "\u{1f}",
    "\u{1b}[31m",
    "=",
    "%",
    "\u{1f680}",
    "\u{301}",
    "\u{202e}",
    "\u{200b}",
    "\u{6f22}",
    "\u{2026}",
    ". ",
    "Run `x`",
    "-- ",
    "\r",
];

fn rand_str(rng: &mut Rng, max: usize) -> String {
    let n = rng.below(max + 1);
    (0..n)
        .map(|_| ALPHABET[rng.below(ALPHABET.len())])
        .collect()
}

fn rand_bytes_str(rng: &mut Rng, max: usize) -> String {
    let n = rng.below(max + 1);
    let bytes: Vec<u8> = (0..n).map(|_| (rng.next() & 0xff) as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn t0() -> Instant {
    Instant::now()
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn stamp(unix_ms: u64) -> WallStamp {
    WallStamp { unix_ms }
}

fn fresh(now: Instant) -> MessageCenter {
    MessageCenter::new(MessageLog::empty(), now)
}

fn glass_ids(c: &MessageCenter) -> Vec<MessageId> {
    c.on_glass().map(|l| l.id).collect()
}

/// The codec's documented shape on the way back: every word clipped to the
/// model's caps, empty detail lines dropped — the identity for anything the
/// engine itself encodes.
fn lossy(line: &LogLine) -> LogLine {
    as_decoded(line)
}

/// Random bytes and every prefix of every valid line decode or error and
/// never panic; whatever decodes re-encodes to a line that decodes to the
/// same value; a replay of the soup keeps the ring bounded and compacts to
/// the identity.
#[test]
fn codec_survives_random_bytes_and_truncations() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for _ in 0..1500 {
        let s = rand_bytes_str(&mut rng, 300);
        if let Ok(line) = LogLine::decode(&s) {
            assert_eq!(LogLine::decode(&line.encode()).unwrap(), line);
        }
        let s = format!(
            "m1\tkind={}\t{}",
            ["posted", "retired", "acted", "dropped"][rng.below(4)],
            rand_str(&mut rng, 200)
        );
        let _ = LogLine::decode(&s);
    }
    let mut valid = Vec::new();
    for i in 1..40u64 {
        let rec = LogRecord {
            id: MessageId::from_raw(i).unwrap(),
            stamp: stamp(i * 1000),
            tag: tags::ALL[rng.below(tags::ALL.len())].clone(),
            severity: Severity::ALL[rng.below(4)],
            glyph: Glyph::or_fallback(Glyph::ALLOWED[rng.below(Glyph::ALLOWED.len())]),
            title: rand_str(&mut rng, 60),
            detail: (0..rng.below(4)).map(|_| rand_str(&mut rng, 40)).collect(),
            actions: (0..rng.below(3))
                .map(|_| every_intent()[rng.below(every_intent().len())].clone())
                .filter(|i| *i != Intent::Details)
                .collect(),
            key: rng
                .chance(50)
                .then(|| rand_str(&mut rng, 20))
                .filter(|k| !k.is_empty()),
            origin: Origin::Host,
            repeats: 1,
            state: LogState::Posted,
            retired_unix_ms: None,
            retired_at: None,
            last_action: None,
        };
        valid.push(LogLine::Posted(rec));
        valid.push(LogLine::Retired {
            id: MessageId::from_raw(i).unwrap(),
            how: Retired::Answered {
                label: rand_str(&mut rng, 20),
            },
            unix_ms: i,
            title: rand_str(&mut rng, 30),
            detail: vec![rand_str(&mut rng, 30)],
            repeats: 3,
        });
        valid.push(LogLine::Acted {
            id: MessageId::from_raw(i).unwrap(),
            unix_ms: i,
            label: rand_str(&mut rng, 20),
        });
    }
    for line in &valid {
        let enc = line.encode();
        assert_eq!(LogLine::decode(&enc), Ok(lossy(line)), "{enc:?}");
        for cut in 0..enc.len() {
            if !enc.is_char_boundary(cut) {
                continue;
            }
            if let Ok(back) = LogLine::decode(&enc[..cut]) {
                assert_eq!(LogLine::decode(&back.encode()).unwrap(), back);
            }
        }
    }
    let mut log = MessageLog::empty();
    for _ in 0..3 {
        for line in &valid {
            log.replay(line.clone());
        }
    }
    for _ in 0..(2 * LOG_CAP) {
        let s = rand_bytes_str(&mut rng, 120);
        if let Ok(line) = LogLine::decode(&s) {
            log.replay(line);
        }
    }
    assert!(log.len() <= LOG_CAP);
    let compact = log.compact_lines();
    let mut again = MessageLog::empty();
    for line in compact {
        again.replay(line);
    }
    assert_eq!(
        again.records().collect::<Vec<_>>(),
        log.records().collect::<Vec<_>>()
    );
}

/// Every small decoder and both wire parsers survive junk; what they admit
/// is within its cap and control-free.
#[test]
fn decoders_and_wire_parsers_never_panic_on_junk() {
    let mut rng = Rng(0xdead_beef_cafe_f00d);
    for _ in 0..2000 {
        let s = if rng.chance(50) {
            rand_str(&mut rng, 40)
        } else {
            rand_bytes_str(&mut rng, 40)
        };
        if let Some(i) = Intent::decode(&s) {
            assert_eq!(Intent::decode(&i.encode()), Some(i));
        }
        if let Some(h) = Hold::decode(&s) {
            assert_eq!(Hold::decode(&h.encode()), Some(h));
        }
        if let Some(r) = Retired::decode(&s) {
            assert_eq!(Retired::decode(&r.encode()), Some(r));
        }
        let _ = Tag::try_new(&s);
        let _ = Severity::parse(&s);
        if let Ok(q) = parse_read_args(&s) {
            assert!((1..=LOG_CAP).contains(&q.page()));
        }
        let prefixed = format!(
            "{} {}",
            ["system", "Bad", "", "sev=warn", "crash"][rng.below(5)],
            s
        );
        if let Ok(req) = PostRequest::parse(&prefixed) {
            let msg = req.into_message();
            assert!(msg.title.chars().count() <= TITLE_CAP && !msg.title.is_empty());
            assert!(msg.detail.len() <= DETAIL_LINES_CAP);
            assert!(msg.actions.is_empty());
            assert!(msg.title.chars().all(|c| !c.is_control()));
        }
    }
}

/// `notice` survives junk (design rulings 163–179): random lines after
/// every sub-form never panic the parser; whatever it accepts applies to a
/// center with a one-line reply, and every row the wire raised carries no
/// capsule, a control-free title, a glass title in form and an attention
/// verdict (a record for news; a row only for a failure or for work with its
/// indicator) — whatever the order, the instants and the budgets did.
#[test]
fn notice_lines_never_panic_and_never_forge_a_row() {
    let mut rng = Rng(0x5eed_0f_a11_0e5);
    let now = t0();
    let mut c = fresh(now);
    let mut gate = WireGate::default();
    let forms = [
        "post system ",
        "post system sev=warn key=k ",
        "post fabric sev=error ",
        "progress k ",
        "progress k pct=4",
        "progress k2 done=3/9 unit=bytes load=disk ",
        "done k ",
        "done k warn ",
        "dismiss ",
        "act 1 ",
        "",
        "notice ",
    ];
    let mut t = now;
    let mut accepted = 0;
    for _ in 0..4000 {
        let junk = if rng.chance(50) {
            rand_str(&mut rng, 40)
        } else {
            rand_bytes_str(&mut rng, 40)
        };
        let line = format!("{}{junk}", forms[rng.below(forms.len())]);
        t += ms(rng.below(400) as u64);
        let Ok(req) = NoticeRequest::parse(&line) else {
            continue;
        };
        accepted += 1;
        let applied = apply(&mut c, &mut gate, req, stamp(1), t);
        assert!(!applied.reply.contains('\n'), "{line:?}: {}", applied.reply);
        assert!(
            applied.reply.starts_with("OK ") || applied.reply.starts_with("ERR "),
            "{}",
            applied.reply
        );
        for l in c.live_rows() {
            assert!(l.msg.actions.is_empty());
            assert!(l.msg.title.chars().all(|ch| !ch.is_control()));
            assert!(!l.msg.title.is_empty());
            assert_eq!(
                crate::text::glass_title_fault(&l.msg.title),
                None,
                "{:?}",
                l.msg.title
            );
            let progress = matches!(l.msg.hold, Hold::Live { .. })
                && l.msg
                    .meter
                    .as_ref()
                    .is_some_and(|m| m.busy || m.fill_permille.is_some());
            assert!(progress || l.msg.severity >= Severity::Warn, "{:?}", l.msg);
            assert!(
                l.msg
                    .key
                    .as_deref()
                    .is_none_or(|k| k.starts_with(crate::WIRE_KEY_PREFIX))
            );
        }
        assert!(c.live_rows().count() <= crate::WIRE_LIVE_CAP);
        c.settle(t, true);
    }
    assert!(accepted > 200, "the net must reach apply: {accepted}");
}

/// The shapers never panic and never exceed their cap, whatever the text.
#[test]
fn text_shapers_never_panic_and_respect_caps() {
    let mut rng = Rng(0x0bad_5eed_0bad_5eed);
    for _ in 0..800 {
        let s = if rng.chance(70) {
            rand_str(&mut rng, 120)
        } else {
            rand_bytes_str(&mut rng, 120)
        };
        for cap in [0usize, 1, 2, 3, 5, 12, 20, 23, 40, 80, 300] {
            let d = shape_detail(&s, cap);
            assert!(
                d.chars().count() <= cap,
                "shape_detail({s:?}, {cap}) = {d:?}"
            );
            assert!(d.chars().all(|c| !c.is_control()));
            let p = shape_path(&s, cap);
            if s.chars().count() > cap {
                assert!(p.chars().count() <= cap, "shape_path({s:?}, {cap}) = {p:?}");
            }
            let t = trim_words(&s, cap);
            assert!(t.chars().count() <= cap.max(s.chars().count()));
            let w = wrap(&s, cap);
            assert!(!w.is_empty());
            if cap > 0 {
                for line in &w {
                    assert!(line.chars().count() <= cap, "wrap({s:?}, {cap}) → {line:?}");
                }
            }
            let _ = sanitize(&s, cap);
        }
    }
}

/// The words never panic at the ends of the clock or on odd homes.
#[test]
fn words_never_panic_at_extremes() {
    for at in [0u64, 1, 999, 1000, u64::MAX / 2, u64::MAX - 1, u64::MAX] {
        assert!(stamp_words(at).len() >= 19);
        assert!(date_words(at).len() >= 10);
        let _ = relative_words(u64::MAX, at);
        let _ = relative_words(at, u64::MAX);
        let _ = relative_words(0, at);
    }
    for (path, home) in [
        ("/Users//w/x", Some("/Users//w/")),
        ("/Users//w", Some("/")),
        ("", Some("/Users//w")),
        ("~", Some("~")),
        ("/Users//w/x", Some("/Users//w/x/y")),
        ("\u{1f680}/a", Some("\u{1f680}")),
    ] {
        let _ = home_abbreviate(path, home);
        let _ = abbreviate_paths_in(path, home);
    }
}

/// 6000 random operations — posts with every hold (including absurd spans),
/// restates, resolves, presses, settles, commits, drains and handoffs —
/// and every bound holds after each one: the live set, the ring, the
/// pending queue, the committed rows, the glass (a duplicate-free subset of
/// the live set, every member anchored), every live string within its cap
/// and control-free, `presentation` exact at a random width, the carry
/// re-seeding into a fresh center without growing.
#[test]
fn a_flood_keeps_every_bound() {
    let mut rng = Rng(0x5eed_5eed_5eed_5eed);
    let now = t0();
    let mut c = fresh(now);
    let mut t = 0u64;
    let keys = [
        "k0",
        "k1",
        "k2",
        "update.progress",
        "config.a",
        "privacy.fda",
        "",
        "k7",
    ];
    let titles = [
        "same",
        "other",
        "Update failed",
        "x",
        "aterm closed unexpectedly last time",
    ];
    let pool = every_intent();
    let mut posted = 0usize;
    for step in 0..6000 {
        t += rng.below(300) as u64;
        let at = now + ms(t);
        match rng.below(100) {
            0..=44 => {
                let sev = Severity::ALL[rng.below(4)];
                let hold = match rng.below(7) {
                    0 => Hold::Default,
                    1 => Hold::For(Duration::from_secs(rng.below(100) as u64)),
                    2 => Hold::For(Duration::from_secs(u64::MAX)),
                    3 => Hold::Live {
                        stale_after: Duration::from_secs(1 + rng.below(60) as u64),
                    },
                    4 => Hold::Standing,
                    5 => Hold::Ask {
                        for_: Duration::from_secs(u64::MAX / 2),
                    },
                    _ => Hold::LogOnly,
                };
                let title = if rng.chance(50) {
                    titles[rng.below(5)].to_string()
                } else {
                    rand_str(&mut rng, 300)
                };
                let mut m = Message::new(tags::ALL[rng.below(12)].clone(), sev, title)
                    .lines((0..rng.below(30)).map(|_| rand_str(&mut rng, 400)))
                    .hold(hold)
                    .key(keys[rng.below(keys.len())]);
                for _ in 0..rng.below(4) {
                    m = m.action(pool[rng.below(pool.len())].clone());
                }
                if rng.chance(30) {
                    m = m.meter(Meter {
                        fill_permille: rng.chance(80).then(|| (rng.next() % 2000) as u16),
                        stats: rand_str(&mut rng, 80),
                        ..Meter::default()
                    });
                }
                c.post(m, stamp(t), at);
                posted += 1;
            }
            45..=59 => {
                let ids: Vec<MessageId> = c.live_rows().map(|l| l.id).collect();
                if let Some(id) = ids.get(rng.below(ids.len().max(1))) {
                    let holds = [
                        Hold::Default,
                        Hold::Standing,
                        Hold::LogOnly,
                        Hold::Live {
                            stale_after: Duration::from_secs(5),
                        },
                    ];
                    c.restate(
                        *id,
                        Restatement {
                            title: rng.chance(30).then(|| rand_str(&mut rng, 300)),
                            detail: rng.chance(30).then(|| {
                                (0..rng.below(40))
                                    .map(|_| rand_str(&mut rng, 400))
                                    .collect()
                            }),
                            meter: rng.chance(30).then(|| {
                                rng.chance(70).then(|| Meter {
                                    fill_permille: Some((rng.next() % 2000) as u16),
                                    stats: rand_str(&mut rng, 80),
                                    ..Meter::default()
                                })
                            }),
                            excerpt: rng.chance(10).then(|| rng.chance(50)),
                            finished: rng
                                .chance(10)
                                .then(|| rng.chance(50).then(|| rand_str(&mut rng, 300))),
                            actions: rng.chance(20).then(|| {
                                (0..rng.below(5))
                                    .map(|_| pool[rng.below(pool.len())].clone())
                                    .collect()
                            }),
                            hold: rng.chance(20).then(|| holds[rng.below(4)]),
                            severity: rng.chance(20).then(|| Severity::ALL[rng.below(4)]),
                            glyph: None,
                        },
                        at,
                    );
                }
            }
            60..=69 => {
                let ids: Vec<MessageId> = c.live_rows().map(|l| l.id).collect();
                if let Some(id) = ids.get(rng.below(ids.len().max(1))) {
                    match rng.below(5) {
                        0 => {
                            c.resolve(*id, Outcome::Ok, at);
                        }
                        1 => {
                            c.dismiss(*id, at);
                        }
                        2 => {
                            c.mark_seen(*id, at);
                        }
                        3 => {
                            c.act(*id, ActionIndex(rng.below(3) as u8), at);
                        }
                        _ => {
                            c.act(*id, ActionIndex::DETAILS, at);
                        }
                    }
                }
            }
            70..=84 => {
                c.settle(at, rng.chance(80));
            }
            85..=94 => {
                c.commit_rows(at, rng.below(4) as u16);
            }
            95..=97 => {
                for l in &c.drain_new_for_persist() {
                    assert_eq!(LogLine::decode(&l.encode()), Ok(lossy(l)));
                }
            }
            _ => {
                let carry = c.carried();
                let mut child = fresh(at);
                child.seed_carried(&carry, stamp(0), at);
                assert!(child.live_rows().count() <= MAX_LIVE);
                assert!(child.log().pending_len() <= PENDING_PERSIST_CAP);
                assert!(child.log().next_id() >= c.log().next_id());
                child.after_handoff_commit(at + ms(1));
                assert_eq!(child.carried().live.len(), carry.live.len());
                if rng.chance(30) {
                    c = child;
                }
            }
        }
        assert!(c.live_rows().count() <= MAX_LIVE, "step {step}");
        assert!(c.log().len() <= LOG_CAP, "step {step}");
        assert!(c.log().pending_len() <= PENDING_PERSIST_CAP, "step {step}");
        assert!(c.committed_rows() <= MAX_ROWS);
        let glass = glass_ids(&c);
        assert!(glass.len() <= usize::from(MAX_ROWS));
        let mut uniq = glass.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(
            uniq.len(),
            glass.len(),
            "step {step}: duplicate glass ids {glass:?}"
        );
        for id in &glass {
            let row = c.live(*id).expect("glass is a subset of live");
            assert!(
                row.on_glass_since.is_some(),
                "step {step}: on glass without an anchor"
            );
        }
        assert_eq!(c.queued(), c.live_rows().count() - glass.len());
        for l in c.live_rows() {
            assert!(l.msg.title.chars().count() <= TITLE_CAP);
            assert!(
                l.msg.title.chars().all(|ch| !ch.is_control()),
                "step {step}: {:?}",
                l.msg.title
            );
            assert!(l.msg.detail.len() <= DETAIL_LINES_CAP);
            assert!(l.msg.detail.iter().all(|d| {
                d.chars().count() <= DETAIL_LINE_CAP && d.chars().all(|ch| !ch.is_control())
            }));
            assert!(l.msg.actions.len() <= MAX_ACTIONS);
            assert!(!l.msg.actions.contains(&Intent::Details));
            assert!(
                l.msg
                    .key
                    .as_ref()
                    .is_none_or(|k| !k.is_empty() && k.chars().count() <= 48)
            );
            assert!(l.msg.meter.as_ref().is_none_or(|m| {
                m.stats.chars().count() <= 40 && m.fill_permille.is_none_or(|p| p <= 1000)
            }));
            assert!(l.msg.hold != Hold::LogOnly);
            assert!(l.patience_at >= l.posted_at);
        }
        if rng.chance(20) {
            let cols = rng.below(220);
            let p = c.presentation(cols, &chars, Some("/Users//w"), Links::Painted);
            assert!(p.rows.len() <= usize::from(c.committed_rows()));
            for row in &p.rows {
                assert_eq!(chars(&render(row, cols)), cols, "step {step}");
            }
            assert_eq!(p.fingerprint() == 0, p.rows.is_empty());
            assert_eq!(c.fingerprint(cols) == 0, c.committed_rows() == 0);
            let _ = c.deadline(true);
            let _ = c.deadline(false);
        }
    }
    assert!(posted > 1500);
}
