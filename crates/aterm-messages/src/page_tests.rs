// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the Settings Messages page projection and wire lines.

use crate::center::MessageCenter;
use crate::log::{LogLine, LogRecord, MessageLog};
use crate::model::{
    Decision, Glyph, Hold, Intent, Message, MessageId, Meter, Restatement, Severity, Tag,
    UpgradeWord, WallStamp, tags,
};
use crate::page::{
    Host, MessageActionView, MessageView, MessagesClock, MessagesFilter, MessagesState, NOT_SAVED,
    cut_off_mark, description, same_chip, tag_words,
};
use crate::{HOLD_ASK, Instant, Outcome, STALE_TAILED, words};

const NOW: u64 = 1_758_470_000_000;

/// A test host: every fact is a field.
#[derive(Default)]
struct Desk {
    folder: Option<String>,
    unsaved: bool,
    offset_s: i64,
    staged: Option<u64>,
    files: Vec<&'static str>,
    /// `(tab, to, word)`: the words each standing upgrade takes.
    takes: Vec<(&'static str, &'static str, UpgradeWord)>,
    /// The tabs whose upgrade stands at all.
    standing: Vec<&'static str>,
    /// A host that performs no navigation (the web's).
    no_navigation: bool,
}

impl Host for Desk {
    fn now_unix_ms(&self) -> u64 {
        NOW
    }
    fn log_folder(&self) -> Option<String> {
        self.folder.clone()
    }
    fn saved(&self) -> bool {
        !self.unsaved
    }
    fn utc_offset_s(&self) -> i64 {
        self.offset_s
    }
    fn staged_build(&self) -> Option<u64> {
        self.staged
    }
    fn is_regular_file(&self, path: &str) -> bool {
        self.files.contains(&path)
    }
    fn upgrade_takes(&self, tab: &str, to: &str, word: UpgradeWord) -> bool {
        self.takes
            .iter()
            .any(|(t, b, w)| *t == tab && *b == to && *w == word)
    }
    fn upgrade_stands(&self, tab: &str) -> bool {
        self.standing.contains(&tab)
    }
    fn performs_navigation(&self) -> bool {
        !self.no_navigation
    }
    fn sentence_lines(&self, key: Option<&str>, detail: &[String]) -> usize {
        if key == Some("two.lines") {
            2.min(detail.len()).max(1)
        } else {
            1
        }
    }
}

fn stamp(unix_ms: u64) -> WallStamp {
    WallStamp { unix_ms }
}

fn center(now: Instant) -> MessageCenter {
    MessageCenter::new(MessageLog::empty(), now)
}

fn entry_at(id: u64, at_unix_ms: u64) -> MessageView {
    MessageView {
        id,
        at_unix_ms,
        began_unix_ms: at_unix_ms,
        tag: "config".to_string(),
        severity: "warn",
        glyph: '\u{26a0}',
        title: format!("config warning {id}"),
        detail: vec!["aterm.toml: unknown key `foo` at line 12".to_string()],
        state: "folded",
        on_glass: None,
        repeats: 1,
        retired_unix_ms: Some(at_unix_ms + 30_000),
        superseded_by: None,
        answer: None,
        actions: Vec::new(),
        sentences: 1,
    }
}

fn upgrade(tab: &str, to: &str, word: UpgradeWord) -> Intent {
    Intent::AgentUpgrade {
        tab: tab.into(),
        to: to.into(),
        word,
    }
}

#[test]
fn the_projection_is_newest_first_in_the_chips_order_with_the_hosts_facts() {
    let now = Instant::now();
    let mut c = center(now);
    let crash = c
        .post(
            Message::new(
                tags::CRASH,
                Severity::Error,
                "aterm closed unexpectedly last time",
            )
            .line("crash log at /logs/crash-1.log")
            .action(Intent::OpenPath {
                path: "/logs/crash-1.log".into(),
            }),
            stamp(NOW - 5 * 60_000),
            now,
        )
        .id;
    let script = c
        .post(
            Message::new(
                Tag::try_new("deploy").expect("a wire tag"),
                Severity::Info,
                "Deploying the site",
            )
            .hold(Hold::LogOnly),
            stamp(NOW - 4 * 60_000),
            now,
        )
        .id;
    let meter = c
        .post(
            Message::new(tags::TOOLCHAIN, Severity::Info, "Installing ALab tools")
                .line("trust \u{00b7} extracting")
                .action(Intent::OpenSettings {
                    route: "/packages".into(),
                })
                .hold(Hold::Live {
                    stale_after: STALE_TAILED,
                }),
            stamp(NOW - 3 * 60_000),
            now,
        )
        .id;
    let other_script = c
        .post(
            Message::new(
                Tag::try_new("ci").expect("a wire tag"),
                Severity::Warn,
                "CI failed",
            )
            .hold(Hold::LogOnly),
            stamp(NOW - 2 * 60_000),
            now,
        )
        .id;
    let ask = c
        .post(
            Message::new(tags::PRIVACY, Severity::Info, "File access not confirmed")
                .action(Intent::OpenSystemPane {
                    pane: "full-disk-access".into(),
                })
                .action(Intent::NotNow {
                    decision: Decision::FileAccess,
                })
                .hold(Hold::asking(HOLD_ASK)),
            stamp(NOW - 60_000),
            now,
        )
        .id;
    assert!(c.restate(
        meter,
        Restatement {
            detail: Some(vec!["trust \u{00b7} linking".into()]),
            ..Restatement::default()
        },
        now
    ));
    assert!(c.resolve(crash, Outcome::Warn, now));
    c.settle(now, true);
    c.commit_rows(now, 3);

    let desk = Desk {
        folder: Some("/logs".into()),
        offset_s: -25_200,
        files: vec!["/logs/crash-1.log"],
        ..Desk::default()
    };
    let page = MessagesState::of(&c, &desk);
    assert_eq!(page.revision, c.revision());
    assert_eq!(
        page.now_unix_ms, NOW,
        "the host's clock, never the engine's"
    );
    assert_eq!(page.log_folder.as_deref(), Some("/logs"));
    assert!(page.saved);
    assert_eq!(page.utc_offset_s, -25_200);
    let ids: Vec<u64> = page.entries.iter().map(|e| e.id).collect();
    assert_eq!(
        ids,
        [
            ask.raw(),
            other_script.raw(),
            meter.raw(),
            script.raw(),
            crash.raw()
        ],
        "newest first"
    );
    assert_eq!(
        page.tags,
        [
            ("crash".to_string(), 1),
            ("toolchain".to_string(), 1),
            ("privacy".to_string(), 1),
            ("ci".to_string(), 1),
            ("deploy".to_string(), 1),
        ],
        "the chips' fixed order, present tags only, then the unknown ones newest first"
    );
    let m = page.entry(meter.raw()).expect("in the projection");
    assert_eq!(m.state, "live");
    assert_eq!(
        m.detail,
        vec!["trust \u{00b7} linking"],
        "the current words"
    );
    assert_eq!(
        m.on_glass,
        c.glass_position(meter).and_then(|r| u8::try_from(r).ok())
    );
    assert!(m.on_glass.is_some(), "a live row on the band says its row");
    assert_eq!(m.state_words(), "showing now");
    assert_eq!(
        m.actions,
        [MessageActionView {
            index: 0,
            label: "Packages",
            still_actionable: true,
            declines: false,
        }],
        "a navigation is always live"
    );
    let a = page.entry(ask.raw()).expect("the ask");
    assert_eq!(a.state, "held");
    assert!(a.actions.iter().all(|x| x.still_actionable));
    let f = page.entry(crash.raw()).expect("the crash record");
    assert_eq!(f.state, "resolved-warn");
    assert_eq!(f.on_glass, None, "a record is on no row");
    assert!(
        f.actions[0].still_actionable,
        "the host says the log is there"
    );
    let gone = MessagesState::of(&c, &Desk::default());
    assert!(
        !gone.entry(crash.raw()).unwrap().actions[0].still_actionable,
        "no file, no press"
    );
    assert_eq!(gone.log_folder, None);
    let unsaved = MessagesState::of(
        &c,
        &Desk {
            unsaved: true,
            ..Desk::default()
        },
    );
    assert!(!unsaved.saved, "an in-memory ring says so ({NOT_SAVED})");
    assert_eq!(MessagesState::empty().revision, 0);
    assert!(MessagesState::empty().saved && MessagesState::empty().entries.is_empty());
}

