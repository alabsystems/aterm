// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `targo --unverified lab mesh churn`: the one-mesh churn lab, its membership scenarios
//! (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §10 "The churn lab", §11 step A8).
//!
//! N private Macs share this Mac's loopback. Each is a world of its own (a scratch `HOME`,
//! `ATERM_STATE_HOME`, XDG and `TMPDIR`, [`world::world_env`]) on the run's own membership log:
//! m00 founds it, so its genesis is the run's and never the compiled ALab one, and no Mac ever
//! dials past loopback (`ATERM_DEBUG_LOOPBACK_ONLY`, `--no-advertise`, and a lab vault's peers are
//! exactly its source and the servers its policy entry names). [`ISOLATED`] holds every run to
//! that.
//!
//! **What a Mac is in wave A.** A headless aterm under a private state root runs no membership
//! (its fleet listener keeps no log and its node service has no vault), so a Mac's membership
//! runs where wave A runs it: its vault server (`aterm secrets serve`, the aterm under test) and
//! its subscriber, the loop of `aterm secrets watch` ([`aterm_secrets::client::watch`]) hosted
//! in a process of this lab ([`member_main`]) so that the converge runner it starts is a stand-in
//! ([`converge_standin`]) and never a real converge pass in a scratch home. Every vault pass,
//! gap heal, re-key and coalesce is the product's, on the product's own clocks. B10 moves the
//! mesh into the node service; this lab's [`Mac`] then starts that instead.
//!
//! **Founding.** [`Lab::found`] is the one place a lab is founded, so D1's `aterm fleet init`
//! replaces it alone: today a genesis comes only from `aterm secrets create` (which founds
//! Commons, whose control the members desk needs) and [`client::seed_vault_genesis`]. No fleet
//! secret is minted and no second admin is named. `secrets create` makes m00 the vault's admin
//! until D1, so its subscriber runs the envelope approver every admin's does; the lab lets no Mac
//! in through it, and the record counts what it did. Every other Mac comes in through a member's
//! desk by the log ([`aterm_secrets::client::hand_over`], F15).
//!
//! **Admission.** Off the home LAN a Mac comes in by a person's Allow and the phrase, which is
//! what every admission here is (an act in the admitting Mac's world, [`Act::Desk`], running the
//! desk and the joiner's side, as `lab swarm partition` does). The LAN rule cannot admit a
//! loopback lab before B5 and B6: the desk turns automatic admission on only for the compiled
//! ALab vault (`members/desk.rs:270-272`), loopback is never home (`home_lan.rs:146`), a proof
//! never reads the route-facts file (`aterm-netplace/src/facts.rs:15-17`), and home evidence is
//! written only by a heartbeat that is off under a private state root.
//!
//! **A rebuilt Mac** with its node key kept asks first by that key's proof ([`Act::Prove`]), then
//! by a person's plain Allow, and the product decides each. Off the LAN in wave A it refuses both:
//! no fleet or LAN proof backs the key, and its seat signed within the day
//! (`client/readmit.rs` `decide`), and a plain Allow of a key another live seat holds is "already
//! bound to a different seat" (`client/admit.rs` `kept_out_with_node_at`). A person then decides
//! as the product asks: the admitter removes the old seat (`aterm members remove`) and presses
//! "Allow again", which restores the Mac under its old label. The record names each refusal and
//! the path that let it in.
//!
//! **The script** ([`run`]), §10's membership steps (tables, presence and the relay join with
//! A6, B8 and B9), in §10's order but for the rebuild:
//!
//! 1. m00 founds the lab; every other Mac is let in.
//! 2. Two Macs are stopped (`SIGSTOP`, server and subscriber) for `--minutes`, and two others
//!    are killed.
//! 3. Two members remove two others at once (`aterm members remove`, the CLI).
//! 4. The two killed Macs start again and read up.
//! 5. The sleepers wake once their minutes are up, and read up.
//! 6. One Mac is rebuilt with its node key kept and let back in. §10 rebuilds after the split's
//!    kill, but the way back rotates `[everyone]`, and that epoch would coalesce with the killed
//!    remover's partial one after the heal and cover its gap: the heal would hold without the
//!    gap rule ever running ([`rebuild`]).
//! 7. The Macs split into two islands (the `ATERM_DEBUG_PARTITION` cut). On island A a member
//!    removes another, and is killed in the middle of its batch: it signed the whole batch, and
//!    only a causal prefix of it (the `Remove`, the new `[everyone]` epoch and half of its `Wrap`s)
//!    reached half of A's servers. The Mac it removed writes an `[everyone]` entry after its
//!    cutoff. On island B a member writes an entry and lets a new Mac in. Nothing either island
//!    signs reaches the other until the heal ([`SPLIT_HELD`]).
//! 8. The islands heal with the remover down: the gap its kill left is the heal's to close.
//! 9. The remover returns.
//!
//! **Waits and timings.** A wait fails only once nothing it watches moved for one minute (the
//! hang detector, [`HANG`]) past the longest timer the product's own rules may still be running
//! when it starts, which each wait names ([`Schedule`]): a removal's coalesce waits out the
//! remover's grace, a heal the settle grace and its rank steps. Those are the product's schedule,
//! never a budget. Every wait's time is recorded in the measured note, never asserted.
//!
//! **Processes.** The run is driven from a private copy of this lab's image, proven by a line it
//! printed ([`launch`]), and every Mac's subscriber and act runs that copy too: a build that
//! relinks the lab mid-run replaces nothing the run runs, and every tethered spawn, which runs
//! through the driver's own image, stays tethered. Each long-running process is tethered to the
//! driver ([`aterm_uds::spawnfd::Tether`]), and the driver to the process that launched it: each
//! ends when its parent ends, killed or crashed included, by a watcher that signals only the
//! process it started. A Mac the script holds up whose process is not running breaks the step
//! whose wait finds it; an interrupt ends the run after the step it cut short, unjudged and
//! unrecorded, with the interrupt's status.
//!
//! **Negative controls** are set only through `ATERM_TEST_NEGATIVE_CONTROL` in every Mac's
//! world (`--negative-control NAME`), read by A3's `aterm_types::negative_control::on` in every
//! process that world starts: the member and act verbs dispatch before the lab forgets its
//! `ATERM*` variables. A control run bites when every act ran, the run stayed isolated and the
//! split held, and the check its control names failed on evidence it judged, never on a guard
//! against having none ([`verdict`]); it is recorded under its own arm.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_secrets::age::Recipient;
use aterm_secrets::client::{self, Paths};
use aterm_secrets::membership_log::{Body, EpochAudience, Kind, Log, Record, RecordId};
use aterm_uds::spawnfd::Stdio;

use crate::fleet_world as world;
use crate::{Fail, Res, procs, stop};

/// The one-minute hang detector.
pub const HANG: Duration = Duration::from_secs(60);

/// How long a wait whose Macs keep moving goes on past the product's schedule and the hang
/// detector before it ends as a livelock ([`ended`]): records that change on every read reset the
/// hang detector, and a membership that churns without settling would otherwise hold a wait, and
/// the run, open for good.
pub const LIVELOCK: Duration = Duration::from_secs(600);

/// The usage line.
pub const USAGE: &str = "mesh churn [--n N] [--seed S] [--minutes M] [--negative-control NAME] [--aterm PATH] [--keep] [--record] [--label WORD]";

/// This lab's verb for a Mac's subscriber ([`member_main`]).
pub const MEMBER_VERB: &str = "mesh-churn-member";

/// This lab's verb for one act in a Mac's world ([`act_main`]).
pub const ACT_VERB: &str = "mesh-churn-act";

/// Set in a Mac's subscriber's environment: the `init --converge` it starts is
/// [`converge_standin`].
pub const MEMBER_SEAM: &str = "ATERM_LAB_MESH_CHURN_MEMBER";

/// The variable a negative control is set by in a Mac's world (§9; A3's reader).
pub const NEGATIVE_CONTROL: &str = "ATERM_TEST_NEGATIVE_CONTROL";

/// The lab vault's name: never `alab`.
const VAULT: &str = "meshchurn";

/// Under a Mac's home: one line per converge runner its subscriber started.
const RUNNERS_FILE: &str = ".lab-converge-runs";

/// The entry m00 seeds the lab's `[everyone]` with, so the founding epoch seals something.
const SEED_ENTRY: &str = "mesh-churn-seed";

/// The entry the Mac removed on island A writes after its cutoff.
const LATE_ENTRY: &str = "mesh-churn-after-cutoff";

/// The entry island B's writer writes during the split.
const SPLIT_ENTRY: &str = "mesh-churn-island-b";

/// The fewest Macs a run takes: every role of [`Plan`] is a Mac of its own, and m00 has none.
pub const MIN_N: usize = 14;
/// The most.
pub const MAX_N: usize = 64;
/// §10's size, the presence and relay assertions' floor once they join.
const DEFAULT_N: usize = 30;
/// §10's sleep.
const DEFAULT_MINUTES: u64 = 5;

// The product's own timers a wait gives the product before its hang detector runs. They mirror
// `crates/aterm-secrets/src/client/log_entries.rs` (`GAP_STEP_MS` :5479, `REMOVER_GRACE_MS`
// :5485, `SETTLE_GRACE_MS` :5494, `pub(super)` there) and the subscriber's longest error nap
// (`client.rs` `LONGEST_ERROR_RETRY`) with the vault server's long-poll hold (`server.rs`
// `DEFAULT_HOLD`).

/// One heal rank's step.
const GAP_STEP: Duration = Duration::from_secs(60);
/// How long a Mac that removed nobody waits before it coalesces a remover's rotation.
const REMOVER_GRACE: Duration = Duration::from_secs(600);
/// How long a Mac waits for a minter's batch to settle before it heals over it.
const SETTLE_GRACE: Duration = Duration::from_secs(600);
/// One subscriber round at its slowest: its longest error nap after a long-poll's hold.
const ROUND: Duration = Duration::from_secs(60 + 25);
/// The longest one exchange with a vault server may take (`transport.rs` `EXCHANGE`,
/// `pub(crate)` there): a write that posts to a stopped Mac's server waits it out.
const EXCHANGE: Duration = Duration::from_secs(120);
/// How long an act in a Mac's world may run: the hang detector past the product's longest
/// exchange.
const ACT_HANG: Duration = Duration::from_secs(EXCHANGE.as_secs() + HANG.as_secs());

// ---------------------------------------------------------------------------------------------
// Negative controls.
// ---------------------------------------------------------------------------------------------

/// The world's `ATERM_TEST_NEGATIVE_CONTROL` entry for `control`, in a build that has its
/// reader (`aterm_types::negative_control::on`, under `any(test, debug_assertions)`).
#[cfg(any(test, debug_assertions))]
fn control_env(control: Option<Control>) -> Option<(String, String)> {
    control.map(|control| (NEGATIVE_CONTROL.to_owned(), control.name.to_owned()))
}

/// None in a release build: it has no reader of the variable, and names it nowhere (census
/// `debug-seam`).
#[cfg(not(any(test, debug_assertions)))]
fn control_env(_: Option<Control>) -> Option<(String, String)> {
    None
}

/// A negative control this lab sets (§10), and the check it must break.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Control {
    /// Its `ATERM_TEST_NEGATIVE_CONTROL` value.
    pub name: &'static str,
    /// The product switch it turns on.
    pub switch: &'static str,
    /// The check that must fail under it.
    pub breaks: &'static str,
}

/// The controls of §10 whose scenario this lab runs. `no-inventory-on-gap` and
/// `reused-incarnation` need the relay's cut-below-cursor and incarnation scenarios, which join
/// with B8 ([`refused_control`]).
pub const CONTROLS: &[Control] = &[
    Control {
        name: "old-gap-rule",
        switch: "OLD_GAP_RULE",
        breaks: NEWEST_KEY,
    },
    Control {
        name: "unsettled-ok",
        switch: "UNSETTLED_OK",
        breaks: WRAP_BOUND,
    },
];

/// Why `name` is no control this lab runs: a relay control (B8), or none of §10's.
#[must_use]
pub fn refused_control(name: &str) -> String {
    match name {
        "no-inventory-on-gap" | "reused-incarnation" => format!(
            "{name} breaks a relay scenario (a cut below a sleeper's cursor, a rebuilt \
             incarnation), which joins this lab with the relay (B8)"
        ),
        _ => format!(
            "{name} is no negative control of the churn lab's (it runs {})",
            CONTROLS
                .iter()
                .map(|c| c.name)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

// ---------------------------------------------------------------------------------------------
// Arguments.
// ---------------------------------------------------------------------------------------------

/// `mesh churn`'s arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Args {
    /// How many Macs found and join the lab.
    pub n: usize,
    /// What chooses each role and island ([`plan`]).
    pub seed: u64,
    /// How long the sleepers stay stopped.
    pub minutes: u64,
    /// The negative control set in every Mac's world.
    pub control: Option<Control>,
    /// The aterm under test.
    pub aterm: Option<String>,
    /// Keep the worlds after the run.
    pub keep: bool,
    /// Write the record under `docs/measured/`.
    pub record: bool,
    /// A word naming the record beside its arm.
    pub label: Option<String>,
}

/// A record label: 1-40 of `[a-z0-9-]`, no hyphen at either end.
fn label_word(word: &str) -> bool {
    (1..=40).contains(&word.len())
        && word
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !word.starts_with('-')
        && !word.ends_with('-')
}

/// Parse `mesh churn`'s arguments (after `churn`); `seed` is the seed a run with no `--seed`
/// takes.
///
/// # Errors
/// An unknown flag, a missing or bad value, or a control this lab does not run (exit 2).
pub fn parse(args: &[String], seed: u64) -> Result<Args, Fail> {
    let mut out = Args {
        n: DEFAULT_N,
        seed,
        minutes: DEFAULT_MINUTES,
        control: None,
        aterm: None,
        keep: false,
        record: false,
        label: None,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = |flag: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| Fail::usage(format!("mesh churn: {flag} takes a value")))
        };
        match arg.as_str() {
            "--n" => {
                let v = value("--n")?;
                out.n = v
                    .parse()
                    .ok()
                    .filter(|n| (MIN_N..=MAX_N).contains(n))
                    .ok_or_else(|| {
                        Fail::usage(format!(
                            "mesh churn: --n takes {MIN_N} to {MAX_N} Macs, not {v:?}"
                        ))
                    })?;
            }
            "--seed" => {
                let v = value("--seed")?;
                out.seed = v.parse().map_err(|_| {
                    Fail::usage(format!("mesh churn: --seed takes a u64, not {v:?}"))
                })?;
            }
            "--minutes" => {
                let v = value("--minutes")?;
                out.minutes = v
                    .parse()
                    .ok()
                    .filter(|m| (1..=30).contains(m))
                    .ok_or_else(|| {
                        Fail::usage(format!("mesh churn: --minutes takes 1 to 30, not {v:?}"))
                    })?;
            }
            "--negative-control" => {
                let v = value("--negative-control")?;
                out.control = Some(
                    CONTROLS
                        .iter()
                        .copied()
                        .find(|c| c.name == v)
                        .ok_or_else(|| {
                            Fail::usage(format!("mesh churn: {}", refused_control(&v)))
                        })?,
                );
            }
            "--aterm" => out.aterm = Some(value("--aterm")?),
            "--keep" => out.keep = true,
            "--record" => out.record = true,
            "--label" => {
                let v = value("--label")?;
                if !label_word(&v) {
                    return Err(Fail::usage(format!(
                        "mesh churn: --label takes 1-40 of [a-z0-9-], not {v:?}"
                    )));
                }
                out.label = Some(v);
            }
            other => {
                return Err(Fail::usage(format!(
                    "mesh churn: unknown argument {other:?}\nusage: {USAGE}"
                )));
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// The plan: who does what, from the seed.
// ---------------------------------------------------------------------------------------------

/// SplitMix64: the plan's one source of choice, so a seed repeats a run's roles.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    /// A generator from `seed`.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// The next value.
    pub const fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// `items` in a seeded order (Fisher-Yates).
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = usize::try_from(self.next() % (i as u64 + 1)).unwrap_or(0);
            items.swap(i, j);
        }
    }
}

/// Which island a Mac is on during the split.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Island {
    /// Where the remover is killed mid-batch.
    A,
    /// Where a write, an admission and a rebuild happen.
    B,
}

impl Island {
    /// Its letter.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::A => "a",
            Self::B => "b",
        }
    }
}

