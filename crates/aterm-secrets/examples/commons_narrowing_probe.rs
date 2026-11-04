// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explain every Commons and `[everyone]` epoch's omissions against its predecessors, from a
//! copy of a membership-log `records` file named on the command line. Read-only: no network, no
//! identity, no decryption. It prints record-id prefixes, labels, recipient public ids,
//! generations and counts; never a capsule, a ciphertext or a key.
//!
//! For an epoch `E` minted by `A` at signed time `t`:
//! - `view(E)` is `E`'s strict causal past less `A`'s own `Epoch`/`Wrap`/`Put`/`Delete` records
//!   at `t` (the earlier part of the same mint batch, which the minting code signs after it chose
//!   its recipients; a `Remove` of the batch stays, as the removal path inserts it first);
//! - its predecessors are the causally maximal epochs of its audience in `view(E)`;
//! - its wrap chain is `E`, then `A`'s records linked by `previous` while each is a `Wrap` of `E`;
//! - an omission is a recipient some predecessor's `Wrap` names (anywhere in the file) that `E`'s
//!   chain does not.
//!
//! Each omission takes the first class that holds:
//! - `a`: the file's own evaluation removes the recipient (a `Remove` target's or a removed
//!   recipient, node removals included);
//! - `b`: a predecessor's chain names it, and `view(E)` holds that chain only as a prefix that
//!   stops before it;
//! - `b2`: a predecessor's `Wrap` of it lies outside `view(E)` and outside that chain (a gap close
//!   or backfill the minter had not seen);
//! - `c`: another predecessor head never wrapped it (the carry keeps the intersection);
//! - `b_ev`, `c_ev` (Commons only): the same against the newest `[everyone]` heads in `view(E)`,
//!   counted only where the file's left-out inference over `view(E)` reads it as left out of
//!   `[everyone]`;
//! - `d`: none of these: a candidate deliberate exclusion (a retirement exclude, a roster deny).
//!
//! `inferred` replays the left-out inference (`Log::audience_left_out`,
//! `Log::audience_left_out_recipient`, removal) over a log built from exactly `view(E)`, with no
//! roster (the roster is a sealed `[everyone]` entry this reader does not open).

use aterm_secrets::age::Recipient;
use aterm_secrets::membership_log::{
    Body, EpochAudience, Kind, Log, MemberId, Record, RecordId, Store,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::error::Error;
use std::fmt::Write as _;

type Res<T> = Result<T, Box<dyn Error>>;

fn short(id: &[u8]) -> String {
    aterm_codec::hex::encode_lower(&id[..6])
}

fn rshort(r: &Recipient) -> String {
    let s = r.to_bech32();
    format!("{}…{}", &s[..10], &s[s.len() - 6..])
}

struct Bits(Vec<u64>);

impl Bits {
    fn new(n: usize) -> Self {
        Self(vec![0; n.div_ceil(64)])
    }
    fn has(&self, i: usize) -> bool {
        self.0[i / 64] >> (i % 64) & 1 == 1
    }
    fn set(&mut self, i: usize) {
        self.0[i / 64] |= 1 << (i % 64);
    }
    fn clear(&mut self, i: usize) {
        self.0[i / 64] &= !(1 << (i % 64));
    }
    fn or(&mut self, other: &Bits) {
        for (a, b) in self.0.iter_mut().zip(&other.0) {
            *a |= *b;
        }
    }
    fn count(&self) -> usize {
        self.0.iter().map(|w| w.count_ones() as usize).sum()
    }
}

struct Graph<'a> {
    recs: Vec<&'a Record>,
    anc: Vec<Bits>,
    next: HashMap<RecordId, Vec<usize>>,
    wraps_of: HashMap<RecordId, Vec<(usize, Recipient)>>,
}

