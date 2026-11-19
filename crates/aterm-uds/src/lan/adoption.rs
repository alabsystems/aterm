// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whether the running broker took up the update an adopting image expects of it, and, from the
//! broker's own record, why not (`docs/DESIGN-lan-broker-2026-11-05.md` §3).
//!
//! [`super::install::read`] reads a team-signed image of an older build as [`Part::Adopting`]:
//! the running broker looks at its source, and at each bundle whose process asked it to
//! ([`super::install::consider_for`]), every [`super::ADOPTION_LOOK`] and swaps in a strictly
//! newer clone its strict check admits. That says what the broker would do, not what it does: an
//! older broker follows its source alone, its compiled check may refuse the bundle, and its clone
//! is `clonefile` alone, which a bundle on another volume than the image fails (`EXDEV`, where the
//! elevation's install copies it instead). So the reading is an expectation, bounded here
//! ([`expect`]):
//!
//! - An account that reads its own install adopting and hears the installed broker's hello
//!   compares the hello's build with its own, the build it runs and expects the broker to serve,
//!   and keeps in its state the first time it saw each such pair differ ([`Record`], [`RECORD`]).
//! - A pair that still differs past its bound has lapsed ([`judge`]): [`LOOKS`] of the broker's
//!   looks after it was first seen where the broker recorded a definite failure for the source,
//!   else [`LONG_LOOKS`] ([`bound`]). The image then reads [`Part::NotAdopted`], which is owed,
//!   and the account journals that once. Owed means the next Mac password setup asks for its
//!   root steps, or a person's own `aterm init`, renews the image; it asks no password of its
//!   own (`aterm_machines::root_steps::Owed::LanBrokerAdoption`), unless the broker speaks
//!   another protocol than this build's, which leaves this build's LAN traffic down
//!   (`aterm_machines::root_steps::Owed::LanBroker`, which asks).
//! - A broker heard serving the build a pair expects, or another build than the pair names,
//!   ends that pair, so a broker that took the update up clears the record.
//!
//! The broker says why ([`Refusal`]): where an adoption round fails (its check refused the
//! bundle or gave no verdict, the clone states no newer build, the clone failed, the bundle could
//! not be read), it writes the bundle that round tried, its own build, whether the failure was
//! definite ([`Kind`]) and the reason: its source's to [`super::ADOPTION_REFUSED`], an asker's to
//! that bundle's own file of [`super::ADOPTION_REFUSED_ASKERS`] ([`asker_refusal_in`]), root's
//! files that every account reads, so one lane's record never takes another bundle's place. A
//! lane removes a bundle's record once it adopts that bundle or finds its image is that bundle,
//! and a record a broker left before it moved up names that broker's build, which [`expect`]
//! does not count. A lapsed reading names that reason where a record is the answering broker's and is about the
//! bundle read ([`refusals_of`]).

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::install::{Part, Reading};

/// How many of the broker's looks ([`super::ADOPTION_LOOK`], 5 s) an account waits, from the
/// first time it saw the broker serve another build than its own while it read the image
/// adopting, before that expectation lapses where the broker recorded a definite failure to
/// adopt the source ([`Kind::Definite`], [`bound`]): 36, three minutes. Such a broker does not
/// adopt the source as it is; the bound only outlasts a record an earlier update at the same
/// path left while the broker's first round on this one runs, which adopts it or records anew.
/// It outlasts more than that: the slowest adoption that succeeds after one check with no
/// verdict takes 29 looks: the look that finds the changed source, at most one period late (1);
/// a strict check of the clone that gives no verdict at its bound
/// (`aterm_keeper::identity::CODE_CHECK_HANG`, 60 s: 12); the broker's backoff after that first
/// failure (1) and the look that tries again (1); a second strict check that answers just inside
/// its bound (12); and launchd's start of the broker after it exits, within its 10-second
/// throttle (2). 36 leaves seven looks for the clone, the seal and the swap under load.
/// `aterm-lan`'s tests hold [`LAPSE`] to that sum computed from the broker's own constants.
pub const LOOKS: u32 = 36;

/// [`LOOKS`] of the broker's looks: how long an expectation stands where the broker recorded a
/// definite failure.
pub const LAPSE: Duration = super::ADOPTION_LOOK.saturating_mul(LOOKS);

