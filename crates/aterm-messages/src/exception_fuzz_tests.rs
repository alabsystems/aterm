// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deterministic randomized tests comparing the digest against the persisted log.

use std::collections::{BTreeMap, BTreeSet};

use crate::Retired;
use crate::center::{MessageCenter, PostOutcome};
use crate::log::{LogLine, MessageLog};
use crate::model::{Message, Severity, WallStamp, tags};
use crate::need::{
    DIGEST_CAP, FILED_KEEP_MS, Fingerprint, LaterAsk, Mover, Reason, Resurface, Then, Why,
};
use crate::{Duration, Instant};

const DAY_MS: u64 = 86_400_000;
const W0: u64 = 1_790_000_000_000;

struct Rng(u64);

impl Rng {
    fn new(seed: u64, salt: u64) -> Self {
        Self(seed.wrapping_mul(salt) | 1)
    }

    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        usize::try_from(self.0 % n as u64).unwrap_or(0)
    }
}

/// The kind of fact under test: the upgrade's own-work entry per tab (a wait: a week) or
/// the Full Disk Access entry (the owner's choice: a month, with a button that does its
/// action).
#[derive(Clone)]
struct Kind {
    key: String,
    base: Resurface,
}

fn kind(pick: usize) -> Kind {
    match pick {
        0..=2 => Kind {
            key: Fingerprint::new("harness.upgrade", &format!("s-{pick}"), "own-work")
                .as_str()
                .to_string(),
            base: Resurface::WAIT,
        },
        _ => Kind {
            key: Fingerprint::new("privacy", "fda", "needed")
                .as_str()
                .to_string(),
            base: Resurface::CHOICE,
        },
    }
}

fn entry(pick: usize, words: u32) -> Message {
    if pick >= 3 {
        let ask = LaterAsk::person(
            Why::OnlyYou(Reason::new(format!("macOS forgot an Allow, {words} times"))),
            Then::press(
                Mover::open_system_pane("full-disk-access"),
                "turn on aterm in Full Disk Access",
            ),
            Fingerprint::new("privacy", "fda", "needed"),
            Resurface::CHOICE,
        );
        return Message::new(tags::PRIVACY, Severity::Info, "macOS keeps asking").later(ask);
    }
    let ask = LaterAsk::person(
        Why::OnlyYou(Reason::new(format!(
            "aterm never ends running work, and {words} programs still run"
        ))),
        Then::step("Stop that work in the tab when it suits you"),
        Fingerprint::new("harness.upgrade", &format!("s-{pick}"), "own-work"),
        Resurface::WAIT,
    );
    Message::new(
        tags::HARNESS,
        Severity::Info,
        format!("Claude in s-{pick} is behind"),
    )
    .later(ask)
}

/// The file, and what it says about every exception's retirements. Each line is encoded
/// and decoded once, as it is drained; a restart replays those decoded lines (a line
/// that does not decode is not in the file).
#[derive(Default)]
struct Oracle {
    file: Vec<LogLine>,
    later_key: BTreeMap<u64, String>,
    words: BTreeMap<String, Vec<(u64, u64, Retired)>>,
}

impl Oracle {
    fn drain(&mut self, c: &mut MessageCenter) {
        for line in c.drain_new_for_persist() {
            if let Ok(back) = LogLine::decode(&line.encode()) {
                self.file.push(back);
            }
            match &line {
                LogLine::Posted(rec) if rec.later => {
                    if let Some(k) = &rec.key {
                        self.later_key.insert(rec.id.raw(), k.clone());
                    }
                }
                LogLine::Retired {
                    id, how, unix_ms, ..
                } => {
                    if let Some(k) = self.later_key.get(&id.raw()) {
                        self.words.entry(k.clone()).or_default().push((
                            *unix_ms,
                            id.raw(),
                            how.clone(),
                        ));
                    }
                }
                _ => {}
            }
        }
    }

    fn quiet(&self, kind: &Kind, wall: u64) -> bool {
        let Some(history) = self.words.get(&kind.key) else {
            return false;
        };
        let Some((t, _, how)) = history.iter().max_by_key(|(t, id, _)| (*t, *id)) else {
            return false;
        };
        let filed = matches!(
            how,
            Retired::Dismissed | Retired::Answered { .. } | Retired::Unseen | Retired::Evicted
        );
        if !filed {
            return false;
        }
        let n = history
            .iter()
            .filter(|(at, _, h)| {
                *at >= t.saturating_sub(FILED_KEEP_MS)
                    && matches!(h, Retired::Dismissed | Retired::Answered { .. })
            })
            .count();
        let span = kind
            .base
            .backed_off(u32::try_from(n).unwrap_or(u32::MAX).max(1));
        t.saturating_add(span) > wall
    }
}

fn open_keys(c: &MessageCenter) -> BTreeSet<String> {
    c.digest().entries.iter().map(|e| e.key.clone()).collect()
}

