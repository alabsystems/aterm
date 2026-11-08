// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! P3's binding (3) for held drafts (`docs/design/one-mesh-proofs/P3-no-lost-edit.md`, "Binding";
//! `crates/aterm-mesh-core/src/no_lost_edit_island.rs` §2 to §7): the file-level functions a
//! publish's hold runs on ([`hold_drafted`], [`forget_held_draft`], [`keep_drafted_until`],
//! [`forget_landed_draft`], and [`adopt_held_draft`] with synthetic envelopes) driven through every
//! event sequence of a small universe of the island's held-draft World, and held to its step after
//! every event.
//!
//! [`World`] and [`step`] transliterate the island's `World` and `step` one for one, its folds
//! (`gateOf`, `settleReach`, `settleHold`, `forgets`) included, asking the compiled kernels
//! ([`COMPILED`], the binding island's `compiled`). The servers, the PUTs, their answers and the URL
//! that moves between servers are the environment the step names, played on the model's own fields;
//! where client.rs decides (a refusal of every server is [`nle::publish_forgets`] of the answers), the
//! driver asks that decision of the answers, as client.rs does. After each event the holds' files
//! (`records.drafts/*`) and the records store are projected onto the model's `hold` and `kept`
//! ([`Lab::project`]) and compared with what the step made of them, and the island's laws
//! (`NoLostEdit`, `HoldReach`, `Provenance`, `HoldPhase`) are asserted on the World whose holds and
//! kept records are the real ones ([`laws`]). An event whose guard the environment fails (a PUT of a
//! version not yet held, a landing before its records are kept while its hold stands) is skipped on
//! both sides; one whose guard is the code's own (a refusal that is no refusal, an adoption of a
//! version not held, a keep after its hold went) runs on the real functions, which must leave the
//! files as the step leaves the model. A keep's answer must be the step's (whether it keeps), and
//! a landing passes [`forget_landed_draft`] what its publish's keep answered, as client.rs does
//! (`World::answered`; the island's `kept` is set by an adoption too, after which a keep answers
//! false).
//!
//! The search ([`search`]) is breadth first over the states of both sides (the World and the
//! home's content, a [`Tree`]), so a divergence comes with the shortest sequence that reaches it.
//! The home's contents are kept by digest and each call of the real functions runs once on each
//! tree it is made on ([`Lab`]): the functions read nothing but the home's files and their
//! arguments, so one run stands for the same call on the same files after any other order of the
//! environment's events. The pending sets and the waiting writes are sealed with a fresh nonce on
//! each write, so a tree is named by what they hold, read through the lab's identity.
//!
//! Each of this Mac's versions also keeps one `[everyone]` write pending on it, as a publish
//! whose writes wait does ([`wait_for_key`] at its hold, [`forget_waiting`] at its refusal), and
//! a landing runs [`land_published`] after [`forget_landed_draft`], and a sync that reads a head
//! [`land_waiting_held`] before its adoption, as client.rs runs them. `land_published` drops a
//! pending set by the held drafts' gate and reach rule, which the waiting World takes as its
//! `dropped` premise (a set goes only for a version on no server for good); it is bound here,
//! through the held World's settle, rather than by a new event of the waiting World, because
//! the held World is the one with each version's sequence, spine, reach and servers that rule
//! reads. After each step the sets follow their holds ([`Lab::sets_follow_holds`]): each set's
//! version has its hold with the same servers, and a set goes only by landing or with its hold,
//! whose going the step and the island's laws judge.
//!
//! The unversioned hold (`records.drafted`, §13) has a search of its own
//! ([`the_unversioned_hold_follows_p3s_step`]): its settles, ingests and adoptions follow the
//! island's `lstep`, and `LegacyKept` and `GoesOnlyStored` hold of the home.

use super::*;
use crate::commons::binding::Request;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};

/// A version of the universe. Version 0 is the one every first draft opens; no event drafts it.
type Ver = usize;
/// A URL, or a server: URL `x` reaches server `x` until the world moves it.
type Place = usize;

/// Versions 0 to 3.
const VERSIONS: usize = 4;
/// URLs and servers 0 to 7.
const PLACES: usize = 8;
/// The URL the world moves between servers: one of the ALab vault's compiled-in names.
const MOVING: Place = 7;
/// The servers a republish or a move may name.
const SERVERS: [Place; 3] = [1, 2, MOVING];
/// The envelope digest of version 0.
const OPENED: [u8; 32] = [0xA0; 32];

/// The island's `Kernels`, the five a held draft asks.
#[derive(Clone, Copy)]
struct Kernels {
    stays: fn(bool, bool) -> bool,
    waits: fn(bool, bool) -> bool,
    refuses: fn(bool, bool) -> bool,
    drafted_keeps: fn(bool) -> bool,
    spine_settles: fn(bool, bool) -> bool,
}

/// The compiled kernels the code asks (the binding island's `compiled`).
const COMPILED: Kernels = Kernels {
    stays: nle::url_stays,
    waits: nle::hold_waits,
    refuses: nle::answer_refuses,
    drafted_keeps: nle::drafted_keeps,
    spine_settles: nle::spine_settles,
};

/// The island's `Reach`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Reach {
    Unknown,
    Known(Vec<Place>),
}

/// The island's held-draft `World`, its functions over the universe's versions and places.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct World {
    seq: [u64; VERSIONS],
    spine: [Vec<Ver>; VERSIONS],
    mine: [bool; VERSIONS],
    recs: [bool; VERSIONS],
    reach: [Vec<Place>; VERSIONS],
    phase: [u8; VERSIONS],
    head: [Ver; PLACES],
    srv: [Place; PLACES],
    via: [[Option<Place>; PLACES]; VERSIONS],
    inflight: [[bool; PLACES]; VERSIONS],
    res: [[Option<bool>; PLACES]; VERSIONS],
    hold: [Option<Reach>; VERSIONS],
    kept: [bool; VERSIONS],
    /// What each version's publish's keep answered ([`keep_drafted_until`]), which client.rs
    /// passes to [`forget_landed_draft`]: the driver's, not a field of the island's World, whose
    /// `kept` is also set by an adoption, after which a keep answers false.
    answered: [bool; VERSIONS],
}

/// The island's held-draft `Event`.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Event {
    Draft {
        v: Ver,
        o: Ver,
        sp: Vec<Ver>,
        mine: bool,
        recs: bool,
        reach: Vec<Place>,
    },
    Hold {
        v: Ver,
        unknown: bool,
    },
    Send {
        v: Ver,
        x: Place,
    },
    Arrive {
        v: Ver,
        x: Place,
        ok: bool,
    },
    RefusedAll {
        v: Ver,
    },
    Keep {
        v: Ver,
    },
    Landed {
        v: Ver,
        took: Vec<Place>,
    },
    Adopt {
        v: Ver,
        s: Place,
    },
    Republish {
        v: Ver,
        s: Place,
    },
    Move {
        x: Place,
        s: Place,
    },
}

fn elem(a: usize, list: &[usize]) -> bool {
    list.contains(&a)
}

/// The island's `gateOf`: 0 the head descends from the hold's version, 1 it proves nothing by
/// reach, 2 the reach decides.
fn gate_of(k: Kernels, named: bool, held_seq: u64, head_seq: u64, took_any: bool) -> u8 {
    if named {
        0
    } else if (k.waits)(head_seq < held_seq, took_any) {
        1
    } else {
        2
    }
}

/// The island's `settleReach`.
fn settle_reach(
    k: Kernels,
    one: &dyn Fn(Place) -> bool,
    took: &[Place],
    reach: &Reach,
) -> Option<Reach> {
    match reach {
        Reach::Unknown => Some(Reach::Unknown),
        Reach::Known(xs) => {
            let left: Vec<Place> = xs
                .iter()
                .copied()
                .filter(|&x| (k.stays)(one(x), elem(x, took)))
                .collect();
            (!left.is_empty()).then_some(Reach::Known(left))
        }
    }
}

/// The island's `settleHold`.
#[expect(
    clippy::too_many_arguments,
    reason = "the island's settleHold, argument for argument"
)]
fn settle_hold(
    k: Kernels,
    one: &dyn Fn(Place) -> bool,
    head_seq: u64,
    parents: &[Ver],
    took: &[Place],
    u: Ver,
    useq: u64,
    reach: &Reach,
) -> Option<Reach> {
    match gate_of(k, elem(u, parents), useq, head_seq, !took.is_empty()) {
        0 => None,
        1 => Some(reach.clone()),
        _ => settle_reach(k, one, took, reach),
    }
}

/// The island's `forgets`.
fn forgets(k: Kernels, answers: &[(bool, bool)]) -> bool {
    answers.iter().fold(true, |all, &(stored, refusal)| {
        (k.refuses)(stored, refusal) && all
    })
}

