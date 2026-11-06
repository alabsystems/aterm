// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! P3's binding (3) for waiting writes (`docs/design/one-mesh-proofs/P3-no-lost-edit.md`,
//! "Binding"; `crates/aterm-mesh-core/src/no_lost_edit_island.rs` §8 to §10): the functions a
//! publish, a sync and a vault pass keep this Mac's `[everyone]` writes with, and the ones a
//! publish whose records write a name directly runs, driven on m22 of the removal lab
//! ([`convergence::six_with`]) through every event sequence of small universes of the island's
//! waiting World, and held to its step after every step.
//!
//! [`WWorld`] and [`wstep`] transliterate the island's `WWorld` and `wstep` one for one, its folds
//! (`landW`, `laterLands`, `landingOf`, `deferredW`, `verdictOf`, `passVerdict`, `signBody`,
//! `signW`, `settleW`, `pushW`, `pubBase`, `turnFree`, `heldIn`, `publishOk`, `keepBody`) included,
//! asking the compiled kernels ([`COMPILED`]: `pending_keeps`, `pass_signs`, `write_signs`,
//! `write_overtaken`, `drafted_keeps`, `draft_refuses`).
//!
//! The real side ([`WLab`]) is m22's home. A publish whose writes wait takes m22's draft turn
//! ([`log_entries::draft_turn`]) and claim ([`log_entries::claim_draft`]) at once, drafts its
//! writes as the code does (what m22's held drafts leave at each name,
//! [`log_entries::held_values_or_unread`], merged into each write's bases by
//! [`log_entries::everyone_edit`]; the draft refuses over a hold that does not read,
//! [`nle::draft_refuses`]), and keeps them pending ([`log_entries::wait_for_key`]). The event
//! happens on m22 only where the code's draft made exactly the event's writes: the island's
//! publish takes its writes and bases as its input, and a draft that made others is another
//! event. A direct draft is m22's own `Put` of a name, drafted on its log in its turn (the turn and
//! the claim held until its hold), held ([`log_entries::hold_drafted`]), kept
//! ([`log_entries::keep_drafted_until`], then [`log_entries::forget_landed_draft`], as client.rs
//! runs them) or let go unkept ([`log_entries::forget_held_draft`]). A sync lands sets
//! ([`log_entries::land_waiting`]), a refusal drops one ([`log_entries::forget_waiting`]), and a
//! vault pass signs ([`log_entries::sign_waiting_writes_by`]).
//!
//! What the island takes as the environment is played on m22: another Mac's write of a name is
//! m7's `Put` or `Delete`, signed on m22's log under the epoch m22 seals under; `due` is m7's
//! removal of m42 (its `Remove`, then its rotation); `signPass`'s argument is whether m22 opens
//! the newest `[everyone]` epoch, which m7 takes from it by minting one it leaves m22 out of and
//! gives back by its `Wrap` to m22 ([`WEvent::Opens`]); a pending set that does not open is a file
//! of no set's shape in `everyone.pending`, and a held draft that does not read one of no hold's
//! shape in `records.drafts`; a server's taking or refusing a version is the island's ghost
//! alone, read by the guards of `land` and `dropped`.
//!
//! After every step m22's pending sets, waiting writes, `[everyone]` values (with who holds each:
//! another Mac, a pass of this Mac, or a direct draft of this Mac by its stamp), journal of
//! overtaken writes (who overtook each), held drafts, unread files and due rotation are read
//! ([`WLab::observe`]) and compared with the step's World ([`observe`]), and the island's laws
//! (`SignedOnlyStored`, `NoLostWrite`, `OvertakenByAnother`, and `CasStep` between steps) are
//! asserted on the World whose observables are m22's ([`project`]). A negative control runs a
//! run on both sides until they part ([`WLab::replay`], which names the step and the fields) and
//! on m22 alone ([`WLab::replay_real`], whose projection of m22 reads what became of each write
//! from m22's files and journal), where the island's law it breaks fails.
//!
//! A vault pass holds its claim from before its draft until its settle, so the steps are a
//! publish (which holds its turn and claim from its draft to its pending set), each event that
//! takes no claim, and a pass: `signPass`, at most one landing or another Mac's write, then
//! `settlePass`, the event between run by the pass's own hook between its commit and its settle
//! ([`log_entries::PASS_COMMITTED`]). The search is depth first over the steps, keyed on the
//! World, the lab's place in the removal and the epoch, and m22's state up to record ids, times,
//! nonces and the log's history ([`WLab::canon`]); each state is explored once with as many steps
//! left, and each step's real calls run on m22's home as it was at that state ([`Home`]).

use super::*;
use crate::membership_log::RecordId;
use aterm_digest::Sha256;
use aterm_mesh_core::no_lost_edit as nle;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// Versions 0 to 11 (0 the one m22 opened, on a server from the start).
const V: usize = 12;
/// Write ids 0 to 11 (0 is no write).
const W: usize = 12;
/// Names 0 to 2 (0 is no name: the universe writes `n1` and `n2`).
const N: usize = 3;
/// Direct draft ids 0 to 3 (0 is no draft).
const D: usize = 4;
/// The value another Mac writes.
const OTHER: u8 = 99;

type Ver = usize;
type Id = usize;
type Name = usize;
type Val = u8;
type Direct = usize;

/// The island's `Kernels`, the six the waiting writes ask.
#[derive(Clone, Copy)]
struct Kernels {
    keeps: fn(bool, bool) -> bool,
    pass_signs: fn(bool, bool, bool) -> bool,
    signs: fn(bool, bool, bool) -> bool,
    overtaken: fn(bool, bool, bool) -> bool,
    drafted_keeps: fn(bool) -> bool,
    draft_refuses: fn(bool, bool, bool) -> bool,
}

/// The compiled kernels the code asks (the binding island's `compiled`).
const COMPILED: Kernels = Kernels {
    keeps: nle::pending_keeps,
    pass_signs: nle::pass_signs,
    signs: nle::write_signs,
    overtaken: nle::write_overtaken,
    drafted_keeps: nle::drafted_keeps,
    draft_refuses: nle::draft_refuses,
};

/// The island's `WWorld`, its functions over the universe's writes, names, versions and direct
/// drafts.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct WWorld {
    next: Id,
    nm: [Name; W],
    val: [Val; W],
    /// Each write's bases, the values it may be signed over, ascending.
    bs: [Vec<Val>; W],
    ver: [Ver; W],
    used: [bool; V],
    pend: [bool; W],
    wait_of: [Id; N],
    log: [Val; N],
    holder: [Id; N],
    stored: [bool; V],
    refused: [bool; V],
    unread: bool,
    due: bool,
    pass_read: [Id; N],
    fate: [u8; W],
    over_by: [Id; W],
    hold_unread: bool,
    dnext: Direct,
    dnm: [Name; D],
    dval: [Val; D],
    dstamp: [Id; D],
    dphase: [u8; D],
    hdirect: [Id; N],
    over_d: [Id; W],
}

/// One write of a publish: its name, its value (0: a take-back to absent) and its bases.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Write {
    name: Name,
    val: Val,
    bases: Vec<Val>,
}

/// The island's `WEvent`, and the lab's epoch.
#[derive(Clone, Debug, PartialEq, Eq)]
enum WEvent {
    /// `raced`: the environment runs a vault pass between the draft and its pending set; the
    /// island's publish holds its claim, so the step reads no difference.
    Publish {
        v: Ver,
        writes: Vec<Write>,
        opened: Vec<Ver>,
        raced: bool,
    },
    Stored(Ver),
    Dropped(Ver),
    Land(Vec<Ver>),
    SignPass(bool),
    SettlePass,
    Other(Name, Val),
    Due(bool),
    Unreadable(bool),
    DirectDraft(Name, Val),
    DirectHold(Direct),
    Keep(Direct),
    Unhold(Direct),
    HoldUnreadable(bool),
    /// The lab's: m22 opens the newest `[everyone]` epoch (m7's `Wrap` to it arrived) or not (m7
    /// minted one it left m22 out of). The island takes it as `signPass`'s argument; the step
    /// leaves the World as it is.
    Opens(bool),
}

impl WWorld {
    /// The island's `ww0`.
    fn initial() -> Self {
        let mut stored = [false; V];
        stored[0] = true;
        Self {
            next: 1,
            nm: [0; W],
            val: [0; W],
            bs: Default::default(),
            ver: [0; W],
            used: [false; V],
            pend: [false; W],
            wait_of: [0; N],
            log: [0; N],
            holder: [0; N],
            stored,
            refused: [false; V],
            unread: false,
            due: false,
            pass_read: [0; N],
            fate: [0; W],
            over_by: [0; W],
            hold_unread: false,
            dnext: 1,
            dnm: [0; D],
            dval: [0; D],
            dstamp: [0; D],
            dphase: [0; D],
            hdirect: [0; N],
            over_d: [0; W],
        }
    }

    /// The island's `laterLands`.
    fn later_lands(&self, s: &[Ver], i: Id) -> bool {
        (0..self.next)
            .any(|j| i < j && self.pend[j] && s.contains(&self.ver[j]) && self.nm[j] == self.nm[i])
    }

    /// The island's `landingOf` (`lastN`: the largest such write, or 0).
    fn landing_of(&self, s: &[Ver], n: Name) -> Id {
        (0..self.next)
            .rev()
            .find(|&i| self.pend[i] && s.contains(&self.ver[i]) && self.nm[i] == n)
            .unwrap_or(0)
    }

    /// The island's `landW`.
    fn land_w(&self, k: Kernels, s: &[Ver]) -> Self {
        let mut n = self.clone();
        for i in 0..W {
            let keeps = (k.keeps)(s.contains(&self.ver[i]), self.later_lands(s, i));
            n.pend[i] = self.pend[i] && keeps;
            let landing = self.landing_of(s, self.nm[i]);
            if (self.pend[i] && !keeps && landing != i)
                || (i != 0 && self.wait_of[self.nm[i]] == i && landing != 0)
            {
                n.fate[i] = 3;
            }
        }
        for (name, wait) in n.wait_of.iter_mut().enumerate() {
            let landing = self.landing_of(s, name);
            if landing != 0 {
                *wait = landing;
            }
        }
        n
    }

    /// The island's `deferredW`: a pending write writes `n`, a hold does not read, or a direct
    /// draft in flight writes `n` (`heldW`).
    fn deferred(&self, n: Name) -> bool {
        (0..self.next).any(|j| self.pend[j] && self.nm[j] == n)
            || self.hold_unread
            || (0..self.dnext)
                .any(|d| (self.dphase[d] == 1 || self.dphase[d] == 2) && self.dnm[d] == n)
    }

    /// The island's `passVerdict`: 0 leaves, 1 done, 2 overtaken, 3 signs.
    fn pass_verdict(&self, k: Kernels, n: Name) -> u8 {
        let i = self.wait_of[n];
        if i == 0 {
            return 0;
        }
        verdict_of(
            k,
            self.deferred(n),
            self.log[n] == self.val[i],
            self.bs[i].contains(&self.log[n]),
            self.val[i] == 0,
        )
    }

    /// The island's `signBody`.
    fn sign_body(&self, k: Kernels) -> Self {
        let mut x = self.clone();
        for n in 0..N {
            let verdict = self.pass_verdict(k, n);
            if verdict == 3 {
                x.log[n] = self.val[self.wait_of[n]];
                x.holder[n] = self.wait_of[n];
                x.hdirect[n] = 0;
            }
            x.pass_read[n] = if verdict == 0 { 0 } else { self.wait_of[n] };
        }
        for i in 1..W {
            let n = self.nm[i];
            if self.wait_of[n] == i {
                let verdict = self.pass_verdict(k, n);
                if verdict != 0 {
                    x.fate[i] = if verdict == 2 { 2 } else { 1 };
                }
                if verdict == 2 {
                    x.over_by[i] = self.holder[n];
                    x.over_d[i] = self.hdirect[n];
                }
            }
        }
        x
    }

    /// The island's `settleW`.
    fn settle_w(&self) -> Self {
        let mut x = self.clone();
        for n in 0..N {
            if self.pass_read[n] != 0 && self.wait_of[n] == self.pass_read[n] {
                x.wait_of[n] = 0;
            }
        }
        x.pass_read = [0; N];
        x
    }

    /// The island's `pushW`.
    fn push_w(&self, v: Ver, writes: &[Write]) -> Self {
        let mut x = self.clone();
        for (a, write) in writes.iter().enumerate() {
            let i = self.next + a;
            x.nm[i] = write.name;
            x.val[i] = write.val;
            x.bs[i] = write.bases.clone();
            x.ver[i] = v;
            x.pend[i] = true;
        }
        x.next = self.next + writes.len();
        x.used[v] = true;
        x
    }