/// Every role of a run, each a Mac's index (m00 founds and has none), and the islands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// How many Macs found and join.
    pub n: usize,
    /// The two concurrent removals: (remover, removed).
    pub removals: [(usize, usize); 2],
    /// The two Macs stopped for `--minutes`.
    pub sleepers: [usize; 2],
    /// The two Macs killed and started again.
    pub restarts: [usize; 2],
    /// Island A's remover, killed in the middle of its batch.
    pub split_remover: usize,
    /// The Mac it removes, which writes after its cutoff.
    pub split_removed: usize,
    /// Island B's writer.
    pub writer: usize,
    /// Island B's admitter: it lets the newcomer in, and the rebuilt Mac back.
    pub admitter: usize,
    /// The Mac rebuilt on island B with its node key kept.
    pub rebuilt: usize,
    /// The new Mac admitted on island B: index `n`.
    pub newcomer: usize,
    /// Each Mac's island, by index (the newcomer's included).
    pub islands: Vec<Island>,
}

impl Plan {
    /// The Macs on `island`.
    #[must_use]
    pub fn on(&self, island: Island) -> Vec<usize> {
        (0..self.islands.len())
            .filter(|&i| self.islands[i] == island)
            .collect()
    }
}

/// The plan of a run of `n` Macs (at least [`MIN_N`]) from `seed`: thirteen distinct roles
/// drawn from m01 onwards, then the rest split in two, so each island holds about half.
///
/// # Panics
/// Never for `n >= MIN_N`.
#[must_use]
pub fn plan(n: usize, seed: u64) -> Plan {
    assert!(n >= MIN_N, "a plan takes at least {MIN_N} Macs");
    let mut rng = Rng::new(seed);
    let mut order: Vec<usize> = (1..n).collect();
    rng.shuffle(&mut order);
    let role = |k: usize| order[k];
    let mut islands = vec![Island::A; n + 1];
    let fixed_a = [role(8), role(9)];
    let fixed_b = [role(10), role(11), role(12)];
    let mut rest: Vec<usize> = (0..n)
        .filter(|i| !fixed_a.contains(i) && !fixed_b.contains(i))
        .collect();
    rng.shuffle(&mut rest);
    let half = (n / 2).saturating_sub(fixed_a.len());
    for (k, &i) in rest.iter().enumerate() {
        islands[i] = if k < half { Island::A } else { Island::B };
    }
    for i in fixed_b {
        islands[i] = Island::B;
    }
    islands[n] = Island::B;
    Plan {
        n,
        removals: [(role(0), role(1)), (role(2), role(3))],
        sleepers: [role(4), role(5)],
        restarts: [role(6), role(7)],
        split_remover: role(8),
        split_removed: role(9),
        writer: role(10),
        admitter: role(11),
        rebuilt: role(12),
        newcomer: n,
        islands,
    }
}

/// A Mac's name.
#[must_use]
pub fn name(i: usize) -> String {
    format!("m{i:02}")
}

// ---------------------------------------------------------------------------------------------
// What the lab reads, and the judgement (pure, so the tests hold it).
// ---------------------------------------------------------------------------------------------

/// One Mac as the lab reads it from its files, never through the Mac.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    /// Its name.
    pub name: String,
    /// It holds a membership log.
    pub has_log: bool,
    /// Its log's frontier: the records no record it holds follows.
    pub heads: Vec<RecordId>,
    /// How many records its log holds.
    pub records: usize,
    /// The records its log holds.
    pub held: BTreeSet<RecordId>,
    /// Its log's genesis.
    pub genesis: Option<RecordId>,
    /// Its vault's name and whether its signer is ALab's.
    pub vault: Option<(String, bool)>,
    /// Its own vault identity's recipient.
    pub recipient: Option<Recipient>,
    /// Its own log seats it current ([`current_seats`]).
    pub current: bool,
    /// The join codes of the seats its log holds current ([`current_seats`]).
    pub seated: BTreeSet<String>,
    /// The newest `[everyone]` epochs its log holds.
    pub newest: Vec<RecordId>,
    /// The recipients those epochs wrap, in its log.
    pub newest_wrap: BTreeSet<Recipient>,
    /// Its identity opens every one of them.
    pub opens_newest: bool,
    /// The labels of the current seats in its per-seat view (`aterm members list`), `None` where
    /// the view did not read: it reads the local roster under a nonblocking take of the vault's
    /// state lock, which a running subscriber holds through each pass, so the lab reads it once
    /// every subscriber has ended ([`Lab::read_end`]).
    pub seats: Option<BTreeSet<String>>,
    /// Current seats (those of [`Seen::seated`]) by node key, in its log.
    pub node_seats: BTreeMap<[u8; 32], usize>,
    /// The `[everyone]` entries it reads: each value's SHA-256, never the value.
    pub entries: BTreeMap<String, [u8; 32]>,
    /// Its subscriber's process runs and is not stopped.
    pub up: bool,
    /// What its own vault says of it (`aterm secrets status`): `in`, `revoked`, `pending`,
    /// `plain`, `unconfigured` or `unreadable`; empty where unread ([`see_whole`] reads it).
    pub vault_state: String,
}

/// One step of the script: what it did, whether it did it, and how long it took.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    /// Its name.
    pub name: String,
    /// Every act of it ran ([`STEPS`]).
    pub ok: bool,
    /// Every wait of it settled ([`SETTLED`]): a wait the product's own rules never settle fails
    /// this, never [`Step::ok`].
    pub settled: bool,
    /// How long it took (milliseconds).
    pub ms: u64,
    /// What it found.
    pub note: String,
}

/// A Mac that left and came back, and when it read up again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadUp {
    /// The Mac.
    pub mac: String,
    /// How it left: `slept`, `restarted`, `rebuilt` or `remover`.
    pub how: &'static str,
    /// Milliseconds from its return to the first wait that watched it running settling (its
    /// log's frontier and newest key matching the others'), `None` where none did.
    pub ms: Option<u64>,
}

/// How the rebuilt Mac came back.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rebuilt {
    /// The Mac.
    pub mac: String,
    /// `proof` (its node key alone let it back in), `phrase` (a person's Allow), or
    /// `allow-again` (a person removed its old seat and pressed "Allow again").
    pub path: String,
    /// Each path the product refused, and why, in the order tried.
    pub refused: Vec<(String, String)>,
    /// Its node key.
    pub node: Option<[u8; 32]>,
}

/// What the split's ledger read just before the heal: every record one island signed during it
/// that the other island held.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Split {
    /// The split was held and read.
    pub read: bool,
    /// What crossed it, each record once, by the Mac whose seat signed it.
    pub crossed: Vec<String>,
}

/// A write the split made, and who holds it at the end.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Wrote {
    /// Its entry.
    pub entry: String,
    /// Its value's SHA-256, never the value.
    pub digest: [u8; 32],
    /// Its `Put` record, as its writer's log holds it.
    pub record: Option<RecordId>,
    /// The current members whose logs do not hold that record at the end.
    pub unheld: Vec<String>,
}

/// Everything a run found ([`judge`]'s input).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Evidence {
    /// The script's steps.
    pub steps: Vec<Step>,
    /// The Macs the run's ground truth holds current, by name, with their seats' labels.
    pub current: BTreeMap<String, String>,
    /// The Macs it removed, by name, with their recipients.
    pub removed: BTreeMap<String, Recipient>,
    /// Every Mac's read once the heal's wait ended, island A's remover still down.
    pub healed: Vec<Seen>,
    /// Every Mac's last read.
    pub end: Vec<Seen>,
    /// The most `Wrap`s of one epoch to one recipient across every Mac's log, and where.
    pub most_wraps: (usize, String),
    /// `Wrap`s across every Mac's log, by how many of one (epoch, recipient) there were.
    pub wrap_histogram: BTreeMap<usize, usize>,
    /// The removed Mac's write after its cutoff.
    pub late: Option<Wrote>,
    /// Island B's honest write during the split.
    pub honest: Option<Wrote>,
    /// Macs that left and came back.
    pub read_ups: Vec<ReadUp>,
    /// How the rebuilt Mac came back.
    pub rebuilt: Option<Rebuilt>,
    /// The newcomer.
    pub newcomer: Option<String>,
    /// The split's ledger.
    pub split: Split,
    /// The negative control the run set.
    pub control: Option<Control>,
}

/// One judged assertion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    /// Its name.
    pub name: &'static str,
    /// It had the evidence it judges: false where it failed for having none (nothing read, a
    /// step that never ran), which says nothing of the product.
    pub ran: bool,
    /// It held: it ran, and what it asserts was true.
    pub pass: bool,
    /// What it saw.
    pub detail: String,
}

/// Every Mac's log is the run's own: not ALab's genesis, vault or signer.
pub const ISOLATED: &str = "isolated";
/// Every act of every scenario step ran.
pub const STEPS: &str = "steps";
/// Every wait settled within the product's own schedule and the hang detector past it.
pub const SETTLED: &str = "settled";
/// Every current member's log frontier is equal.
pub const FRONTIER: &str = "frontier";
/// Once the heal's wait ended, with island A's remover still down, every up current member opens
/// the newest `[everyone]` epoch: the heal closed the gap the remover's kill left. The remover's
/// return would hand every member its missing `Wrap`s and cover a heal that never ran, so the end
/// read is no evidence of this.
pub const NEWEST_KEY: &str = "newest-key";
/// Every current member's own vault reads it in: the key assertion as the Mac's secrets see it,
/// for a subscriber that marks itself revoked stops writing them.
pub const VAULT_IN: &str = "vault-in";
/// No epoch has more than two `Wrap`s to one recipient.
pub const WRAP_BOUND: &str = "wrap-bound";
/// No removed Mac is wrapped by a current member's newest epoch or seated by its log, and none
/// reads itself a current member.
pub const REMOVED_NO_KEY: &str = "removed-no-key";
/// Every current member holds the removed Mac's write after its cutoff and reads it nowhere, and
/// holds and reads island B's honest write.
pub const POST_CUTOFF_VOID: &str = "post-cutoff-void";
/// Every current member's per-seat view equals ground truth.
pub const SEAT_VIEW: &str = "seat-view";
/// Every Mac that left came back and read up.
pub const READ_UP: &str = "read-up";
/// The newcomer and the rebuilt Mac are current, and the rebuilt Mac's node key holds one seat.
pub const ADMITTED: &str = "admitted";
/// Nothing one island signed during the split reached the other before the heal.
pub const SPLIT_HELD: &str = "split-held";

fn short(id: &RecordId) -> String {
    aterm_codec::hex::encode_lower(&id[..4])
}

fn shorts(ids: &[RecordId]) -> String {
    if ids.is_empty() {
        return "none".to_owned();
    }
    ids.iter().map(short).collect::<Vec<_>>().join(",")
}