impl Graph<'_> {
    fn before(&self, a: usize, b: usize) -> bool {
        self.anc[b].has(a)
    }
    /// The minter's wraps of the epoch at `at`, in `previous`-link order from the epoch.
    fn chain(&self, at: usize) -> Vec<(usize, Recipient)> {
        let epoch = *self.recs[at].id();
        let mut out = Vec::new();
        let mut cur = epoch;
        while let Some(&k) = self.next.get(&cur).and_then(|v| v.first()) {
            match self.recs[k].body() {
                Body::Wrap {
                    epoch: e,
                    recipient,
                    ..
                } if *e == epoch => {
                    out.push((k, *recipient));
                    cur = *self.recs[k].id();
                }
                _ => break,
            }
        }
        out
    }
    fn wraps_full(&self, at: usize) -> BTreeSet<Recipient> {
        self.wraps_of
            .get(self.recs[at].id())
            .into_iter()
            .flatten()
            .map(|&(_, r)| r)
            .collect()
    }
    fn wraps_in(&self, at: usize, view: &Bits) -> BTreeSet<Recipient> {
        self.wraps_of
            .get(self.recs[at].id())
            .into_iter()
            .flatten()
            .filter(|&&(k, _)| view.has(k))
            .map(|&(_, r)| r)
            .collect()
    }
    fn maximal(&self, epochs: &[usize], audience: &EpochAudience, view: &Bits) -> Vec<usize> {
        let inside: Vec<usize> = epochs
            .iter()
            .copied()
            .filter(|&j| view.has(j) && audience_of(self.recs[j]) == Some(audience))
            .collect();
        inside
            .iter()
            .copied()
            .filter(|&j| !inside.iter().any(|&k| k != j && self.before(j, k)))
            .collect()
    }
}

fn audience_of(r: &Record) -> Option<&EpochAudience> {
    match r.body() {
        Body::Epoch { audience, .. } => Some(audience),
        _ => None,
    }
}

fn generation_of(r: &Record) -> u64 {
    match r.body() {
        Body::Epoch { generation, .. } => *generation,
        _ => 0,
    }
}

#[derive(Default)]
struct Names {
    actor: HashMap<MemberId, String>,
    recipient: HashMap<Recipient, String>,
    actor_recipient: HashMap<MemberId, Recipient>,
}

impl Names {
    fn of_actor(&self, a: &MemberId) -> String {
        self.actor
            .get(a)
            .cloned()
            .unwrap_or_else(|| format!("actor:{}", short(a)))
    }
    fn of_recipient(&self, r: &Recipient) -> String {
        self.recipient.get(r).map_or_else(
            || format!("unlabelled {}", rshort(r)),
            |l| format!("{l} {}", rshort(r)),
        )
    }
}

/// What the left-out inference over one view says about one recipient.
#[derive(Clone, Copy, Default)]
struct Inferred {
    removed: bool,
    seated: bool,
    seat_left_out_audience: bool,
    seat_left_out_everyone: bool,
    recipient_left_out_audience: bool,
    recipient_left_out_everyone: bool,
}

struct View {
    log: Log,
    removed: BTreeSet<Recipient>,
    /// Seat recipients with an active (not left-out) seat, and with only left-out seats.
    active_seat: BTreeSet<Recipient>,
    left_out_seat: BTreeSet<Recipient>,
    seat_everyone_left: BTreeSet<Recipient>,
    seat_audience_left: BTreeSet<Recipient>,
    seats: BTreeSet<Recipient>,
}

fn build_view(
    full: &Log,
    g: &Graph<'_>,
    view: &Bits,
    audience: &EpochAudience,
    names: &Names,
) -> Res<View> {
    let mut log = Log::new(full.genesis().clone())?;
    for (i, r) in g.recs.iter().enumerate().skip(1) {
        if view.has(i) {
            log.insert((*r).clone())?;
        }
    }
    if log.waiting() != 0 || log.held_count() != view.count() {
        return Err(format!(
            "view not causally closed: held {} waiting {} expected {}",
            log.held_count(),
            log.waiting(),
            view.count()
        )
        .into());
    }
    let membership = log.membership();
    let mut removed = log.removed_recipients();
    for actor in membership.removed().keys() {
        if let Some(r) = names.actor_recipient.get(actor) {
            removed.insert(*r);
        }
    }
    let mut active_seat = BTreeSet::new();
    let mut left_seat = BTreeSet::new();
    let mut seat_everyone_left = BTreeSet::new();
    let mut seat_audience_left = BTreeSet::new();
    let mut seats = BTreeSet::new();
    for (actor, m) in membership.members() {
        let r = m.binding.recipient();
        seats.insert(r);
        let lo_a = log.audience_left_out(audience, actor);
        let lo_e = log.audience_left_out(&EpochAudience::Everyone, actor);
        if lo_a {
            seat_audience_left.insert(r);
        }
        if lo_e {
            seat_everyone_left.insert(r);
        }
        if log.recipient_is_removed(&r) || membership.member_node_removed(m) {
            removed.insert(r);
            continue;
        }
        if lo_a || lo_e {
            left_seat.insert(r);
        } else {
            active_seat.insert(r);
        }
    }
    let left_out_seat = left_seat.difference(&active_seat).copied().collect();
    Ok(View {
        log,
        removed,
        active_seat,
        left_out_seat,
        seat_everyone_left,
        seat_audience_left,
        seats,
    })
}