/// The island's `takesOver`: server.rs's compare-and-set, the environment the reach rule rests on.
fn takes_over(same: bool, newer: bool, named: bool, pin_ok: bool) -> bool {
    same || (newer && named && pin_ok)
}

impl World {
    /// The island's `w0`.
    fn initial() -> Self {
        Self {
            seq: [0; VERSIONS],
            spine: Default::default(),
            mine: [false; VERSIONS],
            recs: [false; VERSIONS],
            reach: Default::default(),
            phase: [0; VERSIONS],
            head: [0; PLACES],
            srv: std::array::from_fn(|x| x),
            via: [[None; PLACES]; VERSIONS],
            inflight: [[false; PLACES]; VERSIONS],
            res: [[None; PLACES]; VERSIONS],
            hold: Default::default(),
            kept: [false; VERSIONS],
            answered: [false; VERSIONS],
        }
    }

    /// The island's `accept`.
    fn accept(&self, s: Place, v: Ver, ok: bool) -> bool {
        let held = self.head[s];
        takes_over(
            held == v,
            self.seq[held] < self.seq[v],
            elem(held, &self.spine[v]),
            ok,
        )
    }

    /// The island's `answers`: per reach URL of `v`, stored and refused.
    fn answers(&self, v: Ver) -> Vec<(bool, bool)> {
        self.reach[v]
            .iter()
            .map(|&x| (self.res[v][x] == Some(true), self.res[v][x] == Some(false)))
            .collect()
    }

    /// The island's `settleAll` of `hold` (this World's holds with one taken out).
    fn settle_all(
        &self,
        k: Kernels,
        one: &dyn Fn(Place) -> bool,
        hold: &[Option<Reach>; VERSIONS],
        head_seq: u64,
        parents: &[Ver],
        took: &[Place],
    ) -> [Option<Reach>; VERSIONS] {
        std::array::from_fn(|u| {
            hold[u].as_ref().and_then(|reach| {
                settle_hold(k, one, head_seq, parents, took, u, self.seq[u], reach)
            })
        })
    }
}

/// The island's `step`, one event: `one` is names_one_server, `moves` which URLs the world moves.
fn step(
    k: Kernels,
    one: &dyn Fn(Place) -> bool,
    moves: &dyn Fn(Place) -> bool,
    w: &World,
    event: &Event,
) -> World {
    let mut n = w.clone();
    match event {
        Event::Draft {
            v,
            o,
            sp,
            mine,
            recs,
            reach,
        } => {
            let (v, o) = (*v, *o);
            if w.phase[v] == 0
                && v != 0
                && elem(o, sp)
                && sp.iter().all(|&p| w.seq[p] < w.seq[o] + 1)
            {
                n.seq[v] = w.seq[o] + 1;
                n.spine[v] = sp.clone();
                n.mine[v] = *mine;
                n.recs[v] = *recs;
                n.reach[v] = reach.clone();
                n.phase[v] = 1;
            }
        }
        Event::Hold { v, unknown } => {
            let v = *v;
            if w.phase[v] == 1 {
                n.phase[v] = 2;
                if w.mine[v] && w.recs[v] {
                    n.hold[v] = Some(if *unknown {
                        Reach::Unknown
                    } else {
                        Reach::Known(w.reach[v].clone())
                    });
                }
            }
        }
        Event::Send { v, x } => {
            let (v, x) = (*v, *x);
            if w.phase[v] == 2 && elem(x, &w.reach[v]) && w.via[v][x].is_none() {
                n.via[v][x] = Some(w.srv[x]);
                n.inflight[v][x] = true;
            }
        }
        Event::Arrive { v, x, ok } => {
            let (v, x) = (*v, *x);
            if let Some(s) = w.via[v][x]
                && w.inflight[v][x]
            {
                let accepted = w.accept(s, v, *ok);
                n.inflight[v][x] = false;
                n.res[v][x] = Some(accepted);
                if accepted {
                    n.head[s] = v;
                }
            }
        }
        Event::RefusedAll { v } => {
            let v = *v;
            if w.phase[v] == 2 && w.mine[v] && forgets(k, &w.answers(v)) {
                n.hold[v] = None;
                n.phase[v] = 3;
            }
        }
        Event::Keep { v } => {
            let v = *v;
            if w.phase[v] == 2
                && w.mine[v]
                && w.reach[v].iter().any(|&x| w.res[v][x] == Some(true))
                && (k.drafted_keeps)(w.hold[v].is_some())
            {
                n.kept[v] = true;
                n.answered[v] = true;
            }
        }
        Event::Landed { v, took } => {
            let v = *v;
            if w.phase[v] == 2
                && w.mine[v]
                && !took.is_empty()
                && took
                    .iter()
                    .all(|&x| elem(x, &w.reach[v]) && w.res[v][x] == Some(true))
                && (!w.recs[v] || w.kept[v] || w.hold[v].is_none())
            {
                n.phase[v] = 3;
                let mut hold = w.hold.clone();
                hold[v] = None;
                // The island's `landSpine`.
                let spine: &[Ver] = if (k.spine_settles)(w.recs[v], w.kept[v]) {
                    &w.spine[v]
                } else {
                    &[]
                };
                n.hold = w.settle_all(k, one, &hold, w.seq[v], spine, took);
            }
        }
        Event::Adopt { v, s } => {
            let (v, s) = (*v, *s);
            if w.mine[v] && w.head[s] == v && w.hold[v].is_some() && !w.kept[v] {
                n.kept[v] = true;
                let mut hold = w.hold.clone();
                hold[v] = None;
                n.hold = w.settle_all(k, one, &hold, w.seq[v], &w.spine[v], &[]);
            }
        }
        Event::Republish { v, s } => {
            let (v, s) = (*v, *s);
            if (!w.mine[v] || w.kept[v]) && w.phase[v] >= 2 && w.accept(s, v, true) {
                n.head[s] = v;
            }
        }
        Event::Move { x, s } => {
            if moves(*x) {
                n.srv[*x] = *s;
            }
        }
    }
    n
}

/// The island's `Past`: server `s` will never hold `v` again.
fn past(w: &World, s: Place, v: Ver) -> bool {
    w.seq[v] <= w.seq[w.head[s]] && w.head[s] != v
}

/// The island's `CanBring`.
fn can_bring(one: &dyn Fn(Place) -> bool, w: &World, v: Ver, x: Place) -> bool {
    (w.via[v][x].is_none() && w.phase[v] == 2 && !(one(x) && past(w, w.srv[x], v)))
        || w.via[v][x].is_some_and(|s| w.inflight[v][x] && !past(w, s, v))
}

/// The island's `Live`: `v` is on a server, or may yet be.
fn live(one: &dyn Fn(Place) -> bool, w: &World, v: Ver) -> bool {
    w.head.contains(&v) || w.reach[v].iter().any(|&x| can_bring(one, w, v, x))
}

/// The island's `Stored`.
fn stored(w: &World, h: Ver) -> bool {
    w.res[h].contains(&Some(true))
}

/// The island's `Covered`: `v`'s edit rides a stored version of this Mac that names it.
fn covered(w: &World, v: Ver) -> bool {
    (0..VERSIONS)
        .any(|h| elem(v, &w.spine[h]) && w.mine[h] && (w.kept[h] || !w.recs[h]) && stored(w, h))
}

/// The island's `NoLostEdit`, `HoldReach`, `Provenance` and `HoldPhase` of `w`.
fn laws(one: &dyn Fn(Place) -> bool, w: &World) -> Result<(), String> {
    for v in 1..VERSIONS {
        if w.phase[v] >= 2
            && w.mine[v]
            && w.recs[v]
            && !w.kept[v]
            && w.hold[v].is_none()
            && live(one, w, v)
            && !covered(w, v)
        {
            return Err(format!(
                "NoLostEdit: version {v} may be on a server, and its records are neither held nor \
                 kept nor carried by a stored descendant"
            ));
        }
        if let Some(Reach::Known(xs)) = &w.hold[v]
            && !w.kept[v]
        {
            for &x in &w.reach[v] {
                if !elem(x, xs) && !(one(x) && past(w, w.srv[x], v)) {
                    return Err(format!(
                        "HoldReach: version {v}'s hold lost URL {x}, which may still bring it"
                    ));
                }
            }
        }
        if w.hold[v].is_some() && w.phase[v] < 2 {
            return Err(format!("HoldPhase: version {v} is held before its hold"));
        }
    }
    for s in 0..PLACES {
        let v = w.head[s];
        if v != 0
            && w.mine[v]
            && !w.kept[v]
            && !w.reach[v]
                .iter()
                .any(|&x| w.via[v][x] == Some(s) && w.res[v][x] == Some(true))
        {
            return Err(format!(
                "Provenance: server {s} holds version {v} that no answer of its publish stored"
            ));
        }
    }
    Ok(())
}