/// Judge `ev`: every check, in a fixed order.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn judge(ev: &Evidence) -> Vec<Check> {
    let end: BTreeMap<&str, &Seen> = ev.end.iter().map(|s| (s.name.as_str(), s)).collect();
    let current: Vec<&Seen> = ev
        .current
        .keys()
        .filter_map(|name| end.get(name.as_str()).copied())
        .collect();
    let missing: Vec<&str> = ev
        .current
        .keys()
        .filter(|name| !end.contains_key(name.as_str()))
        .map(String::as_str)
        .collect();
    // Every current member was read at the end: what the end's checks judge.
    let read_whole = missing.is_empty() && !current.is_empty();
    let unread = |detail: String| {
        if missing.is_empty() {
            detail
        } else {
            format!("{detail}; unread: {}", missing.join(" "))
        }
    };
    let mut checks = Vec::new();
    let mut push = |name: &'static str, ran: bool, held: bool, detail: String| {
        checks.push(Check {
            name,
            ran,
            pass: ran && held,
            detail,
        });
    };

    let foreign: Vec<String> = ev
        .end
        .iter()
        .filter(|s| {
            s.vault
                .as_ref()
                .is_some_and(|(vault, alab_signer)| vault == "alab" || *alab_signer)
                || s.genesis
                    .is_some_and(|g| compiled_genesis().is_some_and(|c| c == g))
        })
        .map(|s| s.name.clone())
        .collect();
    push(
        ISOLATED,
        !ev.end.is_empty(),
        foreign.is_empty(),
        if foreign.is_empty() {
            format!(
                "{} Macs on the run's own genesis and vault {VAULT:?}, none on ALab's",
                ev.end.len()
            )
        } else {
            format!("on ALab's vault, signer or genesis: {}", foreign.join(", "))
        },
    );

    let listed = |pick: &dyn Fn(&Step) -> bool, all: &str| {
        let picked: Vec<String> = ev
            .steps
            .iter()
            .filter(|s| pick(s))
            .map(|s| format!("{}: {}", s.name, s.note))
            .collect();
        if picked.is_empty() {
            format!("{} steps {all}", ev.steps.len())
        } else {
            picked.join("; ")
        }
    };
    push(
        STEPS,
        !ev.steps.is_empty(),
        ev.steps.iter().all(|s| s.ok),
        listed(&|s| !s.ok, "ran every act"),
    );
    push(
        SETTLED,
        !ev.steps.is_empty(),
        ev.steps.iter().all(|s| s.settled),
        listed(&|s| !s.settled, "settled every wait"),
    );

    let frontiers: BTreeSet<&[RecordId]> = current.iter().map(|s| s.heads.as_slice()).collect();
    let no_log: Vec<&str> = current
        .iter()
        .filter(|s| !s.has_log)
        .map(|s| s.name.as_str())
        .collect();
    push(
        FRONTIER,
        read_whole,
        frontiers.len() == 1 && no_log.is_empty(),
        if frontiers.len() == 1 && no_log.is_empty() {
            unread(format!(
                "{} current members at one frontier {}",
                current.len(),
                shorts(frontiers.iter().next().copied().unwrap_or_default())
            ))
        } else {
            let mut by: BTreeMap<String, Vec<&str>> = BTreeMap::new();
            for s in &current {
                by.entry(if s.has_log {
                    shorts(&s.heads)
                } else {
                    "no log".to_owned()
                })
                .or_default()
                .push(&s.name);
            }
            unread(
                by.iter()
                    .map(|(heads, macs)| format!("[{heads}] {}", macs.join(" ")))
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        },
    );

    let healed: BTreeMap<&str, &Seen> = ev.healed.iter().map(|s| (s.name.as_str(), s)).collect();
    let at_heal: Vec<&Seen> = ev
        .current
        .keys()
        .filter_map(|name| healed.get(name.as_str()).copied())
        .filter(|s| s.up)
        .collect();
    let short_key: Vec<&str> = at_heal
        .iter()
        .filter(|s| !s.opens_newest)
        .map(|s| s.name.as_str())
        .collect();
    push(
        NEWEST_KEY,
        !at_heal.is_empty(),
        short_key.is_empty(),
        if at_heal.is_empty() {
            "nothing was read once the heal's wait ended".to_owned()
        } else if short_key.is_empty() {
            format!(
                "once the heal's wait ended, the remover down, every one of {} up current members \
                 opened the newest [everyone] epoch {}",
                at_heal.len(),
                shorts(&at_heal[0].newest)
            )
        } else {
            format!(
                "once the heal's wait ended, the remover down, {} of {} up current members did \
                 not open the newest [everyone] epoch: {}",
                short_key.len(),
                at_heal.len(),
                short_key.join(" ")
            )
        },
    );

    let not_in: Vec<String> = current
        .iter()
        .filter(|s| s.vault_state != "in")
        .map(|s| format!("{} {}", s.name, s.vault_state))
        .collect();
    push(
        VAULT_IN,
        read_whole,
        not_in.is_empty(),
        unread(if not_in.is_empty() {
            format!(
                "every current member's vault reads it in ({})",
                current.len()
            )
        } else {
            format!(
                "{} of {} current members' vaults do not read them in: {}",
                not_in.len(),
                current.len(),
                not_in.join(", ")
            )
        }),
    );

    push(
        WRAP_BOUND,
        !ev.wrap_histogram.is_empty(),
        ev.most_wraps.0 <= 2,
        format!(
            "most Wraps of one epoch to one recipient: {}{} (histogram {})",
            ev.most_wraps.0,
            if ev.most_wraps.1.is_empty() {
                String::new()
            } else {
                format!(", {}", ev.most_wraps.1)
            },
            ev.wrap_histogram
                .iter()
                .map(|(k, v)| format!("{k}x{v}"))
                .collect::<Vec<_>>()
                .join(" ")
        ),
    );

    let mut kept_in = Vec::new();
    for (name, recipient) in &ev.removed {
        let code = client::join_code(recipient);
        for s in &current {
            if s.newest_wrap.contains(recipient) {
                kept_in.push(format!("{name} wrapped by {}'s newest epoch", s.name));
            }
            if s.seated.contains(&code) {
                kept_in.push(format!("{} still seats {name}", s.name));
            }
        }
        if end.get(name.as_str()).is_some_and(|own| own.current) {
            kept_in.push(format!("{name} reads itself a current member"));
        }
    }
    push(
        REMOVED_NO_KEY,
        !ev.removed.is_empty() && read_whole,
        kept_in.is_empty(),
        unread(if kept_in.is_empty() {
            format!(
                "{} removed Macs ({}): no current member wraps or seats one, and none reads \
                 itself current",
                ev.removed.len(),
                ev.removed.keys().cloned().collect::<Vec<_>>().join(", ")
            )
        } else {
            kept_in.join("; ")
        }),
    );

    let mut void = Vec::new();
    let wrote = match (&ev.late, &ev.honest) {
        (Some(late), Some(honest)) if late.record.is_some() && honest.record.is_some() => {
            for s in &current {
                if s.entries.get(&late.entry) == Some(&late.digest) {
                    void.push(format!("{} reads the removed Mac's late write", s.name));
                }
                if s.entries.get(&honest.entry) != Some(&honest.digest) {
                    void.push(format!("{} does not read island B's write", s.name));
                }
            }
            for name in &late.unheld {
                void.push(format!(
                    "{name} never received the removed Mac's late write, so nothing voided it there"
                ));
            }
            for name in &honest.unheld {
                void.push(format!("{name} never received island B's write"));
            }
            true
        }
        _ => false,
    };
    push(
        POST_CUTOFF_VOID,
        wrote && read_whole,
        void.is_empty(),
        unread(if !wrote {
            "the split made no late or honest write whose record its writer holds".to_owned()
        } else if void.is_empty() {
            "every current member holds the removed Mac's write after its cutoff and reads it \
             nowhere, and holds and reads island B's"
                .to_owned()
        } else {
            void.join("; ")
        }),
    );

    let truth: BTreeSet<String> = ev.current.values().cloned().collect();
    let viewless: Vec<&str> = current
        .iter()
        .filter(|s| s.seats.is_none())
        .map(|s| s.name.as_str())
        .collect();
    let wrong: Vec<String> = current
        .iter()
        .filter_map(|s| Some((s, s.seats.as_ref()?)))
        .filter(|(_, seats)| **seats != truth)
        .map(|(s, seats)| {
            let extra: Vec<&String> = seats.difference(&truth).collect();
            let lacks: Vec<&String> = truth.difference(seats).collect();
            format!("{}: +{extra:?} -{lacks:?}", s.name)
        })
        .collect();
    push(
        SEAT_VIEW,
        read_whole && viewless.is_empty(),
        wrong.is_empty(),
        unread(if !viewless.is_empty() {
            format!("no per-seat view read on {}", viewless.join(" "))
        } else if wrong.is_empty() {
            format!(
                "every current member's per-seat view lists the {} seats ground truth holds",
                truth.len()
            )
        } else {
            wrong.join("; ")
        }),
    );

    let never: Vec<String> = ev
        .read_ups
        .iter()
        .filter(|r| r.ms.is_none())
        .map(|r| format!("{} ({})", r.mac, r.how))
        .collect();
    push(
        READ_UP,
        !ev.read_ups.is_empty(),
        never.is_empty(),
        if never.is_empty() {
            format!(
                "{} Macs came back and read up: {}",
                ev.read_ups.len(),
                ev.read_ups
                    .iter()
                    .map(|r| format!(
                        "{} {} {:.1}s",
                        r.mac,
                        r.how,
                        r.ms.unwrap_or(0) as f64 / 1000.0
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        } else {
            format!("never read up: {}", never.join(", "))
        },
    );

    let mut admitted = Vec::new();
    for mac in ev
        .newcomer
        .iter()
        .chain(ev.rebuilt.as_ref().map(|r| &r.mac))
    {
        match end.get(mac.as_str()) {
            Some(s) if s.current => {}
            _ => admitted.push(format!("{mac} is not a current member")),
        }
    }
    if let Some(rebuilt) = &ev.rebuilt {
        match rebuilt.node {
            Some(node) => {
                for s in &current {
                    let held = s.node_seats.get(&node).copied().unwrap_or(0);
                    if held != 1 {
                        admitted.push(format!(
                            "{} reads {held} current seats on {}'s node key",
                            s.name, rebuilt.mac
                        ));
                    }
                }
            }
            None => admitted.push(format!("{} has no node key", rebuilt.mac)),
        }
    }
    let admissions = ev.newcomer.is_some() && ev.rebuilt.is_some();
    push(
        ADMITTED,
        admissions,
        admitted.is_empty(),
        if !admissions {
            "the run let no newcomer in or rebuilt no Mac".to_owned()
        } else if admitted.is_empty() {
            format!(
                "{} is in; {} came back by {}{}",
                ev.newcomer.as_deref().unwrap_or("-"),
                ev.rebuilt.as_ref().map_or("-", |r| r.mac.as_str()),
                ev.rebuilt.as_ref().map_or("-", |r| r.path.as_str()),
                ev.rebuilt
                    .iter()
                    .flat_map(|r| &r.refused)
                    .map(|(path, why)| format!("; its {path} was refused: {why}"))
                    .collect::<String>()
            )
        } else {
            admitted.join("; ")
        },
    );

    push(
        SPLIT_HELD,
        ev.split.read,
        ev.split.crossed.is_empty(),
        if !ev.split.read {
            "the split was not read".to_owned()
        } else if ev.split.crossed.is_empty() {
            "nothing either island signed during the split reached the other before the heal"
                .to_owned()
        } else {
            format!("crossed: {}", ev.split.crossed.join("; "))
        },
    );
    checks
}

/// Whether a run's checks say what it was for. With no control, every check held. Under a
/// negative control, the run itself was sound (every act ran, it stayed isolated, and the split
/// held) and the check the control breaks ran and failed: a check that failed for want of
/// evidence, or a run that broke, says nothing of the switch. The waits may stall under it.
#[must_use]
pub fn verdict(checks: &[Check], control: Option<Control>) -> bool {
    let held = |name: &str| checks.iter().any(|c| c.name == name && c.pass);
    match control {
        None => checks.iter().all(|c| c.pass),
        Some(control) => {
            [ISOLATED, STEPS, SPLIT_HELD].into_iter().all(held)
                && checks
                    .iter()
                    .any(|c| c.name == control.breaks && c.ran && !c.pass)
        }
    }
}

/// The compiled ALab genesis's id.
fn compiled_genesis() -> Option<RecordId> {
    let hex = aterm_secrets::ALAB_GENESIS?;
    let bytes = aterm_codec::hex::decode_vec(hex.as_bytes())?;
    bytes.try_into().ok()
}

/// The `Wrap`s of every record in `logs`, counted per (epoch, recipient): the most of one, where,
/// and the histogram.
#[must_use]
pub fn wrap_counts<'a>(
    logs: impl IntoIterator<Item = &'a Log>,
) -> ((usize, String), BTreeMap<usize, usize>) {
    let mut seen: BTreeSet<RecordId> = BTreeSet::new();
    let mut per: BTreeMap<(RecordId, Recipient), usize> = BTreeMap::new();
    for log in logs {
        for record in log.records() {
            if !seen.insert(*record.id()) {
                continue;
            }
            if let Body::Wrap {
                epoch, recipient, ..
            } = record.body()
            {
                *per.entry((*epoch, *recipient)).or_default() += 1;
            }
        }
    }
    let mut histogram = BTreeMap::new();
    for count in per.values() {
        *histogram.entry(*count).or_default() += 1;
    }
    let most = per
        .iter()
        .max_by_key(|(_, n)| **n)
        .map(|((epoch, recipient), n)| {
            let bech = recipient.to_bech32();
            (
                *n,
                format!(
                    "epoch {} to {}",
                    short(epoch),
                    bech.get(bech.len().saturating_sub(8)..).unwrap_or(&bech)
                ),
            )
        })
        .unwrap_or((0, String::new()));
    (most, histogram)
}

/// The SHA-256 of `bytes`.
fn digest(bytes: &[u8]) -> [u8; 32] {
    aterm_digest::Sha256::digest(bytes)
}

/// One seat a log holds current.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seat {
    /// Its recipient's join code.
    pub code: String,
    /// Its label.
    pub label: String,
    /// Its node key.
    pub node: Option<[u8; 32]>,
}

/// The seats `log` holds current, by the product's rule for a current seat as `aterm members
/// list` applies it (`client::member_list`): its recipient not removed, its node key not
/// removed, and it not left out of `[everyone]`. The list's one further test, the local
/// roster's deny rows, reads under a nonblocking take of the vault's state lock that a running
/// subscriber holds through each pass, so it is left to the per-seat view ([`Seen::seats`]).
#[must_use]
pub fn current_seats(log: &Log) -> Vec<Seat> {
    let membership = log.membership();
    membership
        .members()
        .iter()
        .filter(|(actor, m)| {
            let recipient = m.binding.recipient();
            !log.recipient_is_removed(&recipient)
                && !membership.member_node_removed(m)
                && !log.audience_left_out(&EpochAudience::Everyone, actor)
                && !log.audience_left_out_recipient(&EpochAudience::Everyone, &recipient)
        })
        .map(|(_, m)| Seat {
            code: client::join_code(&m.binding.recipient()),
            label: m.label.clone(),
            node: m.node_key,
        })
        .collect()
}

/// What the lab reads of the Mac `name` whose vault is at `paths`, often and while its
/// subscriber runs: its files only, through no read that takes its vault's state lock, so a wait
/// never contends with the pass it watches. Its vault's status and its per-seat view are the
/// end's ([`see_whole`]).
#[must_use]
pub fn see(name: &str, paths: &Paths, up: bool) -> Seen {
    let mut seen = Seen {
        name: name.to_owned(),
        up,
        ..Seen::default()
    };
    seen.vault = client::config(paths).ok().flatten().map(|config| {
        let alab = config.signer.is_some_and(|signer| {
            aterm_secrets::b64::decode_padded(aterm_secrets::ALAB_SIGNER)
                .is_some_and(|ours| ours.as_slice() == signer.as_slice())
        });
        (config.name, alab)
    });
    let identity = client::read_machine_identity(paths).ok().flatten();
    seen.recipient = identity
        .as_ref()
        .map(aterm_secrets::age::Identity::recipient);
    let Ok(store) = client::load_records(paths) else {
        return seen;
    };
    let Some(log) = store.log() else {
        return seen;
    };
    seen.has_log = true;
    seen.heads = log.heads();
    seen.held = log.records().map(|rec| *rec.id()).collect();
    seen.records = seen.held.len();
    seen.genesis = Some(*log.genesis().id());
    let seats = current_seats(log);
    seen.seated = seats.iter().map(|s| s.code.clone()).collect();
    seen.current = seen
        .recipient
        .as_ref()
        .is_some_and(|me| seen.seated.contains(&client::join_code(me)));
    for node in seats.iter().filter_map(|s| s.node) {
        *seen.node_seats.entry(node).or_default() += 1;
    }
    seen.newest = log.newest_epochs(&EpochAudience::Everyone);
    if let Some(identity) = &identity
        && let Ok(Some(opened)) = log.open_audience_entries(identity)
    {
        seen.opens_newest = !seen.newest.is_empty()
            && seen
                .newest
                .iter()
                .all(|epoch| opened.unwrapped_keys.contains_key(epoch));
        for epoch in &seen.newest {
            if let Some(wrapped) = opened.wrapped_recipients.get(epoch) {
                seen.newest_wrap.extend(wrapped.iter().copied());
            }
        }
        if let Some(entries) = opened
            .by_audience
            .get(&aterm_secrets::text::Audience::Everyone)
        {
            seen.entries = entries
                .iter()
                .map(|(name, entry)| (name.clone(), digest(entry.value.as_slice())))
                .collect();
        }
    }
    seen
}

/// [`see`], with what only a Mac whose subscriber has ended reads whole: its vault's status
/// (`aterm secrets status`) and its per-seat view (`aterm members list`).
#[must_use]
pub fn see_whole(name: &str, paths: &Paths, up: bool) -> Seen {
    let mut seen = see(name, paths, up);
    seen.vault_state = match client::status(paths).map(|status| status.membership) {
        Ok(client::Membership::In { .. }) => "in",
        Ok(client::Membership::Revoked { .. }) => "revoked",
        Ok(client::Membership::Pending) => "pending",
        Ok(client::Membership::Plain) => "plain",
        Ok(client::Membership::Unconfigured) => "unconfigured",
        Err(_) => "unreadable",
    }
    .to_owned();
    seen.seats = client::member_list(paths).ok().flatten().map(|rows| {
        rows.into_iter()
            .filter(|row| row.removed.is_none())
            .map(|row| row.label)
            .collect()
    });
    seen
}

/// What a wait waits for.
#[derive(Clone, Copy, Debug)]
pub enum Need<'a> {
    /// One frontier, each Mac holding every one of these records: what reached its island spread
    /// over it, never a frontier no Mac has yet moved from.
    Holds(&'a BTreeSet<RecordId>),
    /// One frontier, each Mac opening the newest `[everyone]` epochs, none of which wraps a Mac
    /// these name.
    Key(&'a BTreeMap<String, Recipient>),
}

/// Whether `seen` (the reads of the Macs a wait watches) hold what `need` asks: `None` where
/// they do, else why not.
#[must_use]
pub fn settled(seen: &[&Seen], need: Need<'_>) -> Option<String> {
    let frontiers: BTreeSet<&[RecordId]> = seen.iter().map(|s| s.heads.as_slice()).collect();
    if frontiers.len() > 1 {
        return Some(format!(
            "{} frontiers among {} Macs",
            frontiers.len(),
            seen.len()
        ));
    }
    let no_log: Vec<&str> = seen
        .iter()
        .filter(|s| !s.has_log)
        .map(|s| s.name.as_str())
        .collect();
    if !no_log.is_empty() {
        return Some(format!("no log: {}", no_log.join(" ")));
    }
    let removed = match need {
        Need::Holds(records) => {
            let short: Vec<&str> = seen
                .iter()
                .filter(|s| !records.is_subset(&s.held))
                .map(|s| s.name.as_str())
                .collect();
            return (!short.is_empty()).then(|| {
                format!(
                    "not yet holding the {} records that reached its island: {}",
                    records.len(),
                    short.join(" ")
                )
            });
        }
        Need::Key(removed) => removed,
    };
    let still: Vec<&str> = removed
        .iter()
        .filter(|(_, r)| seen.iter().any(|s| s.newest_wrap.contains(r)))
        .map(|(name, _)| name.as_str())
        .collect();
    if !still.is_empty() {
        return Some(format!("the newest epochs still wrap {}", still.join(" ")));
    }
    let short: Vec<&str> = seen
        .iter()
        .filter(|s| !s.opens_newest)
        .map(|s| s.name.as_str())
        .collect();
    if short.is_empty() {
        None
    } else {
        Some(format!("not opening the newest epoch: {}", short.join(" ")))
    }
}

// ---------------------------------------------------------------------------------------------
// The lab: worlds, processes and acts.
// ---------------------------------------------------------------------------------------------

/// A long-running child of one Mac, tethered to this lab ([`aterm_uds::spawnfd::Tether`]): its
/// handle stays kept and unreaped until [`procs::alive`] sees it exit, so its pid names it alone
/// until then.
struct Kid {
    /// Its pid (signals go here).
    pid: i32,
    /// Its start time: a pid that reads another names some other process.
    started: String,
    /// It was stopped (`SIGSTOP`).
    stopped: bool,
}

impl Kid {
    /// It is the process this lab started, still running and not stopped.
    fn running(&self) -> bool {
        !self.stopped && procs::alive(self.pid) && procs::start_time(self.pid) == self.started
    }

    /// End it: continue it if stopped, `SIGKILL` it, and wait until it is reaped.
    fn end(&mut self) {
        if procs::start_time(self.pid) == self.started {
            if self.stopped {
                let _ = crate::sys::kill(self.pid, libc::SIGCONT);
            }
            let _ = crate::sys::kill(self.pid, libc::SIGKILL);
            let _ = procs::wait_gone(self.pid, HANG);
        }
    }

    /// `SIGSTOP` or `SIGCONT` it.
    fn signal(&mut self, stop: bool) -> Result<(), String> {
        if procs::start_time(self.pid) != self.started {
            return Err(format!("pid {} is gone", self.pid));
        }
        crate::sys::kill(self.pid, if stop { libc::SIGSTOP } else { libc::SIGCONT })
            .map_err(|e| e.to_string())?;
        self.stopped = stop;
        Ok(())
    }
}

/// One Mac of the lab.
struct Mac {
    /// Its name, `mNN`.
    name: String,
    /// `<base>/mNN`.
    root: PathBuf,
    /// Its vault server's loopback port.
    port: u16,
    /// Its vault server.
    server: Option<Kid>,
    /// Its subscriber.
    member: Option<Kid>,
    /// Its seat's label in the log, once it holds one.
    label: Option<String>,
    /// Its island.
    island: Island,
}

impl Mac {
    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    fn paths(&self) -> Paths {
        Paths::for_home(&self.home())
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/v1/{VAULT}", self.port)
    }

    /// The script holds it up: both its processes started and neither stopped nor taken down.
    fn held_up(&self) -> bool {
        [&self.server, &self.member]
            .iter()
            .all(|k| k.as_ref().is_some_and(|k| !k.stopped))
    }

    /// Both its processes run, each the one this lab started, and neither is stopped.
    fn up(&self) -> bool {
        [&self.server, &self.member]
            .iter()
            .all(|k| k.as_ref().is_some_and(Kid::running))
    }
}

/// What one act in a Mac's world said.
struct Said {
    code: i32,
    stdout: String,
    stderr: String,
    /// How long it ran (milliseconds): a measurement.
    ms: u64,
}

impl Said {
    fn one_line(&self) -> String {
        let text = format!("{} {}", self.stdout.trim(), self.stderr.trim());
        let mut line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.len() > 400 {
            let cut = (0..=400)
                .rev()
                .find(|&i| line.is_char_boundary(i))
                .unwrap_or(0);
            line.truncate(cut);
            line.push('…');
        }
        line
    }
}

/// The acts a Mac's world runs ([`act_main`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Act {
    /// Seed the run's genesis from the founder's vault.
    Genesis,
    /// Write an `[everyone]` entry through the log.
    Put,
    /// Sign a removal and keep it on this Mac.
    RemoveUnposted,
    /// Point this Mac's vault source at its own server.
    Repoint,
    /// Let a Mac in by a person's Allow and the phrase.
    Desk,
    /// Let a Mac back in by its node key's proof alone.
    Prove,
}

impl Act {
    const fn word(self) -> &'static str {
        match self {
            Self::Genesis => "genesis",
            Self::Put => "put",
            Self::RemoveUnposted => "remove-unposted",
            Self::Repoint => "repoint",
            Self::Desk => "desk",
            Self::Prove => "prove",
        }
    }

    fn of(word: &str) -> Option<Self> {
        [
            Self::Genesis,
            Self::Put,
            Self::RemoveUnposted,
            Self::Repoint,
            Self::Desk,
            Self::Prove,
        ]
        .into_iter()
        .find(|a| a.word() == word)
    }
}

