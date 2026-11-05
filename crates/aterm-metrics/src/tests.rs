// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm-metrics' tests: the subject grammar, the rules as astream takes them, the row codecs,
//! the two merges (§10 "Metrics merge is exact", with its negative control) and AS1's
//! highest-sequence rows on a real broker.

use astream_wire::summary::{MetricName, Summary, Window, WindowLen};

use crate::merge::{Counters, OfferError, Offered, Replica, Snapshots, Usage};
use crate::row::{
    AGY_GROUPS_MAX, AgyGroup, AgyRow, HeldEntry, Label, ManagerEntry, NEEDS_MAX, NeedEntry,
    NeedsRow, PCT_MAX, PlaceRow, QuotaRow, REMAINING_MAX, Row, RowError, Sid, SignIn, UNREAD_MAX,
    UnreadRow, WindowRecord, Word,
};
use crate::rules;
use crate::subject::{
    AccountKey, Class, Cli, Family, HomeKey, METRIC_COUNT, Metric, Node, Now, Subject,
    SubjectError, TokenClass, Width,
};

fn node(n: u8) -> Node {
    Node::parse(&format!("n-{n:02x}00000000000000")).expect("a node id")
}

fn account(n: u8) -> AccountKey {
    AccountKey::parse(&format!("ac-{n:02x}000000000000ab")).expect("an account key")
}

fn word(w: &str) -> Word {
    Word::new(w).expect("a word")
}

fn sid(n: u8) -> Sid {
    Sid::parse(&format!("s-{n:02x}000000000000000000")).expect("a sid")
}

/// Every subject of one Mac this build names, one of each shape.
fn every_subject(n: Node) -> Vec<Subject> {
    let mut all = Vec::new();
    for metric in Metric::ALL {
        all.push(Subject::Raw { node: n, metric });
        all.push(Subject::Window { node: n, metric });
    }
    all.push(Subject::Journal { node: n });
    for cli in Cli::ALL.into_iter().filter(|c| c.has_accounts()) {
        all.push(Subject::quota(n, cli, account(1)).expect("an account CLI"));
        all.push(
            Subject::quota(
                n,
                cli,
                AccountKey::parse("sa-00112233445566ff").expect("sa key"),
            )
            .expect("an account CLI"),
        );
    }
    all.push(Subject::Agy {
        node: n,
        home: HomeKey::Home,
    });
    all.push(Subject::Agy {
        node: n,
        home: HomeKey::Hashed(0x0123_4567_89ab_cdef),
    });
    for now in [
        Now::Presence,
        Now::Needs,
        Now::Unread,
        Now::Place(account(2)),
    ] {
        all.push(Subject::Now { node: n, now });
    }
    all
}

fn quota_row(at: u64, pct7: Option<u16>, week_reset: Option<u64>) -> QuotaRow {
    QuotaRow {
        at,
        cli: Cli::Claude,
        account: account(1),
        label: Label::new("claude-a").expect("a label"),
        condition: word("usable"),
        enrolled: true,
        pct5: Some(12),
        pct7,
        five_hour_reset: None,
        week_reset,
        walled_until: None,
        last_used: Some(at.saturating_sub(60)),
        signin: Some(SignIn {
            state: word("in"),
            t: at.saturating_sub(3_600),
        }),
        last_start: Some(at.saturating_sub(600)),
        last_end: None,
        sessions_7d: 3,
        plan: Some(word("max")),
    }
}

// ---------------------------------------------------------------------------
// Subjects
// ---------------------------------------------------------------------------

/// THE GRAMMAR IS ONE SPELLING PER SUBJECT: every subject of every shape reads back as
/// itself, under the owner its Display names, in the class it names.
#[test]
fn every_subject_reads_back_as_itself() {
    let n = node(0xa7);
    let all = every_subject(n);
    assert_eq!(all.len(), 2 * METRIC_COUNT + 1 + 8 + 2 + 4);
    for s in &all {
        let text = s.to_string();
        assert!(
            astream_wire::subject::Subject::new(text.clone()).is_ok(),
            "{text} is an astream subject"
        );
        assert_eq!(Subject::parse(&text), Ok(*s), "{text}");
        assert_eq!(s.node(), n);
        assert!(text.starts_with("/m/n-a700000000000000/"), "{text}");
        assert_eq!(
            s.class().travels(),
            text.contains("/shared/"),
            "{text}: only shared subjects travel"
        );
    }
    assert_eq!(
        Subject::quota(n, Cli::Agy, account(1)),
        None,
        "agy's quota is per home"
    );
}

/// WHAT THE GRAMMAR REFUSES, each by the segment it names.
#[test]
fn the_grammar_refuses_each_malformed_segment() {
    let id = "n-0123456789abcdef";
    for (text, why) in [
        ("m/n-0123456789abcdef/local/journal", SubjectError::Root),
        ("/f/n-0123456789abcdef/local/journal", SubjectError::Root),
        ("/m/n-0123456789ABCDEF/local/journal", SubjectError::Node),
        // A legacy 32-hex node id never owns a metrics subject.
        (
            "/m/0123456789abcdef0123456789abcdef/local/journal",
            SubjectError::Node,
        ),
        ("/m/n-0123456789abcde/local/journal", SubjectError::Node),
        (&format!("/m/{id}/local/other"), SubjectError::Class),
        (
            &format!("/m/{id}/shared/1d/claude.tokens"),
            SubjectError::Class,
        ),
        (
            &format!("/m/{id}/shared/1m/claude.vibes"),
            SubjectError::Metric,
        ),
        (
            &format!("/m/{id}/shared/1m/claude.opus.output"),
            SubjectError::Width,
        ),
        (
            &format!("/m/{id}/shared/1h/claude.tokens"),
            SubjectError::Width,
        ),
        (
            &format!("/m/{id}/shared/quota/gemini/ac-0011223344556677"),
            SubjectError::Cli,
        ),
        (
            &format!("/m/{id}/shared/quota/claude/home"),
            SubjectError::Account,
        ),
        (
            &format!("/m/{id}/shared/quota/claude/ac-00112233445566"),
            SubjectError::Account,
        ),
        (
            &format!("/m/{id}/shared/quota/agy/ac-0011223344556677"),
            SubjectError::Home,
        ),
        (
            &format!("/m/{id}/shared/quota/agy/H0011223344556677"),
            SubjectError::Home,
        ),
        (&format!("/m/{id}/shared/now/weather"), SubjectError::Now),
        (
            &format!("/m/{id}/shared/now/place/someone@example.com"),
            SubjectError::Account,
        ),
        (&format!("/m/{id}/local/journal/extra"), SubjectError::Shape),
        (&format!("/m/{id}/shared/quota/claude"), SubjectError::Shape),
    ] {
        assert_eq!(Subject::parse(text), Err(why), "{text}");
    }
}

/// A NODE ID IS ASTREAM'S PRINCIPAL of the node key: the one name the transport, the fabric
/// and presence give a Mac.
#[test]
fn a_node_id_is_astreams_identity_principal() {
    let mut key = [0u8; 32];
    for (i, b) in key.iter_mut().enumerate() {
        *b = u8::try_from(i * 37 % 256).expect("a byte");
    }
    let n = Node::of_key(&key);
    assert_eq!(n.to_string(), astream_broker::identity_principal(&key));
    assert_eq!(Node::parse(&n.to_string()), Some(n));
    assert_eq!(n.origin().as_str(), n.to_string());
}