#[test]
fn a_story_told_on_is_one_row_in_its_latest_words() {
    let now = Instant::now();
    let mut c = center(now);
    let notice = |title: &str, key: &str, step: &str| {
        Message::new(tags::HARNESS, Severity::Warn, title)
            .line(step)
            .key(key)
            .hold(Hold::STANDING)
    };
    let other = c
        .post(
            Message::new(tags::SYSTEM, Severity::Info, "Font installed").hold(Hold::LogOnly),
            stamp(NOW - 9 * 60_000),
            now,
        )
        .id;
    let si = c
        .post(
            notice("Login expired in tab 1", "n.s-1.si", "Finish signing in"),
            stamp(NOW - 8 * 60_000),
            now,
        )
        .id;
    let si2 = c
        .post(
            notice(
                "Login expired in tab 1",
                "n.s-1.si",
                "Finish signing in as main",
            ),
            stamp(NOW - 7 * 60_000),
            now,
        )
        .id;
    assert_eq!(
        c.log().get(si).and_then(LogRecord::retired),
        Some(&crate::Retired::Superseded { by: si2 }),
        "the center's own keyed supersede"
    );
    c.commit_rows(now, 3);
    assert!(c.mark_seen(si2, now));
    let ca = c
        .post(
            notice("Sign-in cancelled in tab 1", "n.s-1.ca", "Type /login"),
            stamp(NOW - 6 * 60_000),
            now,
        )
        .id;
    assert!(c.supersede_record(si2, ca, now), "a read record told on");
    let page = MessagesState::of(&c, &Desk::default());
    let ids: Vec<u64> = page.entries.iter().map(|e| e.id).collect();
    assert_eq!(
        ids,
        [ca.raw(), other.raw()],
        "the story once, and the other fact"
    );
    let row = page.entry(ca.raw()).expect("the story");
    assert_eq!(row.title, "Sign-in cancelled in tab 1", "its latest words");
    assert_eq!(row.state, "live");
    assert_eq!(
        page.tags,
        [("harness".to_string(), 1), ("system".to_string(), 1)],
        "counted once"
    );
    assert_eq!(
        page.told_on,
        [(si2.raw(), ca.raw()), (si.raw(), ca.raw())],
        "newest first, each to the story's row"
    );
    assert_eq!(page.standing_for(si.raw()), Some(ca.raw()));
    assert_eq!(page.standing_for(other.raw()), None, "its own entry");
    let late = c
        .post(
            notice("Login expired in tab 2", "n.s-2.si", "Finish signing in"),
            stamp(NOW - 60_000),
            now,
        )
        .id;
    assert!(c.withdraw(late, now));
    let mut ring = MessageLog::empty();
    let mut head = c.log().get(si).expect("the record").clone();
    head.state = crate::LogState::Posted;
    head.retired_unix_ms = None;
    ring.replay_all([
        LogLine::Posted(head),
        LogLine::Retired {
            id: si,
            how: crate::Retired::Superseded { by: si2 },
            unix_ms: NOW - 7 * 60_000,
            title: "Login expired in tab 1".into(),
            detail: vec!["Finish signing in".into()],
            repeats: 1,
            mark: None,
        },
    ]);
    let alone = MessageCenter::new(ring, now);
    let page = MessagesState::of(&alone, &Desk::default());
    let row = page
        .entry(si.raw())
        .expect("its successor is gone: its own row");
    assert_eq!(row.state_words(), "replaced");
    assert!(page.told_on.is_empty(), "it stands for itself");
    let mut ring = MessageLog::empty();
    let mut back = c.log().get(late).expect("the record").clone();
    back.state = crate::LogState::Posted;
    back.retired_unix_ms = None;
    ring.replay_all([
        LogLine::Posted(c.log().get(other).expect("the record").clone()),
        LogLine::Posted(back),
        LogLine::Retired {
            id: late,
            how: crate::Retired::Superseded { by: other },
            unix_ms: NOW - 60_000,
            title: "Login expired in tab 2".into(),
            detail: vec!["Finish signing in".into()],
            repeats: 1,
            mark: None,
        },
    ]);
    let back = MessageCenter::new(ring, now);
    let page = MessagesState::of(&back, &Desk::default());
    let ids: Vec<u64> = page.entries.iter().map(|e| e.id).collect();
    assert_eq!(ids, [late.raw(), other.raw()], "told back: both rows");
}

#[test]
fn a_failure_is_told_on_only_by_a_failure() {
    let now = Instant::now();
    let mut c = center(now);
    let row = |severity: Severity, title: &str, key: &str| {
        Message::new(tags::SCRIPT, severity, title)
            .line("linker error")
            .key(key)
            .hold(Hold::STANDING)
    };
    let failed = c
        .post(
            row(Severity::Error, "Build failed", "build"),
            stamp(NOW - 3 * 60_000),
            now,
        )
        .id;
    let retry = c
        .post(
            row(Severity::Info, "Building again", "build"),
            stamp(NOW - 2 * 60_000),
            now,
        )
        .id;
    assert_eq!(
        c.log().get(failed).and_then(LogRecord::retired),
        Some(&crate::Retired::Superseded { by: retry }),
        "premise: the center's own keyed supersede"
    );
    assert!(c.resolve(retry, Outcome::Ok, now));
    let page = MessagesState::of(&c, &Desk::default());
    let ids: Vec<u64> = page.entries.iter().map(|e| e.id).collect();
    assert_eq!(ids, [retry.raw(), failed.raw()], "both rows");
    let error = page.entry(failed.raw()).expect("the failure");
    assert_eq!(
        (error.severity, error.state_words().as_str()),
        ("error", "replaced")
    );
    assert_eq!(page.severity_counts(None), (2, 1), "in Problems");
    assert!(page.told_on.is_empty());

    let mut c = center(now);
    let first = c
        .post(
            row(Severity::Error, "Disk full", "disk"),
            stamp(NOW - 3 * 60_000),
            now,
        )
        .id;
    let again = c
        .post(
            row(Severity::Error, "Disk still full", "disk").finished_as("Disk has room"),
            stamp(NOW - 2 * 60_000),
            now,
        )
        .id;
    let page = MessagesState::of(&c, &Desk::default());
    let ids: Vec<u64> = page.entries.iter().map(|e| e.id).collect();
    assert_eq!(ids, [again.raw()], "one failure, one row");
    assert!(c.resolve(again, Outcome::Ok, now));
    assert_eq!(
        c.log().get(again).map(|r| r.severity),
        Some(Severity::Success),
        "premise: it reads fixed now"
    );
    let page = MessagesState::of(&c, &Desk::default());
    let ids: Vec<u64> = page.entries.iter().map(|e| e.id).collect();
    assert_eq!(ids, [again.raw()], "still one row once fixed");
    assert_eq!(page.standing_for(first.raw()), Some(again.raw()));
    let mut ring = MessageLog::empty();
    ring.replay_all(
        c.log()
            .pending_lines()
            .map(|line| LogLine::decode(&line.encode()).expect("a line")),
    );
    let relaunched = MessagesState::of(&MessageCenter::new(ring, now), &Desk::default());
    let ids: Vec<u64> = relaunched.entries.iter().map(|e| e.id).collect();
    assert_eq!(ids, [again.raw()], "replayed: one row");

    let mut c = center(now);
    let _warned = c
        .post(
            row(Severity::Warn, "Build has warnings", "build"),
            stamp(NOW - 3 * 60_000),
            now,
        )
        .id;
    let retry = c
        .post(
            row(Severity::Info, "Building again", "build"),
            stamp(NOW - 2 * 60_000),
            now,
        )
        .id;
    let page = MessagesState::of(&c, &Desk::default());
    let ids: Vec<u64> = page.entries.iter().map(|e| e.id).collect();
    assert_eq!(ids, [retry.raw()], "one row");

    for (posted, current) in [
        (Severity::Error, Severity::Warn),
        (Severity::Error, Severity::Info),
        (Severity::Info, Severity::Error),
        (Severity::Error, Severity::Error),
    ] {
        let mut parent = center(now);
        let first = parent
            .post(
                row(Severity::Error, "First failure", "handoff.failure"),
                stamp(NOW - 3 * 60_000),
                now,
            )
            .id;
        let latest = parent
            .post(
                row(posted, "Latest reading", "handoff.failure"),
                stamp(NOW - 2 * 60_000),
                now,
            )
            .id;
        assert!(parent.restate(
            latest,
            Restatement {
                severity: Some(current),
                ..Restatement::default()
            },
            now,
        ));
        parent.commit_rows(now, 3);
        let before = MessagesState::of(&parent, &Desk::default());
        let expected = if posted == Severity::Error {
            vec![latest.raw()]
        } else {
            vec![latest.raw(), first.raw()]
        };
        let ids = |page: &MessagesState| page.entries.iter().map(|e| e.id).collect::<Vec<_>>();
        assert_eq!(ids(&before), expected);
        let carried = parent.carried();
        let live = carried
            .live
            .iter()
            .find(|m| m.id == latest.raw())
            .expect("carried");
        assert_eq!(live.severity, current.as_str());
        assert_eq!(
            live.posted_severity.as_deref(),
            (posted != current).then_some(posted.as_str()),
            "unchanged severity needs no extra wire field"
        );
        let mut disk = MessageLog::empty();
        disk.replay_all(
            parent
                .log()
                .pending_lines()
                .map(|line| LogLine::decode(&line.encode()).expect("the original lines")),
        );
        for legacy in [false, true] {
            let mut carry = carried.clone();
            if legacy {
                for m in &mut carry.live {
                    m.posted_severity = None;
                }
            }
            let mut child = center(now);
            child.seed_carried(&carry, stamp(NOW), now);
            let seeded = child.log().get(latest).expect("same carried id");
            assert_eq!(seeded.severity, current);
            assert_eq!(
                seeded.posted_severity,
                if legacy { current } else { posted }
            );
            child.merge_reloaded_log(&disk);
            let loaded = child.log().get(latest).expect("same id after reload");
            assert_eq!(loaded.stamp, parent.log().get(latest).unwrap().stamp);
            assert_eq!(
                loaded.severity, current,
                "reload leaves its live mark alone"
            );
            assert_eq!(loaded.posted_severity, posted, "original post recovered");
            let after = MessagesState::of(&child, &Desk::default());
            assert_eq!(ids(&after), expected, "carry/reload preserves grouping");
            assert_eq!(after.told_on, before.told_on, "same record identities");
            assert_eq!(
                after.standing_for(first.raw()),
                (posted == Severity::Error).then_some(latest.raw())
            );
        }
    }
}

