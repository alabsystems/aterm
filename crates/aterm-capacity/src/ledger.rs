// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LEDGER: one directory of claims per user. A claim is a file its holder keeps locked
//! ([`RecordLock`]) for as long as it lives, and its record says who holds it and for what:
//! `<start>-<pid>-<seq>.wait` while it waits, `.claim` once admitted. Ordinary v1 claims end
//! with their holder's lock. Cohort namespaces instead retain an armed or bound reservation
//! after every guard closes, until the captured kernel scope proves drain. Unknown or torn
//! custody blocks admission. `ledger.lock` orders each waiter's read and decision.

use std::fs::File;
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use aterm_json::{Map, Value};
use aterm_toolchain::record_lock::RecordLock;

use crate::{Need, Patience};

#[cfg(unix)]
#[path = "cohort_ledger.rs"]
mod cohort_ledger;
#[cfg(all(test, unix))]
pub(crate) use cohort_ledger::tests::Fixture as CohortFixture;
#[cfg(all(test, unix))]
#[path = "cohort_failure_tests.rs"]
mod failure_tests;
#[cfg(test)]
#[path = "legacy_claim_tests.rs"]
mod legacy_claim_tests;

/// Saved identity of the original claim guard. These numbers are observations, not a
/// descriptor, writer, spawn receipt or permission to release a reservation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClaimGuardIdentity {
    /// Device of the original held guard, captured when the claim was admitted.
    pub device: u64,
    /// Inode of that same original guard; a replacement path is not interchangeable.
    pub inode: u64,
}

/// Current factual state of one exact cohort claim generation. A missing working file
/// without its matching durable tombstone remains unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CohortClaimState {
    /// The exact marker and original guard match a durably covered retired tombstone.
    Closed,
    /// A validated claim remains without that published closure fact.
    Retained,
    /// Missing or incomplete evidence proves no closure.
    Unknown,
}

/// Whether holder observations may reduce a record's reservation or establish ancestry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Accounting {
    /// An ordinary live holder's process tree and measured writes may be credited.
    #[default]
    Holder,
    /// Retain the entire budget; a dead or reused holder and closed guards prove no drain.
    Whole,
    /// An immutable cohort-owned reservation, independent of execution lifetime.
    /// It never credits numeric creator ancestry, including before the first ARM.
    CohortOwned,
    /// An ordinary detached memory claim reserves its full peak without numeric holder
    /// credit or ancestry. It ends at the final actual guard close, not a cohort journal.
    Detached,
}

pub(crate) const DETACHED_MEMORY: &[u8] = b"@aterm-detached-memory/1\n";
#[cfg(all(test, unix))]
thread_local! { static DETACH_FAIL: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) }; }
#[cfg(all(test, unix))]
pub(crate) fn fail_detach_after_bytes(bytes: usize) {
    DETACH_FAIL.with(|failure| failure.set(Some(bytes)));
}

/// The most of a claim file any reader takes, and where its lock's range starts on Windows: the record lies before it.
pub const RECORD_MAX: u64 = 16 * 1024;

/// The most bytes of `what`, `checkout` or the volume a record keeps.
const TEXT_MAX: usize = 512;

/// Validate an exact checkout spelling for admission and cross-prefix custody lookup.
/// This performs no path walk; the caller supplies its captured canonical UTF-8 spelling.
///
/// # Errors
/// The spelling is empty, relative, contains a parent traversal, or would be truncated by
/// the bounded record representation.
pub fn checkout_exact(checkout: &str) -> io::Result<&str> {
    if checkout.is_empty()
        || checkout.len() > TEXT_MAX
        || !Path::new(checkout).is_absolute()
        || Path::new(checkout)
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the checkout cannot be represented exactly in the capacity record",
        ));
    }
    Ok(checkout)
}

// A record's three texts at their longest, each character escaped to JSON's six bytes, and its
// figures, admission line and measured writes, still lie before RECORD_MAX: no live claim reads
// back cut short.
const _: () = assert!(3 * 6 * TEXT_MAX + 1024 + WROTE_LINE <= RECORD_MAX as usize);

/// A waiting claim's suffix.
const WAIT: &str = ".wait";
/// An admitted claim's suffix.
const CLAIM: &str = ".claim";
/// The suffix of a claim file being laid: no reader looks at it.
const TAKING: &str = ".taking";
/// The admissions' lock.
const LOCK: &str = "ledger.lock";

/// The length of the line a holder's measured writes take ([`Record::wrote`]): the figure padded to `u64`'s widest.
const WROTE_LINE: usize = "{\"wrote\":}\n".len() + 20;

/// The ledger's directory under the per-user cache: `~/Library/Caches/aterm/capacity` on macOS, `~/.cache/aterm/capacity` on Linux, `%LOCALAPPDATA%\aterm\capacity` on Windows; `None` with no home. On Unix `~` is the account database's home, whatever `$HOME` or `$XDG_CACHE_HOME` says, so a tool run under a scratch home (the lab, the release buildplan, verify's smoke stages) claims in the one ledger every session of this user reads.
#[must_use]
pub fn user_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        crate::build_cpu::capacity_dir().ok()
    }
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .filter(|v| !v.is_empty())
            .map(|d| PathBuf::from(d).join("aterm").join("capacity"))
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

/// An admitted claim's grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Held {
    /// When it was admitted, unix ms.
    pub admitted_ms: u64,
    /// The cores it was granted.
    pub cores: u32,
    /// Whether its memory was admitted ALONE ([`crate::Short::Alone`]): while it is held, no
    /// other memory claim is admitted.
    pub alone: bool,
}

/// One claim's record: who holds it, for what, and whether it is admitted.
#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    /// The accounting law established by the ledger probe, not by a numeric holder pid.
    pub accounting: Accounting,
    /// The holder's pid.
    pub pid: u32,
    /// When the claim was made, unix ms: waiters are served in this order.
    pub start_ms: u64,
    /// The holder's own count of its claims, which orders two made in one millisecond.
    pub seq: u64,
    /// The checkout the holder runs for: its need's ([`Need::checkout`]), else its working directory's enclosing git checkout.
    pub checkout: String,
    /// What it needs; its `path` is the mount of the volume its disk need is on.
    pub need: Need,
    /// Typed timing evidence; absent or malformed legacy metadata is unknown, not readiness.
    pub timing: crate::TimingEvidence,
    /// Its grant, once admitted.
    pub held: Option<Held>,
    /// Processes outside its holder's tree whose use is its own ([`crate::Claim::credit`]): a
    /// VM's, which the system parents to launchd, not to the run that booted it. Their trees'
    /// use is credited against this claim alone, before its holder's.
    pub credit: Vec<u32>,
    /// What its holder has measured itself writing toward its disk need since it was admitted
    /// ([`crate::Claim::wrote`]): the free space a waiter reads has already lost it, so its
    /// disk reservation is the need less this. `None` where its holder measures nothing.
    pub wrote: Option<u64>,
    /// When a quiet claim was first next for the quiet machine, unix ms: short of the load with
    /// no other quiet claim held or waiting ahead of it. Its [`Patience`] counts from here.
    pub next_ms: Option<u64>,
}

