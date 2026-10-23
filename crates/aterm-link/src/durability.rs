// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Broker ack durability tiers, escalation rules, and legacy relaxed-mark migration.

use std::io;
use std::path::{Path, PathBuf};

/// The flag a broker argv carries for a named tier.
pub const FLAG: &str = "--durability";

/// Dev-only volume seam environment variable.
pub const VOLUME_SEAM: &str = "ATERM_LINK_BROKER_VOLUME";

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

    /// Broker [`astream_broker::OpenOptions`] for this tier, attaching [`escalation`] on weak tiers.
    #[must_use]
    pub fn options(self) -> astream_broker::OpenOptions {
        let mut opts = astream_broker::OpenOptions::new(self.tier());
        if self != Durability::Strict {
            opts.escalation = escalation();
        }
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

/// Format a 16-byte boot id as a lowercase `8-4-4-4-12` UUID string.
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
    open_escalating(log, secret, durability, boot, Vec::new())
}

/// [`open`] with `extra` escalation rules appended (used by dev fault-injection seams).
/// # Errors
///
/// As [`open`], and astream's refusal of a rule that does not parse.
pub fn open_escalating(
    log: &Path,
    secret: Option<Vec<u8>>,
    durability: Durability,
    boot: Option<[u8; 16]>,
    extra: Vec<astream_broker::Escalate>,
) -> io::Result<Opened> {
    let mut opts = durability.options();
    opts.boot = boot.map_or(astream_broker::Boot::Unknown, astream_broker::Boot::Known);
    opts.guard = secret;
    if aterm_types::dev_seam!(VOLUME_SEAM).is_some_and(|v| v == "other") {
        opts.volume = astream_broker::Volume::Other;
    }
    if !extra.is_empty() {
        let rules = opts.escalation.rules().to_vec();
        opts.escalation = astream_broker::Escalation::new(rules.into_iter().chain(extra));
    }
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
mod tests {
    use super::*;
    use crate::state::LogWitness;

    fn scratch() -> aterm_tempfile::TempDir {
        aterm_tempfile::Builder::new()
            .prefix("dur")
            .tempdir_in("/tmp")
            .expect("scratch")
    }

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

    fn cut(log: &Path, len: u64) {
        std::fs::OpenOptions::new()
            .write(true)
            .open(log)
            .unwrap()
            .set_len(len)
            .unwrap();
    }

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
        assert_eq!(got, vec![default_for(true); cases.len()]);
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
        assert_eq!(
            Durability::try_of_argv(&opaque),
            Some(default_for(true)),
            "a secret file whose name spells a tier names none"
        );
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
        assert_eq!(Durability::of_argv(&bare), resolve(None, false));
        assert_eq!(
            resolve(None, false),
            Durability::Strict,
            "unguarded: strict"
        );
        assert_eq!(
            resolve(None, true),
            Durability::Ordered,
            "the guarded broker's default, on every platform: astream decides what is served"
        );
        let guarded = [
            "aterm",
            "link",
            "broker",
            "/r/bus.sock",
            "/r/bus.log",
            "--secret-file",
            "/r/mint.secret",
        ];
        assert_eq!(Durability::named_of_argv(&guarded), Some(None));
        assert_eq!(Durability::of_argv(&guarded), resolve(None, true));
        for named in [Durability::Strict, Durability::Ordered, Durability::Relaxed] {
            for guarded in [false, true] {
                assert_eq!(resolve(Some(named), guarded), named, "{named:?} {guarded}");
            }
        }
        assert_eq!(
            Durability::of_argv(&["broker", "--durability", "bogus"]),
            Durability::Strict,
            "a word the broker would refuse reads as no flag"
        );
    }

    #[test]
    fn ordered_is_served_ordered_with_aterms_table() {
        use astream_broker::{ServedInfo, ServedReason, Tier};
        assert_eq!(
            Durability::Ordered.tier(),
            astream_broker::Durability::Ordered {
                horizon: Some(ORDERED_HORIZON)
            }
        );
        assert_eq!(Durability::Ordered.options().escalation, escalation());
        assert_eq!(Durability::Relaxed.options().escalation, escalation());
        assert!(Durability::Strict.options().escalation.is_empty());
        let dir = scratch();
        let log = dir.path().join("bus.log");
        let opened = open(&log, None, Durability::Ordered, Some(test_boot(1))).unwrap();
        let served = opened.broker.served_durability();
        let said = describe_served(Durability::Ordered, Some(served), Some(true));
        assert_eq!(served.requested, Tier::Ordered);
        assert_eq!(served.rules, 4, "aterm's four rules");
        if cfg!(target_vendor = "apple") {
            assert_eq!(
                (served.served, served.reason, served.horizon_ms),
                (Tier::Ordered, ServedReason::None, Some(100))
            );
            assert!(said.starts_with("an ack is in order on the disk"), "{said}");
            assert!(
                said.contains("the last 100 ms of records, plus a batch and a full flush"),
                "astream's bound, not a shorter one: {said}"
            );
            assert!(said.contains("its 4 escalation rules name"), "{said}");
            assert!(
                !said.contains(SPARED),
                "a count identifies no table: {said}"
            );
            assert!(!said.contains("loses nothing"), "{said}");
            let (_, lost) = loss(Durability::Ordered, served.horizon_ms, "machine record");
            let promise = Durability::Ordered.promise();
            assert!(
                promise.contains(&lost) && promise.contains(SPARED),
                "{promise} / {lost}"
            );
        } else {
            assert_eq!(
                (served.served, served.reason),
                (Tier::Strict, ServedReason::NotApple)
            );
            assert!(
                said.starts_with("served strict (the ordering barrier exists only on Apple)"),
                "{said}"
            );
            assert!(said.ends_with(STRICT_PROMISE), "{said}");
        }
        let own = describe_own(Durability::Ordered);
        assert!(
            own.starts_with(&Durability::Ordered.promise())
                && own.contains("astream serves it strict where the device cannot keep the order"),
            "`on`'s note: the served promise, and that astream decides where: {own}"
        );
        let older = ServedInfo {
            requested: Tier::Strict,
            served: Tier::Strict,
            reason: ServedReason::None,
            horizon_ms: None,
            rules: 0,
        };
        let said = describe_served(Durability::Ordered, Some(older), Some(true));
        assert!(
            said.starts_with("served strict (its build serves ordered as strict)"),
            "{said}"
        );
        assert!(said.ends_with(STRICT_PROMISE), "{said}");
        let lowered = ServedInfo {
            requested: Tier::Ordered,
            served: Tier::Strict,
            reason: ServedReason::NotBootVolume,
            horizon_ms: Some(1000),
            rules: 4,
        };
        let said = describe_served(Durability::Ordered, Some(lowered), Some(true));
        assert!(said.contains("its log is not on the boot volume"), "{said}");
        let said = describe_served(Durability::Relaxed, Some(older), Some(true));
        assert!(said.starts_with("served strict ("), "{said}");
        assert!(said.ends_with(STRICT_PROMISE), "{said}");
        let relaxed = open(
            &dir.path().join("relaxed.log"),
            None,
            Durability::Relaxed,
            Some(test_boot(1)),
        )
        .unwrap();
        assert_eq!(relaxed.broker.served_durability().rules, 4);
        let said = describe_served(
            Durability::Relaxed,
            Some(relaxed.broker.served_durability()),
            Some(true),
        );
        assert!(said.contains("its 4 escalation rules name"), "{said}");
        assert!(
            !said.contains(SPARED),
            "a count identifies no table: {said}"
        );
        for weak in [Durability::Ordered, Durability::Relaxed] {
            assert!(weak.promise().contains(SPARED), "{weak:?}");
        }
    }

    #[test]
    fn a_weak_tier_is_told_by_what_its_broker_reports() {
        use astream_broker::{ServedInfo, ServedReason, Tier};
        let ordered = ServedInfo {
            requested: Tier::Ordered,
            served: Tier::Ordered,
            reason: ServedReason::None,
            horizon_ms: Some(1000),
            rules: 4,
        };
        let strict = STRICT_PROMISE;
        let said = describe_served(Durability::Ordered, Some(ordered), Some(true));
        assert!(said.starts_with("an ack is in order on the disk"), "{said}");
        assert!(said.contains("the last 1 s"), "{said}");
        assert!(said.contains("its 4 escalation rules name"), "{said}");
        assert!(
            !said.contains(SPARED),
            "four rules identify no table: {said}"
        );
        assert!(
            !said.contains("machine record"),
            "unknown protected classes: {said}"
        );
        assert!(describe_own(Durability::Relaxed).contains(SPARED));
        assert!(!said.contains("unguarded"), "{said}");
        assert!(
            !said.contains(strict) && !said.contains("loses nothing"),
            "{said}"
        );
        let said = describe_served(Durability::Strict, Some(ordered), Some(true));
        assert!(
            said.starts_with("served ordered (its build opened it ordered) — an ack is in order"),
            "{said}"
        );
        assert!(!said.contains("loses nothing"), "{said}");
        let said = describe_served(Durability::Ordered, Some(ordered), Some(false));
        assert!(
            said.ends_with("no principal is bound, so principal-based rules do not match"),
            "{said}"
        );
        let unbounded = ServedInfo {
            horizon_ms: None,
            rules: 1,
            ..ordered
        };
        let said = describe_served(Durability::Ordered, Some(unbounded), None);
        assert!(
            said.contains("every record since the last full flush"),
            "{said}"
        );
        assert!(said.contains("its 1 escalation rule names"), "{said}");
        assert!(
            !said.contains(SPARED),
            "another table spares its own records"
        );
        let hundred = ServedInfo {
            horizon_ms: Some(100),
            ..ordered
        };
        let said = describe_served(Durability::Ordered, Some(hundred), None);
        assert!(said.contains("the last 100 ms of records"), "{said}");
        for tier in [Tier::Ordered, Tier::Relaxed] {
            let bare = ServedInfo {
                requested: tier,
                served: tier,
                horizon_ms: None,
                rules: 0,
                ..ordered
            };
            let asked = Durability::of_tier(tier).unwrap();
            let said = describe_served(asked, Some(bare), Some(false));
            assert!(said.contains("no record is flushed in full"), "{said}");
            assert!(
                !said.contains(SPARED) && !said.contains("machine record"),
                "{said}"
            );
            assert!(!said.contains("unguarded"), "nothing to tell apart: {said}");
        }
        let said = describe_served(Durability::Relaxed, None, None);
        assert!(said.contains("a new identity"), "{said}");
        assert!(!said.contains("resume where the log continues"), "{said}");
        assert_eq!(describe_served(Durability::Strict, None, None), strict);
        assert_eq!(
            Durability::of_tier(Tier::Ordered),
            Some(Durability::Ordered)
        );
        assert_eq!(Durability::of_tier(Tier::Other(9)), None);
    }

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

    #[test]
    fn the_broker_the_lease_and_an_older_builds_mark_name_one_boot() {
        let boot = this_boot();
        if cfg!(any(target_vendor = "apple", target_os = "linux")) {
            assert!(boot.is_some(), "this host's kernel names its boot");
        }
        let Some(boot) = boot else {
            return;
        };
        let kernel_text = kernel_boot_text().expect("the kernel's own text for its boot");

        let dir = scratch();
        let state = crate::state::StateDir::open(dir.path().join("state")).expect("state dir");
        let lease = state.trusted_boot();
        if cfg!(target_vendor = "apple") {
            assert_eq!(lease, Some(boot), "the lease trusts the broker's boot");
        } else {
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
        };
        std::fs::write(mark_path(&log), legacy_mark_of(&kernel_text)).unwrap();
        assert_eq!(read_mark(&log), Mark::Boot(boot), "{kernel_text:?}");
        let upgraded = open(&log, None, Durability::Relaxed, this_boot()).expect("reopen");
        assert_eq!(upgraded.renewed, None, "the mark is this boot's");
        assert_eq!(upgraded.broker.log_identity(), first, "nothing moves");
        assert_eq!(read_mark(&log), Mark::Absent, "the mark is removed");
        drop(upgraded);

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

    #[test]
    fn a_power_loss_under_relaxed_moves_the_generation_with_lineage() {
        let dir = scratch();
        let log = dir.path().join("bus.log");
        let sock = dir.path().join("bus.sock");
        let (before, witnessed, at4) = {
            let opened = open(&log, None, Durability::Relaxed, Some(test_boot(1))).expect("open");
            opened.broker.arm_unclean_stop();
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
        cut(&log, at4);

        let control = dir.path().join("control.log");
        std::fs::copy(&log, &control).unwrap();
        std::fs::copy(
            astream_broker::identity_path(&log),
            astream_broker::identity_path(&control),
        )
        .unwrap();
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

    #[test]
    fn a_restart_or_a_clean_stop_keeps_the_identity() {
        let dir = scratch();
        let log = dir.path().join("bus.log");
        let sock = dir.path().join("bus.sock");
        let first = {
            let opened = open(&log, None, Durability::Relaxed, Some(test_boot(1))).unwrap();
            opened.broker.arm_unclean_stop();
            let mut h = opened.broker.serve(&sock).unwrap();
            publish(&sock, 7, 1, 3);
            h.shutdown();
            opened.broker.log_identity()
        };
        let again = open(&log, None, Durability::Relaxed, Some(test_boot(1))).unwrap();
        assert_eq!(again.broker.log_identity(), first);
        assert_eq!(again.broker.head(), 3);
        drop(again);
        let rebooted = open(&log, None, Durability::Relaxed, Some(test_boot(2))).unwrap();
        assert_eq!(
            rebooted.broker.log_identity(),
            first,
            "a clean stop's reboot moves nothing"
        );
        rebooted.broker.arm_unclean_stop();
        drop(rebooted);
        let strict = open(&log, None, Durability::Strict, Some(test_boot(3))).unwrap();
        let moved = strict.broker.log_identity();
        assert_eq!(
            (moved.id, moved.generation),
            (first.id, first.generation + 1)
        );
        strict.broker.arm_unclean_stop();
        drop(strict);
        let after_strict = open(&log, None, Durability::Strict, Some(test_boot(4))).unwrap();
        assert_eq!(
            after_strict.broker.log_identity(),
            moved,
            "strict acks were durable"
        );
    }

    #[test]
    fn the_legacy_mark_moves_the_generation_once_and_is_removed() {
        let dir = scratch();
        let log = dir.path().join("bus.log");
        let sock = dir.path().join("bus.sock");
        let (b1, b2, b3, b4) = (test_boot(1), test_boot(2), test_boot(3), test_boot(4));
        let first = {
            let opened = open(&log, None, Durability::Relaxed, Some(b1)).unwrap();
            opened.broker.arm_unclean_stop();
            let mut h = opened.broker.serve(&sock).unwrap();
            publish(&sock, 7, 1, 3);
            h.shutdown();
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
        drop(migrated);
        let clean = open(&log, None, Durability::Relaxed, Some(b3)).unwrap();
        assert_eq!(
            clean.broker.log_identity(),
            second,
            "clean stop, reboot: nothing"
        );
        assert_eq!(read_mark(&log), Mark::Absent, "and no mark written");
        clean.broker.arm_unclean_stop();
        drop(clean);
        let again = open(&log, None, Durability::Relaxed, Some(b4)).unwrap();
        let third = again.broker.log_identity();
        assert_eq!(generations(third), (first.id, second.generation + 1));
        drop(again);

        for tier in [Durability::Relaxed, Durability::Strict] {
            std::fs::write(mark_path(&log), legacy_mark_text(&b4)).unwrap();
            let same = open(&log, None, tier, Some(b4)).unwrap();
            assert_eq!(same.renewed, None, "{tier:?}");
            assert_eq!(same.broker.log_identity(), third, "{tier:?}");
            assert_eq!(read_mark(&log), Mark::Absent, "{tier:?}");
        }
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

    #[test]
    fn a_reader_older_than_the_lineage_window_forgets() {
        let dir = scratch();
        let log = dir.path().join("bus.log");
        let sock = dir.path().join("bus.sock");
        let first = {
            let opened = open(&log, None, Durability::Relaxed, Some(test_boot(1))).unwrap();
            opened.broker.arm_unclean_stop();
            let mut h = opened.broker.serve(&sock).unwrap();
            publish(&sock, 7, 1, 3);
            h.shutdown();
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
            opened.broker.arm_unclean_stop();
            let mut h = opened.broker.serve(&sock).unwrap();
            {
                let mut c = astream_broker::Client::connect(&sock).unwrap();
                assert_eq!(c.log_identity().unwrap(), Some(now));
                lineage = c.log_lineage().cloned();
            }
            h.shutdown();
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

    #[test]
    fn what_open_refuses_and_what_it_leaves_alone() {
        let dir = scratch();
        let log = dir.path().join("bus.log");
        let sock = dir.path().join("bus.sock");
        let first = {
            let opened = open(&log, None, Durability::Relaxed, None).expect("runs, unknown boot");
            opened.broker.arm_unclean_stop();
            let mut h = opened.broker.serve(&sock).unwrap();
            publish(&sock, 7, 1, 2);
            h.shutdown();
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
        let sidecar = std::fs::read(astream_broker::identity_path(&log)).unwrap();
        let err = open(&log, None, Durability::Relaxed, Some(test_boot(1)))
            .err()
            .expect("a second broker on one log");
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists, "{err}");
        assert_eq!(
            read_mark(&log),
            Mark::Boot(test_boot(9)),
            "the mark is the holder's"
        );
        assert_eq!(
            std::fs::read(astream_broker::identity_path(&log)).unwrap(),
            sidecar
        );
        drop(held);
    }

    enum Write {
        /// A publish on `subject` under `who`'s caps.
        Publish(Who, String),
        /// A pure commit of `group` under `who`'s caps.
        Commit(Who, String),
    }

    /// Whose caps a [`Write`] goes under.
    #[derive(Clone, Copy, Debug)]
    enum Who {
        /// The person `h-x`, bound by their ring ([`crate::enable::person_grants`]).
        Person,
        /// The node's bridge, bound by its ring ([`crate::enable::node_grants`]).
        Node,
        /// The fleet root's god cap: unbound, so no principal rule sees it.
        God,
    }

    const TABLE_FLEET: &str = "f";
    const TABLE_NODE: &str = "n-00000000000000aa";
    const TABLE_PERSON: &str = "h-x";

    fn table_writes() -> Vec<(&'static str, Write, bool)> {
        use crate::subject;
        let (f, node, p) = (TABLE_FLEET, TABLE_NODE, TABLE_PERSON);
        vec![
            (
                "a person's halt",
                Write::Publish(Who::Person, subject::person_halt(f, p)),
                true,
            ),
            (
                "a person's answer",
                Write::Publish(Who::Person, subject::post_lane(f, node, "s-1", p, "answer")),
                true,
            ),
            (
                "mail onto a person's lane",
                Write::Publish(
                    Who::Node,
                    format!("{}/ask", subject::person_lane(f, p, node)),
                ),
                true,
            ),
            (
                "a person's inbox commit",
                Write::Commit(Who::Person, subject::person_inbox_group(f, p)),
                true,
            ),
            (
                "the god cap's halt",
                Write::Publish(Who::God, subject::person_halt(f, "h-y")),
                true,
            ),
            (
                "an agent's post",
                Write::Publish(Who::Node, subject::post_lane(f, node, "s-2", node, "note")),
                false,
            ),
            (
                "a session's presence row",
                Write::Publish(Who::Node, subject::session_face(f, node, "s-1", "presence")),
                false,
            ),
            (
                "the node's ev digest",
                Write::Publish(Who::Node, subject::node_face(f, node, "ev")),
                false,
            ),
            (
                "a bridge's halt ack",
                Write::Publish(Who::Node, subject::node_face(f, node, "ack")),
                false,
            ),
            (
                "the node's inbox commit",
                Write::Commit(Who::Node, subject::inbox_group(f, node)),
                false,
            ),
        ]
    }

    fn escalated_by(
        tier: Durability,
        table: astream_broker::Escalation,
    ) -> Option<Vec<(&'static str, bool)>> {
        let dir = scratch();
        let log = dir.path().join("bus.log");
        let sock = dir.path().join("bus.sock");
        let secret = b"a-mint-secret-of-thirty-two-bytes".to_vec();
        let mut opts = tier.options();
        if tier == Durability::Ordered {
            opts.durability = astream_broker::Durability::Ordered { horizon: None };
        }
        opts.escalation = table;
        opts.guard = Some(secret.clone());
        opts.boot = astream_broker::Boot::Known(test_boot(1));
        let broker =
            astream_broker::Broker::open_opts(&log, opts, || Ok(astream_broker::Prepared::Nothing))
                .expect("open");
        if broker.served_durability().served == astream_broker::Tier::Strict {
            return None;
        }
        let mut h = broker.serve(&sock).expect("serve");
        let client = |grants: Vec<String>| {
            let mut c = astream_broker::Client::connect(&sock).expect("connect");
            for grant in grants {
                let cap = astream_cap::mint(&secret, &grant).expect("mint");
                c.attach(&cap.filter, &cap.tag).expect("attach");
            }
            c
        };
        let mut person = client(crate::enable::person_grants(TABLE_FLEET, TABLE_PERSON));
        let mut node = client(crate::enable::node_grants(TABLE_FLEET, TABLE_NODE));
        let mut god = client(vec![format!("/f/{TABLE_FLEET}/>")]);
        let mut seq = 0;
        let mut out = Vec::new();
        for (what, write, _) in table_writes() {
            seq += 1;
            let off = match write {
                Write::Publish(who, subject) => {
                    let (c, producer) = match who {
                        Who::Person => (&mut person, astream_cap::producer_id_of(TABLE_PERSON)),
                        Who::Node => (&mut node, astream_cap::producer_id_of(TABLE_NODE)),
                        Who::God => (&mut god, 9_100),
                    };
                    c.publish(producer, seq, &subject, b"v=1 t=1 state=on")
                        .unwrap_or_else(|e| panic!("{what} on {subject}: {e}"))
                        .0
                }
                Write::Commit(who, group) => {
                    let c = match who {
                        Who::Person => &mut person,
                        Who::Node => &mut node,
                        Who::God => &mut god,
                    };
                    c.commit(&group, 0)
                        .unwrap_or_else(|e| panic!("{what} on {group}: {e}"))
                }
            };
            out.push((what, broker.durable_head() > off));
        }
        h.shutdown();
        Some(out)
    }

    #[test]
    fn each_record_a_person_would_miss_is_escalated() {
        let want: Vec<(&str, bool)> = table_writes()
            .into_iter()
            .map(|(what, _, escalated)| (what, escalated))
            .collect();
        let mut arms = 0;
        for tier in [Durability::Relaxed, Durability::Ordered] {
            match escalated_by(tier, escalation()) {
                Some(got) => {
                    assert_eq!(got, want, "{tier:?}");
                    arms += 1;
                }
                None => eprintln!("{tier:?}: served strict here, so the arm proves nothing"),
            }
        }
        assert!(arms >= 1, "the relaxed arm runs everywhere");

        let without_fleet = astream_broker::Escalation::new(
            escalation()
                .rules()
                .iter()
                .filter(|r| **r != astream_broker::Escalate::Subject("/f/*/fleet/>".into()))
                .cloned(),
        );
        assert_eq!(without_fleet.rules().len(), escalation().rules().len() - 1);
        let got = escalated_by(Durability::Relaxed, without_fleet).expect("relaxed is served");
        let of = |what: &str| got.iter().find(|(w, _)| *w == what).map(|(_, e)| *e);
        assert_eq!(of("a person's halt"), Some(true), "their principal rule");
        assert_eq!(
            of("the god cap's halt"),
            Some(false),
            "an unbound halt escapes a table with no fleet subject rule"
        );
    }

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