#[test]
fn a_story_told_on_starts_when_its_first_record_did() {
    const MIN: u64 = 60_000;
    let now = Instant::now();
    let mut c = center(now);
    let notice = |title: &str, key: &str| {
        Message::new(tags::HARNESS, Severity::Warn, title)
            .line("Finish signing in")
            .key(key)
            .hold(Hold::STANDING)
    };
    let start = NOW - 50 * MIN;
    let other = c
        .post(
            Message::new(tags::SYSTEM, Severity::Info, "Font installed").hold(Hold::LogOnly),
            stamp(start + 10 * MIN),
            now,
        )
        .id;
    let si = c
        .post(notice("Login expired", "n.s-1.si"), stamp(start), now)
        .id;
    let ca = c
        .post(
            notice("Sign-in cancelled", "n.s-1.ca"),
            stamp(start + 40 * MIN),
            now,
        )
        .id;
    assert!(c.supersede_record(si, ca, now));
    assert!(c.withdraw(ca, now));
    assert!(c.fix_record(ca, "Login back", stamp(start + 45 * MIN), now));
    let page = MessagesState::of(&c, &Desk::default());
    let row = page.entry(ca.raw()).expect("the story");
    assert_eq!(
        (row.at_unix_ms, row.began_unix_ms),
        (start + 40 * MIN, start),
        "its own stamp, its story's start"
    );
    assert_eq!(row.state_words(), "took 45 min");
    assert_eq!(
        row.meta_words(NOW, 0),
        format!("{} \u{00b7} took 45 min", words::local_words(NOW, start, 0))
    );
    let own = page.entry(other.raw()).expect("its own row");
    assert_eq!(own.began_unix_ms, own.at_unix_ms);
}

#[test]
fn install_now_follows_the_hosts_staged_build_and_the_sentence_is_the_hosts() {
    let now = Instant::now();
    let mut c = center(now);
    let staged = c
        .post(
            Message::new(tags::UPDATE, Severity::Success, "aterm 0.91.0 is ready")
                .line("build 1234 \u{2014} verified and ready to apply")
                .line("it installs on a press")
                .line("sha 0123")
                .key("two.lines")
                .action(Intent::ApplyUpdate { build: 1234 })
                .action(Intent::OpenSettings {
                    route: "/updates".into(),
                })
                .hold(Hold::STANDING),
            stamp(NOW - 60_000),
            now,
        )
        .id;
    let install = |page: &MessagesState| {
        page.entry(staged.raw())
            .unwrap()
            .actions
            .iter()
            .find(|a| a.label == "Install now")
            .expect("a staged row offers Install now")
            .still_actionable
    };
    assert!(
        !install(&MessagesState::of(&c, &Desk::default())),
        "nothing staged"
    );
    assert!(
        !install(&MessagesState::of(
            &c,
            &Desk {
                staged: Some(1235),
                ..Desk::default()
            }
        )),
        "another build staged"
    );
    let page = MessagesState::of(
        &c,
        &Desk {
            staged: Some(1234),
            ..Desk::default()
        },
    );
    assert!(install(&page), "that build staged");
    assert_eq!(page.entry(staged.raw()).unwrap().sentences, 2);
    let answered = c
        .post(
            Message::new(tags::PRIVACY, Severity::Info, "Allow Full Disk Access?")
                .action(Intent::NotNow {
                    decision: Decision::FileAccess,
                })
                .hold(Hold::asking(HOLD_ASK)),
            stamp(NOW - 30_000),
            now,
        )
        .id;
    assert_eq!(
        c.act(answered, crate::ActionIndex(0), now),
        Some(Intent::NotNow {
            decision: Decision::FileAccess
        })
    );
    let page = MessagesState::of(&c, &Desk::default());
    let ans = page.entry(answered.raw()).unwrap();
    assert_eq!(ans.state, "answered");
    assert_eq!(ans.state_words(), "answered: Not now");
    assert!(ans.actions.is_empty(), "{:?}", ans.actions);
    assert_eq!(ans.sentences, 1);
}

#[test]
fn a_word_for_a_session_that_is_gone_is_not_offered() {
    let now = Instant::now();
    let mut c = center(now);
    let tab = c
        .post(
            Message::new(
                tags::HARNESS,
                Severity::Warn,
                "Claude upgrade waits in tab 1",
            )
            .action(upgrade("s-1", "2.1.282", UpgradeWord::Now))
            .action(upgrade("s-1", "2.1.282", UpgradeWord::NotToday))
            .hold(Hold::STANDING),
            stamp(NOW - 60_000),
            now,
        )
        .id;
    let page = MessagesState::of(&c, &Desk::default());
    let words_of = |page: &MessagesState, id: MessageId| {
        page.entry(id.raw())
            .unwrap()
            .actions
            .iter()
            .map(|a| (a.index, a.still_actionable))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        words_of(&page, tab),
        [(0, false), (1, false)],
        "a live row keeps its words"
    );
    assert!(c.resolve(tab, Outcome::Ok, now));
    let page = MessagesState::of(&c, &Desk::default());
    assert!(words_of(&page, tab).is_empty());
    let page = MessagesState::of(
        &c,
        &Desk {
            standing: vec!["s-1"],
            takes: vec![("s-1", "2.1.282", UpgradeWord::NotToday)],
            ..Desk::default()
        },
    );
    assert_eq!(words_of(&page, tab), [(0, false), (1, true)]);
}