impl Record {
    /// Its place in the queue: earlier is served first.
    #[must_use]
    pub const fn order(&self) -> (u64, u32, u64) {
        (self.start_ms, self.pid, self.seq)
    }

    /// The file stem every file of this claim carries.
    fn stem(&self) -> String {
        format!("{:013}-{}-{}", self.start_ms, self.pid, self.seq)
    }

    /// This claim's COVER MARKER ([`crate::COVER_ENV`]): its file stem, `<start>-<pid>-<seq>`,
    /// which names it and its holder.
    #[must_use]
    pub fn cover_marker(&self) -> String {
        self.stem()
    }

    /// The holder's pid a cover marker names ([`Self::cover_marker`]); `None` for any other
    /// word.
    #[must_use]
    pub fn marker_holder(marker: &str) -> Option<u32> {
        let mut parts = marker.split('-');
        let (Some(start), Some(pid), Some(seq), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return None;
        };
        if !start.bytes().all(|b| b.is_ascii_digit())
            || !pid.bytes().all(|b| b.is_ascii_digit())
            || !seq.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        (start.parse::<u64>().is_ok() && seq.parse::<u64>().is_ok())
            .then(|| pid.parse::<u32>().ok().filter(|&p| p > 0))
            .flatten()
    }

    /// The record's first line: who and what.
    fn head_line(&self) -> String {
        let mut o = Map::new();
        o.insert("v".to_owned(), Value::from(1u64));
        o.insert("pid".to_owned(), Value::from(self.pid));
        o.insert("start_ms".to_owned(), Value::from(self.start_ms));
        o.insert("seq".to_owned(), Value::from(self.seq));
        o.insert("checkout".to_owned(), Value::from(cut(&self.checkout)));
        o.insert("what".to_owned(), Value::from(cut(&self.need.what)));
        o.insert("mem".to_owned(), Value::from(self.need.mem));
        o.insert("margin".to_owned(), Value::from(self.need.margin));
        o.insert("cores".to_owned(), Value::from(self.need.cores));
        o.insert(
            "rank".to_owned(),
            Value::from(match self.need.rank {
                crate::Rank::Release => "release",
                crate::Rank::Ordinary => "ordinary",
                crate::Rank::Background => "background",
            }),
        );
        o.insert("disk".to_owned(), Value::from(self.need.disk));
        let quiet = self.need.quiet.map(|q| {
            if q.is_nan() || q < 0.0 {
                0.0
            } else if q.is_infinite() {
                f64::MAX
            } else {
                q
            }
        });
        o.insert("quiet".to_owned(), Value::from(quiet));
        if self.need.quiet_policy != crate::QuietPolicy::Load {
            o.insert(
                "quiet_policy".to_owned(),
                Value::from(self.need.quiet_policy.word()),
            );
        }
        if self.need.alone {
            o.insert("alone".to_owned(), Value::from(true));
        }
        if self.need.covers {
            o.insert("covers".to_owned(), Value::from(true));
        }
        if self.need.scratch {
            o.insert("scratch".to_owned(), Value::from(true));
        }
        if let Some(p) = self.need.patience {
            let mut q = Map::new();
            q.insert("reserve_ms".to_owned(), Value::from(millis(p.reserve)));
            q.insert("start_ms".to_owned(), Value::from(millis(p.start)));
            q.insert("hold_ms".to_owned(), Value::from(millis(p.hold)));
            o.insert("patience".to_owned(), Value::Object(q));
        }
        o.insert(
            "volume".to_owned(),
            Value::from(self.need.path.as_deref().and_then(Path::to_str).map(cut)),
        );
        line_of(o)
    }

    /// The line an admission adds.
    fn held_line(held: Held) -> String {
        if held.alone {
            format!(
                "{{\"admitted_ms\":{},\"alone\":true,\"cores\":{}}}\n",
                held.admitted_ms, held.cores
            )
        } else {
            format!(
                "{{\"admitted_ms\":{},\"cores\":{}}}\n",
                held.admitted_ms, held.cores
            )
        }
    }

    /// The line a quiet waiter adds when it is first next for the quiet machine ([`Self::next_ms`]).
    pub(crate) fn next_line(ms: u64) -> String {
        format!("{{\"next_ms\":{ms}}}\n")
    }

    pub(crate) fn timing_line(evidence: crate::TimingEvidence) -> String {
        let mut value = Map::new();
        value.insert("timing".into(), evidence.value());
        line_of(value)
    }

    /// How much longer, at most, this quiet claim holds back claimed work that asked after it, at
    /// `now` (unix ms): once next for its [`Patience::reserve`], while it waits, and for its
    /// [`Patience::hold`] after it starts, which it does by its [`Patience::start`]. `None` when
    /// it reserves nothing now: no patience, never next, not next long enough, admitted before
    /// it reserved, or its reservation spent.
    #[must_use]
    pub fn reserves(&self, now: u64) -> Option<Duration> {
        let p = self.need.patience?;
        self.need.quiet?;
        if self.need.quiet_policy != crate::QuietPolicy::Load
            && self.held.is_some()
            && let Some(admitted) = self.timing.admitted_ms
            && self.held?.admitted_ms == admitted
        {
            // Only explicit actual admission provenance supplies this stronger bounded hold.
            let end = admitted.saturating_add(millis(p.hold));
            return (now < end).then(|| Duration::from_millis(end - now));
        }
        let next = self.next_ms?;
        let from = next.saturating_add(millis(p.reserve));
        let bound = next.saturating_add(millis(p.start));
        let started = match self.held {
            Some(h) if h.admitted_ms < from => return None,
            Some(h) => h.admitted_ms.min(bound),
            None if now < from => return None,
            None => bound,
        };
        let end = started.saturating_add(millis(p.hold));
        (now < end).then(|| Duration::from_millis(end - now))
    }

    /// The line a credit adds ([`crate::Claim::credit`]): the latest one read whole stands.
    pub(crate) fn credit_line(pids: &[u32]) -> String {
        let mut o = Map::new();
        o.insert(
            "credit".to_owned(),
            Value::Array(pids.iter().map(|p| Value::from(*p)).collect()),
        );
        line_of(o)
    }

