// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `swarm partition`: the lab partition lane (`docs/DESIGN-fluid-swarm-2026-10-28.md` §4.1 F8).
//!
//! Five private headless aterms and one vault-only Mac share this Mac's loopback as two islands.
//! Island A: a1 founds the vault, is its first admin, serves it to island A and issues the
//! fleet; a2 follows a1's server; a3 asks to join the vault during the cut. Island B: b1 serves
//! the vault to island B and, unless `--writers a`, is an admin too; b2 follows b1's server; b3
//! asks to join the vault and the fleet during the cut. Each Mac's subscriber and approver is the
//! loop of `aterm secrets watch` ([`aterm_secrets::client::watch`]), which this lab runs in a
//! process per Mac ([`agent_main`]), so the converge runner it starts is this lab's stand-in
//! ([`converge_standin`]), which notes the start and runs no pass. Every act a person would type
//! is the aterm under test's CLI, run in that Mac's world, but a member's Allow: a lab instance
//! shows no sheet to press, so the lab runs F10's members desk and the phrase's run in this
//! process on both Macs' own custody ([`Lane::allow`]).
//!
//! The cut is aterm-uds's partition seam (`ATERM_DEBUG_PARTITION`): every process of island A
//! names a file that lists island B's ports (its vault server's and its fleet listeners'), and
//! B's lists A's, so a connect across fails with no route, as a Mac that left the LAN finds. The
//! heal empties both files. The lab's own reads (servers' heads, books, state files) are never
//! cut: they are the ground truth.
//!
//! A run brings the Macs up, forms the fleet, founds the vault, admits every Mac and waits for
//! one head everywhere. Then it cuts. On each side it publishes entries and admits a newcomer, as
//! far as the build allows, and reads surveys, drives and heartbeats, holding the cut past the
//! fleet's 60 s copy window. Then it heals and waits (the one-minute hang detector) for one head
//! on every server and Mac and for the fleet to answer whole. It ends by saying what each part did,
//! with timings, and what was lost. `--one-url` leaves the vault's `aterm-swarm-policy` server list
//! out, so a publish reaches only the publisher's own server: F8's negative control, under which the
//! other island never converges.
//!
//! Commons and the account claims do not run in lab instances (the Commons runtime is off under
//! a private state root, and the harness is off), so their planes are this lab's: the Commons
//! plane ([`commons`]) drives the production repair exchange in this process on each Mac's own
//! vault custody, pairing Macs of one island during the cut; the accounts plane ([`accounts`]) is
//! a model run of main's account claims on two Macs' table replicas on a simulated clock.
//!
//! A run whose cut held is a measurement of the build, whatever it finds, so `--record` writes it
//! under `docs/measured/` red or green. Only a run whose cut leaked is never recorded there.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::Stdio;

use crate::fleet_world::{self as world, World};
use crate::{Fail, Res, procs, stop};

#[path = "swarm_partition_commons.rs"]
pub mod commons;

#[path = "swarm_partition_accounts.rs"]
pub mod accounts;

/// Hang detector for an event that must happen.
const HANG: Duration = Duration::from_secs(60);

/// The lab vault's name: never `alab`, whose name the servers key their advert on.
const VAULT: &str = "f8lab";

/// The fleet heartbeat's period in the lane's instances (`ATERM_DEBUG_FLEET_HEARTBEAT_MS`;
/// production's is 180 s), so a heartbeat lands within the cut and soon after the heal.
const HEARTBEAT_MS: &str = "5000";

/// How long a member's copy of the issuer's book binds a drive alone
/// (`aterm_net::fleet_member::COPY_DRIVES_FOR`), plus a margin: the late drive waits for it.
const COPY_WINDOW: Duration = Duration::from_secs(62);

/// The cut's default length: past [`COPY_WINDOW`] with room for the late drive.
const DEFAULT_HOLD: Duration = Duration::from_secs(80);

/// This lab's verb for a Mac's subscriber and approver ([`agent_main`]).
pub const AGENT_VERB: &str = "partition-agent";

/// Set in an agent's environment: the `init --converge` its subscriber starts is
/// [`converge_standin`].
pub const AGENT_SEAM: &str = "ATERM_LAB_PARTITION_AGENT";

/// Under a Mac's home: one line per converge runner its subscriber started.
const RUNNERS_FILE: &str = ".lab-converge-runs";

/// The usage line.
pub const USAGE: &str = "swarm partition [--one-url] [--writers both|a] [--hold SECS] [--aterm PATH] [--keep] [--record]";

/// One of the two islands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Island {
    /// a1, a2 and a3: the vault's founder and the fleet's issuer are here.
    A,
    /// b1, b2 and b3.
    B,
}

impl Island {
    /// `A` or `B`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Island::A => "A",
            Island::B => "B",
        }
    }
}

/// Where an admin's publish goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Publish {
    /// The vault's `aterm-swarm-policy` lists both servers: main's multi-server path.
    Policy,
    /// No server list: the publisher's own server alone (F8's negative control).
    OneUrl,
}

/// Who can write during the cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Writers {
    /// a1 and b1 are admins.
    Both,
    /// a1 alone: island B holds no admin.
    A,
}

/// `swarm partition`'s options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opts {
    /// The aterm under test (`--aterm`), else the lab's.
    pub aterm: Option<String>,
    /// `--one-url`.
    pub publish: Publish,
    /// `--writers`.
    pub writers: Writers,
    /// `--hold`: the least the cut lasts (longer where the work in it takes longer).
    pub hold: Duration,
    /// `--keep`: leave the worlds behind.
    pub keep: bool,
    /// `--record`: write the record under `docs/measured/`.
    pub record: bool,
}

/// Parse `swarm partition`'s arguments.
///
/// # Errors
/// An unknown or malformed argument.
pub fn parse(args: &[String]) -> Result<Opts, Fail> {
    let mut opts = Opts {
        aterm: None,
        publish: Publish::Policy,
        writers: Writers::Both,
        hold: DEFAULT_HOLD,
        keep: false,
        record: false,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => return Err(Fail::usage(USAGE)),
            "--one-url" => opts.publish = Publish::OneUrl,
            "--keep" => opts.keep = true,
            "--record" => opts.record = true,
            "--aterm" => {
                opts.aterm = Some(
                    it.next()
                        .ok_or_else(|| Fail::usage("--aterm needs a PATH"))?
                        .clone(),
                );
            }
            "--writers" => {
                opts.writers = match it.next().map(String::as_str) {
                    Some("both") => Writers::Both,
                    Some("a") => Writers::A,
                    _ => return Err(Fail::usage("--writers is both or a")),
                };
            }
            "--hold" => {
                let secs: u64 = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .filter(|s| (10..=600).contains(s))
                    .ok_or_else(|| Fail::usage("--hold is 10 to 600 seconds"))?;
                opts.hold = Duration::from_secs(secs);
            }
            other => {
                return Err(Fail::usage(format!(
                    "swarm partition: unexpected '{other}' ({USAGE})"
                )));
            }
        }
    }
    Ok(opts)
}

// ---------------------------------------------------------------------------------------------
// Evidence: what a run saw, judged with no I/O.
// ---------------------------------------------------------------------------------------------

/// A vault version as a server serves it or a Mac holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Head {
    /// Its sequence number.
    pub seq: u64,
    /// Its digest, hex.
    pub digest: String,
    /// How many parents it names (two or more: a merge).
    pub parents: usize,
}

/// One `aterm secrets set` an island tried during the cut.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetTry {
    /// The entry's name.
    pub name: String,
    /// The island it was set on.
    pub island: Island,
    /// The Mac that set it.
    pub by: String,
    /// Whether the `set` exited 0.
    pub ok: bool,
    /// Its words when it did not (one line).
    pub words: String,
}

/// One Mac that asked to join the vault during the cut.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinTry {
    /// The Mac.
    pub mac: String,
    /// Its island.
    pub island: Island,
    /// Whether its request was posted (`join --no-wait` left it pending or in).
    pub posted: bool,
    /// Milliseconds after the cut its own state first read `in`, if it did during the cut.
    pub in_during_cut_ms: Option<u64>,
}

/// One island as the cut's end found it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IslandView {
    /// The island.
    pub island: Island,
    /// Its server's head.
    pub server: Option<Head>,
    /// Each member Mac (`in`) and the head it holds.
    pub macs: Vec<(String, Option<Head>)>,
    /// Each member that holds the value of an entry the other island set during the cut: what
    /// crossed, whatever the servers' heads say.
    pub crossed: Vec<String>,
}

/// One Mac after the heal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MacAfter {
    /// The Mac.
    pub mac: String,
    /// Its island.
    pub island: Island,
    /// Its membership word (`in`, `pending`, `none`, `revoked`).
    pub membership: String,
    /// The head it holds at the end.
    pub head: Option<Head>,
    /// Milliseconds after the heal it first held the final head.
    pub at_ms: Option<u64>,
}

/// Where an entry set during the cut is after the heal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryAfter {
    /// The entry.
    pub name: String,
    /// The island that set it.
    pub island: Island,
    /// The member Macs whose `~/.secrets/<name>` holds its value.
    pub on: Vec<String>,
    /// The member Macs whose file is missing or holds another value.
    pub missing: Vec<String>,
}

/// The vault plane's evidence.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VaultEvidence {
    /// The one head every server and Mac held before the cut.
    pub before: Option<Head>,
    /// The Macs that held it: the members going into the cut.
    pub members_before: Vec<String>,
    /// Every `set` during the cut.
    pub sets: Vec<SetTry>,
    /// Every Mac that asked to join during the cut.
    pub joins: Vec<JoinTry>,
    /// Each island at the cut's end.
    pub end_of_cut: Vec<IslandView>,
    /// When (ms after the heal) every server and member first held one head, and that head.
    pub healed: Option<(u64, Head)>,
    /// Each server's head at the end, by island.
    pub servers_after: Vec<(Island, Option<Head>)>,
    /// Each Mac at the end.
    pub macs_after: Vec<MacAfter>,
    /// Each entry set during the cut, after the heal.
    pub entries_after: Vec<EntryAfter>,
    /// Each Mac's converge runner starts: (before the cut, during it, after the heal).
    pub runners: Vec<(String, [usize; 3])>,
}

/// When a drive was tried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Before the cut.
    Before,
    /// Early in the cut, while every copy of the issuer's book was fresh.
    EarlyCut,
    /// Late in the cut, once the driven Mac's copy was past the 60 s window.
    LateCut,
    /// After the heal.
    Healed,
}

impl Phase {
    /// Its word in the report.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Phase::Before => "before",
            Phase::EarlyCut => "early in the cut",
            Phase::LateCut => "late in the cut",
            Phase::Healed => "after the heal",
        }
    }
}

/// One `aterm ctl --host … status` drive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriveTry {
    /// When.
    pub phase: Phase,
    /// The driving Mac.
    pub from: String,
    /// The driven Mac.
    pub to: String,
    /// Whether it exited 0.
    pub ok: bool,
    /// Its words when it did not (one line).
    pub words: String,
    /// The driven Mac's copy of the issuer's book, seconds old, when the drive started.
    pub copy_age_s: Option<u64>,
}

/// One `aterm ctl survey --fleet` from one Mac.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SurveyRead {
    /// When.
    pub phase: Phase,
    /// The reading Mac.
    pub from: String,
    /// The Macs whose block answered.
    pub answered: Vec<String>,
    /// Each running Mac whose block did not answer, with the survey's words for it (its
    /// `did not answer` header, or none where the survey named it not at all).
    pub silent: Vec<String>,
}

/// One `aterm fleet join` by b3.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FleetJoin {
    /// When.
    pub phase: Phase,
    /// Whether it exited 0 and the instance listens.
    pub ok: bool,
    /// Its words when it did not (one line).
    pub words: String,
}

/// A member's heartbeats as the issuer's book shows them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Heartbeat {
    /// The member.
    pub mac: String,
    /// Its island.
    pub island: Island,
    /// Whether its `seen_ms` advanced between the cut and the heal.
    pub advanced_in_cut: bool,
    /// Milliseconds after the heal its `seen_ms` first passed the heal.
    pub back_ms: Option<u64>,
}