#[test]
fn a_navigation_is_pressable_only_where_the_host_performs_one() {
    let now = Instant::now();
    let mut c = center(now);
    let crash = c
        .post(
            Message::new(tags::CRASH, Severity::Error, "aterm closed unexpectedly")
                .action(Intent::OpenPath {
                    path: "/logs/crash-1.log".into(),
                })
                .action(Intent::OpenSettings {
                    route: "/manual".into(),
                })
                .hold(Hold::LogOnly),
            stamp(NOW - 90_000),
            now,
        )
        .id;
    let config = c
        .post(
            Message::new(
                tags::CONFIG,
                Severity::Warn,
                "aterm.toml has an unknown key",
            )
            .action(Intent::OpenConfigEditor { line: Some(12) })
            .action(Intent::NewWindow)
            .hold(Hold::LogOnly),
            stamp(NOW - 60_000),
            now,
        )
        .id;
    let ask = c
        .post(
            Message::new(tags::PRIVACY, Severity::Info, "Allow Full Disk Access?")
                .action(Intent::NotNow {
                    decision: Decision::FileAccess,
                })
                .action(Intent::OpenSystemPane {
                    pane: "Privacy_AllFiles".into(),
                })
                .hold(Hold::asking(HOLD_ASK)),
            stamp(NOW - 30_000),
            now,
        )
        .id;
    let pressable = |page: &MessagesState, id: MessageId| {
        page.entry(id.raw())
            .unwrap()
            .actions
            .iter()
            .map(|a| (a.label, a.still_actionable))
            .collect::<Vec<_>>()
    };
    let performs = Desk {
        files: vec!["/logs/crash-1.log"],
        ..Desk::default()
    };
    let page = MessagesState::of(&c, &performs);
    assert_eq!(
        pressable(&page, crash),
        [("Open log", true), ("Manual", true)]
    );
    assert_eq!(
        pressable(&page, config),
        [("Open aterm.toml", true), ("New window", true)]
    );
    assert_eq!(
        pressable(&page, ask),
        [("Not now", true), ("Open Settings", true)]
    );
    let none = Desk {
        no_navigation: true,
        ..performs
    };
    let page = MessagesState::of(&c, &none);
    assert_eq!(
        pressable(&page, crash),
        [("Open log", true), ("Manual", false)]
    );
    assert_eq!(
        pressable(&page, config),
        [("Open aterm.toml", false), ("New window", false)]
    );
    assert_eq!(
        pressable(&page, ask),
        [("Not now", true), ("Open Settings", false)],
        "a live row's navigation too; its decision keeps its own rule"
    );
    let lines = crate::page::wire_lines(&page, &MessagesFilter::default());
    assert!(
        lines.contains(&format!(
            "action\tid={}\tindex=1\tlabel=Manual\tactionable=0\tbutton=Open Manual\tprimary=0",
            crash.raw()
        )),
        "{lines:#?}"
    );
}

#[test]
fn a_scripts_tag_never_reads_as_one_of_aterms_chips() {
    for (script, own) in [
        ("updates", "update"),
        ("crashes", "crash"),
        ("sessions", "session"),
        ("windows", "window"),
        ("display", "render"),
        ("accessibility", "a11y"),
        ("agents", "fabric"),
        ("agents", "harness"),
        ("scripts", "script"),
    ] {
        assert_eq!(tag_words(script), script, "{script}");
        assert!(!same_chip(script, own), "{script} under {own}'s chip");
    }
    for own in tags::ALL.iter().map(|t| t.as_str()) {
        let spelled = tag_words(own).to_lowercase();
        if Tag::try_new(&spelled).is_ok() && !tags::ALL.iter().any(|t| t.as_str() == spelled) {
            assert_ne!(tag_words(&spelled), tag_words(own), "{spelled}");
        }
    }
    assert_eq!(tag_words("search"), "Search");
    assert_eq!(tag_words("ci"), "CI");
    assert!(same_chip("fabric", "harness") && same_chip("toolchain", "packages"));
    let now = Instant::now();
    let mut c = center(now);
    let ours = c
        .post(
            Message::new(tags::UPDATE, Severity::Info, "aterm is up to date").hold(Hold::LogOnly),
            stamp(NOW - 3_600_000),
            now,
        )
        .id;
    let theirs = c
        .post(
            Message::new(
                Tag::try_new("updates").unwrap(),
                Severity::Error,
                "Install failed",
            )
            .hold(Hold::LogOnly),
            stamp(NOW - 1_800_000),
            now,
        )
        .id;
    let page = MessagesState::of(&c, &Desk::default());
    let chips: Vec<(String, String, usize)> = page
        .chips(false)
        .into_iter()
        .map(|chip| (chip.tag, chip.words, chip.count))
        .collect();
    assert_eq!(
        chips,
        [
            ("update".to_string(), "Updates".to_string(), 1),
            ("updates".to_string(), "updates".to_string(), 1),
        ]
    );
    let updates = MessagesFilter {
        tag: Some("update".into()),
        warn_only: false,
    };
    let admitted: Vec<u64> = page
        .entries
        .iter()
        .filter(|e| updates.admits(e))
        .map(|e| e.id)
        .collect();
    assert_eq!(admitted, [ours.raw()]);
    assert_eq!(page.severity_counts(Some("update")), (1, 0));
    assert!(
        description(page.entry(theirs.raw()).unwrap(), NOW).contains(", updates,"),
        "spoken as the script wrote its tag"
    );
}

#[test]
fn a_replayed_open_record_is_cut_off_and_a_silent_one_blames_its_reporter() {
    let dead = LogRecord::from_posted(
        MessageId::from_raw(900).expect("an id"),
        stamp(1_000),
        &Message::new(tags::SYSTEM, Severity::Info, "Rendering the video")
            .glyph(Glyph::or_fallback('\u{21bb}')),
    );
    let mut log = MessageLog::empty();
    log.replay(LogLine::Posted(dead));
    let replayed = log.records().next().expect("replayed");
    let view = MessageView::of(replayed, None, None, None, &Desk::default());
    assert_eq!(view.state, "stale");
    assert_eq!(view.state_words(), "still open when aterm stopped");
    assert_eq!(view.glyph, '\u{2139}', "the replayed open record's mark");
    assert_eq!(
        cut_off_mark(replayed.glyph, Severity::Info, "live").ch(),
        '\u{21bb}'
    );
    assert_eq!(
        cut_off_mark(Severity::Warn.default_glyph(), Severity::Warn, "stale").ch(),
        '\u{26a0}'
    );
    let mut silent = replayed.clone();
    silent.retired_unix_ms = Some(61_000);
    assert_eq!(
        MessageView::of(&silent, None, None, None, &Desk::default()).state_words(),
        "stopped reporting after 1 min"
    );
    let now = Instant::now();
    let mut c = center(now);
    let render = c
        .post(
            Message::new(tags::SYSTEM, Severity::Info, "Rendering the video")
                .glyph(Glyph::or_fallback('\u{21bb}'))
                .hold(Hold::Live {
                    stale_after: STALE_TAILED,
                })
                .meter(Meter {
                    fill_permille: Some(283),
                    ..Meter::default()
                }),
            stamp(NOW - 60_000),
            now,
        )
        .id;
    assert_eq!(c.quit(now), 1);
    let page = MessagesState::of(&c, &Desk::default());
    let r = page.entry(render.raw()).expect("the render's record");
    assert_eq!(r.state, "quit");
    assert_eq!(r.glyph, '\u{2139}');
    assert!(
        r.state_words().starts_with("still open when aterm quit"),
        "{}",
        r.state_words()
    );
}

#[test]
fn the_filter_matches_by_chip_and_the_count_line_says_what_it_admits() {
    let fabric = MessageView {
        tag: "fabric".into(),
        severity: "info",
        ..entry_at(1, NOW - 6 * 3_600_000)
    };
    let harness_warn = MessageView {
        tag: "harness".into(),
        ..entry_at(2, NOW - 60_000)
    };
    let all = MessagesFilter::default();
    assert!(all.is_all() && all.admits(&fabric) && all.admits(&harness_warn));
    let agents = MessagesFilter {
        tag: Some("harness".into()),
        warn_only: false,
    };
    assert!(!agents.is_all());
    assert!(agents.admits(&fabric), "one Agents chip");
    let problems = MessagesFilter {
        tag: None,
        warn_only: true,
    };
    assert!(!problems.is_all());
    assert!(!problems.admits(&fabric) && problems.admits(&harness_warn));
    let other = MessagesFilter {
        tag: Some("update".into()),
        warn_only: false,
    };
    assert!(!other.admits(&fabric));
    assert!(same_chip("toolchain", "packages") && !same_chip("toolchain", "update"));
}

