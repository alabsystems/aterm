// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Broker ack durability tiers, escalation rules, and legacy relaxed-mark migration.

use std::io;
use std::path::{Path, PathBuf};

/// The flag a broker argv carries for a named tier.
pub const FLAG: &str = "--durability";

/// Dev-only volume seam environment variable.
pub const VOLUME_SEAM: &str = "ATERM_LINK_BROKER_VOLUME";

/// The bytes of decoded records aterm's broker keeps in memory (astream's
/// `OpenOptions::resident_budget`, set by every [`Durability::options`]): the newest retained
/// records up to it, the newest one always. It reads every older retained record back from the
/// log file when a reader asks for it, through the same decode and, on a sealed log, the same
/// bound open the open's scan applies. So the decoded records no longer grow with the history
/// its retention rules keep (up to 7 days and 352 MiB, `broker_rules`), but the broker's memory
/// still does: every retained record, resident or cold, keeps its slot in astream's residency
/// tier and its key in the dedup index, 28.09 B a record of one consecutive producer (24.00 of
/// it the slot; astream's `broker_index_bench`, `DESIGN-aterm-fabric` §14 question 8), about
/// 28 MB a million records. Taking a cold record below one slot is that question's part (c),
/// which is not built.
pub const RESIDENT_BUDGET: usize = 16 * 1024 * 1024;

/// Dev-only seam replacing [`RESIDENT_BUDGET`]: a byte count, or `none` for no budget (every
/// retained record resident). The fabric ledger's D10d set ran its no-budget arm under `none`.
/// No test sets it: a test that needs records cold holds its own broker to a budget (astream's
/// `OpenOptions::resident_budget` or `Broker::set_resident_budget`), as the sealed cold read
/// (`at_rest_tests`), fabric world's cold refill (`bridge_recovery`) and the broker
/// differential's cold arm (`sim::conform_broker`, over [`Durability::options`]) do at 0, which
/// keeps the newest record alone so every read below the head is a cold one.
pub const RESIDENT_BUDGET_SEAM: &str = "ATERM_LINK_RESIDENT_BUDGET";

/// The residency budget [`Durability::options`] sets: [`RESIDENT_BUDGET`], or what
/// [`RESIDENT_BUDGET_SEAM`] names in a build with the dev seams ([`budget_of`]).
#[must_use]
pub fn resident_budget() -> Option<usize> {
    budget_of(aterm_types::dev_seam!(RESIDENT_BUDGET_SEAM).as_deref())
}

/// The residency budget [`RESIDENT_BUDGET_SEAM`]'s value `seam` names: `none` for no budget, a
/// byte count for that many bytes, and [`RESIDENT_BUDGET`] for no seam or any other value.
#[must_use]
pub fn budget_of(seam: Option<&std::ffi::OsStr>) -> Option<usize> {
    match seam.and_then(std::ffi::OsStr::to_str) {
        Some("none") => None,
        Some(bytes) => Some(bytes.parse().unwrap_or(RESIDENT_BUDGET)),
        None => Some(RESIDENT_BUDGET),
    }
}

/// Legacy relaxed-mode boot mark suffix (`<log>.aterm-relaxed`).
pub const MARK_SUFFIX: &str = ".aterm-relaxed";

/// The tier a broker acks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Durability {
    /// Ack after a full flush (`F_FULLFSYNC`).
    #[default]
    Strict,
    /// Ack after an ordering barrier, with full flushes at [`ORDERED_HORIZON`] and for [`escalation`] records.
    Ordered,
    /// Ack in the page cache, with full flushes for [`escalation`] records and on clean stop.
    Relaxed,
}

/// Horizon after which an ordered broker issues a full flush for uncovered acks.
pub const ORDERED_HORIZON: std::time::Duration = std::time::Duration::from_millis(100);