/// The fleet plane's evidence.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FleetEvidence {
    /// Every survey read.
    pub surveys: Vec<SurveyRead>,
    /// Every drive.
    pub drives: Vec<DriveTry>,
    /// b3's joins.
    pub joins: Vec<FleetJoin>,
    /// The members' heartbeats.
    pub heartbeats: Vec<Heartbeat>,
    /// Why the late drives did not run, when they did not.
    pub late_skipped: Option<String>,
    /// Milliseconds after the heal when both surveys first heard every member, if they did
    /// within the hang detector.
    pub whole_ms: Option<u64>,
    /// Milliseconds after the heal of the first survey read after it (they start once the
    /// vault's wait ends, so `whole_ms` is never earlier than this).
    pub first_read_ms: Option<u64>,
}

/// What a run saw.
#[derive(Clone, Debug, PartialEq)]
pub struct Evidence {
    /// `--one-url` or not.
    pub publish: Publish,
    /// `--writers`.
    pub writers: Writers,
    /// The vault plane.
    pub vault: VaultEvidence,
    /// The fleet plane.
    pub fleet: FleetEvidence,
    /// The Commons plane, where it ran.
    pub commons: Option<commons::Evidence>,
    /// The accounts plane, where it ran.
    pub accounts: Option<accounts::Evidence>,
    /// This Mac's 1-minute load average at each mark (`start`, just after bring-up; `cut`, `heal`,
    /// `end`): a
    /// debug aterm starved of a core misses the fleet's 3 s handshake, so a reader weighs a
    /// missed handshake against it.
    pub load: Vec<(String, f64)>,
    /// How long the cut lasted: at least `--hold`, longer where the work in it took longer.
    pub cut_ms: u64,
}

/// One named check of a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    /// Its name.
    pub name: &'static str,
    /// Whether it held.
    pub pass: bool,
    /// What was seen.
    pub detail: String,
}

/// The vault's cut check; it and [`FLEET_CUT_HELD`] are the checks whose failure voids a run:
/// the cut leaked.
pub const CUT_HELD: &str = "vault: the cut held";
/// The fleet's cut check: under `--one-url` nothing of the vault crosses even with no cut, so
/// only this one sees an aterm that ignored the seam.
pub const FLEET_CUT_HELD: &str = "fleet: the cut held";
/// F8's own check, which `--one-url` must fail.
pub const ONE_HEAD: &str = "vault: one head on every server and Mac after the heal";

/// How long one head must hold after the heal before it counts: a reconcile still in flight
/// would have moved it.
const SETTLE: Duration = Duration::from_secs(3);

/// Judge `ev`: every check, held or not.
#[must_use]
pub fn judge(ev: &Evidence) -> Vec<Check> {
    let mut checks = vault_checks(&ev.vault);
    checks.extend(fleet_checks(&ev.fleet));
    if let Some(c) = &ev.commons {
        checks.extend(commons::judge(c));
    }
    if let Some(a) = &ev.accounts {
        checks.extend(accounts::judge(a));
    }
    checks
}

fn names(list: &[String]) -> String {
    if list.is_empty() {
        "none".to_owned()
    } else {
        list.join(", ")
    }
}

fn short(head: Option<&Head>) -> String {
    head.map_or_else(
        || "no head".to_owned(),
        |h| format!("seq {} {}", h.seq, h.digest.get(..8).unwrap_or(&h.digest)),
    )
}

/// The Macs that should be members after the heal: each one going into the cut and each one let
/// in during it.
fn expected_members(v: &VaultEvidence) -> Vec<String> {
    let mut out = v.members_before.clone();
    for j in v.joins.iter().filter(|j| j.in_during_cut_ms.is_some()) {
        if !out.contains(&j.mac) {
            out.push(j.mac.clone());
        }
    }
    out
}

/// A member after the heal, in a list of what is off: its membership where it is not `in`.
fn member_word(m: &MacAfter) -> String {
    if m.membership == "in" {
        format!("{} {}", m.mac, short(m.head.as_ref()))
    } else {
        format!("{} {} ({})", m.mac, short(m.head.as_ref()), m.membership)
    }
}

fn vault_checks(v: &VaultEvidence) -> Vec<Check> {
    let mut out = Vec::new();

    // The cut held: each island's members held their own server's head at the cut's end, and
    // an island that wrote did not reach the other's server.
    let mut leaks = Vec::new();
    for view in &v.end_of_cut {
        leaks.extend(view.crossed.iter().cloned());
        for (mac, head) in &view.macs {
            if head.as_ref().map(|h| &h.digest) != view.server.as_ref().map(|h| &h.digest) {
                leaks.push(format!(
                    "{mac} held {} while island {}'s server served {}",
                    short(head.as_ref()),
                    view.island.word(),
                    short(view.server.as_ref())
                ));
            }
        }
    }
    let server_of = |i: Island| {
        v.end_of_cut
            .iter()
            .find(|view| view.island == i)
            .and_then(|view| view.server.clone())
    };
    let wrote = |i: Island| v.sets.iter().any(|s| s.island == i && s.ok);
    if let (Some(a), Some(b)) = (server_of(Island::A), server_of(Island::B))
        && (wrote(Island::A) || wrote(Island::B))
        && a.digest == b.digest
    {
        leaks.push(format!(
            "both servers served {} at the cut's end although an island wrote",
            short(Some(&a))
        ));
    }
    if v.end_of_cut.len() != 2 {
        leaks.push("the cut's end was not read on both islands".to_owned());
    }
    out.push(Check {
        name: CUT_HELD,
        pass: leaks.is_empty(),
        detail: if leaks.is_empty() {
            format!(
                "island A ended the cut at {}, island B at {}",
                short(server_of(Island::A).as_ref()),
                short(server_of(Island::B).as_ref())
            )
        } else {
            leaks.join("; ")
        },
    });

    // Each island kept working: it set an entry and let a newcomer in.
    let mut idle = Vec::new();
    for island in [Island::A, Island::B] {
        let sets: Vec<&SetTry> = v.sets.iter().filter(|s| s.island == island).collect();
        if sets.is_empty() {
            idle.push(format!("island {} set nothing", island.word()));
        }
        for s in sets.iter().filter(|s| !s.ok) {
            idle.push(format!(
                "island {}: {} could not set {}: {}",
                island.word(),
                s.by,
                s.name,
                s.words
            ));
        }
        for j in v.joins.iter().filter(|j| j.island == island) {
            if !j.posted {
                idle.push(format!("{} could not ask to join", j.mac));
            } else if j.in_during_cut_ms.is_none() {
                idle.push(format!(
                    "{} asked on island {} and was not let in during the cut",
                    j.mac,
                    island.word()
                ));
            }
        }
    }
    out.push(Check {
        name: "vault: each island kept writing and admitting",
        pass: idle.is_empty(),
        detail: if idle.is_empty() {
            let done: Vec<String> = v
                .joins
                .iter()
                .filter_map(|j| {
                    j.in_during_cut_ms
                        .map(|ms| format!("{} in at +{:.1}s", j.mac, secs(ms)))
                })
                .collect();
            format!(
                "{} sets held; {}",
                v.sets.iter().filter(|s| s.ok).count(),
                names(&done)
            )
        } else {
            idle.join("; ")
        },
    });

    // F8's check: one head everywhere after the heal, on every Mac that should be a member (each
    // one going into the cut and each let in during it), not just on whoever still reads `in`.
    let expected = expected_members(v);
    let members: Vec<&MacAfter> = v
        .macs_after
        .iter()
        .filter(|m| expected.contains(&m.mac))
        .collect();
    let gone = |m: &MacAfter| m.membership != "in";
    // A head that split again once it was one: what the run's end reads, against it.
    let split_again: Vec<String> = v.healed.as_ref().map_or_else(Vec::new, |(_, head)| {
        let off = |h: Option<&Head>| h.is_none_or(|h| h.digest != head.digest);
        v.servers_after
            .iter()
            .filter(|(_, h)| off(h.as_ref()))
            .map(|(island, h)| format!("server {} {}", island.word(), short(h.as_ref())))
            .chain(
                members
                    .iter()
                    .filter(|m| gone(m) || off(m.head.as_ref()))
                    .map(|m| member_word(m)),
            )
            .chain(
                expected
                    .iter()
                    .filter(|e| !members.iter().any(|m| &m.mac == *e))
                    .map(|e| format!("{e} unread")),
            )
            .collect()
    });
    out.push(match &v.healed {
        Some((_, head)) if !split_again.is_empty() => Check {
            name: ONE_HEAD,
            pass: false,
            detail: format!(
                "one head {} after the heal, then split again: {}",
                short(Some(head)),
                split_again.join(", ")
            ),
        },
        Some((ms, head)) => Check {
            name: ONE_HEAD,
            pass: true,
            detail: format!(
                "{} on both servers and {} members, {:.1}s after the heal{}",
                short(Some(head)),
                members.len(),
                secs(*ms),
                if head.parents >= 2 {
                    format!(" (it names {} parents)", head.parents)
                } else {
                    String::new()
                }
            ),
        },
        None => {
            let mut seen: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for (island, head) in &v.servers_after {
                seen.entry(short(head.as_ref()))
                    .or_default()
                    .push(format!("server {}", island.word()));
            }
            for m in &members {
                seen.entry(short(m.head.as_ref()))
                    .or_default()
                    .push(if gone(m) {
                        format!("{} ({})", m.mac, m.membership)
                    } else {
                        m.mac.clone()
                    });
            }
            let split: Vec<String> = seen
                .into_iter()
                .map(|(head, who)| format!("{head}: {}", who.join(", ")))
                .collect();
            Check {
                name: ONE_HEAD,
                pass: false,
                detail: format!(
                    "never converged within {HANG:?} of the heal: {}",
                    split.join(" | ")
                ),
            }
        }
    });

    // Nothing lost: every entry set during the cut is on every member, and every Mac let in
    // during the cut is still in.
    let mut lost = Vec::new();
    for e in &v.entries_after {
        if !e.missing.is_empty() {
            lost.push(format!(
                "{} (set on island {}) is not on {}",
                e.name,
                e.island.word(),
                names(&e.missing)
            ));
        }
    }
    for s in v.sets.iter().filter(|s| s.ok) {
        if !v.entries_after.iter().any(|e| e.name == s.name) {
            lost.push(format!("{} was never looked for after the heal", s.name));
        }
    }
    for mac in &v.members_before {
        let after = v.macs_after.iter().find(|m| &m.mac == mac);
        if after.is_none_or(|m| m.membership != "in") {
            lost.push(format!(
                "{mac} was a member before the cut and is {} after the heal",
                after.map_or("unread", |m| m.membership.as_str())
            ));
        }
    }
    for j in v.joins.iter().filter(|j| j.in_during_cut_ms.is_some()) {
        let after = v.macs_after.iter().find(|m| m.mac == j.mac);
        if after.is_none_or(|m| m.membership != "in") {
            lost.push(format!(
                "{} was let in during the cut and is {} after the heal",
                j.mac,
                after.map_or("unread", |m| m.membership.as_str())
            ));
        }
    }
    out.push(Check {
        name: "vault: nothing set or admitted during the cut was lost",
        pass: lost.is_empty(),
        detail: if lost.is_empty() {
            format!(
                "{} entries on every member; every member before the cut and every Mac admitted \
                 during it still in",
                v.entries_after.len()
            )
        } else {
            lost.join("; ")
        },
    });

    // Every Mac that asked during the cut is in once the islands are one.
    let waiting: Vec<String> = v
        .joins
        .iter()
        .filter(|j| {
            v.macs_after
                .iter()
                .find(|m| m.mac == j.mac)
                .is_none_or(|m| m.membership != "in")
        })
        .map(|j| {
            let word = v
                .macs_after
                .iter()
                .find(|m| m.mac == j.mac)
                .map_or("unread", |m| m.membership.as_str());
            format!("{} ({word})", j.mac)
        })
        .collect();
    out.push(Check {
        name: "vault: every Mac that asked during the cut is in after the heal",
        pass: waiting.is_empty(),
        detail: if waiting.is_empty() {
            format!("{} asked, all in", v.joins.len())
        } else {
            format!("still out: {}", waiting.join(", "))
        },
    });
    out
}

