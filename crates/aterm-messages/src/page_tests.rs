// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Settings ▸ Messages page's model proves itself without a host (design
//! ruling 382): a [`Desk`] answers the seam — its clock, its log folder, a
//! staged build, the files that are there, the upgrades that stand and take
//! a word, and the sentence count a key names — and the builder runs over a
//! real [`MessageCenter`]. Moved here from the host with the model: the
//! calendar's `a_stamp_from_tomorrow_is_todays` (from `native_settings`), the
//! cut-off mark's controls and the span words (from `messages_host`'s
//! projection tests, which keep driving the host's facts through `App`).

use crate::center::MessageCenter;
use crate::log::{LogLine, LogRecord, MessageLog};
use crate::model::{
    Decision, Glyph, Hold, Intent, Message, MessageId, Meter, Restatement, Severity, Tag,
    UpgradeWord, WallStamp, tags,
};
use crate::page::{
    Host, MessageActionView, MessageView, MessagesClock, MessagesFilter, MessagesState, NOT_SAVED,
    TAG_ORDER, act_feedback, cut_off_mark, description, same_chip, span_words, status_words,
    tag_words,
};
use crate::{HOLD_ASK, Instant, Outcome, STALE_TAILED, words};

/// The fixtures' wall clock: 2025-09-21 15:53:20 UTC.
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
    /// A host that performs no navigation (the web's, ruling 389).
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
    /// A record keyed `two.lines` says its sentence in two lines; every
    /// other record in one.
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