    /// The island's `pubBase`, `hu` what it says of the held drafts.
    fn pub_base(&self, k: Kernels, hu: bool, v: Ver, writes: &[Write], opened: &[Ver]) -> bool {
        !self.used[v]
            && !(k.draft_refuses)(!writes.is_empty(), self.unread, hu)
            && !writes.is_empty()
            && (0..self.next).all(|i| {
                !(self.pend[i] && opened.contains(&self.ver[i])) || self.stored[self.ver[i]]
            })
            && writes
                .iter()
                .enumerate()
                .all(|(a, x)| writes[..a].iter().all(|y| y.name != x.name))
            && writes.iter().all(|x| x.bases.contains(&self.log[x.name]))
            && writes.iter().all(|x| x.val != 0 || self.log[x.name] == 0)
    }

    /// The island's `turnFree`: no direct draft is between its draft and its hold.
    fn turn_free(&self) -> bool {
        (0..self.dnext).all(|d| self.dphase[d] != 1)
    }

    /// The island's `heldIn`: each write's bases hold what each held draft of its name leaves.
    fn held_in(&self, writes: &[Write]) -> bool {
        writes.iter().all(|x| {
            (0..self.dnext).all(|d| {
                !(self.dphase[d] == 2 && self.dnm[d] == x.name) || x.bases.contains(&self.dval[d])
            })
        })
    }

    /// The island's `publishOk`.
    fn publish_ok(&self, k: Kernels, v: Ver, writes: &[Write], opened: &[Ver]) -> bool {
        self.pub_base(k, self.hold_unread, v, writes, opened)
            && self.turn_free()
            && self.held_in(writes)
    }

    /// The island's `land` guard: no set fails to open, and each landing set's version is stored.
    fn lands(&self, s: &[Ver]) -> bool {
        !self.unread
            && (0..self.next)
                .all(|i| !(self.pend[i] && s.contains(&self.ver[i])) || self.stored[self.ver[i]])
    }

    /// The island's `keepBody`.
    fn keep_body(&self, d: Direct) -> Self {
        let mut x = self.clone();
        let n = self.dnm[d];
        x.log[n] = self.dval[d];
        x.holder[n] = 0;
        x.hdirect[n] = self.dstamp[d];
        x.dphase[d] = 3;
        x
    }

    /// The island's `directDraft` body: direct draft `dnext` writes `n` to `x`, stamped `next`.
    fn push_direct(&self, n: Name, x: Val) -> Self {
        let mut y = self.clone();
        let d = self.dnext;
        y.dnext = d + 1;
        y.dnm[d] = n;
        y.dval[d] = x;
        y.dstamp[d] = self.next;
        y.dphase[d] = 1;
        y
    }

    /// The values each held draft of `n` leaves there (`heldIn`'s).
    fn held_values(&self, n: Name) -> Vec<Val> {
        (1..self.dnext)
            .filter(|&d| self.dphase[d] == 2 && self.dnm[d] == n)
            .map(|d| self.dval[d])
            .collect()
    }

    /// Whether a direct draft of `n` is drafted and not kept or gone.
    fn drafting(&self, n: Name) -> bool {
        (1..self.dnext).any(|d| (self.dphase[d] == 1 || self.dphase[d] == 2) && self.dnm[d] == n)
    }
}

/// The island's `verdictOf`.
fn verdict_of(k: Kernels, deferred: bool, done: bool, stands: bool, take_back: bool) -> u8 {
    if deferred {
        0
    } else if (k.signs)(done, stands, take_back) {
        3
    } else if (k.overtaken)(done, stands, take_back) {
        2
    } else {
        1
    }
}

/// The island's `wstep`.
fn wstep(k: Kernels, w: &WWorld, event: &WEvent) -> WWorld {
    match event {
        WEvent::Publish {
            v, writes, opened, ..
        } => {
            if w.publish_ok(k, *v, writes, opened) {
                w.land_w(k, opened).push_w(*v, writes)
            } else {
                w.clone()
            }
        }
        WEvent::Stored(v) => {
            let mut x = w.clone();
            if !w.refused[*v] {
                x.stored[*v] = true;
            }
            x
        }
        WEvent::Dropped(v) => {
            let mut x = w.clone();
            if !w.stored[*v] {
                x.refused[*v] = true;
                for i in 0..W {
                    if w.pend[i] && w.ver[i] == *v {
                        x.pend[i] = false;
                        x.fate[i] = 4;
                    }
                }
            }
            x
        }
        WEvent::Land(s) => {
            if w.lands(s) {
                w.land_w(k, s)
            } else {
                w.clone()
            }
        }
        WEvent::SignPass(opens) => {
            if (k.pass_signs)(w.due, *opens, !w.unread) {
                w.sign_body(k)
            } else {
                w.clone()
            }
        }
        WEvent::SettlePass => w.settle_w(),
        WEvent::Other(n, x) => {
            let mut y = w.clone();
            y.log[*n] = *x;
            y.holder[*n] = 0;
            y.hdirect[*n] = 0;
            y
        }
        WEvent::Due(b) => WWorld {
            due: *b,
            ..w.clone()
        },
        WEvent::Unreadable(b) => WWorld {
            unread: *b,
            ..w.clone()
        },
        WEvent::DirectDraft(n, x) => {
            if w.turn_free() {
                w.push_direct(*n, *x)
            } else {
                w.clone()
            }
        }
        WEvent::DirectHold(d) => {
            let mut y = w.clone();
            if w.dphase[*d] == 1 {
                y.dphase[*d] = 2;
            }
            y
        }
        WEvent::Keep(d) => {
            if (k.drafted_keeps)(w.dphase[*d] == 2) {
                w.keep_body(*d)
            } else {
                w.clone()
            }
        }
        WEvent::Unhold(d) => {
            let mut y = w.clone();
            if w.dphase[*d] == 1 || w.dphase[*d] == 2 {
                y.dphase[*d] = 3;
            }
            y
        }
        WEvent::HoldUnreadable(b) => WWorld {
            hold_unread: *b,
            ..w.clone()
        },
        WEvent::Opens(_) => w.clone(),
    }
}

/// The island's `Accounted`.
fn accounted(w: &WWorld, i: Id) -> bool {
    w.pend[i]
        || w.wait_of[w.nm[i]] == i
        || w.fate[i] == 1
        || w.fate[i] == 2
        || (w.fate[i] == 3 && (i + 1..w.next).any(|j| w.nm[j] == w.nm[i]))
}

/// The island's `SignedOnlyStored`, `NoLostWrite`, `OvertakenByAnother` and `LaterKept` of `w`;
/// the error starts with the law's name.
fn laws(w: &WWorld) -> Result<(), String> {
    for n in 0..N {
        if w.wait_of[n] != 0 && !w.stored[w.ver[w.wait_of[n]]] {
            return Err(format!(
                "SignedOnlyStored: n{n}'s write waits on a version no server took"
            ));
        }
    }
    for i in 1..w.next {
        if w.fate[i] == 1 && !w.stored[w.ver[i]] {
            return Err(format!(
                "SignedOnlyStored: write {i} was signed, its version on no server"
            ));
        }
        if w.stored[w.ver[i]] && !accounted(w, i) {
            return Err(format!(
                "NoLostWrite: write {i} of a stored version is lost"
            ));
        }
        if w.fate[i] == 2
            && !(w.over_by[i] == 0 && (w.val[i] == 0 || w.over_d[i] == 0 || i < w.over_d[i]))
        {
            return Err(format!(
                "OvertakenByAnother: write {i} was overtaken by this Mac's own earlier value \
                 (over_by {}, over_d {})",
                w.over_by[i], w.over_d[i]
            ));
        }
        let n = w.nm[i];
        if w.fate[i] == 1 && w.hdirect[n] != 0 && w.hdirect[n] <= i && w.log[n] != w.val[i] {
            return Err(format!(
                "LaterKept: write {i}, signed, lost n{n} to direct draft {}'s keep, drafted \
                 before it",
                w.hdirect[n]
            ));
        }
    }
    Ok(())
}

/// The island's `CasStep` from `w` to `x`.
fn cas_step(w: &WWorld, x: &WWorld) -> Result<(), String> {
    for n in 0..N {
        let i = w.wait_of[n];
        if !(x.log[n] == w.log[n]
            || x.holder[n] == 0
            || (i != 0 && w.bs[i].contains(&w.log[n]) && w.val[i] != 0 && x.log[n] == w.val[i]))
        {
            return Err(format!(
                "CasStep: n{n} changed from {} to {} by no waiting write over one of its bases",
                w.log[n], x.log[n]
            ));
        }
    }
    Ok(())
}

/// A name of the universe, as m22's `[everyone]` holds it.
fn name(n: Name) -> String {
    format!("n{n}")
}

/// A value of the universe, as an entry holds it.
fn bytes(x: Val) -> Vec<u8> {
    format!("w{x}").into_bytes()
}

/// The value an entry holds, as [`bytes`] wrote it.
fn val_of(value: &[u8]) -> Result<Val, String> {
    std::str::from_utf8(value)
        .ok()
        .and_then(|text| text.strip_prefix('w'))
        .and_then(|x| x.parse().ok())
        .ok_or_else(|| format!("a value of no write of the universe: {value:?}"))
}

/// The `[everyone]` entry `n` = `value`.
fn entry(n: &str, value: &[u8]) -> Entry {
    Entry {
        name: n.to_owned(),
        value: SecretBytes::from(value.to_vec()),
        audience: Audience::Everyone,
        if_absent: false,
        anchor: None,
    }
}

/// The digest that names version `v`'s envelope.
fn vd(v: Ver) -> [u8; 32] {
    [0x50 + u8::try_from(v).unwrap(); 32]
}

/// The digest that names direct draft `d`'s envelope.
fn dd(d: Direct) -> [u8; 32] {
    [0x70 + u8::try_from(d).unwrap(); 32]
}

/// Direct draft `d`'s sequence.
fn direct_seq(d: Direct) -> u64 {
    1_000 + u64::try_from(d).unwrap()
}

/// The one server direct draft `d`'s publish reaches: one of its own, so no other draft's landing
/// settles its hold.
fn direct_url(d: Direct) -> String {
    format!("https://d{d}.example/v")
}

/// The file name of direct draft `d`'s hold (`records.drafts/<seq>-<digest>`).
fn hold_name(d: Direct) -> String {
    format!(
        "{:020}-{}",
        direct_seq(d),
        aterm_codec::hex::encode_lower(&dd(d))
    )
}

/// The digest of the file in `everyone.pending` that is no set.
const UNREAD: [u8; 32] = [0xEE; 32];

/// The file name of the hold in `records.drafts` that does not read.
fn hold_unread_name() -> String {
    format!(
        "{:020}-{}",
        999,
        aterm_codec::hex::encode_lower(&[0xEF; 32])
    )
}

/// Who holds a value of the log, or overtook a write: another Mac (or nobody), a pass of this Mac,
/// or a direct draft of this Mac, by its stamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum By {
    Other,
    Pass,
    Direct(Id),
}

/// What the comparison reads of a World: the pending sets by version, oldest first, what waits,
/// the log's values with who holds each, the journal's records per name with who overtook each
/// (in the journal's order), the direct drafts whose hold is there, whether a set or a hold does
/// not read, and whether a rotation is due.
#[derive(Clone, Debug)]
struct Obs {
    pending: Vec<(Ver, log_entries::Shapes)>,
    waiting: log_entries::Shapes,
    log: BTreeMap<String, (Vec<u8>, By)>,
    overtaken: BTreeMap<String, Vec<By>>,
    holds: BTreeSet<Direct>,
    /// The names whose `[everyone]` value is a conflict of concurrent writes, which the island's
    /// log, one value per name, never holds.
    conflicts: BTreeSet<String>,
    unread: bool,
    hold_unread: bool,
    due: bool,
}

impl Obs {
    /// The fields `self` and `other` differ in.
    fn differs(&self, other: &Self) -> Vec<&'static str> {
        let sorted = |o: &Obs| -> BTreeMap<String, Vec<By>> {
            o.overtaken
                .iter()
                .map(|(n, by)| {
                    let mut by = by.clone();
                    by.sort();
                    (n.clone(), by)
                })
                .collect()
        };
        let mut fields = Vec::new();
        if self.pending != other.pending {
            fields.push("pending");
        }
        if self.waiting != other.waiting {
            fields.push("waiting");
        }
        if self.log != other.log {
            fields.push("log");
        }
        if sorted(self) != sorted(other) {
            fields.push("overtaken");
        }
        if self.holds != other.holds {
            fields.push("holds");
        }
        if self.conflicts != other.conflicts {
            fields.push("conflicts");
        }
        if self.unread != other.unread {
            fields.push("unread");
        }
        if self.hold_unread != other.hold_unread {
            fields.push("hold_unread");
        }
        if self.due != other.due {
            fields.push("due");
        }
        fields
    }
}

/// Write `i`'s value and bases, as the code keeps them.
fn shape(w: &WWorld, i: Id) -> (Option<Vec<u8>>, BTreeSet<Option<[u8; 32]>>) {
    shape_of(w.val[i], &w.bs[i])
}