/// THE METRICS ARE A CLOSED, COMPILED SET: 2 minute totals and 28 hourly classes, every name
/// distinct, an astream summary name, and read back by `parse`.
#[test]
fn the_metric_names_are_closed_and_distinct() {
    let names: std::collections::BTreeSet<String> = Metric::ALL.iter().map(|m| m.name()).collect();
    assert_eq!(names.len(), METRIC_COUNT);
    assert_eq!(METRIC_COUNT, 30);
    let minute = Metric::ALL
        .iter()
        .filter(|m| m.width() == Width::Minute)
        .count();
    assert_eq!((minute, METRIC_COUNT - minute), (2, 28));
    for m in Metric::ALL {
        assert_eq!(Metric::parse(&m.name()), Some(m));
        assert_eq!(MetricName::new(&m.name()).ok(), Some(m.metric_name()));
    }
    assert_eq!(Metric::Tokens(Cli::Codex).name(), "codex.tokens");
    assert_eq!(
        Metric::class(Cli::Claude, Family::Opus, TokenClass::CacheRead).map(Metric::name),
        Some("claude.opus.cache_read".to_string())
    );
    assert_eq!(
        Metric::class(Cli::Codex, Family::Opus, TokenClass::Input),
        None
    );
    assert_eq!(
        Metric::class(Cli::Grok, Family::Gpt, TokenClass::Input),
        None
    );
    assert_eq!(Family::of_claude(Some("sonnet")), Family::Sonnet);
    assert_eq!(Family::of_claude(Some("newfamily")), Family::Other);
    assert_eq!(Family::of_claude(None), Family::Other);
}

// ---------------------------------------------------------------------------
// Rules
// ---------------------------------------------------------------------------

/// ASTREAM TAKES THE METRICS RULES, as `aterm-link`'s `broker_rules` test holds the fabric's:
/// one prefix rule over raw samples, three hold rules keeping every record, two keeping each
/// subject's `Last` row, and the default rule; every bound as the constants name it.
#[test]
fn astream_takes_the_metrics_rules() {
    use astream_broker::{Class as Rc, Keep};
    use std::time::Duration;
    type Rule<'a> = (Option<&'a str>, Rc, Keep, Option<Duration>, Option<u64>);
    let r = rules::rules();
    let got: Vec<Rule<'_>> = r
        .rules()
        .iter()
        .chain(std::iter::once(r.default_rule()))
        .map(|rule| {
            (
                rule.filter(),
                rule.class(),
                rule.keep(),
                rule.age(),
                rule.bytes(),
            )
        })
        .collect();
    assert_eq!(
        got,
        vec![
            (
                Some(rules::RAW_FILTER),
                Rc::Prefix,
                Keep::All,
                Some(rules::RAW_AGE),
                Some(rules::RAW_BYTES)
            ),
            (
                Some(rules::JOURNAL_FILTER),
                Rc::Hold,
                Keep::All,
                Some(rules::JOURNAL_AGE),
                Some(rules::JOURNAL_BYTES)
            ),
            (
                Some(rules::MINUTE_FILTER),
                Rc::Hold,
                Keep::All,
                Some(rules::MINUTE_AGE),
                Some(rules::MINUTE_BYTES)
            ),
            (
                Some(rules::HOUR_FILTER),
                Rc::Hold,
                Keep::All,
                Some(rules::HOUR_AGE),
                Some(rules::HOUR_BYTES)
            ),
            (
                Some(rules::QUOTA_FILTER),
                Rc::Hold,
                Keep::Last,
                None,
                Some(rules::QUOTA_BYTES)
            ),
            (
                Some(rules::NOW_FILTER),
                Rc::Hold,
                Keep::Last,
                None,
                Some(rules::NOW_BYTES)
            ),
            (None, Rc::Hold, Keep::Last, None, Some(rules::DEFAULT_BYTES)),
        ]
    );
    assert_eq!(rules::RAW_AGE, Duration::from_secs(24 * 3_600));
    assert_eq!(rules::JOURNAL_AGE, Duration::from_secs(30 * 86_400));
    assert_eq!(rules::MINUTE_AGE, Duration::from_secs(2 * 3_600));
    assert_eq!(rules::HOUR_AGE, Duration::from_secs(8 * 86_400));
    let caps = rules::Bytes::CAPS;
    assert_eq!(caps.shared(), 89 * 1024 * 1024, "§4.14: 89 MiB replicated");
    assert_eq!(
        caps.raw + caps.journal,
        68 * 1024 * 1024,
        "§4.14: 68 MiB local"
    );
    assert_eq!(caps.total(), 161 * 1024 * 1024);
    assert_eq!(rules::rules(), r, "one constructor, one answer");
    let scaled = rules::rules_scaled(rules::Bytes {
        raw: 1,
        journal: 2,
        minute: 3,
        hour: 4,
        quota: 5,
        now: 6,
        default: 7,
    });
    let bytes: Vec<Option<u64>> = scaled
        .rules()
        .iter()
        .chain(std::iter::once(scaled.default_rule()))
        .map(astream_broker::Rule::bytes)
        .collect();
    assert_eq!(bytes, (1..=7).map(Some).collect::<Vec<_>>());
}