    /// The line its holder's measured writes take ([`Self::wrote`]), always [`WROTE_LINE`] bytes, so
    /// a later figure is written over it in place and a reader never finds the record cut short.
    fn wrote_line(bytes: u64) -> String {
        let line = format!("{{\"wrote\":{bytes:<20}}}\n");
        debug_assert_eq!(line.len(), WROTE_LINE);
        line
    }

    /// A record read back from a claim file's text; `held` says the file is admitted (`.claim`).
    #[must_use]
    pub fn decode(text: &str, held: bool) -> Option<Record> {
        let mut lines = text.lines();
        let encoded_head = lines.next()?;
        let poisoned = encoded_head.starts_with('@');
        let restored;
        let head = if poisoned {
            // Old readers reject this non-JSON head. Restore it only in memory after the
            // complete monotone final event exists; an interrupted transfer stays unknown.
            if !held || !text.as_bytes().ends_with(DETACHED_MEMORY) {
                return None;
            }
            restored = format!("{{{}", encoded_head.strip_prefix('@')?);
            restored.as_str()
        } else {
            encoded_head
        };
        let head: Value = aterm_json::from_str(head).ok()?;
        if head.get("v").and_then(Value::as_u64) != Some(1) {
            return None;
        }
        let num = |v: &Value, k: &str| v.get(k).and_then(Value::as_u64);
        let start_ms = num(&head, "start_ms")?;
        let patience = match head.get("patience") {
            None | Some(Value::Null) => None,
            Some(p) => Some(Patience {
                reserve: Duration::from_millis(num(p, "reserve_ms")?),
                start: Duration::from_millis(num(p, "start_ms")?),
                hold: Duration::from_millis(num(p, "hold_ms")?),
            }),
        };
        let quiet = match head.get("quiet") {
            None | Some(Value::Null) => None,
            Some(v) => Some(v.as_f64().filter(|q| q.is_finite() && *q >= 0.0)?),
        };
        let need = Need {
            rank: match head.get("rank") {
                None => crate::Rank::Ordinary,
                Some(value) => match value.as_str() {
                    Some("ordinary") => crate::Rank::Ordinary,
                    Some("release") => crate::Rank::Release,
                    Some("background") => crate::Rank::Background,
                    _ => return None,
                },
            },
            what: head.get("what").and_then(Value::as_str)?.to_owned(),
            mem: num(&head, "mem")?,
            margin: num(&head, "margin").unwrap_or(0),
            cores: u32::try_from(num(&head, "cores")?).ok()?,
            disk: num(&head, "disk")?,
            quiet,
            quiet_policy: match head.get("quiet_policy") {
                None => crate::QuietPolicy::Load,
                Some(v) => match v.as_str() {
                    Some("load") => crate::QuietPolicy::Load,
                    Some("timing") => crate::QuietPolicy::Timing,
                    _ => crate::QuietPolicy::Unknown,
                },
            },
            patience,
            path: head
                .get("volume")
                .and_then(Value::as_str)
                .map(PathBuf::from),
            checkout: None,
            alone: head.get("alone").and_then(Value::as_bool) == Some(true),
            covers: head.get("covers").and_then(Value::as_bool) == Some(true),
            scratch: head.get("scratch").and_then(Value::as_bool) == Some(true),
            stuck_ends: None,
            at_bound: crate::AtBound::Refuse,
        };
        // The lines after the head, in the order they were added: a quiet waiter's `next_ms`,
        // the admission, then the measured writes' (written over in place) and each credit,
        // whichever came first.
        let mut malformed_after = false;
        let mut detached = false;
        let mut bad_detach = false;
        let mut explicit_held: Option<Held> = None;
        let mut next_ms: Option<u64> = None;
        let mut credit: Vec<u32> = Vec::new();
        let mut wrote: Option<u64> = None;
        let mut timing: Option<crate::TimingEvidence> = None;
        for l in lines {
            if l.starts_with('@') {
                if l.as_bytes() == &DETACHED_MEMORY[..DETACHED_MEMORY.len() - 1] && !detached {
                    detached = true;
                } else {
                    bad_detach = true;
                }
                continue;
            }
            let Ok(v) = aterm_json::from_str::<Value>(l) else {
                malformed_after = true;
                continue;
            };
            if explicit_held.is_none()
                && let Some(admitted_ms) = num(&v, "admitted_ms")
                && let Some(cores) = num(&v, "cores").and_then(|c| u32::try_from(c).ok())
            {
                explicit_held = Some(Held {
                    admitted_ms,
                    cores,
                    alone: v.get("alone").and_then(Value::as_bool) == Some(true),
                });
            }
            if next_ms.is_none() {
                next_ms = num(&v, "next_ms");
            }
            if let Some(arr) = v.get("credit").and_then(Value::as_array)
                && let Some(parsed) = arr
                    .iter()
                    .map(|p| p.as_u64().and_then(|p| u32::try_from(p).ok()))
                    .collect::<Option<Vec<u32>>>()
            {
                credit = parsed;
            }
            if wrote.is_none() {
                wrote = num(&v, "wrote");
            }
            if let Some(t) = v.get("timing") {
                timing = Some(crate::TimingEvidence::read(t));
            }
        }
        if bad_detach || detached != poisoned || (detached && (!held || !memory_only(&need))) {
            return None;
        }
        if detached && (explicit_held.is_none() || malformed_after) {
            return None;
        }
        let held = held.then(|| {
            explicit_held.unwrap_or(Held {
                admitted_ms: start_ms,
                cores: need.cores,
                alone: false,
            })
        });
        let wrote = held.and(wrote);
        // Keep the live reservation even when the newest proof is malformed; never retain an
        // earlier ready proof through a later corrupt line.
        let mut timing = if malformed_after {
            crate::TimingEvidence::default()
        } else {
            timing.unwrap_or_default()
        };
        if need.quiet_policy != crate::QuietPolicy::Load {
            // The complete admission line is an independently known resource fact. A later
            // torn CPU observation cannot erase its bounded hold. Never use start_ms's legacy
            // fallback, and never repair a mismatched CPU proof into qualifying evidence.
            let actual = held.and(explicit_held).map(|h| h.admitted_ms);
            if timing.admitted_ms != actual {
                timing = crate::TimingEvidence::default();
            }
            timing.admitted_ms = actual;
        }
        Some(Record {
            accounting: if detached {
                Accounting::Detached
            } else {
                Accounting::Holder
            },
            pid: u32::try_from(num(&head, "pid")?).ok()?,
            start_ms,
            seq: num(&head, "seq")?,
            checkout: head
                .get("checkout")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            need,
            timing,
            held,
            credit,
            wrote,
            next_ms,
        })
    }
}