/// The lab's run.
struct Lab {
    world: world::World,
    /// The aterm under test, where the run found it (the world runs a proven copy).
    under_test: PathBuf,
    /// The private copy of this lab's image the driver runs from ([`launch`]), which every Mac's
    /// subscriber and act runs too: a build that relinks the lab's own path mid-run replaces
    /// nothing the run runs.
    image: PathBuf,
    args: Args,
    plan: Plan,
    t0: Instant,
    /// The checkout's commit, and whether tracked files differed from it, when the run began:
    /// what its image was built from ([`write_record`]).
    tree: Tree,
    macs: Vec<Mac>,
    signer: String,
    timeline: Vec<(f64, String)>,
}

impl Drop for Lab {
    fn drop(&mut self) {
        for mac in &mut self.macs {
            for kid in [mac.member.as_mut(), mac.server.as_mut()]
                .into_iter()
                .flatten()
            {
                kid.end();
            }
        }
    }
}

impl Lab {
    fn up(aterm: &Path, image: &Path, args: &Args) -> Result<Lab, Fail> {
        let plan = plan(args.n, args.seed);
        let mut world = world::World::new(aterm, args.keep).map_err(Fail::die)?;
        let image = image.to_path_buf();
        let ports: Vec<u16> = world::free_ports(args.n + 8)
            .map_err(|e| Fail::die(format!("cannot take loopback ports: {e}")))?
            .into_iter()
            .filter(|p| *p != aterm_net::fleet_member::DEFAULT_PORT && *p != 7484)
            .take(args.n + 1)
            .collect();
        if ports.len() < args.n + 1 {
            return Err(Fail::die("too few loopback ports"));
        }
        let mut macs = Vec::new();
        for (i, port) in ports.into_iter().enumerate() {
            let root = world.root_of(i);
            world::make_world(&root)
                .map_err(|e| Fail::die(format!("{}: its world: {e}", name(i))))?;
            let island = plan.islands[i];
            let mut extra = vec![(
                aterm_uds::partition::SEAM.to_owned(),
                world
                    .base
                    .join(format!("cut-{}", island.word()))
                    .to_string_lossy()
                    .into_owned(),
            )];
            extra.extend(control_env(args.control));
            world.extra.insert(root.clone(), extra);
            macs.push(Mac {
                name: name(i),
                root,
                port,
                server: None,
                member: None,
                label: None,
                island,
            });
        }
        let lab = Lab {
            world,
            under_test: aterm.to_path_buf(),
            image,
            args: args.clone(),
            plan,
            t0: Instant::now(),
            tree: Tree::of(&crate::paths::paths().repo),
            macs,
            signer: String::new(),
            timeline: Vec::new(),
        };
        lab.cut(false)?;
        Ok(lab)
    }

    fn say(&mut self, line: &str) {
        let at = self.t0.elapsed().as_secs_f64();
        println!("  {at:7.1}s  {line}");
        self.timeline.push((at, line.to_owned()));
    }

    /// Write both islands' cut files: each lists the other island's servers, or nothing.
    fn cut(&self, split: bool) -> Result<(), Fail> {
        for island in [Island::A, Island::B] {
            let mut text = String::from("# mesh churn: the other island's vault servers\n");
            if split {
                for mac in self.macs.iter().filter(|m| m.island != island) {
                    let _ = writeln!(text, "127.0.0.1:{}", mac.port);
                }
            }
            let file = self.world.base.join(format!("cut-{}", island.word()));
            let next = file.with_extension("new");
            crate::util::write_fresh(&next, &text)
                .and_then(|()| std::fs::rename(&next, &file))
                .map_err(|e| Fail::die(format!("the cut file {}: {e}", file.display())))?;
        }
        Ok(())
    }

    /// Start `program args` in Mac `i`'s world, tethered to this lab in a session of its own, its
    /// output in `<root>/<log>.log`.
    fn start_kept(
        &self,
        i: usize,
        program: &Path,
        args: &[&str],
        extra: &[(&str, &str)],
        log: &str,
    ) -> Result<Kid, Fail> {
        use aterm_uds::spawnfd::Tether as _;
        let mac = &self.macs[i];
        let out = crate::util::append_regular_file(&mac.root.join(format!("{log}.log")))
            .map_err(|e| Fail::die(format!("{}: its {log} log: {e}", mac.name)))?;
        let err = out.try_clone().map_err(|e| Fail::die(e.to_string()))?;
        let mut cmd = aterm_uds::spawnfd::command(program);
        cmd.args(args)
            .env_clear()
            .envs(self.world.env_of(&mac.root))
            .envs(extra.iter().copied())
            .current_dir(mac.home())
            .setsid(true)
            .tether()
            .stdin(Stdio::null())
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err));
        let pid = procs::spawn_kept(&mut cmd)
            .map_err(|e| Fail::die(format!("{}: cannot start {log}: {e}", mac.name)))?;
        Ok(Kid {
            pid,
            started: procs::start_time(pid),
            stopped: false,
        })
    }

    /// Start Mac `i`'s vault server and wait until it listens.
    fn start_server(&mut self, i: usize) -> Result<(), Fail> {
        if let Some(mut old) = self.macs[i].server.take() {
            old.end();
        }
        let listen = format!("127.0.0.1:{}", self.macs[i].port);
        let store = self.macs[i].root.join("served");
        let store = store.to_string_lossy().into_owned();
        let aterm = self.world.aterm.clone();
        let kid = self.start_kept(
            i,
            &aterm,
            &[
                "secrets",
                "serve",
                "--listen",
                &listen,
                "--no-advertise",
                "--no-keep-awake",
                "--store",
                &store,
            ],
            &[],
            "server",
        )?;
        self.macs[i].server = Some(kid);
        let addr: std::net::SocketAddr = listen.parse().map_err(|_| Fail::die("an address"))?;
        let end = Instant::now() + HANG;
        while std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_err() {
            let gone = !self.macs[i].server.as_ref().is_some_and(Kid::running);
            if gone || Instant::now() >= end {
                return Err(Fail::die(format!(
                    "{}'s vault server {} on {listen}: {}",
                    self.macs[i].name,
                    if gone {
                        "exited before it listened".to_owned()
                    } else {
                        format!("did not listen within {HANG:?}")
                    },
                    world::tail(&self.macs[i].root.join("server.log"), 8)
                )));
            }
            stop::sleep(Duration::from_millis(50))?;
        }
        Ok(())
    }

    /// Start Mac `i`'s subscriber ([`member_main`]).
    fn start_member(&mut self, i: usize) -> Result<(), Fail> {
        if let Some(mut old) = self.macs[i].member.take() {
            old.end();
        }
        let exe = self.image.clone();
        let home = self.macs[i].home().to_string_lossy().into_owned();
        let kid = self.start_kept(
            i,
            &exe,
            &[MEMBER_VERB, "--home", &home],
            &[(MEMBER_SEAM, "1")],
            "member",
        )?;
        self.macs[i].member = Some(kid);
        Ok(())
    }

    /// Start both of Mac `i`'s processes.
    fn start(&mut self, i: usize) -> Result<(), Fail> {
        self.start_server(i)?;
        self.start_member(i)
    }

    /// End both of Mac `i`'s processes.
    fn down(&mut self, i: usize) {
        let mac = &mut self.macs[i];
        for kid in [mac.member.as_mut(), mac.server.as_mut()]
            .into_iter()
            .flatten()
        {
            kid.end();
        }
        mac.member = None;
        mac.server = None;
    }

    /// `SIGSTOP` (or `SIGCONT`) both of Mac `i`'s processes.
    fn sleep(&mut self, i: usize, stop: bool) -> Result<(), Fail> {
        let mac = &mut self.macs[i];
        for kid in [mac.server.as_mut(), mac.member.as_mut()]
            .into_iter()
            .flatten()
        {
            kid.signal(stop)
                .map_err(|e| Fail::die(format!("{}: {e}", mac.name)))?;
        }
        Ok(())
    }

    /// `aterm <args>` in Mac `i`'s world, under the hang detector.
    fn aterm(&self, i: usize, args: &[&str]) -> Said {
        let mut cmd = aterm_uds::spawnfd::command(&self.world.aterm);
        self.run_in(i, &mut cmd, args)
    }

    /// `<this lab> mesh-churn-act --home <home> <act> <words>` in Mac `i`'s world.
    fn act(&self, i: usize, act: Act, words: &[&str]) -> Said {
        let mut cmd = aterm_uds::spawnfd::command(&self.image);
        let home = self.macs[i].home().to_string_lossy().into_owned();
        let mut all = vec![ACT_VERB, "--home", &home, act.word()];
        all.extend_from_slice(words);
        self.run_in(i, &mut cmd, &all)
    }

    fn run_in(&self, i: usize, cmd: &mut aterm_uds::spawnfd::Command, args: &[&str]) -> Said {
        let mac = &self.macs[i];
        cmd.env_clear()
            .envs(self.world.env_of(&mac.root))
            .args(args)
            .current_dir(mac.home());
        let start = Instant::now();
        // An interrupt ends the act and its group at once, as its deadline would.
        let ran = aterm_uds::deadline::output_until_from_unless(
            cmd,
            Stdio::null(),
            start + ACT_HANG,
            &mut || stop::check().is_err(),
        );
        let ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        match ran {
            Ok(out) => Said {
                code: out.status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                ms,
            },
            Err(aterm_uds::deadline::RunError::TimedOut) => Said {
                code: -1,
                stdout: String::new(),
                stderr: if stop::check().is_err() {
                    "interrupted".to_owned()
                } else {
                    format!(
                        "still running {HANG:?} past the product's longest exchange (the hang \
                         detector)"
                    )
                },
                ms,
            },
            Err(e) => Said {
                code: -1,
                stdout: String::new(),
                stderr: format!("{e:?}"),
                ms,
            },
        }
    }

    /// [`Lab::act`] that must exit 0: its stdout.
    fn act_ok(&self, i: usize, act: Act, words: &[&str]) -> Result<String, Fail> {
        let said = self.act(i, act, words);
        if said.code == 0 {
            Ok(said.stdout)
        } else {
            Err(Fail::die(format!(
                "{}: {} exited {}: {}",
                self.macs[i].name,
                act.word(),
                said.code,
                said.one_line()
            )))
        }
    }

    /// Read every Mac, by index.
    fn read_all(&self) -> Vec<Seen> {
        self.macs
            .iter()
            .map(|m| see(&m.name, &m.paths(), m.up()))
            .collect()
    }

    /// Wait until `macs` are [`settled`], under the hang detector past `schedule`: `Ok` with how
    /// long it took, or `Err` with why not. A Mac among them whose process is not running (each
    /// is one the script holds up, [`Lab::current_up`]) fails it at once, named with its log.
    fn await_settled(
        &mut self,
        what: &str,
        macs: &[usize],
        need: Need<'_>,
        schedule: Schedule,
    ) -> Result<u64, Unsettled> {
        self.await_settled_from(what, macs, need, schedule, Instant::now())
    }

    /// [`Lab::await_settled`], the time it reports measured from `from`, when what it waits for
    /// reached the Macs; the hang detector counts from the wait's own start.
    fn await_settled_from(
        &mut self,
        what: &str,
        macs: &[usize],
        need: Need<'_>,
        schedule: Schedule,
        from: Instant,
    ) -> Result<u64, Unsettled> {
        let start = Instant::now();
        let mut last_move = start;
        let mut last: Option<Vec<Seen>> = None;
        loop {
            stop::check().map_err(|_| Unsettled::Stopped)?;
            if let Some(&i) = macs.iter().find(|&&i| !self.macs[i].up()) {
                let mac = &self.macs[i];
                let (log, what_runs) = if mac.member.as_ref().is_some_and(Kid::running) {
                    ("server", "vault server")
                } else {
                    ("member", "subscriber")
                };
                let why = format!(
                    "{}'s {what_runs} is not running: {}",
                    mac.name,
                    world::tail(&mac.root.join(format!("{log}.log")), 4)
                );
                self.say(&format!("{what}: {why}"));
                return Err(Unsettled::Down(why));
            }
            let all: Vec<Seen> = macs
                .iter()
                .map(|&i| see(&self.macs[i].name, &self.macs[i].paths(), true))
                .collect();
            let refs: Vec<&Seen> = all.iter().collect();
            let why = settled(&refs, need);
            if why.is_none() {
                let ms = elapsed_ms(from);
                self.say(&format!(
                    "{what}: {} Macs settled after {:.1}s",
                    macs.len(),
                    ms as f64 / 1000.0
                ));
                return Ok(ms);
            }
            let moved = last.as_ref().is_none_or(|prev| {
                prev.iter().zip(&all).any(|(a, b)| {
                    a.heads != b.heads
                        || a.records != b.records
                        || a.opens_newest != b.opens_newest
                        || a.newest_wrap != b.newest_wrap
                })
            });
            if moved {
                last_move = Instant::now();
                last = Some(all);
            }
            match ended(Instant::now(), start, last_move, schedule.0) {
                None => {}
                Some(Ended::Still) => {
                    let why = why.unwrap_or_default();
                    self.say(&format!(
                        "{what}: not settled; nothing moved for {HANG:?} past the product's {} \
                         ({why})",
                        schedule.1
                    ));
                    return Err(Unsettled::Hung(why));
                }
                Some(Ended::Livelock) => {
                    let why = format!(
                        "a livelock: still moving {LIVELOCK:?} past the product's {} and the hang \
                         detector ({})",
                        schedule.1,
                        why.unwrap_or_default()
                    );
                    self.say(&format!("{what}: not settled; {why}"));
                    return Err(Unsettled::Hung(why));
                }
            }
            stop::sleep(Duration::from_millis(500)).map_err(|_| Unsettled::Stopped)?;
        }
    }

    /// The indices of the Macs ground truth holds current that the script holds up: a wait over
    /// them fails where one of their processes is not running ([`Lab::await_settled`]).
    fn current_up(&self, current: &BTreeMap<String, String>) -> Vec<usize> {
        self.macs
            .iter()
            .enumerate()
            .filter(|(_, m)| current.contains_key(&m.name) && m.held_up())
            .map(|(i, _)| i)
            .collect()
    }

    /// Mac `i`'s join code, the one name of its seat no host name shares: a seat's label (`m`
    /// and digits) can be another Mac's host name, as ALab's are, and then `aterm members remove`
    /// asks for the code.
    fn code_of(&self, i: usize) -> String {
        see(&self.macs[i].name, &self.macs[i].paths(), true)
            .recipient
            .map(|recipient| client::join_code(&recipient))
            .unwrap_or_default()
    }

    /// Mac `i`'s current seat's label in its own log ([`current_seats`]).
    fn own_label(&self, i: usize) -> Option<String> {
        let paths = self.macs[i].paths();
        let me = client::read_machine_identity(&paths)
            .ok()
            .flatten()?
            .recipient();
        let code = client::join_code(&me);
        let store = client::load_records(&paths).ok()?;
        current_seats(store.log()?)
            .into_iter()
            .find(|seat| seat.code == code)
            .map(|seat| seat.label)
    }

    /// The end's read of every Mac, once the script's last wait settled: each subscriber ends
    /// first, so nothing holds a vault's state lock and each per-seat view reads ([`Seen::seats`]);
    /// each Mac reads as up or down as it was just before.
    fn read_end(&mut self) -> Vec<Seen> {
        let up: Vec<bool> = self.macs.iter().map(Mac::up).collect();
        for mac in &mut self.macs {
            if let Some(mut kid) = mac.member.take() {
                kid.end();
            }
        }
        self.macs
            .iter()
            .zip(up)
            .map(|(m, up)| see_whole(&m.name, &m.paths(), up))
            .collect()
    }
}

