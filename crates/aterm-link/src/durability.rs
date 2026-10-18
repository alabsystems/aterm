// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BROKER'S DURABILITY: what an ack from the broker `aterm link broker`
//! runs promises (`--durability strict|ordered|relaxed`), and how every offset
//! aterm keeps outside the log survives the one hazard a weaker tier brings.
//!
//! ## The tiers
//!
//! * `strict`, the default — astream's [`astream_broker::Durability::Strict`]:
//!   an ack follows the batch's full flush (`F_FULLFSYNC` on macOS, a flush of
//!   the drive's own cache), so an acked record survives a power loss.
//! * `relaxed`, opt-in — [`astream_broker::Durability::Relaxed`]: an ack
//!   follows the batch's write into the page cache. It survives a crash or a
//!   restart of the broker (the kernel holds the bytes), NOT a power loss, a
//!   kernel panic or a hard reset, any of which can take every record acked
//!   since the broker last opened the log or stopped cleanly.
//! * `ordered` — ACCEPTED, SERVED STRICT by this build. The word is the tier a
//!   later build serves (astream's [`astream_broker::Durability::Ordered`]: an
//!   ack after an ordering barrier, a full flush at most a horizon later). This
//!   build takes it on the command line and in a job's argv, and opens the log
//!   Strict, so a job a later build wrote with `--durability ordered` runs here
//!   — never weaker than asked — instead of refusing to start after a revert.
//!   Status and doctor say "ordered, served strict", read from the broker
//!   itself ([`describe_served`]).
//!
//! What the flush costs, MEASURED with astream's own
//! `broker_durability_bench` (one producer, each publish acked before the
//! next, the broker alone over a Unix socket, release build; Apple M5 Max,
//! internal SSD, a machine shared with other builds; 2026-10-16, three runs
//! a tier): strict 172, 210 and 224 acks/s (n = 2000 each), a mean of 4.5 to
//! 5.8 ms an ack; relaxed 37 319, 69 953 and 77 921 acks/s (n = 20 000 each),
//! 13 to 27 µs. Means only — the bench reports no percentiles — and the
//! broker's own ack: a post through the bridge pays the bridge's hops too,
//! which nothing here measures, and a strict batch shares one flush among the
//! records pipelined into it.
//!
//! ## The hazard: a log SHORTER than the offsets that point into it
//!
//! Offsets name records in one log, and aterm keeps them outside it:
//!
//! * the bridge's state dir ([`crate::state`]): `seen/`, `topics/`,
//!   `unhandled/`, `acked`, `halt-acked`, `inbox-owed`, `deadlines`, `asked`,
//!   the `original-off` of `keys/` and `sent/`, an `inflight/` record's
//!   witnessed log, and `bus-log`'s high-water;
//! * the bridge's inbox-group commit owed in memory (`Bridge::inbox_pending`),
//!   which it keeps across a reconnect to the same log and hands to the next
//!   bridge as `inbox-owed`: copies, OUTSIDE the log, of a commit the node's
//!   inbox group has not made yet;
//! * the notifier's `cur/` and `journal` ([`crate::notify`]);
//! * the aterm endpoint's inbox ring (`off=`), told which log by the bridge.
//!
//! A relaxed power loss cuts the log back to what reached the disk, and the
//! next writes reuse the lost records' offsets. Kept under an unchanged
//! identity, every one of those offsets would then name a record its holder
//! never saw — the bridge's `seen/` would answer "nothing to refill" over new
//! mail, `topics/` would skip broadcasts, `acked` would vouch for a record that
//! is not this node's, a notifier's journal would swallow a new escalation —
//! and once another writer regrew the log past the old head, nothing could
//! tell. So the identity has to change, and it does:
//!
//! ## What astream does: the generation moves, with LINEAGE
//!
//! astream's broker log records how each run stood with the log in its
//! identity sidecar (`<log>.id`, FORMAT 2): the tier it served, the boot it
//! served in, whether the log was on the boot volume, and whether it stopped
//! cleanly. The first open after a run that served Relaxed and did NOT stop
//! cleanly moves the log's GENERATION by one when that open is in ANOTHER
//! boot — or when either boot cannot be named, or the run's log was off the
//! boot volume — and records where the two generations still agree — `(new
//! generation, common)`, `common` being the head the open recovered: every
//! offset below it names the same record under both
//! ([`astream_broker::LogIdentity::continues`]). A process crash (`kill -9`)
//! reopened in the same boot, on the boot volume, moves nothing: the kernel
//! still held every acked byte, and the open's flush made them durable. Off
//! the boot volume (a separate `/home` partition, an external disk) astream
//! cannot lean on that — a disk's own cache can go with it, a forced eject
//! included — so there every unclean relaxed stop moves the generation, a
//! `kill -9` too.
//!
//! Every reader CLAMPS instead of forgetting: the bridge resumes its cursors
//! at the lower of where they were and `common`, and drops only what names a
//! record at or above it ([`crate::state::LogWitness::continues`],
//! [`crate::state::StateDir::forget_log_offsets`]); the notifier carries its
//! cursors and journal over the same way (`Store::for_log`). So a broadcast or
//! an escalation posted after the restart, at an offset the lost records once
//! had, is delivered and paged — nothing below the fork is re-read, nothing
//! after it skipped. Only `halt-acked` is always cleared, clamped or not: it
//! stores the HALT ROW's offset, below the fork while the ack it stands for
//! may have been lost above it, so a bridge re-acks (one redundant row) rather
//! than skip. A reader with no lineage to go by — a log found shorter, re-based
//! or foreign (a restored copy, another file), a generation older than the
//! newest [`astream_broker::Lineage::MAX`] steps, or an older aterm — forgets
//! its offsets as after a repair, as every reader did before lineage.
//!
//! ## What the broker verb does
//!
//! A RELAXED BROKER STOPS CLEANLY. Stopped by SIGTERM, SIGINT or SIGHUP —
//! launchd at logout or shutdown, `systemctl stop`, `aterm fabric off`, a
//! terminal's ^C — or leaving for a replaced build, it drops its listeners:
//! astream joins the writer, flushes the log in full and writes `Closed` into
//! the sidecar before it lets go of the log's lock, so a reboot after it moves
//! nothing. Only a stop that never got that far — a power loss, a kernel
//! panic, a hard reset, a SIGKILL (a supervisor's stop timeout included) —
//! leaves the run `Open`, and the next boot's open moves the generation (the
//! next open at all, off the boot volume).
//!
//! THE LEGACY MARK. Builds before lineage marked a relaxed log with the boot
//! that opened it (`<log>.aterm-relaxed`, [`MARK_SUFFIX`]) and renewed the
//! log's identity by hand when a later boot found the mark. This build writes
//! no mark and never touches `<log>.id`. It still READS one, inside astream's
//! prepared open (under the log's own lock, before the sidecar is read): a mark
//! from another boot, or one that cannot be read, is the embedder's unclean
//! verdict ([`astream_broker::Prepared::Unclean`]), which astream ORs into its
//! own — so the generation moves once however many verdicts agree. A mark from
//! this boot says nothing to fear (the page cache outlived the broker). After
//! the open, at any tier, the mark is removed. No open moves the generation
//! twice for one stop: a new build never writes a mark, so a mark predates the
//! first new-build open; astream's own verdict needs an `Open` run a new build
//! recorded, which that first open has not; that open removes the mark; and
//! both verdicts feed one decision. A crash before the removal makes the next
//! open in another boot move the generation again — one change per open, in
//! the safe direction.
//!
//! ONE BOOT, ONE READING. "This boot" is [`this_boot`], astream's own reading
//! of the kernel's boot id ([`astream_sys::boot_session`]): the 16 bytes the
//! broker hands astream ([`astream_broker::OpenOptions::boot`]), the bytes the
//! legacy mark is compared with, and the bytes the bridge's incarnation lease
//! trusts (`state.rs`, "The incarnation lease"). A mark's text is parsed by the
//! same parser ([`astream_sys::parse_boot_id`]), so the kernel's text an older
//! build wrote there — Apple's upper-case UUID, Linux's lower-case one — reads
//! as the boot astream names. That parser refuses anything but 32 hex digits
//! and refuses the all-zero id (astream's sidecar reserves it for "unknown"):
//! such a mark is [`Mark::Unreadable`], and an unreadable kernel answer is no
//! boot at all, both of which move the generation — never two damaged answers
//! read as "no reboot came between them".
//!
//! NOT COVERED: a log copied elsewhere together with its sidecar (two logs
//! under one identity, which nothing can tell apart), and an older aterm build
//! opening the log, which reads FORMAT 2 as an unreadable sidecar and takes a
//! fresh identity (one ordinary log change). A host whose kernel names no boot,
//! and a log off the boot volume, run relaxed too: astream then moves the
//! generation after every unclean stop, a process crash included — the price
//! of not knowing.
//!
//! WHAT A MOVE STILL COSTS A SESSION. The clamp is the bridge's and the
//! notifier's; the aterm endpoint (each session's inbox, outbox and owed
//! receipts) is told the new identity and forgets every bus offset it holds,
//! as for any change of log. So a reply a session had queued while the broker
//! was down, and a verdict (`inbox ack`) on a row it was shown before the move,
//! are retired `log-replaced` — and the asker's bridge, which keeps its
//! deadline below the fork, says `expired` for that ask (no reply reached it)
//! where before lineage it said nothing. A reply posted after the move, or one
//! the bridge already held in flight, is published and settles the ask.