pub(crate) fn memory_only(need: &Need) -> bool {
    need.mem > 0
        && need.cores == 0
        && need.disk == 0
        && need.quiet.is_none()
        && need.quiet_policy == crate::QuietPolicy::Load
        && !need.covers
}

/// An optional owner is needed only for consuming transfer out of Laid's Drop. Normal
/// callers still dereference the same RecordLock; None is never observable by a live Laid.
#[derive(Debug)]
struct ClaimLock(Option<RecordLock>);
impl From<RecordLock> for ClaimLock {
    fn from(lock: RecordLock) -> Self {
        Self(Some(lock))
    }
}
impl std::ops::Deref for ClaimLock {
    type Target = RecordLock;
    fn deref(&self) -> &RecordLock {
        self.0.as_ref().expect("live claim lock")
    }
}
impl std::ops::DerefMut for ClaimLock {
    fn deref_mut(&mut self) -> &mut RecordLock {
        self.0.as_mut().expect("live claim lock")
    }
}

/// `d` in whole milliseconds, as a record keeps it.
pub(crate) fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// `text` cut on a character boundary short of [`TEXT_MAX`] bytes.
fn cut(text: &str) -> &str {
    let mut end = text.len().min(TEXT_MAX);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// One JSON object and its newline.
fn line_of(o: Map) -> String {
    let mut line = aterm_json::to_string(&Value::Object(o)).unwrap_or_else(|_| "{}".to_owned());
    line.push('\n');
    line
}

/// The next claim of this process: two claims of one process never share a stem.
fn next_seq() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// A locked claim file. Dropping a cohort claim preserves its file while work is live or unknown.
#[derive(Debug)]
pub(crate) struct Laid {
    lock: ClaimLock,
    path: PathBuf,
    /// The grant its file has written ([`Ledger::admit`]): a rename that failed after it is tried
    /// again under that grant, without writing another.
    granted: Option<Held>,
    #[cfg(unix)]
    custody: Option<cohort_ledger::Custody>,
    #[cfg(unix)]
    detaching: Option<Vec<u8>>,
}

impl Laid {
    /// The claim file.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Add `line` to the record, refused where it would reach into the room kept for the
    /// measured writes' line ([`Self::wrote`]) before [`RECORD_MAX`], which no reader passes.
    pub(crate) fn append(&mut self, line: &str) -> io::Result<()> {
        #[cfg(unix)]
        if self.detaching.is_some() {
            return Err(io::Error::other("memory detachment intent is pending"));
        }
        #[cfg(unix)]
        if self.custody.is_some() {
            return self.append_cohort(line);
        }
        let end = self.lock.seek(SeekFrom::End(0))?;
        let room = RECORD_MAX - WROTE_LINE as u64;
        if end.saturating_add(u64::try_from(line.len()).unwrap_or(u64::MAX)) > room {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                format!("the record would pass {room} bytes"),
            ));
        }
        self.lock.write_all(line.as_bytes())
    }

    /// Write `bytes` as the admitted claim's measured writes ([`Record::wrote`]): the line appended
    /// the first time, at the offset kept in `at`, and written over in place after, at one length.
    pub(crate) fn wrote(&mut self, at: &mut Option<u64>, bytes: u64) -> io::Result<()> {
        #[cfg(unix)]
        if self.detaching.is_some() {
            return Err(io::Error::other("memory detachment intent is pending"));
        }
        #[cfg(unix)]
        if self.custody.is_some() {
            return self.wrote_cohort(at, bytes);
        }
        let offset = match *at {
            Some(offset) => offset,
            None => self.lock.seek(SeekFrom::End(0))?,
        };
        if offset.saturating_add(WROTE_LINE as u64) > RECORD_MAX {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                format!("the record would pass {RECORD_MAX} bytes"),
            ));
        }
        self.lock.seek(SeekFrom::Start(offset))?;
        self.lock.write_all(Record::wrote_line(bytes).as_bytes())?;
        *at = Some(offset);
        Ok(())
    }
}

#[cfg(unix)]
impl Laid {
    fn check_named_memory(&self) -> io::Result<()> {
        use std::os::unix::fs::MetadataExt as _;
        let original = self.lock.metadata()?;
        let named = std::fs::symlink_metadata(&self.path)?;
        if !named.is_file() || (named.dev(), named.ino()) != (original.dev(), original.ino()) {
            return Err(io::Error::other(
                "the original memory claim path was replaced",
            ));
        }
        Ok(())
    }

