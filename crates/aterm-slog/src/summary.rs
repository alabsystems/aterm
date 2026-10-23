// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CLOSED SUMMARY (design §5.1, phase 2A).

use std::collections::VecDeque;

use super::{CarryError, Limits, Log};
use crate::field::Tier;
use crate::fold::Fold;
use crate::line::{decode, encode, encoded_size};
use crate::rec::Rec;
use crate::vocab::{Node, Op};

/// A closed summary's bytes, at most (design §5.1).
pub const CLOSED_SUMMARY_BYTES: usize = 1024;

/// The order the fold's lines are kept in (their tag after `fold `), first
/// first; an `f` line goes with its owner.
fn fold_rank(line: &str) -> Option<u8> {
    let mut it = line.split(' ');
    let _ = it.next();
    Some(match it.next()? {
        "v=1" => 0,
        "totals" => 1,
        "closed" => 2,
        "phase" | "shell" => 3,
        "need" => 4,
        "last" => 5,
        "verdict" | "label" => 6,
        _ => return None,
    })
}

/// The owner a `fold f <owner> …` line belongs to, and the owner a `fold
/// last <slot> …` or `fold closed …` line is.
fn fold_owner(line: &str) -> Option<String> {
    let mut it = line.split(' ');
    let _ = it.next();
    match it.next()? {
        "last" => it.next().map(|slot| format!("l{slot}")),
        "closed" => Some("c".to_string()),
        _ => None,
    }
}

/// Whether `line` (and its newline) fits `room`, taking it if so.
const fn take(line: &str, room: &mut usize) -> bool {
    let need = line.len() + 1;
    if need <= *room {
        *room -= need;
        true
    } else {
        false
    }
}

/// The lines of `snap` (a fold snapshot) of `ranks` ([`fold_rank`]), each
/// with its fields, into `fold` as `room` allows; none but the head without
/// the head.
fn fold_within<'a>(
    snap: &'a [String],
    ranks: std::ops::RangeInclusive<u8>,
    fold: &mut Vec<(usize, &'a str)>,
    room: &mut usize,
) {
    for rank in ranks {
        for (i, line) in snap.iter().enumerate() {
            if fold_rank(line) != Some(rank) || (rank > 0 && fold.is_empty()) {
                continue;
            }
            if !take(line, room) {
                continue;
            }
            fold.push((i, line));
            let Some(owner) = fold_owner(line) else {
                continue;
            };
            for (j, field) in snap.iter().enumerate() {
                let owned = field
                    .strip_prefix("fold f ")
                    .and_then(|rest| rest.split(' ').next())
                    .is_some_and(|o| o == owner);
                if owned && take(field, room) {
                    fold.push((j, field));
                }
            }
        }
    }
}

impl Log {
    /// This closed log as a summary within [`CLOSED_SUMMARY_BYTES`], chosen by priority.
    #[must_use]
    pub fn closed_summary(&self) -> String {
        let session = |op: Op| {
            self.ring
                .iter()
                .find(|e| e.rec.node == Node::SESSION && e.rec.op == op)
        };
        let mut room = CLOSED_SUMMARY_BYTES;
        let head_room =
            "slog-sum v=1 ep= base= first= last= lost= newest_lost=".len() + 2 * 10 + 4 * 20;
        room = room.saturating_sub(head_room + 1);
        let mut recs: Vec<(u64, String)> = Vec::new();
        for op in [Op::Close, Op::Open] {
            if let Some(e) = session(op)
                && let Ok(l) = encode(&e.rec, Tier::Host)
            {
                let line = format!("r {l}");
                if take(&line, &mut room) {
                    recs.push((e.rec.seq, line));
                }
            }
        }
        let snap = self.fold.closed_snapshot();
        let mut fold: Vec<(usize, &str)> = Vec::new();
        fold_within(&snap, 0..=2, &mut fold, &mut room);
        let mut holes: Vec<(u64, u64, super::GapWhy)> = Vec::new();
        let mut first = self.first_seq;
        for &(lo, hi, why) in self.holes.iter().rev() {
            if take(&super::carry::hole_line(lo, hi, why), &mut room) {
                holes.push((lo, hi, why));
            } else {
                first = first.max(hi.saturating_add(1));
                break;
            }
        }
        holes.reverse();
        fold_within(&snap, 3..=6, &mut fold, &mut room);
        fold.sort_by_key(|(i, _)| *i);
        let kept: Vec<u64> = recs.iter().map(|(s, _)| *s).collect();
        let shed = self.ring.iter().filter(|e| !kept.contains(&e.rec.seq));
        let (shed_n, shed_newest) = shed.fold((0u64, 0u64), |(n, m), e| (n + 1, m.max(e.rec.seq)));
        recs.sort_by_key(|(s, _)| *s);
        let mut out = format!(
            "slog-sum v=1 ep={} base={} first={first} last={} lost={} newest_lost={}",
            self.ep,
            self.base_ep,
            self.last_seq,
            self.lost.saturating_add(shed_n),
            self.newest_lost.max(shed_newest)
        );
        for (_, line) in &recs {
            out.push('\n');
            out.push_str(line);
        }
        for (lo, hi, why) in holes {
            out.push('\n');
            out.push_str(&super::carry::hole_line(lo, hi, why));
        }
        for (_, line) in fold {
            out.push('\n');
            out.push_str(line);
        }
        out
    }