/// Escalation rules flushed in full before ack on weak tiers.
#[must_use]
pub fn escalation() -> astream_broker::Escalation {
    use astream_broker::Escalate::{Group, PrincipalPrefix, Subject};
    astream_broker::Escalation::new([
        Subject("/f/*/fleet/>".into()),
        // A Mac's halts with the mesh on, its own and each relayed copy (§11 C4).
        Subject("/f/*/halt/>".into()),
        Subject("/f/*/in/p/>".into()),
        PrincipalPrefix("h-".into()),
        Group("/f/*/cur/p/>".into()),
    ])
}

impl Durability {
    /// Parse a `--durability` value (`strict`, `ordered`, or `relaxed`).
    /// # Errors
    ///
    /// A word that is not `strict`, `ordered` or `relaxed`.
    pub fn parse(word: &str) -> Result<Self, String> {
        match word {
            "strict" => Ok(Durability::Strict),
            "ordered" => Ok(Durability::Ordered),
            "relaxed" => Ok(Durability::Relaxed),
            other => Err(format!(
                "{FLAG} {}: strict, ordered or relaxed",
                crate::render::safe(other, 32)
            )),
        }
    }

    /// Canonical flag value (`strict`, `ordered`, or `relaxed`).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Durability::Strict => "strict",
            Durability::Ordered => "ordered",
            Durability::Relaxed => "relaxed",
        }
    }

    /// Corresponding [`astream_broker::Durability`] tier.
    #[must_use]
    pub const fn tier(self) -> astream_broker::Durability {
        match self {
            Durability::Strict => astream_broker::Durability::Strict,
            Durability::Ordered => astream_broker::Durability::Ordered {
                horizon: Some(ORDERED_HORIZON),
            },
            Durability::Relaxed => astream_broker::Durability::Relaxed,
        }
    }

    /// Broker [`astream_broker::OpenOptions`] for this tier, attaching [`escalation`] on weak tiers
    /// and bounding the decoded records it keeps in memory to [`resident_budget`] on every tier.
    ///
    /// Every tier OFFERS GAP FENCES (`sticky_batch_refusal`, astream's `DESIGN-aterm-fabric` §14
    /// Q7), which its `Hello` says: on a connection that asks (`FenceGaps`), once a producer's
    /// write is refused nothing it sent after it on that connection lands until it lifts the
    /// fence, so a bridge keeps several of a sender's posts in flight only against a broker
    /// opened here, once it asked (`Bridge::pipelines_posts`). A connection that never asks, a
    /// bridge of an older build's among them, is served unfenced, so a refusal it does not
    /// retry next holds back nothing it writes after it.
    #[must_use]
    pub fn options(self) -> astream_broker::OpenOptions {
        let mut opts =
            astream_broker::OpenOptions::new(self.tier()).with_sticky_batch_refusal(true);
        if self != Durability::Strict {
            opts.escalation = escalation();
        }
        opts.resident_budget = resident_budget();
        opts
    }

    /// Human-readable ack durability promise for a guarded broker of this build serving `self`.
    #[must_use]
    pub fn promise(self) -> String {
        match self {
            Durability::Strict => STRICT_PROMISE.to_string(),
            weak => {
                let horizon_ms = (weak == Durability::Ordered)
                    .then(|| astream_broker::ServedInfo::horizon_ms_of(ORDERED_HORIZON));
                let (head, lost) = loss(weak, horizon_ms, "machine record");
                format!("{head}: {lost} — {SPARED} — and readers resume where the log continues")
            }
        }
    }

    /// Map an `astream` wire [`astream_broker::Tier`] to [`Durability`].
    #[must_use]
    pub const fn of_tier(requested: astream_broker::Tier) -> Option<Self> {
        match requested {
            astream_broker::Tier::Strict => Some(Durability::Strict),
            astream_broker::Tier::Ordered => Some(Durability::Ordered),
            astream_broker::Tier::Relaxed => Some(Durability::Relaxed),
            _ => None,
        }
    }

    /// Serving durability of `argv`, falling back to [`Durability::Strict`] if unrecognised.
    #[must_use]
    pub fn of_argv<S: AsRef<std::ffi::OsStr>>(argv: &[S]) -> Self {
        Self::try_of_argv(argv).unwrap_or_default()
    }

    /// Serving durability of a syntactically recognised broker `argv`.
    #[must_use]
    pub fn try_of_argv<S: AsRef<std::ffi::OsStr>>(argv: &[S]) -> Option<Self> {
        crate::broker_args::BrokerArgs::from_invocation(argv)?.serving_durability()
    }

    /// Explicit `--durability` tier from a recognised serving `argv` (`Some(None)` when omitted).
    #[must_use]
    pub fn named_of_argv<S: AsRef<std::ffi::OsStr>>(argv: &[S]) -> Option<Option<Self>> {
        crate::broker_args::BrokerArgs::from_invocation(argv)?.named_durability()
    }
}

