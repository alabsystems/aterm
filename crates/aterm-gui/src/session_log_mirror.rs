// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BUS MIRROR, GUI HALF (`docs/DESIGN-session-log-2026-10-15.md` §5.5): the two bridge-only verbs a bridge mirrors the session logs with, and what this process keeps for them.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, TryLockError};

use aterm_types::MutexExt as _;

use aterm_slog::{
    BusCopy, BusCursor, CoverWhy, LandRefused, Landed, Log, ShortId, Slot, SlotKind, Tier,
    cover_body,
};

/// The most closed sessions whose bus tails are kept.
pub(crate) const RETIRED_MAX: usize = 512;
/// The most estimated heap the kept tails take together.
pub(crate) const RETIRED_BYTES: usize = 16 * 1024 * 1024;
/// The most tombstones kept (each about 64 bytes); past it the oldest is dropped, and said ([`StoreMirror::retire`]).
pub(crate) const TOMBSTONES_MAX: usize = 4096;
/// The most `<sid> <cursor>` pairs one `log-landed` names (a take's slots).
pub(crate) const LANDED_PAIRS_MAX: usize = aterm_slog::TAKE_MAX;

/// What either verb answers a bridge whose recorded `hello` did not ask for
/// the mirror: a definite refusal (the bridge stops its mirror, said in
/// `fabric status`), never a take or a landing.
const MIRROR_OFF: &str = "ERR mirror off: the bridge's recorded hello did not say log=turns\n";

/// THE MIRROR'S STATE EVERY SESSION LOG OF THE STORE READS, under its own timeline lock ([`sync`]): whether the node mirrors, and the bus log's generation.
#[derive(Debug, Default)]
pub(crate) struct Shared {
    /// The node mirrors: a recorded `hello … log=turns` set it (sticky across
    /// the bridge's restarts), a recorded hello without `log=` or an `auto`
    /// bridge's `link mirror unretained` cleared it
    /// (`fabric::bridge_mirror_word`).
    served: AtomicBool,
    /// Bumped each time the bus log is replaced (`link log` `forgot=1`).
    generation: AtomicU64,
    /// Some log of the store has held a mirror cursor in this process (seen
    /// at its [`sync`], or at its [`StoreMirror::retire`]), or a
    /// predecessor's tombstone was adopted: sticky.
    cursors: AtomicBool,
}

impl Shared {
    /// Whether the node mirrors.
    pub(crate) fn served(&self) -> bool {
        self.served.load(Ordering::Acquire)
    }

    /// Whether some log of the store has held a mirror cursor.
    fn cursors(&self) -> bool {
        self.cursors.load(Ordering::Acquire)
    }

    fn note_cursor(&self) {
        if !self.cursors.load(Ordering::Relaxed) {
            self.cursors.store(true, Ordering::Release);
        }
    }

    /// The bus log's generation.
    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
}

/// A SESSION LOG MEETS THE MIRROR'S STATE, under its timeline's lock: a cursor of a replaced bus log (`seen` is the generation it last synced under) is forgotten, and while the node mirrors the log is engaged from its first record; a cursor it then holds is noted for the update's Commit ([`StoreMirror::past_draw`]).
pub(crate) fn sync(shared: &Shared, log: &mut Log, seen: &mut u64) {
    let now = shared.generation();
    if now != *seen {
        log.forget_mirror();
        *seen = now;
    }
    if shared.served() {
        log.engage_mirror();
    }
    if log.mirror_landed().is_some() {
        shared.note_cursor();
    }
}

/// A closed session the mirror dropped whole: its sid, where the bus held its log and that log's `ms` head.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Tombstone {
    pub(crate) sid: String,
    pub(crate) landed: BusCursor,
    pub(crate) ms_head: u64,
}

impl Tombstone {
    /// `<sid> <ep>:<seq>:<ms>:<head>`: a manifest row.
    #[cfg(any(unix, test))]
    pub(crate) fn row(&self) -> String {
        format!("{} {}:{}", self.sid, self.landed.word(), self.ms_head)
    }