/// A write of value `val` over `bases`, as the code keeps it.
fn shape_of(val: Val, bases: &[Val]) -> (Option<Vec<u8>>, BTreeSet<Option<[u8; 32]>>) {
    let value = (val != 0).then(|| bytes(val));
    let bases = bases
        .iter()
        .map(|&x| (x != 0).then(|| log_entries::base_of(&bytes(x))))
        .collect();
    (value, bases)
}

/// Who holds `n` in `w`.
fn holder_of(w: &WWorld, n: Name) -> By {
    if w.holder[n] != 0 {
        By::Pass
    } else if w.hdirect[n] != 0 {
        By::Direct(w.hdirect[n])
    } else {
        By::Other
    }
}

/// What `w` says the code holds.
fn observe(w: &WWorld) -> Obs {
    let pending = (0..V)
        .filter_map(|v| {
            let set: log_entries::Shapes = (1..w.next)
                .filter(|&i| w.pend[i] && w.ver[i] == v)
                .map(|i| (name(w.nm[i]), shape(w, i)))
                .collect();
            (!set.is_empty()).then_some((v, set))
        })
        .collect();
    let waiting = (1..N)
        .filter(|&n| w.wait_of[n] != 0)
        .map(|n| (name(n), shape(w, w.wait_of[n])))
        .collect();
    let mut overtaken: BTreeMap<String, Vec<By>> = BTreeMap::new();
    for i in 1..w.next {
        if w.fate[i] == 2 && w.pass_read[w.nm[i]] != i {
            let by = if w.over_by[i] != 0 {
                By::Pass
            } else if w.over_d[i] != 0 {
                By::Direct(w.over_d[i])
            } else {
                By::Other
            };
            overtaken.entry(name(w.nm[i])).or_default().push(by);
        }
    }
    Obs {
        pending,
        waiting,
        log: (1..N)
            .filter(|&n| w.log[n] != 0)
            .map(|n| (name(n), (bytes(w.log[n]), holder_of(w, n))))
            .collect(),
        overtaken,
        holds: (1..w.dnext).filter(|&d| w.dphase[d] == 2).collect(),
        conflicts: BTreeSet::new(),
        unread: w.unread,
        hold_unread: w.hold_unread,
        due: w.due,
    }
}

/// The World `ghost` with each observable `obs` reads of m22 in its place: its pending and waiting
/// writes (by their shapes, among `ghost`'s writes), the log's values and who holds each, which
/// direct drafts are held, and its unread files and due rotation. The ghosts (fates, the
/// overtakers of each write, the servers' answers) stay `ghost`'s.
fn project(ghost: &WWorld, obs: &Obs) -> Result<WWorld, String> {
    let mut r = ghost.clone();
    for i in 1..ghost.next {
        r.pend[i] = obs.pending.iter().any(|(v, set)| {
            *v == ghost.ver[i] && set.get(&name(ghost.nm[i])) == Some(&shape(ghost, i))
        });
    }
    for n in 1..N {
        r.wait_of[n] = match obs.waiting.get(&name(n)) {
            None => 0,
            Some(waits) => {
                let fits = |i: Id| ghost.nm[i] == n && shape(ghost, i) == *waits;
                if ghost.wait_of[n] != 0 && fits(ghost.wait_of[n]) {
                    ghost.wait_of[n]
                } else {
                    (1..ghost.next)
                        .rev()
                        .find(|&i| fits(i))
                        .ok_or_else(|| format!("n{n} waits with no write of the universe"))?
                }
            }
        };
        let (log, by) = match obs.log.get(&name(n)) {
            None => (0, By::Other),
            Some((value, by)) => (val_of(value)?, *by),
        };
        r.log[n] = log;
        r.holder[n] = 0;
        r.hdirect[n] = 0;
        match by {
            By::Other => {}
            By::Pass => {
                r.holder[n] = if ghost.holder[n] != 0 && ghost.val[ghost.holder[n]] == log {
                    ghost.holder[n]
                } else {
                    (1..ghost.next)
                        .rev()
                        .find(|&i| ghost.nm[i] == n && ghost.val[i] == log)
                        .ok_or_else(|| {
                            format!("a pass of this Mac signed n{n} to no write's value")
                        })?
                };
            }
            By::Direct(stamp) => r.hdirect[n] = stamp,
        }
    }
    for d in 1..ghost.dnext {
        if obs.holds.contains(&d) {
            r.dphase[d] = 2;
        } else if ghost.dphase[d] == 2 {
            r.dphase[d] = 3;
        }
    }
    r.unread = obs.unread;
    r.hold_unread = obs.hold_unread;
    r.due = obs.due;
    Ok(r)
}

/// A home's content: each directory and regular file by its path under the home, with its mode,
/// and each file's bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Home {
    dirs: BTreeMap<PathBuf, u32>,
    files: BTreeMap<PathBuf, (u32, Vec<u8>)>,
}

impl Home {
    fn of(home: &Path) -> Self {
        let mut content = Self::default();
        content.read(home, Path::new(""));
        content
    }

    fn read(&mut self, dir: &Path, at: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = at.join(entry.file_name());
            let kind = entry.file_type().unwrap();
            let mode = entry.metadata().unwrap().permissions().mode() & 0o7777;
            if kind.is_dir() {
                self.dirs.insert(path.clone(), mode);
                self.read(&entry.path(), &path);
            } else if kind.is_file() {
                let bytes = std::fs::read(entry.path()).unwrap();
                self.files.insert(path, (mode, bytes));
            }
        }
    }

    /// Make `home`, which holds `self`, hold `to`.
    fn turn_into(&self, to: &Self, home: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        for path in self
            .files
            .keys()
            .filter(|path| !to.files.contains_key(*path))
        {
            std::fs::remove_file(home.join(path)).unwrap();
        }
        for path in self
            .dirs
            .keys()
            .rev()
            .filter(|path| !to.dirs.contains_key(*path))
        {
            std::fs::remove_dir(home.join(path)).unwrap();
        }
        for (path, &mode) in &to.dirs {
            if self.dirs.get(path) != Some(&mode) {
                fsx::ensure_private_dir(&home.join(path)).unwrap();
                std::fs::set_permissions(home.join(path), std::fs::Permissions::from_mode(mode))
                    .unwrap();
            }
        }
        for (path, file) in &to.files {
            if self.files.get(path) != Some(file) {
                std::fs::write(home.join(path), &file.1).unwrap();
                std::fs::set_permissions(home.join(path), std::fs::Permissions::from_mode(file.0))
                    .unwrap();
            }
        }
    }
}

/// A member's key: its bootstrap, actor and incarnation.
type MemberKey = (crate::commons::bootstrap::Bootstrap, MemberId, [u8; 16]);

/// `who`'s write of `[everyone]`'s `n` drafted on `paths`' log, under the epoch `identity` seals
/// under there: a `Put` of `value`, or a `Delete` where there is none.
fn draft_as(
    paths: &Paths,
    identity: &Identity,
    who: &MemberKey,
    n: &str,
    value: Option<&[u8]>,
    ms: u64,
) -> Result<Record, String> {
    let log = log_of(paths);
    let opened = log
        .open_audience_entries(identity)
        .map_err(|e| e.to_string())?
        .ok_or("no epoch opens")?;
    let [epoch] = opened.maximal_epochs[&EpochAudience::Everyone][..] else {
        return Err("no one newest [everyone] epoch".to_owned());
    };
    let key = **opened
        .unwrapped_keys
        .get(&epoch)
        .ok_or("the newest epoch does not open")?;
    let (bootstrap, actor, incarnation) = who;
    bootstrap
        .device()
        .with_seed(|seed| match value {
            Some(value) => log
                .draft_put(actor, ms, epoch, &key, &entry(n, value))?
                .sign(seed, *incarnation),
            None => log
                .draft_delete(actor, ms, epoch, &key, n)?
                .sign(seed, *incarnation),
        })
        .map_err(|e| e.to_string())
}

/// Where the lab's environment stands on m22: m7's removal of m42 (`chain` 0: not taken, 1: its
/// `Remove` alone, 2: its rotation too), and its epoch that leaves m22 out (`opens` 0: not
/// minted, 1: m22 does not open it, 2: m7's `Wrap` to m22 arrived).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
struct Env {
    chain: u8,
    opens: u8,
}

/// Where the environment stands after `event` from `env`.
fn next_env(env: Env, event: &WEvent) -> Env {
    match (env, event) {
        (Env { chain: 0, .. }, WEvent::Due(true)) => Env { chain: 1, ..env },
        (Env { chain: 1, .. }, WEvent::Due(false)) => Env { chain: 2, ..env },
        (Env { chain: 2, opens: 0 }, WEvent::Opens(false)) => Env { chain: 2, opens: 1 },
        (Env { opens: 1, .. }, WEvent::Opens(true)) => Env { opens: 2, ..env },
        _ => env,
    }
}

/// A direct draft of m22: its record and stamp (the first write id not yet born at its draft).
#[derive(Clone)]
struct DirectRec {
    record: Record,
    stamp: Id,
}

/// Where the two sides part: the step (its index in a run, the events of the path in a search),
/// the fields of [`Obs`] or the law or check that differ, and why.
#[derive(Debug)]
struct Fail {
    fields: Vec<String>,
    why: String,
}

impl Fail {
    fn of(field: &str, why: impl Into<String>) -> Self {
        Self {
            fields: vec![field.to_owned()],
            why: why.into(),
        }
    }

    /// A law's failure, named by the law ([`laws`] starts each with its name).
    fn law(why: String) -> Self {
        let law = why.split(':').next().unwrap_or("law").to_owned();
        Self {
            fields: vec![format!("law {law}")],
            why,
        }
    }
}

impl std::fmt::Display for Fail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.fields, self.why)
    }
}

/// A run's first divergence: the index of the step and its [`Fail`].
#[derive(Debug)]
struct Divergence {
    at: usize,
    fail: Fail,
}

/// The real side: m22 of the removal lab, its home as it is at the start ([`Home`]), its identity
/// and member key, m7's removal of m42 (its `Remove` first), which m22 takes for `due`, and m7's
/// epoch that leaves m22 out, then its `Wrap` to m22.
struct WLab {
    six: convergence::Six,
    identity: Identity,
    /// m7's identity, which reads m22's log where m22 does not open its newest epoch.
    m7_identity: Identity,
    /// m7's key, which signs another Mac's writes and its epoch.
    m7: MemberKey,
    /// m22's own key, which signs its direct drafts.
    m22_key: MemberKey,
    me: MemberId,
    start: Home,
    chain: Vec<Record>,
    /// m7's `[everyone]` epoch after the rotation, with its `Wrap`s to every Mac but m22 and m42.
    left_out: Vec<Record>,
    /// That epoch's `Wrap` to m22.
    wrap_m22: Vec<Record>,
    clock: Cell<u64>,
    /// How many steps ran on the real functions.
    calls: Cell<usize>,
    /// Whether an event ran on m22 since the search last took or put back its home.
    touched: Cell<bool>,
    /// Whether the last event was made on m22: a publish that kept its set, a direct draft that
    /// drafted.
    made: Cell<bool>,
    /// Whether a divergence of the functions' own refusals is let through ([`WLab::replay_real`]).
    lenient: Cell<bool>,
    directs: RefCell<[Option<DirectRec>; D]>,
    /// The turn and claim a direct draft holds from its draft until its hold.
    turn: RefCell<Option<Box<dyn std::any::Any>>>,
}

impl WLab {
    fn new() -> Self {
        let six = convergence::six_with(true);
        let (m22, m7) = (&six.all[0], &six.all[1]);
        let base = convergence::ids(m7);
        test_remove_unposted(m7, &join_code(&six.recipients[4])).expect("m7 removes m42");
        let chain = convergence::signed_since(m7, &base);
        let m7_key = member_key(m7);
        let (left_out, wrap_m22) = Self::epoch_leaving_out(&six, &m7_key);
        // The turn's and the claim's lock files are there from the start, so a search that puts
        // m22's home back never removes one a held lock is on.
        drop(log_entries::draft_turn(m22, None).expect("a turn"));
        drop(log_entries::claim_draft(m22, None).expect("a claim"));
        let lab = Self {
            identity: read_identity(m22).unwrap().unwrap(),
            m7_identity: read_identity(m7).unwrap().unwrap(),
            m7: m7_key,
            m22_key: member_key(m22),
            me: member_key(m22).1,
            start: Home::of(&m22.dir),
            six,
            chain,
            left_out,
            wrap_m22,
            clock: Cell::new(aterm_time::unix_millis().unwrap_or(0)),
            calls: Cell::new(0),
            touched: Cell::new(false),
            made: Cell::new(false),
            lenient: Cell::new(false),
            directs: RefCell::new(Default::default()),
            turn: RefCell::new(None),
        };
        assert!(
            !lab.observe_due(),
            "CONTROL: no rotation is due on m22 at the start"
        );
        lab
    }