fn fleet_checks(f: &FleetEvidence) -> Vec<Check> {
    let mut out = Vec::new();
    let island_of = |mac: &str| {
        if mac.starts_with('a') {
            Island::A
        } else {
            Island::B
        }
    };

    // The cut held: a survey from each side during the cut answered its own island alone.
    let during: Vec<&SurveyRead> = f
        .surveys
        .iter()
        .filter(|s| s.phase == Phase::EarlyCut)
        .collect();
    let mut leaks = Vec::new();
    for s in &during {
        let crossed: Vec<String> = s
            .answered
            .iter()
            .filter(|m| island_of(m) != island_of(&s.from))
            .cloned()
            .collect();
        if !crossed.is_empty() {
            leaks.push(format!("{} heard {}", s.from, crossed.join(", ")));
        }
        if !s.answered.contains(&s.from) {
            leaks.push(format!("{} did not hear itself", s.from));
        }
    }
    for d in f
        .drives
        .iter()
        .filter(|d| d.phase == Phase::EarlyCut && island_of(&d.from) != island_of(&d.to))
    {
        if d.ok {
            leaks.push(format!("{} drove {} across the cut", d.from, d.to));
        }
    }
    if during.len() < 2 {
        leaks.push("the cut was not surveyed from both islands".to_owned());
    }
    out.push(Check {
        name: FLEET_CUT_HELD,
        pass: leaks.is_empty(),
        detail: if leaks.is_empty() {
            during
                .iter()
                .map(|s| format!("{} heard {}", s.from, names(&s.answered)))
                .collect::<Vec<_>>()
                .join("; ")
        } else {
            leaks.join("; ")
        },
    });

    // Each island kept driving its own members for the whole cut.
    let mut refused = Vec::new();
    let late: Vec<&DriveTry> = f
        .drives
        .iter()
        .filter(|d| d.phase == Phase::LateCut)
        .collect();
    for d in &late {
        if !d.ok {
            refused.push(format!(
                "{} -> {} refused with its copy {}s old: {}",
                d.from,
                d.to,
                d.copy_age_s.map_or("?".to_owned(), |s| s.to_string()),
                d.words
            ));
        }
    }
    if let Some(why) = &f.late_skipped {
        refused.push(format!("not tried: {why}"));
    }
    out.push(Check {
        name: "fleet: each island drove its own members for the whole cut",
        pass: refused.is_empty() && !late.is_empty(),
        detail: if refused.is_empty() {
            late.iter()
                .map(|d| format!("{} -> {} admitted", d.from, d.to))
                .collect::<Vec<_>>()
                .join("; ")
        } else {
            refused.join("; ")
        },
    });

    // A newcomer joined the fleet on the island without the issuer.
    let in_cut = f.joins.iter().find(|j| j.phase == Phase::EarlyCut);
    out.push(Check {
        name: "fleet: b3 joined on island B during the cut",
        pass: in_cut.is_some_and(|j| j.ok),
        detail: in_cut.map_or_else(
            || "never tried".to_owned(),
            |j| {
                if j.ok {
                    "joined".to_owned()
                } else {
                    format!("refused: {}", j.words)
                }
            },
        ),
    });

    // After the heal the fleet is whole again: every survey answers every member, every
    // member's heartbeat reaches the issuer, every late refusal is admitted, and b3 is in.
    let mut broken = Vec::new();
    let healed: Vec<&SurveyRead> = f
        .surveys
        .iter()
        .filter(|s| s.phase == Phase::Healed)
        .collect();
    let mut everyone: Vec<String> = f
        .surveys
        .iter()
        .filter(|s| s.phase == Phase::Before)
        .flat_map(|s| s.answered.clone())
        .collect();
    // b3 joined the fleet (in the cut, or after the heal), so the issuer's book holds it and
    // every survey must hear it.
    if f.joins.iter().any(|j| j.ok) && !everyone.iter().any(|m| m == "b3") {
        everyone.push("b3".to_owned());
    }
    for s in &healed {
        let missing: Vec<String> = everyone
            .iter()
            .filter(|m| !s.answered.contains(m))
            .cloned()
            .collect();
        if !missing.is_empty() {
            let why: Vec<&String> = s
                .silent
                .iter()
                .filter(|w| missing.iter().any(|m| w.starts_with(&format!("{m}:"))))
                .collect();
            broken.push(format!(
                "{} did not hear {}{}",
                s.from,
                missing.join(", "),
                if why.is_empty() {
                    String::new()
                } else {
                    format!(
                        " ({})",
                        why.iter()
                            .map(|w| w.as_str())
                            .collect::<Vec<_>>()
                            .join("; ")
                    )
                }
            ));
        }
    }
    if healed.is_empty() {
        broken.push("no survey after the heal".to_owned());
    }
    for h in &f.heartbeats {
        if h.back_ms.is_none() {
            broken.push(format!("{}'s heartbeat never reached the issuer", h.mac));
        }
    }
    for d in f.drives.iter().filter(|d| d.phase == Phase::Healed) {
        if !d.ok {
            broken.push(format!("{} -> {} still refused: {}", d.from, d.to, d.words));
        }
    }
    if !f.joins.iter().any(|j| j.ok) {
        broken.push("b3 never joined".to_owned());
    }
    let back: Vec<String> = f
        .heartbeats
        .iter()
        .filter_map(|h| h.back_ms.map(|ms| format!("{} +{:.1}s", h.mac, secs(ms))))
        .collect();
    out.push(Check {
        name: "fleet: whole again after the heal",
        pass: broken.is_empty(),
        detail: if broken.is_empty() {
            format!(
                "{}heartbeats back: {}",
                f.whole_ms.map_or(String::new(), |ms| format!(
                    "every survey whole +{:.1}s{}; ",
                    secs(ms),
                    f.first_read_ms.map_or(String::new(), |first| format!(
                        " (first read +{:.1}s)",
                        secs(first)
                    ))
                )),
                names(&back)
            )
        } else {
            broken.join("; ")
        },
    });
    out
}

fn secs(ms: u64) -> f64 {
    ms as f64 / 1000.0
}

// ---------------------------------------------------------------------------------------------
// The run.
// ---------------------------------------------------------------------------------------------

/// One Mac of the lane.
#[derive(Clone, Copy, Debug)]
struct Spec {
    name: &'static str,
    island: Island,
    /// Its world's index (`<base>/mNN`).
    slot: usize,
    /// It runs a headless aterm instance (and so a fleet listener).
    instance: bool,
    /// It arrives during the cut.
    late: bool,
}

const MACS: [Spec; 6] = [
    Spec {
        name: "a1",
        island: Island::A,
        slot: 0,
        instance: true,
        late: false,
    },
    Spec {
        name: "a2",
        island: Island::A,
        slot: 1,
        instance: true,
        late: false,
    },
    Spec {
        name: "b1",
        island: Island::B,
        slot: 2,
        instance: true,
        late: false,
    },
    Spec {
        name: "b2",
        island: Island::B,
        slot: 3,
        instance: true,
        late: false,
    },
    Spec {
        name: "b3",
        island: Island::B,
        slot: 4,
        instance: true,
        late: true,
    },
    Spec {
        name: "a3",
        island: Island::A,
        slot: 5,
        instance: false,
        late: true,
    },
];

fn spec(name: &str) -> Spec {
    *MACS
        .iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| unreachable!("no Mac {name}"))
}

/// A Mac's vault state, as its `state.toml` says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Held {
    membership: String,
    head: Option<Head>,
}

/// The lane's running state.
struct Lane {
    world: World,
    opts: Opts,
    t0: Instant,
    /// Each instance Mac's fleet port.
    fleet_ports: BTreeMap<&'static str, u16>,
    /// Island A's and island B's vault server ports.
    vault_ports: [u16; 2],
    cut_files: [PathBuf; 2],
    /// Each running instance's index in `world.insts`.
    insts: BTreeMap<&'static str, usize>,
    /// Each running instance's boot session.
    sids: BTreeMap<&'static str, String>,
    /// The vault's signer pin.
    signer: String,
    /// The fleet's join code.
    code: String,
    /// The lifelines of the servers and agents this lane started (cut when it drops).
    lifelines: Vec<aterm_uds::lifeline::Lifeline>,
    /// The servers and agents this lane started, with each one's start time.
    kids: Vec<(i32, String)>,
    /// Each fixture value, by entry name (in this process alone; never printed).
    values: BTreeMap<String, Vec<u8>>,
    /// Every line said, for the record.
    said: RefCell<Vec<String>>,
}

impl Drop for Lane {
    fn drop(&mut self) {
        self.lifelines.clear();
        for (pid, started) in &self.kids {
            for kid in procs::descendants(*pid) {
                let _ = crate::sys::kill(kid, libc::SIGTERM);
            }
            if crate::sys::alive(*pid) && procs::start_time(*pid) == *started {
                let _ = crate::sys::kill(*pid, libc::SIGTERM);
            }
        }
        for (pid, started) in &self.kids {
            if !procs::wait_gone(*pid, Duration::from_secs(5))
                && procs::start_time(*pid) == *started
            {
                let _ = crate::sys::kill(*pid, libc::SIGKILL);
            }
        }
    }
}

fn one_line(text: &str) -> String {
    crate::util::one_line(text.trim(), 200)
}