    /// A manifest row read back, or `None` (the row is dropped).
    pub(crate) fn parse(row: &str) -> Option<Tombstone> {
        let (sid, rest) = row.split_once(' ')?;
        ShortId::new(sid)?;
        let (cursor, head) = rest.rsplit_once(':')?;
        let landed = BusCursor::parse(cursor)?;
        let ms_head: u64 = head.parse().ok()?;
        (ms_head > landed.ms).then(|| Tombstone {
            sid: sid.to_string(),
            landed,
            ms_head,
        })
    }

    /// Its one slot: the cover of everything past the cursor.
    fn slot(&self, t: u64) -> Slot {
        Slot {
            at: BusCursor::new(self.landed.ep, self.landed.seq, self.ms_head),
            kind: SlotKind::Cover,
            body: cover_body(
                self.landed.ep,
                self.landed.seq,
                self.landed.ms + 1,
                self.ms_head - self.landed.ms,
                CoverWhy::Dropped,
                t,
            ),
        }
    }
}

/// One closed session's bus tail.
struct Tail {
    sid: String,
    log: Log,
}

/// What the mirror keeps of closed sessions.
#[derive(Default)]
struct Retired {
    tails: VecDeque<Tail>,
    bytes: usize,
    tombstones: VecDeque<Tombstone>,
    /// What the last handoff manifest drew: each sid and its landed `ms`
    /// ([`StoreMirror::past_draw`]).
    #[cfg(any(unix, test))]
    drawn: Vec<(String, u64)>,
}

impl Retired {
    /// A tail as a tombstone: its cursor and its LIVE `ms` head (a closed
    /// session's records past a fence are owed too, and no successor numbers
    /// them again).
    fn tomb_of(tail: &Tail) -> Option<Tombstone> {
        let landed = tail.log.mirror_landed()?;
        let ms_head = tail.log.ms_head();
        (ms_head > landed.ms).then(|| Tombstone {
            sid: tail.sid.clone(),
            landed,
            ms_head,
        })
    }

    fn bound(&mut self) {
        while self.tails.len() > RETIRED_MAX || self.bytes > RETIRED_BYTES {
            let Some(old) = self.tails.pop_front() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(old.log.footprint());
            if let Some(t) = Retired::tomb_of(&old) {
                self.tombstones.push_back(t);
            }
        }
        let mut dropped = 0usize;
        while self.tombstones.len() > TOMBSTONES_MAX {
            self.tombstones.pop_front();
            dropped += 1;
        }
        if dropped > 0 {
            aterm_log::warn!(
                "session log mirror: {dropped} closed sessions' unmirrored tails dropped past \
                 the {TOMBSTONES_MAX} tombstones kept; their records never reach the bus and \
                 no cover says so"
            );
        }
    }
}

/// A DELIBERATELY BROKEN HANDLER, for a Tier-1 negative control: each is the real code with the one step its `LogMirror` mutant drops (`session_log_mirror_tests`).
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Broken {
    /// Every handler as built.
    #[default]
    None,
    /// `CloseDropsTail`: a closing session keeps no tail.
    DropTail,
    /// `DropUnadopted`: a sid not held while adopting is `gone`.
    GoneWhileAdopting,
    /// `LandUnderFence`: a live log's landing lifts the readers' fence first.
    LandUnderFence,
}

/// THE MIRROR'S STATE IN THE STORE: the closed sessions' tails and tombstones, the adoption latch, the take's round robin and the state each session log shares ([`Shared`], one small block a store, an `Arc` a log).
#[derive(Default)]
pub(crate) struct StoreMirror {
    /// Whether the node mirrors, and the bus log's generation: every session
    /// log of the store holds it ([`crate::session_log::SessionLog::bind_mirror`]).
    shared: Arc<Shared>,
    retired: Mutex<Retired>,
    /// A seamless successor has not yet registered every handed-off session:
    /// a sid this store does not hold may still arrive.
    adopting: AtomicBool,
    /// An update's Commit is reading the draw ([`Self::past_draw`]): no
    /// closed session's tail lands from then on, as no live one does under
    /// the readers' fence; lifted when the update stands down.
    committing: AtomicBool,
    /// One past the bus log's generation the last handoff manifest drew
    /// under ([`Self::draw_tombstones`]), 0 before any draw: an atomic, so a
    /// draw on a store that never mirrored takes no lock, and a cursor its
    /// export meets after it is still judged against it.
    #[cfg(any(unix, test))]
    drawn_gen: AtomicU64,
    /// The local id the last take started after.
    rr: AtomicU64,
    /// The negative controls' broken handler, if one is armed.
    #[cfg(test)]
    broken: Mutex<Broken>,
    /// What a test runs inside `log-landed`, between the store's read guard
    /// and the tails' lock (a Commit's reading that races the landing).
    #[cfg(test)]
    before_landing: Mutex<Option<LandingHook>>,
    /// One interleaving before ownership is acquired, after initial admission.
    #[cfg(any(test, debug_assertions))]
    before_mutation: Mutex<Option<MutationHook>>,
}