/// EACH SUBJECT FALLS UNDER THE RULE ITS CLASS NAMES, by astream's own filter matching, and
/// astream keeps only the `Last` row exactly of the quota and `now` subjects: the ones whose
/// late lower-sequence rows its relay refuses.
#[test]
fn each_subject_falls_under_the_rule_its_class_names() {
    let r = rules::rules();
    let filter_of = |class: Class| match class {
        Class::Raw => rules::RAW_FILTER,
        Class::Journal => rules::JOURNAL_FILTER,
        Class::Minute => rules::MINUTE_FILTER,
        Class::Hour => rules::HOUR_FILTER,
        Class::Quota => rules::QUOTA_FILTER,
        Class::Now => rules::NOW_FILTER,
    };
    for s in every_subject(node(3)) {
        let text = s.to_string();
        let first = r.rules().iter().find(|rule| {
            rule.filter()
                .is_some_and(|f| astream_wire::Filter::new(f).is_ok_and(|f| f.matches_str(&text)))
        });
        assert_eq!(
            first.and_then(astream_broker::Rule::filter),
            Some(filter_of(s.class())),
            "{text}"
        );
        assert_eq!(r.keeps_last(&text), s.class().keeps_last(), "{text}");
    }
    // The relay grant's owner filter covers exactly the subjects of its Mac that travel, and
    // none of another Mac's.
    let shared = astream_wire::Filter::new(crate::subject::shared_of(node(3))).expect("a filter");
    let local = astream_wire::Filter::new(crate::subject::local_of(node(3))).expect("a filter");
    for s in every_subject(node(3)) {
        let text = s.to_string();
        assert_eq!(shared.matches_str(&text), s.class().travels(), "{text}");
        assert_eq!(local.matches_str(&text), !s.class().travels(), "{text}");
    }
    for s in every_subject(node(4)) {
        let text = s.to_string();
        assert!(!shared.matches_str(&text), "{text}: another Mac's");
        assert!(!local.matches_str(&text), "{text}: another Mac's");
    }
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

fn every_row() -> Vec<(Subject, Row)> {
    let n = node(9);
    let mut quota = quota_row(1_793_000_000, Some(64), Some(1_793_300_000));
    quota.five_hour_reset = Some(1_793_010_000);
    let mut bare = quota_row(5, None, None);
    bare.signin = None;
    bare.plan = None;
    bare.last_used = None;
    bare.last_start = None;
    bare.pct5 = None;
    bare.walled_until = Some(u64::MAX);
    bare.cli = Cli::Kimi;
    let agy = AgyRow {
        at: 1_793_000_100,
        home: HomeKey::Hashed(7),
        groups: vec![
            AgyGroup {
                id: word("gemini-weekly"),
                remaining: Some(6_400),
                reset_at: Some(1_793_400_000),
                off: false,
            },
            AgyGroup {
                id: word("3p-5h"),
                remaining: None,
                reset_at: None,
                off: true,
            },
        ],
    };
    let place = PlaceRow {
        at: 10,
        account: account(5),
        place: word("borrow"),
        since: 10,
        until: 1_000,
    };
    let needs = NeedsRow {
        at: 77,
        needs: vec![NeedEntry {
            sid: sid(1),
            word: word("approval"),
            harness: true,
            worker_of: Some(sid(2)),
            since: Some(70),
        }],
        held: vec![HeldEntry {
            sid: sid(3),
            hold: word("person"),
            by: Some(node(8)),
            since: None,
        }],
        managers: vec![ManagerEntry {
            sid: sid(2),
            workers: 4,
        }],
    };
    let unread = UnreadRow {
        at: 3,
        pools: vec![word("grok"), word("kimi")],
    };
    let window = WindowRecord::new(
        n,
        Metric::Class(Cli::Claude, Family::Opus, TokenClass::Output),
        3_600_000 * 498_000,
        2,
        Summary::of([1, 63, 64, 1_000, 250_000]),
    )
    .expect("an aligned hour");
    let mut all = vec![
        (quota.subject(n).expect("claude"), Row::Quota(quota)),
        (bare.subject(n).expect("kimi"), Row::Quota(bare)),
        (agy.subject(n), Row::Agy(agy)),
        (place.subject(n), Row::Place(place)),
        (NeedsRow::subject(n), Row::Needs(needs)),
        (NeedsRow::subject(n), Row::Needs(NeedsRow::default())),
        (UnreadRow::subject(n), Row::Unread(unread)),
        (window.subject(), Row::Window(window)),
    ];
    // One window record of every metric, on a start both widths align to.
    for (i, metric) in (0u64..).zip(Metric::ALL) {
        let record = WindowRecord::new(n, metric, 3_600_000 * 498_000, i, Summary::of([i]))
            .expect("a named metric on an aligned window");
        all.push((record.subject(), Row::Window(record)));
    }
    all
}

/// EVERY ROW READS BACK AS ITSELF AT ITS SUBJECT, and its bytes are the one encoding of its
/// value: decoding and encoding again gives the same bytes.
#[test]
fn every_row_reads_back_as_itself_at_its_subject() {
    for (subject, row) in every_row() {
        let bytes = row.encode();
        assert_eq!(row.subject(subject.node()), Some(subject), "{subject}");
        let back = Row::decode_at(&subject, &bytes).expect("it decodes at its subject");
        assert_eq!(back, row, "{subject}");
        assert_eq!(back.encode(), bytes, "{subject}: one encoding");
        assert!(bytes.len() < 300, "{subject}: {} bytes", bytes.len());
    }
}

/// A body built by hand, field by field, for what the encoder never writes.
struct Body(Vec<u8>);

impl Body {
    fn new(kind: u8) -> Body {
        Body(vec![kind, crate::row::VERSION])
    }

    fn varint(mut self, mut v: u64) -> Body {
        loop {
            let low = u8::try_from(v & 0x7f).expect("seven bits");
            v >>= 7;
            if v == 0 {
                self.0.push(low);
                return self;
            }
            self.0.push(low | 0x80);
        }
    }

    fn count(self, n: usize) -> Body {
        self.varint(u64::try_from(n).expect("small"))
    }

    fn byte(mut self, b: u8) -> Body {
        self.0.push(b);
        self
    }

    fn text(mut self, s: &str) -> Body {
        self.0.push(u8::try_from(s.len()).expect("short"));
        self.0.extend_from_slice(s.as_bytes());
        self
    }

    fn repeat(self, n: usize, entry: impl Fn(Body) -> Body) -> Body {
        (0..n).fold(self, |b, _| entry(b))
    }
}

/// THE DECODER IS STRICT: every truncation, a trailing byte, another kind, another version, a
/// non-minimal varint and a flag byte of 2 are refused, never misread; so is every bound
/// passed (each percent, each list, each 32-bit count, the identity's length, a share), in a
/// body built by hand, while the same body at the bound reads.
#[test]
fn the_decoder_refuses_everything_the_encoder_does_not_write() {
    for (subject, row) in every_row() {
        let bytes = row.encode();
        for cut in 0..bytes.len() {
            assert!(
                Row::decode_at(&subject, &bytes[..cut]).is_err(),
                "{subject}: a {cut}-byte prefix"
            );
        }
        let mut long = bytes.clone();
        long.push(0);
        assert!(
            Row::decode_at(&subject, &long).is_err(),
            "{subject}: trailing"
        );
        let mut version = bytes.clone();
        version[1] = 2;
        assert_eq!(
            Row::decode_at(&subject, &version),
            Err(RowError::Version(2)),
            "{subject}"
        );
    }
    let q = quota_row(300, Some(5), None);
    let subject = q.subject(node(1)).expect("claude");
    let good = q.encode();
    // `at` is the first field: 300 is the two bytes [0xac, 0x02]; a third, zero byte after a
    // continuation spells it again, longer.
    assert_eq!(&good[2..4], &[0xac, 0x02]);
    let mut overlong = good.clone();
    overlong.splice(2..4, [0xac, 0x82, 0x00]);
    assert_eq!(Row::decode_at(&subject, &overlong), Err(RowError::Varint));
    let mut kind = good.clone();
    kind[0] = b'P';
    assert!(matches!(
        Row::decode_at(&subject, &kind),
        Err(RowError::Kind {
            expected: b'Q',
            found: b'P'
        })
    ));
    // The enrolled flag sits right after the four texts; 2 is neither false nor true.
    let mut flag = good.clone();
    let at = 4 + 1 + "claude".len() + 1 + 19 + 1 + "claude-a".len() + 1 + "usable".len();
    assert_eq!(flag[at], 1, "the enrolled flag");
    flag[at] = 2;
    assert_eq!(Row::decode_at(&subject, &flag), Err(RowError::Tag));

    // Each percent: its tag and one-byte value follow the enrolled flag (12, then 5).
    let over_pct = |offset: usize| {
        let mut b = good.clone();
        assert_eq!(b[at + offset], 1, "a percent's tag");
        b.splice(at + offset + 1..at + offset + 2, [0xe9, 0x07]); // 1001
        QuotaRow::decode(&b)
    };
    assert_eq!(over_pct(1), Err(RowError::Bound("the 5-hour percent")));
    assert_eq!(over_pct(3), Err(RowError::Bound("the week percent")));
    // Sessions in 7 days, a 32-bit count, sits before the plan (`1`, `3`, `max`).
    let mut sessions = good.clone();
    let s_at = sessions.len() - 6;
    assert_eq!(&sessions[s_at..], &[3, 1, 3, b'm', b'a', b'x']);
    sessions.splice(s_at..=s_at, [0x80, 0x80, 0x80, 0x80, 0x10]); // 2^32
    assert_eq!(
        QuotaRow::decode(&sessions),
        Err(RowError::Bound("sessions in 7 days"))
    );

    // Each list, one past its bound and at it.
    let sid_text = sid(1).to_string();
    let needs = |needs: usize, held: usize, managers: usize, workers: u64| {
        NeedsRow::decode(
            &Body::new(b'N')
                .varint(1)
                .count(needs)
                .repeat(needs, |b| {
                    b.text(&sid_text).text("ask").byte(0).byte(0).byte(0)
                })
                .count(held)
                .repeat(held, |b| b.text(&sid_text).text("person").byte(0).byte(0))
                .count(managers)
                .repeat(managers, |b| b.text(&sid_text).varint(workers))
                .0,
        )
    };
    assert_eq!(
        needs(NEEDS_MAX + 1, 0, 0, 1),
        Err(RowError::TooMany("needs"))
    );
    assert_eq!(
        needs(0, NEEDS_MAX + 1, 0, 1),
        Err(RowError::TooMany("held sessions"))
    );
    assert_eq!(
        needs(0, 0, NEEDS_MAX + 1, 1),
        Err(RowError::TooMany("managers"))
    );
    assert_eq!(
        needs(0, 0, 1, 1 << 32),
        Err(RowError::Bound("a manager's workers"))
    );
    let full = needs(NEEDS_MAX, NEEDS_MAX, NEEDS_MAX, u64::from(u32::MAX)).expect("at the bounds");
    assert_eq!(
        (full.needs.len(), full.held.len(), full.managers.len()),
        (NEEDS_MAX, NEEDS_MAX, NEEDS_MAX)
    );
    let unread = |n: usize| {
        UnreadRow::decode(
            &Body::new(b'U')
                .varint(1)
                .count(n)
                .repeat(n, |b| b.text("grok"))
                .0,
        )
    };
    assert_eq!(
        unread(UNREAD_MAX + 1),
        Err(RowError::TooMany("unread pools"))
    );
    assert_eq!(unread(UNREAD_MAX).map(|r| r.pools.len()), Ok(UNREAD_MAX));
    let agy = |n: usize, share: u64| {
        AgyRow::decode(
            &Body::new(b'A')
                .varint(1)
                .text("home")
                .count(n)
                .repeat(n, |b| b.text("a").byte(1).varint(share).byte(0).byte(0))
                .0,
        )
    };
    assert_eq!(
        agy(AGY_GROUPS_MAX + 1, 1),
        Err(RowError::TooMany("agy pools"))
    );
    assert_eq!(
        agy(1, u64::from(REMAINING_MAX) + 1),
        Err(RowError::Bound("an agy share"))
    );
    assert_eq!(
        agy(AGY_GROUPS_MAX, u64::from(REMAINING_MAX)).map(|r| r.groups.len()),
        Ok(AGY_GROUPS_MAX)
    );
    let long_id = Body::new(b'W').count(256).repeat(256, |b| b.byte(0)).0;
    assert_eq!(
        WindowRecord::decode(&long_id),
        Err(RowError::TooMany("identity bytes"))
    );

    // A window that is not its metric's width, which only a record built past `new` has.
    let mut wide = WindowRecord::new(
        node(1),
        Metric::Tokens(Cli::Claude),
        T0_MS,
        0,
        Summary::of([1]),
    )
    .expect("a minute");
    wide.window = Window::starting(T0_MS, WindowLen::Hour).expect("an hour");
    assert_eq!(
        Row::decode_at(&wide.subject(), &wide.encode()),
        Err(RowError::Mismatch("window width"))
    );
}

/// A WRITER WRITES WHAT EVERY READER TAKES: a list past its bound is written to its bound, its
/// first entries; a percent or a share past its bound at the bound; a metric this build does
/// not name makes no window record.
#[test]
fn every_row_a_writer_makes_reads() {
    let mut q = quota_row(1, Some(PCT_MAX + 1), None);
    q.pct5 = Some(u16::MAX);
    let back = QuotaRow::decode(&q.encode()).expect("it reads");
    assert_eq!((back.pct5, back.pct7), (Some(PCT_MAX), Some(PCT_MAX)));
    let entry = |i: usize| ManagerEntry {
        sid: sid(u8::try_from(i % 200).expect("small")),
        workers: u32::try_from(i).expect("small"),
    };
    let crowded = NeedsRow {
        at: 1,
        needs: vec![
            NeedEntry {
                sid: sid(1),
                word: word("ask"),
                harness: false,
                worker_of: None,
                since: None,
            };
            NEEDS_MAX + 3
        ],
        held: vec![
            HeldEntry {
                sid: sid(2),
                hold: word("person"),
                by: None,
                since: None,
            };
            NEEDS_MAX + 1
        ],
        managers: (0..NEEDS_MAX + 1).map(entry).collect(),
    };
    let back = NeedsRow::decode(&crowded.encode()).expect("it reads");
    assert_eq!((back.needs.len(), back.held.len()), (NEEDS_MAX, NEEDS_MAX));
    assert_eq!(
        back.managers,
        (0..NEEDS_MAX).map(entry).collect::<Vec<_>>(),
        "the first entries"
    );
    let agy = AgyRow {
        at: 1,
        home: HomeKey::Home,
        groups: vec![
            AgyGroup {
                id: word("a"),
                remaining: Some(REMAINING_MAX + 1),
                reset_at: None,
                off: false,
            };
            AGY_GROUPS_MAX + 1
        ],
    };
    let back = AgyRow::decode(&agy.encode()).expect("it reads");
    assert_eq!(back.groups.len(), AGY_GROUPS_MAX);
    assert_eq!(back.groups[0].remaining, Some(REMAINING_MAX));
    let unread = UnreadRow {
        at: 1,
        pools: vec![word("grok"); UNREAD_MAX + 1],
    };
    assert_eq!(
        UnreadRow::decode(&unread.encode()).map(|r| r.pools.len()),
        Ok(UNREAD_MAX)
    );
    // NEGATIVE CONTROL: a metric outside the compiled set, built past `new`, encodes and is
    // refused by every reader, which is why `new` refuses it.
    for metric in [
        Metric::Tokens(Cli::Grok),
        Metric::Class(Cli::Claude, Family::Gpt, TokenClass::Input),
    ] {
        assert_eq!(
            WindowRecord::new(node(1), metric, T0_MS, 0, Summary::new()),
            None,
            "{metric}"
        );
        assert_eq!(Metric::parse(&metric.name()), None, "{metric}");
    }
    assert_eq!(Metric::tokens(Cli::Grok), None);
    assert_eq!(
        Metric::tokens(Cli::Claude),
        Some(Metric::Tokens(Cli::Claude))
    );
    let mut unnamed = WindowRecord::new(
        node(1),
        Metric::Tokens(Cli::Claude),
        T0_MS,
        0,
        Summary::new(),
    )
    .expect("named");
    unnamed.metric = Metric::Tokens(Cli::Grok);
    assert_eq!(
        WindowRecord::decode(&unnamed.encode()),
        Err(RowError::Text("the metric"))
    );
}

/// NO ROW CAN CARRY FREE TEXT: the grammars every text field is held to refuse an email
/// address, a URL, a path, a `host:port` and a sentence, on the way in and on the way out (a
/// roster label may be a dotted name, as the roster's own grammar allows).
#[test]
fn no_row_field_can_carry_an_address_a_path_or_a_sentence() {
    for text in [
        "someone@example.com",
        "https://example.com",
        "/Users//someone/.claude",
        "10.0.0.5:22",
        "Needs Approval",
        "",
        "-leading",
        "a b",
        "x".repeat(65).as_str(),
    ] {
        assert!(Word::new(text).is_none(), "word {text:?}");
        assert!(Label::new(text).is_none(), "label {text:?}");
        assert!(Sid::parse(text).is_none(), "sid {text:?}");
        assert!(AccountKey::parse(text).is_none(), "account {text:?}");
        assert!(Node::parse(text).is_none(), "node {text:?}");
    }
    assert!(Word::new("not-enrolled").is_some());
    assert!(Word::new("3p-5h").is_some());
    assert!(Label::new("work.max-2").is_some());
    // A label smuggled past the constructor is refused by the reader.
    let q = quota_row(1, None, None);
    let subject = q.subject(node(1)).expect("claude");
    let mut bytes = q.encode();
    let at = bytes
        .windows(8)
        .position(|w| w == b"claude-a")
        .expect("the label");
    bytes[at + 6] = b'@';
    assert_eq!(
        Row::decode_at(&subject, &bytes),
        Err(RowError::Text("the label"))
    );
    // And a word, a session id and a node id, each in place of a good one of its length.
    let smuggle = |row: &Row, good: &[u8], bad: &[u8]| {
        let (subject, mut bytes) = (row.subject(node(1)).expect("placed"), row.encode());
        let at = bytes
            .windows(good.len())
            .position(|w| w == good)
            .expect("the field");
        bytes[at..at + good.len()].copy_from_slice(bad);
        Row::decode_at(&subject, &bytes)
    };
    let quota = Row::Quota(quota_row(1, None, None));
    assert_eq!(
        smuggle(&quota, b"usable", b"us@ble"),
        Err(RowError::Text("the condition"))
    );
    let needs = every_row()
        .into_iter()
        .find_map(|(_, r)| match r {
            Row::Needs(n) if !n.needs.is_empty() => Some(Row::Needs(n)),
            _ => None,
        })
        .expect("a needs row");
    assert_eq!(
        smuggle(&needs, b"approval", b"appr val"),
        Err(RowError::Text("a need word"))
    );
    assert_eq!(
        smuggle(
            &needs,
            sid(1).to_string().as_bytes(),
            b"s-0100000000000000000/"
        ),
        Err(RowError::Text("a session id"))
    );
    assert_eq!(
        smuggle(
            &needs,
            node(8).to_string().as_bytes(),
            b"10.0.0.8:22/abcde+"
        ),
        Err(RowError::Text("the holding node"))
    );
}

/// A BODY SPEAKS ONLY FOR ITS SUBJECT: the subject is what the relay checked the origin's
/// grant on, so a row naming another account, CLI, home, origin or metric is refused there.
#[test]
fn a_body_that_names_another_subject_is_refused() {
    let n = node(1);
    let q = quota_row(1, Some(1), None);
    let other_account = Subject::quota(n, Cli::Claude, account(2)).expect("claude");
    assert_eq!(
        Row::decode_at(&other_account, &q.encode()),
        Err(RowError::Mismatch("account"))
    );
    let other_cli = Subject::quota(n, Cli::Codex, account(1)).expect("codex");
    assert_eq!(
        Row::decode_at(&other_cli, &q.encode()),
        Err(RowError::Mismatch("CLI"))
    );
    let w = WindowRecord::new(n, Metric::Tokens(Cli::Claude), 60_000, 0, Summary::of([5]))
        .expect("a minute");
    let elsewhere = Subject::Window {
        node: node(2),
        metric: Metric::Tokens(Cli::Claude),
    };
    assert_eq!(
        Row::decode_at(&elsewhere, &w.encode()),
        Err(RowError::Mismatch("origin"))
    );
    let other_metric = Subject::Window {
        node: n,
        metric: Metric::Tokens(Cli::Codex),
    };
    assert_eq!(
        Row::decode_at(&other_metric, &w.encode()),
        Err(RowError::Mismatch("metric"))
    );
    assert_eq!(
        WindowRecord::new(n, Metric::Tokens(Cli::Claude), 60_001, 0, Summary::new()),
        None,
        "a window starts on its width's boundary"
    );
    let agy = AgyRow {
        at: 1,
        home: HomeKey::Hashed(7),
        groups: Vec::new(),
    };
    assert_eq!(
        Row::decode_at(
            &Subject::Agy {
                node: n,
                home: HomeKey::Home
            },
            &agy.encode()
        ),
        Err(RowError::Mismatch("home"))
    );
    let place = PlaceRow {
        at: 1,
        account: account(5),
        place: word("fill"),
        since: 1,
        until: 2,
    };
    assert_eq!(
        Row::decode_at(
            &Subject::Now {
                node: n,
                now: Now::Place(account(2))
            },
            &place.encode()
        ),
        Err(RowError::Mismatch("account"))
    );
    for local in [
        Subject::Journal { node: n },
        Subject::Raw {
            node: n,
            metric: Metric::Tokens(Cli::Claude),
        },
        Subject::Now {
            node: n,
            now: Now::Presence,
        },
    ] {
        assert_eq!(Row::decode_at(&local, &q.encode()), Err(RowError::NoCodec));
    }
    let mut replica = Replica::new();
    assert_eq!(
        replica.offer(&Subject::Journal { node: n }.to_string(), 1, b"{}"),
        Err(OfferError::Local),
        "a replica never merges what stays on its Mac"
    );
    assert!(matches!(
        replica.offer("/f/alab/pub/n-0100000000000000/s-x/ev", 1, b""),
        Err(OfferError::Subject(_))
    ));
}

// ---------------------------------------------------------------------------
// The merges
// ---------------------------------------------------------------------------

/// One sample an origin recorded.
#[derive(Clone, Copy)]
struct Sample {
    node: Node,
    metric: Metric,
    at_ms: u64,
    value: u64,
}

const T0_MS: u64 = 1_793_000_000_000 - 1_793_000_000_000 % 3_600_000;

const PROPERTY_METRICS: [Metric; 4] = [
    Metric::Tokens(Cli::Claude),
    Metric::Tokens(Cli::Codex),
    Metric::Class(Cli::Claude, Family::Opus, TokenClass::Output),
    Metric::Class(Cli::Codex, Family::Gpt, TokenClass::Input),
];

/// One case of the property: `origins` Macs record samples over three hours; each publishes
/// every window that saw a sample once it closes, and again at revision + 1 for each batch of
/// samples folded late. Returns the samples, every record published (every revision), and the
/// deliveries: every record once, then random duplicates, in a random order.
fn one_case(seed: u64) -> (Vec<Sample>, Vec<WindowRecord>, Vec<WindowRecord>) {
    let mut rng = aterm_spec::rng::XorShift::new(seed);
    let origins = rng.range(1, 9);
    let mut samples = Vec::new();
    let mut published = Vec::new();
    for o in 0..origins {
        let n = node(u8::try_from(o).expect("few origins"));
        // Each origin keeps, per (metric, window), its batches: the first on time, the rest
        // folded after the window closed.
        let mut windows: std::collections::BTreeMap<(Metric, Window), Vec<Vec<u64>>> =
            std::collections::BTreeMap::new();
        for _ in 0..rng.range(1, 60) {
            let metric = *rng.pick(&PROPERTY_METRICS);
            let at_ms = T0_MS + rng.below(3 * 3_600_000);
            let value = match rng.below(4) {
                0 => rng.below(64),
                1 => rng.below(10_000),
                2 => rng.below(1 << 40),
                _ => 0,
            };
            samples.push(Sample {
                node: n,
                metric,
                at_ms,
                value,
            });
            let window = Window::containing(at_ms, metric.width().window_len()).expect("a window");
            let batches = windows.entry((metric, window)).or_default();
            if batches.is_empty() || rng.below(4) == 0 {
                batches.push(Vec::new());
            }
            batches.last_mut().expect("a batch").push(value);
        }
        for ((metric, window), batches) in windows {
            let mut so_far = Summary::new();
            for (revision, batch) in batches.iter().enumerate() {
                for &v in batch {
                    so_far.record(v);
                }
                published.push(
                    WindowRecord::new(
                        n,
                        metric,
                        window.start_ms(),
                        u64::try_from(revision).expect("few revisions"),
                        so_far.clone(),
                    )
                    .expect("an aligned window"),
                );
            }
        }
    }
    let mut deliveries = published.clone();
    for _ in 0..rng.index(published.len() + 1) {
        deliveries.push(rng.pick(&published).clone());
    }
    for i in (1..deliveries.len()).rev() {
        let j = rng.index(i + 1);
        deliveries.swap(i, j);
    }
    (samples, published, deliveries)
}

/// METRICS MERGE IS EXACT (§10): samples spread over 1 to 8 origins and delivered in random
/// order, with duplicates and late revisions, merge to exactly the summary of the union of
/// every sample, per metric and per Mac; each record also survives an encode and a decode at
/// its subject on the way, as a relayed record does.
///
/// NEGATIVE CONTROL: the reader that merges every record it holds, not each identity's
/// latest revision ([`Counters::merging_every_record`]), double-counts whenever a record
/// arrived twice or a window was republished, and the property sees it.
#[test]
fn metrics_merge_is_exact_and_merging_every_record_double_counts() {
    let mut controls_bit = 0usize;
    for seed in 1..=200u64 {
        let (samples, published, deliveries) = one_case(seed);
        let mut exact = Replica::new();
        let mut every = Counters::merging_every_record();
        for (i, r) in deliveries.iter().enumerate() {
            let subject = r.subject();
            let offered = exact
                .offer(
                    &subject.to_string(),
                    u64::try_from(i).expect("small"),
                    &r.encode(),
                )
                .expect("a window record is taken");
            assert_ne!(offered, Offered::Expired);
            every.offer(r.clone());
        }
        let all = 0..u64::MAX;
        for metric in PROPERTY_METRICS {
            let union = Summary::of(
                samples
                    .iter()
                    .filter(|s| s.metric == metric)
                    .map(|s| s.value),
            );
            let merged = exact.counters.summarize(metric, all.clone());
            assert_eq!(merged, union, "seed {seed} {metric}: the merge is exact");
            let per_node = exact.counters.per_node(metric, all.clone());
            assert_eq!(
                per_node
                    .keys()
                    .copied()
                    .collect::<std::collections::BTreeSet<_>>(),
                samples
                    .iter()
                    .filter(|s| s.metric == metric)
                    .map(|s| s.node)
                    .collect(),
                "seed {seed} {metric}: every Mac that recorded it"
            );
            assert_eq!(
                per_node
                    .values()
                    .fold(Summary::new(), |acc, s| acc.merged(s)),
                union,
                "seed {seed} {metric}: the Macs add up to the lab"
            );
            for (n, s) in &per_node {
                let own = Summary::of(
                    samples
                        .iter()
                        .filter(|x| x.metric == metric && x.node == *n)
                        .map(|x| x.value),
                );
                assert_eq!(*s, own, "seed {seed} {metric} {n}: per Mac");
            }
            // Repeats of one identity: a duplicate delivery or a republished window.
            let repeats = deliveries.iter().filter(|r| r.metric == metric).count()
                > published
                    .iter()
                    .filter(|r| r.metric == metric)
                    .map(|r| (r.node, r.window))
                    .collect::<std::collections::BTreeSet<_>>()
                    .len();
            let control = every.summarize(metric, all.clone());
            if repeats {
                assert!(
                    control.count() > union.count(),
                    "seed {seed} {metric}: merging every record counts a repeat twice"
                );
                controls_bit += 1;
            } else {
                assert_eq!(control, union, "seed {seed} {metric}: nothing to double");
            }
        }
        // Windows of one width merge exactly over any span of whole windows too.
        let hour = T0_MS + 3_600_000..T0_MS + 2 * 3_600_000;
        for metric in PROPERTY_METRICS {
            let union = Summary::of(
                samples
                    .iter()
                    .filter(|s| s.metric == metric && hour.contains(&s.at_ms))
                    .map(|s| s.value),
            );
            assert_eq!(
                exact.counters.summarize(metric, hour.clone()),
                union,
                "seed {seed}"
            );
        }
    }
    assert!(
        controls_bit > 100,
        "the negative control bit {controls_bit} times"
    );
}

/// THE QUOTA MERGE NEVER SUMS: across any number of Macs holding one account, the week's
/// figure is the most used current reading, credited to its Mac and time, whatever order the
/// rows arrive in and however many times.
#[test]
fn the_quota_merge_never_sums() {
    let now = 1_793_000_000;
    let reset = now + 86_400;
    for seed in 1..=100u64 {
        let mut rng = aterm_spec::rng::XorShift::new(seed);
        let macs = rng.range(1, 9);
        let mut rows = Vec::new();
        for m in 0..macs {
            let pct = u16::try_from(rng.below(101)).expect("a percent");
            let at = now - rng.below(3_600);
            rows.push((
                node(u8::try_from(m).expect("few")),
                quota_row(at, Some(pct), Some(reset)),
            ));
        }
        let mut s = Snapshots::new();
        let mut order: Vec<usize> = (0..rows.len()).chain(0..rows.len()).collect();
        for i in (1..order.len()).rev() {
            let j = rng.index(i + 1);
            order.swap(i, j);
        }
        for i in order {
            let (n, row) = &rows[i];
            s.offer(row.subject(*n).expect("claude"), 1, &row.encode())
                .expect("a quota row is taken");
        }
        let views = s.accounts(now);
        assert_eq!(views.len(), 1, "one account");
        let most = rows
            .iter()
            .max_by_key(|(n, r)| (r.pct7, r.at, *n))
            .expect("a row");
        assert_eq!(
            views[0].week,
            Some(Usage::Current {
                pct: most.1.pct7.expect("a pct"),
                reset: Some(reset),
                read: most.1.at,
                by: most.0,
            }),
            "seed {seed}"
        );
        assert_eq!(views[0].macs.len(), macs);
    }
}

/// THE SNAPSHOT RULES on an injected clock: a late lower-sequence row never replaces a newer
/// one; each Mac's row shows its age; a window whose every reset passed with nothing read
/// since says so; a reading made since speaks for the new window; a group a whole window
/// older than the newest is the window that ended; a wall that stands, or 90 points, is near
/// a wall.
#[test]
fn the_snapshot_rules_on_an_injected_clock() {
    let now = 1_793_000_000;
    let (a, b) = (node(1), node(2));
    let mut s = Snapshots::new();
    let newer = quota_row(now - 30, Some(64), Some(now + 3_600));
    let older = quota_row(now - 9_000, Some(10), Some(now + 3_600));
    let subject = newer.subject(a).expect("claude");
    assert_eq!(s.offer(subject, 9, &newer.encode()), Ok(Offered::Added));
    assert_eq!(s.offer(subject, 5, &older.encode()), Ok(Offered::Kept));
    assert_eq!(s.offer(subject, 9, &older.encode()), Ok(Offered::Kept));
    assert_eq!(s.get(&subject).map(|(seq, _)| seq), Some(9));
    let view = &s.accounts(now)[0];
    assert_eq!(view.macs[0].age_s, 30);
    assert_eq!(view.week.and_then(Usage::pct), Some(64));
    assert!(!view.near_wall());

    // Every reset passed, nothing read since: "reset at T, not read since", never a 0.
    let later = now + 3_600 + rules_tolerance() + 1;
    let view = &s.accounts(later)[0];
    assert_eq!(
        view.week,
        Some(Usage::ResetUnread {
            reset: now + 3_600,
            read: now - 30,
            by: a
        })
    );
    // A reading made since, its reset not read, speaks for the new window.
    let mut since = quota_row(now + 3_700, Some(2), None);
    since.walled_until = Some(later + 60);
    s.offer(since.subject(b).expect("claude"), 1, &since.encode())
        .expect("taken");
    let view = &s.accounts(later)[0];
    assert_eq!(
        view.week,
        Some(Usage::Current {
            pct: 2,
            reset: None,
            read: now + 3_700,
            by: b
        })
    );
    assert_eq!(view.wall, Some((b, later + 60)), "a wall that stands");
    assert!(view.near_wall());
    assert_eq!(
        view.macs.iter().map(|m| m.node).collect::<Vec<_>>(),
        vec![a, b]
    );

    // Mac a still reads last week at 80 (reset in an hour); Mac b already reads the new week
    // at 5: a's group is the window that ended, and frees nothing for b's.
    let mut s = Snapshots::new();
    let stale = quota_row(now - 60, Some(80), Some(now + 3_600));
    let fresh = quota_row(now - 10, Some(5), Some(now + 3_600 + 7 * 86_400));
    s.offer(stale.subject(a).expect("claude"), 1, &stale.encode())
        .expect("taken");
    s.offer(fresh.subject(b).expect("claude"), 1, &fresh.encode())
        .expect("taken");
    assert_eq!(
        s.accounts(now)[0].week,
        Some(Usage::Current {
            pct: 5,
            reset: Some(now + 3_600 + 7 * 86_400),
            read: now - 10,
            by: b
        })
    );
    // 90 points is near a wall.
    let mut s = Snapshots::new();
    let hot = quota_row(now, Some(90), Some(now + 60));
    s.offer(hot.subject(a).expect("claude"), 1, &hot.encode())
        .expect("taken");
    assert!(s.accounts(now)[0].near_wall());
}

const fn rules_tolerance() -> u64 {
    crate::merge::RESET_TOLERANCE_S
}

/// THE 5-HOUR WINDOW ENDS, on an injected clock: on its reset when the row names one, else a
/// whole window (and the tolerance) after its reading; with nothing read since it is "reset
/// at T, not read since", never a stale figure, so an old 95 keeps no account near a wall.
///
/// NEGATIVE CONTROL: the same 95 read an hour ago, its reset unknown or ahead, is current and
/// near a wall.
#[test]
fn the_five_hour_window_ends_on_its_reset_or_a_window_after_its_reading() {
    let now = 1_793_000_000;
    let a = node(1);
    let view_of = |row: &QuotaRow| {
        let mut s = Snapshots::new();
        s.offer(row.subject(a).expect("claude"), 1, &row.encode())
            .expect("taken");
        let v = &s.accounts(now)[0];
        (v.five_hour, v.near_wall())
    };
    let mut hot = quota_row(now - 3_600, None, None);
    hot.pct5 = Some(95);
    let current = |read, reset| Usage::Current {
        pct: 95,
        reset,
        read,
        by: a,
    };
    assert_eq!(
        view_of(&hot),
        (Some(current(now - 3_600, None)), true),
        "the control: read an hour ago"
    );
    hot.at = now - 6 * 3_600;
    assert_eq!(
        view_of(&hot),
        (
            Some(Usage::ResetUnread {
                reset: now - 3_600,
                read: now - 6 * 3_600,
                by: a
            }),
            false
        ),
        "a whole window after its reading"
    );
    hot.at = now - 3_600;
    hot.five_hour_reset = Some(now + 1_800);
    assert_eq!(
        view_of(&hot),
        (Some(current(now - 3_600, Some(now + 1_800))), true),
        "the control: its reset ahead"
    );
    let passed = now - 1_800 - rules_tolerance();
    hot.five_hour_reset = Some(passed);
    assert_eq!(
        view_of(&hot),
        (
            Some(Usage::ResetUnread {
                reset: passed,
                read: now - 3_600,
                by: a
            }),
            false
        ),
        "past its reset"
    );
}

/// THE OTHER SNAPSHOT VIEWS: each Mac's agy homes, needs and unread pools with their ages, and
/// only the placements that hold now.
#[test]
fn the_other_snapshot_views_show_each_macs_row_with_its_age() {
    let mut r = Replica::new();
    let n = node(4);
    for (subject, row) in every_row() {
        if matches!(row, Row::Window(_)) {
            continue;
        }
        let at = Subject::parse(
            &subject
                .to_string()
                .replace("n-0900000000000000", &n.to_string()),
        )
        .expect("the same subject at another Mac");
        r.offer(&at.to_string(), 1, &row.encode()).expect("taken");
    }
    let s = &r.snapshots;
    assert_eq!(s.agy(1_793_000_200)[0].age_s, 100);
    assert_eq!(
        s.needs(100)[0].age_s,
        23,
        "the one needs row the second offer kept"
    );
    assert_eq!(s.needs(100)[0].row.managers.len(), 1);
    assert_eq!(s.unread(3)[0].row.pools.len(), 2);
    assert_eq!(s.places(500).len(), 1, "borrowed until 1000");
    assert_eq!(s.places(1_000).len(), 0, "the placement ended");
    assert_eq!(s.accounts(0).len(), 2, "claude and kimi");
}

/// TOKENS PER HOUR, per Mac and in all: a Mac's closed hour from its hour records, by family;
/// another's from its minute records, its hour record not yet arrived, never both; the lab's
/// total is their sum, and a quiet Mac's latest window is known.
#[test]
fn tokens_per_hour_read_hours_and_minutes_without_double_counting() {
    let (a, b) = (node(1), node(2));
    let hour = T0_MS;
    let mut c = Counters::new();
    let record = |n, metric, start, values: &[u64]| {
        WindowRecord::new(n, metric, start, 0, Summary::of(values.iter().copied()))
            .expect("aligned")
    };
    // Mac a: one message of opus, 100 in and 40 out, as hour records, and the same message
    // as a minute record, which the hour records take precedence over.
    c.offer(record(
        a,
        Metric::Class(Cli::Claude, Family::Opus, TokenClass::Input),
        hour,
        &[100],
    ));
    c.offer(record(
        a,
        Metric::Class(Cli::Claude, Family::Opus, TokenClass::Output),
        hour,
        &[40],
    ));
    c.offer(record(
        a,
        Metric::Class(Cli::Claude, Family::Opus, TokenClass::CacheRead),
        hour,
        &[0],
    ));
    c.offer(record(
        a,
        Metric::Class(Cli::Claude, Family::Opus, TokenClass::CacheWrite),
        hour,
        &[0],
    ));
    c.offer(record(
        a,
        Metric::Tokens(Cli::Claude),
        hour + 60_000,
        &[140],
    ));
    // Mac b: two Codex turns in two minutes, its hour record not arrived.
    c.offer(record(b, Metric::Tokens(Cli::Codex), hour, &[1_000]));
    c.offer(record(
        b,
        Metric::Tokens(Cli::Codex),
        hour + 120_000,
        &[500],
    ));
    let hours = c.tokens_per_hour(hour..hour + 2 * 3_600_000, hour + 3_600_000);
    assert_eq!(hours.len(), 2);
    let h = &hours[0];
    assert!(h.closed);
    assert_eq!(h.by_node[&a].by_cli[&Cli::Claude], 140);
    assert_eq!(h.by_node[&a].turns[&Cli::Claude], 1);
    assert_eq!(h.by_node[&a].by_family[&(Cli::Claude, Family::Opus)], 140);
    assert!(!h.by_node[&a].from_minutes);
    assert_eq!(h.by_node[&b].by_cli[&Cli::Codex], 1_500);
    assert_eq!(h.by_node[&b].turns[&Cli::Codex], 2);
    assert!(h.by_node[&b].from_minutes && h.by_node[&b].by_family.is_empty());
    assert_eq!(h.total.total(), 1_640);
    assert!(hours[1].by_node.is_empty() && !hours[1].closed);
    assert_eq!(c.latest_window(b, Width::Minute), Some(hour + 120_000));
    assert_eq!(c.latest_window(b, Width::Hour), None);
    // A republished window replaces its earlier revision; an older revision changes nothing.
    let late = WindowRecord::new(
        b,
        Metric::Tokens(Cli::Codex),
        hour,
        1,
        Summary::of([1_000, 7]),
    )
    .expect("aligned");
    assert_eq!(c.offer(late.clone()), Offered::Replaced);
    assert_eq!(
        c.offer(record(b, Metric::Tokens(Cli::Codex), hour, &[1_000])),
        Offered::Kept
    );
    assert_eq!(c.offer(late), Offered::Kept);
    let h = &c.tokens_per_hour(hour..hour + 3_600_000, hour + 3_600_000)[0];
    assert_eq!(h.by_node[&b].by_cli[&Cli::Codex], 1_507);
    assert_eq!(
        c.forget_before(Width::Minute, hour + 3_600_000),
        3,
        "the minute rows of the hour"
    );
    assert_eq!(
        c.offer(record(b, Metric::Tokens(Cli::Codex), hour, &[1])),
        Offered::Expired
    );
    assert_eq!(WindowLen::Hour, Width::Hour.window_len());
}

/// A PARTLY DELIVERED HOUR IS NEVER UNDERCOUNTED: a Mac's hour records arrive one subject at a
/// time, and beside its complete minute totals only some of them hold less than the hour; each
/// copy is a lower bound on the same samples, so the larger counts, tokens and turns apart,
/// and once every hour record is in, the hour records and their families count. Never both.
///
/// NEGATIVE CONTROL: the rule that lets any hour record stand for the hour counts 800 of the
/// 1000 tokens and none of the 8 turns here.
#[test]
fn a_partly_delivered_hour_counts_its_larger_copy() {
    let a = node(1);
    let hour = T0_MS;
    let record = |metric, values: &[u64]| {
        WindowRecord::new(a, metric, hour, 0, Summary::of(values.iter().copied())).expect("aligned")
    };
    let input = Metric::Class(Cli::Claude, Family::Opus, TokenClass::Input);
    let output = Metric::Class(Cli::Claude, Family::Opus, TokenClass::Output);
    let mut c = Counters::new();
    // Eight messages of 100 in and 25 out: the minute totals, then the input class alone.
    c.offer(record(Metric::Tokens(Cli::Claude), &[125; 8]));
    c.offer(record(input, &[100; 8]));
    let read = |c: &Counters| {
        let h = &c.tokens_per_hour(hour..hour + 3_600_000, hour + 3_600_000)[0];
        let t = &h.by_node[&a];
        (
            t.by_cli[&Cli::Claude],
            t.turns[&Cli::Claude],
            t.by_family[&(Cli::Claude, Family::Opus)],
            t.from_minutes,
            h.total.total(),
        )
    };
    assert_eq!(read(&c), (1_000, 8, 800, true, 1_000));
    c.offer(record(output, &[25; 8]));
    assert_eq!(read(&c), (1_000, 8, 1_000, false, 1_000), "never both");
}

// ---------------------------------------------------------------------------
// AS1's highest-sequence rows on a broker
// ---------------------------------------------------------------------------

/// A LATE LOWER-SEQUENCE QUOTA ROW NEVER WINS on a broker retained by these rules. AS1's
/// broker appends an origin's relayed row on a subject only above the sequence of the row the
/// subject holds, on exactly the subjects its rules keep the `Last` row of
/// (`BrokerLog::stage_relay` consults `Retention::keeps_last`). A real broker takes
/// [`rules::rules()`] and runs by them, and by them every quota and `now` subject is such a
/// subject while no counter subject is; the reader orders the same way ([`Snapshots`] by
/// sequence, `the_snapshot_rules_on_an_injected_clock`).
///
/// NEGATIVE CONTROL: a minute summary's subject keeps every record, so the broker would land
/// a late copy there in arrival order: on those subjects only the window record's revision,
/// which the reader keeps the highest of ([`Counters`]), orders versions.
///
/// The other half of the law is astream's own: on any subject its rules keep the `Last` row
/// of, a signed relay at or below the current row's sequence is a duplicate, whichever path
/// brings it first (`federation_relay.rs`:
/// `the_highest_origin_seq_wins_over_two_paths_in_either_order`,
/// `a_late_row_is_answered_at_the_row_that_beat_it_and_within_a_batch`). A relay lands only
/// under astream's `relay` feature with origin keys set, which B7a adopts for the node service
/// and its Macs' relay tests; this crate builds without it, so here a relay is refused as not
/// ready and lands nothing.
#[cfg(unix)]
#[test]
fn a_late_lower_sequence_quota_row_never_wins_on_the_broker() {
    use astream_broker::{Broker, Durability, Transport};
    let dir = aterm_tempfile::Builder::new()
        .prefix("am-last")
        .tempdir_in("/tmp")
        .expect("scratch");
    let broker =
        Broker::open_with(dir.path().join("metrics.log"), Durability::Relaxed).expect("a broker");
    assert_eq!(broker.retention(), None, "no rules before they are set");
    broker
        .set_retention(rules::rules())
        .expect("astream takes the metrics rules");
    let held = broker.retention().expect("the rules it runs by");
    assert_eq!(held, rules::rules());

    let origin = node(0x42);
    let quota = quota_row(2_000, Some(64), Some(9_000))
        .subject(origin)
        .expect("claude");
    let needs = NeedsRow::subject(origin);
    let window = Subject::Window {
        node: origin,
        metric: Metric::Tokens(Cli::Claude),
    };
    assert!(held.keeps_last(&quota.to_string()), "{quota}");
    assert!(held.keeps_last(&needs.to_string()), "{needs}");
    assert!(
        !held.keeps_last(&window.to_string()),
        "the control: {window}"
    );
    for s in every_subject(origin) {
        let text = s.to_string();
        assert_eq!(held.keeps_last(&text), s.class().keeps_last(), "{text}");
        assert_eq!(
            held.keeps_last(&text),
            matches!(s.class(), Class::Quota | Class::Now),
            "{text}"
        );
        // A relayed row of it is its node's, signed by the node's key.
        assert_eq!(
            astream_cap::relay_owner(&text),
            Some(origin.to_string().as_str()),
            "{text}"
        );
    }

    // An unsigned relay on this pin: refused as not ready, and nothing lands.
    let sock = dir.path().join("m.sock");
    let _served = broker.serve(&sock).expect("serve");
    let (mut client, closer) =
        astream_broker::connect(&Transport::Unix, &sock.to_string_lossy(), None).expect("dial");
    closer
        .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
        .expect("a read bound");
    closer
        .set_write_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
        .expect("a write bound");
    let text = quota.to_string();
    let producer = astream_cap::relay_producer_of(&text).expect("a node's subject");
    let body = quota_row(2_000, Some(64), Some(9_000)).encode();
    let refused = client
        .relay(producer, 9, &text, &body)
        .expect_err("no relay lands before the origin keys and their check");
    assert_eq!(
        astream_broker::broker_refusal(&refused).map(|r| r.code),
        Some(astream_broker::proto::ERR_RELAY_NOT_READY),
        "{refused}"
    );
    let (rows, _) = client.last_all(&text).expect("the last face");
    assert!(rows.is_empty(), "nothing landed");
}