fn ms_since(t: Instant) -> u64 {
    u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn hex(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(bytes)
}

impl Lane {
    fn say(&self, line: &str) {
        let at = format!("{:7.1}s  {line}", self.t0.elapsed().as_secs_f64());
        println!("  {at}");
        self.said.borrow_mut().push(at);
    }

    fn root(&self, mac: &str) -> PathBuf {
        self.world.root_of(spec(mac).slot)
    }

    fn home(&self, mac: &str) -> PathBuf {
        self.root(mac).join("home")
    }

    fn url(&self, island: Island) -> String {
        let port = self.vault_ports[usize::from(island == Island::B)];
        format!("http://127.0.0.1:{port}/v1/{VAULT}")
    }

    fn pinned(&self, island: Island) -> String {
        format!("{}#{}", self.url(island), self.signer)
    }

    /// `aterm <args>` in `mac`'s world, `stdin` fed from a file, under the hang detector.
    fn aterm(&self, mac: &str, args: &[&str], stdin: Option<&str>) -> world::Out {
        let root = self.root(mac);
        let mut cmd = aterm_uds::spawnfd::command(&self.world.aterm);
        cmd.env_clear()
            .envs(self.world.env_of(&root))
            .args(args)
            .current_dir(root.join("home"));
        let input = match stdin {
            Some(text) => {
                let file = root.join("tmp/lab-stdin");
                match std::fs::write(&file, text).and_then(|()| std::fs::File::open(&file)) {
                    Ok(f) => Stdio::from(f),
                    Err(e) => {
                        return world::Out {
                            code: -1,
                            stderr: format!("its stdin: {e}"),
                            ..world::Out::default()
                        };
                    }
                }
            }
            None => Stdio::null(),
        };
        match aterm_uds::deadline::output_until_from(&mut cmd, input, Instant::now() + HANG) {
            Ok(out) => world::Out {
                code: out.status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            },
            Err(aterm_uds::deadline::RunError::TimedOut) => world::Out {
                code: -1,
                stderr: format!("still running after {HANG:?} (the hang detector)"),
                ..world::Out::default()
            },
            Err(e) => world::Out {
                code: -1,
                stderr: format!("{e:?}"),
                ..world::Out::default()
            },
        }
    }

    /// [`Lane::aterm`] that must exit 0: its stdout.
    fn aterm_ok(&self, mac: &str, args: &[&str], stdin: Option<&str>) -> Result<String, Fail> {
        let out = self.aterm(mac, args, stdin);
        if out.code == 0 {
            Ok(out.stdout)
        } else {
            Err(Fail::die(format!(
                "{mac}: aterm {} exited {}: {}",
                args.join(" "),
                out.code,
                one_line(&format!("{} {}", out.stdout, out.stderr))
            )))
        }
    }

    /// Start a long-running child in `mac`'s world that ends with this lane: `program args`
    /// behind a shell that ends it once this process's lifeline closes (`aterm secrets serve`
    /// watches none of its own), its output in `<root>/<log>`.
    fn start_kept(
        &mut self,
        mac: &str,
        program: &Path,
        args: &[&str],
        extra: &[(String, String)],
        log: &str,
    ) -> Result<i32, Fail> {
        let root = self.root(mac);
        let out = std::fs::File::create(root.join(log))
            .map_err(|e| Fail::die(format!("{mac}: its {log}: {e}")))?;
        let err = out.try_clone().map_err(|e| Fail::die(e.to_string()))?;
        let (lifeline, far) = aterm_uds::lifeline::Lifeline::open(&root)
            .map_err(|e| Fail::die(format!("{mac}: a lifeline: {e}")))?;
        let mut cmd = aterm_uds::spawnfd::command("/bin/sh");
        cmd.arg("-c")
            .arg("\"$@\" </dev/null & kid=$!; read -r _; kill \"$kid\" 2>/dev/null; wait \"$kid\"")
            .arg(format!("lab-{mac}-{log}"))
            .arg(program)
            .args(args)
            .env_clear()
            .envs(self.world.env_of(&root))
            .envs(extra.iter().map(|(k, v)| (k, v)))
            .current_dir(root.join("home"))
            .setsid(true)
            .stdin(Stdio::from(far))
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err));
        let pid = procs::spawn_kept(&mut cmd)
            .map_err(|e| Fail::die(format!("{mac}: cannot start {}: {e}", program.display())))?;
        self.lifelines.push(lifeline);
        self.kids.push((pid, procs::start_time(pid)));
        Ok(pid)
    }

    /// Start `mac`'s vault server on its island's port and wait until it listens.
    fn start_server(&mut self, mac: &str) -> Result<(), Fail> {
        let island = spec(mac).island;
        let port = self.vault_ports[usize::from(island == Island::B)];
        let listen = format!("127.0.0.1:{port}");
        let aterm = self.world.aterm.clone();
        self.start_kept(
            mac,
            &aterm,
            &[
                "secrets",
                "serve",
                "--listen",
                &listen,
                "--no-advertise",
                "--no-keep-awake",
            ],
            &[],
            "vault-server.log",
        )?;
        let addr: std::net::SocketAddr = listen.parse().map_err(|_| Fail::die("an address"))?;
        let end = Instant::now() + HANG;
        while std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_err() {
            if Instant::now() >= end {
                return Err(Fail::die(format!(
                    "{mac}'s vault server did not listen on {listen} within {HANG:?}: {}",
                    world::tail(&self.root(mac).join("vault-server.log"), 8)
                )));
            }
            stop::sleep(Duration::from_millis(100))?;
        }
        self.say(&format!(
            "{mac} serves the vault to island {} on {listen}",
            island.word()
        ));
        Ok(())
    }

    /// Start `mac`'s subscriber and approver ([`agent_main`]).
    fn start_agent(&mut self, mac: &str) -> Result<(), Fail> {
        let exe = crate::paths::paths().exe.clone();
        let home = self.home(mac).to_string_lossy().into_owned();
        self.start_kept(
            mac,
            &exe,
            &[AGENT_VERB, "--home", &home],
            &[(AGENT_SEAM.to_owned(), "1".to_owned())],
            "agent.log",
        )?;
        Ok(())
    }

    /// `mac`'s vault state from its `state.toml`, read with no lock (a torn read reads as none).
    fn held(&self, mac: &str) -> Held {
        let file = self.home(mac).join(".config/aterm/secrets/state.toml");
        let Ok(text) = std::fs::read_to_string(file) else {
            return Held {
                membership: "none".to_owned(),
                head: None,
            };
        };
        let Ok(table) = aterm_toml::from_str::<aterm_toml::Table>(&text) else {
            return Held {
                membership: "none".to_owned(),
                head: None,
            };
        };
        let membership = table
            .get("membership")
            .and_then(aterm_toml::Value::as_str)
            .unwrap_or("none")
            .to_owned();
        let seq = table
            .get("seq")
            .and_then(aterm_toml::Value::as_integer)
            .and_then(|n| u64::try_from(n).ok());
        let digest = table
            .get("digest")
            .and_then(aterm_toml::Value::as_str)
            .map(str::to_owned);
        let head = match (seq, digest) {
            (Some(seq), Some(digest)) if seq > 0 => {
                let parents = std::fs::read(self.home(mac).join(".config/aterm/secrets/vault"))
                    .ok()
                    .and_then(|b| aterm_secrets::envelope::parse(&b).ok())
                    .filter(|e| hex(&e.digest()) == digest)
                    .map_or(0, |e| e.parents.len());
                Some(Head {
                    seq,
                    digest,
                    parents,
                })
            }
            _ => None,
        };
        Held { membership, head }
    }

    /// The head `island`'s server serves, read by this process (never cut). `Ok(None)`: it
    /// serves no vault. A server that does not answer within the hang detector, or serves bytes
    /// that are no envelope, is a lane that could not run, never "no head".
    fn served(&self, island: Island) -> Result<Option<Head>, Fail> {
        let fetched = aterm_secrets::transport::fetch_within(&self.url(island), None, HANG)
            .map_err(|e| {
                Fail::die(format!(
                    "island {}'s server could not be read within {HANG:?}: {e}",
                    island.word()
                ))
            })?;
        let aterm_secrets::transport::Fetched::Body(bytes) = fetched else {
            return Ok(None);
        };
        let env = aterm_secrets::envelope::parse(&bytes).map_err(|e| {
            Fail::die(format!(
                "island {}'s server served no envelope: {e}",
                island.word()
            ))
        })?;
        Ok(Some(Head {
            seq: env.seq,
            digest: hex(&env.digest()),
            parents: env.parents.len(),
        }))
    }

    /// Put island A's head on island B's server where B's is not it: setting up island B's
    /// server before the cut, as whoever set it up would, under either publish.
    fn seed_b(&self) -> Result<(), Fail> {
        let a = aterm_secrets::transport::fetch_within(&self.url(Island::A), None, HANG)
            .map_err(|e| Fail::die(format!("island A's head: {e}")))?;
        let aterm_secrets::transport::Fetched::Body(bytes) = a else {
            return Err(Fail::die("island A's server served no vault"));
        };
        let wanted = aterm_secrets::envelope::parse(&bytes)
            .map(|e| hex(&e.digest()))
            .map_err(|e| Fail::die(format!("island A's head: {e}")))?;
        if self.served(Island::B)?.is_some_and(|h| h.digest == wanted) {
            return Ok(());
        }
        aterm_secrets::transport::publish(
            &aterm_secrets::transport::Source::Url(self.url(Island::B)),
            &bytes,
        )
        .map_err(|e| Fail::die(format!("seeding island B's server: {e}")))
    }

    /// Wait (the hang detector) until `mac` reads `in`.
    fn await_in(&self, mac: &str, sync: bool) -> Result<(), Fail> {
        let end = Instant::now() + HANG;
        loop {
            if sync {
                let _ = self.aterm(mac, &["secrets", "sync", "--diagnostic"], None);
            }
            if self.held(mac).membership == "in" {
                return Ok(());
            }
            if Instant::now() >= end {
                return Err(Fail::die(format!(
                    "{mac} was not let in within {HANG:?} (it reads {}); its agent: {}",
                    self.held(mac).membership,
                    one_line(&world::tail(&self.root(mac).join("agent.log"), 4))
                )));
            }
            stop::sleep(Duration::from_millis(500))?;
        }
    }

    /// A person's Allow on `member` for `mac`'s request, and the phrase typed into `mac`
    /// (`docs/DESIGN-fluid-swarm-2026-10-28.md` F10): an open vault lets in no machine by its
    /// asking alone. A lab instance shows no sheet to press, so the members desk and the phrase's
    /// run are F10's own code (`aterm_secrets::members`), run in this process on both Macs' own
    /// custody as the Commons plane is; the member then posts the request it allowed, carrying
    /// its `Allowed`, to its own server as its window does, and the island's approver admits it.
    /// `Err` names what stopped it.
    fn allow(&self, member: &str, mac: &str) -> Result<(), String> {
        use aterm_secrets::client::{self, Paths};
        use aterm_secrets::commons::bootstrap::Device;
        use aterm_secrets::members::{desk, joiner};
        use aterm_types::MutexExt as _;
        use std::os::unix::net::UnixStream;
        let (by, paths) = (
            Paths::for_home(&self.home(member)),
            Paths::for_home(&self.home(mac)),
        );
        let local = desk::Local::of(&by)
            .map_err(|e| format!("{member}'s Commons state: {e}"))?
            .ok_or_else(|| format!("{member} holds no Commons control"))?;
        if !local.can_allow() {
            return Err(format!("no Commons roster binds {member}"));
        }
        let desk = std::sync::Mutex::new(desk::Desk::new(local));
        let device = Device::open(&paths)
            .map_err(|e| format!("{mac}'s device: {e}"))?
            .ok_or_else(|| format!("{mac} holds no identity"))?;
        let joining = joiner::Joining {
            device: &device,
            node: [0; 32],
            name: mac.into(),
            model: "Mac15,3".into(),
        };
        let bundle = |handing: &desk::Handing<'_>| client::hand_over(&by, handing);
        // One connection to the member's desk, each end under the hang detector.
        let connect = |speak: &dyn Fn(&mut UnixStream) -> Result<(), String>| {
            let (mut ours, mut theirs) = UnixStream::pair().map_err(|e| e.to_string())?;
            for end in [&ours, &theirs] {
                end.set_read_timeout(Some(HANG))
                    .map_err(|e| e.to_string())?;
            }
            std::thread::scope(|scope| {
                let served = scope.spawn(|| {
                    desk::serve(&desk, &mut ours, [127, 0, 0, 1].into(), &bundle)
                        .map_err(|e| format!("{member}'s desk: {e}"))
                });
                let spoke = speak(&mut theirs);
                drop(theirs);
                let served = served
                    .join()
                    .unwrap_or_else(|_| Err(format!("{member}'s desk panicked")));
                spoke.and(served)
            })
        };
        connect(&|stream| {
            let greeting = joiner::greeting(stream).map_err(|e| e.to_string())?;
            joiner::ask(stream, &greeting, &joining)
                .map(drop)
                .map_err(|e| e.to_string())
        })?;
        let words = {
            let mut desk = desk.lock_or_recover();
            let now = aterm_time::Instant::now();
            let row = desk
                .rows(now)
                .first()
                .map(|row| row.id)
                .ok_or_else(|| format!("{member}'s desk shows no request from {mac}"))?;
            let phrase = desk.allow(row, now).map_err(|e| e.to_string())?;
            desk.words(phrase, now)
                .ok_or_else(|| format!("{member}'s phrase ran out before it was read"))?
                .join(" ")
        };
        let phrase = aterm_secrets::phrase::Phrase::parse(&words).map_err(|e| e.to_string())?;
        let let_in = connect(&|stream| {
            let greeting = joiner::greeting(stream).map_err(|e| e.to_string())?;
            joiner::run(stream, &greeting, &joining, &phrase)
                .map(drop)
                .map_err(|e| e.to_string())
        })?
        .ok_or_else(|| format!("{member}'s desk sent {mac} no bundle"))?;
        client::post_allowed(&by, &let_in.allowed)
            .map_err(|e| format!("{member} posting the request it allowed: {e}"))
    }

    /// Wait (the hang detector) until both servers and every member hold one head.
    fn await_one_head(&self, macs: &[&str]) -> Result<Head, Fail> {
        let end = Instant::now() + HANG;
        loop {
            let a = self.served(Island::A)?;
            let b = self.served(Island::B)?;
            if let (Some(a), Some(b)) = (&a, &b)
                && a.digest == b.digest
                && macs.iter().all(|m| {
                    let held = self.held(m);
                    held.membership == "in"
                        && held.head.as_ref().is_some_and(|h| h.digest == a.digest)
                })
            {
                return Ok(a.clone());
            }
            if Instant::now() >= end {
                let row: Vec<String> = macs
                    .iter()
                    .map(|m| format!("{m} {}", short(self.held(m).head.as_ref())))
                    .collect();
                return Err(Fail::die(format!(
                    "no one head before the cut within {HANG:?}: server A {}, server B {}, {}",
                    short(a.as_ref()),
                    short(b.as_ref()),
                    row.join(", ")
                )));
            }
            stop::sleep(Duration::from_millis(250))?;
        }
    }

    /// The boot session's sid of `mac`'s instance.
    fn boot_sid(&self, mac: &str) -> Result<String, Fail> {
        let inst = &self.world.insts[self.insts[mac]];
        let body = self.world.ctl_ok(inst, &["sessions"]).map_err(Fail::die)?;
        body.lines()
            .find(|l| l.starts_with(|c: char| c.is_ascii_digit()))
            .and_then(|row| row.split_whitespace().nth(1))
            .map(str::to_owned)
            .ok_or_else(|| Fail::die(format!("{mac}: no boot session: {}", one_line(&body))))
    }

    /// Start `mac`'s headless instance.
    fn start_instance(&mut self, mac: &str) -> Result<(), Fail> {
        let s = spec(mac);
        let inst = self
            .world
            .start(s.slot, self.fleet_ports[mac])
            .map_err(Fail::die)?;
        self.world.insts.push(inst);
        self.insts.insert(s.name, self.world.insts.len() - 1);
        // Its boot session, which the world's status reads (`fleet_word`) and the drives name.
        let sid = self.boot_sid(mac)?;
        if let Some(inst) = self.world.insts.last_mut() {
            inst.sessions.push(world::Sess {
                sid: sid.clone(),
                kind: world::Kind::Shell,
                n: 0,
            });
        }
        self.sids.insert(s.name, sid);
        Ok(())
    }

    /// `aterm fleet join` for `mac`, dialling the member `at`: whether it joined and listens, and
    /// its words. A code names no member since F17, and a lab's worlds neither advertise nor
    /// browse `_alab._tcp`, so `--at` stands in for the advert a Mac on a LAN hears.
    fn fleet_join(&self, mac: &str, at: &str) -> (bool, String) {
        let port = self.fleet_ports[mac].to_string();
        let member = format!("127.0.0.1:{}", self.fleet_ports[at]);
        let out = self.aterm(
            mac,
            &[
                "fleet",
                "join",
                "--host",
                "127.0.0.1",
                "--port",
                &port,
                "--allow",
                "content",
                "--at",
                &member,
            ],
            Some(self.code.as_str()),
        );
        if out.code != 0 {
            return (false, one_line(&format!("{} {}", out.stdout, out.stderr)));
        }
        let inst = &self.world.insts[self.insts[mac]];
        match self.world.await_listening(inst) {
            Ok(()) => (true, String::new()),
            Err(e) => (false, one_line(&e)),
        }
    }

    /// `aterm ctl survey --fleet` from `mac`: the Macs whose block answered.
    fn survey(&self, mac: &str, phase: Phase) -> SurveyRead {
        let out = self.aterm(mac, &["ctl", "survey", "--fleet"], None);
        let pids = crate::fleet_judge::header_pids(&out.stdout);
        let mut answered = Vec::new();
        let mut silent = Vec::new();
        for (m, i) in &self.insts {
            let inst = &self.world.insts[*i];
            if pids.contains(&inst.pid) {
                answered.push((*m).to_owned());
                continue;
            }
            let rows = crate::fleet_judge::unanswered_rows(&out.stdout, inst.port);
            silent.push(match rows.first() {
                Some(row) => format!("{m}: {}", one_line(row.trim_start_matches("== "))),
                None if out.code != 0 => format!(
                    "{m}: the survey exited {}: {}",
                    out.code,
                    one_line(&out.stderr)
                ),
                None => format!("{m}: not in the survey"),
            });
        }
        SurveyRead {
            phase,
            from: mac.to_owned(),
            answered,
            silent,
        }
    }

    /// How old `mac`'s copy of the issuer's book is.
    fn copy_age(&self, mac: &str) -> Option<Duration> {
        let file = self.root(mac).join("state").join("fleet-issuer-book");
        std::fs::metadata(file)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
    }

    /// Which of `members` (other than `mac`) `mac` binds to no key or to another than its own
    /// node key, as its book and any copy it holds bind them; `None` when it binds them all.
    fn unbound(&self, mac: &str, members: &[&str]) -> Option<String> {
        let keys = aterm_net::fleet_member::Keyring::read(&self.root(mac).join("state"));
        let missing: Vec<&str> = members
            .iter()
            .copied()
            .filter(|member| *member != mac)
            .filter(|member| {
                aterm_net::fleet_member::read_node_identity(&self.root(member).join("state"))
                    .ok()
                    .flatten()
                    .is_none_or(|id| keys.bound(id.node()) != Some(id.key()))
            })
            .collect();
        (!missing.is_empty()).then(|| format!("{mac} binds no key for {}", missing.join(", ")))
    }

    /// `aterm ctl --host 127.0.0.1:<to's port> @<to's boot sid> status` from `from`.
    fn drive(&self, from: &str, to: &str, phase: Phase) -> DriveTry {
        let host = format!("127.0.0.1:{}", self.fleet_ports[to]);
        let at = format!("@{}", self.sids.get(to).map_or("", String::as_str));
        let copy_age_s = self.copy_age(to).map(|d| d.as_secs());
        let out = self.aterm(from, &["ctl", "--host", &host, &at, "status"], None);
        DriveTry {
            phase,
            from: from.to_owned(),
            to: to.to_owned(),
            ok: out.code == 0,
            words: if out.code == 0 {
                String::new()
            } else {
                one_line(&format!("{} {}", out.stdout, out.stderr))
            },
            copy_age_s,
        }
    }

    /// Each member's `seen_ms` in the issuer's book, by Mac.
    fn seen(&self) -> BTreeMap<&'static str, u64> {
        let state = self.root("a1").join("state");
        let book = aterm_net::fleet_member::read_peers(&state).unwrap_or_default();
        let mut out = BTreeMap::new();
        for (mac, port) in &self.fleet_ports {
            if let Some(seen) = book
                .iter()
                .find(|p| p.port == *port && p.via == aterm_net::fleet_member::Via::Joined)
                .and_then(|p| p.seen_ms)
            {
                out.insert(*mac, seen);
            }
        }
        out
    }

    /// Whether `mac`'s `~/.secrets/<name>` holds the fixture `name` was set to.
    fn holds_value(&self, mac: &str, name: &str) -> bool {
        let file = self.home(mac).join(".secrets").join(name);
        match (std::fs::read(file), self.values.get(name)) {
            (Ok(bytes), Some(want)) => bytes == *want,
            _ => false,
        }
    }

    /// The converge runner starts `mac`'s subscriber made: (before the cut, during, after).
    fn runners(&self, mac: &str, cut_ms: u64, heal_ms: u64, t0_ms: u64) -> [usize; 3] {
        let text = std::fs::read_to_string(self.home(mac).join(RUNNERS_FILE)).unwrap_or_default();
        let mut out = [0; 3];
        for line in text.lines() {
            let Some(at) = line
                .split_whitespace()
                .next()
                .and_then(|w| w.parse::<u64>().ok())
            else {
                continue;
            };
            let rel = at.saturating_sub(t0_ms);
            let i = if rel < cut_ms {
                0
            } else if rel < heal_ms {
                1
            } else {
                2
            };
            out[i] += 1;
        }
        out
    }

    /// Write a fixture value for `name` under `mac`'s world and remember it.
    fn fixture(&mut self, mac: &str, name: &str) -> Result<PathBuf, Fail> {
        let value = format!(
            "f8-fixture-{name}-{}\n",
            aterm_uds::rand::hex_token::<8>().map_err(|e| Fail::die(e.to_string()))?
        );
        let file = self.root(mac).join("tmp").join(name);
        write_whole(&file, value.as_bytes(), 0o600)
            .map_err(|e| Fail::die(format!("{name}'s fixture: {e}")))?;
        self.values.insert(name.to_owned(), value.into_bytes());
        Ok(file)
    }

    /// Write both cut files: `cut` lists the other island's ports in each, else nothing.
    fn set_cut(&self, cut: bool) -> Result<(), Fail> {
        for island in [Island::A, Island::B] {
            let mut text = String::new();
            if cut {
                let _ = writeln!(text, "# island {}'s view: the other island", island.word());
                for s in MACS.iter().filter(|s| s.island != island) {
                    if let Some(port) = self.fleet_ports.get(s.name) {
                        let _ = writeln!(text, "127.0.0.1:{port}");
                    }
                }
                let other = self.vault_ports[usize::from(island == Island::A)];
                let _ = writeln!(text, "127.0.0.1:{other}");
            }
            let file = &self.cut_files[usize::from(island == Island::B)];
            write_whole(file, text.as_bytes(), 0o644)
                .map_err(|e| Fail::die(format!("the cut file {}: {e}", file.display())))?;
        }
        Ok(())
    }
}