use std::io;
use std::path::{Path, PathBuf};

/// The flag a broker argv carries for a named tier.
pub const FLAG: &str = "--durability";

/// What a build before lineage marked a log with while a Relaxed run's acks
/// might be only in the page cache: `<log>` + this. Read, never written.
pub const MARK_SUFFIX: &str = ".aterm-relaxed";

/// The tier a broker acks at ([module docs](self)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Durability {
    /// An ack is flushed in full: it survives a power loss. The default.
    #[default]
    Strict,
    /// An ack is ordered on the device, flushed in full at most a horizon
    /// later — the tier a later build serves. THIS build accepts the word and
    /// serves it strict ([`Durability::tier`]).
    Ordered,
    /// An ack is in the page cache: it survives a broker crash, not a power
    /// loss, and the log's generation moves (with lineage) after the machine
    /// restarts without the broker's clean stop.
    Relaxed,
}

impl Durability {
    /// Parse the `--durability` word.
    ///
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

    /// The word, as `--durability` takes it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Durability::Strict => "strict",
            Durability::Ordered => "ordered",
            Durability::Relaxed => "relaxed",
        }
    }

    /// astream's tier, AS THIS BUILD OPENS IT: `ordered` is served strict
    /// ([module docs](self)), never weaker than asked.
    #[must_use]
    pub fn tier(self) -> astream_broker::Durability {
        match self {
            Durability::Strict | Durability::Ordered => astream_broker::Durability::Strict,
            Durability::Relaxed => astream_broker::Durability::Relaxed,
        }
    }

    /// Whether this build serves the tier as it is named: every tier but
    /// `ordered`, which it serves strict.
    #[must_use]
    pub fn served_as_named(self) -> bool {
        self != Durability::Ordered
    }

    /// What an ack promises, in words, for the tier AS SERVED — the broker's
    /// readiness line, `aterm fabric`, doctor and `on`, through
    /// [`describe_served`], which reads the tier served from the broker
    /// itself. `ordered`'s words are the tier's own, without the numbers its
    /// broker reports ([`describe_served`] says those); a broker of THIS build
    /// serves `ordered` strict and says strict's words.
    #[must_use]
    pub fn promise(self) -> &'static str {
        match self {
            Durability::Strict => "an ack is on the disk: a power loss loses nothing acked",
            Durability::Ordered => {
                "an ack is in order on the disk: a power loss keeps a prefix of whole acked \
                 batches, every one a full flush covered, and can lose the newest ones, and \
                 readers resume where the log continues"
            }
            Durability::Relaxed => {
                "an ack is in memory: a power loss can lose any record acked since the broker \
                 last started or stopped cleanly, and readers resume where the log continues"
            }
        }
    }

    /// The tier `requested` names in astream's terms (the `Hello` block's
    /// tag 0x01), as aterm says it: what a broker this host cannot read the
    /// argv of (a joined host's, a hand-run one in a sandbox) asked for.
    #[must_use]
    pub fn of_tier(requested: astream_broker::Tier) -> Option<Self> {
        match requested {
            astream_broker::Tier::Strict => Some(Durability::Strict),
            astream_broker::Tier::Ordered => Some(Durability::Ordered),
            astream_broker::Tier::Relaxed => Some(Durability::Relaxed),
            _ => None,
        }
    }

    /// Compatibility projection for configured argv: an unrecognised line
    /// retains the historical Strict fallback. Running observations and jobs
    /// being preserved must use [`Self::try_of_argv`] and retain Unknown.
    #[must_use]
    pub fn of_argv<S: AsRef<std::ffi::OsStr>>(argv: &[S]) -> Self {
        Self::try_of_argv(argv).unwrap_or_default()
    }

    /// A syntactically recognised serving invocation's tier — a bare argv's
    /// is the build's default ([`resolve`]) — using exactly the execution
    /// parser's option arities. This does not read keys, resolve hosts or
    /// prove a process is currently serving the claimed endpoint.
    #[must_use]
    pub fn try_of_argv<S: AsRef<std::ffi::OsStr>>(argv: &[S]) -> Option<Self> {
        crate::broker_args::BrokerArgs::from_invocation(argv)?.serving_durability()
    }

    /// What a recognised serving invocation NAMES: `Some(Some(tier))` for a
    /// `--durability <tier>`, `Some(None)` for a bare argv (the build's
    /// default), `None` for an argv that is not a recognised serving one. What
    /// `aterm fabric on` keeps from an installed job: a job that named strict
    /// stays named, so a later build whose default differs keeps it strict.
    #[must_use]
    pub fn named_of_argv<S: AsRef<std::ffi::OsStr>>(argv: &[S]) -> Option<Option<Self>> {
        crate::broker_args::BrokerArgs::from_invocation(argv)?.named_durability()
    }
}

/// The tier a broker runs whose argv names `named` (`None`: a bare argv,
/// which runs THE BUILD'S DEFAULT — strict in this build). Every judgement of
/// what a broker acks under goes through here, never through the argv's words.
#[must_use]
pub fn resolve(named: Option<Durability>) -> Durability {
    named.unwrap_or_default()
}

/// The words a broker argv carries for a tier: `--durability <tier>` for a
/// NAMED one — strict included, so a job that chose strict keeps it under a
/// build whose default differs — and nothing for a bare one, which runs the
/// build's default, as every argv written before the flag did.
#[must_use]
pub fn argv_words(named: Option<Durability>) -> Vec<String> {
    match named {
        Some(tier) => vec![FLAG.to_string(), tier.name().to_string()],
        None => Vec::new(),
    }
}

/// astream's tier (a `Hello` block's) as aterm's word; `other` for a code
/// this build does not know.
#[must_use]
pub fn tier_name(tier: astream_broker::Tier) -> &'static str {
    Durability::of_tier(tier).map_or("other", Durability::name)
}

/// Why a broker serves a tier other than the one asked for, in words.
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