    pub(crate) fn detach_memory(&mut self) -> io::Result<()> {
        use std::os::unix::fs::FileExt as _;
        if self.custody.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "cohort claims cannot detach ordinary memory custody",
            ));
        }
        self.check_named_memory()?;
        let ledger = Ledger::at(
            self.path
                .parent()
                .ok_or_else(|| io::Error::other("memory claim has no ledger"))?,
        );
        if ledger.cohort_aware()? {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "detachment cannot replace cohort accounting",
            ));
        }
        // Legacy readers hold this original turn through scan/decide/grant. Once this turn
        // is ours, none can carry a pre-poison Holder observation past the durable handoff.
        let _turn = ledger.try_turn()?.map_err(|_| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "the original ledger admission turn is held",
            )
        })?;
        self.check_named_memory()?;
        if self.detaching.is_none() {
            let offset = self.lock.metadata()?.len();
            if offset.saturating_add(DETACHED_MEMORY.len() as u64) > RECORD_MAX {
                return Err(io::Error::new(
                    io::ErrorKind::FileTooLarge,
                    "detached memory marker exceeds the original record bound",
                ));
            }
            let mut original = vec![0; offset as usize];
            self.lock.read_exact_at(&mut original, 0)?;
            if original.first() != Some(&b'{') {
                return Err(io::Error::other(
                    "the original memory claim is not a plain JSON header",
                ));
            }
            // Record exact original bytes BEFORE any intent byte can reach disk.
            self.detaching = Some(original);
        }
        let original = self.detaching.as_ref().expect("detachment intent");
        let offset = original.len() as u64;
        let mut base = vec![0; original.len()];
        self.lock.read_exact_at(&mut base, 0)?;
        if &base != original && !(base.first() == Some(&b'@') && base.get(1..) == original.get(1..))
        {
            return Err(io::Error::other(
                "detachment intent's original record changed",
            ));
        }
        let length = self.lock.metadata()?.len();
        let tail_len = length
            .checked_sub(offset)
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n <= DETACHED_MEMORY.len())
            .ok_or_else(|| io::Error::other("detachment intent has an unknown tail"))?;
        let mut tail = vec![0; tail_len];
        self.lock.read_exact_at(&mut tail, offset)?;
        if !DETACHED_MEMORY.starts_with(&tail) {
            return Err(io::Error::other("detachment intent has foreign bytes"));
        }
        if &base == original {
            if tail_len != 0 {
                return Err(io::Error::other(
                    "an unpoisoned memory record cannot have a detached tail",
                ));
            }
            // A single-byte irreversible poison makes every old JSON-head reader fail
            // closed before any tail event can become visible. Never write '{' again.
            self.lock.write_all_at(b"@", 0)?;
        }
        // Order the old-reader fence before appending intent, including on a retry after a
        // failed poison flush. No descriptor is exported until the final event also flushes.
        aterm_durable::Flush::Durable.file(&self.lock)?;
        // Never remove an existing intent byte: a concurrent reader or owner crash must
        // not expose Holder again. Complete only this exact original owner's missing suffix.
        let remaining = &DETACHED_MEMORY[tail_len..];
        self.lock.seek(SeekFrom::Start(length))?;
        #[cfg(test)]
        if let Some(bytes) = DETACH_FAIL.with(|failure| failure.take()) {
            self.lock
                .write_all(&remaining[..bytes.min(remaining.len())])?;
            return Err(io::Error::other(
                "injected original memory intent write/flush failure",
            ));
        }
        self.lock.write_all(remaining)?;
        aterm_durable::Flush::Durable.file(&self.lock)?;
        self.check_named_memory()?;
        Ok(())
    }

    pub(crate) fn into_close_only(mut self) -> aterm_toolchain::record_lock::CloseOnlyLock {
        self.lock
            .0
            .take()
            .expect("live claim lock")
            .into_close_only()
    }

    pub(crate) fn detach_pending(&self) -> bool {
        self.detaching.is_some()
    }
    /// A close-on-exec copy of the claim file's descriptor: the same open file description, so
    /// the same lock, which the kernel lets go only once every descriptor of it is closed (or one
    /// of them lets it go).
    pub(crate) fn descriptor(&self) -> io::Result<std::os::fd::OwnedFd> {
        use std::os::fd::AsFd as _;
        self.lock.as_fd().try_clone_to_owned()
    }

    /// HAND THE CLAIM OFF: this process's copy is forgotten, neither letting the lock go nor
    /// removing the file, so a copy of its descriptor held elsewhere ([`Self::descriptor`],
    /// another process's inherited one) keeps the claim. The forgotten descriptor is
    /// close-on-exec: it closes at this process's exec or exit, and closing it lets nothing go
    /// while that copy is open.
    pub(crate) fn hand_off(self) {
        std::mem::forget(self);
    }

    /// ADOPT the claim file at `path` whose open file description `fd` is a copy of (an
    /// inherited descriptor of [`Self::hand_off`]'s claim), proved three ways before its record
    /// is read, none of which takes or lets go of the claim's lock until the last: `fd` opens the
    /// file at `path` (its device and inode), never one a link there names; taking the lock
    /// through a fresh open of `path`, which follows no link and opens only a regular file, is
    /// refused, so some description holds it; and taking it through `fd` succeeds, so that
    /// description is `fd`'s. Its record, and the claim as this process's to let go. A proof that
    /// fails lets nothing go, and so does a record that cannot be read once all three held (the
    /// claim is handed off, as [`Self::hand_off`] does).
    ///
    /// # Errors
    /// A proof that fails (`InvalidData` naming it), the fresh open's error, or the file could not
    /// be read or its record decoded.
    pub(crate) fn adopt(
        fd: std::os::fd::OwnedFd,
        path: &Path,
        writer: Option<std::os::fd::OwnedFd>,
    ) -> io::Result<(Record, Laid)> {
        use std::os::unix::fs::MetadataExt as _;
        let refused = |why: &str| io::Error::new(io::ErrorKind::InvalidData, why.to_owned());
        let file = File::from(fd);
        let ledger = Ledger::at(
            path.parent()
                .ok_or_else(|| refused("claim has no ledger"))?,
        );
        let cohort = ledger.cohort_aware()?;
        if cohort && writer.is_none() {
            return Err(refused(
                "a read-only cohort guard cannot be promoted to a writer; transfer controller authority separately",
            ));
        }
        if !cohort && writer.is_some() {
            return Err(refused(
                "legacy claims cannot receive a cohort controller transfer",
            ));
        }
        let held = file.metadata()?;
        let named = std::fs::symlink_metadata(path)?;
        if (held.dev(), held.ino()) != (named.dev(), named.ino()) {
            return Err(refused(
                "the descriptor opens another file than the claim's path",
            ));
        }
        match RecordLock::try_lock(crate::open_regular_reader(path)?, RECORD_MAX) {
            Err(std::fs::TryLockError::WouldBlock) => {}
            Ok(stray) => {
                drop(stray);
                return Err(refused("no description holds the claim's lock"));
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(e),
        }
        let lock = match RecordLock::try_lock(file, RECORD_MAX) {
            Ok(lock) => lock,
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(refused("another description holds the claim's lock"));
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(e),
        };
        let laid = Laid {
            lock: lock.into(),
            path: path.to_path_buf(),
            granted: None,
            custody: None,
            detaching: None,
        };
        let mut text = String::new();
        let stamp = |m: &std::fs::Metadata| {
            (
                m.dev(),
                m.ino(),
                m.len(),
                m.ctime(),
                m.ctime_nsec(),
                m.mtime(),
                m.mtime_nsec(),
            )
        };
        let read = (|| {
            use std::os::unix::fs::FileExt as _;
            let before = laid.lock.metadata()?;
            let cap = usize::try_from(before.len().min(RECORD_MAX))
                .unwrap_or(RECORD_MAX as usize)
                .saturating_add(1);
            let mut bytes = vec![0u8; cap];
            let mut length = 0;
            while length < bytes.len() {
                let read = laid.lock.read_at(&mut bytes[length..], length as u64)?;
                if read == 0 {
                    break;
                }
                length += read;
            }
            if length as u64 > RECORD_MAX {
                return Err(refused("claim record is too large"));
            }
            if length as u64 != before.len()
                || stamp(&before) != stamp(&laid.lock.metadata()?)
                || stamp(&before) != stamp(&std::fs::symlink_metadata(path)?)
            {
                return Err(refused(
                    "claim origin changed while reading its held descriptor",
                ));
            }
            bytes.truncate(length);
            text = String::from_utf8(bytes).map_err(|_| refused("claim record is not UTF-8"))?;
            Ok(())
        })();
        let held = path.to_str().is_some_and(|p| p.ends_with(CLAIM));
        let decoded = read.and_then(|_| {
            let ledger = Ledger::at(
                path.parent()
                    .ok_or_else(|| refused("claim has no ledger"))?,
            );
            ledger.check_mode()?;
            match ledger.mode {
                Mode::Cohort(namespace) => {
                    let (record, journal) =
                        crate::cohort_journal::Journal::decode(&text, held, namespace.nonce())?;
                    let writer = File::from(
                        writer.ok_or_else(|| refused("missing private cohort writer transfer"))?,
                    );
                    let custody = cohort_ledger::Custody::adopt(
                        namespace, journal, &text, &laid.lock, writer, path,
                    )?;
                    Ok((record, Some(custody)))
                }
                Mode::Legacy => Record::decode(&text, held)
                    .map(|r| (r, None))
                    .ok_or_else(|| refused("the claim's record does not decode")),
                Mode::Refused(why) => Err(refused(&why)),
            }
        });
        match decoded {
            Ok((record, custody)) => {
                if record.accounting == Accounting::Detached {
                    drop(laid.into_close_only());
                    return Err(refused(
                        "detached memory requires close-only adoption, not an ordinary claim writer",
                    ));
                }
                let mut laid = laid;
                laid.granted = record.held;
                laid.custody = custody;
                if let Err(error) = laid.check_custody() {
                    laid.hand_off();
                    return Err(error);
                }
                Ok((record, laid))
            }
            Err(e) => {
                laid.hand_off();
                Err(e)
            }
        }
    }
}