/// How a wait that has not settled ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    /// Nothing moved for the hang detector, past the product's schedule.
    Still,
    /// Its Macs kept moving [`LIVELOCK`] past the schedule and the hang detector.
    Livelock,
}

/// Whether a wait begun at `start`, whose Macs last moved at `last_move`, ends `now` with the
/// product given `schedule`: once nothing moved for [`HANG`] past the schedule, or, moving still,
/// [`LIVELOCK`] past that; `None` while it goes on.
#[must_use]
pub fn ended(
    now: Instant,
    start: Instant,
    last_move: Instant,
    schedule: Duration,
) -> Option<Ended> {
    let past = start + schedule + HANG;
    if now < past {
        None
    } else if now >= last_move + HANG {
        Some(Ended::Still)
    } else if now >= past + LIVELOCK {
        Some(Ended::Livelock)
    } else {
        None
    }
}

/// What a wait gives the product before its hang detector runs: how long, and whose timer.
#[derive(Clone, Copy, Debug)]
struct Schedule(Duration, &'static str);

impl Schedule {
    const ROUND: Self = Self(ROUND, "subscriber round (error nap and long-poll)");
    const COALESCE: Self = Self(
        Duration::from_secs(REMOVER_GRACE.as_secs() + ROUND.as_secs()),
        "remover grace, then a round",
    );
    const HEAL: Self = Self(
        Duration::from_secs(SETTLE_GRACE.as_secs() + 3 * GAP_STEP.as_secs() + ROUND.as_secs()),
        "settle grace, three heal rank steps, then a round",
    );
}

// ---------------------------------------------------------------------------------------------
// The script.
// ---------------------------------------------------------------------------------------------

/// A step's start, and how to record it.
struct Timed(Instant);

impl Timed {
    fn now() -> Self {
        Self(Instant::now())
    }

    /// The step: whether every act of it ran, whether every wait of it settled, and what it
    /// found.
    fn step(&self, name: &str, ok: bool, settled: bool, note: impl Into<String>) -> Step {
        Step {
            name: name.to_owned(),
            ok,
            settled,
            ms: u64::try_from(self.0.elapsed().as_millis()).unwrap_or(u64::MAX),
            note: note.into(),
        }
    }
}

/// The line [`act_main`] prints for `--ran`: the proof that an image of this lab ran.
const RAN: &str = "mesh-churn-act ran";

/// The verb [`launch`] starts a churn run's driver with: `mesh churn-driver <churn's words>`, run
/// from the private copy of the image it drives from. A verb, not a variable, because a lab forgets
/// its caller's aterm variables as it starts (`paths::forgotten`); a driver never launches again.
const DRIVER: &str = "churn-driver";

/// The image this driver drives a churn run from: the one it runs from, which its tethered spawns
/// must run through ([`aterm_uds::tether`]).
///
/// # Errors
/// Its tethered spawns would run through another image, or none.
fn own_image() -> Result<PathBuf, String> {
    let real =
        |path: &Path| std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()));
    let exe = real(&crate::paths::paths().exe)?;
    let through = aterm_uds::tether::executable().map(real).transpose()?;
    if through.as_ref() != Some(&exe) {
        return Err(format!(
            "this driver's tethered spawns would run through {through:?}, not its image {}",
            exe.display()
        ));
    }
    Ok(exe)
}

/// Run the churn from a private copy of this lab's image: copy it into a folder of its own
/// (`/tmp/af.*/.ubd/app.noindex/aterm-lab`, read and run only), prove the copy runs by the line it
/// prints ([`RAN`]), start it as the run's driver ([`DRIVER`], `words`, with the run's seed and
/// the aterm under test made explicit) in a session of its own, tethered to this process, and
/// answer its
/// status. The driver's own tethered spawns then run through that copy, which nothing relinks, so
/// a build that relinks this lab's path mid-run leaves every process the run starts tethered and
/// on the one build the driver runs; a tether this process recorded is only ever used for the
/// driver, at once: a relink between this process's start and the driver's spawn leaves the driver
/// untethered, and that driver is killed at once and the run refused. An interrupt here is handed
/// to the driver once, which ends its run; a second ends the driver at once ([`end_driven`]) and
/// then this process. The folder goes once the driver ends, and stays where the proof failed,
/// since macOS may still be assessing the copy.
fn launch(words: &[String], seed: u64, aterm: &Path) -> Res {
    use aterm_uds::spawnfd::Tether as _;
    use std::os::unix::fs::PermissionsExt as _;
    use std::os::unix::process::ExitStatusExt as _;
    if aterm_uds::tether::executable().is_none() {
        return Err(Fail::exit(
            3,
            "mesh churn: this lab's image was replaced since it started, so what it starts would \
             not be tethered; run it again",
        ));
    }
    let dir = crate::sys::make_temp_dir("/tmp/af.XXXXXX")
        .map(PathBuf::from)
        .map_err(|e| {
            Fail::exit(
                3,
                format!("mesh churn: no folder for this lab's image: {e}"),
            )
        })?;
    let image = dir.join(".ubd/app.noindex/aterm-lab");
    let copied = image
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| crate::util::copy_regular_file(&crate::paths::paths().exe, &image))
        .and_then(|_| std::fs::set_permissions(&image, std::fs::Permissions::from_mode(0o555)));
    if let Err(e) = copied {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(Fail::exit(
            3,
            format!(
                "mesh churn: cannot copy this lab's image to {}: {e}",
                image.display()
            ),
        ));
    }
    let mut proof = aterm_uds::spawnfd::command(&image);
    proof
        .args([ACT_VERB, "--ran"])
        .current_dir(&dir)
        .stdin(Stdio::null());
    match aterm_uds::deadline::output_within(&mut proof, HANG) {
        Ok(out) if out.status.success() && out.stdout.starts_with(RAN.as_bytes()) => {}
        other => {
            return Err(Fail::exit(
                3,
                format!(
                    "mesh churn: this lab's copy {} did not print {RAN:?}; {} is kept: {other:?}",
                    image.display(),
                    dir.display()
                ),
            ));
        }
    }
    let mut driver = aterm_uds::spawnfd::command(&image);
    driver.args(["mesh", DRIVER]).args(words);
    if !words.iter().any(|w| w == "--seed") {
        driver.args(["--seed".to_owned(), seed.to_string()]);
    }
    if !words.iter().any(|w| w == "--aterm") {
        driver.arg("--aterm").arg(aterm);
    }
    driver.setsid(true).tether().stdin(Stdio::null());
    let ran = driver.spawn().and_then(|mut child| {
        let pid = i32::try_from(child.id()).unwrap_or(0);
        // A file's identity does not come back: a tether this process reads as gone now was gone
        // when the spawn routed the driver, which then started with no watcher.
        if aterm_uds::tether::executable().is_none() {
            let _ = crate::sys::kill(pid, libc::SIGKILL);
            child.wait()?;
            return Err(std::io::Error::other(
                "this lab's image was replaced while its driver started, so the driver was not \
                 tethered and was killed; run it again",
            ));
        }
        if DRIVEN.set((pid, dir.clone())).is_ok() {
            stop::at_hard_exit(end_driven);
        }
        loop {
            if let Some(status) = child.try_wait()? {
                break Ok(status);
            }
            if let Err(stop::Stopped(sig)) = stop::sleep(Duration::from_millis(200)) {
                let _ = crate::sys::kill(pid, sig);
                break child.wait();
            }
        }
    });
    let _ = std::fs::remove_dir_all(&dir);
    let status = ran.map_err(|e| Fail::exit(3, format!("mesh churn: the driver: {e}")))?;
    Ok(status
        .code()
        .or_else(|| status.signal().map(|sig| 128 + sig))
        .unwrap_or(3))
}

/// The driver a launcher started and the folder its image is in, for a second interrupt to end
/// ([`end_driven`]).
static DRIVEN: std::sync::OnceLock<(i32, PathBuf)> = std::sync::OnceLock::new();