#[test]
fn a_stamp_from_tomorrow_is_todays() {
    const DAY: u64 = 86_400_000;
    let now = NOW;
    let mut entries: Vec<MessageView> = (38..=40).rev().map(|id| entry_at(id, now)).collect();
    entries[0].at_unix_ms = now + 9 * 3_600_000;
    entries[1].at_unix_ms = now - 60_000;
    entries[2].at_unix_ms = now - 120_000;
    let refs: Vec<&MessageView> = entries.iter().collect();
    let today = words::local_day(now, 0);
    let clock = MessagesClock::of(&refs, now, 0);
    assert!(
        !clock.headed,
        "a list of today and tomorrow carries no header"
    );
    assert_eq!(clock.sections(&refs), None);

    entries[2].at_unix_ms = now - 3 * DAY;
    let refs: Vec<&MessageView> = entries.iter().collect();
    let clock = MessagesClock::of(&refs, now, 0);
    assert!(clock.headed, "a past entry heads it");
    assert_eq!(
        clock.sections(&refs),
        Some(vec![today, today, today - 3]),
        "tomorrow's stamp is in today's section"
    );
}

#[test]
fn an_old_open_exception_keeps_its_own_day_header() {
    const DAY: u64 = 86_400_000;
    let today = words::local_day(NOW, 0);
    let exception = MessageView {
        state: "review",
        ..entry_at(1, NOW - DAY)
    };
    let record = entry_at(2, NOW - 60_000);
    let refs = [&exception, &record];
    let clock = MessagesClock::of(&refs, NOW, 0);
    assert!(clock.headed);
    assert_eq!(clock.sections(&refs), Some(vec![today - 1, today]));
    let page = MessagesState {
        revision: 1,
        now_unix_ms: NOW,
        entries: vec![exception.clone(), record.clone()],
        told_on: Vec::new(),
        tags: vec![("config".to_string(), 2)],
        log_folder: None,
        saved: false,
        utc_offset_s: 0,
    };
    let lines = crate::page::wire_lines(&page, &MessagesFilter::default());
    let days: Vec<(String, Vec<(String, String)>)> = lines
        .iter()
        .map(|l| read_page_line(l))
        .filter(|(k, _)| k == "day")
        .collect();
    let words: Vec<&str> = days
        .iter()
        .filter_map(|(_, f)| f.iter().find(|(k, _)| k == "words"))
        .map(|(_, v)| v.as_str())
        .collect();
    assert_eq!(words, ["Yesterday", "Today"], "{lines:?}");

    let older = entry_at(3, NOW - DAY - 60_000);
    let refs = [&exception, &record, &older];
    assert_eq!(
        MessagesClock::of(&refs, NOW, 0).sections(&refs),
        Some(vec![today - 1, today, today - 1]),
        "the log under the open run heads its own Yesterday"
    );

    let plain = MessageView {
        state: "folded",
        ..exception.clone()
    };
    let refs = [&plain, &record];
    assert_eq!(
        MessagesClock::of(&refs, NOW, 0).sections(&refs),
        Some(vec![today - 1, today - 1]),
        "out-of-order plain records keep one header a day"
    );
}

#[test]
fn a_headed_list_says_the_local_time_of_a_past_row() {
    const DAY: u64 = 86_400_000;
    let today = entry_at(1, NOW - 3 * 3_600_000);
    let yesterday = entry_at(2, NOW - DAY);
    let clock = MessagesClock::of(&[&today, &yesterday], NOW, 0);
    assert!(clock.headed);
    assert_eq!(clock.when(today.at_unix_ms), "3 h ago");
    assert_eq!(
        clock.when(yesterday.at_unix_ms),
        words::clock_words(yesterday.at_unix_ms, 0)
    );
    let flat = MessagesClock::of(&[&today], NOW, 0);
    assert!(!flat.headed);
    assert_eq!(flat.when(today.at_unix_ms), "3 h ago");
    let west = MessagesClock::of(&[&today], NOW, -25_200);
    assert_eq!(west.offset_s, -25_200);
    assert_eq!(west.today, words::local_day(NOW, -25_200));
    assert_eq!(west.now_unix_ms, NOW);
}

#[test]
fn copy_text_of_a_view_is_the_engines() {
    let now = Instant::now();
    let mut c = center(now);
    let id = c
        .post(
            Message::new(tags::CONFIG, Severity::Warn, "aterm.toml has a problem")
                .line("unknown key `foo` at line 12")
                .line("the rest of the file applied"),
            stamp(NOW - 60_000),
            now,
        )
        .id;
    assert!(c.resolve(id, Outcome::Warn, now));
    let page = MessagesState::of(&c, &Desk::default());
    let rec = c.log().get(id).unwrap();
    let entry = page.entry(id.raw()).unwrap();
    assert_eq!(entry.copy_text(), words::copy_text(rec));
    assert_eq!(entry.state, crate::wire::state_word(rec, None));
}

fn read_page_line(line: &str) -> (String, Vec<(String, String)>) {
    assert!(!line.contains('\n') && !line.contains('\r'), "{line:?}");
    let mut tokens = line.split('\t');
    let kind = tokens.next().expect("a kind word").to_string();
    let fields = tokens
        .map(|token| {
            let (key, value) = token.split_once('=').expect("key=value");
            (key.to_string(), crate::log::unescape(value))
        })
        .collect();
    (kind, fields)
}

fn page_field<'a>(fields: &'a [(String, String)], key: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

fn page_lines_fixture() -> (MessageCenter, Desk) {
    const DAY: u64 = 86_400_000;
    let now = Instant::now();
    let mut c = center(now);
    for (message, at) in [
        (
            Message::new(
                tags::TOOLCHAIN,
                Severity::Warn,
                "ALab tools could not update",
            )
            .line("the index could not be reached; it tries again in an hour")
            .line("GET /index.toml: 503")
            .hold(Hold::LogOnly),
            NOW - 3 * DAY,
        ),
        (
            Message::new(tags::PACKAGES, Severity::Info, "trust updated")
                .line("0.9.1 is live")
                .hold(Hold::LogOnly),
            NOW - 2 * 3_600_000,
        ),
        (
            Message::new(tags::CRASH, Severity::Error, "aterm closed unexpectedly")
                .line("panicked at src/grid.rs:3:9")
                .line("    ^ index out of bounds")
                .action(Intent::OpenPath {
                    path: "/logs/crash-1.log".into(),
                })
                .hold(Hold::LogOnly),
            NOW - 10 * 60_000,
        ),
        (
            Message::new(
                tags::HARNESS,
                Severity::Warn,
                "Claude upgrade waits in C:\\work",
            )
            .line("it asks again on its own at 4:00 PM")
            .line("Upgrade now asks it at its next turn end")
            .line("in tab 1 \u{00b7} 2.1.281 \u{2192} 2.1.282")
            .key("two.lines")
            .action(upgrade("s-1", "2.1.282", UpgradeWord::Now))
            .action(upgrade("s-1", "2.1.282", UpgradeWord::NotToday))
            .hold(Hold::STANDING),
            NOW - 60_000,
        ),
    ] {
        let _ = c.post(message, stamp(at), now);
    }
    c.settle(now, true);
    c.commit_rows(now, 3);
    let desk = Desk {
        files: vec!["/logs/crash-1.log"],
        takes: vec![("s-1", "2.1.282", UpgradeWord::Now)],
        standing: vec!["s-1"],
        ..Desk::default()
    };
    (c, desk)
}

#[test]
fn the_page_as_lines_says_every_word_in_one_grammar() {
    let (c, desk) = page_lines_fixture();
    let page = MessagesState::of(&c, &desk);
    let lines = crate::page::wire_lines(&page, &MessagesFilter::default());
    let (kind, fields) = lines
        .iter()
        .map(|l| read_page_line(l))
        .find(|(k, f)| k == "entry" && page_field(f, "id") == Some("4"))
        .expect("the agent's entry");
    assert_eq!(kind, "entry");
    assert_eq!(
        page_field(&fields, "title"),
        Some("Claude upgrade waits in C:\\work")
    );
    assert_eq!(
        crate::log::split_us(page_field(&fields, "sentence").unwrap()),
        [
            "it asks again on its own at 4:00 PM",
            "Upgrade now asks it at its next turn end"
        ]
    );
    let wait = page.entry(4).unwrap();
    assert_eq!(
        page_field(&fields, "state_words"),
        Some(&*wait.state_words())
    );
    assert_eq!(page_field(&fields, "copy"), Some(&*wait.copy_text()));
    assert_eq!(page_field(&fields, "meta"), Some(&*wait.meta_words(NOW, 0)));
    assert_eq!(
        page_field(&fields, "description"),
        Some(&*description(wait, NOW))
    );
    let crash = page.entry(3).unwrap();
    assert_eq!(
        crash.sentence_and_technical(),
        (
            Vec::new(),
            vec!["panicked at src/grid.rs:3:9", "    ^ index out of bounds"]
        ),
        "a diagram is technical whole"
    );
}