fn restart(o: &mut Oracle, c: &mut MessageCenter, now: Instant) -> MessageCenter {
    o.drain(c);
    let mut log = MessageLog::empty();
    log.replay_all(o.file.iter().cloned());
    MessageCenter::new(log, now)
}

#[test]
fn the_digest_agrees_with_the_file_through_restarts_floods_and_records() {
    for seed in 1..=20u64 {
        let mut rng = Rng::new(seed, 0x9e37_79b9_7f4a_7c15);
        let t0 = Instant::now();
        let mut at_ms = 0u64;
        let mut c = MessageCenter::new(MessageLog::empty(), t0);
        let mut o = Oracle::default();
        for step in 0..150 {
            let wall = W0 + at_ms;
            let st = WallStamp { unix_ms: wall };
            let now = t0 + Duration::from_millis(at_ms);
            let op = rng.below(12);
            match op {
                0..=2 => {
                    let pick = rng.below(4);
                    let k = kind(pick);
                    let was_open = open_keys(&c).contains(&k.key);
                    let quiet = !was_open && o.quiet(&k, wall);
                    let out = c
                        .post(entry(pick, 1 + rng.below(5) as u32), st, now)
                        .outcome;
                    if was_open {
                        assert!(
                            matches!(out, PostOutcome::Duplicate | PostOutcome::Restated),
                            "seed {seed} step {step}: {} is open, got {out:?}",
                            k.key
                        );
                    } else if quiet {
                        assert_eq!(
                            out,
                            PostOutcome::Suppressed,
                            "seed {seed} step {step}: {} was put away and must be quiet",
                            k.key
                        );
                    } else {
                        assert_eq!(
                            out,
                            PostOutcome::New,
                            "seed {seed} step {step}: {} is not quiet and not open",
                            k.key
                        );
                    }
                }
                3 => {
                    let n = open_keys(&c).len();
                    assert_eq!(c.file_digest(st, now), n);
                }
                4 => {
                    let ids: Vec<_> = c.digest().entries.iter().map(|e| e.id).collect();
                    if !ids.is_empty() {
                        assert!(c.file_digest_one(ids[rng.below(ids.len())], st, now));
                    }
                }
                5 => {
                    let ks: Vec<String> = open_keys(&c).into_iter().collect();
                    if !ks.is_empty() {
                        assert!(c.clear_digest(&ks[rng.below(ks.len())], now));
                    }
                }
                6 => {
                    let before = open_keys(&c);
                    let mut next = restart(&mut o, &mut c, now);
                    let adopted = next.adopt_digest(st, now);
                    assert_eq!(open_keys(&next), before, "seed {seed} step {step}: restart");
                    assert_eq!(adopted, before.len());
                    c = next;
                }
                7 => {
                    let k = kind(rng.below(4));
                    c.post(
                        Message::new(tags::HARNESS, Severity::Info, "a wait, recorded")
                            .key(&k.key)
                            .record(),
                        st,
                        now,
                    );
                }
                8 => {
                    at_ms += (1 + rng.below(6) as u64) * DAY_MS / 3;
                    let _ = c.settle_digest(
                        WallStamp {
                            unix_ms: W0 + at_ms,
                        },
                        t0 + Duration::from_millis(at_ms),
                    );
                }
                9 => {
                    for i in 0..(40 + rng.below(560)) {
                        c.post(
                            Message::new(tags::SYSTEM, Severity::Info, format!("n{i}")).record(),
                            st,
                            now,
                        );
                        if i % 100 == 99 {
                            o.drain(&mut c);
                        }
                    }
                }
                10 => {
                    let e = c
                        .digest()
                        .entries
                        .into_iter()
                        .find(|e| e.actions.first().is_some());
                    if let Some(e) = e {
                        let _ = c.act_digest(e.id, crate::model::ActionIndex(0), st, now);
                    }
                }
                _ => {
                    let desired: Vec<Message> = (0..3)
                        .filter(|_| rng.below(2) == 0)
                        .map(|p| entry(p, 3))
                        .collect();
                    let _ = c.sync_digest("harness.upgrade.", desired, st, now);
                }
            }
            o.drain(&mut c);
            at_ms += 60_000 + rng.below(240_000) as u64;
            assert!(c.digest().entries.len() <= DIGEST_CAP);
            assert_eq!(c.wanted_rows(), 0, "seed {seed} step {step}: never a row");
            let ks: Vec<String> = c.digest().entries.iter().map(|e| e.key.clone()).collect();
            assert_eq!(
                ks.len(),
                ks.iter().collect::<BTreeSet<_>>().len(),
                "seed {seed} step {step}: keys are unique"
            );
        }
    }
}