    /// m7's `[everyone]` epoch after its rotation, wrapped to every Mac but m22 and m42, and its
    /// `Wrap` to m22.
    fn epoch_leaving_out(six: &convergence::Six, m7: &MemberKey) -> (Vec<Record>, Vec<Record>) {
        let paths = &six.all[1];
        let mut log = log_of(paths);
        let identity = read_identity(paths).unwrap().unwrap();
        let opened = log.open_audience_entries(&identity).unwrap().unwrap();
        let generation = opened.max_generation[&EpochAudience::Everyone] + 1;
        let (bootstrap, actor, incarnation) = m7;
        let ms = aterm_time::unix_millis().unwrap_or(0);
        bootstrap.device().with_seed(|seed| {
            let (draft, key) = log
                .draft_epoch(actor, ms, EpochAudience::Everyone, generation)
                .unwrap();
            let epoch = draft.sign(seed, *incarnation).unwrap();
            log.insert(epoch.clone()).unwrap();
            let wrap = |recipient: Recipient| {
                log.draft_wrap(actor, ms, *epoch.id(), &key, recipient)
                    .unwrap()
                    .sign(seed, *incarnation)
                    .unwrap()
            };
            let mut left_out = vec![epoch.clone()];
            for (at, &recipient) in six.recipients.iter().enumerate() {
                if at != 0 && at != 4 {
                    left_out.push(wrap(recipient));
                }
            }
            (left_out, vec![wrap(six.recipients[0])])
        })
    }

    fn m22(&self) -> &Paths {
        &self.six.all[0]
    }

    /// A time after every one the lab signed at.
    fn tick(&self) -> u64 {
        self.clock.set(self.clock.get() + 1_000);
        self.clock.get()
    }

    fn unread_file(&self) -> PathBuf {
        self.m22().dir.join("everyone.pending").join(format!(
            "{:020}-{:020}-{}",
            0,
            0,
            aterm_codec::hex::encode_lower(&UNREAD)
        ))
    }

    fn holds_dir(&self) -> PathBuf {
        self.m22().dir.join("records.drafts")
    }

    fn observe_due(&self) -> bool {
        log_of(self.m22())
            .rekey_dues()
            .iter()
            .any(|due| due.audience == EpochAudience::Everyone && due.rotation)
    }

    /// Who signed `record`, as [`By`] reads it.
    fn by(&self, log: &crate::membership_log::Log, record: &RecordId) -> By {
        match log.get(record) {
            Some(op) if *op.actor() == self.me => self
                .directs
                .borrow()
                .iter()
                .flatten()
                .find(|direct| direct.record.id() == record)
                .map_or(By::Pass, |direct| By::Direct(direct.stamp)),
            _ => By::Other,
        }
    }

    /// What m22 holds, as [`Obs`].
    fn observe(&self) -> Result<Obs, String> {
        let m22 = self.m22();
        let identity = &self.identity;
        let (waiting, sets) = log_entries::waiting_shapes(m22, identity);
        let mut pending = Vec::new();
        for (digest, set) in sets {
            let v = (0..V)
                .find(|&v| vd(v) == digest)
                .ok_or("a pending set of no version of the universe")?;
            pending.push((v, set));
        }
        let log = log_of(m22);
        let open = |identity: &Identity| -> Result<_, String> {
            log.open_audience_entries(identity)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "no epoch opens".to_owned())
        };
        let mut opened = open(identity)?;
        let newest = opened
            .maximal_epochs
            .get(&EpochAudience::Everyone)
            .cloned()
            .unwrap_or_default();
        if !newest
            .iter()
            .all(|epoch| opened.unwrapped_keys.contains_key(epoch))
        {
            // m22 is left out of the newest epoch and reads no `[everyone]` value: the log holds
            // them still, as a Mac that opens it reads them.
            opened = open(&self.m7_identity)?;
        }
        let everyone = opened.by_audience.get(&Audience::Everyone);
        let mut values = BTreeMap::new();
        for n in 1..N {
            if let Some(entry) = everyone.and_then(|entries| entries.get(&name(n))) {
                let winner = opened
                    .winning_ops
                    .get(&(Audience::Everyone, name(n)))
                    .ok_or("a value no op wins")?;
                values.insert(
                    name(n),
                    (entry.value.as_slice().to_vec(), self.by(&log, winner)),
                );
            }
        }
        let mut overtaken: BTreeMap<String, Vec<By>> = BTreeMap::new();
        for record in log_entries::read_overtaken(m22).map_err(|e| e.to_string())? {
            let by = record
                .by
                .as_ref()
                .map_or(By::Other, |by| self.by(&log, &by.record));
            overtaken.entry(record.name).or_default().push(by);
        }
        let mut holds = BTreeSet::new();
        let mut hold_unread = false;
        if let Ok(dir) = std::fs::read_dir(self.holds_dir()) {
            for entry in dir {
                let file = entry.map_err(|e| e.to_string())?.file_name();
                let file = file.to_string_lossy();
                if file == hold_unread_name() {
                    hold_unread = true;
                } else {
                    let d = (1..D)
                        .find(|&d| file == hold_name(d))
                        .ok_or_else(|| format!("a hold of no direct draft: {file}"))?;
                    holds.insert(d);
                }
            }
        }
        let conflicts = opened
            .conflicts
            .iter()
            .filter(|(audience, _)| *audience == Audience::Everyone)
            .map(|(_, name)| name.clone())
            .collect();
        Ok(Obs {
            pending,
            waiting,
            log: values,
            overtaken,
            holds,
            conflicts,
            unread: self.unread_file().exists(),
            hold_unread,
            due: self.observe_due(),
        })
    }

    /// m22's state beside `obs`, up to record ids, times, nonces and the log's history, which no
    /// function driven here reads but as the winning values, the conflicts and the epochs: the
    /// pending sets' files in their order (by rank) with their servers, the journal's records
    /// with each overtaker's kind and whether it deleted, the conflicted names, the files of the
    /// holds, and the newest epochs; as a digest.
    fn canon(&self, obs: &Obs) -> Result<[u8; 32], String> {
        let m22 = self.m22();
        let mut text = format!("{obs:?}\n");
        let mut files: Vec<String> = std::fs::read_dir(m22.dir.join("everyone.pending"))
            .into_iter()
            .flatten()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        files.sort();
        for (rank, file) in files.iter().enumerate() {
            let rest = file.split_once('-').map_or(file.as_str(), |(_, rest)| rest);
            text.push_str(&format!("pending {rank} {rest}\n"));
        }
        if !obs.unread {
            for (version, reach) in log_entries::pending_reach(m22, &self.identity) {
                text.push_str(&format!(
                    "reach {} {reach:?}\n",
                    aterm_codec::hex::encode_lower(&version)
                ));
            }
        }
        let log = log_of(m22);
        for record in log_entries::read_overtaken(m22).map_err(|e| e.to_string())? {
            let by = record
                .by
                .as_ref()
                .map(|by| (self.by(&log, &by.record), by.deleted));
            text.push_str(&format!(
                "overtaken {} {by:?} {}\n",
                record.name, record.shown
            ));
        }
        let opened = log
            .open_audience_entries(&self.identity)
            .map_err(|e| e.to_string())?
            .ok_or("m22 opens no epoch")?;
        text.push_str(&format!("conflicts {:?}\n", opened.conflicts));
        let newest = opened
            .maximal_epochs
            .get(&EpochAudience::Everyone)
            .cloned()
            .unwrap_or_default();
        text.push_str(&format!(
            "newest {} opens {} generation {:?}\n",
            newest.len(),
            newest
                .iter()
                .all(|epoch| opened.unwrapped_keys.contains_key(epoch)),
            opened.max_generation.get(&EpochAudience::Everyone)
        ));
        let mut holds: Vec<String> = std::fs::read_dir(self.holds_dir())
            .into_iter()
            .flatten()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        holds.sort();
        text.push_str(&format!("holds {holds:?}\n"));
        Ok(Sha256::digest(text.as_bytes()))
    }

    /// A divergence of the functions' own refusals: an error, or nothing where it is let through.
    fn parted(&self, field: &str, why: &str) -> Result<(), Fail> {
        if self.lenient.get() {
            Ok(())
        } else {
            Err(Fail::of(field, why))
        }
    }

    /// m22's draft of `writes` as the code drafts a publish whose writes wait: what its held
    /// drafts leave at each name, merged into each write's bases by the edit, beside the log's
    /// value; a take-back over what they leave; and the base another Mac's write puts where the
    /// event names it (a base a view showed). Whether its held drafts did not read.
    fn draft_writes(&self, writes: &[Write]) -> Result<(log_entries::Waiting, bool), Fail> {
        let m22 = self.m22();
        let io = |e: crate::Error| Fail::of("io", e.to_string());
        let store = load_records(m22).map_err(io)?;
        let log = store
            .log()
            .ok_or_else(|| Fail::of("io", "m22 holds no log"))?;
        let opened = log
            .open_audience_entries(&self.identity)
            .map_err(|e| Fail::of("io", e.to_string()))?
            .ok_or_else(|| Fail::of("io", "m22 opens no epoch"))?;
        let read = opened
            .by_audience
            .get(&Audience::Everyone)
            .cloned()
            .unwrap_or_default();
        let (held, unread) = log_entries::held_values_or_unread(m22, &store, &self.identity);
        let mut doc = read.clone();
        for x in writes.iter().filter(|x| x.val != 0) {
            doc.insert(name(x.name), entry(&name(x.name), &bytes(x.val)));
        }
        let edit = log_entries::everyone_edit(
            Some(&read),
            None,
            &log_entries::Waiting::default(),
            &held,
            Some(&doc),
            |_| false,
        );
        let mut waiting = edit.writes;
        for x in writes.iter().filter(|x| x.val == 0) {
            let n = name(x.name);
            let back = log_entries::Waiting::of_writes(&[(n.as_str(), None, &[None])])
                .with_bases(&n, held.get(&n).into_iter().flatten().copied());
            waiting = waiting.joined(back);
        }
        for x in writes.iter().filter(|x| x.bases.contains(&OTHER)) {
            waiting =
                waiting.with_bases(&name(x.name), [Some(log_entries::base_of(&bytes(OTHER)))]);
        }
        Ok((waiting, unread.is_some()))
    }

    /// Run `event` on m22 from the state `w` (`used`: the versions m22 published) and `env`
    /// stand for; whether it ran anything there. [`WLab::made`] says whether it made the event.
    fn act(&self, w: &WWorld, env: Env, used: &[bool; V], event: &WEvent) -> Result<bool, Fail> {
        let m22 = self.m22();
        let identity = &self.identity;
        let io = |what: &str, e: crate::Error| Fail::of("io", format!("{what}: {e}"));
        self.made.set(false);
        match event {
            WEvent::Publish {
                v,
                writes,
                opened,
                raced,
            } => {
                if used[*v] {
                    return Ok(false);
                }
                self.touched.set(true);
                let Ok(turn) = log_entries::draft_turn(m22, Some(Instant::now())) else {
                    if w.turn_free() {
                        self.parted("turn", "a publish found its turn taken, no draft in it")?;
                    }
                    return Ok(true);
                };
                let claim = log_entries::claim_draft(m22, Some(Instant::now()))
                    .map_err(|e| io("claim_draft", e))?;
                let (waiting, held_unread) = self.draft_writes(writes)?;
                if *raced {
                    // A vault pass between the draft and its pending set.
                    log_entries::sign_waiting_writes_by(m22, self.tick(), None)
                        .map_err(|e| io("a racing pass", e))?;
                }
                // The draft refuses over held drafts that do not read; this Mac's waiting writes
                // read here.
                if nle::draft_refuses(!waiting.is_empty(), false, held_unread) {
                    return Ok(true);
                }
                let made: log_entries::Shapes = writes
                    .iter()
                    .map(|x| (name(x.name), shape_of(x.val, &x.bases)))
                    .collect();
                if waiting.shapes() != made {
                    // The code's draft made other writes: another event than this one.
                    return Ok(true);
                }
                let stored: Vec<[u8; 32]> = opened.iter().map(|&u| vd(u)).collect();
                let went = log_entries::wait_for_key(
                    m22,
                    identity,
                    &waiting,
                    u64::try_from(*v).unwrap(),
                    &vd(*v),
                    &["https://s1.example/v".to_owned()],
                    &stored,
                    None,
                );
                drop((claim, turn));
                match (went, w.unread) {
                    (Ok(()), false) => self.made.set(true),
                    (Err(_), true) => {}
                    (Ok(()), true) => {
                        self.made.set(true);
                        self.parted("unread", "a publish went over a set that does not open")?;
                    }
                    (Err(e), false) => return Err(io("wait_for_key", e)),
                }
            }
            WEvent::Dropped(v) => {
                if w.stored[*v] {
                    return Ok(false);
                }
                log_entries::forget_waiting(m22, &vd(*v));
            }
            WEvent::Land(s) => {
                // A server holds each version of `s`: the environment lands only stored sets.
                let held = (0..w.next)
                    .all(|i| !(w.pend[i] && s.contains(&w.ver[i])) || w.stored[w.ver[i]]);
                if !held {
                    return Ok(false);
                }
                let versions: Vec<[u8; 32]> = s.iter().map(|&u| vd(u)).collect();
                match (
                    log_entries::land_waiting(m22, identity, &versions, None),
                    w.unread,
                ) {
                    (Ok(()), false) | (Err(_), true) => {}
                    (Ok(()), true) => {
                        self.parted("unread", "a landing went over a set that does not open")?;
                    }
                    (Err(e), false) => return Err(io("land_waiting", e)),
                }
            }
            WEvent::Other(n, x) => {
                let value = (*x != 0).then(|| bytes(*x));
                let record = draft_as(
                    m22,
                    identity,
                    &self.m7,
                    &name(*n),
                    value.as_deref(),
                    self.tick(),
                )
                .map_err(|e| Fail::of("io", e))?;
                save_records(m22, &encode_records(&[record])).map_err(|e| io("save", e))?;
            }
            WEvent::Due(true) if env.chain == 0 => {
                save_records(m22, &encode_records(&self.chain[..1])).map_err(|e| io("save", e))?;
            }
            WEvent::Due(false) if env.chain == 1 => {
                save_records(m22, &encode_records(&self.chain[1..])).map_err(|e| io("save", e))?;
            }
            WEvent::Opens(false) if env.chain == 2 && env.opens == 0 => {
                save_records(m22, &encode_records(&self.left_out)).map_err(|e| io("save", e))?;
            }
            WEvent::Opens(true) if env.opens == 1 => {
                save_records(m22, &encode_records(&self.wrap_m22)).map_err(|e| io("save", e))?;
            }
            WEvent::Unreadable(true) => {
                fsx::ensure_private_dir(&m22.dir.join("everyone.pending")).unwrap();
                fsx::write_atomic(&self.unread_file(), b"no set of this Mac's", 0o600).unwrap();
            }
            WEvent::Unreadable(false) => {
                let _ = std::fs::remove_file(self.unread_file());
            }
            WEvent::HoldUnreadable(true) => {
                fsx::ensure_private_dir(&self.holds_dir()).unwrap();
                let path = self.holds_dir().join(hold_unread_name());
                fsx::write_atomic(&path, b"no hold of this Mac's", 0o600).unwrap();
            }
            WEvent::HoldUnreadable(false) => {
                let _ = std::fs::remove_file(self.holds_dir().join(hold_unread_name()));
            }
            WEvent::DirectDraft(n, x) => {
                self.touched.set(true);
                let Ok(turn) = log_entries::draft_turn(m22, Some(Instant::now())) else {
                    if w.turn_free() {
                        self.parted(
                            "turn",
                            "a direct draft found its turn taken, no draft in it",
                        )?;
                    }
                    return Ok(true);
                };
                if !w.turn_free() {
                    self.parted(
                        "turn",
                        "a direct draft drafted while another publish of this Mac drafted",
                    )?;
                }
                let claim = log_entries::claim_draft(m22, Some(Instant::now()))
                    .map_err(|e| io("claim_draft", e))?;
                let record = draft_as(
                    m22,
                    identity,
                    &self.m22_key,
                    &name(*n),
                    Some(&bytes(*x)),
                    self.tick(),
                )
                .map_err(|e| Fail::of("io", e))?;
                self.directs.borrow_mut()[w.dnext] = Some(DirectRec {
                    record,
                    stamp: w.next,
                });
                *self.turn.borrow_mut() = Some(Box::new((turn, claim)));
                self.made.set(true);
            }
            WEvent::DirectHold(d) => {
                if w.dphase[*d] != 1 {
                    return Ok(false);
                }
                let drafted = self.drafted(*d);
                log_entries::hold_drafted(
                    m22,
                    &drafted,
                    direct_seq(*d),
                    &dd(*d),
                    &[direct_url(*d)],
                )
                .map_err(|e| io("hold_drafted", e))?;
                // The hold written, the publish's turn and claim end.
                *self.turn.borrow_mut() = None;
            }
            WEvent::Keep(d) => {
                if *d >= w.dnext {
                    return Ok(false);
                }
                let drafted = self.drafted(*d);
                let kept = log_entries::keep_drafted_until(
                    m22,
                    &drafted,
                    (direct_seq(*d), &dd(*d)),
                    &[],
                    None,
                    None,
                )
                .map_err(|e| io("keep_drafted_until", e))?;
                log_entries::forget_landed_draft(
                    m22,
                    direct_seq(*d),
                    &dd(*d),
                    &[],
                    &[direct_url(*d)],
                    true,
                    kept,
                    None,
                );
            }
            WEvent::Unhold(d) => match w.dphase[*d] {
                // The publish ended before its hold: its turn and claim end.
                1 => *self.turn.borrow_mut() = None,
                2 => log_entries::forget_held_draft(m22, direct_seq(*d), &dd(*d), None),
                _ => return Ok(false),
            },
            WEvent::Stored(_)
            | WEvent::SignPass(_)
            | WEvent::SettlePass
            | WEvent::Due(_)
            | WEvent::Opens(_) => {
                return Ok(false);
            }
        }
        self.touched.set(true);
        Ok(true)
    }

    /// What direct draft `d` drafted: its one record.
    fn drafted(&self, d: Direct) -> log_entries::Drafted {
        let directs = self.directs.borrow();
        let direct = directs[d].as_ref().expect("a direct draft the lab drafted");
        log_entries::Drafted {
            records: vec![direct.record.clone()],
            heads: vec![*direct.record.id()],
        }
    }

    /// Hold the draft turn and claim where `w` has a direct draft between its draft and its
    /// hold, and none where it has none: a search that put m22's home back puts its locks back.
    fn sync_turn(&self, w: &WWorld) -> Result<(), String> {
        let drafting = (1..w.dnext).any(|d| w.dphase[d] == 1);
        let mut turn = self.turn.borrow_mut();
        if drafting && turn.is_none() {
            let held = log_entries::draft_turn(self.m22(), Some(Instant::now()))
                .map_err(|e| e.to_string())?;
            let claim = log_entries::claim_draft(self.m22(), Some(Instant::now()))
                .map_err(|e| e.to_string())?;
            *turn = Some(Box::new((held, claim)));
        } else if !drafting {
            *turn = None;
        }
        Ok(())
    }

    /// A vault pass on m22 whose hook runs `between` (the step's event between its `signPass` and
    /// its `settlePass`, from the World `w` after `signPass`) between its commit and its settle;
    /// where the pass ends before a commit, `between` runs after it.
    fn pass(self: &Rc<Self>, w: &WWorld, env: Env, between: Option<&WEvent>) -> Result<(), Fail> {
        let ran: Rc<RefCell<Result<(), Fail>>> = Rc::new(RefCell::new(Ok(())));
        if let Some(event) = between {
            let (lab, w, event, out) = (Rc::clone(self), w.clone(), event.clone(), Rc::clone(&ran));
            let hook: Box<dyn FnOnce()> = Box::new(move || {
                *out.borrow_mut() = lab.act(&w, env, &w.used, &event).map(|_| ());
            });
            log_entries::PASS_COMMITTED.with(|slot| *slot.borrow_mut() = Some(hook));
        }
        self.touched.set(true);
        let signed = log_entries::sign_waiting_writes_by(self.m22(), self.tick(), None);
        if let Some(hook) = log_entries::PASS_COMMITTED.with(|slot| slot.borrow_mut().take()) {
            hook();
        }
        match signed {
            Ok(Some(_)) => {}
            Ok(None) => return Err(Fail::of("io", "the pass met a held lock")),
            Err(e) => return Err(Fail::of("io", format!("sign_waiting_writes_by: {e}"))),
        }
        ran.replace(Ok(()))
    }
}