impl Drop for Laid {
    fn drop(&mut self) {
        if self.lock.0.is_none() {
            return;
        }
        #[cfg(unix)]
        if self.detaching.is_some() {
            // Unknown intent cannot run the ordinary unlink/unlock destructor. A copied
            // actual guard keeps the original record held; final close still ends this
            // ordinary lifetime. Never apply this policy to cohort writer custody.
            drop(
                self.lock
                    .0
                    .take()
                    .expect("live claim lock")
                    .into_close_only(),
            );
            return;
        }
        #[cfg(unix)]
        if self.custody.is_some() {
            self.drop_cohort();
            self.lock.unlock();
            return;
        }
        let _ = std::fs::remove_file(&self.path);
        self.lock.unlock();
    }
}

/// The admissions' lock, held while one waiter reads the ledger and decides; its holder's pid lies before the locked range, for a waiter kept out to name.
#[derive(Debug)]
pub struct Turn {
    _lock: RecordLock,
}

/// The claims of one user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ledger {
    dir: PathBuf,
    mode: Mode,
    #[cfg(unix)]
    user_source: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Mode {
    Legacy,
    #[cfg(unix)]
    Cohort(crate::cohort_namespace::Namespace),
    #[cfg(unix)]
    Refused(String),
}

impl Ledger {
    /// The ledger in `dir`.
    #[must_use]
    pub fn at(dir: impl Into<PathBuf>) -> Ledger {
        let dir = dir.into();
        #[cfg(unix)]
        match crate::cohort_namespace::Namespace::resolve(&dir) {
            Ok(Some(namespace)) => {
                return Ledger {
                    dir: namespace.backend(),
                    mode: Mode::Cohort(namespace),
                    user_source: None,
                };
            }
            Err(error) => {
                return Ledger {
                    dir,
                    mode: Mode::Refused(error.to_string()),
                    user_source: None,
                };
            }
            Ok(None) => {}
        }
        Ledger {
            dir,
            mode: Mode::Legacy,
            #[cfg(unix)]
            user_source: None,
        }
    }

    fn check_mode(&self) -> io::Result<()> {
        #[cfg(unix)]
        if let Some(source) = &self.user_source {
            match (crate::user_namespace::resolve(source)?, &self.mode) {
                (Some(actual), Mode::Cohort(expected)) if &actual == expected => {}
                (None, Mode::Legacy) => {}
                _ => {
                    return Err(io::Error::other(
                        "account memory namespace changed; reopen admission",
                    ));
                }
            }
        }
        #[cfg(unix)]
        match &self.mode {
            Mode::Cohort(namespace) => return namespace.validate(),
            Mode::Refused(why) => {
                return Err(io::Error::new(io::ErrorKind::InvalidData, why.clone()));
            }
            Mode::Legacy => {
                if crate::cohort_namespace::Namespace::resolve(&self.dir)?.is_some() {
                    return Err(io::Error::other(
                        "ledger namespace changed; reopen the cohort ledger",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Whether this actual ledger currently validates its physical cohort namespace.
    /// Pass the ledger itself to admission; a saved `true` is no transferable authority.
    ///
    /// # Errors
    /// The declared namespace is partial, replaced, foreign or has unaccounted predecessors.
    pub fn cohort_aware(&self) -> io::Result<bool> {
        self.check_mode()?;
        #[cfg(unix)]
        {
            Ok(matches!(self.mode, Mode::Cohort(_)))
        }
        #[cfg(not(unix))]
        {
            Ok(false)
        }
    }

    /// Claims for an exactly represented captured checkout in this actual cohort ledger.
    /// Unknown records and incomplete namespaces remain errors, never a different checkout.
    ///
    /// # Errors
    /// The ledger is not cohort-aware, the spelling cannot be represented exactly, or any
    /// namespace, authority or journal read fails. This method performs no checkout path IO.
    pub fn for_checkout(&self, checkout: &str) -> io::Result<Vec<Record>> {
        let checkout = checkout_exact(checkout)?;
        if !self.cohort_aware()? {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "exact checkout custody requires a validated cohort ledger",
            ));
        }
        Ok(self
            .list()?
            .into_iter()
            .filter(|record| record.checkout == checkout)
            .collect())
    }

    /// Observe closure only through the current namespace's durable tombstone matched to
    /// this exact marker and original guard inode. This never repairs, reaps or grants a
    /// writer or birth capability; absence alone is unknown.
    /// # Errors
    /// The namespace, metadata durability, physical origin or bounded read cannot validate.
    pub fn cohort_claim_state(
        &self,
        reference: &crate::CohortClosureRef,
    ) -> io::Result<CohortClaimState> {
        self.check_mode()?;
        #[cfg(unix)]
        if let Mode::Cohort(namespace) = &self.mode {
            return cohort_ledger::claim_state(namespace, reference, false);
        }
        let _ = reference;
        Ok(CohortClaimState::Unknown)
    }

    /// Recover only an exact sealed pending generation, then reap only under actual kernel
    /// scope drain and a free guard. This cannot truncate an unknown tail, refresh an origin,
    /// adopt a writer or manufacture a BirthAttempt, Spawned or NoChild receipt.
    /// # Errors
    /// The exact namespace, marker, original guard, pending bytes/origin, flush or read fails.
    pub fn reap_cohort_claim(
        &self,
        reference: &crate::CohortClosureRef,
    ) -> io::Result<CohortClaimState> {
        self.check_mode()?;
        #[cfg(unix)]
        if let Mode::Cohort(namespace) = &self.mode {
            return cohort_ledger::claim_state(namespace, reference, true);
        }
        let _ = reference;
        Ok(CohortClaimState::Unknown)
    }

    /// TEST SEAM: whether the cohort claim `reference` names is still active (listed, not yet
    /// retired), looked at without its record's mutation lock, so another crate's test can wait
    /// for the claim's owner or reaper to retire it without making that drop meet a busy record.
    ///
    /// # Errors
    /// The namespace or its active directory cannot be read; a legacy ledger.
    #[cfg(any(test, debug_assertions))]
    pub fn cohort_claim_listed(&self, reference: &crate::CohortClosureRef) -> io::Result<bool> {
        self.check_mode()?;
        #[cfg(unix)]
        if let Mode::Cohort(namespace) = &self.mode {
            return cohort_ledger::claim_listed(namespace, reference);
        }
        let _ = reference;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "a legacy ledger has no cohort claims",
        ))
    }

    /// This user's ledger ([`user_dir`]).
    #[must_use]
    pub fn user() -> Option<Ledger> {
        user_dir().map(Self::account_at)
    }

    pub(crate) fn account_at(source: PathBuf) -> Self {
        #[cfg(unix)]
        {
            let mut ledger = match crate::user_namespace::resolve(&source) {
                Ok(Some(namespace)) => Self::at(namespace.fence()),
                Ok(None) => Self::at(&source),
                Err(error) => Self {
                    dir: source.clone(),
                    mode: Mode::Refused(error.to_string()),
                    user_source: None,
                },
            };
            ledger.user_source = Some(source);
            ledger
        }
        #[cfg(not(unix))]
        Self::at(source)
    }

    /// Its directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Every held or retained claim, oldest first, without removing drained files: what
    /// `aterm pkg capacity` prints. Cohort reservations can outlive every guard descriptor, and a
    /// drained cohort claim stays listed, reserving its whole budget, until a reaping scan's
    /// durable tombstone retires it.
    ///
    /// # Errors
    /// The directory could not be listed (never `NotFound`, which is no claims), or a claim file could not be tested.
    pub fn list(&self) -> io::Result<Vec<Record>> {
        self.scan(false)
    }

    /// Read every funded original cohort scope through its validated immutable
    /// checkpoint and complete custody journal. Unarmed, pending and unknown
    /// execution scope never becomes evidence of absence. This does not reap,
    /// adopt, grant or release custody. Legacy ledgers are unsupported.
    /// # Errors
    /// The account selector, namespace, checkpoint or bounded journal fails.
    #[cfg(unix)]
    pub fn funded_scopes(
        &self,
        deadline: std::time::Instant,
    ) -> io::Result<Vec<crate::FundedScope>> {
        self.check_mode()?;
        if let Mode::Cohort(namespace) = &self.mode {
            let scopes = cohort_ledger::funded_scopes(namespace, deadline)?;
            self.check_mode()?;
            return Ok(scopes);
        }
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "funded scopes require a cohort ledger",
        ))
    }

