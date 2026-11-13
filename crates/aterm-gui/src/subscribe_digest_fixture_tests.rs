// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    PushOptions, ResolvedTarget, TargetStreams, Watch, WatchSource, frames_for_watch, new_watch,
};
use crate::session_timeline::SessionTimeline;
use crate::turn_ledger::{TurnLedger, TurnRecord};
use aterm_core::terminal::Terminal;
use std::sync::{Arc, Mutex};

const CYCLE: &[u8] = b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\r\n\x1b]133;C\x07out\r\n\x1b]133;D;0\x07";

pub(crate) struct DigestFixture {
    watches: Vec<Watch>,
    streams: TargetStreams,
    terms: Vec<Arc<Mutex<Terminal>>>,
    turns: Vec<Arc<Mutex<TurnLedger>>>,
    timelines: Vec<Arc<Mutex<SessionTimeline>>>,
    next_turn_id: u64,
}

impl DigestFixture {
    pub(crate) fn build(targets: usize, blocks: usize, turns: usize, timeline: usize) -> Self {
        let mut f = DigestFixture {
            watches: Vec::with_capacity(targets),
            streams: TargetStreams {
                events: true,
                ..Default::default()
            },
            terms: Vec::with_capacity(targets),
            turns: Vec::with_capacity(targets),
            timelines: Vec::with_capacity(targets),
            next_turn_id: 0,
        };
        for i in 0..targets {
            let fixture_term = Arc::new(Mutex::new(Terminal::new(24, 80)));
            {
                let mut t = fixture_term.lock().expect("fresh engine");
                for _ in 0..blocks {
                    t.process(CYCLE);
                }
                t.process(b"\x1b]2;fixture\x07");
            }
            let fixture_ctx =
                crate::session_store::test_handle_with(i as u64, fixture_term.clone()).ctx;
            let ledger = fixture_ctx.turns.clone();
            {
                let mut l = ledger.lock().expect("fresh ledger");
                for _ in 0..turns {
                    f.next_turn_id += 1;
                    l.push(turn_record(f.next_turn_id));
                }
            }
            let tl = fixture_ctx.timeline.clone();
            {
                let mut t = tl.lock().expect("fresh timeline");
                for j in 0..timeline {
                    // A mix: the digest filters for `meta-change`, so an all-meta
                    // fixture would flatter a whole-ring scan and an all-other one
                    // would never reach the emit path.
                    if j % 8 == 0 {
                        t.record_raw_meta_change("field=role value=lead".to_string());
                    } else {
                        let _ = t.record_hand_row("state-change", "state=alive".to_string());
                    }
                }
            }
            let target = ResolvedTarget {
                local_id: i as u64,
                term: fixture_term.clone(),
                src: WatchSource::of(&fixture_ctx),
            };
            f.watches
                .push(new_watch(&target, f.streams, &PushOptions::default()));
            f.terms.push(fixture_term);
            f.turns.push(ledger);
            f.timelines.push(tl);
        }
        f
    }

    pub(crate) fn wake(&mut self, woke: bool) -> usize {
        let mut bytes = 0usize;
        for w in &mut self.watches {
            for f in frames_for_watch(w, self.streams, woke) {
                bytes += f.segs.iter().map(|seg| seg.bytes().len()).sum::<usize>();
            }
        }
        bytes
    }

    /// The first target's blocks, turns, and its timeline view's rows with the row ids its log no longer holds.
    pub(crate) fn retained(&self) -> (usize, usize, usize, u64) {
        let fixture_term = &self.terms[0];
        let fixture_turns = &self.turns[0];
        let view = self.timelines[0]
            .lock()
            .expect("fixture timeline")
            .view_since(None);
        (
            fixture_term
                .lock()
                .expect("fixture engine")
                .all_blocks()
                .count(),
            fixture_turns.lock().expect("fixture ledger").len(),
            view.rows.len(),
            view.lost,
        )
    }

    /// One new `meta-change` row on every target's timeline.
    pub(crate) fn land_row(&mut self) {
        for tl in &self.timelines {
            let _ = tl
                .lock()
                .expect("fixture timeline")
                .record_raw_meta_change("field=role value=lead".to_string());
        }
    }

    pub(crate) fn land_turn(&mut self) {
        let mut id = self.next_turn_id;
        for fixture_ledger in &self.turns {
            id += 1;
            fixture_ledger
                .lock()
                .expect("fixture ledger")
                .push(turn_record(id));
        }
        self.next_turn_id = id;
    }
}

pub(crate) fn turn_record(id: u64) -> TurnRecord {
    TurnRecord {
        id,
        started_ms: id,
        dur_ms: 12,
        submitted: true,
        status: "settled",
        text: "run the suite".into(),
        screen_hash: id.wrapping_mul(0x9E37_79B9_7F4A_7C15),
        seq: id,
        arch: crate::turn_ledger::ArchMark::default(),
        by: crate::turn_ledger::TurnBy::Ctl,
        carried: false,
    }
}