/// One step: an event, or a pass (`signPass`, at most one event that takes no claim, then
/// `settlePass`).
type Step = Vec<WEvent>;

impl WLab {
    /// `step` from `w`, `env` and m22's state `canon` on both sides: the step's World, the
    /// environment and m22's state after, once m22 was compared with the World and the laws held
    /// of the World whose observables are m22's, each event's `CasStep` included.
    fn check(
        self: &Rc<Self>,
        w: &WWorld,
        env: Env,
        canon: [u8; 32],
        step: &[WEvent],
    ) -> Result<(WWorld, Env, [u8; 32]), Fail> {
        let advance = |x: &WWorld, event: &WEvent| -> Result<WWorld, Fail> {
            let y = wstep(COMPILED, x, event);
            cas_step(x, &y).map_err(Fail::law)?;
            laws(&y).map_err(Fail::law)?;
            Ok(y)
        };
        let (mut x, mut at, mut ran) = (w.clone(), env, true);
        if let [WEvent::SignPass(opens), between @ .., WEvent::SettlePass] = step {
            let signed = advance(w, &WEvent::SignPass(*opens))?;
            self.calls.set(self.calls.get() + 1);
            self.pass(&signed, env, between.first())?;
            x = signed;
            for event in between {
                x = advance(&x, event)?;
                at = next_env(at, event);
            }
            x = advance(&x, &WEvent::SettlePass)?;
        } else {
            ran = false;
            for event in step {
                if self.act(&x, at, &x.used, event)? {
                    self.calls.set(self.calls.get() + 1);
                    ran = true;
                }
                x = advance(&x, event)?;
                at = next_env(at, event);
            }
        }
        // Where nothing ran on m22, its files are as the comparison before found them: the step's
        // World before this step's.
        let real = if ran {
            self.observe().map_err(|e| Fail::of("io", e))?
        } else {
            observe(w)
        };
        let model = observe(&x);
        let fields = real.differs(&model);
        if !fields.is_empty() {
            return Err(Fail {
                fields: fields.into_iter().map(str::to_owned).collect(),
                why: format!("m22 diverges from the step: m22 holds {real:?}, the step {model:?}"),
            });
        }
        laws(&project(&x, &real).map_err(|e| Fail::of("projection", e))?).map_err(Fail::law)?;
        let canon = if ran {
            self.canon(&real).map_err(|e| Fail::of("io", e))?
        } else {
            canon
        };
        Ok((x, at, canon))
    }

    /// m22's home as it was at the start, its locks free and its direct drafts forgotten.
    fn reset(&self) {
        *self.turn.borrow_mut() = None;
        *self.directs.borrow_mut() = Default::default();
        Home::of(&self.m22().dir).turn_into(&self.start, &self.m22().dir);
    }

    /// `steps` from the start on both sides; the World, the environment and m22's state at the
    /// end, or the first step at which they part.
    fn replay(self: &Rc<Self>, steps: &[Step]) -> Result<(WWorld, Env, [u8; 32]), Divergence> {
        self.reset();
        let mut state = (WWorld::initial(), Env::default(), [0; 32]);
        for (at, step) in steps.iter().enumerate() {
            self.sync_turn(&state.0).map_err(|why| Divergence {
                at,
                fail: Fail::of("io", why),
            })?;
            state = self
                .check(&state.0, state.1, state.2, step)
                .map_err(|fail| Divergence { at, fail })?;
        }
        Ok(state)
    }

    /// `steps` from the start on m22 alone, each environment event and each event m22 made
    /// followed by the island's step in a World of m22's own ([`track`]), whose observables are
    /// m22's after each step and whose fates m22's files and journal say ([`fates`]); that World
    /// at the end, and the first law of the island it breaks on the way.
    fn replay_real(self: &Rc<Self>, steps: &[Step]) -> (WWorld, Option<String>) {
        self.reset();
        self.lenient.set(true);
        let (mut r, mut env) = (WWorld::initial(), Env::default());
        let mut seen = observe(&r);
        let mut broken = None;
        for step in steps {
            self.sync_turn(&r).unwrap();
            let before = r.clone();
            if let [WEvent::SignPass(_), between @ .., WEvent::SettlePass] = step.as_slice() {
                self.pass(&r, env, between.first()).unwrap();
                for event in step {
                    r = track(&r, event, true);
                    env = next_env(env, event);
                }
            } else {
                for event in step {
                    self.act(&r, env, &r.used, event).unwrap();
                    r = track(&r, event, self.made.get());
                    env = next_env(env, event);
                }
            }
            let now = self.observe().unwrap();
            r = fates(&before, &seen, &project(&r, &now).unwrap(), &now);
            seen = now;
            if broken.is_none() {
                broken = cas_step(&before, &r).and_then(|()| laws(&r)).err();
            }
        }
        self.lenient.set(false);
        (r, broken)
    }
}

/// The island's step of `event` in m22's own World `r`, where `made` says m22 made it: a publish
/// and a direct draft by their bodies whatever their guards (m22 made them), every other event by
/// the island's step. What m22's files hold replaces the observables after ([`project`]).
fn track(r: &WWorld, event: &WEvent, made: bool) -> WWorld {
    match event {
        WEvent::Publish {
            v, writes, opened, ..
        } => {
            if made {
                r.land_w(COMPILED, opened).push_w(*v, writes)
            } else {
                r.clone()
            }
        }
        WEvent::DirectDraft(n, x) => {
            if made {
                r.push_direct(*n, *x)
            } else {
                r.clone()
            }
        }
        _ => wstep(COMPILED, r, event),
    }
}