/// Default durability for a bare broker argv: `Ordered` when `guarded`, `Strict` when unguarded.
#[must_use]
pub const fn default_for(guarded: bool) -> Durability {
    if guarded {
        Durability::Ordered
    } else {
        Durability::Strict
    }
}

/// Resolve an explicit `named` tier or fall back to [`default_for`].
#[must_use]
pub fn resolve(named: Option<Durability>, guarded: bool) -> Durability {
    named.unwrap_or_else(|| default_for(guarded))
}

/// Argv words for an explicit tier, or empty for the build default.
#[must_use]
pub fn argv_words(named: Option<Durability>) -> Vec<String> {
    match named {
        Some(tier) => vec![FLAG.to_string(), tier.name().to_string()],
        None => Vec::new(),
    }
}

/// Name of an `astream` [`astream_broker::Tier`], or `"other"` if unknown.
#[must_use]
pub fn tier_name(tier: astream_broker::Tier) -> &'static str {
    Durability::of_tier(tier).map_or("other", Durability::name)
}

fn served_reason(reason: astream_broker::ServedReason) -> String {
    use astream_broker::ServedReason;
    match reason {
        ServedReason::None => "it serves what it was asked for".to_string(),
        ServedReason::NotApple => "the ordering barrier exists only on Apple".to_string(),
        ServedReason::NotBootVolume => "its log is not on the boot volume".to_string(),
        ServedReason::BarrierRefused => {
            "the device refused the ordering barrier when the log opened".to_string()
        }
        ServedReason::LatchedAfterBarrierError => {
            "a barrier failed while it served, so the log latched to strict".to_string()
        }
        ServedReason::LatchedAfterFlushFailure => "a full flush failed while it served: the log \
                                                   refuses every write until it is reopened"
            .to_string(),
        other => format!("{other:?}"),
    }
}

/// Describe the durability promise of a running broker from its `asked` tier, `Hello` `served` info, and `guarded` state.
#[must_use]
pub fn describe_served(
    asked: Durability,
    served: Option<astream_broker::ServedInfo>,
    guarded: Option<bool>,
) -> String {
    use astream_broker::{ServedReason, Tier};
    let Some(info) = served else {
        return match asked {
            Durability::Strict => Durability::Strict.promise(),
            Durability::Ordered => "a broker too old to report what it serves, and too old to \
                                    serve ordered"
                .to_string(),
            Durability::Relaxed => "an ack is in memory: a power loss can lose any record acked \
                                    since the broker last started or stopped cleanly, and this \
                                    broker, older than lineage, then gives the log a new \
                                    identity, which every reader forgets its place in"
                .to_string(),
        };
    };
    let promise = match Durability::of_tier(info.served) {
        Some(Durability::Strict) => Durability::Strict.promise(),
        Some(weak) => weak_promise(weak, &info, guarded),
        None => format!(
            "a tier this build cannot name ({:?}), whose promise it cannot say",
            info.served
        ),
    };
    if Durability::of_tier(info.served) == Some(asked) {
        return promise;
    }
    let why = if info.reason != ServedReason::None {
        served_reason(info.reason)
    } else if asked == Durability::Ordered
        && info.served == Tier::Strict
        && info.requested == Tier::Strict
    {
        "its build serves ordered as strict".to_string()
    } else {
        format!("its build opened it {}", tier_name(info.requested))
    };
    format!("served {} ({why}) — {promise}", tier_name(info.served))
}