/// A plain retired entry stamped `at` (the calendar reads only the stamp).
fn entry_at(id: u64, at_unix_ms: u64) -> MessageView {
    MessageView {
        id,
        at_unix_ms,
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

/// THE PROJECTION (design §4.2), built by the engine over a real center:
/// every record newest first in the wire's own state words, the live row's
/// CURRENT words (a restatement is never logged), the band row each live row
/// is on, tag counts in the chips' order with a tag this build does not
/// know after them in first-seen order, and the host's facts — its clock,
/// its log folder, saved, its offset — passed through as it answered them.
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
                .hold(Hold::Ask { for_: HOLD_ASK }),
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

/// `Install now` is offered while its row is up and pressable only while the
/// HOST says that build is still the staged one; a decision is pressable
/// while live and not offered once answered (ruling 265); the sentence count
/// is the host's (ruling 314).
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
                .hold(Hold::Standing),
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
    // A decision answered: its label in the meta words, and not offered again.
    let answered = c
        .post(
            Message::new(tags::PRIVACY, Severity::Info, "Allow Full Disk Access?")
                .action(Intent::NotNow {
                    decision: Decision::FileAccess,
                })
                .hold(Hold::Ask { for_: HOLD_ASK }),
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

/// A WORD FOR A SESSION THAT IS GONE is not offered (ruling 315): on a
/// retired record, an upgrade word whose tab the host no longer has is
/// dropped; one whose upgrade still stands but no longer takes the word is
/// offered disabled; one it takes is pressable. An agent's one row's word
/// stays while ANY tab it named stands, and is pressable while any takes it.
/// A LIVE row keeps every word, gone or not.
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
            .hold(Hold::Standing),
            stamp(NOW - 60_000),
            now,
        )
        .id;
    let group = c
        .post(
            Message::new(
                tags::HARNESS,
                Severity::Warn,
                "Claude upgrade waits in 2 tabs",
            )
            .action(Intent::AgentUpgradeTabs {
                word: UpgradeWord::Skip,
                moves: vec![
                    ("s-1".into(), "2.1.282".into()),
                    ("s-2".into(), "2.1.282".into()),
                ],
            })
            .hold(Hold::Standing),
            stamp(NOW - 30_000),
            now,
        )
        .id;
    // Live: every word offered, pressable only where the host takes it.
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
    assert_eq!(words_of(&page, group), [(0, false)]);
    assert!(c.resolve(tab, Outcome::Ok, now));
    assert!(c.resolve(group, Outcome::Ok, now));
    // Retired, every tab gone: nothing offered.
    let page = MessagesState::of(&c, &Desk::default());
    assert!(words_of(&page, tab).is_empty());
    assert!(words_of(&page, group).is_empty());
    // s-2 stands and takes Skip: the group word stays and is pressable; the
    // tab-1 words are gone.
    let page = MessagesState::of(
        &c,
        &Desk {
            standing: vec!["s-2"],
            takes: vec![("s-2", "2.1.282", UpgradeWord::Skip)],
            ..Desk::default()
        },
    );
    assert!(words_of(&page, tab).is_empty());
    assert_eq!(words_of(&page, group), [(0, true)]);
    // s-1 stands but moved on: its words stay, disabled; one it takes is
    // pressable. The group word stays (s-1 stands), disabled: no tab takes
    // Skip.
    let page = MessagesState::of(
        &c,
        &Desk {
            standing: vec!["s-1"],
            takes: vec![("s-1", "2.1.282", UpgradeWord::NotToday)],
            ..Desk::default()
        },
    );
    assert_eq!(words_of(&page, tab), [(0, false), (1, true)]);
    assert_eq!(words_of(&page, group), [(0, false)]);
}

/// A NAVIGATION IS PRESSABLE WHERE THE HOST PERFORMS ONE (ruling 389): its
/// moment never passes, so on a host that performs navigations — the macOS
/// one, from any record — every `Manual`, `Open aterm.toml`, System Settings
/// pane and `New window` is pressable, live or retired; on a host that
/// performs none (the web's) not one is, while every other intent keeps its
/// own fact: `Open log` follows the file, a live ask's `Not now` stays
/// pressable. The page's lines say the same (`actionable=`). NEGATIVE
/// CONTROL: the performing host's page, where the same capsules read `1`.
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
                .hold(Hold::Ask { for_: HOLD_ASK }),
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

/// A SCRIPT'S TAG NEVER READS AS ONE OF ATERM'S CHIPS (ruling 390): chips
/// merge by their words, so a script's `updates`, capitalized, would have
/// counted, filtered and been spoken as aterm's own Updates — the lane the
/// wire refuses a script (ruling 171). Every aterm chip's words spelled as a
/// script's tag read as the script wrote them, a chip of their own; every
/// other script tag keeps its capitals (`search` → `Search`, `ci` → `CI`).
/// Over a real center: a script's `updates` error and aterm's own `update`
/// record are two chips, the Updates filter admits aterm's alone, and the
/// script's row is spoken as `updates`. NEGATIVE CONTROL: aterm's own pairs
/// still share a chip.
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
    // Every chip's words as a script would tag them, wherever that is a tag
    // and not one of aterm's own.
    for own in TAG_ORDER {
        let spelled = tag_words(own).to_lowercase();
        if Tag::try_new(&spelled).is_ok() && !TAG_ORDER.contains(&spelled.as_str()) {
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
    assert_eq!(
        description(page.entry(theirs.raw()).unwrap(), NOW),
        "Error, updates, 30 minutes ago"
    );
}

/// THE MARK OF WORK CUT OFF (ruling 317, day eight E5), the engine's half of
/// `messages_host::tests::a_row_open_at_quit_is_recorded_as_cut_off_and_a_dead_processes_as_open`
/// (whose controls on the mark moved here with it): a record replayed open
/// from a process that died without an ending says aterm stopped and wears
/// its severity's mark, not the working `↻`; a row THIS process retired for
/// going silent still blames its reporter; a row cut off by a quit says so.
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
    let view = MessageView::of(replayed, None, None, &Desk::default());
    assert_eq!(view.state, "stale");
    assert_eq!(view.state_words(), "still open when aterm stopped");
    assert_eq!(view.glyph, '\u{2139}', "the replayed open record's mark");
    // Control: the same record while live keeps its working mark, and a
    // mark that is not a working one is kept when cut off.
    assert_eq!(
        cut_off_mark(replayed.glyph, Severity::Info, "live").ch(),
        '\u{21bb}'
    );
    assert_eq!(
        cut_off_mark(Severity::Warn.default_glyph(), Severity::Warn, "stale").ch(),
        '\u{26a0}'
    );
    // A row this process retired Stale keeps its words.
    let mut silent = replayed.clone();
    silent.retired_unix_ms = Some(61_000);
    assert_eq!(
        MessageView::of(&silent, None, None, &Desk::default()).state_words(),
        "stopped reporting after 1 min"
    );
    // Quit through the center: the percent it reached, the severity's mark.
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

/// Every state in PLAIN words (design ruling 66): what the row is doing,
/// and once it has left how long it stood — never the wire's word, never
/// `cleared` over a failure, `took` only for delivered work.
#[test]
fn every_state_is_said_in_plain_words() {
    let base = entry_at(1, NOW - 120_000);
    let with = |state: &'static str, severity: &'static str, retired: Option<u64>| MessageView {
        state,
        severity,
        retired_unix_ms: retired,
        ..base.clone()
    };
    let at = base.at_unix_ms;
    let later = Some(at + 40_000);
    let cases: [(MessageView, &str); 20] = [
        (with("live", "info", None), "waiting to show"),
        (
            MessageView {
                on_glass: Some(1),
                ..with("held", "info", None)
            },
            "showing now",
        ),
        (with("folded", "warn", later), "shown for 40 s"),
        (with("folded", "warn", None), "shown"),
        (with("folded", "warn", Some(at)), "recorded"),
        (with("stale", "info", None), "still open when aterm stopped"),
        (with("stale", "info", later), "stopped reporting after 40 s"),
        (
            with("quit", "info", later),
            "still open when aterm quit after 40 s",
        ),
        (
            with("unseen", "info", later),
            "not shown \u{2014} too many at once",
        ),
        (with("superseded", "info", later), "replaced"),
        (with("resolved-ok", "success", later), "took 40 s"),
        (with("resolved-ok", "info", later), "lasted 40 s"),
        (with("withdrawn", "success", None), "finished"),
        (with("withdrawn", "warn", None), "ended"),
        (
            with("resolved-warn", "warn", Some(at + 3 * 3_600_000)),
            "ended with a problem after 3 h",
        ),
        (with("dismissed", "info", later), "dismissed"),
        (with("answered", "info", later), "answered"),
        (
            with("evicted", "info", later),
            "dropped \u{2014} too many at once",
        ),
        (
            with("carried", "info", None),
            "carried over to the updated aterm",
        ),
        (with("future-word", "info", later), "future-word after 40 s"),
    ];
    for (entry, words) in cases {
        assert_eq!(entry.state_words(), words, "{}", entry.state);
    }
    // A record that stood on the band says how long it was shown.
    let strain = MessageView {
        detail: vec!["shown for 41 s".into(), "cpu 3.2 cores".into()],
        ..with("recorded", "warn", Some(at))
    };
    assert_eq!(strain.shown_line(), Some("shown for 41 s"));
    assert_eq!(strain.record_words(), "shown for 41 s");
    assert_eq!(strain.state_words(), "shown for 41 s");
    assert_eq!(with("recorded", "info", Some(at)).state_words(), "recorded");
    assert_eq!(
        MessageView {
            answer: Some("Not now".into()),
            ..with("answered", "info", later)
        }
        .state_words(),
        "answered: Not now"
    );
    for (severity, words, alarm) in [
        ("error", "Error", true),
        ("warn", "Warning", true),
        ("success", "Done", false),
        ("info", "Info", false),
    ] {
        let e = with("folded", severity, later);
        assert_eq!(e.severity_words(), words);
        assert_eq!(e.alarm(), alarm, "{severity}");
    }
}

/// Every vocabulary word has a chip position, in the chips' own words; a
/// script's tags read like the chips beside them (ruling 265); a span in
/// words. The span words moved here from the host's projection test.
#[test]
fn tags_chips_cover_the_vocabulary() {
    for tag in tags::ALL {
        assert!(TAG_ORDER.contains(&tag.as_str()), "{tag}");
    }
    assert_eq!(TAG_ORDER.len(), tags::ALL.len());
    assert_eq!(tag_words("script"), "Scripts");
    assert_eq!(tag_words("search"), "Search");
    assert_eq!(tag_words("ci"), "CI");
    assert_eq!(tag_words("Deploy"), "Deploy");
    assert_eq!(tag_words("crash"), "Crashes");
    assert_eq!(tag_words("toolchain"), tag_words("packages"));
    assert_eq!(tag_words("fabric"), "Agents");
    assert_eq!(tag_words("harness"), "Agents");
    assert_eq!(span_words(40_000), "40 s");
    assert_eq!(span_words(150_000), "2 min");
    assert_eq!(span_words(7_200_000), "2 h");
    assert_eq!(span_words(200_000_000), "2 d");
}

/// The filters match a tag by the CHIP it shows under (ruling 262): one
/// Agents chip for `fabric` and `harness`, one ALab tools chip for
/// `toolchain` and `packages`, and Problems is warnings and errors together
/// (ruling 264); the count line says the whole of what they admit; an
/// entry's spoken name is its severity, chip and age in words.
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
    assert_eq!(status_words(42, 42, true), "42 messages");
    assert_eq!(status_words(1, 1, true), "1 message");
    assert_eq!(status_words(0, 0, false), "0 messages");
    assert_eq!(status_words(42, 12, false), "12 of 42");
    assert_eq!(description(&fabric, NOW), "Info, Agents, 6 hours ago");
    assert_eq!(
        description(
            &MessageView {
                tag: "toolchain".into(),
                ..entry_at(3, NOW - 6 * 3_600_000)
            },
            NOW
        ),
        "Warning, ALab tools, 6 hours ago"
    );
}

/// A STAMP FROM TOMORROW IS TODAY'S (ruling 281): a writer whose clock
/// runs ahead stamps a record past the reader's midnight. It heads no
/// list of today alone (ruling 277: a list all of today carries none),
/// and in a headed list it opens no section of its own — before 281 it
/// did, and read `Today` above today's own `Today`. NEGATIVE CONTROL: a
/// past entry still heads the list, into two sections.
#[test]
fn a_stamp_from_tomorrow_is_todays() {
    const DAY: u64 = 86_400_000;
    let now = NOW;
    let mut entries: Vec<MessageView> = (38..=40).rev().map(|id| entry_at(id, now)).collect();
    // Past the reader's midnight (the reader is at 15:53 UTC).
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

/// Under the day headers a row older than today says its local time; today's
/// rows, and every row of an unheaded list, keep their relative words
/// (ruling 273); the clock reads the host's offset.
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
    // 15:53 UTC is 08:53 at UTC-7: the same instant, another local day line.
    let west = MessagesClock::of(&[&today], NOW, -25_200);
    assert_eq!(west.offset_s, -25_200);
    assert_eq!(west.today, words::local_day(NOW, -25_200));
    assert_eq!(west.now_unix_ms, NOW);
}

/// Copy puts on the clipboard byte for byte what the engine's
/// [`words::copy_text`] makes of the record (the page holds views, not
/// records).
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

/// The page's feedback says what the host did, in a person's words: an
/// `Install now` press installs only where it applies, opens Software Update
/// otherwise, and a failure reads as one; a refusal never carries a route.
#[test]
fn act_feedback_words_what_the_host_did() {
    assert_eq!(
        act_feedback(&Intent::NewWindow, true, false),
        "Opened a new window"
    );
    let apply = Intent::ApplyUpdate { build: 1 };
    assert_eq!(act_feedback(&apply, true, true), "Installing the update");
    assert_eq!(
        act_feedback(&apply, true, false),
        "Opened Settings \u{25b8} Software Update"
    );
    assert_eq!(
        act_feedback(&apply, false, true),
        "Could not start the install"
    );
    assert_eq!(
        act_feedback(&apply, false, false),
        "Could not open Settings"
    );
    let packages = Intent::OpenSettings {
        route: "/packages".into(),
    };
    assert_eq!(
        act_feedback(&packages, true, false),
        "Opened Settings \u{25b8} Packages"
    );
    assert_eq!(
        act_feedback(
            &Intent::OpenSettings {
                route: "/nowhere".into()
            },
            false,
            false
        ),
        "Could not open Settings",
        "no route string on the page"
    );
    let not_now = Intent::NotNow {
        decision: Decision::FileAccess,
    };
    assert_eq!(act_feedback(&not_now, true, false), "Not now recorded");
    assert_eq!(act_feedback(&not_now, false, false), "Not now recorded");
    assert_eq!(
        act_feedback(&Intent::ShowTab { tab: 2, window: 7 }, true, false),
        "Showed tab 2"
    );
    let word = upgrade("s-1", "2.1.282", UpgradeWord::NotToday);
    assert!(
        act_feedback(&word, true, false).ends_with("; what it did shows in this log"),
        "{}",
        act_feedback(&word, true, false)
    );
    assert_eq!(
        act_feedback(&word, false, false),
        "The upgrade no longer takes that word"
    );
}

/// A line of [`crate::page::wire_lines`]' grammar read back as a page would:
/// its kind word, then each field split at its first `=` and unescaped (the
/// log codec's inverse). One line never carries a `\n`.
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

/// The value of `key` in a read-back line.
fn page_field<'a>(fields: &'a [(String, String)], key: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// The `page` line's chrome keys (ruling 396), byte for byte, after every
/// older key: for a page whose tag filter admits `all` entries, `problems`
/// of them warnings or errors, with no tag filter down.
fn page_chrome(all: usize, problems: usize) -> String {
    format!(
        "\theading=Messages\tsubtitle=What aterm reported, newest first.\tall_words=All \u{00b7} {all}\tproblems_words=Problems \u{00b7} {problems}\tall_tags=All tags\tall_tags_on=1\ttag_menu=Tag: All\ttag_menu_name=Filter by tag: All\tcopy_all_button=Copy All\tcopy_all_name=Copy the shown messages\tempty=Nothing yet.\ttechnical_caption=Technical details\tcopy_button=Copy"
    )
}

/// The page-lines fixture: a warning record from three days ago (its
/// sentence and a technical line), a packages record two hours ago (one
/// ALab tools chip with it), a crash record ten minutes ago whose details
/// are a diagram and whose log the host has, and an agent's warning on the
/// band a minute ago whose sentence is two lines (the desk's `two.lines`
/// key), with a backslash in its title and two upgrade words, one taken.
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
            .hold(Hold::Standing),
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

/// THE PAGE AS LINES (ruling 387), byte for byte: one grammar a web page
/// renders without wording anything itself — the head with the counts and
/// the count line, the chips in their order (`toolchain` and `packages`
/// ONE chip), a day header before each section of a headed list, and each
/// entry newest first with its lead, meta, chip, severity, state and spoken
/// words, its sentence and technical lines (a diagram technical whole, an
/// upgrade row's two-line sentence), and its offered capsules — every value
/// through the log codec's escaping (the title's backslash is `\\`, the
/// lines joined by US are `\u`).
#[test]
fn the_page_as_lines_says_every_word_in_one_grammar() {
    let (c, desk) = page_lines_fixture();
    let page = MessagesState::of(&c, &desk);
    let lines = crate::page::wire_lines(&page, &MessagesFilter::default());
    let rev = c.revision();
    let chrome = page_chrome(4, 3);
    let want = [
        format!(
            "page\trev={rev}\tnow={NOW}\ttotal=4\tshown=4\tall=4\tproblems=3\tstatus=4 messages{chrome}"
        ),
        "chip\ttag=crash\twords=Crashes\tcount=1\ton=0\tlabel=Crashes \u{00b7} 1".to_string(),
        "chip\ttag=toolchain\twords=ALab tools\tcount=2\ton=0\tlabel=ALab tools \u{00b7} 2"
            .to_string(),
        "chip\ttag=harness\twords=Agents\tcount=1\ton=0\tlabel=Agents \u{00b7} 1".to_string(),
        "day\tday=20352\twords=Today\tshort=Today".to_string(),
        format!(
            "entry\tid=4\tat={}\twhen=1 min ago\tlocal=Today 3:52:20 PM\ttag=harness\ttag_words=Agents\tsev=warn\tsev_words=Warning\tmark=\u{26a0}\ttitle=Claude upgrade waits in C:\\\\work\tstate=live\tstate_words=showing now\trep=1\tdescription=Warning, Agents, 1 minute ago\tsentence=it asks again on its own at 4:00 PM\\uUpgrade now asks it at its next turn end\ttechnical=in tab 1 \u{00b7} 2.1.281 \u{2192} 2.1.282\tmeta=Today 3:52:20 PM \u{00b7} showing now\tcopy=Claude upgrade waits in C:\\\\work\\n2025-09-21 15:52:20 UTC \u{00b7} harness \u{00b7} warn\\nit asks again on its own at 4:00 PM\\nUpgrade now asks it at its next turn end\\nin tab 1 \u{00b7} 2.1.281 \u{2192} 2.1.282",
            NOW - 60_000
        ),
        "action\tid=4\tindex=0\tlabel=Upgrade now\tactionable=1\tbutton=Upgrade now\tprimary=1"
            .to_string(),
        "action\tid=4\tindex=1\tlabel=Not today\tactionable=0\tbutton=Not today\tprimary=0"
            .to_string(),
        format!(
            "entry\tid=3\tat={}\twhen=10 min ago\tlocal=Today 3:43:20 PM\ttag=crash\ttag_words=Crashes\tsev=error\tsev_words=Error\tmark=\u{2715}\ttitle=aterm closed unexpectedly\tstate=recorded\tstate_words=recorded\trep=1\tdescription=Error, Crashes, 10 minutes ago\tsentence=\ttechnical=panicked at src/grid.rs:3:9\\u    ^ index out of bounds\tmeta=Today 3:43:20 PM \u{00b7} recorded\tcopy=aterm closed unexpectedly\\n2025-09-21 15:43:20 UTC \u{00b7} crash \u{00b7} error\\npanicked at src/grid.rs:3:9\\n    ^ index out of bounds",
            NOW - 10 * 60_000
        ),
        "action\tid=3\tindex=0\tlabel=Open log\tactionable=1\tbutton=Open log\tprimary=1"
            .to_string(),
        format!(
            "entry\tid=2\tat={}\twhen=2 h ago\tlocal=Today 1:53:20 PM\ttag=packages\ttag_words=ALab tools\tsev=info\tsev_words=Info\tmark=\u{2139}\ttitle=trust updated\tstate=recorded\tstate_words=recorded\trep=1\tdescription=Info, ALab tools, 2 hours ago\tsentence=0.9.1 is live\ttechnical=\tmeta=Today 1:53:20 PM \u{00b7} recorded\tcopy=trust updated\\n2025-09-21 13:53:20 UTC \u{00b7} packages \u{00b7} info\\n0.9.1 is live",
            NOW - 2 * 3_600_000
        ),
        "day\tday=20349\twords=Thursday\tshort=Thursday".to_string(),
        format!(
            "entry\tid=1\tat={}\twhen=3:53 PM\tlocal=Thursday 3:53:20 PM\ttag=toolchain\ttag_words=ALab tools\tsev=warn\tsev_words=Warning\tmark=\u{26a0}\ttitle=ALab tools could not update\tstate=recorded\tstate_words=recorded\trep=1\tdescription=Warning, ALab tools, 2025-09-18\tsentence=the index could not be reached; it tries again in an hour\ttechnical=GET /index.toml: 503\tmeta=Thursday 3:53:20 PM \u{00b7} recorded\tcopy=ALab tools could not update\\n2025-09-18 15:53:20 UTC \u{00b7} toolchain \u{00b7} warn\\nthe index could not be reached; it tries again in an hour\\nGET /index.toml: 503",
            NOW - 3 * 86_400_000
        ),
    ];
    assert_eq!(lines, want);
    // Read back as a page reads it: the sentence lines split at US.
    let (kind, fields) = read_page_line(&lines[5]);
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
    // Every word is the engine's own (the page's model, not a second copy).
    let wait = page.entry(4).unwrap();
    assert_eq!(
        page_field(&fields, "state_words"),
        Some(&*wait.state_words())
    );
    // Copy's text reads back byte for byte, its line breaks the escaping's
    // (ruling 396), and the meta line is the one an expanded entry paints.
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

/// THE LINES FOLLOW THE FILTER (ruling 387): the tag filter selects a chip
/// by its words (a deep link to `packages` lights the ALab tools chip keyed
/// `toolchain`) and the severity counts follow it; Problems counts every chip
/// by its warnings and errors and admits only those; the count line says
/// what the filters admit; a list the filter leaves all of today carries no
/// header. The host's facts: the not-saved note and the log folder appear
/// only while they are true. An empty log is its head alone, with no count.
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
    // The chrome follows the filter (ruling 396): the segments' words over
    // the chip's counts, `All tags` up, the pop-up naming the chip.
    assert_eq!(page_field(&head, "all_words"), Some("All \u{00b7} 2"));
    assert_eq!(
        page_field(&head, "problems_words"),
        Some("Problems \u{00b7} 1")
    );
    assert_eq!(page_field(&head, "all_tags_on"), Some("0"));
    assert_eq!(page_field(&head, "tag_menu"), Some("Tag: ALab tools"));
    assert_eq!(
        page_field(&head, "tag_menu_name"),
        Some("Filter by tag: ALab tools")
    );
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
    assert_eq!(page_field(&head, "tag_menu"), Some("Tag: All"));
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
    // No filter admits nothing but says so.
    let none = crate::page::wire_lines(
        &page,
        &MessagesFilter {
            tag: Some("update".into()),
            warn_only: false,
        },
    );
    assert_eq!(none.len(), 4, "the head and the chips: {none:?}");
    assert!(none[0].contains("\tshown=0\tall=0\tproblems=0\tstatus=0 of 4"));

    // The host's facts: the note and the folder only while true.
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
    let chrome = page_chrome(4, 3);
    assert!(
        lines[0].ends_with(&format!(
            "\tnote={NOT_SAVED}\tfolder=/logs\\\\aterm{chrome}"
        )),
        "{}",
        lines[0]
    );
    // No host has the log: `Open log` offered, not pressable (the negative
    // control of the desk's file).
    assert!(
        lines.contains(
            &"action\tid=3\tindex=0\tlabel=Open log\tactionable=0\tbutton=Open log\tprimary=0"
                .to_string()
        )
    );

    // An empty log: the head alone, and no count line.
    let fresh = center(Instant::now());
    let empty = crate::page::wire_lines(
        &MessagesState::of(&fresh, &Desk::default()),
        &MessagesFilter::default(),
    );
    assert_eq!(
        empty,
        [format!(
            "page\trev={}\tnow={NOW}\ttotal=0\tshown=0\tall=0\tproblems=0\tstatus={}",
            fresh.revision(),
            page_chrome(0, 0)
        )]
    );
}

/// ANY WORD SURVIVES THE LINES (ruling 387): a title, tag, detail line or
/// label carrying the grammar's own separators — TAB, LF, CR, US and the
/// escape's backslash — reads back whole, field for field, and never splits
/// a line or a field (a host hands the page words it did not post: a
/// restored log, another build's line). Every line of a kind has its keys
/// in the one fixed order.
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
    // COPY'S TEXT READS BACK BYTE FOR BYTE (ruling 396): its title and tag
    // carry TAB, LF, CR, US and a backslash, a detail line a literal `\n`,
    // and its own line breaks ride the escaping's `\n` — one value, never a
    // US-joined list.
    let copy = entry.copy_text();
    assert!(
        ['\t', '\n', '\r', '\u{1f}', '\\']
            .iter()
            .all(|c| copy.contains(*c)),
        "{copy:?}"
    );
    assert_eq!(page_field(e, "copy"), Some(copy.as_str()));
    assert_eq!(page_field(e, "meta"), Some(&*entry.meta_words(NOW, 0)));
    // NEGATIVE CONTROL: the list fields' US join would have lost the
    // title's US (`join_us` drops one inside a line).
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
    // The pop-up names a hostile chip whole.
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
    // NEGATIVE CONTROL: the same field unescaped would split the line.
    assert!(format!("title={hostile}").split('\t').count() > 1);
}