/// The island's `lostAt`: server `s` serves `v`, whose records exist nowhere.
fn lost_at(w: &World, v: Ver, s: Place) -> bool {
    w.mine[v] && w.recs[v] && w.head[s] == v && !w.kept[v] && w.hold[v].is_none()
}

/// Version `v`'s envelope sequence: the model's, one up, since an envelope's sequence starts at 1
/// and the first names no parents (version 0 is sequence 1). The hold and the gate compare
/// sequences only, so the shift keeps every outcome.
fn real_seq(w: &World, v: Ver) -> u64 {
    w.seq[v] + 1
}

/// The URL `x` names: two that each name one server, and the ALab vault's compiled-in name.
fn url(x: Place) -> String {
    match x {
        MOVING => crate::ALAB_DEFAULT_URL.to_owned(),
        _ => format!("https://s{x}.example/v"),
    }
}

/// The URL `url` (a line of a hold's reach) is, as [`url`] wrote it.
fn place_of(line: &str) -> Result<Place, String> {
    [1, 2, MOVING]
        .into_iter()
        .find(|&x| url(x) == line)
        .ok_or_else(|| format!("a hold names {line:?}, no URL of the universe"))
}

/// The code's `one`: whether URL `x` names one server ([`names_one_server`]).
fn one(x: Place) -> bool {
    names_one_server(&url(x))
}

/// Which URLs the world moves between servers: the ALab name alone.
fn moves(x: Place) -> bool {
    x == MOVING
}

/// A founding member's seed, incarnation, id and entrant.
fn founder() -> ([u8; 32], [u8; 16], MemberId, Entrant) {
    let (seed, incarnation) = ([1; 32], [1; 16]);
    let identity = Identity::from_secret_bytes([65; 32]);
    let request =
        Request::create([17; 16], [34; 32], [33; 16], incarnation, &identity, &seed).unwrap();
    let actor = request.binding().actor();
    let entrant = Entrant {
        request,
        node_key: [129; 32],
        name: "m1".to_owned(),
        label: "m1".to_owned(),
    };
    (seed, incarnation, actor, entrant)
}

/// A home's content: each directory and file by its path under the home, with its mode, and each
/// file's bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Content {
    dirs: BTreeMap<PathBuf, u32>,
    files: BTreeMap<PathBuf, (u32, Vec<u8>)>,
}

impl Content {
    /// What is under `home` now.
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
            let mode = entry.metadata().unwrap().permissions().mode() & 0o7777;
            if entry.file_type().unwrap().is_dir() {
                self.dirs.insert(path.clone(), mode);
                self.read(&entry.path(), &path);
            } else {
                let bytes = std::fs::read(entry.path()).unwrap();
                self.files.insert(path, (mode, bytes));
            }
        }
    }

    /// The content's digest, which names its tree.
    fn digest_without(&self, sealed: impl Fn(&Path) -> bool) -> [u8; 32] {
        let mut bytes = Vec::new();
        for (path, mode) in &self.dirs {
            bytes.extend_from_slice(format!("dir {} {mode:o}\n", path.display()).as_bytes());
        }
        for (path, (mode, file)) in &self.files {
            if sealed(path) {
                let line = format!("sealed {} {mode:o}\n", path.display());
                bytes.extend_from_slice(line.as_bytes());
                continue;
            }
            let line = format!("file {} {mode:o} {}\n", path.display(), file.len());
            bytes.extend_from_slice(line.as_bytes());
            bytes.extend_from_slice(file);
        }
        Sha256::digest(&bytes)
    }

    /// Make `home`, which holds `self`, hold `to`: each file and directory `to` has not removed,
    /// each it has or has otherwise made.
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

/// A content of the home the lab has seen, by the order it first saw it.
type Tree = usize;

/// A call of the real functions an event makes, with every argument it passes them.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Call {
    Hold {
        v: Ver,
        seq: u64,
        version: [u8; 32],
        reach: Vec<String>,
    },
    Forget {
        seq: u64,
        version: [u8; 32],
    },
    Keep {
        v: Ver,
        seq: u64,
        version: [u8; 32],
        /// What the step's keep answers: whether it keeps the records.
        answers: bool,
    },
    Landed {
        seq: u64,
        version: [u8; 32],
        parents: Vec<[u8; 32]>,
        took: Vec<String>,
        drafted: bool,
        kept: bool,
    },
    Adopt {
        v: Ver,
        seq: u64,
        parents: Vec<[u8; 32]>,
    },
}

/// A hold's file: its sequence, its envelope digest, its reach's lines and its records' ids.
type HeldFile = (u64, [u8; 32], Vec<String>, Vec<RecordId>);

/// An envelope of the lab: its version, its sequence and its parents' digests.
type EnvelopeKey = (Ver, u64, Vec<[u8; 32]>);

/// What a tree's files say: each hold by its sequence and envelope digest, with its reach's lines
/// and its records' ids, and which versions' records the records store holds.
#[derive(Debug)]
struct Files {
    holds: Vec<HeldFile>,
    unversioned: bool,
    stored: [bool; VERSIONS],
    /// Each pending set ([`wait_for_key`]) by its version's digest: its servers and its names.
    pending: BTreeMap<[u8; 32], (Vec<String>, Vec<String>)>,
    /// The names that wait now: a set that landed ([`land_published`], [`land_waiting_held`])
    /// left its one write there.
    landed: std::collections::BTreeSet<String>,
}

/// The real side: a home whose records store holds a founded log, the one record each version
/// drafts, and each version's synthetic envelope, by its sequence and parents (its stamp names the
/// version's record, sealed to `identity`), whose digest names the version's hold.
///
/// The home's content is a [`Tree`], kept by its digest, and each call of the real functions is
/// run once on each tree it is made on: a second event that makes the same call on the same tree
/// (another order of the environment's events before it) gets the tree the first left. The
/// functions read nothing but the home's files and their arguments, so that run stands for every
/// later one; each tree's files are read once, when first seen.
struct Lab {
    /// The home's folder, removed with the lab.
    _root: aterm_tempfile::TempDir,
    work: Paths,
    identity: Identity,
    records: Vec<Record>,
    envelopes: RefCell<HashMap<EnvelopeKey, Envelope>>,
    digests: RefCell<HashMap<EnvelopeKey, [u8; 32]>>,
    trees: RefCell<HashMap<[u8; 32], Tree>>,
    contents: RefCell<Vec<Content>>,
    files: RefCell<Vec<Files>>,
    calls: RefCell<HashMap<(Tree, Call), Result<Tree, String>>>,
    /// The tree the home holds now.
    current: std::cell::Cell<Tree>,
    /// How many steps landed a pending set, and how many dropped one ([`Lab::sets_follow_holds`]).
    sets: std::cell::Cell<(usize, usize)>,
}