/// Write `bytes` to `path` whole: a sibling `.new` made with `mode`, renamed over it, so a
/// reader (each connect reads the cut file) sees the old text or the new, never a part. The
/// lane's own scratch base sits under `/tmp`, a link the lab's guarded `sys::put` refuses.
fn write_whole(path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let new = path.with_extension("new");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(mode)
        .open(&new)?;
    file.write_all(bytes)?;
    drop(file);
    std::fs::rename(&new, path)
}

/// Bring the Macs up, form the fleet, found the vault and admit every Mac.
fn bring_up(aterm: &Path, opts: &Opts) -> Result<Lane, Fail> {
    let mut world = World::new(aterm, opts.keep).map_err(Fail::die)?;
    let ports = world::free_ports(8).map_err(|e| Fail::die(format!("free ports: {e}")))?;
    // Never a live service's port on this Mac: the fleet's 7100 or the vault's 7484.
    let ports: Vec<u16> = ports
        .into_iter()
        .filter(|p| *p != aterm_net::fleet_member::DEFAULT_PORT && *p != 7484)
        .collect();
    if ports.len() < 7 {
        return Err(Fail::die("too few free loopback ports"));
    }
    let cut_files = [world.base.join("cut-a"), world.base.join("cut-b")];
    let mut fleet_ports = BTreeMap::new();
    for (s, port) in MACS.iter().filter(|s| s.instance).zip(&ports) {
        fleet_ports.insert(s.name, *port);
    }
    for s in &MACS {
        let root = world.root_of(s.slot);
        world.extra.insert(
            root,
            vec![
                (
                    aterm_uds::partition::SEAM.to_owned(),
                    cut_files[usize::from(s.island == Island::B)]
                        .to_string_lossy()
                        .into_owned(),
                ),
                (
                    "ATERM_DEBUG_FLEET_HEARTBEAT_MS".to_owned(),
                    HEARTBEAT_MS.to_owned(),
                ),
            ],
        );
    }
    let mut lane = Lane {
        world,
        opts: opts.clone(),
        t0: Instant::now(),
        fleet_ports,
        vault_ports: [ports[5], ports[6]],
        cut_files,
        insts: BTreeMap::new(),
        sids: BTreeMap::new(),
        signer: String::new(),
        code: String::new(),
        lifelines: Vec::new(),
        kids: Vec::new(),
        values: BTreeMap::new(),
        said: RefCell::new(Vec::new()),
    };
    lane.set_cut(false)?;
    lane.say(&format!(
        "worlds under {}; the aterm under test is {}",
        lane.world.base.display(),
        lane.world
            .aterm
            .strip_prefix(&crate::paths::paths().repo)
            .unwrap_or(&lane.world.aterm)
            .display()
    ));
    for s in MACS.iter().filter(|s| !s.instance) {
        world::make_world(&lane.world.root_of(s.slot))
            .map_err(|e| Fail::die(format!("{}: its world: {e}", s.name)))?;
    }
    for s in MACS.iter().filter(|s| s.instance && !s.late) {
        lane.start_instance(s.name)?;
        stop::check()?;
    }
    lane.say("a1, a2, b1 and b2 run headless");
    form_fleet(&mut lane)?;
    found_vault(&mut lane)?;
    Ok(lane)
}

fn form_fleet(lane: &mut Lane) -> Result<(), Fail> {
    let port = lane.fleet_ports["a1"].to_string();
    lane.aterm_ok(
        "a1",
        &[
            "fleet",
            "init",
            "--host",
            "127.0.0.1",
            "--port",
            &port,
            "--allow",
            "content",
        ],
        None,
    )?;
    let issuer = &lane.world.insts[lane.insts["a1"]];
    lane.world.await_listening(issuer).map_err(Fail::die)?;
    lane.code = std::fs::read_to_string(lane.root("a1").join("state/fleet-join-code"))
        .map_err(|e| Fail::die(format!("the fleet's join code: {e}")))?;
    for mac in ["a2", "b1", "b2"] {
        let (ok, words) = lane.fleet_join(mac, "a1");
        if !ok {
            return Err(Fail::die(format!("{mac}: fleet join: {words}")));
        }
        stop::check()?;
    }
    for mac in ["a1", "a2", "b1", "b2"] {
        lane.aterm_ok(mac, &["fleet", "allow", "drive", "--all"], None)?;
    }
    lane.say(
        "fleet formed: a1 minted its secret; a2, b1 and b2 joined; every member allows drive --all",
    );
    Ok(())
}