/// What became of each write that was pending or waiting in `before` (whose observation was
/// `seen`) and is neither in `now` (observed `obs`), as m22's files and journal say: overtaken,
/// by whom the journal names, where the journal at its name grew while it waited; signed or done
/// where the log holds its value; replaced where a later write of its name, pending before, waits
/// now; dropped where its version is on no server for good; otherwise nothing, and so lost.
fn fates(before: &WWorld, seen: &Obs, now: &WWorld, obs: &Obs) -> WWorld {
    let mut r = now.clone();
    for i in 1..now.next {
        let n = now.nm[i];
        let (was_pend, was_wait) = (
            i < before.next && before.pend[i],
            before.wait_of[n] == i && i != 0,
        );
        if !(was_pend || was_wait) || now.pend[i] || now.wait_of[n] == i {
            r.fate[i] = if i < before.next { before.fate[i] } else { 0 };
            r.over_by[i] = if i < before.next {
                before.over_by[i]
            } else {
                0
            };
            r.over_d[i] = if i < before.next { before.over_d[i] } else { 0 };
            continue;
        }
        let journal = |o: &Obs| o.overtaken.get(&name(n)).map_or(0, Vec::len);
        let (fate, over_by, over_d) = if was_wait && journal(obs) > journal(seen) {
            match obs.overtaken[&name(n)].last() {
                Some(By::Pass) => (2, before.holder[n].max(1), 0),
                Some(By::Direct(stamp)) => (2, 0, *stamp),
                _ => (2, 0, 0),
            }
        } else if was_wait && now.log[n] == now.val[i] {
            (1, 0, 0)
        } else if (i + 1..now.next)
            .any(|j| now.nm[j] == n && now.wait_of[n] == j && j < before.next && before.pend[j])
        {
            (3, 0, 0)
        } else if was_pend && now.refused[now.ver[i]] {
            (4, 0, 0)
        } else {
            (0, 0, 0)
        };
        r.fate[i] = fate;
        r.over_by[i] = over_by;
        r.over_d[i] = over_d;
    }
    r
}

/// The bounds of a search.
struct Universe {
    /// Versions 1 to this are published, in order.
    versions: usize,
    /// The names a publish may write together.
    shapes: &'static [&'static [Name]],
    /// The most direct drafts.
    directs: usize,
    /// The names a direct draft may write.
    direct_names: &'static [Name],
    /// Whether m7 may take the newest epoch from m22 and give it back, once the rotation arrived.
    opens: bool,
    /// The most steps a sequence takes.
    depth: usize,
}

/// What a search covered.
#[derive(Debug, Default)]
struct Covered {
    /// States of both sides (the World, the environment and m22's state).
    states: usize,
    /// States of the World and the environment alone.
    model_states: usize,
    steps: usize,
    calls: usize,
    deepest: usize,
}

/// The versions of `w` a server took whose sets are pending, each nonempty set of them.
fn landings(w: &WWorld) -> Vec<Vec<Ver>> {
    let held: Vec<Ver> = (1..V)
        .filter(|&v| w.stored[v] && (1..w.next).any(|i| w.pend[i] && w.ver[i] == v))
        .collect();
    (1..1usize << held.len())
        .map(|mask| {
            held.iter()
                .enumerate()
                .filter(|&(at, _)| mask & (1 << at) != 0)
                .map(|(_, &v)| v)
                .collect()
        })
        .collect()
}

/// The steps the universe takes from `w`, the environment at `env`.
///
/// A publish writes each name of a shape once, its value its write's id; each write's bases are
/// the log's value there and what this Mac's held drafts leave there (as the code drafts them),
/// or for `n1` those and [`OTHER`] (a base a view showed), or it takes `n1` back to absent where
/// the log holds none. It opens version 0 alone or that and a stored version whose set is
/// pending. Where the draft refuses (a set or a hold does not read) or finds its turn taken, one
/// publish stands for every other; where a write waits, one publish has a pass race its draft.
/// Another Mac writes `n1` alone: [`OTHER`], or a `Delete` where the log holds it. A pass runs
/// only where a write waits (a pass over none changes nothing), with nothing between, a landing,
/// or another Mac's write of `n1`. While m22 is left out of the newest epoch it
/// publishes nothing (the code seals a publish under the newest epoch) and m7 writes nothing (the
/// lab's m7 writes under the epoch m22 seals under); its pass runs, and signs nothing.
///
/// A direct draft is m22's own `Put`, which no rotation due holds back (the code's edit waits
/// then), drafted where m22 opens the newest epoch. Two bounds keep the island's one value per
/// name the log's (the log holds concurrent writes of a name as a conflict, which [`Obs`] reads):
/// while a direct draft of a name is drafted and not kept or gone, no other direct draft writes
/// it (the lab drafts each on m22's log alone) and another Mac does not (two Macs' concurrent
/// writes are no P3 matter). A pass runs while a direct draft is held, and defers its name, as
/// the island's does; it does not run while a direct draft holds its turn and claim (from its
/// draft to its hold), where the code's claim refuses every pass, which the island lets sign
/// the names no draft in flight writes.
fn steps(w: &WWorld, env: Env, u: &Universe) -> Vec<Step> {
    let mut out: Vec<Step> = Vec::new();
    let turn_free = w.turn_free();
    if let Some(v) = (1..=u.versions).find(|&v| !w.used[v] && env.opens != 1) {
        let mut publishes = Vec::new();
        for shape in u.shapes {
            let mut drafts: Vec<Vec<Write>> = vec![Vec::new()];
            for (a, &n) in shape.iter().enumerate() {
                let id = u8::try_from(w.next + a).unwrap();
                let read = w.log[n];
                let held = w.held_values(n);
                let bases = |more: &[Val]| {
                    let mut bases: Vec<Val> =
                        [read].iter().chain(more).chain(&held).copied().collect();
                    bases.sort_unstable();
                    bases.dedup();
                    bases
                };
                let mut options = vec![Write {
                    name: n,
                    val: id,
                    bases: bases(&[]),
                }];
                if read != OTHER && n == 1 {
                    options.push(Write {
                        name: n,
                        val: id,
                        bases: bases(&[OTHER]),
                    });
                }
                if read == 0 && n == 1 {
                    options.push(Write {
                        name: n,
                        val: 0,
                        bases: bases(&[]),
                    });
                }
                drafts = drafts
                    .into_iter()
                    .flat_map(|draft| {
                        options.iter().map(move |option| {
                            let mut next = draft.clone();
                            next.push(option.clone());
                            next
                        })
                    })
                    .collect();
            }
            let mut opened = vec![vec![0]];
            opened.extend(
                landings(w)
                    .into_iter()
                    .filter(|s| s.len() == 1)
                    .map(|s| vec![0, s[0]]),
            );
            for draft in drafts {
                for opened in &opened {
                    publishes.push(vec![WEvent::Publish {
                        v,
                        writes: draft.clone(),
                        opened: opened.clone(),
                        raced: false,
                    }]);
                }
            }
        }
        if w.unread || w.hold_unread || !turn_free {
            publishes.truncate(1);
        } else if w.wait_of.iter().any(|&i| i != 0)
            && let Some(first) = publishes.first()
            && let [
                WEvent::Publish {
                    v, writes, opened, ..
                },
            ] = first.as_slice()
        {
            let raced = vec![WEvent::Publish {
                v: *v,
                writes: writes.clone(),
                opened: opened.clone(),
                raced: true,
            }];
            publishes.push(raced);
        }
        out.extend(publishes);
    }
    for v in 1..=u.versions {
        if w.used[v] && !w.stored[v] && !w.refused[v] {
            out.push(vec![WEvent::Stored(v)]);
            out.push(vec![WEvent::Dropped(v)]);
        }
    }
    let mut lands: Vec<WEvent> = landings(w).into_iter().map(WEvent::Land).collect();
    if w.unread {
        lands.truncate(1);
    }
    out.extend(lands.iter().cloned().map(|land| vec![land]));
    let other_may = env.opens != 1 && !w.drafting(1);
    let pass_may = turn_free;
    if w.wait_of.iter().any(|&i| i != 0) && pass_may {
        let opens = env.opens != 1;
        out.push(vec![WEvent::SignPass(opens), WEvent::SettlePass]);
        let mut between = lands;
        if other_may {
            between.push(WEvent::Other(1, OTHER));
        }
        for event in between {
            out.push(vec![WEvent::SignPass(opens), event, WEvent::SettlePass]);
        }
    }
    if other_may {
        if !(w.log[1] == OTHER && holder_of(w, 1) == By::Other) {
            out.push(vec![WEvent::Other(1, OTHER)]);
        }
        if w.log[1] != 0 {
            out.push(vec![WEvent::Other(1, 0)]);
        }
    }
    match env.chain {
        0 => out.push(vec![WEvent::Due(true)]),
        1 => out.push(vec![WEvent::Due(false)]),
        _ => {}
    }
    out.push(vec![WEvent::Unreadable(!w.unread)]);
    if u.directs > 0 {
        if w.dnext <= u.directs && env.chain != 1 && env.opens != 1 {
            let mut drafts: Vec<Step> = u
                .direct_names
                .iter()
                .filter(|&&n| !w.drafting(n))
                .map(|&n| vec![WEvent::DirectDraft(n, 40 + u8::try_from(w.dnext).unwrap())])
                .collect();
            if !turn_free {
                drafts.truncate(1);
            }
            out.extend(drafts);
        }
        for (d, &phase) in w.dphase.iter().enumerate().take(w.dnext).skip(1) {
            match phase {
                1 => {
                    out.push(vec![WEvent::DirectHold(d)]);
                    out.push(vec![WEvent::Unhold(d)]);
                }
                2 => {
                    out.push(vec![WEvent::Keep(d)]);
                    out.push(vec![WEvent::Unhold(d)]);
                }
                _ => out.push(vec![WEvent::Keep(d)]),
            }
        }
        out.push(vec![WEvent::HoldUnreadable(!w.hold_unread)]);
    }
    if u.opens && env.chain == 2 {
        match env.opens {
            0 => out.push(vec![WEvent::Opens(false)]),
            1 => out.push(vec![WEvent::Opens(true)]),
            _ => {}
        }
    }
    out
}

/// Every sequence of `u` after `prefix` from the start, depth first, each step on both sides
/// ([`WLab::check`]), a state of both sides the search reached before with as many steps left
/// explored once; the first divergence, with the sequence that reached it.
fn search(lab: &Rc<WLab>, u: &Universe, prefix: &[Step]) -> Result<Covered, String> {
    let (w, env, canon) = lab
        .replay(prefix)
        .map_err(|d| format!("the prefix diverges at step {}: {}", d.at, d.fail))?;
    lab.calls.set(0);
    let mut covered = Covered::default();
    let mut seen: HashMap<(WWorld, Env, [u8; 32]), usize> = HashMap::new();
    let mut models: HashSet<(WWorld, Env)> = HashSet::new();
    let mut path = prefix.to_vec();
    explore(
        lab,
        u,
        (&w, env, canon),
        u.depth,
        (&mut seen, &mut models),
        &mut path,
        &mut covered,
    )?;
    covered.states = seen.len();
    covered.model_states = models.len();
    covered.calls = lab.calls.get();
    Ok(covered)
}

/// What a search has seen: each state of both sides with the most steps left it was explored
/// with, and each state of the World and the environment.
type Seen<'a> = (
    &'a mut HashMap<(WWorld, Env, [u8; 32]), usize>,
    &'a mut HashSet<(WWorld, Env)>,
);

fn explore(
    lab: &Rc<WLab>,
    u: &Universe,
    (w, env, canon): (&WWorld, Env, [u8; 32]),
    left: usize,
    (seen, models): Seen<'_>,
    path: &mut Vec<Step>,
    covered: &mut Covered,
) -> Result<(), String> {
    covered.deepest = covered.deepest.max(u.depth - left);
    if left == 0 {
        return Ok(());
    }
    let here = Home::of(&lab.m22().dir);
    lab.touched.set(false);
    for step in steps(w, env, u) {
        if lab.touched.replace(false) {
            Home::of(&lab.m22().dir).turn_into(&here, &lab.m22().dir);
        }
        lab.sync_turn(w)?;
        path.push(step.clone());
        let (next, next_env, next_canon) = lab
            .check(w, env, canon, &step)
            .map_err(|fail| format!("after {path:?}: {fail}"))?;
        covered.steps += 1;
        models.insert((next.clone(), next_env));
        let key = (next, next_env, next_canon);
        if (key.0 != *w || key.1 != env || key.2 != canon)
            && seen.get(&key).is_none_or(|&had| had < left - 1)
        {
            seen.insert(key.clone(), left - 1);
            explore(
                lab,
                u,
                (&key.0, key.1, key.2),
                left - 1,
                (seen, models),
                path,
                covered,
            )?;
            lab.touched.set(true);
        }
        path.pop();
    }
    Ok(())
}