#[test]
fn the_page_lines_follow_the_filter_and_the_hosts_facts() {
    let (c, desk) = page_lines_fixture();
    let page = MessagesState::of(&c, &desk);
    let alab = crate::page::wire_lines(
        &page,
        &MessagesFilter {
            tag: Some("packages".into()),
            warn_only: false,
        },
    );
    let (_, head) = read_page_line(&alab[0]);
    assert_eq!(page_field(&head, "shown"), Some("2"));
    assert_eq!(page_field(&head, "all"), Some("2"));
    assert_eq!(page_field(&head, "problems"), Some("1"));
    assert_eq!(page_field(&head, "status"), Some("2 of 4"));
    assert_eq!(
        alab[2],
        "chip\ttag=toolchain\twords=ALab tools\tcount=2\ton=1\tlabel=ALab tools \u{00b7} 2",
        "the chip lights by its words"
    );
    assert!(alab[1].contains("\ton=0\t") && alab[3].contains("\ton=0\t"));
    assert_eq!(page_field(&head, "all_tags_on"), Some("0"));
    let ids: Vec<&str> = alab
        .iter()
        .filter_map(|l| l.strip_prefix("entry\tid="))
        .map(|l| l.split('\t').next().unwrap())
        .collect();
    assert_eq!(ids, ["2", "1"]);
    assert_eq!(
        alab.iter().filter(|l| l.starts_with("day\t")).count(),
        2,
        "a past day heads the list"
    );

    let problems = MessagesFilter {
        tag: None,
        warn_only: true,
    };
    let warn = crate::page::wire_lines(&page, &problems);
    let (_, head) = read_page_line(&warn[0]);
    assert_eq!(page_field(&head, "shown"), Some("3"));
    assert_eq!(
        (page_field(&head, "all"), page_field(&head, "problems")),
        (Some("4"), Some("3")),
        "each segment says what a press on it would show"
    );
    assert_eq!(page_field(&head, "status"), Some("3 of 4"));
    assert_eq!(
        warn[2],
        "chip\ttag=toolchain\twords=ALab tools\tcount=1\ton=0\tlabel=ALab tools \u{00b7} 1",
        "a chip counts under the severity that is down"
    );
    assert_eq!(page_field(&head, "all_tags_on"), Some("1"));
    assert_eq!(
        page.chips(true),
        [
            crate::page::Chip {
                tag: "crash".into(),
                words: "Crashes".into(),
                count: 1
            },
            crate::page::Chip {
                tag: "toolchain".into(),
                words: "ALab tools".into(),
                count: 1
            },
            crate::page::Chip {
                tag: "harness".into(),
                words: "Agents".into(),
                count: 1
            },
        ]
    );
    let today = crate::page::wire_lines(
        &page,
        &MessagesFilter {
            tag: Some("crash".into()),
            warn_only: false,
        },
    );
    assert!(
        !today.iter().any(|l| l.starts_with("day\t")),
        "a list of today alone carries no header: {today:?}"
    );
    let (_, entry) = read_page_line(&today[4]);
    assert_eq!(page_field(&entry, "when"), Some("10 min ago"));
    let none = crate::page::wire_lines(
        &page,
        &MessagesFilter {
            tag: Some("update".into()),
            warn_only: false,
        },
    );
    assert_eq!(none.len(), 4, "the head and the chips: {none:?}");
    assert!(none[0].contains("\tshown=0\tall=0\tproblems=0\tstatus=0 of 4"));

    assert!(!alab[0].contains("\tnote=") && !alab[0].contains("\tfolder="));
    let unsaved = MessagesState::of(
        &c,
        &Desk {
            unsaved: true,
            folder: Some("/logs\\aterm".into()),
            ..Desk::default()
        },
    );
    let lines = crate::page::wire_lines(&unsaved, &MessagesFilter::default());
    let (_, head) = read_page_line(&lines[0]);
    assert_eq!(page_field(&head, "note"), Some(NOT_SAVED));
    assert_eq!(page_field(&head, "folder"), Some("/logs\\aterm"));
    assert!(
        lines.contains(
            &"action\tid=3\tindex=0\tlabel=Open log\tactionable=0\tbutton=Open log\tprimary=0"
                .to_string()
        )
    );

    let fresh = center(Instant::now());
    let empty = crate::page::wire_lines(
        &MessagesState::of(&fresh, &Desk::default()),
        &MessagesFilter::default(),
    );
    assert_eq!(empty.len(), 1, "{empty:?}");
    assert!(
        empty[0].starts_with(&format!(
            "page\trev={}\tnow={NOW}\ttotal=0\tshown=0\tall=0\tproblems=0\t",
            fresh.revision()
        )),
        "{}",
        empty[0]
    );
}

#[test]
fn any_word_survives_the_page_lines() {
    let hostile = "a\tb\nc\rd\u{1f}e\\f \\t";
    let entry = MessageView {
        tag: hostile.to_string(),
        title: hostile.to_string(),
        detail: vec![
            "first \\n line".to_string(),
            "second\tline".to_string(),
            "third".to_string(),
        ],
        sentences: 2,
        actions: vec![MessageActionView {
            index: 3,
            label: "Open\tlog\n",
            still_actionable: true,
            declines: false,
        }],
        ..entry_at(9, NOW - 60_000)
    };
    let page = MessagesState {
        revision: 7,
        now_unix_ms: NOW,
        entries: vec![entry.clone()],
        told_on: Vec::new(),
        tags: vec![(hostile.to_string(), 1)],
        log_folder: Some(hostile.to_string()),
        saved: false,
        utc_offset_s: 0,
    };
    let lines = crate::page::wire_lines(&page, &MessagesFilter::default());
    let read: Vec<(String, Vec<(String, String)>)> =
        lines.iter().map(|l| read_page_line(l)).collect();
    let kinds: Vec<&str> = read.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(kinds, ["page", "chip", "entry", "action"]);
    let keys = |k: usize| -> Vec<&str> { read[k].1.iter().map(|(k, _)| k.as_str()).collect() };
    assert_eq!(
        keys(0),
        [
            "rev",
            "now",
            "total",
            "shown",
            "all",
            "problems",
            "status",
            "note",
            "folder",
            "heading",
            "subtitle",
            "all_words",
            "problems_words",
            "all_tags",
            "all_tags_on",
            "tag_menu",
            "tag_menu_name",
            "copy_all_button",
            "copy_all_name",
            "empty",
            "technical_caption",
            "copy_button"
        ]
    );
    assert_eq!(keys(1), ["tag", "words", "count", "on", "label"]);
    assert_eq!(
        keys(2),
        [
            "id",
            "at",
            "when",
            "local",
            "tag",
            "tag_words",
            "sev",
            "sev_words",
            "mark",
            "title",
            "state",
            "state_words",
            "rep",
            "description",
            "sentence",
            "technical",
            "meta",
            "copy"
        ]
    );
    assert_eq!(
        keys(3),
        ["id", "index", "label", "actionable", "button", "primary"]
    );
    let (_, head) = &read[0];
    assert_eq!(page_field(head, "folder"), Some(hostile));
    let (_, chip) = &read[1];
    assert_eq!(page_field(chip, "tag"), Some(hostile));
    assert_eq!(page_field(chip, "words"), Some(&*tag_words(hostile)));
    assert_eq!(
        page_field(chip, "label"),
        Some(&*format!("{} \u{00b7} 1", tag_words(hostile)))
    );
    let (_, e) = &read[2];
    assert_eq!(page_field(e, "title"), Some(hostile));
    assert_eq!(page_field(e, "tag"), Some(hostile));
    let copy = entry.copy_text();
    assert!(
        ['\t', '\n', '\r', '\u{1f}', '\\']
            .iter()
            .all(|c| copy.contains(*c)),
        "{copy:?}"
    );
    assert_eq!(page_field(e, "copy"), Some(copy.as_str()));
    assert_eq!(page_field(e, "meta"), Some(&*entry.meta_words(NOW, 0)));
    let as_list: Vec<String> = copy.split('\n').map(str::to_string).collect();
    assert_ne!(
        crate::log::split_us(&crate::log::join_us(&as_list)).join("\n"),
        copy
    );
    assert_eq!(
        crate::log::split_us(page_field(e, "sentence").unwrap()),
        ["first \\n line", "second\tline"]
    );
    assert_eq!(
        crate::log::split_us(page_field(e, "technical").unwrap()),
        ["third"]
    );
    let (_, a) = &read[3];
    assert_eq!(page_field(a, "label"), Some("Open\tlog\n"));
    assert_eq!(page_field(a, "button"), Some("Open\tlog\n"));
    assert_eq!(page_field(a, "index"), Some("3"));
    assert_eq!(page_field(a, "actionable"), Some("1"));
    let under = crate::page::wire_lines(
        &page,
        &MessagesFilter {
            tag: Some(hostile.to_string()),
            warn_only: false,
        },
    );
    let (_, head) = read_page_line(&under[0]);
    assert_eq!(
        page_field(&head, "tag_menu"),
        Some(&*format!("Tag: {}", tag_words(hostile)))
    );
    assert_eq!(page_field(&head, "all_tags_on"), Some("0"));
    assert!(format!("title={hostile}").split('\t').count() > 1);
}