    /// The read-only log a [`Log::closed_summary`] wrote.
    /// # Errors
    /// A [`CarryError`] for a summary this build does not read.
    pub fn from_closed_summary(text: &str) -> Result<Log, CarryError> {
        if text.len() > 4 * CLOSED_SUMMARY_BYTES {
            return Err(CarryError::Bound("a summary past its bound"));
        }
        let mut lines = text.split('\n');
        let head = lines.next().ok_or(CarryError::Version)?;
        if !head.starts_with("slog-sum v=1 ") {
            return Err(CarryError::Version);
        }
        let hk = |k: &str| -> Result<u64, CarryError> {
            head.split(' ')
                .skip(2)
                .filter_map(|t| t.split_once('='))
                .find(|(key, _)| *key == k)
                .and_then(|(_, v)| v.parse().ok())
                .ok_or(CarryError::Line("a summary header field"))
        };
        let ep = u32::try_from(hk("ep")?).map_err(|_| CarryError::Line("an epoch"))?;
        let base = u32::try_from(hk("base")?).map_err(|_| CarryError::Line("an epoch"))?;
        let (first, last) = (hk("first")?, hk("last")?);
        let (lost, newest_lost) = (hk("lost")?, hk("newest_lost")?);
        if base > ep || first == 0 || first > last.saturating_add(1) || last > super::SEQ_LIMIT {
            return Err(CarryError::Shape("a summary's epochs or seqs out of order"));
        }
        let mut recs: Vec<Rec> = Vec::new();
        let mut holes: VecDeque<(u64, u64, super::GapWhy)> = VecDeque::new();
        let mut fold_lines: Vec<&str> = Vec::new();
        for line in lines {
            let (tag, rest) = line.split_once(' ').unwrap_or((line, ""));
            match tag {
                "r" => {
                    let r = decode(rest).map_err(|_| CarryError::Record("a summary record"))?;
                    if r.node != Node::SESSION
                        || recs.last().is_some_and(|p| p.seq >= r.seq)
                        || r.seq == 0
                        || r.seq > last
                    {
                        return Err(CarryError::Record("a summary record out of place"));
                    }
                    recs.push(r);
                }
                "hole" => {
                    let mut it = rest.split(' ');
                    let lo: u64 = it
                        .next()
                        .and_then(|v| v.parse().ok())
                        .ok_or(CarryError::Line("a hole"))?;
                    let hi: u64 = it
                        .next()
                        .and_then(|v| v.parse().ok())
                        .ok_or(CarryError::Line("a hole"))?;
                    if lo == 0
                        || lo > hi
                        || hi > last
                        || holes.back().is_some_and(|h| h.1 >= lo)
                        || holes.len() >= super::MAX_HOLES
                    {
                        return Err(CarryError::Shape("a summary's holes out of order"));
                    }
                    holes.push_back((lo, hi, super::carry::hole_why(it)));
                }
                "fold" => fold_lines.push(line),
                _ => return Err(CarryError::Line("a summary line this build does not read")),
            }
        }
        let fold = if fold_lines.is_empty() {
            Fold::new(ep)
        } else {
            Fold::from_snapshot(&fold_lines).map_err(CarryError::Fold)?
        };
        fold.agrees_with_open(&[]).map_err(CarryError::Fold)?;
        let mut log = Log::with_limits(ep, Limits::SESSION);
        log.base_ep = base;
        log.first_seq = first;
        log.last_seq = last;
        log.lost = lost;
        log.newest_lost = newest_lost.min(last);
        log.holes = holes;
        log.fold = fold;
        log.session_opened = true;
        log.session_closed = true;
        for rec in recs {
            let bytes = encoded_size(&rec, Tier::Content)
                .map_err(|_| CarryError::Record("a summary record past its bound"))?;
            if rec.op == Op::Open {
                log.session_kind = rec.k;
                log.session_t = rec.t;
            }
            log.session_bytes = log.session_bytes.saturating_add(bytes);
            log.heap = log.heap.saturating_add(super::entry_heap(&rec));
            log.bump_chain(Node::SESSION, true);
            log.ring.push_back(super::Entry {
                rec: rec.into(),
                bytes,
                ms: 0,
                mono: 0,
                row: super::rows::RowTag::NONE,
            });
            log.bytes = log.bytes.saturating_add(bytes);
        }
        if let Some(info) = log.nodes.get_mut(&Node::SESSION) {
            info.open = false;
        }
        Ok(log)
    }
}