/// A publish of version `v` of `writes` (name, value, bases), opening no version.
fn publish_over(v: Ver, writes: &[(Name, Val, &[Val])]) -> Step {
    vec![WEvent::Publish {
        v,
        writes: writes
            .iter()
            .map(|&(name, val, bases)| Write {
                name,
                val,
                bases: bases.to_vec(),
            })
            .collect(),
        opened: Vec::new(),
        raced: false,
    }]
}

/// A publish of version `v` of `writes` (name, value), each over the log's absent value, opening
/// no version: the island's runs' `publish v k names vals (baseIs 0) opensNone`.
fn publish(v: Ver, writes: &[(Name, Val)]) -> Step {
    let writes: Vec<(Name, Val, &[Val])> = writes
        .iter()
        .map(|&(name, val)| (name, val, &[0][..]))
        .collect();
    publish_over(v, &writes)
}

fn pass() -> Step {
    vec![WEvent::SignPass(true), WEvent::SettlePass]
}

fn one(event: WEvent) -> Step {
    vec![event]
}

/// The island's §10 runs in this universe's names (its 5 is `n1`, its 6 `n2`), each step a step
/// of [`steps`]' shape.
fn two_publishes() -> Vec<Step> {
    vec![
        publish(1, &[(1, 11)]),
        publish(2, &[(2, 7)]),
        one(WEvent::Stored(1)),
        one(WEvent::Land(vec![1])),
    ]
}

fn unread_set() -> Vec<Step> {
    vec![
        publish(1, &[(1, 11)]),
        one(WEvent::Unreadable(true)),
        one(WEvent::Stored(1)),
        one(WEvent::Land(vec![1])),
    ]
}

fn failed_publish() -> Vec<Step> {
    vec![publish(1, &[(1, 11)]), one(WEvent::Dropped(1)), pass()]
}

fn two_servers() -> Vec<Step> {
    vec![
        publish(1, &[(1, 11), (2, 7)]),
        publish(2, &[(1, 12)]),
        one(WEvent::Stored(1)),
        one(WEvent::Stored(2)),
        one(WEvent::Land(vec![2])),
        one(WEvent::Land(vec![1])),
    ]
}

fn sign_between() -> Vec<Step> {
    vec![
        publish(1, &[(1, 11)]),
        publish(2, &[(1, 12)]),
        one(WEvent::Stored(1)),
        one(WEvent::Stored(2)),
        one(WEvent::Land(vec![1])),
        pass(),
        one(WEvent::Land(vec![2])),
        pass(),
    ]
}

fn pass_over_unread() -> Vec<Step> {
    vec![
        publish(1, &[(1, 11)]),
        publish(2, &[(1, 12)]),
        one(WEvent::Stored(1)),
        one(WEvent::Stored(2)),
        one(WEvent::Land(vec![1])),
        one(WEvent::Unreadable(true)),
        pass(),
        one(WEvent::Unreadable(false)),
        one(WEvent::Land(vec![2])),
        pass(),
    ]
}

/// The island's direct-draft runs: this Mac drafts `n1` to 11 directly (direct draft 1), then a
/// publish writes `n1` to 12 as a waiting write (write 1). A run offers the mutant's publish first
/// (bases: the log's 0), then the shipped one's (bases also 11).
fn mutant_publish() -> Step {
    publish_over(1, &[(1, 12, &[0])])
}

fn shipped_publish() -> Step {
    publish_over(1, &[(1, 12, &[0, 11])])
}

fn land_keep_sign() -> Vec<Step> {
    vec![
        one(WEvent::Stored(1)),
        one(WEvent::Land(vec![1])),
        one(WEvent::Keep(1)),
        pass(),
    ]
}

fn held_before_write() -> Vec<Step> {
    let mut run = vec![
        one(WEvent::DirectDraft(1, 11)),
        one(WEvent::DirectHold(1)),
        mutant_publish(),
        shipped_publish(),
    ];
    run.extend(land_keep_sign());
    run
}

fn draft_in_turn() -> Vec<Step> {
    let mut run = vec![
        one(WEvent::DirectDraft(1, 11)),
        mutant_publish(),
        one(WEvent::DirectHold(1)),
        shipped_publish(),
    ];
    run.extend(land_keep_sign());
    run
}

fn keep_after_unhold() -> Vec<Step> {
    let mut run = vec![
        one(WEvent::DirectDraft(1, 11)),
        one(WEvent::DirectHold(1)),
        one(WEvent::Unhold(1)),
        mutant_publish(),
    ];
    run.extend(land_keep_sign());
    run
}

fn unread_hold() -> Vec<Step> {
    let mut run = vec![
        one(WEvent::DirectDraft(1, 11)),
        one(WEvent::DirectHold(1)),
        one(WEvent::HoldUnreadable(true)),
        mutant_publish(),
        one(WEvent::HoldUnreadable(false)),
        shipped_publish(),
    ];
    run.extend(land_keep_sign());
    run
}

/// The island's `passBeforeKeep`: this Mac drafts `n1` to 11 directly and holds it, a publish
/// writes `n1` to 12 over it, a server takes it, and a pass runs before the draft's keep, then
/// one after.
fn pass_before_keep() -> Vec<Step> {
    vec![
        one(WEvent::DirectDraft(1, 11)),
        one(WEvent::DirectHold(1)),
        shipped_publish(),
        one(WEvent::Stored(1)),
        one(WEvent::Land(vec![1])),
        pass(),
        one(WEvent::Keep(1)),
        pass(),
    ]
}

/// v1 writes `n1` = 11, a server takes it and it waits; v2's draft of `n1` = 12, over the log's
/// absent value, races a vault pass, which its claim holds off; v2 lands and a pass signs 12.
fn raced_draft() -> Vec<Step> {
    let mut raced = publish(2, &[(1, 12)]);
    if let [WEvent::Publish { raced, .. }] = raced.as_mut_slice() {
        *raced = true;
    }
    vec![
        publish(1, &[(1, 11)]),
        one(WEvent::Stored(1)),
        one(WEvent::Land(vec![1])),
        raced,
        one(WEvent::Stored(2)),
        one(WEvent::Land(vec![2])),
        pass(),
    ]
}

/// v1 writes `n1` = 11, a server takes it and it waits; m7 writes `n1` = 99 over it; a pass
/// drops it as overtaken.
fn overtaken_by_m7() -> Vec<Step> {
    vec![
        publish(1, &[(1, 11)]),
        one(WEvent::Stored(1)),
        one(WEvent::Land(vec![1])),
        one(WEvent::Other(1, OTHER)),
        pass(),
    ]
}

/// Nine publishes of `n1`, none stored: A3's bound of eight sets is past.
fn nine_publishes() -> Vec<Step> {
    (1..=9)
        .map(|v| publish(v, &[(1, u8::try_from(10 + v).unwrap())]))
        .collect()
}

/// v1 writes `n1` and `n2`, a server takes it, and both wait.
fn two_waiting() -> Vec<Step> {
    vec![
        publish(1, &[(1, 1), (2, 2)]),
        one(WEvent::Stored(1)),
        one(WEvent::Land(vec![1])),
    ]
}

/// Three pending sets: v1 writes `n1`, v2 `n1` and `n2`, v3 `n1`, none stored yet.
fn three_pending() -> Vec<Step> {
    vec![
        publish(1, &[(1, 1)]),
        publish(2, &[(1, 2), (2, 3)]),
        publish(3, &[(1, 4)]),
    ]
}

/// [`two_waiting`], then the removal's `Remove` and its rotation.
fn after_rotation() -> Vec<Step> {
    let mut run = two_waiting();
    run.extend([one(WEvent::Due(true)), one(WEvent::Due(false))]);
    run
}

/// Runs `steps` with `control` on and asserts that m22 parts from the step at step `at` in
/// `fields`; then runs them on m22 alone and returns the World of m22's files at the end and the
/// first law of the island it broke.
fn diverges(
    lab: &Rc<WLab>,
    control: &'static std::thread::LocalKey<Cell<bool>>,
    name: &str,
    steps: &[Step],
    (at, fields): (usize, &[&str]),
) -> (WWorld, Option<String>) {
    control.with(|cell| cell.set(true));
    let diverged = lab.replay(steps);
    let real = lab.replay_real(steps);
    control.with(|cell| cell.set(false));
    let divergence = diverged.expect_err(&format!("CONTROL: {name} diverges from the step"));
    eprintln!(
        "P3 waiting conformance, {name}: at step {} ({:?}): {}",
        divergence.at, steps[divergence.at], divergence.fail
    );
    assert_eq!(
        (
            divergence.at,
            divergence
                .fail
                .fields
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        ),
        (at, fields.to_vec()),
        "CONTROL: {name} parts from the step at its own event, in its own fields: {}",
        divergence.fail
    );
    eprintln!("P3 waiting conformance, {name}, on m22 alone: {:?}", real.1);
    real
}

/// [`diverges`], and the law of the island the control breaks on m22's files, `law`.
fn breaks(
    lab: &Rc<WLab>,
    control: &'static std::thread::LocalKey<Cell<bool>>,
    name: &str,
    steps: &[Step],
    at: (usize, &[&str]),
    law: &str,
) -> WWorld {
    let (real, broken) = diverges(lab, control, name, steps, at);
    let broken = broken.unwrap_or_else(|| panic!("CONTROL: {name} breaks a law on m22's files"));
    assert!(
        broken.starts_with(law),
        "CONTROL: {name} breaks {law} on m22's files: {broken}"
    );
    real
}

/// Two versions, the first writing `n1`, or `n1` and `n2`, the second over version 0 or the
/// first once a server took it and its set is pending; every step of [`steps`], to four steps.
const UNIVERSE: Universe = Universe {
    versions: 2,
    shapes: &[&[1], &[1, 2]],
    directs: 0,
    direct_names: &[],
    opens: false,
    depth: 4,
};

/// From [`two_waiting`], a second version writing `n1`, or `n1` and `n2`, over version 0 or the
/// first; every step of [`steps`], to four steps: a pass while a rotation is due, after it
/// arrived, over another Mac's write or a set that does not open, and over a later pending set.
const WAITING: Universe = Universe {
    versions: 2,
    shapes: &[&[1], &[1, 2]],
    directs: 0,
    direct_names: &[],
    opens: false,
    depth: 4,
};

/// From [`three_pending`]: each stored or dropped, any of them landing alone or together, passes
/// between, to six steps (two sets that land together, then a pass, then the third).
const THREE_SETS: Universe = Universe {
    versions: 3,
    shapes: &[&[1]],
    directs: 0,
    direct_names: &[],
    opens: false,
    depth: 6,
};

/// From the start, one version writing `n1`, and up to two direct drafts of `n1` or `n2`, held,
/// kept, let go, a hold that does not read; to five steps.
const DIRECTS: Universe = Universe {
    versions: 1,
    shapes: &[&[1]],
    directs: 2,
    direct_names: &[1, 2],
    opens: false,
    depth: 5,
};

/// From a direct draft of `n1` held ([`WEvent::DirectHold`]), as [`DIRECTS`], to five more
/// steps: the publish over it, its landing, the keep, the pass.
const HELD: Universe = Universe {
    versions: 1,
    shapes: &[&[1]],
    directs: 2,
    direct_names: &[1],
    opens: false,
    depth: 5,
};

/// From [`after_rotation`], a second version writing `n1`, and m7 taking the newest epoch from
/// m22 and giving it back; to four steps: a pass that does not open the newest epoch.
const OPENS: Universe = Universe {
    versions: 2,
    shapes: &[&[1]],
    directs: 0,
    direct_names: &[],
    opens: true,
    depth: 4,
};

/// The search of `u` after `prefix`, its numbers printed; the first divergence fails the test.
fn conforms(u: &Universe, prefix: &[Step], name: &str) -> Rc<WLab> {
    let lab = Rc::new(WLab::new());
    let started = Instant::now();
    let covered = search(&lab, u, prefix).unwrap_or_else(|why| panic!("{why}"));
    eprintln!(
        "P3 waiting conformance, {name}: {} states of both sides ({} of the World alone), {} \
         steps, {} real calls, deepest {}, in {:?}",
        covered.states,
        covered.model_states,
        covered.steps,
        covered.calls,
        covered.deepest,
        started.elapsed()
    );
    assert_eq!(
        covered.deepest, u.depth,
        "the search reaches its depth: {covered:?}"
    );
    lab
}

/// A run that conforms, its World at the end.
fn run(lab: &Rc<WLab>, steps: &[Step], name: &str) -> WWorld {
    lab.replay(steps)
        .unwrap_or_else(|d| panic!("{name} conforms: at step {}: {}", d.at, d.fail))
        .0
}