/// THE PAGE'S CHROME WORDS (ruling 395), byte for byte: the words the macOS
/// page paints and names its controls by, the engine's so a page on any
/// host says the same page — the heading, the subtitle, the empty log's
/// line, the first tag chip, Copy and Copy All, the technical caption; the
/// severity segments over their counts; a chip's name and count; the tag
/// pop-up's painted and spoken words (the chip's words, never the tag, and a
/// script's colliding tag as the script wrote it, ruling 390); a
/// destination's footer button `Open …` and every other intent's own label;
/// the meta line, with the posts folded in only past one; and Copy All's
/// text — the host's build information verbatim, then each shown entry's
/// Copy text after two line breaks (ruling 397).
#[test]
fn the_pages_chrome_words_are_the_engines() {
    use crate::page::{
        ALL_TAGS, COPY, COPY_ALL, COPY_ALL_NAME, Chip, EMPTY, HEADING, SUBTITLE, TECHNICAL,
        button_label, copy_all_text, severity_segment_words, tag_menu_label, tag_menu_name,
    };
    assert_eq!(HEADING, "Messages");
    assert_eq!(SUBTITLE, "What aterm reported, newest first.");
    assert_eq!(EMPTY, "Nothing yet.");
    assert_eq!(ALL_TAGS, "All tags");
    assert_eq!(COPY, "Copy");
    assert_eq!(COPY_ALL, "Copy All");
    assert_eq!(COPY_ALL_NAME, "Copy the shown messages");
    assert_eq!(TECHNICAL, "Technical details");
    assert_eq!(
        severity_segment_words(42, 23),
        (
            "All \u{00b7} 42".to_string(),
            "Problems \u{00b7} 23".to_string()
        )
    );
    assert_eq!(
        severity_segment_words(0, 0),
        (
            "All \u{00b7} 0".to_string(),
            "Problems \u{00b7} 0".to_string()
        )
    );
    let chip = Chip {
        tag: "toolchain".into(),
        words: "ALab tools".into(),
        count: 2,
    };
    assert_eq!(chip.label(), "ALab tools \u{00b7} 2");
    assert_eq!(tag_menu_label(None), "Tag: All");
    assert_eq!(tag_menu_name(None), "Filter by tag: All");
    assert_eq!(tag_menu_label(Some("packages")), "Tag: ALab tools");
    assert_eq!(tag_menu_name(Some("harness")), "Filter by tag: Agents");
    assert_eq!(tag_menu_label(Some("ci")), "Tag: CI");
    assert_eq!(tag_menu_label(Some("updates")), "Tag: updates");
    for (intent, button) in [
        (
            Intent::OpenSettings {
                route: "/packages".into(),
            },
            "Open Packages",
        ),
        (
            Intent::OpenSettings {
                route: "/updates".into(),
            },
            "Open Software Update",
        ),
        (
            Intent::OpenSettings {
                route: "/manual".into(),
            },
            "Open Manual",
        ),
        (
            Intent::OpenSettings {
                route: "/appearance".into(),
            },
            "Open Appearance",
        ),
        (
            Intent::OpenSettings {
                route: "/messages".into(),
            },
            "Open Messages",
        ),
        (
            Intent::OpenSettings {
                route: "/privacy".into(),
            },
            "Open Settings",
        ),
        (
            Intent::OpenPath {
                path: "/logs/crash-1.log".into(),
            },
            "Open log",
        ),
        (Intent::ApplyUpdate { build: 7 }, "Install now"),
        (upgrade("s-1", "2.1.282", UpgradeWord::Now), "Upgrade now"),
        (Intent::NewWindow, "New window"),
        (Intent::Details, "Details \u{203a}"),
    ] {
        assert_eq!(button_label(intent.label()), button, "{intent:?}");
    }
    // The meta line: the local time and the state's plain words, `×N` only
    // past one post.
    let once = entry_at(1, NOW - 60_000);
    assert_eq!(
        once.meta_words(NOW, 0),
        "Today 3:52:20 PM \u{00b7} shown for 30 s"
    );
    let thrice = MessageView {
        repeats: 3,
        ..entry_at(2, NOW - 60_000)
    };
    assert_eq!(
        thrice.meta_words(NOW, 3600),
        "Today 4:52:20 PM \u{00b7} shown for 30 s \u{00b7} \u{00d7}3"
    );
    // Copy All: the build information, then each shown entry's Copy text
    // after a blank line, in the order shown.
    let newer = entry_at(1, NOW - 60_000);
    let older = entry_at(2, NOW - 120_000);
    assert_eq!(
        copy_all_text("aterm 0.70.0", [&newer, &older]),
        "aterm 0.70.0\n\nconfig warning 1\n2025-09-21 15:52:20 UTC \u{00b7} config \u{00b7} warn\naterm.toml: unknown key `foo` at line 12\n\nconfig warning 2\n2025-09-21 15:51:20 UTC \u{00b7} config \u{00b7} warn\naterm.toml: unknown key `foo` at line 12"
    );
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
    // The build information is taken verbatim: the macOS host's ends in a
    // line break of its own (`about::provenance_text`), so its first entry
    // follows two blank lines and the next one a single blank line.
    assert_eq!(
        copy_all_text("aterm\nversion: 0.70.0\n", [&newer, &older]),
        format!(
            "aterm\nversion: 0.70.0\n\n\n{}\n\n{}",
            newer.copy_text(),
            older.copy_text()
        )
    );
}