/// The longest `trustd` stall the long bound ([`LONG_LOOKS`]) outlasts: five minutes.
pub const STALL: Duration = Duration::from_secs(5 * 60);

/// How many of the broker's looks an account waits where the broker recorded no definite failure
/// for the source (its last failure gave no verdict, or it wrote no record, as a broker from
/// before the records writes none): 120, ten minutes. A strict check that gives no verdict
/// leaves its helper on the broker's adoption-check gate (its own, apart from the `trustd` gate
/// its admissions use) overdue until `trustd` answers it, and no look clones anything meanwhile:
/// a check started then would be turned away, and an asker whose own check got no answer waits
/// five minutes at once. Through a [`STALL`] that began with the first check, the slowest
/// adoption that succeeds takes 76 looks: the look that finds the source (1), the stall, the
/// check that gave no verdict among it, through which no look clones (60), the first look after
/// it, a look late (1), a check that answers just inside its bound (12), and launchd's start of
/// the broker (2). 120 leaves 44 looks beside them. `aterm-lan`'s tests hold [`LONG_LAPSE`] to
/// that walk, made with the broker's own backoff and bound.
pub const LONG_LOOKS: u32 = 120;

/// [`LONG_LOOKS`] of the broker's looks: how long an expectation stands where the broker recorded
/// no definite failure.
pub const LONG_LAPSE: Duration = super::ADOPTION_LOOK.saturating_mul(LONG_LOOKS);

/// How long an expectation stands given the broker's `refusal` of the source, filtered to that
/// source and the build that answered: [`LAPSE`] where it names a definite failure, else (one
/// with no verdict, or none) [`LONG_LAPSE`].
#[must_use]
pub fn bound(refusal: Option<&Refusal>) -> Duration {
    match refusal.map(|refusal| refusal.kind) {
        Some(Kind::Definite) => LAPSE,
        Some(Kind::NoVerdict) | None => LONG_LAPSE,
    }
}

/// The account's record's file name, in its aterm state (`aterm_types::dirs::account_state_dir`).
pub const RECORD: &str = "lan-adoption";

/// The most pairs a record keeps; past them the oldest is dropped.
pub const PAIRS: usize = 8;

/// The longest build a pair names; aterm's are far shorter (a version, `+g` and a commit).
const BUILD_MAX: usize = 96;

/// The longest record read: [`PAIRS`] lines, each two builds and two numbers.
pub const RECORD_MAX: usize = PAIRS * (2 * BUILD_MAX + 32);

/// The longest broker refusal read or written.
pub const REFUSAL_MAX: usize = 4096;

/// The longest reason a refusal keeps.
const REASON_MAX: usize = 1024;

/// The first line of the broker's refusal; a file that does not start with it is none.
const REFUSAL_HEADER: &str = "aterm-lan-adoption-refused 2\n";

/// Whether `build` is one a record line can carry: not empty, at most [`BUILD_MAX`] bytes, no
/// whitespace and no control character.
fn word(build: &str) -> bool {
    !build.is_empty()
        && build.len() <= BUILD_MAX
        && !build.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// One pair a record keeps: the build the broker served, the build this account expected of it,
/// when this account first saw the two differ while it read the image adopting (Unix seconds),
/// and whether their lapse was journaled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seen {
    /// The build the broker's hello named.
    pub broker: String,
    /// The build expected of it.
    pub expected: String,
    /// When the pair was first seen, in Unix seconds.
    pub first: u64,
    /// Whether the pair's lapse was journaled.
    pub journaled: bool,
}

/// The account's record of the pairs it saw differ ([`Seen`]), oldest first: one line each,
/// `<first> <0|1> <broker> <expected>`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Record(pub Vec<Seen>);