/// P3 binding (3), waiting writes: m22's pending sets, waiting writes, log, journal and unread
/// sets follow the island's waiting step after every step of every sequence of [`UNIVERSE`] and of
/// the island's §10 runs, whose ends the island's lemmas name, and the island's laws hold of them.
///
/// NEGATIVE CONTROLS, each run in the test on the island's run for it, each parting from the step
/// at its own step and fields and, where m22's files can show it, breaking the island's law on
/// them: `race-undeferred` (a pass signs a write a pending set rewrites) on `signBetween`, at the
/// first pass (m22 signs v1's 11, the step leaves it waiting), then the second pass drops v2's 12
/// as overtaken by m22's own pass (`OvertakenByAnother`); `unread-as-none` (a set that does not
/// open reads as none) on `unreadSet`, at the landing the step refuses (the file that does not open
/// is no set of the universe's, so m22's files lose no write of it); `pending-by-count` (A3's
/// bound) on nine publishes, at the ninth, which drops v1's set, and once v1 is stored, its write
/// is lost (`NoLostWrite`); `overtaken-unrecorded` on m7's write over a waiting write, at the
/// pass, which drops it with no record (`NoLostWrite`); `draft-unclaimed` (a publish takes no
/// claim) on a draft a pass races, at that publish (the pass signs v1's 11 under v2's draft), and
/// v2's 12 is then dropped as overtaken by m22's own pass (`OvertakenByAnother`).
///
/// The switches no universe here reaches, and why: `bases-log-only` and `names-unmerged` switch
/// how the edit merges a view's waiting writes (the driver drafts from no view: `bases-log-only`'s
/// loss of the held values is the direct universe's, below), `settle-uncarried` a save's settle of
/// a conflict, `view-without-waiting` the admin view, `moves-wait` host-list moves,
/// `writers-dont-wait` whether an edit's writes wait, and `unobserved-draft` the draft's reads of
/// the log and the waiting writes under one observation: the island takes each as the publish's
/// input (its writes, its bases), so none is a step of its World; their own tests in
/// `waiting_tests.rs` catch them. The island's `failedPublish` (wait at once) and `twoServers` (a
/// landing that drops every other set) name rules no switch restores; their runs conform here.
#[test]
fn waiting_writes_follow_p3s_step_through_every_sequence_of_the_universe() {
    let lab = conforms(&UNIVERSE, &[], "from the start");

    let w = run(&lab, &two_publishes(), "twoPublishes");
    assert!(
        accounted(&w, 1) && w.stored[1],
        "every_set_stays_until_its_proof"
    );
    let w = run(&lab, &unread_set(), "unreadSet");
    assert!(accounted(&w, 1), "an_unread_set_stays");
    let mut read = unread_set();
    read.extend([one(WEvent::Unreadable(false)), one(WEvent::Land(vec![1]))]);
    assert_eq!(
        run(&lab, &read, "unreadSet, read").wait_of[1],
        1,
        "it_lands_once_it_reads"
    );
    let w = run(&lab, &failed_publish(), "failedPublish");
    assert_eq!(w.log[1], 0, "pending_until_stored_signs_nothing");
    let w = run(&lab, &two_servers(), "twoServers");
    assert!(accounted(&w, 2), "land_by_version_keeps_only6");
    assert_eq!(w.wait_of[1], 3, "the_later_write_of_5_waits");
    let w = run(&lab, &sign_between(), "signBetween");
    assert_eq!(w.log[1], 12, "the_later_write_wins");
    assert!(w.fate[2] != 2, "nothing_is_overtaken");
    let w = run(&lab, &pass_over_unread(), "passOverUnread");
    assert_eq!(w.log[1], 12, "a_pass_over_an_unread_set_signs_nothing");
    let w = run(&lab, &overtaken_by_m7(), "overtaken by m7");
    assert_eq!(w.fate[1], 2, "m7's write overtakes the waiting one");
    run(&lab, &nine_publishes(), "nine publishes");
    let w = run(&lab, &raced_draft(), "a draft a pass races");
    assert_eq!(
        w.log[1], 12,
        "the claim holds the pass off: the later write wins"
    );

    let real = breaks(
        &lab,
        &log_entries::RACE_UNDEFERRED,
        "race-undeferred",
        &sign_between(),
        (5, &["waiting", "log"]),
        "OvertakenByAnother",
    );
    assert_eq!(
        (real.log[1], real.fate[2]),
        (11, 2),
        "CONTROL: undeferred_keeps_the_earlier_write, and drops the later as overtaken"
    );
    diverges(
        &lab,
        &log_entries::UNREAD_AS_NONE,
        "unread-as-none",
        &unread_set(),
        (3, &["unread"]),
    );
    let mut stored = nine_publishes();
    stored.push(one(WEvent::Stored(1)));
    breaks(
        &lab,
        &log_entries::PENDING_BY_COUNT,
        "pending-by-count",
        &stored,
        (8, &["pending"]),
        "NoLostWrite",
    );
    breaks(
        &lab,
        &log_entries::OVERTAKEN_UNRECORDED,
        "overtaken-unrecorded",
        &overtaken_by_m7(),
        (4, &["overtaken"]),
        "NoLostWrite",
    );
    let real = breaks(
        &lab,
        &log_entries::DRAFT_UNCLAIMED,
        "draft-unclaimed",
        &raced_draft(),
        (3, &["waiting", "log"]),
        "OvertakenByAnother",
    );
    assert_eq!(
        (real.log[1], real.fate[2], real.over_by[2] != 0),
        (11, 2, true),
        "CONTROL: the later write is dropped as overtaken by m22's own pass"
    );
}

/// P3 binding (3), waiting writes, as the test from the start
/// ([`waiting_writes_follow_p3s_step_through_every_sequence_of_the_universe`]) over [`WAITING`],
/// from two waiting writes, and on a removal's rotation: a pass while it is due signs nothing,
/// and once it arrived the next pass signs the write. Then over [`THREE_SETS`], from three
/// pending sets, and over [`OPENS`], after the rotation, where a pass that does not open the
/// newest `[everyone]` epoch signs nothing, and once m7's `Wrap` to m22 arrived the next signs.
///
/// NEGATIVE CONTROL, run in the test: `race-undeferred` diverges from two waiting writes once v2
/// rewrites `n1`, at the pass, which signs v1's 1 under v2's pending set; once v2 lands, the
/// next pass drops its 3 as overtaken by m22's own pass (`OvertakenByAnother`).
#[test]
fn waiting_writes_follow_p3s_step_from_two_waiting_writes() {
    let lab = conforms(&WAITING, &two_waiting(), "from two waiting writes");
    let mut due = vec![one(WEvent::Due(true))];
    due.extend(two_waiting());
    due.push(pass());
    let w = run(&lab, &due, "a due pass");
    assert_eq!(
        (w.log[1], w.wait_of[1]),
        (0, 1),
        "a pass while a rotation is due signs nothing"
    );
    due.extend([one(WEvent::Due(false)), pass()]);
    let w = run(&lab, &due, "the rotation");
    assert_eq!(
        (w.log[1], w.wait_of[1]),
        (1, 0),
        "once the rotation arrived the pass signs"
    );
    let mut raced = two_waiting();
    raced.extend([
        publish(2, &[(1, 3)]),
        pass(),
        one(WEvent::Stored(2)),
        one(WEvent::Land(vec![2])),
        pass(),
    ]);
    let real = breaks(
        &lab,
        &log_entries::RACE_UNDEFERRED,
        "race-undeferred, two waiting",
        &raced,
        (4, &["waiting", "log"]),
        "OvertakenByAnother",
    );
    assert_eq!(
        (real.log[1], real.fate[3]),
        (1, 2),
        "CONTROL: v1's 1 signed under v2's pending 3, which a pass then drops as overtaken"
    );

    conforms(&THREE_SETS, &three_pending(), "from three pending sets");

    let lab = conforms(&OPENS, &after_rotation(), "after the rotation");
    let mut closed = after_rotation();
    closed.extend([
        one(WEvent::Opens(false)),
        vec![WEvent::SignPass(false), WEvent::SettlePass],
    ]);
    let w = run(&lab, &closed, "a pass that does not open the newest epoch");
    assert_eq!(
        (w.log[1], w.wait_of[1]),
        (0, 1),
        "a pass that does not open the newest epoch signs nothing"
    );
    closed.extend([one(WEvent::Opens(true)), pass()]);
    let w = run(&lab, &closed, "the Wrap to m22");
    assert_eq!(
        (w.log[1], w.wait_of[1]),
        (1, 0),
        "once m22 opens the newest epoch the pass signs"
    );
}

/// P3 binding (3), the direct drafts: m22's own publishes whose records write a name directly,
/// drafted in their turn, held, kept only while held, or let go, beside publishes whose writes
/// wait, follow the island's waiting step after every step of every sequence of [`DIRECTS`] and
/// [`HELD`] and of the island's direct-draft runs (`heldBeforeWrite`, `draftInTurn`,
/// `keepAfterUnhold`, `unreadHold`, `passBeforeKeep`), whose ends the island's lemmas name: the
/// later write wins, 12 over this Mac's own earlier 11.
///
/// NEGATIVE CONTROLS, each run in the test on the island's run for it, each parting from the step
/// at its own step and fields and breaking `OvertakenByAnother` on m22's files, where the pass
/// drops the later write as overtaken by this Mac's own earlier direct draft, as the island's
/// mutant runs say (log 11, write 1 overtaken by direct draft 1, drafted before it):
/// `bases-without-held` (the bases leave out what the held drafts leave) and `bases-log-only`
/// (the edit's bases are the log's value alone) on `heldBeforeWrite`, at the mutant's publish,
/// which m22 makes and the step refuses; `drafts-at-once` (a publish takes no turn) on
/// `draftInTurn`, at the mutant's publish, made in the direct draft's turn; `keeps-superseded`
/// (a keep whatever became of the hold) on `keepAfterUnhold`, at the keep; `unread-as-none` on
/// `unreadHold`, at the mutant's publish, drafted over a hold that does not read. And
/// `held-undeferred` (a pass defers only the names pending sets write) on `passBeforeKeep`, at the
/// first pass, which signs 12 while the direct draft of 11 is in flight; its keep then meets 12
/// as a concurrent write, and m22's log holds `n1` as a conflict of its own two edits. Which of
/// them the conflict shows is the records' order, so the test asserts the conflict, not a
/// winner; the island's mutant, whose log holds one value, keeps 11 over the signed 12
/// (`LaterKept`).
#[test]
fn direct_drafts_follow_p3s_step() {
    let lab = conforms(&DIRECTS, &[], "direct drafts from the start");
    let held = [one(WEvent::DirectDraft(1, 41)), one(WEvent::DirectHold(1))];
    conforms(&HELD, &held, "from a held direct draft");

    for (steps, name) in [
        (held_before_write(), "held_values_are_among_the_bases"),
        (draft_in_turn(), "a_publish_waits_for_the_drafts_turn"),
        (keep_after_unhold(), "a_gone_hold_keeps_nothing"),
        (unread_hold(), "an_unread_hold_refuses_the_draft"),
        (pass_before_keep(), "a_pass_waits_for_the_draft_in_flight"),
    ] {
        let w = run(&lab, &steps, name);
        assert_eq!((w.log[1], w.fate[1]), (12, 1), "{name}");
    }

    let mutant = |real: &WWorld, name: &str| {
        assert_eq!(
            (real.log[1], real.fate[1], real.over_d[1]),
            (11, 2, 1),
            "CONTROL: {name} loses the later edit to this Mac's own earlier 11"
        );
    };
    for (control, name) in [
        (&log_entries::BASES_WITHOUT_HELD, "bases-without-held"),
        (&log_entries::BASES_LOG_ONLY, "bases-log-only"),
    ] {
        let real = breaks(
            &lab,
            control,
            name,
            &held_before_write(),
            (2, &["pending"]),
            "OvertakenByAnother",
        );
        mutant(&real, name);
    }
    let real = breaks(
        &lab,
        &log_entries::DRAFTS_AT_ONCE,
        "drafts-at-once",
        &draft_in_turn(),
        (1, &["pending"]),
        "OvertakenByAnother",
    );
    mutant(&real, "drafts-at-once");
    let real = breaks(
        &lab,
        &log_entries::KEEPS_SUPERSEDED,
        "keeps-superseded",
        &keep_after_unhold(),
        (6, &["log"]),
        "OvertakenByAnother",
    );
    mutant(&real, "keeps-superseded");
    let real = breaks(
        &lab,
        &log_entries::UNREAD_AS_NONE,
        "unread-as-none, a hold",
        &unread_hold(),
        (3, &["pending"]),
        "OvertakenByAnother",
    );
    mutant(&real, "unread-as-none, a hold");

    diverges(
        &lab,
        &log_entries::HELD_UNDEFERRED,
        "held-undeferred",
        &pass_before_keep(),
        (5, &["waiting", "log"]),
    );
    let after = lab.observe().expect("m22 reads");
    assert_eq!(
        after.conflicts,
        BTreeSet::from([name(1)]),
        "CONTROL: held-undeferred leaves n1 a conflict of m22's own earlier 11 and later 12"
    );
}