/// Describe what this build's guarded broker promises when `aterm fabric on` configures `tier`.
#[must_use]
pub fn describe_own(tier: Durability) -> String {
    match tier {
        Durability::Ordered => format!(
            "{}; astream serves it strict where the device cannot keep the order, and `aterm \
             fabric` says so and why",
            tier.promise()
        ),
        _ => tier.promise(),
    }
}

const STRICT_PROMISE: &str = "an ack is on the disk: a power loss loses nothing acked";

const SPARED: &str =
    "never a halt, a person's own words, mail to a person, or a person's read mark";

fn loss(tier: Durability, horizon_ms: Option<u32>, what: &str) -> (&'static str, String) {
    match tier {
        Durability::Ordered => (
            "an ack is in order on the disk",
            match horizon_ms {
                Some(0) => format!(
                    "a panic or forced power-off can lose the newest batch and a full flush \
                     of {what}s"
                ),
                Some(ms) if ms % 1000 == 0 => format!(
                    "a panic or forced power-off can lose the last {} s of {what}s, plus a batch \
                     and a full flush",
                    ms / 1000
                ),
                Some(ms) => format!(
                    "a panic or forced power-off can lose the last {ms} ms of {what}s, plus a \
                     batch and a full flush"
                ),
                None => format!(
                    "a panic or forced power-off can lose every {what} since the last full flush \
                     (an escalated record's, a flush asked for, or a clean stop's)"
                ),
            },
        ),
        _ => (
            "an ack is in memory",
            format!(
                "a power loss can lose any {what} since the broker last started or stopped \
                 cleanly"
            ),
        ),
    }
}

fn weak_promise(
    tier: Durability,
    info: &astream_broker::ServedInfo,
    guarded: Option<bool>,
) -> String {
    let (head, lost) = loss(tier, info.horizon_ms, "record");
    let spared = match info.rules {
        0 => " — no record is flushed in full before its ack —".to_string(),
        1 => " — except the records its 1 escalation rule names, flushed in full before their \
              ack —"
            .to_string(),
        n => format!(
            " — except the records its {n} escalation rules name, flushed in full before their \
             ack —"
        ),
    };
    let unguarded = if guarded == Some(false) && info.rules > 0 {
        "; on an unguarded broker no principal is bound, so principal-based rules do not match"
    } else {
        ""
    };
    format!("{head}: {lost}{spared} and readers resume where the log continues{unguarded}")
}

/// Path to the legacy relaxed mark file (`<log>.aterm-relaxed`).
#[must_use]
pub fn mark_path(log: &Path) -> PathBuf {
    let mut s = log.as_os_str().to_os_string();
    s.push(MARK_SUFFIX);
    PathBuf::from(s)
}

/// A boot id for a test that stands in a reboot, distinct for each `n` (never
/// the all-zero id, which astream reads as unknown): the 16 bytes of the UUID
/// `00000000-0000-0000-0000-0000000001nn`.
#[cfg(test)]
pub(crate) fn test_boot(n: u8) -> [u8; 16] {
    let mut boot = [0u8; 16];
    boot[14] = 0x01;
    boot[15] = n;
    boot
}

/// The bytes a build before lineage wrote at a relaxed log's mark for boot
/// `boot` ([`MARK_SUFFIX`]), in the kernel text macOS answers (the upper-case
/// UUID, which that build wrote as it read it): what the migration tests plant.
#[cfg(test)]
pub(crate) fn legacy_mark_text(boot: &[u8; 16]) -> String {
    legacy_mark_of(&boot_text(boot).to_uppercase())
}

/// The bytes a build before lineage wrote at a relaxed log's mark when the
/// kernel answered `kernel_text` for its boot.
#[cfg(test)]
pub(crate) fn legacy_mark_of(kernel_text: &str) -> String {
    format!(
        "boot={kernel_text}\n\
         # aterm link broker --durability relaxed opened the log beside this file in that boot:\n\
         # its acks may be only in the page cache. Its clean stop flushes the log and removes\n\
         # this file; the first open in another boot that finds it gives the log a new\n\
         # identity, so no client trusts an offset a power loss may have taken.\n"
    )
}