impl Record {
    /// The record `bytes` hold: each well-formed line, at most [`PAIRS`] of them. A line that is
    /// not one this module writes is left out, so a damaged record starts that pair afresh.
    #[must_use]
    pub fn parse(bytes: &[u8]) -> Self {
        let text = String::from_utf8_lossy(bytes.get(..RECORD_MAX).unwrap_or(bytes));
        let pairs = text.lines().filter_map(|line| {
            let mut words = line.split(' ');
            let first = words.next()?.parse().ok()?;
            let journaled = match words.next()? {
                "0" => false,
                "1" => true,
                _ => return None,
            };
            let (broker, expected) = (words.next()?, words.next()?);
            (words.next().is_none() && word(broker) && word(expected)).then(|| Seen {
                broker: broker.to_owned(),
                expected: expected.to_owned(),
                first,
                journaled,
            })
        });
        Self(pairs.take(PAIRS).collect())
    }

    /// The record's text, one line a pair.
    #[must_use]
    pub fn text(&self) -> String {
        self.0
            .iter()
            .map(|seen| {
                format!(
                    "{} {} {} {}\n",
                    seen.first,
                    u8::from(seen.journaled),
                    seen.broker,
                    seen.expected
                )
            })
            .collect()
    }
}

/// What [`judge`] makes of one reading.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The broker serves the build expected of it.
    Serves,
    /// Nothing is judged: the image is not adopting, or a build is not one a record carries.
    Unjudged,
    /// The broker serves another build, first seen so at `since` (Unix seconds), within its
    /// bound.
    Waiting {
        /// When the pair was first seen.
        since: u64,
    },
    /// The broker still serves another build its bound or more after `since`: the expectation
    /// lapsed. `journal` is true the first time a pair reads lapsed, and only then.
    Lapsed {
        /// When the pair was first seen.
        since: u64,
        /// Whether this reading is the one to journal the lapse.
        journal: bool,
    },
}

/// Judge one reading at `now` (Unix seconds), `record` updated in place: the broker's hello
/// names `broker`, this account expects `expected`, `adopting` says the image reads
/// [`Part::Adopting`], and an expectation stands for `bound` ([`bound`]).
///
/// Every pair whose broker build is not `broker` (the broker moved on from it) or whose expected
/// build is `broker` (the broker serves it) is dropped first, whatever the image reads. Then a
/// broker serving `expected` is [`Verdict::Serves`]; a reading that is not adopting is
/// [`Verdict::Unjudged`]; a pair seen for the first time is kept from `now` and waits; and a pair
/// first seen `bound` or more before `now` has lapsed, journaled once. A pair dated after `now`
/// (the clock set back since) is dated `now`, so no lapse is read off a date ahead of the
/// reader's clock.
pub fn judge(
    record: &mut Record,
    adopting: bool,
    broker: &str,
    expected: &str,
    now: u64,
    bound: Duration,
) -> Verdict {
    record
        .0
        .retain(|seen| seen.broker == broker && seen.expected != broker);
    if broker == expected {
        return Verdict::Serves;
    }
    if !adopting || !word(broker) || !word(expected) {
        return Verdict::Unjudged;
    }
    let Some(seen) = record.0.iter_mut().find(|seen| seen.expected == expected) else {
        if record.0.len() >= PAIRS {
            record.0.remove(0);
        }
        record.0.push(Seen {
            broker: broker.to_owned(),
            expected: expected.to_owned(),
            first: now,
            journaled: false,
        });
        return Verdict::Waiting { since: now };
    };
    seen.first = seen.first.min(now);
    if now - seen.first < bound.as_secs() {
        return Verdict::Waiting { since: seen.first };
    }
    let journal = !seen.journaled;
    seen.journaled = true;
    Verdict::Lapsed {
        since: seen.first,
        journal,
    }
}

/// Whether a failure the broker recorded says it will not adopt the source as it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Its strict check refused the bundle, the bundle was refused before its check, or the
    /// clone failed (another volume, or another errno): [`LAPSE`] bounds the expectation.
    Definite,
    /// Its check gave no verdict (`trustd` did not answer, the gate turned it down, it could not
    /// run), or the source could not be read: a later round may adopt it, so
    /// [`LONG_LAPSE`] does.
    NoVerdict,
}

impl Kind {
    /// The word a record carries.
    const fn word(self) -> &'static str {
        match self {
            Self::Definite => "definite",
            Self::NoVerdict => "no-verdict",
        }
    }
}