/// A one-shot test hook `log-landed` runs ([`StoreMirror::arm_before_landing`]).
#[cfg(test)]
type LandingHook = Box<dyn FnOnce(&StoreMirror) + Send>;

#[cfg(any(test, debug_assertions))]
type MutationHook = Box<dyn FnOnce() + Send>;

impl StoreMirror {
    /// The state every session log of this store holds.
    pub(crate) fn shared(&self) -> Arc<Shared> {
        Arc::clone(&self.shared)
    }

    /// A RECORDED `hello` (the control dispatch's, the owning lane's only): whether it said `log=turns`.
    pub(crate) fn set_served(&self, on: bool) {
        self.shared.served.store(on, Ordering::Release);
    }

    /// Whether the node mirrors.
    #[cfg(any(unix, test))]
    pub(crate) fn served(&self) -> bool {
        self.shared.served()
    }

    /// A session closed with its log: kept for the bus when its log was ever mirrored, or the node mirrors (it is then engaged from its first record, [`sync`]), and the bus does not hold all of it — judged against its live `ms` head.
    pub(crate) fn retire(&self, sid: &str, slog: &mut crate::session_log::SessionLog) {
        #[cfg(test)]
        if self.broken() == Broken::DropTail {
            return;
        }
        let log = slog.mirror_log();
        if log.mirror_landed().is_none() || log.mirror_complete() {
            return;
        }
        let tail = log.bus_tail();
        self.shared.note_cursor();
        let mut r = self.retired.lock_or_recover();
        r.bytes = r.bytes.saturating_add(tail.footprint());
        r.tails.push_back(Tail {
            sid: sid.to_string(),
            log: tail,
        });
        r.bound();
    }

    /// How many tails and tombstones it holds.
    #[cfg(test)]
    pub(crate) fn counts(&self) -> (usize, usize) {
        let r = self.retired.lock_or_recover();
        (r.tails.len(), r.tombstones.len())
    }

    /// `f` of session `sid`'s kept tail, if one is kept.
    #[cfg(test)]
    pub(crate) fn tail_view<R>(&self, sid: &str, f: impl FnOnce(&Log) -> R) -> Option<R> {
        let r = self.retired.lock_or_recover();
        r.tails.iter().find(|t| t.sid == sid).map(|t| f(&t.log))
    }

    /// Arm a broken handler (a negative control).
    #[cfg(test)]
    pub(crate) fn set_broken(&self, b: Broken) {
        *self.broken.lock_or_recover() = b;
    }

    #[cfg(test)]
    fn broken(&self) -> Broken {
        *self.broken.lock_or_recover()
    }