/// A launcher's undo when a second interrupt ends it at once: its driver ends by `SIGKILL`, so it
/// starts nothing once its image is gone (a spawn through a gone image runs untethered), and is
/// waited for, the hang detector at most, as the launcher's own wait reaps it; then the folder
/// goes. The driver's processes end by their own tethers to it.
fn end_driven() {
    let Some((pid, dir)) = DRIVEN.get() else {
        return;
    };
    let _ = crate::sys::kill(*pid, libc::SIGKILL);
    let until = Instant::now() + HANG;
    while crate::sys::kill(*pid, 0).is_ok() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// Milliseconds since `at`: a measurement.
fn elapsed_ms(at: Instant) -> u64 {
    u64::try_from(at.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// A random synthetic value, hex: a lab entry's value, never a secret.
fn synthetic() -> Result<String, Fail> {
    aterm_uds::rand::hex_token::<16>().map_err(|e| Fail::die(format!("random bytes: {e}")))
}

/// Found the lab on m00 ([`Lab::found`]'s doc says why this way), then let every other Mac in.
fn bring_up(lab: &mut Lab, ev: &mut Evidence) -> Result<(), Fail> {
    let t = Timed::now();
    lab.found()?;
    lab.say("m00 founded the lab: its genesis, its [everyone] epoch, the servers policy");
    let mut admit_ms = Vec::new();
    for i in 1..lab.args.n {
        let at = Instant::now();
        lab.admit(0, i)?;
        admit_ms.push(u64::try_from(at.elapsed().as_millis()).unwrap_or(u64::MAX));
    }
    let all: Vec<usize> = (0..lab.args.n).collect();
    let settled = waited(lab.await_settled(
        "bring-up",
        &all,
        Need::Key(&BTreeMap::new()),
        Schedule::ROUND,
    ));
    for i in 0..lab.args.n {
        lab.macs[i].label = lab.own_label(i);
        if let Some(label) = lab.macs[i].label.clone() {
            ev.current.insert(lab.macs[i].name.clone(), label);
        }
    }
    let mean = admit_ms.iter().sum::<u64>() / admit_ms.len().max(1) as u64;
    let max = admit_ms.iter().max().copied().unwrap_or(0);
    let unlabeled: Vec<String> = (0..lab.args.n)
        .filter(|&i| lab.macs[i].label.is_none())
        .map(name)
        .collect();
    record(
        ev,
        t.step(
            "bring-up",
            unlabeled.is_empty() && settled.is_ok(),
            !matches!(settled, Ok(Some(_))),
            format!(
                "{} Macs let in by m00's desk (each admission {:.1}s mean, {:.1}s max){}{}",
                lab.args.n - 1,
                mean as f64 / 1000.0,
                max as f64 / 1000.0,
                match &settled {
                    Ok(None) => String::new(),
                    Ok(Some(why)) => format!("; did not settle: {why}"),
                    Err(why) => format!("; broke: {why}"),
                },
                if unlabeled.is_empty() {
                    String::new()
                } else {
                    format!("; no seat: {}", unlabeled.join(" "))
                }
            ),
        ),
    )?;
    if unlabeled.is_empty() {
        Ok(())
    } else {
        Err(Fail::die(format!("no seat for {}", unlabeled.join(" "))))
    }
}

impl Lab {
    /// Found the lab on m00. This is the one place a lab is founded, so D1's `aterm fleet init`
    /// replaces it alone. Today a membership log's genesis comes only from a vault `aterm secrets
    /// create` made (it founds Commons, whose control the members desk needs to let anyone in)
    /// and [`client::seed_vault_genesis`]; nothing here mints a fleet secret or names a second
    /// admin. `secrets create` makes m00 the vault's admin, so the subscriber started last runs
    /// the envelope approver every admin's does; the lab lets no Mac in through it. m00's
    /// `[everyone]` holds one synthetic entry, so its founding epoch seals something, and the
    /// servers policy every Mac's subscriber reads its peers from
    /// ([`aterm_secrets::SWARM_POLICY_ENTRY`], a member's write through the log).
    fn found(&mut self) -> Result<(), Fail> {
        self.start_server(0)?;
        let seed_dir = self.macs[0].root.join("tmp/seed");
        std::fs::create_dir_all(&seed_dir).map_err(|e| Fail::die(e.to_string()))?;
        crate::util::write_fresh(&seed_dir.join(SEED_ENTRY), synthetic()?)
            .map_err(|e| Fail::die(e.to_string()))?;
        let url = self.macs[0].url();
        let seed = seed_dir.to_string_lossy().into_owned();
        let said = self.aterm(
            0,
            &[
                "secrets",
                "create",
                &url,
                "--import",
                &seed,
                "--name",
                "m00",
                "--no-agent",
            ],
        );
        if said.code != 0 {
            return Err(Fail::die(format!(
                "m00's secrets create: {}",
                said.one_line()
            )));
        }
        self.signer = said
            .stdout
            .lines()
            .find_map(|l| l.strip_prefix("signer: "))
            .and_then(|rest| rest.split_whitespace().next())
            .map(str::to_owned)
            .ok_or_else(|| {
                Fail::die(format!(
                    "m00's create printed no signer: {}",
                    said.one_line()
                ))
            })?;
        self.act_ok(0, Act::Genesis, &[])?;
        let servers: Vec<String> = self.macs[..self.args.n]
            .iter()
            .map(|m| format!("\"{}\"", m.url()))
            .collect();
        let policy = format!("servers = [{}]\n", servers.join(", "));
        self.act_ok(0, Act::Put, &[aterm_secrets::SWARM_POLICY_ENTRY, &policy])?;
        self.start_member(0)
    }

    /// `by` lets Mac `who` in by a person's Allow and the phrase on its desk, then `who` points
    /// its vault source at its own server and its subscriber starts. Returns what the desk said.
    fn admit(&mut self, by: usize, who: usize) -> Result<String, Fail> {
        if self.macs[who].server.is_none() {
            self.start_server(who)?;
        }
        let home = self.macs[who].home().to_string_lossy().into_owned();
        let said = self.act_ok(by, Act::Desk, &[&home, &name(who)])?;
        self.repoint(who)?;
        self.start_member(who)?;
        Ok(said.trim().to_owned())
    }

    /// Point Mac `who`'s vault source at its own server: every Mac here shares 127.0.0.1, where
    /// one shared source would pass a server's sixteen connections from one address
    /// (`server.rs` `MAX_PEER_CONNECTIONS`), which distinct LAN addresses never meet.
    fn repoint(&mut self, who: usize) -> Result<(), Fail> {
        let spec = format!("{}#{}", self.macs[who].url(), self.signer);
        self.act_ok(who, Act::Repoint, &[&spec, &name(who)])
            .map(drop)
    }
}

/// How a step that acts and then waits went: `Err` where an act did not run or the run broke
/// under a wait, `Ok(Some(why))` where every act ran but a wait did not settle, `Ok(None)` where
/// it all did.
type Went = Result<Option<String>, String>;

/// How a wait over the Macs ended short of their settling ([`Lab::await_settled`]).
#[derive(Debug)]
enum Unsettled {
    /// An interrupt arrived: the run's, which [`run`] ends after the step it cut short.
    Stopped,
    /// A process of a Mac the script holds up is not running: the run broke, whatever the
    /// product would have done next.
    Down(String),
    /// Nothing it watched moved for the hang detector past the product's schedule.
    Hung(String),
}

/// What a step makes of its wait ([`Went`]): where a Mac's process is down, or an interrupt cut
/// the wait short, the step did not run; where nothing moved, its wait did not settle.
fn waited(wait: Result<u64, Unsettled>) -> Went {
    match wait {
        Ok(_) => Ok(None),
        Err(Unsettled::Hung(why)) => Ok(Some(why)),
        Err(Unsettled::Down(why)) => Err(why),
        Err(Unsettled::Stopped) => Err("interrupted".to_owned()),
    }
}

/// Record `step`, then end the run where an interrupt arrived: a run cut short is never judged
/// or recorded, and exits with the interrupt's status.
///
/// # Errors
/// The run was interrupted.
fn record(ev: &mut Evidence, step: Step) -> Result<(), stop::Stopped> {
    ev.steps.push(step);
    stop::check()
}

impl Timed {
    /// The step `went` made, `done` saying what it did.
    fn went(&self, name: &str, went: Went, done: &str) -> Step {
        match went {
            Ok(None) => self.step(name, true, true, done),
            Ok(Some(why)) => self.step(name, true, false, format!("{done}; {why}")),
            Err(why) => self.step(name, false, true, why),
        }
    }
}

/// Run the script on a brought-up lab.
#[allow(clippy::too_many_lines)]
fn run(lab: &mut Lab) -> Result<Evidence, Fail> {
    let mut ev = Evidence {
        control: lab.args.control,
        ..Evidence::default()
    };
    bring_up(lab, &mut ev)?;
    let plan = lab.plan.clone();

    // 2. The sleepers stop, and the two Macs that restart go down, so each misses what follows.
    let slept = Instant::now();
    for s in plan.sleepers {
        lab.sleep(s, true)?;
    }
    for q in plan.restarts {
        lab.down(q);
    }
    lab.say(&format!(
        "{} and {} stopped (SIGSTOP) for {} min; {} and {} killed",
        name(plan.sleepers[0]),
        name(plan.sleepers[1]),
        lab.args.minutes,
        name(plan.restarts[0]),
        name(plan.restarts[1])
    ));

    // 3. Two removals at once, while those four are away.
    let t = Timed::now();
    let codes: Vec<String> = plan
        .removals
        .iter()
        .map(|&(_, removed)| lab.code_of(removed))
        .collect();
    let said: Vec<Said> = std::thread::scope(|scope| {
        let lab = &*lab;
        let runs: Vec<_> = plan
            .removals
            .iter()
            .zip(&codes)
            .map(|(&(remover, _), code)| {
                scope.spawn(move || lab.aterm(remover, &["members", "remove", code]))
            })
            .collect();
        runs.into_iter()
            .map(|h| {
                h.join().unwrap_or(Said {
                    code: -1,
                    stdout: String::new(),
                    stderr: "panicked".to_owned(),
                    ms: 0,
                })
            })
            .collect()
    });
    for (&(remover, removed), s) in plan.removals.iter().zip(&said) {
        lab.say(&format!(
            "{} removes {} ({}) in {:.1}s: {}",
            name(remover),
            name(removed),
            lab.macs[removed].label.as_deref().unwrap_or("?"),
            s.ms as f64 / 1000.0,
            s.one_line()
        ));
        ev.current.remove(&name(removed));
        if let Some(recipient) = see(&name(removed), &lab.macs[removed].paths(), true).recipient {
            ev.removed.insert(name(removed), recipient);
        }
    }
    let went = if said.iter().all(|s| s.code == 0) {
        let watch = lab.current_up(&ev.current);
        waited(lab.await_settled(
            "concurrent removals",
            &watch,
            Need::Key(&ev.removed),
            Schedule::COALESCE,
        ))
    } else {
        Err(format!(
            "a removal did not run: {}",
            said.iter()
                .filter(|s| s.code != 0)
                .map(Said::one_line)
                .collect::<Vec<_>>()
                .join("; ")
        ))
    };
    record(
        &mut ev,
        t.went(
            "concurrent-removals",
            went,
            &format!(
                "two removals at once ({}), then one frontier whose newest epochs every up member \
             opens and no removed Mac does",
                said.iter()
                    .map(|s| format!("{:.1}s", s.ms as f64 / 1000.0))
                    .collect::<Vec<_>>()
                    .join(" and ")
            ),
        ),
    )?;

    // 4. The two killed Macs start again and read up.
    let t = Timed::now();
    let back = Instant::now();
    for q in plan.restarts {
        lab.start(q)?;
    }
    lab.say(&format!(
        "{} and {} started again",
        name(plan.restarts[0]),
        name(plan.restarts[1])
    ));
    let read = read_up(lab, &mut ev, &plan.restarts, "restarted", back, "restarts");
    record(
        &mut ev,
        t.went(
            "restarts",
            read,
            "two Macs killed before the removals, started after them, and read up",
        ),
    )?;

    // 5. The sleepers wake once their minutes are up.
    let t = Timed::now();
    let wake_at = slept + Duration::from_secs(lab.args.minutes * 60);
    while Instant::now() < wake_at {
        stop::sleep(Duration::from_millis(500))?;
    }
    for s in plan.sleepers {
        lab.sleep(s, false)?;
    }
    let woke = Instant::now();
    lab.say(&format!(
        "{} and {} continued (SIGCONT) after {:.0}s stopped",
        name(plan.sleepers[0]),
        name(plan.sleepers[1]),
        slept.elapsed().as_secs_f64()
    ));
    let read = read_up(lab, &mut ev, &plan.sleepers, "slept", woke, "sleepers woke");
    record(
        &mut ev,
        t.went(
            "sleep",
            read,
            &format!("two Macs stopped {} min, then read up", lab.args.minutes),
        ),
    )?;

    // 6. A rebuild, before the split ([`rebuild`] says why).
    let t = Timed::now();
    let went = rebuild(lab, &mut ev, &plan);
    record(
        &mut ev,
        t.went(
            "rebuild",
            went,
            &format!(
                "{} rebuilt with its node key kept and let back in",
                name(plan.rebuilt)
            ),
        ),
    )?;

    // 7. The split. The remover stays down through the heal whatever the split did.
    let t = Timed::now();
    let went = split(lab, &mut ev, &plan);
    lab.down(plan.split_remover);
    record(
        &mut ev,
        t.went(
            "split",
            went,
            "island A's remover killed mid-batch, its removed Mac wrote after its cutoff; island B \
         wrote and let a new Mac in",
        ),
    )?;

    // 8. The heal, the remover still down; what every Mac holds once its wait ends is the key
    // assertion's evidence ([`NEWEST_KEY`]).
    let t = Timed::now();
    lab.cut(false)?;
    lab.say("the islands heal (the cut files emptied)");
    let watch = lab.current_up(&ev.current);
    let healed = waited(lab.await_settled("heal", &watch, Need::Key(&ev.removed), Schedule::HEAL));
    ev.healed = lab.read_all();
    record(
        &mut ev,
        t.went(
            "heal",
            healed,
            "every up current member settled with island A's remover down",
        ),
    )?;

    // 9. The remover returns.
    let t = Timed::now();
    let r = plan.split_remover;
    let back = Instant::now();
    lab.start(r)?;
    lab.say(&format!("{} (island A's remover) started again", name(r)));
    let read = read_up(lab, &mut ev, &[r], "remover", back, "remover back");
    record(
        &mut ev,
        t.went(
            "remover-back",
            read,
            "island A's remover returned and every current member settled",
        ),
    )?;

    // The end, read whole, every subscriber ended ([`Lab::read_end`]).
    stop::check()?;
    ev.end = lab.read_end();
    let stores: Vec<_> = lab
        .macs
        .iter()
        .filter_map(|m| client::load_records(&m.paths()).ok())
        .collect();
    let (most, histogram) = wrap_counts(stores.iter().filter_map(|s| s.log()));
    ev.most_wraps = most;
    ev.wrap_histogram = histogram;
    let current: Vec<usize> = (0..lab.macs.len())
        .filter(|&i| ev.current.contains_key(&lab.macs[i].name))
        .collect();
    for wrote in [ev.late.as_mut(), ev.honest.as_mut()].into_iter().flatten() {
        if let Some(id) = wrote.record {
            wrote.unheld = current
                .iter()
                .filter(|&&i| !held_by(lab, &[i]).contains_key(&id))
                .map(|&i| name(i))
                .collect();
        }
    }
    stop::check()?;
    Ok(ev)
}

/// Wait for every up current member after `macs` came back at `back`, and note each one's
/// read-up: the time to that wait's settling where it watched the Mac running, else none. What
/// the step makes of the wait ([`waited`]).
fn read_up(
    lab: &mut Lab,
    ev: &mut Evidence,
    macs: &[usize],
    how: &'static str,
    back: Instant,
    what: &str,
) -> Went {
    let watch = lab.current_up(&ev.current);
    let read = lab.await_settled(what, &watch, Need::Key(&ev.removed), Schedule::ROUND);
    for &i in macs {
        ev.read_ups.push(ReadUp {
            mac: name(i),
            how,
            ms: (read.is_ok() && watch.contains(&i)).then(|| elapsed_ms(back)),
        });
    }
    waited(read)
}

/// Step 6: rebuild one Mac with its node key kept and let it back in, before the split. Its way
/// back rotates `[everyone]` (a `Remove` of its old seat; the design's `Replace` rotates too),
/// and a rotation during the split would coalesce with the killed remover's partial epoch after
/// the heal and cover its gap, so the heal would pass without the gap rule ever running.
fn rebuild(lab: &mut Lab, ev: &mut Evidence, plan: &Plan) -> Went {
    let fail = |e: Fail| e.msg.unwrap_or_default();
    let (d, x) = (plan.admitter, plan.rebuilt);
    let x_name = name(x);
    // The rebuild: its vault folder and served store go, its node key stays. It asks to come
    // back as a proof by that key would, then as a person's Allow; where the product refuses
    // both, a person decides: the admitter removes the old seat (`aterm members remove`, the CLI)
    // and presses "Allow again", which restores the Mac under its old label.
    let old_label = lab.macs[x].label.clone().unwrap_or_default();
    let old_code = lab.code_of(x);
    let old_recipient = see(&name(x), &lab.macs[x].paths(), true).recipient;
    lab.down(x);
    let node = aterm_net::fleet_member::read_node_identity(&lab.macs[x].root.join("state"))
        .ok()
        .flatten()
        .map(|n| *n.key().bytes());
    for dir in [
        lab.macs[x].home().join(".config/aterm/secrets"),
        lab.macs[x].root.join("served"),
    ] {
        if let Err(e) = std::fs::remove_dir_all(&dir)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            return Err(format!("rebuilding {}: {e}", name(x)));
        }
    }
    lab.start_server(x).map_err(fail)?;
    let home = lab.macs[x].home().to_string_lossy().into_owned();
    let mut rebuilt = Rebuilt {
        mac: name(x),
        node,
        ..Rebuilt::default()
    };
    for (path, act, words) in [
        ("proof", Act::Prove, vec![home.as_str(), x_name.as_str()]),
        ("phrase", Act::Desk, vec![home.as_str(), x_name.as_str()]),
    ] {
        let said = lab.act(d, act, &words);
        if said.code == 0 {
            rebuilt.path = path.to_owned();
            break;
        }
        rebuilt.refused.push((path.to_owned(), said.one_line()));
    }
    if rebuilt.path.is_empty() {
        let removed = lab.aterm(d, &["members", "remove", &old_code]);
        lab.say(&format!(
            "{} removes {}'s old seat ({old_label}): {}",
            name(d),
            name(x),
            removed.one_line()
        ));
        if removed.code != 0 {
            return Err(format!(
                "removing {}'s old seat: {}",
                name(x),
                removed.one_line()
            ));
        }
        if let Some(recipient) = old_recipient {
            ev.removed
                .insert(format!("{} (old seat)", name(x)), recipient);
        }
        lab.act_ok(d, Act::Desk, &[&home, &x_name, "again"])
            .map_err(fail)?;
        rebuilt.path = "allow-again".to_owned();
    }
    lab.repoint(x).map_err(fail)?;
    let back = Instant::now();
    lab.start_member(x).map_err(fail)?;
    lab.say(&format!(
        "{} rebuilt with its node key kept, back in by {}{}",
        name(x),
        rebuilt.path,
        rebuilt
            .refused
            .iter()
            .map(|(path, why)| format!("; its {path} was refused: {why}"))
            .collect::<String>()
    ));
    lab.macs[x].label = lab.own_label(x);
    if let Some(label) = lab.macs[x].label.clone() {
        ev.current.insert(name(x), label);
    }
    ev.rebuilt = Some(rebuilt);
    read_up(lab, ev, &[x], "rebuilt", back, "rebuilt")
}

/// The ids of the records Mac `i` signed between `before` (the ids its log held) and now, of
/// `kind`: what one of its acts just wrote.
fn signed_since(lab: &Lab, i: usize, before: &BTreeSet<RecordId>, kind: Kind) -> Vec<RecordId> {
    let paths = lab.macs[i].paths();
    let Some(me) = client::read_machine_identity(&paths)
        .ok()
        .flatten()
        .map(|id| id.recipient())
    else {
        return Vec::new();
    };
    let Ok(store) = client::load_records(&paths) else {
        return Vec::new();
    };
    let Some(log) = store.log() else {
        return Vec::new();
    };
    let membership = log.membership();
    log.records()
        .filter(|rec| rec.kind() == kind && !before.contains(rec.id()))
        .filter(|rec| {
            membership
                .get(rec.actor())
                .is_some_and(|m| m.binding.recipient() == me)
        })
        .map(|rec| *rec.id())
        .collect()
}

/// Mac `i` writes `entry` (a synthetic value) through its log: the write, or why it did not run.
fn write(lab: &mut Lab, i: usize, entry: &str) -> Result<Wrote, String> {
    let before: BTreeSet<RecordId> = held_by(lab, &[i]).into_keys().collect();
    let value = synthetic().map_err(|e| e.msg.unwrap_or_default())?;
    let said = lab.act(i, Act::Put, &[entry, &value]);
    lab.say(&format!("{} writes {entry}: {}", name(i), said.one_line()));
    if said.code != 0 {
        return Err(format!(
            "{}'s write of {entry}: {}",
            name(i),
            said.one_line()
        ));
    }
    let put = signed_since(lab, i, &before, Kind::Put);
    let [record] = put.as_slice() else {
        return Err(format!(
            "{}'s write of {entry} left {} new Put records of its own in its log, not one",
            name(i),
            put.len()
        ));
    };
    Ok(Wrote {
        entry: entry.to_owned(),
        digest: digest(value.as_bytes()),
        record: Some(*record),
        unheld: Vec::new(),
    })
}

/// Step 7: split the islands. On island A the remover's subscriber dies, it signs a removal it
/// keeps, the Mac it removes writes after that cutoff (before anything could tell it), a prefix
/// of the batch reaches half of A's servers, and the remover goes down. Island B writes and
/// admits the newcomer. Each island's records spread over it, and the ledger is read.
#[allow(clippy::too_many_lines)]
fn split(lab: &mut Lab, ev: &mut Evidence, plan: &Plan) -> Went {
    let fail = |e: Fail| e.msg.unwrap_or_default();
    lab.cut(true).map_err(fail)?;
    let all: Vec<usize> = (0..lab.macs.len()).collect();
    let at_cut = held_by(lab, &all);
    lab.say("the Macs split into two islands (the ATERM_DEBUG_PARTITION cut)");
    let (r, c) = (plan.split_remover, plan.split_removed);
    let c_label = lab.macs[c].label.clone().unwrap_or_default();
    let c_code = lab.code_of(c);

    // Island A: the remover's subscriber dies first, so nothing carries its batch but the lab's
    // prefix, and it signs the whole batch.
    if let Some(mut kid) = lab.macs[r].member.take() {
        kid.end();
    }
    let before: BTreeSet<RecordId> = held_by(lab, &[r]).into_keys().collect();
    let signed = lab
        .act_ok(r, Act::RemoveUnposted, &[&c_code])
        .map_err(fail)?;

    // The removed Mac writes after its cutoff: the removal fixed it as c's latest record r held,
    // and no record of the removal has left r yet, so c writes as a member that heard nothing.
    ev.late = Some(write(lab, c, LATE_ENTRY)?);

    // A prefix of the batch lands on half of A's servers, and the remover is killed.
    let store = client::load_records(&lab.macs[r].paths()).map_err(|e| e.to_string())?;
    let log = store.log().ok_or("the remover holds no log")?;
    let mut batch: Vec<&Record> = log
        .records()
        .filter(|rec| !before.contains(rec.id()))
        .collect();
    batch.sort_by_key(|rec| rec.sequence());
    let prefix = partial(&batch);
    let a_servers: Vec<usize> = plan
        .on(Island::A)
        .into_iter()
        .filter(|&i| i != r && i < lab.macs.len() && lab.macs[i].server.is_some())
        .collect();
    let reached: Vec<usize> = a_servers.iter().copied().step_by(2).collect();
    let wire = aterm_secrets::membership_log::encode_records(prefix.iter().copied());
    for &i in &reached {
        aterm_secrets::transport::post_records(&lab.macs[i].url(), &wire)
            .map_err(|e| format!("the prefix to {}: {e}", name(i)))?;
    }
    let reached_at = Instant::now();
    lab.down(r);
    lab.say(&format!(
        "{} removed {} ({c_label}): signed {} records ({}), {} reached {} of island A's {} servers; \
         then it was killed",
        name(r),
        name(c),
        batch.len(),
        signed.trim(),
        prefix.len(),
        reached.len(),
        a_servers.len()
    ));
    ev.current.remove(&name(c));
    if let Some(recipient) = see(&name(c), &lab.macs[c].paths(), true).recipient {
        ev.removed.insert(name(c), recipient);
    }

    // Island B: a write and the newcomer.
    let (w, d, b) = (plan.writer, plan.admitter, plan.newcomer);
    ev.honest = Some(write(lab, w, SPLIT_ENTRY)?);
    let wrote_at = Instant::now();
    let said = lab.admit(d, b).map_err(fail)?;
    lab.say(&format!(
        "{} lets the newcomer {} in: {said}",
        name(d),
        name(b)
    ));
    lab.macs[b].label = lab.own_label(b);
    ev.newcomer = Some(name(b));
    if let Some(label) = lab.macs[b].label.clone() {
        ev.current.insert(name(b), label);
    }

    // Each island's records spread over it: one frontier, every Mac holding what reached the
    // island (A the prefix its servers took, B its write); neither knows the other's removal, and
    // island A's gap waits for the heal. Each island's time runs from what reached it; island B is
    // read once island A's wait ends, so its time bounds its spread from above. Then the ledger is
    // read.
    let reached_a: BTreeSet<RecordId> = prefix.iter().map(|rec| *rec.id()).collect();
    let reached_b: BTreeSet<RecordId> = ev.honest.iter().filter_map(|w| w.record).collect();
    let (mut unsettled, mut broke) = (Vec::new(), Vec::new());
    for (island, reached, at) in [
        (Island::A, &reached_a, reached_at),
        (Island::B, &reached_b, wrote_at),
    ] {
        let watch: Vec<usize> = lab
            .current_up(&ev.current)
            .into_iter()
            .filter(|&i| lab.macs[i].island == island)
            .collect();
        match waited(lab.await_settled_from(
            &format!("island {}", island.word()),
            &watch,
            Need::Holds(reached),
            Schedule::ROUND,
            at,
        )) {
            Ok(None) => {}
            Ok(Some(why)) => unsettled.push(format!("island {}: {why}", island.word())),
            Err(why) => broke.push(format!("island {}: {why}", island.word())),
        }
    }
    let on_a = held_by(lab, &plan.on(Island::A));
    let on_b = held_by(lab, &plan.on(Island::B));
    let signers = signers(lab);
    ev.split.read = true;
    for (id, actor) in &on_a {
        if at_cut.contains_key(id) || !on_b.contains_key(id) {
            continue;
        }
        let by = signers.get(actor).map_or_else(
            || "a seat no Mac here holds".to_owned(),
            |&i| format!("{} (island {})", name(i), lab.macs[i].island.word()),
        );
        ev.split.crossed.push(format!(
            "{by} signed {} during the split, and both islands hold it",
            short(id)
        ));
    }
    if !broke.is_empty() {
        return Err(broke.join("; "));
    }
    Ok((!unsettled.is_empty()).then(|| unsettled.join("; ")))
}

/// Every record the logs of `macs` hold, with the seat that signed it.
fn held_by(
    lab: &Lab,
    macs: &[usize],
) -> BTreeMap<RecordId, aterm_secrets::membership_log::MemberId> {
    let mut held = BTreeMap::new();
    for &i in macs.iter().filter(|&&i| i < lab.macs.len()) {
        if let Ok(store) = client::load_records(&lab.macs[i].paths())
            && let Some(log) = store.log()
        {
            held.extend(log.records().map(|rec| (*rec.id(), *rec.actor())));
        }
    }
    held
}

/// Each seat any Mac's log has bound, removed ones included (a genesis founder's binding, an
/// `Admit`'s entrant), by the Mac whose identity it binds: whose a signed record is.
fn signers(lab: &Lab) -> BTreeMap<aterm_secrets::membership_log::MemberId, usize> {
    let mac_of: BTreeMap<Recipient, usize> = (0..lab.macs.len())
        .filter_map(|i| {
            client::read_machine_identity(&lab.macs[i].paths())
                .ok()
                .flatten()
                .map(|id| (id.recipient(), i))
        })
        .collect();
    let mut signers = BTreeMap::new();
    for mac in &lab.macs {
        if let Ok(store) = client::load_records(&mac.paths())
            && let Some(log) = store.log()
        {
            for record in log.records() {
                let bound: Vec<_> = match record.body() {
                    Body::Genesis { founders, .. } => founders
                        .iter()
                        .filter_map(|f| f.binding.as_ref())
                        .map(|b| (b.actor(), b.recipient()))
                        .collect(),
                    Body::Admit { entrant, .. } => {
                        let binding = entrant.request.binding();
                        vec![(binding.actor(), binding.recipient())]
                    }
                    _ => Vec::new(),
                };
                for (actor, recipient) in bound {
                    if let Some(&i) = mac_of.get(&recipient) {
                        signers.insert(actor, i);
                    }
                }
            }
        }
    }
    signers
}

/// The causal prefix of a removal batch (by its signer's sequence) that a kill in the middle of
/// it leaves delivered: through the `[everyone]` epoch and half of the `Wrap`s that follow it.
#[must_use]
pub fn partial<'a>(batch: &[&'a Record]) -> Vec<&'a Record> {
    let kinds: Vec<Kind> = batch.iter().map(|r| r.kind()).collect();
    batch[..prefix_len(&kinds)].to_vec()
}