#[test]
fn copy_all_is_the_build_information_then_each_entrys_copy_text() {
    use crate::page::copy_all_text;
    let newer = entry_at(1, NOW - 60_000);
    let older = entry_at(2, NOW - 120_000);
    assert_eq!(
        copy_all_text("aterm 0.70.0", [&newer, &older]),
        format!(
            "aterm 0.70.0\n\n{}\n\n{}",
            newer.copy_text(),
            older.copy_text()
        )
    );
    assert_eq!(copy_all_text("aterm 0.70.0", []), "aterm 0.70.0");
    assert_eq!(
        copy_all_text("", [&newer]),
        format!("\n\n{}", newer.copy_text())
    );
    assert_eq!(
        copy_all_text("aterm\nversion: 0.70.0\n", [&newer, &older]),
        format!(
            "aterm\nversion: 0.70.0\n\n\n{}\n\n{}",
            newer.copy_text(),
            older.copy_text()
        )
    );
}

#[test]
fn an_open_exception_offers_dismiss_and_the_first_of_several_dismiss_all() {
    use crate::exceptions::{Entry, Family};
    use crate::need::{Reason, Then, Why};
    use crate::page::{DISMISS_ACTION, DISMISS_ALL_ACTION};
    let now = Instant::now();
    let mut c = center(now);
    let entry = |cause: &str| {
        Entry::new(
            Family::Upgrade,
            "",
            cause,
            &format!("Claude is behind ({cause})"),
            Why::OnlyYou(Reason::new("its own work holds the move")),
            Then::step("End that job in the tab"),
        )
        .show(Intent::ShowSession {
            tab: 1,
            window: 1,
            session: "s-01234567890123456789".into(),
        })
        .message()
        .expect("the entry passes the rule")
    };
    c.post(
        Message::new(tags::UPDATE, Severity::Info, "Checked for updates").hold(Hold::LogOnly),
        stamp(NOW),
        now,
    );
    c.sync_digest(
        &Family::Upgrade.prefix(),
        vec![entry("own-work")],
        stamp(NOW + 1_000),
        now,
    );
    let one = MessagesState::of(&c, &Desk::default());
    let open: Vec<&MessageView> = one.entries.iter().filter(|e| e.state == "review").collect();
    assert_eq!(open.len(), 1);
    let indices = |e: &MessageView| e.actions.iter().map(|a| a.index).collect::<Vec<_>>();
    assert_eq!(
        indices(open[0]),
        vec![0, DISMISS_ACTION],
        "own capsule, then Dismiss"
    );
    assert!(DISMISS_ACTION >= 200 && DISMISS_ALL_ACTION > DISMISS_ACTION);
    let plain = one.entries.iter().find(|e| e.state != "review").unwrap();
    assert!(plain.actions.iter().all(|a| a.index < DISMISS_ACTION));

    c.sync_digest(
        &Family::Upgrade.prefix(),
        vec![entry("own-work"), entry("typed-draft")],
        stamp(NOW + 2_000),
        now,
    );
    let two = MessagesState::of(&c, &Desk::default());
    let open: Vec<&MessageView> = two.entries.iter().filter(|e| e.state == "review").collect();
    assert_eq!(open.len(), 2);
    assert_eq!(
        indices(open[0]),
        vec![0, DISMISS_ACTION, DISMISS_ALL_ACTION]
    );
    assert_eq!(indices(open[1]), vec![0, DISMISS_ACTION]);
    let labels: Vec<&str> = open[0].actions.iter().map(|a| a.label).collect();
    assert_eq!(labels[1..], ["Dismiss", "Dismiss all"]);

    c.file_digest(stamp(NOW + 3_000), now);
    let after = MessagesState::of(&c, &Desk::default());
    assert!(after.entries.iter().all(|e| e.state != "review"));
    assert!(
        after
            .entries
            .iter()
            .all(|e| e.actions.iter().all(|a| a.index < DISMISS_ACTION)),
        "a filed entry offers neither"
    );
}

#[test]
fn an_open_exception_stands_first_in_its_current_words() {
    use crate::exceptions::{Entry, Family};
    use crate::need::{Reason, Then, Why};
    let now = Instant::now();
    let mut c = center(now);
    let entry = |what: &str| {
        Entry::new(
            Family::Upgrade,
            "",
            "own-work",
            what,
            Why::OnlyYou(Reason::new("its own work holds the move")),
            Then::step("End that job in the tab"),
        )
        .show(Intent::ShowSession {
            tab: 1,
            window: 1,
            session: "s-01234567890123456789".into(),
        })
        .message()
        .expect("the entry passes the rule")
    };
    let prefix = Family::Upgrade.prefix();
    let s = c.sync_digest(
        &prefix,
        vec![entry("Claude in tab 1 is behind")],
        stamp(NOW),
        now,
    );
    assert_eq!(s.posted, 1);
    c.post(
        Message::new(tags::UPDATE, Severity::Info, "Checked for updates").hold(Hold::LogOnly),
        stamp(NOW + 1_000),
        now,
    );
    let before = MessagesState::of(&c, &Desk::default());
    assert_eq!(before.entries[0].state, "review");
    assert_eq!(before.entries[0].title, "Claude in tab 1 is behind");
    assert!(
        before.entries[0].actions.iter().all(|a| a.still_actionable),
        "an open exception's buttons are offered"
    );
    assert!(before.entries[1..].iter().all(|e| e.state != "review"));
    let s = c.sync_digest(
        &prefix,
        vec![entry("Claude in tab 1 is still behind")],
        stamp(NOW + 2_000),
        now + crate::need::RESTATE_QUIET,
    );
    assert_eq!(s.restated, 1);
    let after = MessagesState::of(&c, &Desk::default());
    assert_eq!(after.entries[0].title, "Claude in tab 1 is still behind");
    assert!(after.revision > before.revision);
    assert_eq!(after.subtitle(), crate::page::SUBTITLE_OPEN_FIRST);
    let head = crate::page::wire_lines(&after, &MessagesFilter::default()).remove(0);
    assert!(
        head.contains(&format!(
            "\tsubtitle={}\t",
            crate::page::SUBTITLE_OPEN_FIRST
        )),
        "{head}"
    );
    let mut plain = center(now);
    plain.post(
        Message::new(tags::UPDATE, Severity::Info, "Checked for updates").hold(Hold::LogOnly),
        stamp(NOW),
        now,
    );
    let plain = MessagesState::of(&plain, &Desk::default());
    assert_eq!(plain.subtitle(), crate::page::SUBTITLE);
}