    /// The tails' lock, held by a test (a default instance takes none).
    #[cfg(test)]
    pub(crate) fn hold_retired(&self) -> impl Sized + '_ {
        self.retired.lock_or_recover()
    }

    /// Arm `f` to run once inside the next `log-landed` pair whose session is not live, before it takes the tails' lock.
    #[cfg(test)]
    pub(crate) fn arm_before_landing(&self, f: impl FnOnce(&StoreMirror) + Send + 'static) {
        *self.before_landing.lock_or_recover() = Some(Box::new(f));
    }

    #[cfg(test)]
    pub(crate) fn arm_before_mutation(&self, f: impl FnOnce() + Send + 'static) {
        *self.before_mutation.lock_or_recover() = Some(Box::new(f));
    }

    /// The adoption latch: on while a seamless successor's restore has not registered every handed-off session.
    pub(crate) fn set_adopting(&self, on: bool) {
        self.adopting.store(on, Ordering::Release);
    }

    /// Whether the adoption latch is on.
    pub(crate) fn adopting(&self) -> bool {
        self.adopting.load(Ordering::Acquire)
    }

    /// THE HANDOFF'S DRAW: every tail and tombstone as a tombstone row (`SessionHandoff::mirror_dropped`), each one's landed `ms` and the bus log's generation remembered for [`Self::past_draw`].
    #[cfg(any(unix, test))]
    pub(crate) fn draw_tombstones(&self) -> Vec<String> {
        self.drawn_gen.store(
            self.shared.generation().saturating_add(1),
            Ordering::Release,
        );
        if !self.shared.cursors() {
            return Vec::new();
        }
        let mut r = self.retired.lock_or_recover();
        let mut rows = Vec::new();
        let mut drawn = Vec::new();
        for t in r
            .tails
            .iter()
            .filter_map(Retired::tomb_of)
            .chain(r.tombstones.iter().cloned())
        {
            drawn.push((t.sid.clone(), t.landed.ms));
            rows.push(t.row());
        }
        r.drawn = drawn;
        rows
    }

    /// THE COMMIT'S READING of the draw: from now on no closed session's tail lands here (a landing answers `fenced`, and the bridge keeps its pin for the successor), and whether one landed past what the last manifest drew (or was all landed and forgotten since) — the successor's cover would then cover a record this process put on the bus after the draw, so the Commit stands down (a lossless retry).
    #[cfg(any(unix, test))]
    pub(crate) fn past_draw(&self) -> bool {
        self.committing.store(true, Ordering::Release);
        if !self.shared.cursors() {
            return false;
        }
        let drawn_gen = self.drawn_gen.load(Ordering::Acquire);
        if drawn_gen != 0 && drawn_gen != self.shared.generation().saturating_add(1) {
            return true;
        }
        let r = self.retired.lock_or_recover();
        r.drawn.iter().any(|(sid, ms)| {
            let now = r
                .tails
                .iter()
                .find(|t| t.sid == *sid)
                .and_then(|t| t.log.mirror_landed().map(|c| c.ms))
                .or_else(|| {
                    r.tombstones
                        .iter()
                        .find(|t| t.sid == *sid)
                        .map(|t| t.landed.ms)
                });
            now.is_none_or(|now| now > *ms)
        })
    }

    /// The update stood down: closed sessions' tails land again, each one that closed under the export's readers' fence with that fence lifted (its copy of the live log's, `aterm_slog::Log::bus_tail`), so its take reaches its close.
    #[cfg(any(unix, test))]
    pub(crate) fn lift_commit_fence(&self) {
        if self.shared.cursors() {
            let mut r = self.retired.lock_or_recover();
            for t in &mut r.tails {
                t.log.lift_read_fence();
            }
        }
        self.committing.store(false, Ordering::Release);
    }

    /// THE BUS LOG WAS REPLACED: one generation bump, which every live log meets at its next [`sync`] (no timeline is locked here), and every closed session's tail and tombstone starts over now — its next take one exact cover of what it no longer holds, from `ms` 1.
    pub(crate) fn forget(&self) {
        self.shared.generation.fetch_add(1, Ordering::AcqRel);
        if !self.shared.cursors() {
            return;
        }
        let mut r = self.retired.lock_or_recover();
        for t in &mut r.tails {
            t.log.forget_mirror();
            t.log.engage_mirror();
        }
        for t in &mut r.tombstones {
            t.landed = BusCursor::new(t.landed.ep, 0, 0);
        }
    }

    /// `fabric status`'s word on retained state no bridge serves: a closed session's tail or tombstone held while the owning bridge's recorded hello does not ask for the mirror (` mirror=unserved tails=<k> tombstones=<j>`), and nothing otherwise — with no lock, the link's included, while nothing is retained.
    pub(crate) fn unserved_facts(&self) -> String {
        if !self.shared.cursors() || crate::fabric::bridge_mirrors_logs() {
            return String::new();
        }
        let r = self.retired.lock_or_recover();
        if r.tails.is_empty() && r.tombstones.is_empty() {
            return String::new();
        }
        let lead = if crate::fabric::fabric_mirror_facts().is_empty() {
            " mirror=unserved"
        } else {
            ""
        };
        format!(
            "{lead} tails={} tombstones={}",
            r.tails.len(),
            r.tombstones.len()
        )
    }

    /// A seamless successor's: the predecessor's tombstones (its manifest's rows), each of which this process's mirror says as one cover.
    pub(crate) fn adopt_tombstones(&self, rows: &[String]) {
        let adopted: Vec<Tombstone> = rows
            .iter()
            .filter_map(|row| Tombstone::parse(row))
            .collect();
        if adopted.is_empty() {
            return;
        }
        self.shared.note_cursor();
        let mut r = self.retired.lock_or_recover();
        r.tombstones.extend(adopted);
        r.bound();
    }
}