    /// Every live claim of the process `pid`, oldest first, read as [`Self::list`] reads them: only
    /// the files whose name carries that pid are tested, the stem being `<start>-<pid>-<seq>`.
    ///
    /// # Errors
    /// As [`Self::list`].
    pub fn of_pid(&self, pid: u32) -> io::Result<Vec<Record>> {
        let mark = format!("-{pid}-");
        let mut live = self.scan_where(false, &|name| name.contains(&mark))?;
        live.retain(|r| r.pid == pid);
        Ok(live)
    }

    /// Every held or retained claim, oldest first; reap only when its protocol proves drain.
    pub(crate) fn scan(&self, reap: bool) -> io::Result<Vec<Record>> {
        self.scan_where(reap, &|_| true)
    }

    /// [`Self::scan`] over the claim files whose name `named` takes.
    fn scan_where(&self, reap: bool, named: &dyn Fn(&str) -> bool) -> io::Result<Vec<Record>> {
        self.check_mode()?;
        #[cfg(unix)]
        if let Mode::Cohort(namespace) = &self.mode {
            return cohort_ledger::scan(namespace, reap, named);
        }
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut live = Vec::new();
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let held = if name.ends_with(CLAIM) {
                true
            } else if name.ends_with(WAIT) {
                false
            } else {
                continue;
            };
            if !named(name) {
                continue;
            }
            if let Some(record) = probe(&entry.path(), held, reap)? {
                live.push(record);
            }
        }
        live.sort_unstable_by_key(Record::order);
        Ok(live)
    }

    /// TEST SEAM: [`Self::try_turn`] for another crate's test, which holds the admissions' lock
    /// as an admission deciding holds it, so its looks meet that lock until the [`Turn`] drops.
    ///
    /// # Errors
    /// As [`Self::try_turn`].
    #[cfg(any(test, debug_assertions))]
    pub fn hold_turn(&self) -> io::Result<Result<Turn, Option<u32>>> {
        self.try_turn()
    }

    /// Take the admissions' lock if nobody holds it, writing this process's pid into it; `Err(holder)` while another waiter holds it (its pid, where it wrote one).
    pub(crate) fn try_turn(&self) -> io::Result<Result<Turn, Option<u32>>> {
        self.check_mode()?;
        #[cfg(unix)]
        if let Mode::Cohort(namespace) = &self.mode {
            return cohort_ledger::turn(namespace);
        }
        ensure_dir(&self.dir)?;
        let path = self.dir.join(LOCK);
        let file = open_rendezvous(&path)?;
        match RecordLock::try_lock(file, RECORD_MAX) {
            Ok(mut lock) => {
                let _ = lock
                    .set_len(0)
                    .and_then(|()| lock.seek(SeekFrom::Start(0)))
                    .and_then(|_| writeln!(lock, "{}", std::process::id()));
                Ok(Ok(Turn { _lock: lock }))
            }
            Err(std::fs::TryLockError::WouldBlock) => Ok(Err(lock_holder(&path))),
            Err(std::fs::TryLockError::Error(e)) => Err(e),
        }
    }

    /// A claim for `need` in the queue: the record laid under a name no reader looks at, locked, then named a waiter.
    pub(crate) fn lay(&self, need: &Need, start_ms: u64) -> io::Result<(Record, Laid)> {
        self.lay_accounted(need, start_ms, Accounting::Holder)
    }

    pub(crate) fn lay_accounted(
        &self,
        need: &Need,
        start_ms: u64,
        accounting: Accounting,
    ) -> io::Result<(Record, Laid)> {
        self.check_mode()?;
        if accounting == Accounting::CohortOwned && !self.cohort_aware()? {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "cohort-owned admission requires a private cohort ledger",
            ));
        }
        let mut need = need.clone();
        let record = Record {
            accounting,
            pid: std::process::id(),
            start_ms,
            seq: next_seq(),
            checkout: need.checkout.take().unwrap_or_else(checkout_of_cwd),
            need,
            timing: crate::TimingEvidence::default(),
            held: None,
            credit: Vec::new(),
            wrote: None,
            next_ms: None,
        };
        #[cfg(unix)]
        if let Mode::Cohort(namespace) = &self.mode {
            return cohort_ledger::lay(namespace, record);
        }
        ensure_dir(&self.dir)?;
        let stem = record.stem();
        let taking = self.dir.join(format!("{stem}{TAKING}"));
        let path = self.dir.join(format!("{stem}{WAIT}"));
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
        }
        let file: File = aterm_uds::kept_out(|| options.open(&taking))?;
        let lock = match RecordLock::try_lock(file, RECORD_MAX) {
            Ok(lock) => lock,
            Err(e) => {
                let _ = std::fs::remove_file(&taking);
                return Err(match e {
                    std::fs::TryLockError::Error(e) => e,
                    std::fs::TryLockError::WouldBlock => io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "a claim file this process just created was already locked",
                    ),
                });
            }
        };
        let mut laid = Laid {
            lock: lock.into(),
            path: taking,
            granted: None,
            #[cfg(unix)]
            custody: None,
            #[cfg(unix)]
            detaching: None,
        };
        laid.lock.write_all(record.head_line().as_bytes())?;
        std::fs::rename(&laid.path, &path)?;
        laid.path = path;
        Ok((record, laid))
    }

    /// Mark `laid` admitted with `held`: its grant written, then the file named a claim. The grant
    /// is written once: where the rename failed (the file removed meanwhile), the next call tries
    /// it again under the grant the file has, whatever `held` it is given.
    pub(crate) fn admit(&self, laid: &mut Laid, record: &mut Record, held: Held) -> io::Result<()> {
        self.check_mode()?;
        #[cfg(unix)]
        if laid.custody.is_some() {
            return laid.admit_cohort(record, held);
        }
        let held = match laid.granted {
            Some(first) => first,
            None => {
                laid.lock.write_all(Record::held_line(held).as_bytes())?;
                laid.granted = Some(held);
                held
            }
        };
        let claim = self.dir.join(format!("{}{CLAIM}", record.stem()));
        std::fs::rename(&laid.path, &claim)?;
        laid.path = claim;
        record.held = Some(held);
        Ok(())
    }
}