/// State of a log's legacy relaxed mark ([`MARK_SUFFIX`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mark {
    /// No mark file exists.
    Absent,
    /// Mark file records this 16-byte boot id.
    Boot([u8; 16]),
    /// Mark file exists but cannot be parsed as a valid non-zero boot id.
    Unreadable,
}

/// Read the legacy mark beside `log`, returning [`Mark::Unreadable`] if present but invalid.
#[must_use]
pub fn read_mark(log: &Path) -> Mark {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let path = mark_path(log);
    let f = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(&path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // A dangling link is something at the name, not an absent mark.
            return if std::fs::symlink_metadata(&path).is_ok() {
                Mark::Unreadable
            } else {
                Mark::Absent
            };
        }
        Err(_) => return Mark::Unreadable,
    };
    let Ok(meta) = f.metadata() else {
        return Mark::Unreadable;
    };
    if !meta.is_file() {
        return Mark::Unreadable;
    }
    let mut bytes =
        Vec::with_capacity(usize::try_from(meta.len().min(4096).saturating_add(1)).unwrap_or(256));
    if f.take(4096).read_to_end(&mut bytes).is_err() {
        return Mark::Unreadable;
    }
    let line = bytes.split(|b| *b == b'\n').next().unwrap_or_default();
    line.strip_prefix(b"boot=")
        .and_then(astream_sys::parse_boot_id)
        .map_or(Mark::Unreadable, Mark::Boot)
}

/// Why a legacy mark made this open's verdict unclean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Renewed {
    /// A relaxed broker opened the log in boot `was`; this is boot `now`.
    Rebooted {
        /// The boot the mark names.
        was: [u8; 16],
        /// This boot.
        now: [u8; 16],
    },
    /// The log is marked, and this host names no boot to compare it with.
    NoBoot,
    /// Something at the mark's name says no boot.
    Unreadable,
}

impl Renewed {
    /// Human-readable reason for stderr and [`astream_broker::Prepared::Unclean`].
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Renewed::Rebooted { was, now } => format!(
                "the machine restarted (boot {}, now {}) after a relaxed broker of an \
                 earlier build acked records the disk may not hold",
                boot_text(was),
                boot_text(now)
            ),
            Renewed::NoBoot => "a relaxed broker of an earlier build opened it, and this host \
                                names no boot to tell whether the machine restarted since"
                .to_string(),
            Renewed::Unreadable => format!(
                "its relaxed mark ({MARK_SUFFIX}) does not say which boot opened it, so a \
                 restart cannot be ruled out"
            ),
        }
    }
}

/// Return `Some(Renewed)` if `mark` indicates an unclean prior boot relative to `boot`.
#[must_use]
pub fn renewal(mark: &Mark, boot: Option<[u8; 16]>) -> Option<Renewed> {
    match (mark, boot) {
        (Mark::Absent, _) => None,
        (Mark::Boot(was), Some(now)) if *was == now => None,
        (Mark::Boot(was), Some(now)) => Some(Renewed::Rebooted { was: *was, now }),
        (Mark::Boot(_), None) => Some(Renewed::NoBoot),
        (Mark::Unreadable, _) => Some(Renewed::Unreadable),
    }
}

/// Current kernel boot session id from [`astream_sys::boot_session`].
#[must_use]
pub fn this_boot() -> Option<[u8; 16]> {
    astream_sys::boot_session()
}

/// Format a 16-byte boot id as a lowercase `8-4-4-4-12` UUID string
/// (`astream_sys::boot_id_text`).
#[must_use]
pub fn boot_text(boot: &[u8; 16]) -> String {
    astream_sys::boot_id_text(*boot, true)
}