/// `log-take`'s parsed request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TakeRequest {
    pub(crate) n: usize,
    pub(crate) tier: Tier,
    pub(crate) key: Option<[u8; 16]>,
}

const TAKE_USAGE: &str = "ERR usage: log-take [n=<1..64>] [tier=wire|meta] [key=<32 hex>]\n";
const LANDED_USAGE: &str =
    "ERR usage: log-landed <sid> <ep>:<seq>:<ms> [<sid> <ep>:<seq>:<ms>]...\n";

impl TakeRequest {
    /// The request's words read, or `None`.
    pub(crate) fn parse(rest: &str) -> Option<TakeRequest> {
        let mut req = TakeRequest {
            n: aterm_slog::TAKE_MAX,
            tier: Tier::Wire,
            key: None,
        };
        for tok in rest.split_whitespace() {
            let (k, v) = tok.split_once('=')?;
            match k {
                "n" => {
                    req.n = v
                        .parse()
                        .ok()
                        .filter(|n| (1..=aterm_slog::TAKE_MAX).contains(n))?;
                }
                "tier" => {
                    req.tier = match v {
                        "wire" => Tier::Wire,
                        "meta" => Tier::WireMeta,
                        _ => return None,
                    };
                }
                "key" => {
                    let mut key = [0u8; 16];
                    if v.len() != 32 || !aterm_codec::hex::decode_into(v.as_bytes(), &mut key) {
                        return None;
                    }
                    req.key = Some(key);
                }
                _ => return None,
            }
        }
        Some(req)
    }
}

/// THE ALIAS OF AN EDGE: `e` and the first 8 hex digits of HMAC-SHA256(`key`, sid) — stable for a node's key, so one edge is one actor on the bus, and no reader without the key can tell which session it is.
pub(crate) fn alias_of(key: &[u8; 16], sid: &ShortId) -> Option<ShortId> {
    let mut mac = aterm_digest::HmacSha256::new_from_slice(key).ok()?;
    mac.update(sid.as_str().as_bytes());
    let tag = mac.finalize();
    ShortId::new(&format!(
        "e{:02x}{:02x}{:02x}{:02x}",
        tag[0], tag[1], tag[2], tag[3]
    ))
}

/// One session's copy, taken under its lock.
struct Copied {
    sid: String,
    copy: BusCopy,
}

/// `log-take …`: the bridge-only take ([`crate::control`]'s dispatch hands it the lane's generation).
pub(crate) fn cmd_log_take(
    store: &crate::session_store::Store,
    rest: &str,
    generation: Option<crate::fabric::BridgeGeneration>,
) -> String {
    let Some(req) = TakeRequest::parse(rest) else {
        return TAKE_USAGE.to_string();
    };
    if !crate::fabric::lane_owns(generation) {
        return "OK 0 slots=0 stale=1\n".to_string();
    }
    if !crate::fabric::bridge_mirrors_logs() {
        return MIRROR_OFF.to_string();
    }
    crate::fabric::with_current_bridge(generation, || {
        if !crate::fabric::bridge_mirrors_logs() {
            return MIRROR_OFF.to_string();
        }
        take_owned(store, req)
    })
    .unwrap_or_else(|| "OK 0 slots=0 stale=1\n".to_string())
}