impl Lab {
    fn new() -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let (seed, incarnation, actor, entrant) = founder();
        let log = Log::new(Record::found(&[7; 32], vec![entrant], 1_000).unwrap()).unwrap();
        // One record per version, each a sibling over the founded log: kept or adopted in any order.
        let records = (0..VERSIONS)
            .map(|v| {
                let name = format!("version-{v}");
                log.clone()
                    .draft(&actor, 2_000 + v as u64, Body::Name { name })
                    .sign(&seed, incarnation)
                    .unwrap()
            })
            .collect();
        let lab = Self {
            work: Paths::for_home(&root.path().join("work")),
            _root: root,
            identity: Identity::from_secret_bytes([66; 32]),
            records,
            envelopes: RefCell::new(HashMap::new()),
            digests: RefCell::new(HashMap::new()),
            trees: RefCell::new(HashMap::new()),
            contents: RefCell::new(Vec::new()),
            files: RefCell::new(Vec::new()),
            calls: RefCell::new(HashMap::new()),
            current: std::cell::Cell::new(0),
            sets: std::cell::Cell::new((0, 0)),
        };
        fsx::ensure_private_dir(&lab.work.dir).unwrap();
        super::super::save_records(&lab.work, &encode_records(log.records())).unwrap();
        assert_eq!(
            lab.seen().unwrap(),
            START,
            "the founded home is the first tree"
        );
        lab
    }

    /// The home's tree now, read and kept the first time the lab sees it.
    fn seen(&self) -> Result<Tree, String> {
        let content = Content::of(&self.work.dir);
        // The pending sets and the waiting writes are sealed with a fresh nonce each write: the
        // tree is what they hold, read through the lab's identity, not their bytes.
        let mut text = content
            .digest_without(|path| {
                path.starts_with("everyone.pending") || path == Path::new("everyone.waiting")
            })
            .to_vec();
        text.extend_from_slice(
            format!(
                "{:?}",
                (
                    waiting_shapes(&self.work, &self.identity),
                    pending_reach(&self.work, &self.identity)
                )
            )
            .as_bytes(),
        );
        let digest = Sha256::digest(&text);
        if let Some(&tree) = self.trees.borrow().get(&digest) {
            return Ok(tree);
        }
        let files = self.read_files()?;
        let tree = self.files.borrow().len();
        self.contents.borrow_mut().push(content);
        self.files.borrow_mut().push(files);
        self.trees.borrow_mut().insert(digest, tree);
        Ok(tree)
    }

    /// The holds' files and the records store of the home now.
    fn read_files(&self) -> Result<Files, String> {
        let mut holds = Vec::new();
        for (seq, digest, path) in held_drafts(&self.work) {
            let held = read_held(&path)
                .map_err(|e| format!("read_held: {e}"))?
                .ok_or("a hold listed and gone")?;
            let records = crate::membership_log::decode_records(held.records())
                .map_err(|e| format!("a hold's records: {e}"))?;
            let ids = records.iter().map(|record| *record.id()).collect();
            holds.push((seq, digest, held.reach.clone(), ids));
        }
        let store = super::super::load_records(&self.work).map_err(|e| format!("store: {e}"))?;
        let (waiting, sets) = waiting_shapes(&self.work, &self.identity);
        let reach: BTreeMap<[u8; 32], Vec<String>> = pending_reach(&self.work, &self.identity)
            .into_iter()
            .collect();
        Ok(Files {
            holds,
            unversioned: unversioned_held_draft_file(&self.work).exists(),
            stored: std::array::from_fn(|v| store.contains(self.records[v].id())),
            pending: sets
                .into_iter()
                .map(|(version, set)| {
                    let servers = reach.get(&version).cloned().unwrap_or_default();
                    (version, (servers, set.into_keys().collect()))
                })
                .collect(),
            landed: waiting.into_keys().collect(),
        })
    }

    /// `call` on `tree`: run once on the real functions, the tree it leaves seen.
    fn call(&self, tree: Tree, call: &Call) -> Result<Tree, String> {
        if let Some(left) = self.calls.borrow().get(&(tree, call.clone())) {
            return left.clone();
        }
        if self.current.get() != tree {
            let contents = self.contents.borrow();
            match contents.get(self.current.get()) {
                // The home as it is: a tree stands for every home whose sealed files hold what
                // its do, whatever their bytes.
                Some(_) => Content::of(&self.work.dir).turn_into(&contents[tree], &self.work.dir),
                None => {
                    std::fs::remove_dir_all(&self.work.dir).unwrap();
                    fsx::ensure_private_dir(&self.work.dir).unwrap();
                    Content::default().turn_into(&contents[tree], &self.work.dir);
                }
            }
        }
        let left = self.run(call).and_then(|()| self.seen());
        self.current.set(*left.as_ref().unwrap_or(&usize::MAX));
        self.calls
            .borrow_mut()
            .insert((tree, call.clone()), left.clone());
        left
    }

    /// Run `call` on the home's files.
    fn run(&self, call: &Call) -> Result<(), String> {
        let paths = &self.work;
        match call {
            Call::Hold {
                v,
                seq,
                version,
                reach,
            } => {
                hold_drafted(paths, &self.drafted(*v), *seq, version, reach)
                    .map_err(|e| format!("hold_drafted: {e}"))?;
                // The publish's `[everyone]` write, pending on its version with its servers.
                let name = format!("v{v}");
                let write = Waiting::of_writes(&[(name.as_str(), Some(b"x"), &[None])]);
                wait_for_key(
                    paths,
                    &self.identity,
                    &write,
                    *seq,
                    version,
                    reach,
                    &[],
                    None,
                )
                .map_err(|e| format!("wait_for_key: {e}"))
            }
            Call::Forget { seq, version } => {
                forget_held_draft(paths, *seq, version, None);
                forget_waiting(paths, version);
                Ok(())
            }
            Call::Keep {
                v,
                seq,
                version,
                answers,
            } => {
                let kept =
                    keep_drafted_until(paths, &self.drafted(*v), (*seq, version), &[], None, None)
                        .map_err(|e| format!("keep_drafted_until: {e}"))?;
                if kept == *answers {
                    Ok(())
                } else {
                    Err(format!(
                        "keep_drafted_until answered kept {kept}, the step's keep {answers}"
                    ))
                }
            }
            Call::Landed {
                seq,
                version,
                parents,
                took,
                drafted,
                kept,
            } => {
                forget_landed_draft(paths, *seq, version, parents, took, *drafted, *kept, None);
                land_published(paths, &self.identity, *seq, version, parents, took, None);
                Ok(())
            }
            Call::Adopt { v, seq, parents } => {
                let envelope = self.envelope_of(*v, *seq, parents);
                // A sync that reads a head lands the sets of its version and spine first.
                land_waiting_held(paths, &self.identity, &stored_versions(&envelope))
                    .map_err(|e| format!("land_waiting_held: {e}"))?;
                adopt_held_draft(paths, &envelope, &self.identity)
                    .map(|_| ())
                    .map_err(|e| format!("adopt_held_draft: {e}"))
            }
        }
    }

    /// Version `v`'s envelope of sequence `seq` over `parents`, with a locker for `identity`
    /// whose stamp names `v`'s record.
    fn envelope_of(&self, v: Ver, seq: u64, parents: &[[u8; 32]]) -> Envelope {
        let key = (v, seq, parents.to_vec());
        if let Some(envelope) = self.envelopes.borrow().get(&key) {
            return envelope.clone();
        }
        let stamp = stamp_line(&[*self.records[v].id()]).unwrap();
        let text = stamped_text(b"[everyone]\n", &stamp);
        let sealed = crate::age::encrypt(&[self.identity.recipient()], text.as_slice()).unwrap();
        let bytes = envelope::sign(
            &envelope::Unsigned {
                name: "alab".to_owned(),
                seq,
                time: 1,
                handoff: Vec::new(),
                parents: parents.to_vec(),
                lockers: vec![envelope::Locker {
                    label: "1".to_owned(),
                    armored: crate::age::armor(&sealed),
                }],
            },
            &[0x42; 32],
        )
        .unwrap();
        let envelope = envelope::parse(&bytes).unwrap();
        self.envelopes.borrow_mut().insert(key, envelope.clone());
        envelope
    }

    /// The digest of version `v`'s envelope of sequence `seq` over `parents`.
    fn version_digest(&self, v: Ver, seq: u64, parents: &[[u8; 32]]) -> [u8; 32] {
        let key = (v, seq, parents.to_vec());
        if let Some(&digest) = self.digests.borrow().get(&key) {
            return digest;
        }
        let digest = self.envelope_of(v, seq, parents).digest();
        self.digests.borrow_mut().insert(key, digest);
        digest
    }

    /// Each version's envelope digest in `w`, which names its hold: its sequence, and its spine's
    /// digests as its parents (a spine names earlier versions only). Version 0's is [`OPENED`];
    /// a version not yet drafted has none, and its row is one no envelope has.
    fn digests(&self, w: &World) -> [[u8; 32]; VERSIONS] {
        let mut digests = [[0; 32]; VERSIONS];
        digests[0] = OPENED;
        for v in 1..VERSIONS {
            if w.phase[v] >= 1 {
                let parents: Vec<[u8; 32]> = w.spine[v].iter().map(|&p| digests[p]).collect();
                digests[v] = self.version_digest(v, real_seq(w, v), &parents);
            }
        }
        digests
    }

    /// What `v` drafted: its one record.
    fn drafted(&self, v: Ver) -> Drafted {
        Drafted {
            records: vec![self.records[v].clone()],
            heads: vec![*self.records[v].id()],
            network: None,
        }
    }

    /// The call `event` makes of the real functions from the state `w` stands for, as the publish
    /// and the sync that drive them would, where the environment lets it happen.
    fn call_of(&self, w: &World, digests: &[[u8; 32]; VERSIONS], event: &Event) -> Option<Call> {
        match event {
            Event::Hold { v, unknown } => {
                let v = *v;
                (w.phase[v] == 1 && w.mine[v] && w.recs[v]).then(|| {
                    let mut reach: Vec<String> = w.reach[v].iter().map(|&x| url(x)).collect();
                    if *unknown {
                        // A server the publish could not write as a line of its own.
                        reach.push(String::new());
                    }
                    Call::Hold {
                        v,
                        seq: real_seq(w, v),
                        version: digests[v],
                        reach,
                    }
                })
            }
            // client.rs's classification of the publish's answers.
            Event::RefusedAll { v } => (w.phase[*v] == 2
                && w.mine[*v]
                && nle::publish_forgets(w.answers(*v)))
            .then(|| Call::Forget {
                seq: real_seq(w, *v),
                version: digests[*v],
            }),
            // The publish keeps once a server took its version, its hold there or gone: the
            // keep's gate is the code's own (`keep_drafted_until`).
            Event::Keep { v } => (w.phase[*v] == 2
                && w.mine[*v]
                && w.recs[*v]
                && w.reach[*v].iter().any(|&x| w.res[*v][x] == Some(true)))
            .then(|| Call::Keep {
                v: *v,
                seq: real_seq(w, *v),
                version: digests[*v],
                answers: (COMPILED.drafted_keeps)(w.hold[*v].is_some()),
            }),
            Event::Landed { v, took } => {
                let v = *v;
                (w.phase[v] == 2
                    && w.mine[v]
                    && !took.is_empty()
                    && took
                        .iter()
                        .all(|&x| elem(x, &w.reach[v]) && w.res[v][x] == Some(true))
                    && (!w.recs[v] || w.kept[v] || w.hold[v].is_none()))
                .then(|| Call::Landed {
                    seq: real_seq(w, v),
                    version: digests[v],
                    parents: w.spine[v].iter().map(|&p| digests[p]).collect(),
                    took: took.iter().map(|&x| url(x)).collect(),
                    drafted: w.recs[v],
                    // What the publish's keep answered (`answered`, held to the step's keep at
                    // each `Keep`), as client.rs passes it; one that drafted nothing passes
                    // true. It differs from the step's `kept` after an adoption, which kept the
                    // records where the keep did not: the step settles by the spine there, the
                    // code by reach alone, and the files show whether the two part.
                    kept: !w.recs[v] || w.answered[v],
                })
            }
            // The sync reads `v` back from server `s`.
            Event::Adopt { v, s } => (w.mine[*v] && w.head[*s] == *v).then(|| Call::Adopt {
                v: *v,
                seq: real_seq(w, *v),
                parents: w.spine[*v].iter().map(|&p| digests[p]).collect(),
            }),
            Event::Draft { .. }
            | Event::Send { .. }
            | Event::Arrive { .. }
            | Event::Republish { .. }
            | Event::Move { .. } => None,
        }
    }

    /// `tree`'s holds and records store, as the model's `hold` and `kept` of `w`'s versions.
    fn project(
        &self,
        w: &World,
        digests: &[[u8; 32]; VERSIONS],
        tree: Tree,
    ) -> Result<([Option<Reach>; VERSIONS], [bool; VERSIONS]), String> {
        let files = self.files.borrow();
        let files = &files[tree];
        if files.unversioned {
            return Err("the Mac's one unversioned hold, of no version".to_owned());
        }
        let mut hold: [Option<Reach>; VERSIONS] = Default::default();
        for (seq, digest, lines, ids) in &files.holds {
            let v = (1..VERSIONS)
                .find(|&v| w.phase[v] >= 1 && real_seq(w, v) == *seq && digests[v] == *digest)
                .ok_or_else(|| format!("a hold of no version of the universe (seq {seq})"))?;
            if *ids != [*self.records[v].id()] {
                return Err(format!(
                    "version {v}'s hold holds another version's records"
                ));
            }
            let reach = if *lines == [REACH_UNKNOWN] {
                Reach::Unknown
            } else {
                Reach::Known(
                    lines
                        .iter()
                        .map(|line| place_of(line))
                        .collect::<Result<_, _>>()?,
                )
            };
            if hold[v].replace(reach).is_some() {
                return Err(format!("version {v} is held twice"));
            }
        }
        // A version that drafted no records keeps none: its `kept` is the model's alone.
        let kept = std::array::from_fn(|v| {
            if w.recs[v] {
                files.stored[v]
            } else {
                w.kept[v]
            }
        });
        Ok((hold, kept))
    }

    /// `event` from `w` on `tree`, on both sides: the step's World and the tree the real
    /// functions left, after the tree's files were compared with the World and the laws held of
    /// the World whose holds and kept records are the files'.
    fn check(&self, w: &World, tree: Tree, event: &Event) -> Result<(World, Tree), String> {
        let before = tree;
        let next = step(COMPILED, &one, &moves, w, event);
        let tree = match self.call_of(w, &self.digests(w), event) {
            Some(call) => self.call(tree, &call)?,
            None => tree,
        };
        let real = self.real(&next, tree)?;
        if real.hold != next.hold || real.kept != next.kept {
            return Err(format!(
                "the files diverge from the step: real hold {:?} kept {:?}, model hold {:?} kept \
                 {:?}",
                real.hold, real.kept, next.hold, next.kept
            ));
        }
        laws(&one, &real)?;
        self.sets_follow_holds(before, tree)?;
        Ok((next, tree))
    }

    /// The pending sets follow the holds of their versions from tree `before` to tree `after`
    /// ([`land_published`] settles them by the holds' reach rule): each set's version has its
    /// hold, with the same servers, and a set goes only where it landed or its hold went too, so
    /// a set is dropped only for a version the held World's settle let go (one-mesh W2 and the
    /// waiting World's `dropped` premise).
    fn sets_follow_holds(&self, before: Tree, after: Tree) -> Result<(), String> {
        let files = self.files.borrow();
        let (was, now) = (&files[before], &files[after]);
        let hold = |f: &Files, version: &[u8; 32]| {
            f.holds
                .iter()
                .find(|held| held.1 == *version)
                .map(|held| held.2.clone())
        };
        for (version, (reach, _)) in &now.pending {
            if hold(now, version).as_ref() != Some(reach) {
                return Err(format!(
                    "a pending set of servers {reach:?} beside its version's hold {:?}",
                    hold(now, version)
                ));
            }
        }
        for (version, (_, names)) in &was.pending {
            let landed = names.iter().all(|name| now.landed.contains(name));
            if !now.pending.contains_key(version) {
                let (lands, drops) = self.sets.get();
                self.sets.set(if landed {
                    (lands + 1, drops)
                } else {
                    (lands, drops + 1)
                });
            }
            if !now.pending.contains_key(version) && !landed && hold(now, version).is_some() {
                return Err(format!(
                    "a pending set of {} dropped while its hold stays",
                    aterm_codec::hex::encode_lower(version)
                ));
            }
        }
        Ok(())
    }

    /// The World `w` with the holds and kept records of `tree`'s files.
    fn real(&self, w: &World, tree: Tree) -> Result<World, String> {
        let (hold, kept) = self.project(w, &self.digests(w), tree)?;
        Ok(World {
            hold,
            kept,
            ..w.clone()
        })
    }

    /// `run` from the start on both sides, the World after each event compared and its laws held;
    /// the real World at the end (its holds and kept records the files').
    fn replay(&self, run: &[Event]) -> Result<World, String> {
        let (mut w, mut tree) = (World::initial(), START);
        for (at, event) in run.iter().enumerate() {
            (w, tree) = self
                .check(&w, tree, event)
                .map_err(|why| format!("after {:?}: {why}", &run[..=at]))?;
        }
        self.real(&w, tree)
    }

    /// [`Lab::replay`] with the laws and the comparison off: the real World at the end, the step's
    /// environment beside the files' holds and kept records, for a control that breaks them.
    fn replay_real(&self, run: &[Event]) -> World {
        let (mut w, mut tree) = (World::initial(), START);
        for event in run {
            let next = step(COMPILED, &one, &moves, &w, event);
            if let Some(call) = self.call_of(&w, &self.digests(&w), event) {
                tree = self.call(tree, &call).unwrap();
            }
            w = next;
        }
        self.real(&w, tree).unwrap()
    }
}