fn found_vault(lane: &mut Lane) -> Result<(), Fail> {
    lane.start_server("a1")?;
    lane.start_server("b1")?;
    let url = lane.url(Island::A);
    let out = lane.aterm_ok(
        "a1",
        &["secrets", "create", &url, "--name", "a1", "--no-agent"],
        None,
    )?;
    lane.signer = out
        .lines()
        .find_map(|l| l.strip_prefix("signer: "))
        .and_then(|rest| rest.split_whitespace().next())
        .map(str::to_owned)
        .ok_or_else(|| Fail::die(format!("a1's create printed no signer: {}", one_line(&out))))?;
    lane.start_agent("a1")?;
    lane.say(&format!(
        "a1 founded vault {VAULT} on island A's server; its agent runs"
    ));
    if lane.opts.publish == Publish::Policy {
        let policy = format!(
            "servers = [\"{}\", \"{}\"]\n",
            lane.url(Island::A),
            lane.url(Island::B)
        );
        let file = lane.root("a1").join("tmp/aterm-swarm-policy.toml");
        std::fs::write(&file, policy).map_err(|e| Fail::die(e.to_string()))?;
        let file = file.to_string_lossy().into_owned();
        lane.aterm_ok(
            "a1",
            &[
                "secrets",
                "set",
                aterm_secrets::SWARM_POLICY_ENTRY,
                "--file",
                &file,
            ],
            None,
        )?;
        lane.say("a1 set aterm-swarm-policy: servers = [island A's, island B's]");
    } else {
        lane.say(
            "--one-url: no aterm-swarm-policy, so a publish reaches the publisher's server alone",
        );
    }
    let pinned_a = lane.pinned(Island::A);
    for mac in ["a2", "b1", "b2"] {
        lane.aterm_ok(
            mac,
            &[
                "secrets",
                "join",
                &pinned_a,
                "--name",
                mac,
                "--no-agent",
                "--no-wait",
            ],
            None,
        )?;
    }
    for mac in ["a2", "b1", "b2"] {
        lane.allow("a1", mac)
            .map_err(|e| Fail::die(format!("a1 allows {mac}: {e}")))?;
    }
    for mac in ["a2", "b1", "b2"] {
        lane.await_in(mac, true)?;
    }
    lane.say("a1 allowed a2, b1 and b2; a1's approver let them in (asked at island A's server)");
    lane.seed_b()?;
    let pinned_b = lane.pinned(Island::B);
    for mac in ["b1", "b2"] {
        lane.aterm_ok(
            mac,
            &[
                "secrets",
                "join",
                &pinned_b,
                "--name",
                mac,
                "--no-agent",
                "--no-wait",
            ],
            None,
        )?;
        lane.await_in(mac, false)?;
    }
    lane.say("b1 and b2 now follow island B's server");
    for mac in ["a2", "b1", "b2"] {
        lane.start_agent(mac)?;
    }
    if lane.opts.writers == Writers::Both {
        lane.aterm_ok("a1", &["secrets", "admins", "add", "b1"], None)?;
        lane.say("a1 made b1 an admin, so island B can write and admit");
    }
    lane.seed_b()?;
    let head = lane.await_one_head(&["a1", "a2", "b1", "b2"])?;
    lane.say(&format!(
        "one head before the cut: {} on both servers and all four Macs",
        short(Some(&head))
    ));
    Ok(())
}

