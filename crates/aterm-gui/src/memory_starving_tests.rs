// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_vitals::events::{Event, EventKind};
use aterm_vitals::starving::{Why, Worst};

fn episode() -> Episode {
    Episode {
        why: Why::Thrash,
        opened_ns: 1,
        opened_unix_ms: 1_763_277_369_800,
        at_open: Look {
            push_bps: Some(259.0 * MIB),
            refault_bps: Some(89.0 * MIB),
            headroom: Some(382_055 * 16_384),
            ram: 128 << 30,
            kept_permille: Some(468),
            ..Look::default()
        },
        worst: Worst {
            push_bps: 300.0 * MIB,
            refault_bps: 90.0 * MIB,
            headroom: Some(914 * 16_384),
            wall_permille: Some(1000),
            stall_full: None,
        },
        holders: vec![
            Holder {
                pid: 77_936,
                name: "ty".into(),
                bytes: 143_200_000_000,
                scope: RowScope::Others,
                label: None,
            },
            Holder {
                pid: 77_700,
                name: "a typed\nname".into(),
                bytes: 89_070_000_000,
                scope: RowScope::Tab(7),
                label: Some("Tab 3".into()),
            },
        ],
        holders_age_ms: Some(5_000),
        ended: None,
    }
}

/// AN EDGE'S LINE IS WORDS AND NUMBERS: its head (which the census reads), the rule, what the
/// opening read in MiB, and its holders as `pid:name:MiB:where`, a name that is no word as `?`
/// and a tab by its key, never its label; the end, how long it lasted and the worst it read.
/// NEGATIVE CONTROL: every part of every token after the head is an aterm.log word or the `?`
/// that stands for a text that is none, so the typed name and the tab's label are nowhere in it.
#[test]
fn an_edges_line_is_words_and_numbers() {
    let opened = line(&Edge::Opened(episode()));
    assert_eq!(
        opened,
        "memory starving: opened why=thrash push_mibps=259 refault_mibps=89 headroom_mib=5969 \
         ram_mib=131072 kept_permille=468 pressure=unknown \
         largest=77936:ty:136566:others,77700:?:84943:tab7 largest_age_ms=5000"
    );
    let ended = line(&Edge::Ended(Episode {
        ended: Some((1_763_277_369_800 + 300_000, Look::default())),
        holders: Vec::new(),
        holders_age_ms: None,
        ..episode()
    }));
    assert_eq!(
        ended,
        "memory starving: ended why=thrash for_ms=300000 worst_push_mibps=300 \
         worst_refault_mibps=90 least_headroom_mib=14 worst_wall_permille=1000 largest=none"
    );
    for l in [&opened, &ended] {
        let tail = l
            .strip_prefix(OPENED_LINE)
            .or_else(|| l.strip_prefix(ENDED_LINE))
            .expect("a head");
        for token in tail.split_whitespace() {
            for part in token.split([',', ':']) {
                assert!(
                    part == "?" || aterm_log::words::word(part) == part,
                    "{part} in {l}"
                );
            }
        }
        assert!(!l.contains("Tab 3") && !l.contains("typed"), "{l}");
    }
}

/// A stand-in sink: what was written, at which level, and each sync, answering as `answers`
/// says (its last answer from then on).
struct Seen {
    log: std::sync::Mutex<Vec<String>>,
    answers: std::sync::Mutex<Vec<bool>>,
}

impl Seen {
    fn new(answers: &[bool]) -> Self {
        Self {
            log: std::sync::Mutex::new(Vec::new()),
            answers: std::sync::Mutex::new(answers.iter().rev().copied().collect()),
        }
    }

    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.log.lock().unwrap())
    }
}

impl Sink for Seen {
    fn write(&self, level: aterm_log::Level, line: &str) {
        let head = if line.starts_with(OPENED_LINE) {
            "opened"
        } else {
            "ended"
        };
        self.log.lock().unwrap().push(format!("{level:?}:{head}"));
    }

    fn sync(&self) -> bool {
        let mut answers = self.answers.lock().unwrap();
        let ok = if answers.len() > 1 {
            answers.pop().unwrap()
        } else {
            answers.last().copied().unwrap_or(true)
        };
        self.log.lock().unwrap().push(format!("sync:{ok}"));
        ok
    }
}

fn with_edges(edges: Vec<Edge>) -> Snapshot {
    let mut events: Vec<Event> = edges
        .into_iter()
        .map(|edge| Event {
            seq: 1,
            at_ns: 0,
            at_unix_ms: None,
            scope: aterm_vitals::events::EventScope::Machine,
            kind: EventKind::Starving(Box::new(edge)),
        })
        .collect();
    events.push(Event {
        seq: 1,
        at_ns: 0,
        at_unix_ms: None,
        scope: aterm_vitals::events::EventScope::default(),
        kind: EventKind::Orphan {
            key: 1,
            label: "Tab 2".into(),
            procs: 1,
            top: None,
        },
    });
    Snapshot {
        events,
        ..Snapshot::default()
    }
}

/// THE PROBE THREAD JOURNALS EACH STARVATION EDGE AND ORDERS IT TO DISK: an opening at warn and
/// an end at info, then one sync for the reading; a sync that did not get the file is owed and
/// made again at the next reading, edge or none, until one does. NEGATIVE CONTROLS: a reading
/// with no edge and nothing owed writes and syncs nothing, and an orphan's event is no edge.
#[test]
fn the_journal_writes_each_edge_and_owes_a_sync_until_it_lands() {
    let sink = Seen::new(&[false, false, true]);
    let mut journal = Journal::default();
    let both = with_edges(vec![Edge::Opened(episode()), Edge::Ended(episode())]);
    assert_eq!(journal.note(&both, &sink), 2);
    assert_eq!(sink.take(), ["Warn:opened", "Info:ended", "sync:false"]);
    assert!(journal.owed());
    let none = with_edges(Vec::new());
    assert_eq!(journal.note(&none, &sink), 0);
    assert_eq!(sink.take(), ["sync:false"], "owed: tried again");
    assert_eq!(journal.note(&none, &sink), 0);
    assert_eq!(sink.take(), ["sync:true"], "and again, till it lands");
    assert!(!journal.owed());
    assert_eq!(journal.note(&none, &sink), 0);
    assert!(sink.take().is_empty(), "nothing owed, nothing done");
}