impl View {
    fn infer(&self, audience: &EpochAudience, r: &Recipient) -> Inferred {
        Inferred {
            removed: self.removed.contains(r),
            seated: self.seats.contains(r),
            seat_left_out_audience: self.seat_audience_left.contains(r),
            seat_left_out_everyone: self.seat_everyone_left.contains(r),
            recipient_left_out_audience: self.log.audience_left_out_recipient(audience, r),
            recipient_left_out_everyone: self
                .log
                .audience_left_out_recipient(&EpochAudience::Everyone, r),
        }
    }
    fn everyone_left(&self, i: &Inferred) -> bool {
        i.seat_left_out_everyone || i.recipient_left_out_everyone
    }
    /// The carried set the minting code computes (`carried_audience_recipients` over the
    /// remaining members), with no roster: active seats plus the seatless intersection.
    fn predicted(
        &self,
        audience: &EpochAudience,
        heads_view: &[BTreeSet<Recipient>],
    ) -> BTreeSet<Recipient> {
        let keep = |r: &Recipient| {
            !self.removed.contains(r)
                && !self.left_out_seat.contains(r)
                && !self.log.audience_left_out_recipient(audience, r)
                && !self
                    .log
                    .audience_left_out_recipient(&EpochAudience::Everyone, r)
        };
        let mut out: BTreeSet<Recipient> = self
            .active_seat
            .iter()
            .copied()
            .filter(|r| keep(r))
            .collect();
        if let Some((first, rest)) = heads_view.split_first() {
            for r in first {
                if keep(r) && rest.iter().all(|s| s.contains(r)) {
                    out.insert(*r);
                }
            }
        }
        out
    }
}

const CLASSES: [&str; 7] = ["a", "b", "b2", "c", "b_ev", "c_ev", "d"];