/// Whether `e` means a ledger directory entry is gone or is not a regular claim file (a stray
/// link, directory or pipe), which [`probe`] skips without aborting the scan.
fn ignored_claim_entry(e: &io::Error) -> bool {
    if matches!(
        e.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::InvalidInput | io::ErrorKind::IsADirectory
    ) {
        return true;
    }
    #[cfg(unix)]
    if e.raw_os_error() == Some(libc::ELOOP) {
        return true;
    }
    false
}

/// The pid the admissions' lock at `path` names: its holder's, unless it was let go meanwhile.
fn lock_holder(path: &Path) -> Option<u32> {
    let mut file = crate::open_regular_reader(path).ok()?;
    let mut bytes = [0u8; 64];
    let mut len = 0usize;
    while len < bytes.len() {
        match file.read(&mut bytes[len..]) {
            Ok(0) => break,
            Ok(n) => len += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    let text = std::str::from_utf8(&bytes[..len]).ok()?.trim();
    (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse::<u32>().ok().filter(|&p| p > 0))
        .flatten()
}

/// Test one claim file: `Some(record)` while its holder lives, `None` for a dead holder's
/// (removed when `reap`) or one gone before opening. Unknown live metadata is an error.
fn probe(path: &Path, held: bool, reap: bool) -> io::Result<Option<Record>> {
    let file = match crate::open_regular_reader(path) {
        Ok(f) => f,
        Err(e) if ignored_claim_entry(&e) => return Ok(None),
        Err(e) => return Err(e),
    };
    probe_opened(file, path, held, reap)
}

fn probe_opened(file: File, path: &Path, held: bool, reap: bool) -> io::Result<Option<Record>> {
    // Keep the original open description for reading: reopening the path after
    // testing its lock could read a replacement file with a different budget.
    match RecordLock::try_lock(file.try_clone()?, RECORD_MAX) {
        Ok(lock) => {
            if reap {
                let _ = std::fs::remove_file(path);
            }
            drop(lock);
            Ok(None)
        }
        Err(std::fs::TryLockError::WouldBlock) => {
            let invalid = |why: &str| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("capacity claim {}: {why}", path.display()),
                )
            };
            let len = file.metadata()?.len();
            if len > RECORD_MAX {
                return Err(invalid("locked capacity record exceeds its size bound"));
            }
            let mut bytes = Vec::with_capacity(usize::try_from(len.saturating_add(1)).unwrap_or(0));
            (&file).take(RECORD_MAX + 1).read_to_end(&mut bytes)?;
            if bytes.len() as u64 > RECORD_MAX {
                return Err(invalid("locked capacity record grew past its size bound"));
            }
            let text = std::str::from_utf8(&bytes)
                .map_err(|_| invalid("locked capacity record is not UTF-8"))?;
            Record::decode(text, held)
                .map(Some)
                .ok_or_else(|| invalid("locked capacity record has an unreadable header"))
        }
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

/// The ledger's directory, made if absent: private (`0700`) on Unix, since a record names a checkout.
pub(crate) fn ensure_dir(dir: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(dir) {
        Ok(meta) => {
            if !meta.file_type().is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "ledger directory path is not a regular directory",
                ));
            }
            return Ok(());
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)?;
    }
    if !std::fs::symlink_metadata(dir)?.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ledger directory path is not a regular directory",
        ));
    }
    Ok(())
}

/// Open (creating) a lock file: `0600`, never truncated, refusing a symlink or non-regular file.
pub(crate) fn open_rendezvous(path: &Path) -> io::Result<File> {
    #[cfg(not(any(unix, windows)))]
    if let Ok(meta) = std::fs::symlink_metadata(path)
        && !meta.file_type().is_file()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing non-regular lock file",
        ));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = aterm_uds::kept_out(|| options.open(path))?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "lock path is not a regular file",
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "lock path is a reparse point",
            ));
        }
    }
    Ok(file)
}

/// The working directory's enclosing git checkout (the nearest directory holding `.git`), else the working directory; empty where it cannot be read.
fn checkout_of_cwd() -> String {
    let Ok(cwd) = std::env::current_dir() else {
        return String::new();
    };
    let found = cwd
        .ancestors()
        .find(|d| d.join(".git").symlink_metadata().is_ok())
        .unwrap_or(&cwd);
    found.to_string_lossy().into_owned()
}