/// How many of a batch's records, by kind in signing order, [`partial`] delivers: through its
/// first `Epoch` and half the `Wrap`s right after it; half the batch where it holds no epoch.
#[must_use]
pub fn prefix_len(kinds: &[Kind]) -> usize {
    let Some(epoch) = kinds.iter().position(|k| *k == Kind::Epoch) else {
        return kinds.len() / 2;
    };
    let wraps = kinds[epoch + 1..]
        .iter()
        .take_while(|k| **k == Kind::Wrap)
        .count();
    epoch + 1 + wraps / 2
}

// ---------------------------------------------------------------------------------------------
// The report and the record.
// ---------------------------------------------------------------------------------------------

/// The run's report.
fn report(lab: &Lab, ev: &Evidence, checks: &[Check]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "## Steps\n");
    for step in &ev.steps {
        let _ = writeln!(
            out,
            "- {} {} ({:.1}s): {}",
            match (step.ok, step.settled) {
                (true, true) => "[ran]",
                (true, false) => "[UNSETTLED]",
                (false, _) => "[FAILED]",
            },
            step.name,
            step.ms as f64 / 1000.0,
            step.note
        );
    }
    let _ = writeln!(out, "\n## Macs at the end\n");
    let _ = writeln!(
        out,
        "| Mac | island | role | seat | up | records | frontier | newest key | vault |"
    );
    let _ = writeln!(out, "|---|---|---|---|---|---|---|---|---|");
    let plan = &lab.plan;
    let role = |i: usize| -> String {
        let mut roles = Vec::new();
        if i == 0 {
            roles.push("founder");
        }
        for (k, &(remover, removed)) in plan.removals.iter().enumerate() {
            if i == remover {
                roles.push(if k == 0 { "remover 1" } else { "remover 2" });
            }
            if i == removed {
                roles.push("removed");
            }
        }
        if plan.sleepers.contains(&i) {
            roles.push("sleeper");
        }
        if plan.restarts.contains(&i) {
            roles.push("restarted");
        }
        if i == plan.split_remover {
            roles.push("split remover");
        }
        if i == plan.split_removed {
            roles.push("removed in the split");
        }
        if i == plan.writer {
            roles.push("writer");
        }
        if i == plan.admitter {
            roles.push("admitter");
        }
        if i == plan.rebuilt {
            roles.push("rebuilt");
        }
        if i == plan.newcomer {
            roles.push("newcomer");
        }
        roles.join(", ")
    };
    for (i, s) in ev.end.iter().enumerate() {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            s.name,
            lab.macs.get(i).map_or("?", |m| m.island.word()),
            role(i),
            lab.macs
                .get(i)
                .and_then(|m| m.label.clone())
                .unwrap_or_default(),
            if s.up { "up" } else { "down" },
            s.records,
            shorts(&s.heads),
            if s.opens_newest { "yes" } else { "no" },
            s.vault_state
        );
    }
    let revoked_lines: usize = lab
        .macs
        .iter()
        .filter_map(|m| crate::util::read_regular_text(&m.root.join("member.log")).ok())
        .map(|text| {
            text.matches("this machine is no longer in the vault")
                .count()
        })
        .sum();
    // Every Mac here dials from 127.0.0.1, where a vault server takes sixteen connections at
    // once (`server.rs` `MAX_PEER_CONNECTIONS`); distinct LAN addresses never meet that.
    let crowded: usize = lab
        .macs
        .iter()
        .filter_map(|m| crate::util::read_regular_text(&m.root.join("member.log")).ok())
        .map(|text| {
            text.matches("too many connections from your address")
                .count()
        })
        .sum();
    let approvals = crate::util::read_regular_text(&lab.macs[0].root.join("member.log"))
        .map(|text| text.lines().filter(|l| l.contains(" approved ")).count())
        .unwrap_or(0);
    let _ = writeln!(
        out,
        "\nm00 founded the vault by `aterm secrets create`, which makes it the vault's admin until \
         D1's `fleet init`: its subscriber's envelope approver approved {approvals} join requests \
         (`approved` lines in its log). Subscribers logged \"this machine is no longer in the \
         vault\" {revoked_lines} times, and were refused {crowded} times for too many \
         connections from one address (every Mac here dials from 127.0.0.1)."
    );
    let _ = writeln!(out, "\n## Checks\n");
    for c in checks {
        let _ = writeln!(
            out,
            "- [{}] {}: {}",
            if c.pass { "PASS" } else { "FAIL" },
            c.name,
            c.detail
        );
    }
    if let Some(control) = ev.control {
        let _ = writeln!(
            out,
            "\nNegative control `{}` ({}): {}",
            control.name,
            control.switch,
            if verdict(checks, Some(control)) {
                format!(
                    "{} failed on its evidence, with every act run, the run isolated and the \
                     split held: the control bit",
                    control.breaks
                )
            } else {
                format!(
                    "the control did not bite: {} must fail on its evidence while {ISOLATED}, \
                     {STEPS} and {SPLIT_HELD} hold",
                    control.breaks
                )
            }
        );
    }
    let _ = writeln!(out, "\n## Timeline\n\n```text");
    for (at, line) in &lab.timeline {
        let _ = writeln!(out, "  {at:7.1}s  {line}");
    }
    let _ = writeln!(out, "```");
    out
}

/// The part of a record's name after `mesh-churn` for `args`'s arm, and the flags that chose it.
#[must_use]
pub fn record_arm(args: &Args) -> (String, String) {
    let mut arm = String::new();
    let mut flags = String::new();
    if args.n != DEFAULT_N {
        let _ = write!(arm, "-n{}", args.n);
        let _ = write!(flags, " --n {}", args.n);
    }
    if let Some(control) = args.control {
        let _ = write!(arm, "-nc-{}", control.name);
        let _ = write!(flags, " --negative-control {}", control.name);
    }
    if let Some(label) = &args.label {
        let _ = write!(arm, "-{label}");
        let _ = write!(flags, " --label {label}");
    }
    let _ = write!(flags, " --seed {} --minutes {}", args.seed, args.minutes);
    (arm, flags)
}

/// Write the run's record under `docs/measured/` when `to_docs`, else under the lab's results.
/// A checkout's commit and whether its tracked files differ from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tree {
    /// `HEAD`, abbreviated to ten digits, or empty where git could not say.
    pub commit: String,
    /// Tracked files differ from `HEAD`.
    pub dirty: bool,
}

impl Tree {
    fn of(repo: &Path) -> Tree {
        let git = |args: &[&str]| {
            aterm_uds::spawnfd::command("/usr/bin/git")
                .arg("-C")
                .arg(repo)
                .args(args)
                .stdin(Stdio::null())
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
                .unwrap_or_default()
        };
        Tree {
            commit: git(&["rev-parse", "--short=10", "HEAD"]),
            dirty: !git(&["status", "--porcelain", "--untracked-files=no"]).is_empty(),
        }
    }
}

/// What a record adds where the checkout moved during the run: the record names the tree the
/// run's image was built from ([`Lab::tree`]), and this the tree it was written beside.
#[must_use]
pub fn moved_note(began: &Tree, now: &Tree) -> String {
    if began == now {
        return String::new();
    }
    format!(
        " (the checkout was at `{}`{} when this was written)",
        now.commit,
        if now.dirty {
            " with uncommitted changes"
        } else {
            ""
        }
    )
}