fn take_owned(store: &crate::session_store::Store, req: TakeRequest) -> String {
    let live = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        if g.mirror().adopting() {
            return "ERR busy adopting=1\n".to_string();
        }
        let mut live: Vec<(u64, String, std::sync::Arc<crate::SessionCtx>)> = g
            .live_handles()
            .map(|h| (h.local_id, h.sid.as_str().to_string(), h.ctx.clone()))
            .collect();
        live.sort_by_key(|(id, ..)| *id);
        let rr = &g.mirror().rr;
        let after = rr.load(Ordering::Relaxed);
        let start = live.iter().position(|(id, ..)| *id > after).unwrap_or(0);
        live.rotate_left(start);
        if let Some((id, ..)) = live.first() {
            rr.store(*id, Ordering::Relaxed);
        }
        live
    };
    let mut room = req.n;
    let mut more = false;
    let mut copies: Vec<Copied> = Vec::new();
    for (_, sid, ctx) in &live {
        if room == 0 {
            more = true;
            break;
        }
        let mut tl = match ctx.timeline.try_lock() {
            Ok(tl) => tl,
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
            Err(TryLockError::WouldBlock) => {
                more = true;
                continue;
            }
        };
        let now = tl.slog().now();
        let copy = tl.slog_mut().mirror_log().bus_copy(room, now);
        drop(tl);
        more |= copy.more();
        room = room.saturating_sub(copy.len());
        if !copy.is_empty() {
            copies.push(Copied {
                sid: sid.clone(),
                copy,
            });
        }
    }
    let mut tombs: Vec<(String, Slot)> = Vec::new();
    if room > 0 {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        let mut r = g.mirror().retired.lock_or_recover();
        let now = crate::session_log::host_stamp();
        for tail in r.tails.iter_mut() {
            if room == 0 {
                more = true;
                break;
            }
            let copy = tail.log.bus_copy(room, now);
            more |= copy.more();
            room = room.saturating_sub(copy.len());
            if !copy.is_empty() {
                copies.push(Copied {
                    sid: tail.sid.clone(),
                    copy,
                });
            }
        }
        for t in &r.tombstones {
            if room == 0 {
                more = true;
                break;
            }
            tombs.push((t.sid.clone(), t.slot(now.wall_ms)));
            room -= 1;
        }
    }
    let key = req.key;
    let alias = move |sid: &ShortId| key.as_ref().and_then(|k| alias_of(k, sid));
    let mut body = String::new();
    let mut slots = 0usize;
    for Copied { sid, copy } in copies {
        for s in copy.encode(req.tier, &alias).slots {
            push_slot(&mut body, &sid, &s);
            slots += 1;
        }
    }
    for (sid, s) in &tombs {
        push_slot(&mut body, sid, s);
        slots += 1;
    }
    let more = if more { " more=1" } else { "" };
    format!("OK {} slots={slots}{more}\n{body}", body.len())
}

fn push_slot(out: &mut String, sid: &str, s: &Slot) {
    out.push_str(sid);
    out.push(' ');
    out.push_str(&s.at.word());
    out.push(' ');
    out.push_str(&s.body);
    out.push('\n');
}

/// What one pair of a `log-landed` came to.
const fn landing_word(r: Result<Landed, LandRefused>) -> &'static str {
    match r {
        Ok(Landed::Moved) => "ok",
        Ok(Landed::Stale) => "stale",
        Err(LandRefused::Fenced) => "fenced",
        Err(LandRefused::Ahead) => "ahead",
    }
}