/// The founded home's tree.
const START: Tree = 0;

/// The bounds of a search.
struct Universe {
    /// Versions 1 to this are drafted, in order.
    versions: usize,
    /// The reaches a draft may name.
    reaches: &'static [&'static [Place]],
    /// The last version may be another Mac's (no records, never held).
    foreign: bool,
    /// A hold may name its reach unknown.
    unknown: bool,
    /// A server may refuse a PUT on its pin (`arrive` with `ok` false).
    pin_refusals: bool,
    /// The most steps a sequence takes (a step is one event, or the two of a draft and its hold,
    /// or of a PUT's send and arrival).
    depth: usize,
}

/// What a search covered.
#[derive(Debug, Default)]
struct Covered {
    states: usize,
    steps: usize,
    events: usize,
    trees: usize,
    calls: usize,
    deepest: usize,
}

/// The steps the universe takes from `w`. A draft and its hold are one step, and so are a PUT's
/// send and its arrival: nothing between a draft and its hold reads either, and a PUT in flight is
/// read by no event but its own arrival (its server fixed at its send), so these orders lose no
/// outcome of the others.
fn steps(w: &World, u: &Universe) -> Vec<Vec<Event>> {
    let mut out = Vec::new();
    if let Some(v) = (1..=u.versions).find(|&v| w.phase[v] == 0) {
        let opened = std::iter::once(0).chain((1..v).filter(|&o| w.phase[o] >= 2));
        for o in opened {
            let mut sp = vec![o];
            sp.extend(w.spine[o].iter().copied().filter(|&p| p != o));
            let mut kinds = vec![(true, true)];
            if u.foreign && v == u.versions {
                kinds.push((false, false));
            }
            for &(mine, recs) in &kinds {
                for reach in u.reaches {
                    for unknown in [false, true] {
                        if unknown && !(u.unknown && mine) {
                            continue;
                        }
                        out.push(vec![
                            Event::Draft {
                                v,
                                o,
                                sp: sp.clone(),
                                mine,
                                recs,
                                reach: reach.to_vec(),
                            },
                            Event::Hold { v, unknown },
                        ]);
                    }
                }
            }
        }
    }
    for v in 1..=u.versions {
        if w.phase[v] == 2 {
            for &x in &w.reach[v] {
                if w.via[v][x].is_none() {
                    for ok in [true, false] {
                        if ok || u.pin_refusals {
                            out.push(vec![Event::Send { v, x }, Event::Arrive { v, x, ok }]);
                        }
                    }
                }
            }
        }
        if !w.mine[v] {
            continue;
        }
        if w.phase[v] == 2 && w.reach[v].iter().all(|&x| w.res[v][x].is_some()) {
            out.push(vec![Event::RefusedAll { v }]);
        }
        let took: Vec<Place> = w.reach[v]
            .iter()
            .copied()
            .filter(|&x| w.res[v][x] == Some(true))
            .collect();
        // A keep runs once a server took the version, until it answered that it keeps: after an
        // adoption, too, where it answers false.
        if w.phase[v] == 2 && w.recs[v] && !w.answered[v] && !took.is_empty() {
            out.push(vec![Event::Keep { v }]);
        }
        if w.phase[v] == 2 && (!w.recs[v] || w.kept[v] || w.hold[v].is_none()) {
            for mask in 1..(1usize << took.len()) {
                let tk = took
                    .iter()
                    .enumerate()
                    .filter(|&(at, _)| mask & (1 << at) != 0)
                    .map(|(_, &x)| x)
                    .collect();
                out.push(vec![Event::Landed { v, took: tk }]);
            }
        }
        for s in SERVERS {
            if w.phase[v] >= 2 && w.head[s] == v {
                out.push(vec![Event::Adopt { v, s }]);
            }
        }
    }
    for v in 1..=u.versions {
        for s in SERVERS {
            if (!w.mine[v] || w.kept[v])
                && w.phase[v] >= 2
                && w.head[s] != v
                && w.accept(s, v, true)
            {
                out.push(vec![Event::Republish { v, s }]);
            }
        }
    }
    if u.reaches.iter().any(|reach| reach.contains(&MOVING)) {
        for s in SERVERS {
            if w.srv[MOVING] != s {
                out.push(vec![Event::Move { x: MOVING, s }]);
            }
        }
    }
    out
}