#[test]
fn the_primary_is_the_first_pressable_and_never_a_promoted_decline() {
    use crate::page::primary_index;
    let act = |index: u8, label: &'static str, still_actionable: bool, declines: bool| {
        MessageActionView {
            index,
            label,
            still_actionable,
            declines,
        }
    };
    assert_eq!(
        primary_index(&[
            act(0, "Upgrade now", true, false),
            act(1, "Skip version", true, true),
            act(2, "Software Update", true, false),
        ]),
        Some(0)
    );
    assert_eq!(
        primary_index(&[
            act(0, "Upgrade now", false, false),
            act(1, "Skip version", true, true),
            act(2, "Software Update", true, false),
        ]),
        Some(2)
    );
    assert_eq!(
        primary_index(&[
            act(0, "Install now", false, false),
            act(1, "Open log", false, false),
            act(2, "Manual", true, false),
        ]),
        Some(2)
    );
    assert_eq!(
        primary_index(&[
            act(0, "Not now", false, true),
            act(1, "Open Settings", true, false),
        ]),
        Some(1)
    );
    assert_eq!(
        primary_index(&[
            act(0, "Upgrade now", false, false),
            act(1, "Skip version", true, true),
        ]),
        None
    );
    assert_eq!(
        primary_index(&[
            act(0, "Upgrade now", false, false),
            act(1, "Not today", true, true),
            act(2, "Skip version", true, true),
        ]),
        None
    );
    assert_eq!(
        primary_index(&[
            act(0, "Not now", true, true),
            act(1, "Open Settings", true, false),
        ]),
        Some(0)
    );
    assert_eq!(
        primary_index(&[
            act(0, "Open log", false, false),
            act(1, "Manual", false, false),
        ]),
        None
    );
    assert_eq!(primary_index(&[]), None);
}

#[test]
fn the_action_lines_carry_the_primary_after_every_older_key() {
    let act = |index: u8, label: &'static str, still_actionable: bool, declines: bool| {
        MessageActionView {
            index,
            label,
            still_actionable,
            declines,
        }
    };
    let entries = vec![
        MessageView {
            actions: vec![
                act(0, "Upgrade now", false, false),
                act(1, "Skip version", true, true),
                act(2, "Software Update", true, false),
            ],
            ..entry_at(3, NOW - 60_000)
        },
        MessageView {
            actions: vec![
                act(0, "Upgrade now", false, false),
                act(1, "Skip version", true, true),
            ],
            ..entry_at(2, NOW - 120_000)
        },
        MessageView {
            actions: vec![
                act(0, "Not now", true, true),
                act(1, "Open Settings", true, false),
            ],
            ..entry_at(1, NOW - 180_000)
        },
    ];
    let page = MessagesState {
        revision: 1,
        now_unix_ms: NOW,
        entries,
        told_on: Vec::new(),
        tags: vec![("config".to_string(), 3)],
        log_folder: None,
        saved: true,
        utc_offset_s: 0,
    };
    let lines = crate::page::wire_lines(&page, &MessagesFilter::default());
    let actions: Vec<&str> = lines
        .iter()
        .map(String::as_str)
        .filter(|line| line.starts_with("action\t"))
        .collect();
    for entry in &page.entries {
        let picked: Vec<usize> = actions
            .iter()
            .map(|line| read_page_line(line).1)
            .filter(|fields| page_field(fields, "id") == Some(&*entry.id.to_string()))
            .enumerate()
            .filter(|(_, fields)| page_field(fields, "primary") == Some("1"))
            .map(|(k, _)| k)
            .collect();
        assert_eq!(
            picked,
            crate::page::primary_index(&entry.actions)
                .into_iter()
                .collect::<Vec<_>>(),
            "entry {}",
            entry.id
        );
    }
}

#[test]
fn the_primary_line_marks_a_position_in_the_offered_intents_not_an_action_index() {
    let act = |index: u8, label: &'static str, still_actionable: bool| MessageActionView {
        index,
        label,
        still_actionable,
        declines: false,
    };
    let page = MessagesState {
        revision: 1,
        now_unix_ms: NOW,
        entries: vec![
            MessageView {
                actions: vec![act(1, "Show tab 2", true), act(2, "Settings", true)],
                ..entry_at(2, NOW - 60_000)
            },
            MessageView {
                actions: vec![
                    act(1, "Install now", false),
                    act(2, "Software Update", true),
                ],
                ..entry_at(1, NOW - 120_000)
            },
        ],
        told_on: Vec::new(),
        tags: vec![("config".to_string(), 2)],
        log_folder: None,
        saved: true,
        utc_offset_s: 0,
    };
    let lines = crate::page::wire_lines(&page, &MessagesFilter::default());
    let actions: Vec<&str> = lines
        .iter()
        .map(String::as_str)
        .filter(|line| line.starts_with("action\t"))
        .collect();
    assert_eq!(
        actions,
        [
            "action\tid=2\tindex=1\tlabel=Show tab 2\tactionable=1\tbutton=Show tab 2\tprimary=1",
            "action\tid=2\tindex=2\tlabel=Settings\tactionable=1\tbutton=Open Settings\tprimary=0",
            "action\tid=1\tindex=1\tlabel=Install now\tactionable=0\tbutton=Install now\tprimary=0",
            "action\tid=1\tindex=2\tlabel=Software Update\tactionable=1\tbutton=Open Software Update\tprimary=1",
        ]
    );
}

#[test]
fn a_zone_that_moves_its_offset_is_owed_a_publish_when_it_moves() {
    const HOUR_S: i64 = 3_600;
    let mut state = MessagesState::empty();
    state.now_unix_ms = NOW;
    state.utc_offset_s = -8 * HOUR_S;
    let at = NOW - 536_000_000;
    state.entries = vec![entry_at(1, at)];
    let midnight = NOW + 58_000_000;
    assert_eq!(
        Some(midnight),
        Some(words::words_change_after(NOW, at, -8 * HOUR_S)),
        "Sep 22 00:00 at UTC-8"
    );
    let zone =
        |moves_at: u64| move |instant: u64| Some(HOUR_S * if instant < moves_at { -8 } else { -7 });
    assert_eq!(state.words_change_at(|_| Some(-8 * HOUR_S)), Some(midnight));
    assert_eq!(state.words_change_at(zone(midnight + 1)), Some(midnight));
    assert_eq!(
        state.words_change_at(|_| None),
        Some(midnight),
        "a zone it cannot read"
    );
    let moves_at = midnight - 16_200_000 + 337;
    let owed = state
        .words_change_at(zone(moves_at))
        .expect("a page with an entry says time");
    assert!(
        (moves_at..moves_at + 1_000).contains(&owed),
        "the second the zone moves, not before and never later: {owed} against {moves_at}"
    );
    assert_ne!(
        words::local_words(owed, at, -7 * HOUR_S),
        words::local_words(owed - 1_000, at, -8 * HOUR_S),
        "the entry's local clock reads otherwise there"
    );
    const DAY_MS: u64 = 86_400_000;
    state.utc_offset_s = 0;
    let future = NOW + 400 * DAY_MS;
    state.entries = vec![entry_at(1, future)];
    let word_deadline = words::words_change_after(NOW, future, 0);
    let moves_at = NOW + 3_600_337;
    let back_at = NOW + 2 * DAY_MS;
    let paired_zone = |instant: u64| {
        Some(if (moves_at..back_at).contains(&instant) {
            HOUR_S
        } else {
            0
        })
    };
    assert_eq!(paired_zone(word_deadline), Some(0));
    let owed = state.words_change_at(paired_zone).unwrap();
    assert!((moves_at..moves_at + 1_000).contains(&owed));
    assert_ne!(
        words::local_words(owed, future, HOUR_S),
        words::local_words(NOW, future, 0),
        "the first offset change changes the future entry's local clock too"
    );
    let later_zone = |instant: u64| {
        Some(
            if (NOW + 30 * DAY_MS..NOW + 210 * DAY_MS).contains(&instant) {
                HOUR_S
            } else {
                0
            },
        )
    };
    assert_eq!(later_zone(word_deadline), Some(0));
    assert_eq!(state.words_change_at(later_zone), Some(NOW + DAY_MS));
    state.entries.clear();
    assert_eq!(state.words_change_at(|_| Some(0)), None, "no time said");
}