/// The cut, the work on both sides, the heal, and what was seen.
#[allow(clippy::too_many_lines)]
fn run(lane: &mut Lane) -> Result<Evidence, Fail> {
    let members_before = ["a1", "a2", "b1", "b2"];
    let mut v = VaultEvidence {
        before: Some(lane.await_one_head(&members_before)?),
        members_before: members_before.map(str::to_owned).to_vec(),
        ..VaultEvidence::default()
    };
    let mut f = FleetEvidence::default();
    let t0_ms = u64::try_from(crate::util::now_ms()).unwrap_or(0) - ms_since(lane.t0);
    let mut load = Vec::new();
    let mut mark = |at: &str| {
        if let Ok(l) = crate::aos_workers_bench::load_average() {
            load.push((at.to_owned(), l));
        }
    };
    mark("start");

    let mut plane = commons::Plane {
        macs: MACS
            .iter()
            .map(|s| commons::Mac {
                name: s.name.to_owned(),
                paths: aterm_secrets::client::Paths::for_home(&lane.home(s.name)),
                config: lane.root(s.name).join("tmp/commons.toml"),
            })
            .collect(),
        ev: commons::Evidence::default(),
    };
    plane.before();
    lane.say(&match &plane.ev.skipped {
        Some(why) => format!("Commons: not run: {why}"),
        None => format!(
            "Commons: {} enrolled with a1, which declared the lane's table{}",
            plane.ev.before.join(", "),
            if plane.ev.failed_exchanges.is_empty() {
                String::new()
            } else {
                format!("; failed: {}", plane.ev.failed_exchanges.join("; "))
            }
        ),
    });

    // The cut must split a whole fleet: every survey hears every member, a drive inside each
    // island and one across are admitted, and b1 and b2, the members that must outlast a1, each
    // bind every member's key (by its own book since F17, which a heartbeat fills from a
    // member's; before it, by a copy of a1's book, fetched when it surveyed or when a driver it
    // must bind dialled it). A fleet still settling would make the run measure bring-up, so this
    // waits for it within the hang detector, and a fleet never whole is a run that could not run.
    let settle = Instant::now();
    let pairs = [("a1", "a2"), ("b1", "b2"), ("a1", "b1")];
    let mut admitted: BTreeMap<(&str, &str), DriveTry> = BTreeMap::new();
    loop {
        stop::check()?;
        let surveys: Vec<SurveyRead> = ["a1", "b1"]
            .iter()
            .map(|mac| lane.survey(mac, Phase::Before))
            .collect();
        let mut refused = Vec::new();
        for (from, to) in pairs {
            if admitted.contains_key(&(from, to)) {
                continue;
            }
            let d = lane.drive(from, to, Phase::Before);
            if d.ok {
                admitted.insert((from, to), d);
            } else {
                refused.push(format!("{from} -> {to}: {}", d.words));
            }
        }
        let unbound: Vec<String> = ["b1", "b2"]
            .into_iter()
            .filter_map(|mac| lane.unbound(mac, &["a1", "a2", "b1", "b2"]))
            .collect();
        let deaf: Vec<String> = surveys
            .iter()
            .filter(|s| {
                ["a1", "a2", "b1", "b2"]
                    .iter()
                    .any(|m| !s.answered.iter().any(|a| a == m))
            })
            .map(|s| {
                format!(
                    "{} hears {}{}",
                    s.from,
                    names(&s.answered),
                    if s.silent.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", s.silent.join("; "))
                    }
                )
            })
            .collect();
        if deaf.is_empty() && refused.is_empty() && unbound.is_empty() {
            f.surveys.extend(surveys);
            break;
        }
        if settle.elapsed() >= HANG {
            let mut why = deaf;
            why.extend(refused.into_iter().map(|r| format!("drive {r}")));
            why.extend(unbound);
            return Err(Fail::exit(
                3,
                format!(
                    "the fleet was not whole within {}s, so there was no whole fleet to cut: {}{}",
                    HANG.as_secs(),
                    why.join("; "),
                    crate::aos_workers_bench::load_average().map_or(String::new(), |l| format!(
                        " (this Mac's 1-minute load {l:.1} on {} cores)",
                        std::thread::available_parallelism().map_or(0, std::num::NonZero::get)
                    ))
                ),
            ));
        }
        stop::sleep(Duration::from_millis(500))?;
    }
    f.drives
        .extend(pairs.iter().filter_map(|p| admitted.remove(p)));
    for s in f.surveys.iter().filter(|s| s.phase == Phase::Before) {
        lane.say(&format!("before: {} hears {}", s.from, names(&s.answered)));
    }

    // The cut.
    lane.set_cut(true)?;
    let cut = Instant::now();
    mark("cut");
    let cut_ms = ms_since(lane.t0);
    lane.say("CUT: island A (a1, a2, a3) and island B (b1, b2, b3) cannot reach each other");

    for mac in ["a1", "b1"] {
        let s = lane.survey(mac, Phase::EarlyCut);
        lane.say(&format!(
            "in the cut: {} hears {}",
            s.from,
            names(&s.answered)
        ));
        f.surveys.push(s);
    }
    for (from, to) in [("a1", "a2"), ("b1", "b2"), ("a1", "b1")] {
        let d = lane.drive(from, to, Phase::EarlyCut);
        lane.say(&format!(
            "in the cut: drive {from} -> {to}: {}",
            if d.ok {
                "admitted".to_owned()
            } else {
                format!("refused: {}", d.words)
            }
        ));
        f.drives.push(d);
    }

    // The issuer's book now: a heartbeat in flight when the cut fell has landed by the time
    // the early surveys and drives are done, so a later one crossed the cut.
    let seen_at_cut = lane.seen();

    // Each side writes.
    let mut sets = vec![("a1", "f8-a-1"), ("a1", "f8-a-2"), ("b1", "f8-b-1")];
    if lane.opts.writers == Writers::A {
        sets.retain(|(by, _)| *by == "a1");
        sets.push(("b1", "f8-b-1"));
    }
    for (by, name) in sets {
        let file = lane.fixture(by, name)?;
        let file = file.to_string_lossy().into_owned();
        let out = lane.aterm(by, &["secrets", "set", name, "--file", &file], None);
        let t = SetTry {
            name: name.to_owned(),
            island: spec(by).island,
            by: by.to_owned(),
            ok: out.code == 0,
            words: if out.code == 0 {
                String::new()
            } else {
                one_line(&format!("{} {}", out.stdout, out.stderr))
            },
        };
        lane.say(&format!(
            "in the cut: {by} sets {name}: {}",
            if t.ok {
                "published".to_owned()
            } else {
                format!("refused: {}", t.words)
            }
        ));
        v.sets.push(t);
    }

    // A newcomer asks on each side; b3 also asks to join the fleet.
    for (mac, island) in [("a3", Island::A), ("b3", Island::B)] {
        if spec(mac).instance {
            lane.start_instance(mac)?;
        }
        let pinned = lane.pinned(island);
        let out = lane.aterm(
            mac,
            &[
                "secrets",
                "join",
                &pinned,
                "--name",
                mac,
                "--no-agent",
                "--no-wait",
            ],
            None,
        );
        let membership = lane.held(mac).membership;
        let posted = matches!(membership.as_str(), "pending" | "in");
        lane.say(&format!(
            "in the cut: {mac} asks to join at island {}'s server: {}",
            island.word(),
            if posted {
                membership.clone()
            } else {
                format!(
                    "not posted: {}",
                    one_line(&format!("{} {}", out.stdout, out.stderr))
                )
            }
        ));
        if posted {
            let member = if island == Island::A { "a1" } else { "b1" };
            let allowed = lane.allow(member, mac);
            lane.say(&format!(
                "in the cut: {member} allows {mac}: {}",
                allowed.map_or_else(|e| format!("not allowed: {e}"), |()| "posted".to_owned())
            ));
            lane.start_agent(mac)?;
        }
        v.joins.push(JoinTry {
            mac: mac.to_owned(),
            island,
            posted,
            in_during_cut_ms: None,
        });
    }
    let (ok, words) = lane.fleet_join("b3", "b1");
    lane.say(&format!(
        "in the cut: b3 fleet join: {}",
        if ok {
            "joined".to_owned()
        } else {
            format!("refused: {words}")
        }
    ));
    f.joins.push(FleetJoin {
        phase: Phase::EarlyCut,
        ok,
        words,
    });

    // Hold the cut: watch the newcomers, and drive late once the copies are past the window.
    let heal_at = cut + lane.opts.hold;
    let mut late_done = false;
    let mut commons_done = false;
    if lane.opts.hold < COPY_WINDOW + Duration::from_secs(3) {
        f.late_skipped = Some(format!(
            "--hold {}s is under the fleet's {}s copy window",
            lane.opts.hold.as_secs(),
            COPY_WINDOW.as_secs()
        ));
        late_done = true;
    }
    while Instant::now() < heal_at {
        stop::check()?;
        for j in &mut v.joins {
            if j.in_during_cut_ms.is_none() && lane.held(&j.mac).membership == "in" {
                j.in_during_cut_ms = Some(ms_since(cut));
                lane.say(&format!("in the cut: {} is in", j.mac));
            }
        }
        if !commons_done
            && v.joins
                .iter()
                .all(|j| !j.posted || j.in_during_cut_ms.is_some())
        {
            commons_in_cut(lane, &mut plane, &v);
            commons_done = true;
        }
        // Island B's copies of the issuer's book stopped refreshing at the cut; once the cut
        // outlasts the window they bind a drive for, drive on both sides (island A's is the
        // control: its issuer is still there).
        if !late_done && cut.elapsed() >= COPY_WINDOW {
            for (from, to) in [("a1", "a2"), ("b1", "b2")] {
                let d = lane.drive(from, to, Phase::LateCut);
                lane.say(&format!(
                    "late in the cut: drive {from} -> {to} ({}s copy): {}",
                    d.copy_age_s.unwrap_or(0),
                    if d.ok {
                        "admitted".to_owned()
                    } else {
                        format!("refused: {}", d.words)
                    }
                ));
                f.drives.push(d);
            }
            late_done = true;
        }
        stop::sleep(Duration::from_millis(500))?;
    }
    if !commons_done {
        commons_in_cut(lane, &mut plane, &v);
    }
    let seen_at_heal = lane.seen();
    for (mac, island) in [("a2", Island::A), ("b1", Island::B), ("b2", Island::B)] {
        f.heartbeats.push(Heartbeat {
            mac: mac.to_owned(),
            island,
            advanced_in_cut: seen_at_heal.get(mac) > seen_at_cut.get(mac),
            back_ms: None,
        });
    }
    for island in [Island::A, Island::B] {
        let macs: Vec<(String, Option<Head>)> = MACS
            .iter()
            .filter(|s| s.island == island)
            .filter(|s| lane.held(s.name).membership == "in")
            .map(|s| (s.name.to_owned(), lane.held(s.name).head))
            .collect();
        let crossed = macs
            .iter()
            .flat_map(|(m, _)| {
                v.sets
                    .iter()
                    .filter(|s| s.ok && s.island != island && lane.holds_value(m, &s.name))
                    .map(move |s| {
                        format!(
                            "{m} held {}, which island {} set during the cut",
                            s.name,
                            s.island.word()
                        )
                    })
            })
            .collect();
        v.end_of_cut.push(IslandView {
            island,
            server: lane.served(island)?,
            macs,
            crossed,
        });
    }
    for view in &v.end_of_cut {
        lane.say(&format!(
            "the cut's end: island {} serves {}",
            view.island.word(),
            short(view.server.as_ref())
        ));
    }

    // The heal.
    lane.set_cut(false)?;
    let heal = Instant::now();
    mark("heal");
    let heal_ms = ms_since(lane.t0);
    let heal_unix = t0_ms + heal_ms;
    let cut_for_ms = ms_since(cut);
    lane.say(&format!(
        "HEAL: both islands reach each other again, after {:.1}s of cut",
        secs(cut_for_ms)
    ));

    for (from, to) in [("b1", "b2"), ("a1", "b1")] {
        let d = lane.drive(from, to, Phase::Healed);
        lane.say(&format!(
            "healed: drive {from} -> {to}: {}",
            if d.ok {
                "admitted".to_owned()
            } else {
                format!("refused: {}", d.words)
            }
        ));
        f.drives.push(d);
    }
    if !f.joins.iter().any(|j| j.ok) {
        let (ok, words) = lane.fleet_join("b3", "b1");
        lane.say(&format!(
            "healed: b3 fleet join again: {}",
            if ok {
                "joined".to_owned()
            } else {
                format!("refused: {words}")
            }
        ));
        f.joins.push(FleetJoin {
            phase: Phase::Healed,
            ok,
            words,
        });
    }

    // Vault: wait for one head on both servers and every member; meanwhile, heartbeats.
    let mut samples: BTreeMap<&'static str, Vec<(u64, String)>> = BTreeMap::new();
    let end = heal + HANG;
    let mut stable_since: Option<(Instant, String)> = None;
    let expected = expected_members(&v);
    loop {
        stop::check()?;
        let now_ms = ms_since(heal);
        let seen = lane.seen();
        for h in &mut f.heartbeats {
            if h.back_ms.is_none() && seen.get(h.mac.as_str()).is_some_and(|s| *s > heal_unix) {
                h.back_ms = Some(now_ms);
            }
        }
        if v.healed.is_none() {
            let a = lane.served(Island::A)?;
            let b = lane.served(Island::B)?;
            let mut all_one = matches!((&a, &b), (Some(a), Some(b)) if a.digest == b.digest);
            for s in &MACS {
                if !expected.iter().any(|e| e == s.name) {
                    continue;
                }
                let held = lane.held(s.name);
                if held.membership != "in" {
                    all_one = false;
                }
                let digest = held
                    .head
                    .as_ref()
                    .map_or_else(String::new, |h| h.digest.clone());
                if a.as_ref().is_none_or(|a| a.digest != digest) {
                    all_one = false;
                }
                samples.entry(s.name).or_default().push((now_ms, digest));
            }
            if all_one {
                let digest = a.as_ref().map(|a| a.digest.clone()).unwrap_or_default();
                match &stable_since {
                    // One head held for SETTLE: a reconcile still in flight would have moved it.
                    Some((since, d)) if *d == digest => {
                        if since.elapsed() >= SETTLE {
                            let first = samples
                                .values()
                                .filter_map(|list| {
                                    list.iter().rev().take_while(|(_, d)| *d == digest).last()
                                })
                                .map(|(t, _)| *t)
                                .max()
                                .unwrap_or(now_ms);
                            v.healed = a.clone().map(|h| (first, h));
                        }
                    }
                    _ => stable_since = Some((Instant::now(), digest)),
                }
            } else {
                stable_since = None;
            }
        }
        let heartbeats_back = f.heartbeats.iter().all(|h| h.back_ms.is_some());
        // A one-head window that opened before the hang detector ran out may finish, so a head
        // first held at 58 s still counts as within the minute.
        let now = Instant::now();
        let window_open = v.healed.is_none() && stable_since.is_some();
        if (v.healed.is_some() && heartbeats_back)
            || (now >= end && (!window_open || now >= end + SETTLE))
        {
            break;
        }
        stop::sleep(Duration::from_millis(250))?;
    }
    if let Some((ms, head)) = &v.healed {
        lane.say(&format!(
            "healed: one head {} everywhere, {:.1}s after the heal",
            short(Some(head)),
            secs(*ms)
        ));
    } else {
        lane.say(&format!("healed: no one head within {HANG:?}"));
    }
    v.servers_after = vec![
        (Island::A, lane.served(Island::A)?),
        (Island::B, lane.served(Island::B)?),
    ];
    let final_digest = v.healed.as_ref().map(|(_, h)| h.digest.clone());
    for s in &MACS {
        let held = lane.held(s.name);
        let at_ms = final_digest.as_ref().and_then(|d| {
            samples.get(s.name).and_then(|list| {
                list.iter()
                    .rev()
                    .take_while(|(_, x)| x == d)
                    .last()
                    .map(|(t, _)| *t)
            })
        });
        v.macs_after.push(MacAfter {
            mac: s.name.to_owned(),
            island: s.island,
            membership: held.membership,
            head: held.head,
            at_ms,
        });
    }
    // Every Mac that should be a member is looked at, so one the heal left out is missing
    // every entry.
    let members = expected_members(&v);
    for s in v.sets.iter().filter(|s| s.ok) {
        let (on, missing): (Vec<String>, Vec<String>) = members
            .iter()
            .cloned()
            .partition(|m| lane.holds_value(m, &s.name));
        v.entries_after.push(EntryAfter {
            name: s.name.clone(),
            island: s.island,
            on,
            missing,
        });
    }
    for s in &MACS {
        v.runners.push((
            s.name.to_owned(),
            lane.runners(s.name, cut_ms, heal_ms, t0_ms),
        ));
    }

    // Fleet: the surveys after the heal, until both hear every member the surveys before the
    // cut heard (the hang detector bounds it; the last pair read is the evidence).
    let mut everyone: Vec<String> = f
        .surveys
        .iter()
        .filter(|s| s.phase == Phase::Before)
        .flat_map(|s| s.answered.clone())
        .collect();
    if f.joins.iter().any(|j| j.ok) && !everyone.iter().any(|m| m == "b3") {
        everyone.push("b3".to_owned());
    }
    let looking = Instant::now();
    f.first_read_ms = Some(ms_since(heal));
    let last = loop {
        stop::check()?;
        let pair: Vec<SurveyRead> = ["a1", "b1"]
            .iter()
            .map(|mac| lane.survey(mac, Phase::Healed))
            .collect();
        if pair
            .iter()
            .all(|s| everyone.iter().all(|m| s.answered.contains(m)))
        {
            f.whole_ms = Some(ms_since(heal));
            break pair;
        }
        if looking.elapsed() >= HANG {
            break pair;
        }
        stop::sleep(Duration::from_millis(500))?;
    };
    for s in last {
        lane.say(&format!("healed: {} hears {}", s.from, names(&s.answered)));
        f.surveys.push(s);
    }

    // Commons: exchange across, then repair round a ring of every enrolled Mac and read each.
    plane.after_heal();
    for a in &plane.ev.after {
        lane.say(&format!(
            "healed: Commons on {}: {} ({} heads), reads {}",
            a.mac,
            a.state,
            a.heads.len(),
            a.unreadable
                .as_ref()
                .map_or_else(|| names(&a.rows), |why| format!("nothing ({why})"))
        ));
    }
    let accounts = accounts::run();
    mark("end");
    Ok(Evidence {
        publish: lane.opts.publish,
        writers: lane.opts.writers,
        vault: v,
        fleet: f,
        commons: Some(plane.ev),
        accounts: Some(accounts),
        load,
        cut_ms: cut_for_ms,
    })
}

/// The Commons plane's work in the cut, said on the timeline.
fn commons_in_cut(lane: &Lane, plane: &mut commons::Plane, v: &VaultEvidence) {
    let vault_in: Vec<&str> = v
        .joins
        .iter()
        .filter(|j| j.in_during_cut_ms.is_some())
        .map(|j| j.mac.as_str())
        .collect();
    plane.in_cut(&vault_in);
    for e in &plane.ev.enrolled {
        lane.say(&format!(
            "in the cut: Commons: {} lets {} in: {}",
            e.by,
            e.mac,
            e.error.as_deref().unwrap_or("enrolled")
        ));
    }
    for w in plane.ev.wrote.iter().filter(|w| w.row.ends_with("-in-cut")) {
        lane.say(&format!(
            "in the cut: Commons: {} writes {}: {}",
            w.by,
            w.row,
            w.error.as_deref().unwrap_or("written")
        ));
    }
}