#[allow(clippy::too_many_lines)]
fn main() -> Res<()> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: commons_narrowing_probe <records file>")?;
    let bytes = std::fs::read(&path)?;
    let store = Store::recover("alab", &bytes)?;
    let log = store.log().ok_or("no log")?;
    if log.waiting() != 0 {
        return Err("records wait for missing parents".into());
    }
    let recs: Vec<&Record> = log.records().collect();
    let n = recs.len();
    let idx: HashMap<RecordId, usize> =
        recs.iter().enumerate().map(|(i, r)| (*r.id(), i)).collect();
    let mut anc: Vec<Bits> = Vec::with_capacity(n);
    for (i, r) in recs.iter().enumerate() {
        let mut bits = Bits::new(n);
        for p in r.parents() {
            let j = idx[p];
            assert!(j < i, "file order is not topological");
            bits.or(&anc[j]);
            bits.set(j);
        }
        anc.push(bits);
    }
    let mut next: HashMap<RecordId, Vec<usize>> = HashMap::new();
    let mut wraps_of: HashMap<RecordId, Vec<(usize, Recipient)>> = HashMap::new();
    let mut previous_not_parent = 0usize;
    for (i, r) in recs.iter().enumerate() {
        if let Some(p) = r.previous() {
            next.entry(*p).or_default().push(i);
            if !r.parents().contains(p) {
                previous_not_parent += 1;
            }
        }
        if let Body::Wrap {
            epoch, recipient, ..
        } = r.body()
        {
            wraps_of.entry(*epoch).or_default().push((i, *recipient));
        }
    }
    let forks = next.values().filter(|v| v.len() > 1).count();
    let g = Graph {
        recs,
        anc,
        next,
        wraps_of,
    };

    // Names: current labels first, then historical ones.
    let membership = log.membership();
    let mut names = Names::default();
    for r in &g.recs {
        match r.body() {
            Body::Genesis { founders, .. } => {
                for f in founders {
                    names
                        .recipient
                        .entry(f.recipient)
                        .or_insert_with(|| format!("{}(hist)", f.label));
                    if let Some(b) = &f.binding {
                        names
                            .actor
                            .entry(b.actor())
                            .or_insert_with(|| format!("{}(hist)", f.label));
                        names.actor_recipient.insert(b.actor(), f.recipient);
                    }
                }
            }
            Body::Admit { entrant, .. } => {
                let b = entrant.request.binding();
                names
                    .actor
                    .insert(b.actor(), format!("{}(hist)", entrant.label));
                names
                    .recipient
                    .insert(b.recipient(), format!("{}(hist)", entrant.label));
                names.actor_recipient.insert(b.actor(), b.recipient());
            }
            _ => {}
        }
    }
    for f in membership.unproved() {
        names
            .recipient
            .insert(f.recipient, format!("{}(unproved)", f.label));
    }
    for (actor, m) in membership.members() {
        names.actor.insert(*actor, m.label.clone());
        names
            .recipient
            .insert(m.binding.recipient(), m.label.clone());
    }

    let mut removed_full = log.removed_recipients();
    for actor in membership.removed().keys() {
        if let Some(r) = names.actor_recipient.get(actor) {
            removed_full.insert(*r);
        }
    }
    let removed_actors: BTreeSet<MemberId> = membership.removed().keys().copied().collect();

    let epochs: Vec<usize> = (0..n)
        .filter(|&i| g.recs[i].kind() == Kind::Epoch)
        .collect();
    let by_removed = epochs
        .iter()
        .filter(|&&i| removed_actors.contains(g.recs[i].actor()))
        .count();
    let wraps_by_removed = (0..n)
        .filter(|&i| g.recs[i].kind() == Kind::Wrap && removed_actors.contains(g.recs[i].actor()))
        .count();
    let chains_sorted = epochs
        .iter()
        .filter(|&&e| {
            let c: Vec<Recipient> = g.chain(e).into_iter().map(|(_, r)| r).collect();
            c.windows(2).all(|w| w[0] < w[1])
        })
        .count();
    let mut out = String::new();
    writeln!(out, "# commons_narrowing_probe")?;
    writeln!(
        out,
        "records {n}; members {}; removed actors {}; removed recipients (file evaluation) {}; quorums {}; voided {}",
        membership.members().len(),
        removed_actors.len(),
        removed_full.len(),
        membership.quorums().len(),
        membership.voided().len()
    )?;
    writeln!(
        out,
        "epochs {}; epochs by removed actors {by_removed}; wraps by removed actors {wraps_by_removed}; previous-not-a-parent {previous_not_parent}; previous forks {forks}; minter chains in ascending recipient order {chains_sorted}/{}",
        epochs.len(),
        epochs.len()
    )?;

    for &e in &epochs {
        let c: Vec<Recipient> = g.chain(e).into_iter().map(|(_, r)| r).collect();
        if !c.windows(2).all(|w| w[0] < w[1]) {
            let descents = c.windows(2).filter(|w| w[0] >= w[1]).count();
            writeln!(
                out,
                "unsorted chain: gen {} {} by {}: {} wraps, {descents} descents",
                generation_of(g.recs[e]),
                short(g.recs[e].id()),
                names.of_actor(g.recs[e].actor()),
                c.len()
            )?;
        }
    }

    let mut totals: BTreeMap<(String, &str), usize> = BTreeMap::new();
    let mut details: Vec<String> = Vec::new();
    let mut mismatches: Vec<String> = Vec::new();
    let mut dropped: Vec<(usize, Recipient, &str)> = Vec::new();
    for audience in [EpochAudience::Commons, EpochAudience::Everyone] {
        let aname = if audience == EpochAudience::Commons {
            "Commons"
        } else {
            "[everyone]"
        };
        writeln!(out, "\n## {aname} epochs\n")?;
        writeln!(
            out,
            "| # | gen | epoch | minter | chain wraps | all wraps | view | 64 parents | predecessors gen:id(held/chain, wraps in view/all) | omitted | a | b | b2 | c | b_ev | c_ev | d | inferred excluded | later wrapped | predicted | chain∖pred | pred∖chain |"
        )?;
        writeln!(out, "|{}", "---|".repeat(23))?;
        let mut row = 0usize;
        for &e in &epochs {
            if audience_of(g.recs[e]) != Some(&audience) {
                continue;
            }
            row += 1;
            let er = g.recs[e];
            let mut view = Bits(g.anc[e].0.clone());
            let mut batch = 0usize;
            for (j, rj) in g.recs.iter().enumerate().take(e) {
                if view.has(j)
                    && rj.actor() == er.actor()
                    && rj.authored_ms() == er.authored_ms()
                    && matches!(
                        rj.kind(),
                        Kind::Epoch | Kind::Wrap | Kind::Put | Kind::Delete
                    )
                {
                    view.clear(j);
                    batch += 1;
                }
            }
            let chain = g.chain(e);
            let mine: BTreeSet<Recipient> = chain.iter().map(|&(_, r)| r).collect();
            let all_e = g.wraps_full(e);
            let preds = g.maximal(&epochs, &audience, &view);
            let ev_preds = if audience == EpochAudience::Commons {
                g.maximal(&epochs, &EpochAudience::Everyone, &view)
            } else {
                Vec::new()
            };
            let pred_view: Vec<BTreeSet<Recipient>> =
                preds.iter().map(|&p| g.wraps_in(p, &view)).collect();
            let pred_full: Vec<BTreeSet<Recipient>> =
                preds.iter().map(|&p| g.wraps_full(p)).collect();
            let pred_chain: Vec<Vec<(usize, Recipient)>> =
                preds.iter().map(|&p| g.chain(p)).collect();
            let pred_held: Vec<usize> = pred_chain
                .iter()
                .map(|c| c.iter().filter(|&&(k, _)| view.has(k)).count())
                .collect();
            let mut prefix_violations = 0usize;
            for (c, &h) in pred_chain.iter().zip(&pred_held) {
                if c.iter()
                    .enumerate()
                    .any(|(pos, &(k, _))| view.has(k) != (pos < h))
                {
                    prefix_violations += 1;
                }
            }
            let ev_view: Vec<BTreeSet<Recipient>> =
                ev_preds.iter().map(|&p| g.wraps_in(p, &view)).collect();
            let ev_full: Vec<BTreeSet<Recipient>> =
                ev_preds.iter().map(|&p| g.wraps_full(p)).collect();
            let v = build_view(log, &g, &view, &audience, &names)?;
            let predicted = v.predicted(&audience, &pred_view);
            let omitted: BTreeSet<Recipient> = pred_full
                .iter()
                .flatten()
                .copied()
                .filter(|r| !mine.contains(r))
                .collect();
            let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
            let mut inferred_excluded = 0usize;
            let mut later = 0usize;
            for r in &omitted {
                let inf = v.infer(&audience, r);
                let in_chain_beyond = preds.iter().enumerate().any(|(pi, _)| {
                    !pred_view[pi].contains(r)
                        && pred_chain[pi]
                            .iter()
                            .position(|&(_, x)| x == *r)
                            .is_some_and(|pos| pos >= pred_held[pi])
                });
                let outside = preds.iter().enumerate().any(|(pi, _)| {
                    pred_full[pi].contains(r)
                        && !pred_view[pi].contains(r)
                        && !pred_chain[pi]
                            .iter()
                            .position(|&(_, x)| x == *r)
                            .is_some_and(|pos| pos >= pred_held[pi])
                });
                let absent_head = pred_full.iter().any(|s| !s.contains(r));
                let ev_left = v.everyone_left(&inf);
                let ev_beyond = ev_left
                    && ev_preds
                        .iter()
                        .enumerate()
                        .any(|(fi, _)| ev_full[fi].contains(r) && !ev_view[fi].contains(r));
                let ev_absent = ev_left && ev_full.iter().any(|s| !s.contains(r));
                let class = if removed_full.contains(r) {
                    "a"
                } else if in_chain_beyond {
                    "b"
                } else if outside {
                    "b2"
                } else if absent_head {
                    "c"
                } else if ev_beyond {
                    "b_ev"
                } else if ev_absent {
                    "c_ev"
                } else {
                    "d"
                };
                *counts.entry(class).or_default() += 1;
                *totals.entry((aname.to_owned(), class)).or_default() += 1;
                if audience == EpochAudience::Commons {
                    dropped.push((e, *r, class));
                }
                let excluded = inf.removed
                    || inf.seat_left_out_audience
                    || inf.seat_left_out_everyone
                    || inf.recipient_left_out_audience
                    || inf.recipient_left_out_everyone
                    || (!v.active_seat.contains(r) && !pred_view.iter().all(|s| s.contains(r)));
                if excluded {
                    inferred_excluded += 1;
                }
                if all_e.contains(r) {
                    later += 1;
                }
                if class == "d" || !excluded {
                    details.push(format!(
                        "{aname} gen {} {} by {}: {} class {class}; view: removed {} seated {} seat-left-out(aud {} ev {}) recipient-left-out(aud {} ev {}) in-all-heads {} inferred-excluded {excluded}; removed in file {}; later wrapped {}",
                        generation_of(er),
                        short(er.id()),
                        names.of_actor(er.actor()),
                        names.of_recipient(r),
                        inf.removed,
                        inf.seated,
                        inf.seat_left_out_audience,
                        inf.seat_left_out_everyone,
                        inf.recipient_left_out_audience,
                        inf.recipient_left_out_everyone,
                        pred_view.iter().all(|s| s.contains(r)),
                        removed_full.contains(r),
                        all_e.contains(r),
                    ));
                }
            }
            for (what, set) in [
                (
                    "wrapped though the replayed inference excludes it",
                    mine.difference(&predicted),
                ),
                (
                    "not wrapped though the replayed inference keeps it",
                    predicted.difference(&mine),
                ),
            ] {
                for r in set {
                    let inf = v.infer(&audience, r);
                    mismatches.push(format!(
                        "{aname} gen {} {} by {}: {} {what}; view: removed {} seated {} seat-left-out(aud {} ev {}) recipient-left-out(aud {} ev {}) in-all-heads {} in-a-head {}",
                        generation_of(er),
                        short(er.id()),
                        names.of_actor(er.actor()),
                        names.of_recipient(r),
                        inf.removed,
                        inf.seated,
                        inf.seat_left_out_audience,
                        inf.seat_left_out_everyone,
                        inf.recipient_left_out_audience,
                        inf.recipient_left_out_everyone,
                        !pred_view.is_empty() && pred_view.iter().all(|s| s.contains(r)),
                        pred_view.iter().any(|s| s.contains(r)),
                    ));
                }
            }
            if prefix_violations > 0 {
                details.push(format!(
                    "{aname} gen {} {}: {prefix_violations} predecessor chains held out of previous-link order",
                    generation_of(er),
                    short(er.id())
                ));
            }
            let mut pcol = String::new();
            for (pi, &p) in preds.iter().enumerate() {
                if pi > 0 {
                    pcol.push_str("; ");
                }
                write!(
                    pcol,
                    "{}:{}({}/{}, {}/{})",
                    generation_of(g.recs[p]),
                    short(g.recs[p].id()),
                    pred_held[pi],
                    pred_chain[pi].len(),
                    pred_view[pi].len(),
                    pred_full[pi].len()
                )?;
            }
            if !ev_preds.is_empty() {
                pcol.push_str(" ‖ ev ");
                for (fi, &f) in ev_preds.iter().enumerate() {
                    if fi > 0 {
                        pcol.push_str("; ");
                    }
                    let fc = g.chain(f);
                    let held = fc.iter().filter(|&&(k, _)| view.has(k)).count();
                    write!(
                        pcol,
                        "{}:{}({}/{}, {}/{})",
                        generation_of(g.recs[f]),
                        short(g.recs[f].id()),
                        held,
                        fc.len(),
                        ev_view[fi].len(),
                        ev_full[fi].len()
                    )?;
                }
            }
            if preds.is_empty() {
                pcol = format!("none{pcol}");
            }
            let c = |k: &str| counts.get(k).copied().unwrap_or(0);
            writeln!(
                out,
                "| {row} | {} | {} | {} | {} | {} | {}{} | {} | {pcol} | {} | {} | {} | {} | {} | {} | {} | {} | {inferred_excluded} | {later} | {} | {} | {} |",
                generation_of(er),
                short(er.id()),
                names.of_actor(er.actor()),
                mine.len(),
                all_e.len(),
                view.count(),
                if batch > 0 {
                    format!(" (−{batch} batch)")
                } else {
                    String::new()
                },
                er.parents().len() >= aterm_secrets::membership_log::MAX_PARENTS,
                omitted.len(),
                c("a"),
                c("b"),
                c("b2"),
                c("c"),
                c("b_ev"),
                c("c_ev"),
                c("d"),
                predicted.len(),
                mine.difference(&predicted).count(),
                predicted.difference(&mine).count(),
            )?;
        }
    }
    writeln!(out, "\n## Totals by class\n")?;
    writeln!(out, "| audience | {} |", CLASSES.join(" | "))?;
    writeln!(out, "|---|{}", "---|".repeat(CLASSES.len()))?;
    for aname in ["Commons", "[everyone]"] {
        let cells: Vec<String> = CLASSES
            .iter()
            .map(|k| {
                totals
                    .get(&(aname.to_owned(), *k))
                    .copied()
                    .unwrap_or(0)
                    .to_string()
            })
            .collect();
        writeln!(out, "| {aname} | {} |", cells.join(" | "))?;
    }
    writeln!(
        out,
        "\n## Omissions in class d, or not excluded by the replayed inference\n"
    )?;
    if details.is_empty() {
        writeln!(out, "none")?;
    }
    for d in &details {
        writeln!(out, "- {d}")?;
    }
    writeln!(
        out,
        "\n## Chain recipients the replayed inference does not predict, and the reverse\n"
    )?;
    if mismatches.is_empty() {
        writeln!(out, "none")?;
    }
    for d in &mismatches {
        writeln!(out, "- {d}")?;
    }

    // Live seats against the newest Commons epochs.
    let all = Bits({
        let mut b = Bits::new(n);
        for i in 0..n {
            b.set(i);
        }
        b.0
    });
    writeln!(out, "\n## Live seats and the newest Commons epochs\n")?;
    let live: Vec<(MemberId, String, Recipient)> = membership
        .members()
        .iter()
        .filter(|(_, m)| {
            !log.recipient_is_removed(&m.binding.recipient()) && !membership.member_node_removed(m)
        })
        .map(|(a, m)| (*a, m.label.clone(), m.binding.recipient()))
        .collect();
    writeln!(
        out,
        "members {}; live seats (not removed, node not removed) {}",
        membership.members().len(),
        live.len()
    )?;
    for audience in [EpochAudience::Commons, EpochAudience::Everyone] {
        let heads = g.maximal(&epochs, &audience, &all);
        for &h in &heads {
            let w = g.wraps_full(h);
            let holders: Vec<&str> = live
                .iter()
                .filter(|(_, _, r)| w.contains(r))
                .map(|(_, l, _)| l.as_str())
                .collect();
            let others: Vec<String> = w
                .iter()
                .filter(|r| !live.iter().any(|(_, _, x)| x == *r))
                .map(|r| names.of_recipient(r))
                .collect();
            writeln!(
                out,
                "- newest {} head gen {} {} by {}: {} wraps; live seats holding it {} {:?}; other recipients {:?}",
                if audience == EpochAudience::Commons {
                    "Commons"
                } else {
                    "[everyone]"
                },
                generation_of(g.recs[h]),
                short(g.recs[h].id()),
                names.of_actor(g.recs[h].actor()),
                w.len(),
                holders.len(),
                if audience == EpochAudience::Commons {
                    holders.clone()
                } else {
                    Vec::new()
                },
                others
            )?;
        }
    }
    let commons: Vec<usize> = epochs
        .iter()
        .copied()
        .filter(|&i| audience_of(g.recs[i]) == Some(&EpochAudience::Commons))
        .collect();
    // Per live seat: the causally maximal Commons epochs among those with a `Wrap` to its
    // recipient (a raw wrap, not a decryption), grouped by that set.
    let mut by_set: BTreeMap<Vec<(u64, usize)>, Vec<String>> = BTreeMap::new();
    let heads = g.maximal(&epochs, &EpochAudience::Commons, &all);
    for (_, label, r) in &live {
        let opens: Vec<usize> = commons
            .iter()
            .copied()
            .filter(|&c| g.wraps_full(c).contains(r))
            .collect();
        let mut newest: Vec<(u64, usize)> = opens
            .iter()
            .copied()
            .filter(|&c| !opens.iter().any(|&d| d != c && g.before(c, d)))
            .map(|c| (generation_of(g.recs[c]), c))
            .collect();
        newest.sort_unstable_by(|a, b| b.cmp(a));
        by_set.entry(newest).or_default().push(label.clone());
    }
    writeln!(
        out,
        "\n| newest Commons epochs a live seat holds a wrap of (gen:id by minter) | highest gen | holds every newest head | seats | labels |"
    )?;
    writeln!(out, "|---|---|---|---|---|")?;
    let mut rows: Vec<_> = by_set.into_iter().collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    for (set, labels) in rows {
        let cells: Vec<String> = set
            .iter()
            .map(|&(generation, at)| {
                format!(
                    "{generation}:{} by {}{}",
                    short(g.recs[at].id()),
                    names.of_actor(g.recs[at].actor()),
                    if heads.contains(&at) {
                        " (newest head)"
                    } else {
                        ""
                    }
                )
            })
            .collect();
        writeln!(
            out,
            "| {} | {} | {} | {} | {} |",
            if cells.is_empty() {
                "none".to_owned()
            } else {
                cells.join("; ")
            },
            set.first()
                .map_or_else(|| "-".to_owned(), |&(generation, _)| generation.to_string()),
            heads.iter().all(|h| set.iter().any(|&(_, at)| at == *h)),
            labels.len(),
            labels.join(", ")
        )?;
    }
    writeln!(
        out,
        "\n## First Commons omission of each live seat (causally minimal omitting epochs)\n"
    )?;
    writeln!(
        out,
        "| first omitting epochs (gen:id by minter, class) | seats | labels |"
    )?;
    writeln!(out, "|---|---|---|")?;
    let mut first_by: BTreeMap<Vec<(u64, usize, &str)>, Vec<String>> = BTreeMap::new();
    let mut first_class: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, label, r) in &live {
        let mine: Vec<(usize, &str)> = dropped
            .iter()
            .filter(|(_, x, _)| x == r)
            .map(|&(e, _, c)| (e, c))
            .collect();
        let mut first: Vec<(u64, usize, &str)> = mine
            .iter()
            .filter(|&&(e, _)| !mine.iter().any(|&(d, _)| d != e && g.before(d, e)))
            .map(|&(e, c)| (generation_of(g.recs[e]), e, c))
            .collect();
        first.sort_unstable();
        first.dedup();
        for &(_, _, c) in &first {
            *first_class.entry(c).or_default() += 1;
        }
        first_by.entry(first).or_default().push(label.clone());
    }
    for (set, labels) in &first_by {
        let cells: Vec<String> = set
            .iter()
            .map(|&(generation, at, c)| {
                format!(
                    "{generation}:{} by {} ({c})",
                    short(g.recs[at].id()),
                    names.of_actor(g.recs[at].actor())
                )
            })
            .collect();
        writeln!(
            out,
            "| {} | {} | {} |",
            if cells.is_empty() {
                "never omitted".to_owned()
            } else {
                cells.join("; ")
            },
            labels.len(),
            labels.join(", ")
        )?;
    }
    writeln!(
        out,
        "\nfirst-omission classes over live seats (one per minimal epoch): {first_class:?}"
    )?;
    print!("{out}");
    Ok(())
}