#[test]
fn the_successor_agrees_with_the_parent_after_the_overlap() {
    for seed in 1..=60u64 {
        let mut rng = Rng::new(seed, 0xd1b5_4a32_d192_ed03);
        let t0 = Instant::now();
        let mut at_ms = 0u64;
        let mut p = MessageCenter::new(MessageLog::empty(), t0);
        let mut o = Oracle::default();
        for _ in 0..rng.below(12) {
            let st = WallStamp {
                unix_ms: W0 + at_ms,
            };
            let now = t0 + Duration::from_millis(at_ms);
            p.post(entry(rng.below(3), 1 + rng.below(4) as u32), st, now);
            if rng.below(4) == 0 {
                p.file_digest(st, now);
            }
            at_ms += 60_000;
        }
        o.drain(&mut p);
        let mut log = MessageLog::empty();
        log.replay_all(o.file.iter().cloned());
        let mut s = MessageCenter::new(log, t0 + Duration::from_millis(at_ms));
        s.adopt_digest(
            WallStamp {
                unix_ms: W0 + at_ms,
            },
            t0 + Duration::from_millis(at_ms),
        );
        let mut put_away: BTreeSet<String> = BTreeSet::new();
        for _ in 0..rng.below(15) {
            let now = t0 + Duration::from_millis(at_ms);
            let st = WallStamp {
                unix_ms: W0 + at_ms,
            };
            match rng.below(4) {
                0 => {
                    p.post(entry(rng.below(3), 1 + rng.below(4) as u32), st, now);
                }
                1 => {
                    put_away.extend(open_keys(&p));
                    p.file_digest(st, now);
                }
                2 => {
                    let ks: Vec<String> = open_keys(&p).into_iter().collect();
                    if !ks.is_empty() {
                        let k = ks[rng.below(ks.len())].clone();
                        p.clear_digest(&k, now);
                        put_away.remove(&k);
                    }
                }
                _ => {
                    let d: Vec<Message> = (0..3)
                        .filter(|_| rng.below(2) == 0)
                        .map(|t| entry(t, 3))
                        .collect();
                    p.sync_digest("harness.upgrade.", d, st, now);
                }
            }
            at_ms += 60_000;
        }
        o.drain(&mut p);
        let now = t0 + Duration::from_millis(at_ms);
        let st = WallStamp {
            unix_ms: W0 + at_ms,
        };
        let tail: Vec<LogLine> = o.file.clone();
        let found = s.catch_up(tail, st, now);
        assert_eq!(
            open_keys(&s),
            open_keys(&p),
            "seed {seed}: the successor and the parent disagree ({found:?})"
        );
        for k in put_away.difference(&open_keys(&p)) {
            let pick = k
                .split('.')
                .nth(2)
                .and_then(|t| t.strip_prefix("s-"))
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(0);
            assert_ne!(
                s.post(entry(pick, 9), st, now).outcome,
                PostOutcome::New,
                "seed {seed}: {k} was put away in the overlap and came back in the successor"
            );
        }
    }
}

#[test]
fn a_key_is_never_both_a_live_row_and_an_open_exception() {
    use crate::need::{Ask, StandingWhy};
    for seed in 1..=40u64 {
        let mut rng = Rng::new(seed, 0xa24b_aed4_963e_e407);
        let t0 = Instant::now();
        let mut at_ms = 0u64;
        let mut c = MessageCenter::new(MessageLog::empty(), t0);
        for step in 0..160 {
            let now = t0 + Duration::from_millis(at_ms);
            let st = WallStamp {
                unix_ms: W0 + at_ms,
            };
            let pick = rng.below(2);
            let key = kind(pick).key;
            match rng.below(7) {
                0 => {
                    c.post(entry(pick, 2), st, now);
                }
                1 => {
                    c.post(
                        Message::new(tags::HARNESS, Severity::Warn, "Claude stopped in a tab")
                            .key(&key)
                            .now(Ask::standing(
                                StandingWhy::Spent {
                                    tried: Reason::new("asked it to resume after the upgrade"),
                                    reason: Reason::new("it did not come back"),
                                },
                                Then::step("Type `claude --resume` in the tab"),
                            )),
                        st,
                        now,
                    );
                }
                2 => {
                    let _ = c.settle(now + Duration::from_secs(3), true);
                }
                3 => {
                    c.file_digest(st, now);
                }
                4 => {
                    let ids: Vec<_> = c.live_rows().map(|l| l.id).collect();
                    if !ids.is_empty() {
                        let _ = c.mark_seen(ids[rng.below(ids.len())], now);
                    }
                }
                5 => {
                    let _ = c.commit_rows(now + Duration::from_secs(3), 3);
                }
                _ => {
                    let _ =
                        c.resolve_key_prefix("harness.upgrade.", crate::center::Outcome::Ok, now);
                }
            }
            at_ms += 30_000;
            let live: BTreeSet<String> = c.live_rows().filter_map(|l| l.msg.key.clone()).collect();
            let both: Vec<_> = live.intersection(&open_keys(&c)).cloned().collect();
            assert!(both.is_empty(), "seed {seed} step {step}: {both:?}");
        }
    }
}