/// Every sequence of `u` from the start, breadth first, each step on both sides ([`Lab::check`])
/// and each state of both sides (the World and the home's tree) explored once, at the fewest
/// steps that reach it; the first divergence, with the shortest sequence that reaches it.
fn search(lab: &Lab, u: &Universe) -> Result<Covered, String> {
    let mut covered = Covered::default();
    let start = (World::initial(), START);
    // Each state's index, and the state before it with the events between them.
    let mut seen: HashMap<(World, Tree), usize> = HashMap::new();
    let mut before: Vec<Option<(usize, Vec<Event>)>> = vec![None];
    seen.insert(start.clone(), 0);
    let mut queue = std::collections::VecDeque::from([(start, 0, 0)]);
    let path_to = |before: &[Option<(usize, Vec<Event>)>], mut at: usize| {
        let mut steps = Vec::new();
        while let Some((prior, events)) = &before[at] {
            steps.push(events.clone());
            at = *prior;
        }
        steps.into_iter().rev().flatten().collect::<Vec<Event>>()
    };
    while let Some(((w, tree), at, depth)) = queue.pop_front() {
        covered.deepest = covered.deepest.max(depth);
        if depth == u.depth {
            continue;
        }
        for events in steps(&w, u) {
            let mut next = (w.clone(), tree);
            for (done, event) in events.iter().enumerate() {
                next = lab.check(&next.0, next.1, event).map_err(|why| {
                    let mut path = path_to(&before, at);
                    path.extend_from_slice(&events[..=done]);
                    format!("after {path:?}: {why}")
                })?;
                covered.events += 1;
            }
            covered.steps += 1;
            if !seen.contains_key(&next) {
                let index = before.len();
                before.push(Some((at, events)));
                seen.insert(next.clone(), index);
                queue.push_back((next, index, depth + 1));
            }
        }
    }
    covered.states = seen.len();
    covered.trees = lab.files.borrow().len();
    covered.calls = lab.calls.borrow().len();
    Ok(covered)
}

/// The island's runs (§7) in its events: `lostReply`, `unreached`, `moving`, `crashAfterPut` and
/// `cappedSpine`.
fn draft(v: Ver, reach: &[Place]) -> Event {
    Event::Draft {
        v,
        o: 0,
        sp: vec![0],
        mine: true,
        recs: true,
        reach: reach.to_vec(),
    }
}

fn put(v: Ver, x: Place) -> [Event; 2] {
    [Event::Send { v, x }, Event::Arrive { v, x, ok: true }]
}

fn held(v: Ver) -> Event {
    Event::Hold { v, unknown: false }
}

fn lost_reply() -> Vec<Event> {
    let mut run = vec![draft(1, &[1]), held(1)];
    run.extend(put(1, 1));
    run.extend([draft(2, &[1]), held(2)]);
    run.extend(put(2, 1));
    run.push(Event::RefusedAll { v: 2 });
    run
}

fn unreached() -> Vec<Event> {
    let mut run = vec![draft(1, &[1, 2]), held(1)];
    run.extend(put(1, 2));
    run.extend([draft(2, &[1, 2]), held(2)]);
    run.extend(put(2, 1));
    run.extend(put(2, 2));
    run.extend([
        Event::Keep { v: 2 },
        Event::Landed {
            v: 2,
            took: vec![1],
        },
    ]);
    run
}

fn moving() -> Vec<Event> {
    let mut run = vec![draft(1, &[MOVING]), held(1)];
    run.extend(put(1, MOVING));
    run.extend([
        Event::Move { x: MOVING, s: 2 },
        draft(2, &[MOVING]),
        held(2),
    ]);
    run.extend(put(2, MOVING));
    run.extend([
        Event::Keep { v: 2 },
        Event::Landed {
            v: 2,
            took: vec![MOVING],
        },
    ]);
    run
}

fn crash_after_put() -> Vec<Event> {
    let mut run = vec![draft(1, &[1])];
    run.extend(put(1, 1));
    run
}

/// v3, drafted from v2 with v1 left out of its spine (a spine capped at
/// [`MAX_PARENTS`](crate::envelope::MAX_PARENTS)), lands on server 2 first and settles v2's hold
/// by descent; v2 then lands there with its records not kept.
fn capped_spine() -> Vec<Event> {
    let from = |v: Ver, sp: Vec<Ver>| Event::Draft {
        v,
        o: sp[0],
        sp,
        mine: true,
        recs: true,
        reach: vec![2],
    };
    let mut run = vec![draft(1, &[1]), held(1)];
    run.extend(put(1, 1));
    run.extend([from(2, vec![1, 0]), held(2)]);
    run.extend(put(2, 2));
    run.extend([from(3, vec![2, 0]), held(3)]);
    run.extend(put(3, 2));
    run.extend([
        Event::Keep { v: 3 },
        Event::Landed {
            v: 3,
            took: vec![2],
        },
        Event::Landed {
            v: 2,
            took: vec![2],
        },
    ]);
    run
}

/// Two versions (the second may be another Mac's), the second drafted from version 0 or from the
/// first once it went out, each reaching server 1, servers 1 and 2, or the ALab name, with its
/// reach known or unknown, PUTs taken or refused on their pin, the ALab name moved between the
/// three servers: every state it reaches, the search stopping when none is left.
const TWO_VERSIONS: Universe = Universe {
    versions: 2,
    reaches: &[&[1], &[1, 2], &[MOVING]],
    foreign: true,
    unknown: true,
    pin_refusals: true,
    depth: usize::MAX,
};

/// Three versions (the third may be another Mac's), each drafted from version 0 or from one
/// before it that went out (so a spine of three), each reaching server 1 or servers 1 and 2, with
/// its reach known or unknown, PUTs taken or refused on their pin, to six steps.
const THREE_VERSIONS: Universe = Universe {
    versions: 3,
    reaches: &[&[1], &[1, 2]],
    foreign: true,
    unknown: true,
    pin_refusals: true,
    depth: 6,
};