/// WHAT AN ACK PROMISES, as a broker serves it: `asked` is the tier its argv
/// names (or, for a broker this host cannot read the argv of, the one its
/// `Hello` says was requested), `served` what its `Hello` block's tag 0x01
/// says it serves ([`astream_broker::Client::served_durability`]; `None` from
/// a broker that predates the block). The words `aterm fabric`'s `acks` row,
/// doctor and the broker's own readiness line print after the tier's name.
///
/// THE SERVED TIER IS READ FROM THE BROKER, NEVER INFERRED FROM THE ARGV, and
/// its words are the served tier's whatever was asked: a broker of another
/// build need not serve what this build would for the same argv. An `ordered`
/// argv run by THIS build serves strict and says "served strict"; a LATER
/// build's broker that serves ordered — named, or as its default for a bare
/// argv this build reads as strict — says ordered's words with the horizon and
/// the escalation rules it reports, never strict's "loses nothing acked"; and
/// whenever the tier served is not the one asked, the line says so, and why.
/// With no block the broker predates lineage (an aterm before this one): its
/// argv is all there is to go by, and a relaxed one's power loss gives the log
/// a NEW IDENTITY, which every reader forgets its place in — not a generation
/// readers resume on.
#[must_use]
pub fn describe_served(asked: Durability, served: Option<astream_broker::ServedInfo>) -> String {
    use astream_broker::{ServedReason, Tier};
    let Some(info) = served else {
        return match asked {
            Durability::Strict => Durability::Strict.promise().to_string(),
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
        Some(Durability::Ordered) => ordered_promise(&info),
        Some(tier) => tier.promise().to_string(),
        None => format!(
            "a tier this build cannot name ({:?}), whose promise it cannot say",
            info.served
        ),
    };
    if Durability::of_tier(info.served) == Some(asked) {
        return promise;
    }
    let why = if info.reason != ServedReason::None {
        // The broker lowered what it was asked for itself.
        served_reason(info.reason)
    } else if asked == Durability::Ordered && info.served == Tier::Strict {
        // A broker of THIS increment: it takes the word and opens the log strict.
        "its build serves ordered as strict".to_string()
    } else {
        // Asked one tier, opened at another, and not lowered: a broker of
        // another build — a later one whose default a bare argv runs.
        format!("its build opened it {}", tier_name(info.requested))
    };
    format!("served {} ({why}) — {promise}", tier_name(info.served))
}

/// What THIS build's broker serves for a tier `aterm fabric on` writes into
/// its job — `on`'s own note, said before any broker of it runs: `ordered` is
/// served strict, every other tier as named.
#[must_use]
pub fn describe_own(tier: Durability) -> String {
    if tier.served_as_named() {
        tier.promise().to_string()
    } else {
        format!(
            "served strict (this build serves ordered as strict) — {}",
            Durability::Strict.promise()
        )
    }
}

/// ORDERED'S PROMISE WITH THE NUMBERS ITS BROKER REPORTS: the horizon a full
/// flush follows an ack within (`None`: no horizon, so only an escalated
/// record, a flush asked for or a clean stop flushes in full) and how many
/// escalation rules flush their records in full before the ack. The bound is
/// astream's ([`astream_broker::ServedInfo::horizon_ms`]): the oldest ack no
/// full flush covers is at most the horizon plus one batch plus one full
/// flush old.
fn ordered_promise(info: &astream_broker::ServedInfo) -> String {
    let lost = match info.horizon_ms {
        Some(ms) if ms % 1000 == 0 && ms > 0 => format!(
            "a power loss can lose the acks of the last {} s, plus a batch and a full flush",
            ms / 1000
        ),
        Some(ms) => format!(
            "a power loss can lose the acks of the last {ms} ms, plus a batch and a full flush"
        ),
        None => "a power loss can lose every ack since the last full flush (an escalated \
                 record's, a flush asked for, or a clean stop's)"
            .to_string(),
    };
    let escalated = match info.rules {
        0 => "no record is flushed in full before its ack".to_string(),
        1 => "the records 1 escalation rule names are flushed in full before their ack".to_string(),
        n => format!("the records {n} escalation rules name are flushed in full before their ack"),
    };
    format!(
        "an ack is in order on the disk: the log keeps a prefix of whole acked batches; {lost}; \
         {escalated}; readers resume where the log continues"
    )
}

/// `<log>.aterm-relaxed`.
#[must_use]
pub fn mark_path(log: &Path) -> PathBuf {
    let mut s = log.as_os_str().to_os_string();
    s.push(MARK_SUFFIX);
    PathBuf::from(s)
}

/// astream's identity sidecar for `log`, `<log>.id`
/// ([`astream_broker::LogIdentity`]'s module docs).
#[cfg(test)]
fn identity_sidecar(log: &Path) -> PathBuf {
    let mut s = log.as_os_str().to_os_string();
    s.push(".id");
    PathBuf::from(s)
}

/// Tests that stand in a POWER LOSS by cutting a log: an unclean stop must come first.
/// astream's broker closes its log cleanly at shutdown — a full flush, then `Closed`
/// with the head in `<log>.id` — and no power loss can cut a log after that. A run
/// that did not stop cleanly leaves the sidecar its OPEN wrote, so a test captures it
/// right after the open ([`UncleanStop::capture`]) and puts it back after the broker
/// is gone, before it cuts the file ([`UncleanStop::leave`]).
#[cfg(test)]
pub(crate) struct UncleanStop(Vec<u8>);

#[cfg(test)]
impl UncleanStop {
    /// The sidecar as the open just wrote it.
    pub(crate) fn capture(log: &Path) -> UncleanStop {
        UncleanStop(std::fs::read(identity_sidecar(log)).expect("the open wrote <log>.id"))
    }

    /// Put it back: the log now stands as a run that never closed cleanly left it.
    pub(crate) fn leave(&self, log: &Path) {
        std::fs::write(identity_sidecar(log), &self.0).expect("rewrite <log>.id");
    }
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

/// A [`astream_broker::Lineage`] of these `(generation, common)` steps, oldest
/// first, for a test that needs one no real reboot made. astream builds one
/// only inside the crate (an unclean open's own decision); OUTSIDE it the one
/// door is the `Hello` answer a client parses. So this encodes a real `Nonce`
/// answer with an identity, appends the extension block's lineage entry in its
/// documented layout (tag 0x02, `len u16 LE`, `n u8`, `n × (generation u64 LE,
/// common u64 LE)`), and lets astream's own decoder read it back — the bytes a
/// broker would send, through the parser every client runs. `None` for steps
/// that parser refuses (generations that do not rise, more than
/// [`astream_broker::Lineage::MAX`]).
#[cfg(test)]
pub(crate) fn lineage_of(steps: &[(u64, u64)]) -> Option<astream_broker::Lineage> {
    use astream_broker::proto::{decode_response, encode_response};
    use astream_broker::{NonceExt, Response};
    let mut frame = encode_response(&Response::Nonce {
        nonce: vec![0; 32],
        log: Some(astream_broker::LogIdentity {
            id: [0; 16],
            generation: 0,
        }),
        ext: NonceExt::default(),
    });
    let mut body = vec![u8::try_from(steps.len()).ok()?];
    for (generation, common) in steps {
        body.extend_from_slice(&generation.to_le_bytes());
        body.extend_from_slice(&common.to_le_bytes());
    }
    frame.push(0x02);
    frame.extend_from_slice(&u16::try_from(body.len()).ok()?.to_le_bytes());
    frame.extend_from_slice(&body);
    match decode_response(&frame)? {
        Response::Nonce { ext, .. } => ext.lineage,
        _ => None,
    }
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

/// What a log's legacy mark says ([`MARK_SUFFIX`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mark {
    /// No mark.
    Absent,
    /// A relaxed broker of a build before lineage opened the log in this boot
    /// (its 16 bytes, as [`astream_sys::parse_boot_id`] reads the text).
    Boot([u8; 16]),
    /// Something is at the mark's name that does not say a boot.
    Unreadable,
}

/// Read the legacy mark beside `log`. Anything at its name that is not a mark
/// a broker wrote — unreadable, a directory, a damaged first line, a `boot=`
/// that is not a boot id astream would read ([module docs](self), "One boot,
/// one reading") — is [`Mark::Unreadable`], which reads like another boot: an
/// uncertain mark is never read as "nothing to fear".
#[must_use]
pub fn read_mark(log: &Path) -> Mark {
    let path = mark_path(log);
    match std::fs::read(&path) {
        Ok(bytes) => {
            let line = bytes.split(|b| *b == b'\n').next().unwrap_or_default();
            line.strip_prefix(b"boot=")
                .and_then(astream_sys::parse_boot_id)
                .map_or(Mark::Unreadable, Mark::Boot)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            // A dangling link is something at the name, not an absent mark.
            if std::fs::symlink_metadata(&path).is_ok() {
                Mark::Unreadable
            } else {
                Mark::Absent
            }
        }
        Err(_) => Mark::Unreadable,
    }
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
    /// The reason, in words: the broker's stderr, and astream's
    /// [`astream_broker::Prepared::Unclean`].
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

/// Whether a log whose legacy mark is `mark` may have lost acked records,
/// opened in boot `boot` ([module docs](self)): only a mark from ANOTHER
/// boot, or one this open cannot compare.
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

/// THIS BOOT, the one reading of it aterm-link takes ([module docs](self),
/// "One boot, one reading"): astream's [`astream_sys::boot_session`], `None`
/// where the kernel names no boot or answers something that is not one. The
/// broker opens its log in it (`cli.rs`, [`open`]'s `boot`) and the bridge's
/// incarnation lease trusts it (`state.rs`, `trusted_boot`), so the two can
/// never judge the page cache by different boots.
#[must_use]
pub fn this_boot() -> Option<[u8; 16]> {
    astream_sys::boot_session()
}

/// A boot id as the UUID text Linux's kernel writes (lower case,
/// `8-4-4-4-12`): what the broker's stderr names a boot by.
#[must_use]
pub fn boot_text(boot: &[u8; 16]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(36);
    for (i, b) in boot.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            out.push('-');
        }
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// What [`open`] opened, and what it did beside the open.
pub struct Opened {
    /// The broker, at the tier asked for (`ordered` served strict).
    pub broker: astream_broker::Broker,
    /// Why a legacy mark made the open's verdict unclean, when it did: astream
    /// then moved the log's generation (with lineage, when it could bound the
    /// loss), once, together with its own verdict.
    pub renewed: Option<Renewed>,
    /// An open that could not remove a legacy mark says why: the mark stays,
    /// which costs one needless generation move after the next reboot.
    pub mark_kept: Option<String>,
}

/// Open the broker over `log` at `durability` — guarded by `secret` when there
/// is one — in boot `boot` ([`this_boot`]; a parameter so a test can stand in a
/// reboot, and `None` is a boot astream cannot vouch for), through astream's
/// [`astream_broker::Broker::open_opts`], whose prepared step reads a legacy
/// mark into astream's unclean verdict ([module docs](self)). The mark is
/// removed once the log is open, at any tier.
///
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
    let mut opts = astream_broker::OpenOptions::new(durability.tier());
    // The same 16 bytes the legacy mark is compared with (`renewal` below):
    // astream's verdict and the mark's judge one boot. An unknown boot makes
    // every unclean weak run move the generation, the safe direction.
    opts.boot = boot.map_or(astream_broker::Boot::Unknown, astream_broker::Boot::Known);
    opts.guard = secret;
    // The mark is read INSIDE astream's open, under the log's own exclusive
    // lock and before astream reads `<log>.id`: a log another broker serves is
    // refused before anything here looks at it, and the verdict is the one
    // astream decides the generation by. astream has also created the log, or
    // found it, by then — a missing directory is its `NotFound`, never one
    // this makes.
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
        // astream runs `prepare` once before any open succeeds; an open that
        // did not is one whose mark nobody judged, so it does not serve.
        return Err(io::Error::other(
            "the broker log opened without its legacy mark being judged",
        ));
    };
    // The open's flush made every record a legacy relaxed run acked durable,
    // and astream has settled what the mark said: it has nothing left to
    // guard. The broker holds the log's lock here.
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

/// Remove `path` so a power loss does not bring it back. An absent one is
/// removed already.
fn remove_durably(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => crate::state::sync_dir(path.parent().unwrap_or(Path::new(""))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::LogWitness;

    /// A scratch dir short enough for a socket path (`sun_path`).
    fn scratch() -> aterm_tempfile::TempDir {
        aterm_tempfile::Builder::new()
            .prefix("dur")
            .tempdir_in("/tmp")
            .expect("scratch")
    }

    /// `n` publishes through the broker served at `sock`, each one acked
    /// before the next is sent.
    fn publish(sock: &Path, producer: u64, from_seq: u64, n: u64) {
        let mut c = astream_broker::Client::connect(sock).expect("connect");
        for seq in from_seq..from_seq + n {
            c.publish(producer, seq, "/f/f/x/history", b"v=1")
                .expect("acked");
        }
    }

    fn len(path: &Path) -> u64 {
        std::fs::metadata(path).expect("the log").len()
    }

    /// Cut the log file back to `len` bytes: what a power loss leaves of
    /// writes that never reached the disk.
    fn cut(log: &Path, len: u64) {
        std::fs::OpenOptions::new()
            .write(true)
            .open(log)
            .unwrap()
            .set_len(len)
            .unwrap();
    }

    /// Option values are whole argument words, including a filename that is
    /// itself the durability flag. Both accepted controls precede the aggregate.
    #[test]
    fn a_broker_option_value_is_not_a_durability_flag() {
        let base = ["aterm", "link", "broker", "/r/bus.sock"];
        assert_eq!(Durability::of_argv(&base), Durability::Strict);
        let explicit = [
            "aterm",
            "link",
            "broker",
            "/r/bus.sock",
            "--secret-file",
            "secret",
            "--durability",
            "relaxed",
        ];
        assert_eq!(Durability::of_argv(&explicit), Durability::Relaxed);
        let cases = [
            vec![
                "aterm",
                "link",
                "broker",
                "/r/bus.sock",
                "--secret-file",
                "--durability",
                "relaxed",
            ],
            vec![
                "aterm",
                "link",
                "broker",
                "/r/bus.sock",
                "--secret-file",
                "name with --durability relaxed",
                "bus.log",
            ],
            vec![
                "aterm-link",
                "broker",
                "/r/bus.sock",
                "--secret-file",
                "--durability=strict",
                "bus.log",
            ],
        ];
        let got: Vec<_> = cases.iter().map(|args| Durability::of_argv(args)).collect();
        assert_eq!(got, vec![Durability::Strict; cases.len()]);
    }

    #[cfg(unix)]
    #[test]
    fn only_a_recognised_serving_argv_has_a_known_durability() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;
        let words = |args: &[&str]| {
            args.iter()
                .map(|word| OsString::from(*word))
                .collect::<Vec<_>>()
        };
        let base = words(&["/renamed/aterm", "link", "broker", "/r/bus.sock"]);
        assert_eq!(Durability::try_of_argv(&base), Some(Durability::Strict));
        let mut named = base.clone();
        named.extend(words(&["--durability", "relaxed"]));
        assert_eq!(Durability::try_of_argv(&named), Some(Durability::Relaxed));
        let mut opaque = base.clone();
        opaque.extend(words(&["--secret-file"]));
        opaque.push(OsString::from_vec(
            b"/r/name\xff --durability relaxed".to_vec(),
        ));
        assert_eq!(Durability::try_of_argv(&opaque), Some(Durability::Strict));
        let invalid = [
            vec!["aterm", "link", "broker", "sock", "--secret-file"],
            vec![
                "aterm",
                "link",
                "broker",
                "sock",
                "--durability",
                "relaxed",
                "--durability",
                "strict",
            ],
            vec!["aterm", "link", "broker", "sock", "--durability", "bogus"],
            vec!["aterm", "link", "broker", "sock", "--unrecognised"],
            vec!["aterm", "link", "broker", "--repair", "log"],
            vec!["aterm", "link", "broker", "sock", "--help"],
            vec!["aterm", "link", "broker", "sock", "--key-file", "key"],
            vec!["aterm", "link", "broker", "sock", "extra", "surplus"],
            vec!["aterm", "link", "serve", "sock"],
        ];
        let got: Vec<_> = invalid
            .iter()
            .map(|args| Durability::try_of_argv(args))
            .collect();
        assert!(got.iter().all(Option::is_none));
        let named: Vec<_> = invalid
            .iter()
            .map(|args| Durability::named_of_argv(args))
            .collect();
        assert!(named.iter().all(Option::is_none));
    }

    /// THE WORD, THE ARGV AND THE DEFAULT. Every word round-trips, a bare
    /// argv names nothing and runs the build's default (strict), and a NAMED
    /// strict is written into the argv and read back as named — so a job that
    /// chose strict keeps it under a later build whose default differs. A word
    /// the broker would refuse reads as no recognised argv.
    #[test]
    fn the_word_the_argv_and_the_default() {
        for tier in [Durability::Strict, Durability::Ordered, Durability::Relaxed] {
            assert_eq!(Durability::parse(tier.name()), Ok(tier));
            let words = argv_words(Some(tier));
            assert_eq!(words, ["--durability", tier.name()]);
            let mut argv = ["aterm", "link", "broker", "/r/bus.sock", "/r/bus.log"]
                .map(str::to_string)
                .to_vec();
            argv.extend(words);
            assert_eq!(Durability::named_of_argv(&argv), Some(Some(tier)));
            assert_eq!(Durability::of_argv(&argv), tier);
        }
        assert!(Durability::parse("Relaxed").is_err());
        assert!(Durability::parse("fast").is_err());
        assert_eq!(Durability::default(), Durability::Strict);
        assert!(argv_words(None).is_empty(), "a bare argv names nothing");
        let bare = ["aterm", "link", "broker", "/r/bus.sock", "/r/bus.log"];
        assert_eq!(Durability::named_of_argv(&bare), Some(None));
        assert_eq!(Durability::of_argv(&bare), resolve(None));
        assert_eq!(resolve(None), Durability::Strict, "this build's default");
        assert_eq!(
            Durability::of_argv(&["broker", "--durability", "bogus"]),
            Durability::Strict,
            "a word the broker would refuse reads as no flag"
        );
    }

    /// `ORDERED IS ACCEPTED AND SERVED STRICT`. The word maps to astream's
    /// Strict, a broker opened with it answers `Hello` with a Strict tier
    /// served as asked (the request astream saw was strict), and every line
    /// that describes it says "served strict" with this build's reason —
    /// never the ordered promise, which this build does not keep. A broker
    /// that was asked for ordered and lowered it itself names its own reason.
    #[test]
    fn ordered_is_accepted_and_served_strict() {
        use astream_broker::{ServedInfo, ServedReason, Tier};
        assert_eq!(
            Durability::Ordered.tier(),
            astream_broker::Durability::Strict
        );
        assert!(!Durability::Ordered.served_as_named());
        assert!(Durability::Strict.served_as_named() && Durability::Relaxed.served_as_named());
        let dir = scratch();
        let log = dir.path().join("bus.log");
        let opened = open(&log, None, Durability::Ordered, Some(test_boot(1))).unwrap();
        let served = opened.broker.served_durability();
        assert_eq!(
            (served.requested, served.served, served.reason),
            (Tier::Strict, Tier::Strict, ServedReason::None)
        );
        let said = describe_served(Durability::Ordered, Some(served));
        assert!(
            said.starts_with("served strict (its build serves ordered as strict)"),
            "{said}"
        );
        assert!(said.ends_with(Durability::Strict.promise()), "{said}");
        assert_eq!(
            describe_own(Durability::Ordered),
            format!(
                "served strict (this build serves ordered as strict) — {}",
                Durability::Strict.promise()
            ),
            "`on` says what this build will serve"
        );
        let lowered = ServedInfo {
            requested: Tier::Ordered,
            served: Tier::Strict,
            reason: ServedReason::NotBootVolume,
            horizon_ms: Some(1000),
            rules: 0,
        };
        let said = describe_served(Durability::Ordered, Some(lowered));
        assert!(said.contains("its log is not on the boot volume"), "{said}");
        // The words are the SERVED tier's, whatever was asked: a broker that reports
        // strict is described as strict even when this host asked relaxed of it.
        let said = describe_served(Durability::Relaxed, Some(served));
        assert!(said.starts_with("served strict ("), "{said}");
        assert!(said.ends_with(Durability::Strict.promise()), "{said}");
        let relaxed = open(
            &dir.path().join("relaxed.log"),
            None,
            Durability::Relaxed,
            Some(test_boot(1)),
        )
        .unwrap();
        assert_eq!(
            describe_served(
                Durability::Relaxed,
                Some(relaxed.broker.served_durability())
            ),
            Durability::Relaxed.promise(),
            "a tier served as asked says its own promise"
        );
    }

    /// A BROKER THAT SERVES ORDERED IS NEVER TOLD AS STRICT. A later build's
    /// broker reports ordered in its `Hello`: asked ordered (a named job, or a
    /// joined host's broker, whose requested tier is the one asked), or asked
    /// what this build reads a bare argv as (strict) while that build's own
    /// default is ordered. Either way the line says ordered's words with the
    /// broker's own horizon and escalation count — never strict's "loses nothing
    /// acked", which a power loss there would make a lie — and in the second
    /// case says the tier served differs from the one asked. A broker with no
    /// block (older than lineage) running relaxed is told with the cost such a
    /// broker has: a new identity readers forget, not a generation they resume
    /// on.
    #[test]
    fn a_broker_that_serves_ordered_is_never_told_as_strict() {
        use astream_broker::{ServedInfo, ServedReason, Tier};
        let ordered = ServedInfo {
            requested: Tier::Ordered,
            served: Tier::Ordered,
            reason: ServedReason::None,
            horizon_ms: Some(1000),
            rules: 4,
        };
        let strict = Durability::Strict.promise();
        let said = describe_served(Durability::Ordered, Some(ordered));
        assert!(said.starts_with("an ack is in order on the disk"), "{said}");
        assert!(said.contains("the last 1 s"), "{said}");
        assert!(said.contains("4 escalation rules"), "{said}");
        assert!(
            !said.contains(strict) && !said.contains("loses nothing"),
            "{said}"
        );
        let said = describe_served(Durability::Strict, Some(ordered));
        assert!(
            said.starts_with("served ordered (its build opened it ordered) — an ack is in order"),
            "{said}"
        );
        assert!(!said.contains("loses nothing"), "{said}");
        let unbounded = ServedInfo {
            horizon_ms: None,
            rules: 1,
            ..ordered
        };
        let said = describe_served(Durability::Ordered, Some(unbounded));
        assert!(
            said.contains("every ack since the last full flush"),
            "{said}"
        );
        assert!(said.contains("1 escalation rule names"), "{said}");
        let said = describe_served(Durability::Relaxed, None);
        assert!(said.contains("a new identity"), "{said}");
        assert!(!said.contains("resume where the log continues"), "{said}");
        assert_eq!(describe_served(Durability::Strict, None), strict);
        assert_eq!(
            Durability::of_tier(Tier::Ordered),
            Some(Durability::Ordered)
        );
        assert_eq!(Durability::of_tier(Tier::Other(9)), None);
    }

    /// The legacy mark reads as unclean against exactly another boot, an
    /// unreadable mark or no boot to compare — never the same boot, never no
    /// mark. A mark's boot is read by astream's own parser: the kernel's text
    /// in either case is the same 16 bytes, and text astream would not take
    /// for a boot — a short or stray id, the reserved all-zero one — is an
    /// unreadable mark, never a boot that could match.
    #[test]
    fn only_another_boot_or_an_uncertain_mark_is_unclean() {
        let (one, two) = (test_boot(1), test_boot(2));
        let b1 = Mark::Boot(one);
        assert_eq!(renewal(&Mark::Absent, Some(one)), None);
        assert_eq!(renewal(&Mark::Absent, None), None);
        assert_eq!(renewal(&b1, Some(one)), None);
        assert_eq!(
            renewal(&b1, Some(two)),
            Some(Renewed::Rebooted { was: one, now: two })
        );
        assert_eq!(renewal(&b1, None), Some(Renewed::NoBoot));
        assert_eq!(
            renewal(&Mark::Unreadable, Some(one)),
            Some(Renewed::Unreadable)
        );
        assert_eq!(boot_text(&one), "00000000-0000-0000-0000-000000000101");
        let said = Renewed::Rebooted { was: one, now: two }.describe();
        assert!(
            said.contains(&boot_text(&one)) && said.contains(&boot_text(&two)),
            "{said}"
        );

        let dir = scratch();
        let log = dir.path().join("bus.log");
        assert_eq!(read_mark(&log), Mark::Absent);
        std::fs::write(mark_path(&log), legacy_mark_text(&one)).unwrap();
        assert_eq!(read_mark(&log), b1, "macOS's upper-case text");
        std::fs::write(mark_path(&log), legacy_mark_of(&boot_text(&one))).unwrap();
        assert_eq!(read_mark(&log), b1, "Linux's lower-case text");
        for damaged in [
            "",
            "boot=\n",
            "boot=a b\n",
            "b1\n",
            "boot=\u{7}\n",
            // Printable, and so a boot to the text reader this replaced: a
            // mark astream could never match is no boot, never one that could.
            "boot=b1\n",
            "boot=00000000-0000-0000-0000-00000000010\n",
            "boot=00000000-0000-0000-0000-000000000000\n",
        ] {
            std::fs::write(mark_path(&log), damaged).unwrap();
            assert_eq!(read_mark(&log), Mark::Unreadable, "{damaged:?}");
        }
        std::fs::remove_file(mark_path(&log)).unwrap();
        std::os::unix::fs::symlink("gone", mark_path(&log)).unwrap();
        assert_eq!(read_mark(&log), Mark::Unreadable, "a dangling link");
    }

    /// The kernel's own text for this boot, read as a build before this one
    /// read it and NOT through astream: `sysctl -n kern.bootsessionuuid` on
    /// macOS (the sysctl that build called), `/proc/sys/kernel/random/boot_id`
    /// on Linux. `None` where neither names one. The wait is a hang detector.
    fn kernel_boot_text() -> Option<String> {
        if cfg!(target_vendor = "apple") {
            let mut sysctl = aterm_uds::spawnfd::command("/usr/sbin/sysctl");
            sysctl.args(["-n", "kern.bootsessionuuid"]);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            let out = aterm_uds::deadline::output_until(&mut sysctl, deadline).ok()?;
            out.status
                .success()
                .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            let text = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
            Some(text.trim().to_string())
        }
    }

    /// ONE BOOT: THE BROKER'S, THE LEASE'S, AND THE ONE AN OLDER BUILD WROTE.
    /// `cli.rs` opens the broker's log in [`this_boot`]; the bridge's
    /// incarnation lease trusts the same bytes for a state dir on the boot
    /// volume (`StateDir::trusted_boot`); and a legacy mark an older build
    /// wrote in THIS boot — the kernel's text, read here the way that build
    /// read it ([`kernel_boot_text`]), not through astream — reads back as that
    /// boot, so the upgrade's first open, a real relaxed open of a cleanly
    /// stopped log, moves nothing and removes the mark. Were the readings to
    /// disagree (a parse that kept the text's case, a boot read two ways), every
    /// relaxed log would lose a generation to the upgrade, and the lease and
    /// the log would judge the page cache by different boots.
    ///
    /// Negative control: a mark from another boot, opened in this one through
    /// the same path, is a reboot and moves the generation once — the
    /// agreement is the bytes, not an open that never moves.
    #[test]
    fn the_broker_the_lease_and_an_older_builds_mark_name_one_boot() {
        let boot = this_boot();
        if cfg!(any(target_vendor = "apple", target_os = "linux")) {
            assert!(boot.is_some(), "this host's kernel names its boot");
        }
        let Some(boot) = boot else {
            return; // a kernel that names no boot: nothing to agree on
        };
        let kernel_text = kernel_boot_text().expect("the kernel's own text for its boot");

        let dir = scratch();
        let state = crate::state::StateDir::open(dir.path().join("state")).expect("state dir");
        let lease = state.trusted_boot();
        if cfg!(target_vendor = "apple") {
            // The temp dir is on the boot volume's data volume, as `~` is.
            assert_eq!(lease, Some(boot), "the lease trusts the broker's boot");
        } else {
            // A tmpfs `/tmp` is off the boot volume: no lease at all, never another boot.
            assert!(lease.is_none_or(|b| b == boot), "{lease:?} vs {boot:?}");
        }

        let log = dir.path().join("bus.log");
        let sock = dir.path().join("bus.sock");
        let first = {
            let opened = open(&log, None, Durability::Relaxed, this_boot()).expect("open");
            let mut h = opened.broker.serve(&sock).unwrap();
            publish(&sock, 7, 1, 3);
            h.shutdown();
            opened.broker.log_identity()
        }; // dropped: a clean stop
        std::fs::write(mark_path(&log), legacy_mark_of(&kernel_text)).unwrap();
        assert_eq!(read_mark(&log), Mark::Boot(boot), "{kernel_text:?}");
        let upgraded = open(&log, None, Durability::Relaxed, this_boot()).expect("reopen");
        assert_eq!(upgraded.renewed, None, "the mark is this boot's");
        assert_eq!(upgraded.broker.log_identity(), first, "nothing moves");
        assert_eq!(read_mark(&log), Mark::Absent, "the mark is removed");
        drop(upgraded);

        // NEGATIVE CONTROL: another boot's mark, the same open in this boot.
        let other = test_boot(1);
        assert_ne!(other, boot);
        std::fs::write(mark_path(&log), legacy_mark_text(&other)).unwrap();
        let moved = open(&log, None, Durability::Relaxed, this_boot()).expect("reopen");
        assert_eq!(
            moved.renewed,
            Some(Renewed::Rebooted {
                was: other,
                now: boot
            })
        );
        assert_eq!(
            moved.broker.log_identity().generation,
            first.generation + 1,
            "another boot's mark moves the generation once"
        );
    }

    /// THE HAZARD, AND ASTREAM'S CURE, ON REAL FILES. A relaxed broker in
    /// boot 1 acks ten records; a consumer witnesses head 10 under the log's
    /// identity. A power loss keeps only the first four (the file cut back to
    /// their length: the six later acks were never flushed), after a stop that
    /// did not close the log cleanly.
    ///
    /// The next boot's open moves the GENERATION — the same `id`, generation
    /// plus one — and records the LINEAGE `(generation, 4)`: offsets below 4
    /// name the same records under both. The witness calls it another log,
    /// grown or not, and [`LogWitness::continues`] says where it continues: at
    /// 4, so a reader resumes there instead of forgetting.
    ///
    /// Negative control, on a copy of exactly those files: opened in the SAME
    /// boot it keeps the identity (a same-boot reopen is a process crash,
    /// which loses nothing) — and once regrown past 10, the witness cannot
    /// tell it from the log it saw. Only the reboot announces the loss.
    #[test]
    fn a_power_loss_under_relaxed_moves_the_generation_with_lineage() {
        let dir = scratch();
        let log = dir.path().join("bus.log");
        let sock = dir.path().join("bus.sock");
        let unclean;
        let (before, witnessed, at4) = {
            let opened = open(&log, None, Durability::Relaxed, Some(test_boot(1))).expect("open");
            unclean = UncleanStop::capture(&log);
            assert_eq!(opened.renewed, None, "a fresh log has nothing to judge");
            assert_eq!(read_mark(&log), Mark::Absent, "no mark is written");
            let mut h = opened.broker.serve(&sock).expect("serve");
            publish(&sock, 7, 1, 4);
            let at4 = len(&log);
            publish(&sock, 7, 5, 6);
            let witnessed = LogWitness {
                high: opened.broker.head(),
                id: Some(opened.broker.log_identity()),
            };
            assert_eq!(witnessed.high, 10);
            h.shutdown();
            (opened.broker.log_identity(), witnessed, at4)
        };
        unclean.leave(&log);
        cut(&log, at4);

        // NEGATIVE CONTROL: the same cut files, opened in the same boot.
        let control = dir.path().join("control.log");
        std::fs::copy(&log, &control).unwrap();
        std::fs::copy(identity_sidecar(&log), identity_sidecar(&control)).unwrap();
        {
            let b = open(&control, None, Durability::Relaxed, Some(test_boot(1))).unwrap();
            assert_eq!(b.broker.head(), 4, "the cut log is a clean prefix");
            assert_eq!(b.broker.log_identity(), before, "under the SAME identity");
            let csock = dir.path().join("c.sock");
            let mut h = b.broker.serve(&csock).unwrap();
            publish(&csock, 8, 1, 7);
            assert_eq!(b.broker.head(), 11);
            assert!(
                !witnessed.replaced_by(b.broker.head(), Some(b.broker.log_identity())),
                "regrown past the high-water, the cut log passes for the one witnessed"
            );
            h.shutdown();
        }

        // THE CURE: the next boot's open moves the generation, with lineage.
        let opened = open(&log, None, Durability::Relaxed, Some(test_boot(2))).expect("reopen");
        assert_eq!(opened.renewed, None, "astream's own verdict, no mark");
        assert_eq!(opened.broker.head(), 4);
        let now = opened.broker.log_identity();
        assert_eq!(now.id, before.id, "the same log");
        assert_eq!(now.generation, before.generation + 1, "the next generation");
        assert!(witnessed.replaced_by(4, Some(now)));
        let mut h = opened.broker.serve(&sock).unwrap();
        let mut client = astream_broker::Client::connect(&sock).unwrap();
        assert_eq!(client.log_identity().unwrap(), Some(now));
        let lineage = client
            .log_lineage()
            .cloned()
            .expect("the Hello carries lineage");
        assert_eq!(lineage.steps().last(), Some(&(now.generation, 4)));
        assert_eq!(witnessed.continues(Some(now), Some(&lineage)), Some(4));
        assert_eq!(
            witnessed.continues(Some(now), None),
            None,
            "no lineage, no continuation: forget, as before"
        );
        publish(&sock, 8, 1, 7);
        assert!(
            witnessed.replaced_by(opened.broker.head(), Some(now)),
            "grown or not, it is another generation"
        );
        h.shutdown();
    }

    /// A RESTART IN ONE BOOT IS NOT A LOG CHANGE: the page cache outlived the
    /// broker, so a relaxed reopen after a stop that did not close the log
    /// keeps the identity and the records. A reboot after a CLEAN STOP (the
    /// broker's own shutdown: a full flush, then `Closed`) keeps it too, at
    /// either tier. Only a reboot after an unclean relaxed stop moves the
    /// generation, and a strict run never does: its acks were on the disk.
    #[test]
    fn a_restart_or_a_clean_stop_keeps_the_identity() {
        let dir = scratch();
        let log = dir.path().join("bus.log");
        let sock = dir.path().join("bus.sock");
        let first = {
            let opened = open(&log, None, Durability::Relaxed, Some(test_boot(1))).unwrap();
            let unclean = UncleanStop::capture(&log);
            let mut h = opened.broker.serve(&sock).unwrap();
            publish(&sock, 7, 1, 3);
            h.shutdown();
            unclean.leave(&log);
            opened.broker.log_identity()
        };
        // A same-boot reopen after the unclean stop: a process crash.
        let again = open(&log, None, Durability::Relaxed, Some(test_boot(1))).unwrap();
        assert_eq!(again.broker.log_identity(), first);
        assert_eq!(again.broker.head(), 3);
        // Dropped unserved: astream closes the log cleanly.
        drop(again);
        let rebooted = open(&log, None, Durability::Relaxed, Some(test_boot(2))).unwrap();
        assert_eq!(
            rebooted.broker.log_identity(),
            first,
            "a clean stop's reboot moves nothing"
        );
        let unclean = UncleanStop::capture(&log);
        drop(rebooted);
        unclean.leave(&log);
        // A strict open in another boot after an unclean RELAXED stop moves it;
        // a strict run's own unclean stop, reopened in another boot, does not.
        let strict = open(&log, None, Durability::Strict, Some(test_boot(3))).unwrap();
        let moved = strict.broker.log_identity();
        assert_eq!(
            (moved.id, moved.generation),
            (first.id, first.generation + 1)
        );
        let unclean = UncleanStop::capture(&log);
        drop(strict);
        unclean.leave(&log);
        let after_strict = open(&log, None, Durability::Strict, Some(test_boot(4))).unwrap();
        assert_eq!(
            after_strict.broker.log_identity(),
            moved,
            "strict acks were durable"
        );
    }

    /// THE LEGACY-MARK MIGRATION: ONE DECISION, NO DOUBLE RENEWAL. A build
    /// before lineage left a relaxed run unclean in boot 1: its mark and its
    /// `Open` sidecar both stand. The first open of this build in boot 2 has
    /// TWO unclean verdicts — the mark's (`Prepared::Unclean`) and astream's
    /// own — and moves the generation exactly once, with lineage; the mark is
    /// removed and no new one written. A clean stop and a reboot then move
    /// nothing; an unclean stop and a reboot move it once more. A mark from
    /// THIS boot moves nothing and is removed, at either tier. Two sequential
    /// opens in one boot move it at most once.
    #[test]
    fn the_legacy_mark_moves_the_generation_once_and_is_removed() {
        let dir = scratch();
        let log = dir.path().join("bus.log");
        let sock = dir.path().join("bus.sock");
        let (b1, b2, b3, b4) = (test_boot(1), test_boot(2), test_boot(3), test_boot(4));
        let first = {
            let opened = open(&log, None, Durability::Relaxed, Some(b1)).unwrap();
            let unclean = UncleanStop::capture(&log);
            let mut h = opened.broker.serve(&sock).unwrap();
            publish(&sock, 7, 1, 3);
            h.shutdown();
            unclean.leave(&log);
            opened.broker.log_identity()
        };
        std::fs::write(mark_path(&log), legacy_mark_text(&b1)).unwrap();
        let generations = |id: astream_broker::LogIdentity| (id.id, id.generation);

        let migrated = open(&log, None, Durability::Relaxed, Some(b2)).unwrap();
        assert!(matches!(migrated.renewed, Some(Renewed::Rebooted { .. })));
        assert_eq!(migrated.mark_kept, None);
        assert_eq!(read_mark(&log), Mark::Absent, "the mark is removed");
        let second = migrated.broker.log_identity();
        assert_eq!(
            generations(second),
            (first.id, first.generation + 1),
            "two verdicts, ONE move"
        );
        assert_eq!(
            migrated.broker.served_durability().served,
            astream_broker::Tier::Relaxed
        );
        drop(migrated); // a clean stop
        let clean = open(&log, None, Durability::Relaxed, Some(b3)).unwrap();
        assert_eq!(
            clean.broker.log_identity(),
            second,
            "clean stop, reboot: nothing"
        );
        assert_eq!(read_mark(&log), Mark::Absent, "and no mark written");
        let unclean = UncleanStop::capture(&log);
        drop(clean);
        unclean.leave(&log);
        let again = open(&log, None, Durability::Relaxed, Some(b4)).unwrap();
        let third = again.broker.log_identity();
        assert_eq!(generations(third), (first.id, second.generation + 1));
        drop(again);

        // A mark from THIS boot: removed, nothing moves, at either tier.
        for tier in [Durability::Relaxed, Durability::Strict] {
            std::fs::write(mark_path(&log), legacy_mark_text(&b4)).unwrap();
            let same = open(&log, None, tier, Some(b4)).unwrap();
            assert_eq!(same.renewed, None, "{tier:?}");
            assert_eq!(same.broker.log_identity(), third, "{tier:?}");
            assert_eq!(read_mark(&log), Mark::Absent, "{tier:?}");
        }
        // An unreadable mark with no boot to compare: unclean, once.
        std::fs::write(mark_path(&log), "boot=\n").unwrap();
        let unknown = open(&log, None, Durability::Strict, None).unwrap();
        assert_eq!(unknown.renewed, Some(Renewed::Unreadable));
        assert_eq!(
            generations(unknown.broker.log_identity()),
            (first.id, third.generation + 1)
        );
        drop(unknown);
        let next = open(&log, None, Durability::Strict, None).unwrap();
        assert_eq!(
            generations(next.broker.log_identity()),
            (first.id, third.generation + 1),
            "the next open moves nothing more"
        );
    }

    /// THE LINEAGE WINDOW, END TO END: astream keeps the newest
    /// [`astream_broker::Lineage::MAX`] steps, and a reader whose generation
    /// needs an older one forgets ([`crate::state::LogWitness::continues`]
    /// answers `None`), exactly as before lineage. The model's Tier 1
    /// (`state.rs`, `the_reader_resume_refines_its_model`) runs the real reader
    /// over lineages of at most two steps; this runs it over what a real broker
    /// log records after `MAX + 1` unclean stops, each opened in the next boot:
    /// a witness one generation inside the window resumes at the head every
    /// step agrees below, and one a single generation older forgets.
    #[test]
    fn a_reader_older_than_the_lineage_window_forgets() {
        let dir = scratch();
        let log = dir.path().join("bus.log");
        let sock = dir.path().join("bus.sock");
        let first = {
            let opened = open(&log, None, Durability::Relaxed, Some(test_boot(1))).unwrap();
            let unclean = UncleanStop::capture(&log);
            let mut h = opened.broker.serve(&sock).unwrap();
            publish(&sock, 7, 1, 3);
            h.shutdown();
            unclean.leave(&log);
            opened.broker.log_identity()
        };
        let moves = u64::try_from(astream_broker::Lineage::MAX).unwrap() + 1;
        let mut now = first;
        let mut lineage = None;
        for n in 0..moves {
            let opened = open(
                &log,
                None,
                Durability::Relaxed,
                Some(test_boot(u8::try_from(2 + n).unwrap())),
            )
            .unwrap();
            now = opened.broker.log_identity();
            assert_eq!(now.generation, first.generation + n + 1, "one move a boot");
            let unclean = UncleanStop::capture(&log);
            let mut h = opened.broker.serve(&sock).unwrap();
            {
                let mut c = astream_broker::Client::connect(&sock).unwrap();
                assert_eq!(c.log_identity().unwrap(), Some(now));
                lineage = c.log_lineage().cloned();
            }
            h.shutdown();
            unclean.leave(&log);
        }
        let lineage = lineage.expect("the Hello carries lineage");
        assert_eq!(lineage.steps().len(), astream_broker::Lineage::MAX);
        let witness = |generation| crate::state::LogWitness {
            high: 3,
            id: Some(astream_broker::LogIdentity {
                generation,
                ..first
            }),
        };
        assert_eq!(
            witness(first.generation + 1).continues(Some(now), Some(&lineage)),
            Some(3),
            "every step from the oldest kept on is in the window: resumed"
        );
        assert_eq!(
            witness(first.generation).continues(Some(now), Some(&lineage)),
            None,
            "the step this generation needs has fallen off: another log, forgotten"
        );
    }

    /// THE LEGACY MARK ALONE MOVES THE GENERATION WITH LINEAGE. Every other
    /// lineage check in this crate goes through astream's own verdict (an `Open`
    /// sidecar opened in another boot); a log a build before lineage left
    /// marked moves ONLY on the embedder's verdict (`Prepared::Unclean`), so
    /// this one leaves astream nothing of its own to go by: the boot-1 run
    /// stops cleanly (`Closed`), and the mark is planted after it. The move
    /// must still record `(g + 1, head)`, or every reader of a migrated log
    /// would forget its place — the cost before lineage — while the docs say it
    /// resumes. The unreadable mark over a strict log, the other prepared-only
    /// path, records it too.
    #[test]
    fn a_legacy_mark_alone_moves_the_generation_with_lineage() {
        let dir = scratch();
        let log = dir.path().join("bus.log");
        let sock = dir.path().join("bus.sock");
        let lineage_of_served = |broker: &astream_broker::Broker| {
            let mut h = broker.serve(&sock).unwrap();
            let lineage = {
                let mut c = astream_broker::Client::connect(&sock).unwrap();
                assert_eq!(c.log_identity().unwrap(), Some(broker.log_identity()));
                c.log_lineage().cloned()
            };
            h.shutdown();
            lineage
        };
        let first = {
            let opened = open(&log, None, Durability::Relaxed, Some(test_boot(1))).unwrap();
            let mut h = opened.broker.serve(&sock).unwrap();
            publish(&sock, 7, 1, 3);
            h.shutdown();
            opened.broker.log_identity()
        };
        let witnessed = crate::state::LogWitness {
            high: 3,
            id: Some(first),
        };
        for (n, mark, tier) in [
            (2, legacy_mark_text(&test_boot(1)), Durability::Relaxed),
            (3, "boot=\n".to_string(), Durability::Strict),
        ] {
            std::fs::write(mark_path(&log), mark).unwrap();
            let migrated = open(&log, None, tier, Some(test_boot(n))).unwrap();
            assert!(migrated.renewed.is_some(), "{tier:?}: the mark's verdict");
            let now = migrated.broker.log_identity();
            assert_eq!(
                (now.id, now.generation),
                (first.id, first.generation + u64::from(n) - 1),
                "{tier:?}: one move"
            );
            assert_eq!(migrated.broker.head(), 3);
            let lineage = lineage_of_served(&migrated.broker)
                .unwrap_or_else(|| panic!("{tier:?}: the Hello carries lineage"));
            assert_eq!(
                lineage.steps().last(),
                Some(&(now.generation, 3)),
                "{tier:?}: the generations agree below the head"
            );
            assert_eq!(
                witnessed.continues(Some(now), Some(&lineage)),
                Some(3),
                "{tier:?}: a reader of the old generation resumes, not forgets"
            );
        }
    }

    /// RELAXED ON A HOST THAT NAMES NO BOOT RUNS, the safe way: astream
    /// cannot tell a reboot from a restart there, so every unclean stop moves
    /// the generation, a process crash included. A log another broker serves
    /// is refused with its mark and its identity untouched, even when the mark
    /// is another boot's: astream refuses the open before the prepared step
    /// reads it. A log in a directory that does not exist is refused
    /// `NotFound` at every tier, and the directory is not made.
    #[test]
    fn what_open_refuses_and_what_it_leaves_alone() {
        let dir = scratch();
        let log = dir.path().join("bus.log");
        let sock = dir.path().join("bus.sock");
        let first = {
            let opened = open(&log, None, Durability::Relaxed, None).expect("runs, unknown boot");
            let unclean = UncleanStop::capture(&log);
            let mut h = opened.broker.serve(&sock).unwrap();
            publish(&sock, 7, 1, 2);
            h.shutdown();
            unclean.leave(&log);
            opened.broker.log_identity()
        };
        let crashed = open(&log, None, Durability::Relaxed, None).unwrap();
        assert_eq!(
            crashed.broker.log_identity().generation,
            first.generation + 1,
            "with no boot to compare, even a restart moves the generation"
        );
        drop(crashed);

        for durability in [Durability::Relaxed, Durability::Ordered, Durability::Strict] {
            let missing = dir.path().join("missing");
            let err = open(
                &missing.join("bus.log"),
                None,
                durability,
                Some(test_boot(1)),
            )
            .err()
            .expect("no directory, no log");
            assert_eq!(err.kind(), io::ErrorKind::NotFound, "{durability:?}: {err}");
            assert!(!missing.exists(), "{durability:?}: no directory made");
        }

        let held = open(&log, None, Durability::Strict, Some(test_boot(1))).unwrap();
        std::fs::write(mark_path(&log), legacy_mark_text(&test_boot(9))).unwrap();
        let sidecar = std::fs::read(identity_sidecar(&log)).unwrap();
        let err = open(&log, None, Durability::Relaxed, Some(test_boot(1)))
            .err()
            .expect("a second broker on one log");
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists, "{err}");
        assert_eq!(
            read_mark(&log),
            Mark::Boot(test_boot(9)),
            "the mark is the holder's"
        );
        assert_eq!(std::fs::read(identity_sidecar(&log)).unwrap(), sidecar);
        drop(held);
    }

    /// The guarded broker takes the tier too (astream's `OpenOptions::guard`):
    /// a relaxed guarded open refuses an un-attached publish like a strict one.
    #[test]
    fn a_guarded_relaxed_broker_is_still_guarded() {
        let dir = scratch();
        let log = dir.path().join("bus.log");
        let sock = dir.path().join("bus.sock");
        let opened = open(
            &log,
            Some(b"a-mint-secret-of-thirty-two-bytes".to_vec()),
            Durability::Relaxed,
            Some(test_boot(1)),
        )
        .unwrap();
        let mut h = opened.broker.serve(&sock).unwrap();
        let err = astream_broker::Client::connect(&sock)
            .unwrap()
            .publish(1, 1, "/f/f/x", b"anon")
            .unwrap_err();
        assert!(err.to_string().contains("unauthorized"), "{err}");
        h.shutdown();
    }
}