/// The broker's record of why it did not adopt a bundle ([`super::ADOPTION_REFUSED`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    /// The bundle the broker did not adopt: its source, or an asker's.
    pub source: PathBuf,
    /// The broker's own build, as its hello names it.
    pub build: String,
    /// Whether the failure was definite.
    pub kind: Kind,
    /// Why, in one line.
    pub reason: String,
}

impl Refusal {
    /// The refusal of `source` by the broker of `build`, a failure of `kind`, `reason` made one
    /// line: each control character a space, cut to at most [`REASON_MAX`] bytes.
    #[must_use]
    pub fn new(source: &Path, build: &str, kind: Kind, reason: &str) -> Self {
        let mut line: String = reason
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        if line.len() > REASON_MAX {
            let mut cut = REASON_MAX;
            while !line.is_char_boundary(cut) {
                cut -= 1;
            }
            line.truncate(cut);
        }
        Self {
            source: source.to_path_buf(),
            build: build.to_owned(),
            kind,
            reason: line,
        }
    }

    /// The record's text: [`REFUSAL_HEADER`], then `source=`, `build=`, `kind=` and `reason=`
    /// lines.
    /// `None` for a source or build a line cannot carry.
    #[must_use]
    pub fn text(&self) -> Option<String> {
        let source = self
            .source
            .to_str()
            .filter(|s| !s.chars().any(char::is_control))?;
        (word(&self.build) && !self.reason.chars().any(char::is_control)).then(|| {
            format!(
                "{REFUSAL_HEADER}source={source}\nbuild={}\nkind={}\nreason={}\n",
                self.build,
                self.kind.word(),
                self.reason
            )
        })
    }

    /// The refusal `bytes` hold: `None` for anything [`Self::text`] does not write.
    #[must_use]
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > REFUSAL_MAX {
            return None;
        }
        let text = std::str::from_utf8(bytes).ok()?;
        let mut lines = text.strip_prefix(REFUSAL_HEADER)?.split_terminator('\n');
        let mut field = |key: &str| lines.next()?.strip_prefix(key);
        let (source, build) = (field("source=")?, field("build=")?);
        let kind = match field("kind=")? {
            "definite" => Kind::Definite,
            "no-verdict" => Kind::NoVerdict,
            _ => return None,
        };
        let reason = field("reason=")?;
        (lines.next().is_none() && word(build) && Path::new(source).is_absolute()).then(|| Self {
            source: PathBuf::from(source),
            build: build.to_owned(),
            kind,
            reason: reason.to_owned(),
        })
    }
}

/// The file of `folder` ([`super::ADOPTION_REFUSED_ASKERS`] for the running broker) that holds the
/// broker's record of why it did not adopt `bundle`, which asked it to: named by the SHA-256 of
/// the path's bytes in hex, so any path names one file and none names another's.
#[must_use]
pub fn asker_refusal_in(folder: &Path, bundle: &Path) -> PathBuf {
    use std::fmt::Write as _;
    #[cfg(unix)]
    let bytes = std::os::unix::ffi::OsStrExt::as_bytes(bundle.as_os_str());
    #[cfg(not(unix))]
    let bytes = bundle.as_os_str().as_encoded_bytes();
    let mut digest = aterm_digest::Sha256::new();
    digest.update(bytes);
    let name = digest
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut name, byte| {
            let _ = write!(name, "{byte:02x}");
            name
        });
    folder.join(name)
}

/// The broker's records of why it did not adopt `source` that `port` reads
/// ([`Expecting::refusal`]): its source lane's ([`super::ADOPTION_REFUSED`]) where that names
/// `source`, then `source`'s own of the askers' ([`super::ADOPTION_REFUSED_ASKERS`]) where that
/// names it. A record that does not read, parse or name `source` is none.
pub fn refusals_of(port: &dyn Expecting, source: &Path) -> Vec<Refusal> {
    [
        PathBuf::from(super::ADOPTION_REFUSED),
        asker_refusal_in(Path::new(super::ADOPTION_REFUSED_ASKERS), source),
    ]
    .iter()
    .filter_map(|path| Refusal::parse(&port.refusal(path).ok()?))
    .filter(|refusal| refusal.source == source)
    .collect()
}