/// THE PAGE'S PRIMARY IS THE ENGINE'S (ruling 407; the rule of rulings 265, 401
/// and 403, which the macOS page applied itself until then): the first offered
/// intent while a press can perform it — a pressable first decline keeps it —
/// else, past a dead first, the first LATER pressable intent that is no decline,
/// else none. Every case of ruling 403 is pinned, with the controls that tell the
/// rule from its near misses: plain "first pressable" would promote `Skip
/// version` past a dead `Upgrade now` (case b), and "first non-decline" would
/// take the Primary from a pressable first `Not now` (case d).
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
    // (a) The first offered intent, pressable, leads — whatever follows.
    assert_eq!(
        primary_index(&[
            act(0, "Upgrade now", true, false),
            act(1, "Skip version", true, true),
            act(2, "Software Update", true, false),
        ]),
        Some(0)
    );
    // (b) A dead first hands the Primary to the first LATER pressable intent
    // that is no decline: `Skip version` is passed over, `Software Update`
    // leads.
    assert_eq!(
        primary_index(&[
            act(0, "Upgrade now", false, false),
            act(1, "Skip version", true, true),
            act(2, "Software Update", true, false),
        ]),
        Some(2)
    );
    // …and a dead later non-decline is passed over too.
    assert_eq!(
        primary_index(&[
            act(0, "Install now", false, false),
            act(1, "Open log", false, false),
            act(2, "Manual", true, false),
        ]),
        Some(2)
    );
    // A dead first DECLINE hands it on the same way.
    assert_eq!(
        primary_index(&[
            act(0, "Not now", false, true),
            act(1, "Open Settings", true, false),
        ]),
        Some(1)
    );
    // (c) Past a dead first, only declines left pressable: none leads.
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
    // (d) A first decline that is pressable keeps it (the reporter's order
    // leads, ruling 265).
    assert_eq!(
        primary_index(&[
            act(0, "Not now", true, true),
            act(1, "Open Settings", true, false),
        ]),
        Some(0)
    );
    // Nothing pressable, and nothing offered: none.
    assert_eq!(
        primary_index(&[
            act(0, "Open log", false, false),
            act(1, "Manual", false, false),
        ]),
        None
    );
    assert_eq!(primary_index(&[]), None);
}