/// The search of `u`, its numbers printed; the first divergence fails the test.
fn conforms(u: &Universe, name: &str) -> Covered {
    let started = Instant::now();
    let lab = Lab::new();
    let covered = search(&lab, u).unwrap_or_else(|why| panic!("{why}"));
    let (lands, drops) = lab.sets.get();
    eprintln!(
        "P3 held-draft conformance, {name}: {} states, {} steps, {} events, {} real calls on {} \
         trees, deepest {}, in {:?}; {lands} steps landed a pending set and {drops} dropped one",
        covered.states,
        covered.steps,
        covered.events,
        covered.calls,
        covered.trees,
        covered.deepest,
        started.elapsed()
    );
    assert!(
        lands > 0 && drops > 0,
        "the search lands pending sets and drops them by the reach rule"
    );
    covered
}

/// NEGATIVE CONTROL [`SINGLE_SLOT_HOLD`] (`single-slot-hold`, one hold for the Mac, as before
/// W2), in a lab of its own so that every call runs on the real functions again: the search of
/// `u` diverges from the step, and the island's `lostReply` run ends with server 1 serving
/// version 1, whose records exist nowhere (`lostAt`, as the island's `single_slot_loses_v1` says
/// of `stepSingleSlot`), `NoLostEdit` false of the real files.
fn single_slot_hold_diverges(u: &Universe, name: &str) {
    let lab = Lab::new();
    SINGLE_SLOT_HOLD.with(|cell| cell.set(true));
    let diverged = search(&lab, u);
    let w = lab.replay_real(&lost_reply());
    SINGLE_SLOT_HOLD.with(|cell| cell.set(false));
    let why = diverged.expect_err("CONTROL: one hold for the Mac diverges from the step");
    eprintln!("P3 held-draft conformance, {name}, single-slot-hold: {why}");
    assert!(
        lost_at(&w, 1, 1),
        "CONTROL: with one hold, server 1 serves v1 and its records exist nowhere"
    );
    let law = laws(&one, &w).expect_err("CONTROL: with one hold, a law of P3 is false");
    assert!(law.starts_with("NoLostEdit"), "CONTROL: {law}");
}

/// NEGATIVE CONTROL [`KEEPS_SUPERSEDED`] (`keeps-superseded`, a keep whatever became of its hold,
/// as before P3's keep gate), in a lab of its own: the search of `u` diverges from the step at a
/// keep of version 1 after its hold went, which answers that it keeps where the step's keep does
/// not (the shortest such run: after its adoption; after a descent the keep would also put its
/// values back over the descending head's).
fn keeps_superseded_diverges(u: &Universe, name: &str) {
    let lab = Lab::new();
    KEEPS_SUPERSEDED.with(|cell| cell.set(true));
    let diverged = search(&lab, u);
    KEEPS_SUPERSEDED.with(|cell| cell.set(false));
    let why = diverged.expect_err("CONTROL: a keep after descent diverges from the step");
    eprintln!("P3 held-draft conformance, {name}, keeps-superseded: {why}");
    assert!(
        why.contains("Keep { v: 1 }]"),
        "CONTROL: the divergence is a keep of a superseded hold: {why}"
    );
}

/// NEGATIVE CONTROL [`SPINE_ALWAYS`] (`spine-always`, a landed version settles by its spine
/// whatever became of its records, as before P3's gate), in a lab of its own: the island's
/// `cappedSpine` run diverges from the step at v2's landing, and ends with server 1 serving v1,
/// whose records exist nowhere (`lostAt`, as the island's `spine_without_kept_records_loses_v1`
/// says of `spineAlways`), `NoLostEdit` false of the real files. The searches draft every spine
/// whole, so a hold their landings settle by spine is settled by the descending head's spine
/// too, and none tells the gate's arms apart: this run does.
fn spine_always_diverges() {
    let lab = Lab::new();
    SPINE_ALWAYS.with(|cell| cell.set(true));
    let diverged = lab.replay(&capped_spine());
    let w = lab.replay_real(&capped_spine());
    SPINE_ALWAYS.with(|cell| cell.set(false));
    let why = diverged.expect_err("CONTROL: a spine read through records not kept diverges");
    eprintln!("P3 held-draft conformance, cappedSpine, spine-always: {why}");
    assert!(
        why.contains("Landed { v: 2, took: [2] }]"),
        "CONTROL: the divergence is v2's landing: {why}"
    );
    assert!(
        lost_at(&w, 1, 1),
        "CONTROL: settled by v2's spine, server 1 serves v1 and its records exist nowhere"
    );
    let law = laws(&one, &w).expect_err("CONTROL: settled by v2's spine, a law of P3 is false");
    assert!(law.starts_with("NoLostEdit"), "CONTROL: {law}");
}

/// P3 binding (3), held drafts: the real holds and records store follow the island's held-draft
/// step after every event of every sequence of [`TWO_VERSIONS`], every state it reaches, and of the
/// island's runs (`lostReply`, `unreached`, `moving`, `crashAfterPut`, `cappedSpine`, whose ends
/// the island's lemmas name), and the island's laws hold of them. The ALab name moves between servers
/// (`moves`), and the code's names_one_server never calls it one server, so the island's premise
/// `OneIsFixed` holds of the universe.
///
/// NEGATIVE CONTROLS, run in the test: [`single_slot_hold_diverges`],
/// [`keeps_superseded_diverges`], [`spine_always_diverges`].
#[test]
fn held_drafts_follow_p3s_step_through_every_state_of_two_versions() {
    let lab = Lab::new();
    let w = lab.replay(&lost_reply()).expect("lostReply conforms");
    assert!(!lost_at(&w, 1, 1), "per_version_hold_keeps_v1");
    assert_eq!(w.head[1], 1, "v1_is_on_the_server");
    let w = lab.replay(&unreached()).expect("unreached conforms");
    assert!(!lost_at(&w, 1, 2), "reach_rule_keeps_v1");
    let w = lab.replay(&moving()).expect("moving conforms");
    assert!(!lost_at(&w, 1, MOVING), "alab_name_settles_nothing");
    let w = lab
        .replay(&crash_after_put())
        .expect("crashAfterPut conforms");
    assert!(!lost_at(&w, 1, 1), "hold_first_sends_nothing_unheld");
    let w = lab.replay(&capped_spine()).expect("cappedSpine conforms");
    assert!(!w.kept[2], "capped_spine_keeps_nothing_of_v2");
    assert!(!lost_at(&w, 1, 1), "spine_gate_keeps_v1");

    let covered = conforms(&TWO_VERSIONS, "two versions");
    assert!(
        covered.deepest < TWO_VERSIONS.depth && covered.states > 100_000,
        "the search reaches every state of the universe: {covered:?}"
    );
    single_slot_hold_diverges(&TWO_VERSIONS, "two versions");
    keeps_superseded_diverges(&TWO_VERSIONS, "two versions");
    spine_always_diverges();
}

/// P3 binding (3), held drafts, as [`held_drafts_follow_p3s_step_through_every_state_of_two_versions`]
/// over [`THREE_VERSIONS`]: a landing that settles two holds at once, one its spine names two
/// steps back.
///
/// NEGATIVE CONTROL, run in the test: [`single_slot_hold_diverges`].
#[test]
fn held_drafts_follow_p3s_step_through_three_versions_to_six_steps() {
    let covered = conforms(&THREE_VERSIONS, "three versions");
    assert!(
        covered.deepest == THREE_VERSIONS.depth && covered.states > 50_000,
        "the search reaches six steps: {covered:?}"
    );
    single_slot_hold_diverges(&THREE_VERSIONS, "three versions");
}

/// A refusal of the records store at an adoption (here its held total, full of the founded log)
/// is transient: the version's hold stays as it was, and a later adoption, once the store has
/// room, takes its records in and removes it. The adoption removes a hold only where the ingest
/// took its records ([`nle::hold_adopts`], the island's `stAdopt`), so no hold goes while its
/// records are nowhere else.
#[test]
fn a_refused_adoption_keeps_the_hold_for_a_later_one() {
    let lab = Lab::new();
    let paths = &lab.work;
    // Version 1, drafted from version 0 (`real_seq`: one past the model's sequence).
    let (v, seq) = (1, 2);
    let envelope = lab.envelope_of(v, seq, &[OPENED]);
    let version = envelope.digest();
    hold_drafted(paths, &lab.drafted(v), seq, &version, &[url(1)]).unwrap();
    let hold = held_draft_file(paths, seq, &version);
    let record = *lab.records[v].id();

    let full = crate::membership_log::RecordBudgets {
        held: 1,
        ..crate::membership_log::RecordBudgets::PRODUCTION
    };
    let adopted = full
        .within(|| adopt_held_draft(paths, &envelope, &lab.identity))
        .expect("a refused ingest is no error");
    assert!(!adopted, "a full store takes nothing in");
    assert!(hold.exists(), "and the hold stays");
    assert!(!super::super::load_records(paths).unwrap().contains(&record));

    assert!(
        adopt_held_draft(paths, &envelope, &lab.identity).unwrap(),
        "with room, the adoption takes the records in"
    );
    assert!(!hold.exists(), "and the hold goes");
    assert!(super::super::load_records(paths).unwrap().holds(&record));
}