fn write_record(lab: &Lab, text: &str, checks: &[Check], to_docs: bool) -> Result<String, String> {
    let p = crate::paths::paths();
    let stamp = crate::util::utc_stamp(crate::util::now_secs());
    let date = stamp.get(..10).unwrap_or("undated").to_owned();
    let (arm, flags) = record_arm(&lab.args);
    let path = if to_docs {
        p.repo
            .join("docs/measured")
            .join(format!("mesh-churn{arm}-{date}.md"))
    } else {
        p.need_state()?.join("results").join(format!(
            "mesh-churn{arm}-{}.md",
            stamp.get(..19).unwrap_or(&stamp).replace(':', "")
        ))
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let commit = &lab.tree.commit;
    let dirty = if lab.tree.dirty {
        " with uncommitted changes"
    } else {
        ""
    };
    let moved = moved_note(&lab.tree, &Tree::of(&p.repo));
    let mut body = String::new();
    let _ = writeln!(body, "<!-- SPDX-License-Identifier: Apache-2.0 -->");
    let _ = writeln!(body, "<!-- Copyright 2026 Andrew Yates -->");
    let _ = writeln!(body);
    let _ = writeln!(
        body,
        "# mesh churn{arm}: membership churn on one Mac — {date}"
    );
    let _ = writeln!(body);
    let _ = writeln!(
        body,
        "Written by `targo --unverified lab mesh churn{flags}` (`crates/aterm-lab/src/mesh_churn.rs`) \
         at commit `{commit}`{dirty}, its checkout when the run began{moved}, {} s after it \
         began, at {stamp}. {} of {} checks held.",
        lab.t0.elapsed().as_secs(),
        checks.iter().filter(|c| c.pass).count(),
        checks.len()
    );
    let _ = writeln!(body);
    let _ = writeln!(
        body,
        "{} Macs, each a private world on this Mac's loopback with its own vault server and \
         subscriber, on the run's own genesis; islands are the `ATERM_DEBUG_PARTITION` seam. The \
         aterm under test, `{}`, runs every vault server, the founding `secrets create` and the \
         CLI removals. Every subscriber (each vault pass, gap heal, re-key and coalesce), desk, \
         join and write runs the product's code as linked into this lab's own image, at the \
         commit above. Admission is a person's Allow and the phrase on a member's desk (the LAN \
         rule needs B5 and B6). Every period and bound is production's, and every time below is \
         a measurement, never a budget: a wait fails only once nothing moved for a minute past \
         the product's own timers, or, its Macs still moving, ten minutes past that (a \
         livelock).",
        lab.macs.len(),
        lab.under_test
            .strip_prefix(&p.repo)
            .unwrap_or(&lab.under_test)
            .display(),
    );
    let _ = writeln!(body);
    body.push_str(text);
    crate::util::atomic_write(&path, body.as_bytes()).map_err(|e| e.to_string())?;
    Ok(path.display().to_string())
}

/// `lab mesh <verb> …`.
///
/// # Errors
/// An unknown verb or bad arguments (exit 2), or a lab that could not run (exit 3).
pub fn cmd(args: &[String]) -> Res {
    match args.first().map(String::as_str) {
        Some("churn") => cmd_churn(&args[1..], false),
        Some(DRIVER) => cmd_churn(&args[1..], true),
        _ => Err(Fail::usage(format!("usage: {USAGE}"))),
    }
}

/// `mesh churn`: as the driver ([`DRIVER`]), the run; otherwise its launch ([`launch`]).
fn cmd_churn(words: &[String], driver: bool) -> Res {
    stop::install();
    let seed = u64::try_from(crate::util::now_ms()).unwrap_or(0);
    let args = parse(words, seed)?;
    if !cfg!(debug_assertions) {
        return Err(Fail::exit(
            3,
            "mesh churn needs a debug lab image: its acts sign through the product's debug seams",
        ));
    }
    let aterm = crate::instance::resolve_aterm(args.aterm.clone()).map_err(|e| Fail::exit(3, e))?;
    if !driver {
        return launch(words, args.seed, &aterm);
    }
    let image = own_image().map_err(|why| Fail::exit(3, format!("mesh churn: {why}")))?;
    println!(
        "aterm-lab mesh churn: {} Macs on this Mac's loopback, seed {}, sleepers stopped {} min{}",
        args.n,
        args.seed,
        args.minutes,
        args.control
            .map(|c| format!(", negative control {}", c.name))
            .unwrap_or_default()
    );
    // A run that ended early could not run, unless an interrupt ended it: then its status is the
    // interrupt's.
    let early = |f: Fail| match stop::check() {
        Err(stopped) if f.stopped.is_none() => Fail {
            msg: f.msg,
            ..Fail::from(stopped)
        },
        _ if f.stopped.is_some() => f,
        _ => Fail { code: 3, ..f },
    };
    let mut lab = Lab::up(&aterm, &image, &args).map_err(early)?;
    if args.keep {
        println!("worlds (kept): {}", lab.world.base.display());
    }
    let ev = run(&mut lab).map_err(early)?;
    let checks = judge(&ev);
    let text = report(&lab, &ev, &checks);
    println!("\n{text}");
    let measured = checks.iter().any(|c| c.name == SPLIT_HELD && c.pass);
    match write_record(&lab, &text, &checks, args.record && measured) {
        Ok(path) => println!("written: {path}"),
        Err(e) => println!("could not write the record: {e}"),
    }
    if args.record && !measured {
        println!(
            "not recorded under docs/measured: the split did not hold, so the run measured no build"
        );
    }
    let failed = checks.iter().filter(|c| !c.pass).count();
    if verdict(&checks, args.control) {
        println!(
            "mesh churn: {}",
            match args.control {
                Some(c) => format!("the control {} bit ({} failed)", c.name, c.breaks),
                None => "every check held".to_owned(),
            }
        );
        Ok(0)
    } else {
        println!("mesh churn: {failed}/{} checks failed", checks.len());
        Ok(1)
    }
}

// ---------------------------------------------------------------------------------------------
// A Mac's processes: its subscriber, its acts, and its converge stand-in.
// ---------------------------------------------------------------------------------------------

/// `aterm-lab mesh-churn-member --home <home>`: a Mac's subscriber, the loop of `aterm secrets
/// watch` over the vault under `<home>`.
#[must_use]
pub fn member_main(args: &[String]) -> i32 {
    let home = match args {
        [flag, home] if flag == "--home" => PathBuf::from(home),
        _ => {
            eprintln!("usage: aterm-lab {MEMBER_VERB} --home <home>");
            return 2;
        }
    };
    let paths = Paths::for_home(&home);
    let ended = client::watch(&paths, &|line: &str| {
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

/// The converge runner a Mac's subscriber starts (`<this lab> init --converge --trigger
/// <word>`): notes the start under `$HOME` and runs no pass. `None` outside a Mac's subscriber.
#[must_use]
pub fn converge_standin(args: &[String]) -> Option<i32> {
    if std::env::var_os(MEMBER_SEAM).is_none_or(|v| v != "1") {
        return None;
    }
    if args.first().map(String::as_str) != Some("--converge") {
        return None;
    }
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let line = format!("{} {}\n", crate::util::now_ms(), args.join(" "));
    Some(
        match crate::util::append_regular_file(&home.join(RUNNERS_FILE)) {
            Ok(mut f) => {
                use std::io::Write as _;
                i32::from(f.write_all(line.as_bytes()).is_err())
            }
            Err(_) => 1,
        },
    )
}

/// `aterm-lab mesh-churn-act --home <home> <act> <words>`: one act on the vault under `<home>`,
/// in a process of that Mac's world, so its island's cut holds every thread of it. What it says
/// goes to stdout; a failure's why to stderr with exit 1. `aterm-lab mesh-churn-act --ran` prints
/// [`RAN`], the proof that a copy of this image runs ([`launch`]).
#[must_use]
pub fn act_main(args: &[String]) -> i32 {
    if let [flag] = args
        && flag == "--ran"
    {
        println!("{RAN}");
        return 0;
    }
    let (home, act, words) = match args {
        [flag, home, act, words @ ..] if flag == "--home" => match Act::of(act) {
            Some(act) => (PathBuf::from(home), act, words),
            None => {
                eprintln!("{ACT_VERB}: no act {act:?}");
                return 2;
            }
        },
        _ => {
            eprintln!("usage: aterm-lab {ACT_VERB} --home <home> <act> [<word>…]");
            return 2;
        }
    };
    let paths = Paths::for_home(&home);
    match acts::run(&paths, act, words) {
        Ok(said) => {
            println!("{said}");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// The acts themselves, each on one Mac's vault (the desk's on the joiner's too).
mod acts {
    use super::{Act, HANG};
    use aterm_secrets::client::{self, Paths};
    use aterm_secrets::commons::bootstrap::Device;
    use aterm_secrets::members::{desk, join, joiner};
    use aterm_types::MutexExt as _;
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    pub(super) fn run(paths: &Paths, act: Act, words: &[String]) -> Result<String, String> {
        match (act, words) {
            (Act::Genesis, []) => {
                let out = paths.dir.join("mesh-churn.genesis");
                let _ = std::fs::remove_file(&out);
                client::seed_vault_genesis(paths, &out).map_err(|e| e.to_string())?;
                Ok("genesis seeded".to_owned())
            }
            (Act::Put, [entry, value]) => put(paths, entry, value.as_bytes()),
            (Act::RemoveUnposted, [who]) => remove_unposted(paths, who),
            (Act::Repoint, [spec, host]) => {
                client::configure_log_member(paths, spec, host).map_err(|e| e.to_string())?;
                Ok(format!(
                    "source {}",
                    spec.split('#').next().unwrap_or_default()
                ))
            }
            (Act::Desk, [home, name]) => admit(paths, Path::new(home), name, Way::Allow),
            (Act::Desk, [home, name, again]) if again == "again" => {
                admit(paths, Path::new(home), name, Way::AllowAgain)
            }
            (Act::Prove, [home, name]) => admit(paths, Path::new(home), name, Way::Prove),
            _ => Err(format!("{}: wrong words {words:?}", act.word())),
        }
    }

    #[cfg(debug_assertions)]
    fn put(paths: &Paths, entry: &str, value: &[u8]) -> Result<String, String> {
        client::test_put_everyone(paths, entry, value)
            .map(|epoch| format!("put under epoch {}", super::short(&epoch)))
            .map_err(|e| e.to_string())
    }

    #[cfg(not(debug_assertions))]
    fn put(_: &Paths, _: &str, _: &[u8]) -> Result<String, String> {
        Err("a release lab has no member write; run a debug lab".to_owned())
    }

    #[cfg(debug_assertions)]
    fn remove_unposted(paths: &Paths, who: &str) -> Result<String, String> {
        client::test_remove_unposted(paths, who)
            .map(|n| format!("{n} records signed, none posted"))
            .map_err(|e| e.to_string())
    }

    #[cfg(not(debug_assertions))]
    fn remove_unposted(_: &Paths, _: &str) -> Result<String, String> {
        Err("a release lab cannot keep a removal unposted; run a debug lab".to_owned())
    }

    /// The joiner's node identity under its world's state directory (`<root>/state`), made if
    /// it has none: a rebuild keeps it.
    fn node(joiner_home: &Path) -> Result<aterm_net::fleet::NodeIdentity, String> {
        let state: PathBuf = joiner_home
            .parent()
            .map(|root| root.join("state"))
            .ok_or("the joiner's home has no world")?;
        aterm_net::fleet_member::node_identity(&state).map_err(|e| e.to_string())
    }

    /// How a person, or the Mac's own key, lets it in.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Way {
        /// A person's Allow and the phrase.
        Allow,
        /// A person's "Allow again" for a Mac a plain removal took out, and the phrase.
        AllowAgain,
        /// Its node key's proof, with nobody's Allow.
        Prove,
    }

    /// The address the joiner dials this Mac's desk at: a name, since every connection its dial
    /// makes goes to the desk this process serves ([`session`]).
    const AT: &str = "desk";

    /// One session of the joiner with this Mac's desk, both in this process: `joining` runs on a
    /// thread of its own with a dial whose every connection `serve` takes here, one at a time,
    /// until `joining` returns and its dial with it. The desk serves on this thread because its
    /// proof's callbacks are its own. Each end of a connection reads under the hang detector, and
    /// the act's own deadline bounds the whole. What `joining` returned, and every refusal the
    /// desk ended a connection with (a busy one among them), in order.
    fn session<R: Send>(
        serve: &mut dyn FnMut(&mut UnixStream) -> Result<(), String>,
        joining: &(dyn Fn(&join::Dial<'_>) -> R + Sync),
    ) -> Result<(R, Vec<String>), String> {
        let (to_desk, at_desk) = std::sync::mpsc::channel::<UnixStream>();
        std::thread::scope(|scope| {
            let joined = scope.spawn(move || {
                let dial = move |_: &str| -> aterm_secrets::Result<Box<dyn join::Stream>> {
                    let (ours, theirs) = UnixStream::pair()
                        .map_err(|e| aterm_secrets::Error::new(format!("a socket pair: {e}")))?;
                    for end in [&ours, &theirs] {
                        end.set_read_timeout(Some(HANG))
                            .map_err(|e| aterm_secrets::Error::new(e.to_string()))?;
                    }
                    to_desk
                        .send(ours)
                        .map_err(|_| aterm_secrets::Error::new("this Mac's desk stopped"))?;
                    Ok(Box::new(theirs))
                };
                joining(&dial)
            });
            let mut refused = Vec::new();
            for mut stream in at_desk {
                if let Err(why) = serve(&mut stream) {
                    refused.push(why);
                }
            }
            joined
                .join()
                .map(|r| (r, refused))
                .map_err(|_| "the joiner panicked".to_owned())
        })
    }

    /// This Mac (`by`) lets the Mac whose home is `joiner_home` in: by its node key's proof alone
    /// (`prove`), else by a person's Allow and the phrase. The joiner's side is the product's own
    /// (`members::join`'s `prove_first`, `ask_all` and `run_phrase`, which asks a busy member
    /// again while the phrase lives, writes the bundle it is handed and follows on); its desk is
    /// this Mac's, served here ([`session`]). This Mac then posts its records to the servers it
    /// reaches, as its window does.
    fn admit(by: &Paths, joiner_home: &Path, name: &str, way: Way) -> Result<String, String> {
        let joiner = Paths::for_home(joiner_home);
        client::machine_identity(&joiner).map_err(|e| e.to_string())?;
        let device = Device::open(&joiner)
            .map_err(|e| format!("the joiner's device: {e}"))?
            .ok_or("the joiner holds no identity")?;
        let node = node(joiner_home)?;
        let store = client::load_records(&joiner).map_err(|e| e.to_string())?;
        let joining = joiner::Joining {
            device: &device,
            records_bound: client::records_bound(&joiner, &store).map_err(|e| e.to_string())?,
            node: *node.key().bytes(),
            name: name.to_owned(),
            model: "Mac16,10".to_owned(),
        };
        let (local, came) = desk_of(by)?;
        let desk = std::sync::Mutex::new(desk::Desk::new(local));
        let hand = |handing: &desk::Handing<'_>| client::hand_over(by, handing);
        let book = |proving: &desk::Proving<'_>| {
            aterm_agent::vault_join::book_word_read(
                None,
                &proving.node,
                Some([127, 0, 0, 1].into()),
                aterm_time::unix_millis().unwrap_or(0),
            )
        };
        let decide = |proving: &desk::Proving<'_>| client::prove_at(by, proving, book(proving));
        let refuses = |proving: &desk::Proving<'_>| {
            client::refuses_before_possession(by, proving, book(proving))
        };
        let proof = desk::Proof {
            decide: &decide,
            refuses: &refuses,
            fleet: None,
        };
        let mut serve = |stream: &mut UnixStream| {
            desk::serve_with(&desk, stream, [127, 0, 0, 1].into(), &hand, Some(proof))
                .map(drop)
                .map_err(|e| e.to_string())
        };
        let ats = [AT.to_owned()];
        // What the desk refused, where it says more than the joiner's own answer.
        let with_desk = |why: String, refused: &[String]| {
            let more: Vec<&str> = refused
                .iter()
                .map(String::as_str)
                .filter(|r| !why.contains(r))
                .collect();
            if more.is_empty() {
                why
            } else {
                format!("{why} (this Mac's desk: {})", more.join("; "))
            }
        };
        let joined = if way == Way::Prove {
            let sign = |message: &[u8]| node.sign_record(desk::PROVE_RECORD, message);
            let (proved, refused) = session(&mut serve, &|dial| {
                join::prove_first(&joiner, dial, &joining, &sign, &ats).map_err(|missed| {
                    missed
                        .refused
                        .into_iter()
                        .map(|(_, why)| why)
                        .collect::<Vec<_>>()
                        .join("; ")
                })
            })?;
            proved.map_err(|why| with_desk(why, &refused))?
        } else {
            // The request reaches the desk, where a person sees it and presses Allow (or "Allow
            // again"); the joiner then asks again, is told it was allowed, and runs the phrase.
            let ((heard, missed), _) =
                session(&mut serve, &|dial| join::ask_all(dial, &joining, &ats))?;
            if heard.is_empty() {
                return Err(format!(
                    "this Mac's desk did not answer the ask: {missed:?}"
                ));
            }
            let phrase = {
                let mut desk = desk.lock_or_recover();
                let now = aterm_time::Instant::now();
                let row = desk
                    .rows(now)
                    .first()
                    .map(|row| row.id)
                    .ok_or("this Mac's desk shows no request")?;
                let phrase = if way == Way::AllowAgain {
                    desk.allow_again(row, now)
                } else {
                    desk.allow(row, now)
                }
                .map_err(|e| e.to_string())?;
                desk.words(phrase, now)
                    .ok_or("the phrase ran out before it was read")?
                    .join(" ")
            };
            let phrase =
                aterm_secrets::phrase::Phrase::parse(&phrase).map_err(|e| e.to_string())?;
            let (ran, refused) = session(&mut serve, &|dial| {
                let (heard, missed) = join::ask_all(dial, &joining, &ats);
                if heard.is_empty() {
                    return Err(format!(
                        "this Mac's desk did not answer the ask: {missed:?}"
                    ));
                }
                join::run_phrase(&joiner, dial, &joining, &heard, &phrase, None, &mut |_| {})
                    .map_err(|e| e.to_string())
            })?;
            ran.map_err(|why| with_desk(why, &refused))?
        };
        let posted = client::post_records_to_servers(by).unwrap_or(0);
        Ok(format!(
            "{} in by {} ({posted} servers took this Mac's records{})",
            joined.name,
            match way {
                Way::Allow => "Allow and the phrase",
                Way::AllowAgain => "Allow again and the phrase",
                Way::Prove => "its node key's proof",
            },
            came.map(|(ms, why)| format!(
                "; its desk came {:.1}s after the first read, which said {why}",
                ms as f64 / 1000.0
            ))
            .unwrap_or_default()
        ))
    }

    /// This Mac's desk, one that can allow, as its window follows it: the window keeps the desk
    /// it has while a read finds none for a moment (a vault pass that held the state past the
    /// read's deadline, an apply still pending), and a seated member's desk must come, so this
    /// waits for it under the hang detector. The desk, and where the first read did not give it,
    /// how long it took (milliseconds) and what that read said.
    fn desk_of(by: &Paths) -> Result<(desk::Local, Option<(u64, String)>), String> {
        let start = Instant::now();
        let mut first: Option<String> = None;
        loop {
            let why = match desk::Local::of(by) {
                Ok(Some(local)) if local.can_allow() => {
                    let ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
                    return Ok((local, first.map(|why| (ms, why))));
                }
                Ok(Some(_)) => {
                    "neither the membership log seats this Mac nor a Commons roster binds it"
                        .to_owned()
                }
                Ok(None) => "this Mac is no member".to_owned(),
                Err(e) => format!("this Mac's membership: {e}"),
            };
            if start.elapsed() >= HANG {
                return Err(format!("{why} (no desk that can allow came in {HANG:?})"));
            }
            first.get_or_insert(why);
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

#[cfg(test)]
#[path = "mesh_churn_tests.rs"]
mod tests;