/// THE LINES CARRY THE PRIMARY (ruling 407): each `action` line ends
/// `primary=`, `1` on the entry's Primary alone ([`crate::page::primary_index`]),
/// `0` on every other — so a page drawing the lines could lead with the one the
/// native footer does without re-deriving the rule (none does yet: no site
/// renders the page, and the web hosts press nothing on a record, ruling 389)
/// — and the key is appended after
/// every older one (`button=` stays where it was). Across entries: a dead
/// first hands it to a later navigation; only declines left, none leads; a
/// pressable first decline keeps it.
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
    assert_eq!(
        actions,
        [
            "action\tid=3\tindex=0\tlabel=Upgrade now\tactionable=0\tbutton=Upgrade now\tprimary=0",
            "action\tid=3\tindex=1\tlabel=Skip version\tactionable=1\tbutton=Skip version\tprimary=0",
            "action\tid=3\tindex=2\tlabel=Software Update\tactionable=1\tbutton=Open Software Update\tprimary=1",
            "action\tid=2\tindex=0\tlabel=Upgrade now\tactionable=0\tbutton=Upgrade now\tprimary=0",
            "action\tid=2\tindex=1\tlabel=Skip version\tactionable=1\tbutton=Skip version\tprimary=0",
            "action\tid=1\tindex=0\tlabel=Not now\tactionable=1\tbutton=Not now\tprimary=1",
            "action\tid=1\tindex=1\tlabel=Open Settings\tactionable=1\tbutton=Open Settings\tprimary=0",
        ]
    );
    // Read back as a page reads it: the lines' `primary=1` is the rule's pick,
    // entry by entry.
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

/// `primary=` MARKS A POSITION IN THE OFFERED INTENTS, NOT AN `ActionIndex`
/// (the round-39 review; ruling 408). [`crate::page::primary_index`] answers a
/// position in `entry.actions`, while each line's `index=` keeps the intent's
/// own `ActionIndex`: an intent whose moment passed is not offered (ruling
/// 265: `Not now` on an answered ask), so the offered indices can start past
/// 0 and skip. Every other pin offers `index=` equal to the position, where a
/// mark by `index=` reads the same. Here they differ: (A) the first offered
/// intent, `index=1`, leads, where a mark by `index=` would mark none; (B) past a
/// dead `index=1`, the pressable `index=2` leads, where a mark by `index=`
/// would put the Primary on the dead button (day ten's D2 again).
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