/// The island's `LWorld` (section 13): whether the unversioned hold (`records.drafted`) is there,
/// and which of the records it was written with the records store holds complete
/// ([`Store::holds`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct LWorld {
    file: bool,
    store: [bool; 2],
}

/// The island's `LEvent`s the lab plays: a settled head (`settle true`; the file reads, holding
/// the records it was written with, which nothing rewrites), an ingest of one of its records, and
/// an adoption by the head whose stamp names one of them, in a store with room for `room` more
/// records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LEvent {
    Settle,
    Ingest(usize),
    Adopt { stamp: usize, room: usize },
}

/// The island's `lstep` for the compiled kernels, `orig` both records: an adoption takes in what
/// the store has room for, in the hold's order (`took`), complete where its stamp's record is
/// then held.
fn lstep(w: LWorld, event: LEvent) -> LWorld {
    let goes = |store: [bool; 2]| nle::unversioned_goes(true, store.iter().all(|&held| held));
    match event {
        LEvent::Settle if w.file && goes(w.store) => LWorld { file: false, ..w },
        LEvent::Settle => w,
        LEvent::Ingest(r) => {
            let mut store = w.store;
            store[r] = true;
            LWorld { store, ..w }
        }
        LEvent::Adopt { stamp, room } => {
            let mut store = w.store;
            let mut left = room;
            for held in &mut store {
                if !*held && left > 0 {
                    *held = true;
                    left -= 1;
                }
            }
            if w.file && nle::hold_adopts(true, store[stamp]) {
                LWorld {
                    file: !goes(store),
                    store,
                }
            } else {
                w
            }
        }
    }
}

/// The island's `LegacyKept` and `GoesOnlyStored` (from `before`, where given) of `w`.
fn legacy_laws(before: Option<LWorld>, w: LWorld) -> Result<(), String> {
    if !w.file && !w.store.iter().all(|&held| held) {
        return Err(format!(
            "LegacyKept: a record of the unversioned hold is in neither it nor the store: {w:?}"
        ));
    }
    if before.is_some_and(|b| b.file) && !w.file && !w.store.iter().all(|&held| held) {
        return Err(format!(
            "GoesOnlyStored: the hold went while the store lacked a record of it: {w:?}"
        ));
    }
    Ok(())
}

impl Lab {
    /// The unversioned hold's records: versions 1 and 2's, siblings over the founded log.
    fn legacy(&self) -> [&Record; 2] {
        [&self.records[1], &self.records[2]]
    }

    /// What the home holds of the unversioned hold, as an [`LWorld`].
    fn legacy_world(&self) -> Result<LWorld, String> {
        let store = super::super::load_records(&self.work).map_err(|e| format!("store: {e}"))?;
        Ok(LWorld {
            file: unversioned_held_draft_file(&self.work).exists(),
            store: self.legacy().map(|record| store.holds(record.id())),
        })
    }

    /// `event` on the home, from `w`: a settled head ([`settle_held_drafts_locked`]), a sync's
    /// ingest ([`super::super::save_records`]), or the adoption of the head whose stamp names a
    /// record ([`adopt_held_draft`]) in a store with room for `room` more.
    fn legacy_act(&self, event: LEvent) -> Result<(), String> {
        let paths = &self.work;
        match event {
            LEvent::Settle => settle_held_drafts_locked(paths, 1, &[OPENED], &[url(1)]),
            LEvent::Ingest(r) => {
                super::super::save_records(paths, &encode_records([self.legacy()[r]]))
                    .map_err(|e| format!("save_records: {e}"))?;
            }
            LEvent::Adopt { stamp, room } => {
                let store = super::super::load_records(paths).map_err(|e| format!("store: {e}"))?;
                let budgets = crate::membership_log::RecordBudgets {
                    held: store.len() + room,
                    ..crate::membership_log::RecordBudgets::PRODUCTION
                };
                let envelope = self.envelope_of(stamp + 1, 9, &[OPENED]);
                budgets
                    .within(|| adopt_held_draft(paths, &envelope, &self.identity))
                    .map_err(|e| format!("adopt_held_draft: {e}"))?;
            }
        }
        Ok(())
    }
}

/// The unversioned hold's steps from `w`: a settle, an ingest of a record the store does not
/// hold, and an adoption by a head whose stamp names a record the store does not hold yet (the
/// code takes in no hold for a stamp already complete), with room for one record or both.
fn legacy_steps(w: LWorld) -> Vec<LEvent> {
    let mut out = vec![LEvent::Settle];
    for (r, &stored) in w.store.iter().enumerate() {
        if !stored {
            out.push(LEvent::Ingest(r));
            for room in [1, 2] {
                out.push(LEvent::Adopt { stamp: r, room });
            }
        }
    }
    out
}

/// Every sequence of [`legacy_steps`] to `depth` from `w` on both sides, each on the home as it
/// was at `w` ([`Content`]); the number of steps, or the first divergence or broken law, with its
/// path.
fn legacy_explore(
    lab: &Lab,
    w: LWorld,
    depth: usize,
    path: &mut Vec<LEvent>,
) -> Result<usize, String> {
    if depth == 0 {
        return Ok(0);
    }
    let here = Content::of(&lab.work.dir);
    let mut steps = 0;
    for event in legacy_steps(w) {
        path.push(event);
        lab.legacy_act(event)
            .map_err(|e| format!("after {path:?}: {e}"))?;
        let model = lstep(w, event);
        let real = lab
            .legacy_world()
            .map_err(|e| format!("after {path:?}: {e}"))?;
        if real != model {
            return Err(format!(
                "after {path:?}: the home holds {real:?}, the step {model:?}"
            ));
        }
        legacy_laws(Some(w), real).map_err(|e| format!("after {path:?}: {e}"))?;
        steps += 1 + legacy_explore(lab, model, depth - 1, path)?;
        Content::of(&lab.work.dir).turn_into(&here, &lab.work.dir);
        path.pop();
    }
    Ok(steps)
}

/// A lab whose home holds the unversioned hold of versions 1 and 2's records, written by a build
/// before per-version holds.
fn legacy_lab() -> (Lab, LWorld) {
    let lab = Lab::new();
    let file = unversioned_held_draft_file(&lab.work);
    fsx::write_atomic(&file, &encode_records(lab.legacy()), 0o600).unwrap();
    let w = lab.legacy_world().unwrap();
    assert_eq!(
        w,
        LWorld {
            file: true,
            store: [false, false]
        },
        "the hold is there, its records in it alone"
    );
    (lab, w)
}

/// P3 binding (3), the unversioned hold (section 13): the file and the records store follow the
/// island's `lstep` through every sequence of settles, ingests and adoptions (with room for one
/// record or both) to four steps, and `LegacyKept` and `GoesOnlyStored` hold of the home after
/// every step.
///
/// NEGATIVE CONTROL, run in the test: `unversioned-goes-early` (the hold goes once it reads,
/// whatever of it the store holds) parts from the step at the first settle, in the file, and the
/// home breaks `LegacyKept`: both records are in neither the file nor the store.
#[test]
fn the_unversioned_hold_follows_p3s_step() {
    let (lab, w) = legacy_lab();
    let started = Instant::now();
    let steps = legacy_explore(&lab, w, 4, &mut Vec::new()).unwrap_or_else(|why| panic!("{why}"));
    eprintln!(
        "P3 unversioned-hold conformance: {steps} steps to depth 4 in {:?}",
        started.elapsed()
    );

    UNVERSIONED_GOES_EARLY.with(|cell| cell.set(true));
    let diverged = legacy_explore(&lab, w, 1, &mut Vec::new());
    // The search stops at the divergence: the home is as the settle left it.
    let real = lab.legacy_world().unwrap();
    UNVERSIONED_GOES_EARLY.with(|cell| cell.set(false));
    let why = diverged.expect_err("CONTROL: unversioned-goes-early diverges from the step");
    assert!(
        why.starts_with("after [Settle]: the home holds LWorld { file: false"),
        "CONTROL: it parts at the settle, in the file: {why}"
    );
    let broken = legacy_laws(Some(w), real).expect_err("CONTROL: the home breaks a law");
    assert!(
        broken.starts_with("LegacyKept"),
        "CONTROL: unversioned-goes-early breaks LegacyKept on the home: {broken}"
    );
}