/// `log-landed <sid> <cursor>…`: the bridge-only landing.
pub(crate) fn cmd_log_landed(
    store: &crate::session_store::Store,
    rest: &str,
    generation: Option<crate::fabric::BridgeGeneration>,
) -> String {
    let toks: Vec<&str> = rest.split_whitespace().collect();
    if toks.is_empty() || !toks.len().is_multiple_of(2) || toks.len() / 2 > LANDED_PAIRS_MAX {
        return LANDED_USAGE.to_string();
    }
    let mut pairs: Vec<(&str, BusCursor)> = Vec::with_capacity(toks.len() / 2);
    for pair in toks.as_chunks::<2>().0 {
        let (Some(_), Some(at)) = (ShortId::new(pair[0]), BusCursor::parse(pair[1])) else {
            return LANDED_USAGE.to_string();
        };
        pairs.push((pair[0], at));
    }
    if !crate::fabric::lane_owns(generation) {
        return "OK stale=1\n".to_string();
    }
    if !crate::fabric::bridge_mirrors_logs() {
        return MIRROR_OFF.to_string();
    }
    #[cfg(any(test, debug_assertions))]
    {
        let before = {
            let g = store.read().unwrap_or_else(|p| p.into_inner());
            let before = g.mirror().before_mutation.lock_or_recover().take();
            drop(g);
            before
        };
        if let Some(before) = before {
            before();
        }
    }
    crate::fabric::with_current_bridge(generation, || {
        if !crate::fabric::bridge_mirrors_logs() {
            return MIRROR_OFF.to_string();
        }
        land_owned(store, pairs)
    })
    .unwrap_or_else(|| "OK stale=1\n".to_string())
}

fn land_owned(store: &crate::session_store::Store, pairs: Vec<(&str, BusCursor)>) -> String {
    #[cfg(test)]
    let broken = store
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .mirror()
        .broken();
    let mut words: Vec<&'static str> = Vec::with_capacity(pairs.len());
    for (sid, at) in pairs {
        let live = {
            let g = store.read().unwrap_or_else(|p| p.into_inner());
            g.by_sid(&aterm_session::SessionId::new(sid.to_string()))
                .map(|h| h.ctx.clone())
        };
        if let Some(ctx) = live {
            let mut tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let log = tl.slog_mut().mirror_log();
            #[cfg(test)]
            if broken == Broken::LandUnderFence && log.read_fence().is_some() {
                log.lift_read_fence();
                let landed = log.land(at);
                log.fence_reads();
                words.push(landing_word(landed));
                continue;
            }
            words.push(landing_word(log.land(at)));
            continue;
        }
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        let adopting = g.mirror().adopting();
        #[cfg(test)]
        {
            let hook = g.mirror().before_landing.lock_or_recover().take();
            if let Some(hook) = hook {
                hook(g.mirror());
            }
        }
        let mut r = g.mirror().retired.lock_or_recover();
        let committing = g.mirror().committing.load(Ordering::Acquire);
        let retired =
            r.tails.iter().any(|t| t.sid == sid) || r.tombstones.iter().any(|t| t.sid == sid);
        if committing && retired {
            words.push("fenced");
            continue;
        }
        if let Some(i) = r.tails.iter().position(|t| t.sid == sid) {
            let landed = r.tails[i].log.land(at);
            if landed.is_ok() && r.tails[i].log.mirror_complete() {
                if let Some(gone) = r.tails.remove(i) {
                    r.bytes = r.bytes.saturating_sub(gone.log.footprint());
                }
                words.push("done");
            } else {
                words.push(landing_word(landed));
            }
            continue;
        }
        if let Some(i) = r.tombstones.iter().position(|t| t.sid == sid) {
            let (ep, ms, head) = {
                let t = &r.tombstones[i];
                (t.landed.ep, t.landed.ms, t.ms_head)
            };
            let word = if at.ep != ep || at.ms <= ms {
                "stale"
            } else if at.ms > head {
                "ahead"
            } else if at.ms == head {
                r.tombstones.remove(i);
                "done"
            } else {
                r.tombstones[i].landed = at;
                "ok"
            };
            words.push(word);
            continue;
        }
        #[cfg(test)]
        let adopting = adopting && broken != Broken::GoneWhileAdopting;
        words.push(if adopting { "adopting" } else { "gone" });
    }
    format!("OK {}\n", words.join(" "))
}

/// THE BUS LOG WAS REPLACED (`link log` answered `forgot=1`): the offsets and records the mirror landed are another log's, so every cursor is forgotten — the live logs' at their next sync, the closed sessions' at once ([`StoreMirror::forget`]) — and each starts over from its first record, one exact cover for what it no longer holds.
pub(crate) fn forget_landed(store: &crate::session_store::Store) {
    store
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .mirror()
        .forget();
}

#[cfg(test)]
#[path = "session_log_mirror_tests.rs"]
mod tests;