/// The report: what each part did, with timings and losses, then the checks.
#[must_use]
pub fn report(ev: &Evidence, checks: &[Check]) -> String {
    let mut out = String::new();
    let v = &ev.vault;
    let f = &ev.fleet;
    let _ = writeln!(out, "The cut lasted {:.1}s.", secs(ev.cut_ms));
    let _ = writeln!(out);
    if !ev.load.is_empty() {
        let _ = writeln!(
            out,
            "This Mac: {} cores; 1-minute load {}.",
            std::thread::available_parallelism().map_or(0, std::num::NonZero::get),
            ev.load
                .iter()
                .map(|(at, l)| format!("{l:.1} at the {at}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let _ = writeln!(out);
    }
    let _ = writeln!(out, "## Vault");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "- before the cut: {} on both servers and a1, a2, b1, b2",
        short(v.before.as_ref())
    );
    for s in &v.sets {
        let _ = writeln!(
            out,
            "- in the cut, island {}: {} sets `{}`: {}",
            s.island.word(),
            s.by,
            s.name,
            if s.ok {
                "published".to_owned()
            } else {
                format!("refused ({})", s.words)
            }
        );
    }
    for j in &v.joins {
        let _ = writeln!(
            out,
            "- in the cut, island {}: {} asks to join: {}",
            j.island.word(),
            j.mac,
            match (j.posted, j.in_during_cut_ms) {
                (false, _) => "could not ask".to_owned(),
                (true, Some(ms)) => format!("in after {:.1}s", secs(ms)),
                (true, None) => "never let in during the cut".to_owned(),
            }
        );
    }
    for view in &v.end_of_cut {
        let macs: Vec<String> = view
            .macs
            .iter()
            .map(|(m, h)| format!("{m} {}", short(h.as_ref())))
            .collect();
        let _ = writeln!(
            out,
            "- the cut's end, island {}: server {}; {}",
            view.island.word(),
            short(view.server.as_ref()),
            names(&macs)
        );
    }
    match &v.healed {
        Some((ms, head)) => {
            let _ = writeln!(
                out,
                "- after the heal: one head {} ({} parents) on both servers and every member, {:.1}s after the heal",
                short(Some(head)),
                head.parents,
                secs(*ms)
            );
        }
        None => {
            let _ = writeln!(out, "- after the heal: no one head within {HANG:?}");
        }
    }
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "| Mac | island | membership | head at the end | at the final head after the heal | converge runners (before / cut / after) |"
    );
    let _ = writeln!(out, "|---|---|---|---|---|---|");
    for m in &v.macs_after {
        let runs = v
            .runners
            .iter()
            .find(|(mac, _)| *mac == m.mac)
            .map_or("-".to_owned(), |(_, r)| {
                format!("{} / {} / {}", r[0], r[1], r[2])
            });
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {runs} |",
            m.mac,
            m.island.word(),
            m.membership,
            short(m.head.as_ref()),
            m.at_ms
                .map_or("-".to_owned(), |ms| format!("+{:.1}s", secs(ms)))
        );
    }
    let _ = writeln!(out);
    for e in &v.entries_after {
        let _ = writeln!(
            out,
            "- `{}` (island {}): on {}; missing on {}",
            e.name,
            e.island.word(),
            names(&e.on),
            names(&e.missing)
        );
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "## Fleet");
    let _ = writeln!(out);
    for s in &f.surveys {
        let _ = writeln!(
            out,
            "- survey {} from {}: heard {}{}",
            s.phase.word(),
            s.from,
            names(&s.answered),
            if s.silent.is_empty() {
                String::new()
            } else {
                format!("; silent: {}", s.silent.join("; "))
            }
        );
    }
    for d in &f.drives {
        let _ = writeln!(
            out,
            "- drive {} {} -> {}{}: {}",
            d.phase.word(),
            d.from,
            d.to,
            d.copy_age_s
                .map_or(String::new(), |s| format!(" (its copy {s}s old)")),
            if d.ok {
                "admitted".to_owned()
            } else {
                format!("refused ({})", d.words)
            }
        );
    }
    if let Some(why) = &f.late_skipped {
        let _ = writeln!(out, "- late drives not tried: {why}");
    }
    for j in &f.joins {
        let _ = writeln!(
            out,
            "- b3 fleet join {}: {}",
            j.phase.word(),
            if j.ok {
                "joined".to_owned()
            } else {
                format!("refused ({})", j.words)
            }
        );
    }
    for h in &f.heartbeats {
        let _ = writeln!(
            out,
            "- {} (island {}) heartbeat: {} in the cut; back at the issuer {}",
            h.mac,
            h.island.word(),
            if h.advanced_in_cut {
                "reached the issuer"
            } else {
                "did not reach the issuer"
            },
            h.back_ms.map_or("never".to_owned(), |ms| format!(
                "{:.1}s after the heal",
                secs(ms)
            ))
        );
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "## Commons");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "- driven by this lab on each Mac's own vault custody: a lab instance runs no Commons \
         runtime (it returns under ATERM_STATE_HOME), so the lab runs the repair exchange the \
         runtime would, pairing Macs of one island during the cut; after the heal it pairs across, \
         then round a ring of every enrolled Mac until they read alike, a pass moves nothing \
         with every exchange run, or one pass per Mac has run"
    );
    match &ev.commons {
        Some(c) => out.push_str(&commons::report(c)),
        None => {
            let _ = writeln!(out, "- not run");
        }
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "## Accounts");
    let _ = writeln!(out);
    match &ev.accounts {
        Some(a) => out.push_str(&accounts::report(a)),
        None => {
            let _ = writeln!(out, "- not run");
        }
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "## Checks");
    let _ = writeln!(out);
    for c in checks {
        let _ = writeln!(
            out,
            "- [{}] {}: {}",
            if c.pass { "PASS" } else { "FAIL" },
            c.name,
            c.detail
        );
    }
    out
}

/// `targo --unverified lab swarm partition …`: exit 0 when every check held, 1 when one did
/// not, 3 when the lane could not run.
///
/// # Errors
/// The lane could not be brought up or run through (3), or it was interrupted.
pub fn cmd_partition(args: &[String]) -> Res {
    stop::install();
    let opts = parse(args)?;
    let aterm = crate::instance::resolve_aterm(opts.aterm.clone()).map_err(|e| Fail::exit(3, e))?;
    println!(
        "aterm-lab swarm partition: two islands on this Mac's loopback; {}, writers {}, cut held at least {}s",
        match opts.publish {
            Publish::Policy => "publish to every server aterm-swarm-policy lists",
            Publish::OneUrl => "one-URL publish (the negative control)",
        },
        match opts.writers {
            Writers::Both => "a1 and b1",
            Writers::A => "a1 alone",
        },
        opts.hold.as_secs()
    );
    let mut lane = bring_up(&aterm, &opts).map_err(could_not_run)?;
    let ev = run(&mut lane).map_err(could_not_run)?;
    let checks = judge(&ev);
    let text = report(&ev, &checks);
    println!("\n{text}");
    let failed = checks.iter().filter(|c| !c.pass).count();
    let valid = [CUT_HELD, FLEET_CUT_HELD]
        .iter()
        .all(|name| checks.iter().any(|c| c.name == *name && c.pass));
    match write_record(&lane, &text, &checks, opts.record && valid) {
        Ok(path) => println!("written: {path}"),
        Err(e) => println!("could not write the record: {e}"),
    }
    if opts.record && !valid {
        println!(
            "not recorded under docs/measured: the cut did not hold on the vault or the fleet, so the \
             run measured nothing"
        );
    }
    if failed == 0 {
        println!("swarm partition: every check held");
        Ok(0)
    } else {
        println!("swarm partition: {failed}/{} checks failed", checks.len());
        Ok(1)
    }
}

/// A lane that could not be brought up or run through measured nothing: exit 3, not a finding.
fn could_not_run(f: Fail) -> Fail {
    if f.code == 1 {
        Fail { code: 3, ..f }
    } else {
        f
    }
}

/// Write the run's record under `docs/measured/` when `to_docs`, else under the lab's results.
fn write_record(
    lane: &Lane,
    text: &str,
    checks: &[Check],
    to_docs: bool,
) -> Result<String, String> {
    let p = crate::paths::paths();
    let stamp = crate::util::utc_stamp(crate::util::now_secs());
    let date = stamp.get(..10).unwrap_or("undated").to_owned();
    let (arm, flags) = match (lane.opts.publish, lane.opts.writers) {
        (Publish::Policy, Writers::Both) => ("", ""),
        (Publish::OneUrl, Writers::Both) => ("-one-url", " --one-url"),
        (Publish::Policy, Writers::A) => ("-writers-a", " --writers a"),
        (Publish::OneUrl, Writers::A) => ("-one-url-writers-a", " --one-url --writers a"),
    };
    let path = if to_docs {
        p.repo
            .join("docs/measured")
            .join(format!("swarm-partition{arm}-{date}.md"))
    } else {
        p.need_state()?.join("results").join(format!(
            "swarm-partition{arm}-{}.md",
            stamp.get(..19).unwrap_or(&stamp).replace(':', "")
        ))
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let commit = aterm_uds::spawnfd::command("/usr/bin/git")
        .arg("-C")
        .arg(&p.repo)
        .args(["rev-parse", "--short=10", "HEAD"])
        .stdin(Stdio::null())
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default();
    let mut body = String::new();
    let _ = writeln!(body, "<!-- SPDX-License-Identifier: Apache-2.0 -->");
    let _ = writeln!(body, "<!-- Copyright 2026 Andrew Yates -->");
    let _ = writeln!(body);
    let _ = writeln!(
        body,
        "# swarm partition{arm}: two islands on one Mac — {date}"
    );
    let _ = writeln!(body);
    let _ = writeln!(
        body,
        "Written by `targo --unverified lab swarm partition{flags}` (`crates/aterm-lab/src/swarm_partition.rs`) at commit `{commit}`, {} s after it began, at {stamp}. {} of {} checks held.",
        lane.t0.elapsed().as_secs(),
        checks.iter().filter(|c| c.pass).count(),
        checks.len()
    );
    let _ = writeln!(body);
    let _ = writeln!(
        body,
        "The aterm under test is `{}`; every Mac is a private world on this Mac's loopback, and the cut is the `ATERM_DEBUG_PARTITION` seam (`crates/aterm-uds/src/partition.rs`), which both cut checks show it honoured (a run where either leaked is never written here). The fleet heartbeat runs every {HEARTBEAT_MS} ms here (production: 180 s); every other period is production's. The Commons plane is the lab driving production's repair exchange on each Mac's custody; the accounts plane is a model run of main's account claims on two Macs' table replicas on a simulated clock.",
        lane.world
            .aterm
            .strip_prefix(&p.repo)
            .unwrap_or(&lane.world.aterm)
            .display()
    );
    let _ = writeln!(body);
    body.push_str(text);
    let _ = writeln!(body);
    let _ = writeln!(body, "## Timeline");
    let _ = writeln!(body);
    let _ = writeln!(body, "```text");
    for line in lane.said.borrow().iter() {
        let _ = writeln!(body, "{line}");
    }
    let _ = writeln!(body, "```");
    std::fs::write(&path, body).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

// ---------------------------------------------------------------------------------------------
// A Mac's agent and its converge stand-in.
// ---------------------------------------------------------------------------------------------

/// `aterm-lab partition-agent --home <home>`: a Mac's subscriber and approver, the loop of
/// `aterm secrets watch` over the vault under `<home>`. The shell [`Lane::start_kept`] starts it
/// behind ends it once the lane's lifeline closes.
#[must_use]
pub fn agent_main(args: &[String]) -> i32 {
    let home = match args {
        [flag, home] if flag == "--home" => PathBuf::from(home),
        _ => {
            eprintln!("usage: aterm-lab {AGENT_VERB} --home <home>");
            return 2;
        }
    };
    let paths = aterm_secrets::client::Paths::for_home(&home);
    let ended = aterm_secrets::client::watch(&paths, &|line: &str| {
        eprintln!("{} {line}", crate::util::now_ms());
    });
    match ended {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{} the subscriber ended: {e}", crate::util::now_ms());
            1
        }
    }
}

/// The converge runner an agent's subscriber starts (`<this lab> init --converge --trigger
/// <word>`): notes the start under `$HOME` and runs no pass. `None` outside an agent.
#[must_use]
pub fn converge_standin(args: &[String]) -> Option<i32> {
    if std::env::var_os(AGENT_SEAM).is_none_or(|v| v != "1") {
        return None;
    }
    if args.first().map(String::as_str) != Some("--converge") {
        return None;
    }
    let trigger = args
        .iter()
        .skip_while(|a| *a != "--trigger")
        .nth(1)
        .cloned()
        .unwrap_or_default();
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let line = format!("{} {trigger}\n", crate::util::now_ms());
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(home.join(RUNNERS_FILE));
    Some(match file {
        Ok(mut f) => {
            use std::io::Write as _;
            i32::from(f.write_all(line.as_bytes()).is_err())
        }
        Err(_) => 1,
    })
}

#[cfg(test)]
#[path = "swarm_partition_tests.rs"]
mod tests;