/// What [`expect`] reads and writes: the account's record, the installed broker's hello, root's
/// refusal, the clock and the journal.
pub trait Expecting {
    /// Unix seconds now.
    fn now(&self) -> u64;

    /// The account's record ([`RECORD`]): `None` where this account keeps none (no state
    /// folder, or root, whose elevation renews the image itself), which judges nothing; else the
    /// read's answer, `NotFound` for no record yet.
    fn record(&self) -> Option<io::Result<Vec<u8>>>;

    /// Replace the account's record with `bytes`, whole (an atomic write).
    ///
    /// # Errors
    /// The write's.
    fn keep(&self, bytes: &[u8]) -> io::Result<()>;

    /// The installed root broker's hello, of whatever protocol it speaks, `None` where it did
    /// not answer (or another server answered for it, such as this process's direct path).
    fn hello(&self) -> Option<super::BrokerHello>;

    /// Root's own file at `path` ([`super::ADOPTION_REFUSED`], or a file of
    /// [`super::ADOPTION_REFUSED_ASKERS`]), at most [`REFUSAL_MAX`] bytes.
    ///
    /// # Errors
    /// The read's; a file that is not root's own regular file is refused.
    fn refusal(&self, path: &Path) -> io::Result<Vec<u8>>;

    /// Journal `line`, the one report of a lapse.
    fn journal(&self, line: &str);
}

/// `reading`, of the install from the bundle `source` that this process runs from (whose build
/// is `expected`), with its adopting image judged ([`judge`]): [`Part::NotAdopted`] once the
/// expectation lapsed, naming the broker's own reason where its refusal is of `source` and its
/// build is the one that answered, and whether that broker speaks this thread's protocol (one
/// that does not serves this build nothing); else `reading` as it was. The expectation stands
/// for [`LAPSE`] where that refusal names a definite failure, else for [`LONG_LAPSE`]
/// ([`bound`]). Every adopting reading is judged against this process's own build: an account
/// that keeps no record or cannot read it, and a broker that does not answer, are not judged. The
/// hello is asked for only where the image reads adopting or the record holds a pair, which a
/// broker serving the expected build then clears.
///
/// The record is kept only where the reading changed it, best effort: one that cannot be kept
/// leaves the next reading to start its pair afresh. The lapse is journaled once, and only where
/// the record saying so was kept, so a failing write does not journal it at every reading.
pub fn expect(port: &dyn Expecting, reading: Reading, source: &Path, expected: &str) -> Reading {
    let adopting = matches!(reading.image, Part::Adopting(_));
    let mut record = match port.record() {
        None => return reading,
        Some(Ok(bytes)) => Record::parse(&bytes),
        Some(Err(error)) if error.kind() == io::ErrorKind::NotFound => Record::default(),
        Some(Err(_)) => return reading,
    };
    if !adopting && record.0.is_empty() {
        return reading;
    }
    let Some(hello) = port.hello() else {
        return reading;
    };
    let (broker, serves) = (hello.build, hello.protocol == super::protocol());
    // Root's record says how long to wait, only where it is of this source and of the broker
    // that answered: an older broker's, or none, is no definite failure.
    let refusal = (broker != expected)
        .then(|| refusals_of(port, source))
        .unwrap_or_default()
        .into_iter()
        .find(|refusal| refusal.build == broker);
    let before = record.clone();
    let now = port.now();
    let verdict = judge(
        &mut record,
        adopting,
        &broker,
        expected,
        now,
        bound(refusal.as_ref()),
    );
    let kept = record == before || port.keep(record.text().as_bytes()).is_ok();
    let (Verdict::Lapsed { since, journal }, Part::Adopting(team)) = (verdict, &reading.image)
    else {
        return reading;
    };
    let image = Part::NotAdopted {
        team: team.clone(),
        broker,
        serves,
        why: refusal.map(|refusal| refusal.reason),
    };
    if journal && kept {
        port.journal(&format!(
            "LAN broker: the image is {}, {} s after this account first expected {expected} of \
             it; the next Mac password setup asks for its root steps, or a person's own `aterm \
             init`, renews it",
            image.words(),
            now - since,
        ));
    }
    Reading { image, ..reading }
}

#[cfg(test)]
#[path = "adoption_tests.rs"]
mod tests;