/// Broker instance opened by [`open`] along with legacy-mark migration status.
pub struct Opened {
    /// Opened broker instance.
    pub broker: astream_broker::Broker,
    /// Legacy-mark unclean verdict folded into the open, if any.
    pub renewed: Option<Renewed>,
    /// Error message if removing a legacy mark failed after opening.
    pub mark_kept: Option<String>,
}

/// Open the broker over `log` at `durability` in `boot`, folding any legacy mark into `astream`'s open verdict.
/// # Errors
///
/// `AlreadyExists` when another broker holds the log (astream refuses before
/// the mark is read); `NotFound` for a log whose directory does not exist
/// (nothing is created); `InvalidInput` for an empty secret; and astream's own
/// open refusal.
pub fn open(
    log: &Path,
    secret: Option<Vec<u8>>,
    durability: Durability,
    boot: Option<[u8; 16]>,
) -> io::Result<Opened> {
    open_escalating(log, secret, durability, boot, Vec::new(), None)
}

/// [`open`] with `extra` escalation rules appended (used by dev fault-injection seams), and sealed
/// at rest under `at_rest` (`crate::at_rest::prepare` decides it): keyed and bound, so a log whose
/// `.seal` is gone refuses rather than reads as an older unbound one. No anti-rollback: astream
/// keeps that for Strict, and this broker defaults to Ordered.
/// # Errors
///
/// As [`open`], astream's refusal of a rule that does not parse, and its refusal of a key that is
/// not the log's.
pub fn open_escalating(
    log: &Path,
    secret: Option<Vec<u8>>,
    durability: Durability,
    boot: Option<[u8; 16]>,
    extra: Vec<astream_broker::Escalate>,
    at_rest: Option<[u8; 32]>,
) -> io::Result<Opened> {
    let mut opts = durability.options();
    opts.guard = secret;
    opts.require_bound_at_rest = at_rest.is_some();
    opts.at_rest_key = at_rest;
    if aterm_types::dev_seam!(VOLUME_SEAM).is_some_and(|v| v == "other") {
        opts.volume = astream_broker::Volume::Other;
    }
    if !extra.is_empty() {
        let rules = opts.escalation.rules().to_vec();
        opts.escalation = astream_broker::Escalation::new(rules.into_iter().chain(extra));
    }
    open_with(log, opts, boot)
}

/// Open the broker over `log` under `opts` in `boot`, folding any legacy mark into astream's open
/// verdict: [`open_escalating`] with options its caller built (the guard, the tier, the seal),
/// which the node service's host of `bus.log` (`aterm_mesh::logs::open_bus`) and a test's broker
/// share. `opts.boot` is set from `boot`.
/// # Errors
///
/// As [`open`], and astream's own refusal of `opts`.
pub fn open_with(
    log: &Path,
    mut opts: astream_broker::OpenOptions,
    boot: Option<[u8; 16]>,
) -> io::Result<Opened> {
    opts.boot = boot.map_or(astream_broker::Boot::Unknown, astream_broker::Boot::Known);
    let mut decided: Option<(Mark, Option<Renewed>)> = None;
    let prepare = || -> io::Result<astream_broker::Prepared> {
        let mark = read_mark(log);
        let renewed = renewal(&mark, boot);
        let verdict = renewed
            .as_ref()
            .map_or(astream_broker::Prepared::Nothing, |why| {
                astream_broker::Prepared::Unclean(why.describe())
            });
        decided = Some((mark, renewed));
        Ok(verdict)
    };
    let broker = astream_broker::Broker::open_opts(log, opts, prepare)?;
    let Some((mark, renewed)) = decided else {
        return Err(io::Error::other(
            "the broker log opened without its legacy mark being judged",
        ));
    };
    let mark_kept = (mark != Mark::Absent)
        .then(|| remove_durably(&mark_path(log)).err())
        .flatten()
        .map(|e| format!("{}: {e}", mark_path(log).display()));
    Ok(Opened {
        broker,
        renewed,
        mark_kept,
    })
}

fn remove_durably(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => crate::state::sync_dir(path.parent().unwrap_or(Path::new(""))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
#[path = "durability_tests.rs"]
mod tests;
