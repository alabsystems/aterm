// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What Spotlight indexes against the scope the owner wants (SD-95): the System Resources
//! page's Spotlight document reads it ([`crate::stats_lens`]'s `spotlight_doc`), and later the
//! page's macOS configuration rows can too.
//!
//! The wanted scope is FL-127's ([`spotlight_scope::kept`] and [`spotlight_scope::wanted`]):
//! Applications, Documents, Desktop and the synced drives are indexed, everything else on the
//! data volume is kept out. What is indexed now is read from FL-127's receipt, from the data
//! volume's indexing as SD-93's reading last carried it, and from Spotlight itself: an
//! `mdfind -onlyin` existence probe ([`ScopePort`]) that stops at its first match, of each
//! folder the scope keeps out (first those the receipt does not name, then, more slowly, those
//! it does, since a person may have taken one back) and of the build cache. No probe ever reads
//! a folder: Spotlight answers from its index, so a guarded folder raises no question; and a
//! kept folder is never probed, since an empty answer cannot tell an empty folder from an
//! excluded one. The listing reads only what FL-127's reading reads, never the synced drives'
//! own folders (`~/Library/CloudStorage` and `~/Library/Mobile Documents`, which macOS guards).
//!
//! The document's buttons (SD-96: Apply scope…, Pause or Resume indexing, Rebuild index…, Keep a
//! folder out…) are each a root step of [`root_only::Step`]'s, run under the password sheet bound
//! to the page's view; the folders a person kept out ([`spotlight_scope::YOURS`]) and their pause
//! ([`spotlight_scope::PAUSE_RECORD`]) ride the listing, read on the probe's thread, and the pause
//! is judged against the data volume's reading by the run that started after it
//! ([`spotlight_scope::pause`]).
//!
//! The probe runs only while the page is on screen, at most once a [`PROBE_EVERY`], at most
//! [`PROBES_A_TICK`] folders a tick, the folders never asked first and then those asked longest
//! ago, and asks again of a folder only after [`ANSWER_KEPT`] ([`RECEIPT_ANSWER_KEPT`] for one
//! the receipt names) ([`ScopeSlot::service`]).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_types::macos_setup::root_only;
use aterm_types::macos_setup::spotlight_scope::{self, Part, Pause, PauseRecord};
use aterm_vitals::spotlight::Indexing;

/// The page's probe ticks at most this often while the page is on screen.
pub(crate) const PROBE_EVERY: Duration = Duration::from_secs(60);
/// A folder's answer stands this long before the probe asks of it again; so does a probe that
/// timed out, so a slow Spotlight is not asked every minute.
pub(crate) const ANSWER_KEPT: Duration = Duration::from_secs(600);
/// The same for a folder setup's receipt names: asked again more slowly, behind every folder
/// the receipt does not name, to find one a person took back.
pub(crate) const RECEIPT_ANSWER_KEPT: Duration = Duration::from_secs(3600);
/// The most folders one tick probes.
pub(crate) const PROBES_A_TICK: usize = 4;
/// One probe's deadline: past it the folder reads unread.
pub(crate) const PROBE_WAIT: Duration = Duration::from_secs(5);
/// Spotlight's command-line query.
const MDFIND: &str = "/usr/bin/mdfind";
/// Every item the index holds. Not `kMDItemFSName`, which `mdfind` answers by walking the file
/// system (it found items inside an excluded `*.noindex` folder, and a walk inside a guarded
/// folder can raise a privacy question); and not `-count`, which makes `mds_stores` count every
/// match (a count of the home had not answered after two minutes) where this stops at the first.
const QUERY: &str = "kMDItemContentTypeTree == \"public.item\"";

/// The parts of what Spotlight keeps indexing, in the page's order.
pub(crate) const PARTS: [Part; 4] = [
    Part::Applications,
    Part::Documents,
    Part::Desktop,
    Part::SyncedDrives,
];

/// A part's name as the page says it, mid-sentence: a new [`Part`] fails to compile here.
pub(crate) const fn part_word(part: Part) -> &'static str {
    match part {
        Part::Applications => "Applications",
        Part::Documents => "Documents",
        Part::Desktop => "Desktop",
        Part::SyncedDrives => "synced drives",
    }
}

/// `words` as a list: `A`, `A and B`, `A, B and C`.
fn listed(words: &[String]) -> String {
    match words.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        None => String::new(),
    }
}

/// The wanted scope in words: `Applications, Documents, Desktop and synced drives`.
pub(crate) fn wanted_words() -> String {
    let words: Vec<String> = PARTS
        .iter()
        .map(|&part| part_word(part).to_owned())
        .collect();
    listed(&words)
}

/// What one probe of a folder found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Found {
    /// Spotlight's index holds an item inside it.
    Item,
    /// Spotlight's index holds nothing inside it.
    Nothing,
    /// The probe failed or timed out.
    Unread,
}

/// What the page reads of this Mac's scope with no probe: listings and root's receipts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ScopeListing {
    /// The account's home folder.
    pub(crate) home: Option<PathBuf>,
    /// The folders FL-127's receipt names ([`spotlight_scope::RECEIPT`]): those setup excluded.
    pub(crate) receipt: Result<BTreeSet<PathBuf>, String>,
    /// The folders a failed apply could not keep out ([`spotlight_scope::REFUSED`]).
    pub(crate) refused: BTreeSet<PathBuf>,
    /// The folders the scope keeps out ([`spotlight_scope::wanted`]).
    pub(crate) wanted: Result<BTreeSet<PathBuf>, String>,
    /// The shared build cache ([`aterm_toolchain::build_cache::cache_root`]).
    pub(crate) cache: Option<PathBuf>,
    /// The folders the person kept out from this page ([`spotlight_scope::YOURS`]).
    pub(crate) yours: BTreeSet<PathBuf>,
    /// The person's pause, as its record last read ([`spotlight_scope::PAUSE_RECORD`]).
    pub(crate) pause: Option<PauseRecord>,
}

impl ScopeListing {
    /// A listing nothing of which was read, for `why`.
    pub(crate) fn unread(why: &str) -> Self {
        Self {
            home: None,
            receipt: Err(why.to_owned()),
            refused: BTreeSet::new(),
            wanted: Err(why.to_owned()),
            cache: None,
            yours: BTreeSet::new(),
            pause: None,
        }
    }

    /// Whether setup's receipt names every folder the scope keeps out: `None` while either is
    /// unread.
    pub(crate) fn applied(&self) -> Option<bool> {
        match (&self.wanted, &self.receipt) {
            (Ok(_), Ok(_)) => Some(self.missing().is_empty()),
            _ => None,
        }
    }

    /// The folders the scope keeps out that the receipt does not name: those Spotlight may still
    /// index. Empty while either is unread.
    fn missing(&self) -> BTreeSet<PathBuf> {
        match (&self.wanted, &self.receipt) {
            (Ok(wanted), Ok(receipt)) => wanted.difference(receipt).cloned().collect(),
            _ => BTreeSet::new(),
        }
    }

    /// The folders the scope keeps out that the receipt names. Empty while either is unread.
    fn on_receipt(&self) -> BTreeSet<PathBuf> {
        match (&self.wanted, &self.receipt) {
            (Ok(wanted), Ok(receipt)) => wanted.intersection(receipt).cloned().collect(),
            _ => BTreeSet::new(),
        }
    }

    /// The folders a tick may probe, in order, each with how long its answer stands: the build
    /// cache first (its row goes by that one answer), then the missing folders, the home's own
    /// first (where a person's new folders appear), each for [`ANSWER_KEPT`]; then the folders
    /// the receipt names, for [`RECEIPT_ANSWER_KEPT`].
    fn targets(&self) -> Vec<(PathBuf, Duration)> {
        let missing = self.missing();
        let at_top = |path: &&PathBuf| self.home.as_deref() == path.parent();
        let mut out: Vec<(PathBuf, Duration)> = self
            .cache
            .iter()
            .map(|cache| (cache.clone(), ANSWER_KEPT))
            .collect();
        out.extend(
            missing
                .iter()
                .filter(at_top)
                .chain(missing.iter().filter(|path| !at_top(path)))
                .map(|path| (path.clone(), ANSWER_KEPT)),
        );
        out.extend(self.yours.iter().map(|path| (path.clone(), ANSWER_KEPT)));
        out.extend(
            self.on_receipt()
                .into_iter()
                .map(|path| (path, RECEIPT_ANSWER_KEPT)),
        );
        out
    }

    /// The folders the receipt names that `found` says Spotlight's index holds items in again,
    /// apart from those an apply failed to keep out ([`Self::refused`]): a person took them back
    /// since. Empty while the scope's folders or the receipt are unread.
    fn taken_back<'a>(&'a self, found: &BTreeMap<PathBuf, Found>) -> Vec<&'a PathBuf> {
        match (&self.wanted, &self.receipt) {
            (Ok(wanted), Ok(receipt)) => wanted
                .iter()
                .filter(|path| receipt.contains(*path) && !self.refused.contains(*path))
                .filter(|path| found.get(*path) == Some(&Found::Item))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// `path` as a person reads it: under the home as `~/…`.
    pub(crate) fn shown(&self, path: &Path) -> String {
        match self
            .home
            .as_deref()
            .and_then(|home| path.strip_prefix(home).ok())
        {
            Some(rest) => format!("~/{}", rest.display()),
            None => path.display().to_string(),
        }
    }

    /// Every one of `paths` as a person reads them: `~/a, ~/b and /c`.
    fn every(&self, paths: &[&PathBuf]) -> String {
        let words: Vec<String> = paths.iter().map(|path| self.shown(path)).collect();
        listed(&words)
    }

    /// `paths` as a person reads them: the first named, then how many more.
    fn named(&self, paths: &[&PathBuf]) -> String {
        match paths {
            [] => String::new(),
            [one] => self.shown(one),
            [first, rest @ ..] => format!("{} and {} more", self.shown(first), rest.len()),
        }
    }
}

/// What the page knows of the scope: the listing and every folder's last probe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ScopeReading {
    pub(crate) listing: ScopeListing,
    pub(crate) found: BTreeMap<PathBuf, Found>,
}

impl ScopeReading {
    /// The folders a person took back into Spotlight since setup kept them out, as a person
    /// reads them (`~/a`): what What is indexed names as taken back, and System State's
    /// Spotlight indexing row reads as Kept as you set it (FL-188).
    pub(crate) fn taken_back(&self) -> Vec<String> {
        self.listing
            .taken_back(&self.found)
            .into_iter()
            .map(|path| self.listing.shown(path))
            .collect()
    }
}

/// Whether a row's part is indexed now, or is wanted indexed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Now {
    Indexed,
    KeptOut,
    Unread,
}

impl Now {
    /// Its word in the table.
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Indexed => "Indexed",
            Self::KeptOut => "Kept out",
            Self::Unread => "Not read",
        }
    }
}

/// One row of What is indexed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ScopeRow {
    pub(crate) name: String,
    pub(crate) now: Now,
    pub(crate) wanted: Now,
    /// Why it reads as it does.
    pub(crate) words: String,
}

impl ScopeRow {
    /// Whether what is indexed now is not what is wanted. An unread row does not differ.
    pub(crate) fn differs(&self) -> bool {
        self.now != Now::Unread && self.now != self.wanted
    }

    /// The row in words, so colour is never the only signal.
    pub(crate) fn said(&self) -> String {
        format!(
            "{}: {} now, wanted {}{}; {}.",
            self.name,
            self.now.word().to_lowercase(),
            self.wanted.word().to_lowercase(),
            if self.differs() {
                ", differs from wanted"
            } else {
                ""
            },
            self.words,
        )
    }
}

/// `is` for one, `are` for more.
const fn is_are(n: usize) -> &'static str {
    if n == 1 { "is" } else { "are" }
}

/// Everything else on the data volume against the receipt and the probes' answers: Indexed,
/// naming every folder Spotlight finds items in and why each is not kept out; Kept out where
/// the receipt names every folder and no probe found one indexed, or where Spotlight finds
/// nothing in the folders the receipt does not name; else Not read, naming the folders not yet
/// asked and those whose probe did not answer.
fn everything_else(
    listing: &ScopeListing,
    found: &BTreeMap<PathBuf, Found>,
    wanted: &BTreeSet<PathBuf>,
) -> (Now, String) {
    let of = |path: &PathBuf| found.get(path).copied();
    let indexed: Vec<&PathBuf> = wanted
        .iter()
        .filter(|path| of(path) == Some(Found::Item))
        .collect();
    if !indexed.is_empty() {
        let mut clauses = vec![format!(
            "Spotlight finds items in {}",
            listing.every(&indexed)
        )];
        let refused: Vec<&PathBuf> = indexed
            .iter()
            .copied()
            .filter(|path| listing.refused.contains(*path))
            .collect();
        if !refused.is_empty() {
            clauses.push(format!(
                "setup could not keep {} out",
                listing.every(&refused)
            ));
        }
        let taken = listing.taken_back(found);
        if !taken.is_empty() {
            clauses.push(format!(
                "setup's receipt says it kept {} out, so {} taken back since",
                listing.every(&taken),
                if taken.len() == 1 {
                    "it was"
                } else {
                    "they were"
                }
            ));
        }
        return (Now::Indexed, clauses.join("; "));
    }
    let missing = listing.missing();
    if missing.is_empty() {
        let asked = wanted
            .iter()
            .filter(|path| of(path) == Some(Found::Nothing))
            .count();
        let record = format!(
            "setup excluded all {} of its folders (its receipt)",
            wanted.len()
        );
        let probes = if asked == 0 {
            "; Spotlight was not asked of them yet".to_owned()
        } else if asked == wanted.len() {
            ", and Spotlight finds nothing in any of them".to_owned()
        } else {
            format!(", and Spotlight finds nothing in the {asked} asked so far")
        };
        return (Now::KeptOut, format!("{record}{probes}"));
    }
    let unasked: Vec<&PathBuf> = missing.iter().filter(|path| of(path).is_none()).collect();
    let silent: Vec<&PathBuf> = missing
        .iter()
        .filter(|path| of(path) == Some(Found::Unread))
        .collect();
    let empty = missing.len() - unasked.len() - silent.len();
    if unasked.is_empty() && silent.is_empty() {
        return (
            Now::KeptOut,
            format!(
                "Spotlight finds nothing in the {} {} setup has not kept out",
                missing.len(),
                if missing.len() == 1 {
                    "folder"
                } else {
                    "folders"
                }
            ),
        );
    }
    let mut clauses = Vec::new();
    if !unasked.is_empty() {
        clauses.push(format!(
            "{} {} not kept out by setup and not asked of Spotlight yet",
            listing.named(&unasked),
            is_are(unasked.len())
        ));
    }
    if !silent.is_empty() {
        clauses.push(format!(
            "Spotlight did not answer within {} s for {}",
            PROBE_WAIT.as_secs(),
            listing.named(&silent)
        ));
    }
    if empty > 0 {
        clauses.push(format!(
            "Spotlight finds nothing in {empty} other {} setup has not kept out",
            if empty == 1 { "folder" } else { "folders" }
        ));
    }
    (Now::Unread, clauses.join("; "))
}

/// What is indexed against the wanted scope: one row a part of [`PARTS`], then everything else
/// on the data volume, then the build output. `scope` is `None` until the probe's first tick
/// read the listing, and every row then reads Not read (unless the data volume's indexing is
/// off, which keeps everything out). `data_volume` is the data volume's indexing as SD-93's
/// reading last carried it; the page runs no `mdutil` of its own.
pub(crate) fn what_is_indexed(
    scope: Option<&ScopeReading>,
    data_volume: Option<&Indexing>,
) -> Vec<ScopeRow> {
    let off = matches!(data_volume, Some(Indexing::Disabled));
    let volume_read = matches!(data_volume, Some(Indexing::Enabled | Indexing::Disabled));
    const OFF: &str = "indexing is off on the data volume";
    const NOT_YET: &str = "not read yet";
    let judged = |read: &dyn Fn(&ScopeReading) -> (Now, String)| match (off, scope) {
        (true, _) => (Now::KeptOut, OFF.to_owned()),
        (false, None) => (Now::Unread, NOT_YET.to_owned()),
        (false, Some(scope)) => read(scope),
    };
    let mut rows = Vec::with_capacity(PARTS.len() + 2);
    for part in PARTS {
        let (now, words) = judged(&|scope| {
            let listing = &scope.listing;
            let folders: Vec<PathBuf> = listing
                .home
                .as_deref()
                .map(spotlight_scope::kept)
                .unwrap_or_default()
                .into_iter()
                .filter(|(of, _)| *of == part)
                .map(|(_, path)| path)
                .collect();
            match &listing.receipt {
                Err(why) => (
                    Now::Unread,
                    format!("setup's receipt could not be read: {why}"),
                ),
                Ok(receipt) => {
                    let excluded: Vec<&PathBuf> =
                        folders.iter().filter(|f| receipt.contains(*f)).collect();
                    if !excluded.is_empty() {
                        (
                            Now::KeptOut,
                            format!(
                                "setup's receipt says it kept {} out",
                                listing.every(&excluded)
                            ),
                        )
                    } else if !volume_read {
                        (
                            Now::Unread,
                            "the data volume's indexing is not read yet".to_owned(),
                        )
                    } else {
                        let inside: Vec<&PathBuf> = listing
                            .yours
                            .iter()
                            .filter(|path| folders.iter().any(|f| path.starts_with(f)))
                            .collect();
                        (
                            Now::Indexed,
                            if inside.is_empty() {
                                "nothing aterm recorded keeps it out".to_owned()
                            } else {
                                format!(
                                    "indexed but for {}, which you kept out",
                                    listing.every(&inside)
                                )
                            },
                        )
                    }
                }
            }
        });
        let mut name = part_word(part).to_owned();
        if let Some(first) = name.get_mut(..1) {
            first.make_ascii_uppercase();
        }
        rows.push(ScopeRow {
            name,
            now,
            wanted: Now::Indexed,
            words,
        });
    }
    // Each folder the person kept out from this page: theirs, wanted out, and out unless
    // Spotlight's index still finds items in it.
    for path in scope
        .map(|scope| &scope.listing.yours)
        .into_iter()
        .flatten()
    {
        let (now, words) = judged(&|scope| {
            let mine = "you kept it out from this page";
            match scope.found.get(path) {
                Some(Found::Item) => (
                    Now::Indexed,
                    format!("{mine}, and Spotlight still finds items in it"),
                ),
                Some(Found::Nothing) => (
                    Now::KeptOut,
                    format!("{mine}, and Spotlight finds nothing in it"),
                ),
                _ => (Now::KeptOut, format!("{mine} (its record)")),
            }
        });
        rows.push(ScopeRow {
            name: scope.map_or_else(|| path.display().to_string(), |s| s.listing.shown(path)),
            now,
            wanted: Now::KeptOut,
            words,
        });
    }
    let (now, words) = judged(&|scope| {
        let listing = &scope.listing;
        match (&listing.wanted, &listing.receipt) {
            (Err(why), _) => (
                Now::Unread,
                format!("its folders could not be listed: {why}"),
            ),
            (_, Err(why)) => (
                Now::Unread,
                format!("setup's receipt could not be read: {why}"),
            ),
            (Ok(wanted), Ok(_)) => everything_else(listing, &scope.found, wanted),
        }
    });
    rows.push(ScopeRow {
        name: "Everything else on the data volume".to_owned(),
        now,
        wanted: Now::KeptOut,
        words,
    });
    const BY_NAME: &str = "kept out by name (*.noindex)";
    let (now, words) = judged(&|scope| {
        let listing = &scope.listing;
        let Some(cache) = listing.cache.as_ref() else {
            return (Now::KeptOut, BY_NAME.to_owned());
        };
        match scope.found.get(cache) {
            Some(Found::Item) => (
                Now::Indexed,
                format!("Spotlight finds items in {}", listing.shown(cache)),
            ),
            Some(Found::Nothing) => (
                Now::KeptOut,
                format!("{BY_NAME}, and Spotlight finds nothing in the build cache"),
            ),
            Some(Found::Unread) => (
                Now::KeptOut,
                format!(
                    "{BY_NAME}; Spotlight did not answer within {} s for the build cache",
                    PROBE_WAIT.as_secs()
                ),
            ),
            None => (
                Now::KeptOut,
                format!("{BY_NAME}; Spotlight was not asked of the build cache yet"),
            ),
        }
    });
    rows.push(ScopeRow {
        name: "Build output and Rust docs".to_owned(),
        now,
        wanted: Now::KeptOut,
        words,
    });
    rows
}

/// The data volume's indexing in `snap` as the pause is judged by it ([`spotlight_scope::pause`]):
/// whether it read enabled, and when the `mdutil -s -a` run that read it started, unix ms. `None`
/// while unread or unknown.
pub(crate) fn data_volume_read(snap: &aterm_vitals::Snapshot) -> Option<(bool, i64)> {
    let reading = snap.spotlight.as_ref()?;
    let volume = reading
        .volumes
        .as_ref()?
        .iter()
        .find(|v| v.path == spotlight_scope::DATA_VOLUME)?;
    let on = match volume.indexing {
        Indexing::Enabled => true,
        Indexing::Disabled => false,
        Indexing::Unknown(_) => return None,
    };
    let age = i64::try_from(reading.volumes_age_ms?).unwrap_or(i64::MAX);
    Some((on, reading.at_unix_ms.saturating_sub(age)))
}

/// How the person's pause stands on the page: `scope`'s record against `snap`'s data volume.
pub(crate) fn pause_of(scope: Option<&ScopeReading>, snap: &aterm_vitals::Snapshot) -> Pause {
    spotlight_scope::pause(
        scope.and_then(|scope| scope.listing.pause.as_ref()),
        data_volume_read(snap),
    )
    .0
}

/// The document's buttons, by the step each runs: Apply scope…, Pause indexing or Resume
/// indexing (one place, its face following the pause), Rebuild index… and Keep a folder out….
/// Each is live only where the page's own model says its step would do something now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Controls {
    /// Each button, in order: its step and whether it is live.
    pub(crate) buttons: Vec<(root_only::Step, bool)>,
}

/// The document's buttons for `scope` and the data volume in `snap`, indexing judged on only
/// where the pause reads [`Pause::On`] (a pause of the person's or a resume no reading has seen
/// yet is not on, whatever the last reading of the volume said): Apply scope… live while
/// indexing is on and setup's receipt misses a folder the scope keeps out (with indexing off,
/// the scope step reads done and would change nothing); Pause while indexing is on, Resume while
/// the pause is the person's or indexing reads off (Resume is a person's press, whoever turned
/// it off); Rebuild while indexing is on and the scope reads applied (a rebuild before the scope
/// is applied indexes the whole volume again, the hours of indexing the scope is there to spare,
/// and one with indexing off reads not applied); Keep a folder out… while the home and the data
/// volume are read.
pub(crate) fn controls(
    scope: Option<&ScopeReading>,
    snap: Option<&aterm_vitals::Snapshot>,
) -> Controls {
    use root_only::Step;
    let volume = snap.and_then(data_volume_read);
    let pause = snap.map_or(Pause::Unread, |snap| pause_of(scope, snap));
    let on = pause == Pause::On;
    let listing = scope.map(|scope| &scope.listing);
    let applied = listing.and_then(ScopeListing::applied);
    let resume = matches!(pause, Pause::ByYou { .. } | Pause::Off);
    Controls {
        buttons: vec![
            (Step::SpotlightScope, on && applied == Some(false)),
            if resume {
                (Step::SpotlightResume, true)
            } else {
                (Step::SpotlightPause, on)
            },
            (Step::SpotlightRebuild, on && applied == Some(true)),
            (
                Step::SpotlightKeepOut,
                volume.is_some() && listing.is_some_and(|l| l.home.is_some()),
            ),
        ],
    }
}

/// How the page reads the scope: listings with no probe, and one folder's probe.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ScopePort {
    pub(crate) list: fn() -> ScopeListing,
    pub(crate) found: fn(&Path) -> Found,
}

impl ScopePort {
    /// Reads and runs nothing.
    pub(crate) const INERT: Self = Self {
        list: || ScopeListing::unread("inert"),
        found: |_| Found::Unread,
    };

    /// This Mac.
    const LIVE: Self = Self {
        list: list_live,
        found: mdfind_found,
    };

    /// The port a host starts with: a test's host probes nothing.
    pub(crate) const fn host() -> Self {
        if cfg!(test) { Self::INERT } else { Self::LIVE }
    }
}

/// The listing of this Mac: FL-127's receipts and wanted set, by FL-127's own reading (which
/// never lists the synced drives' folders), and the build cache's path. No `mdutil` (the data
/// volume's indexing comes from SD-93's reading).
fn list_live() -> ScopeListing {
    #[cfg(target_os = "macos")]
    {
        use spotlight_scope::{Live, Reads};
        let reads = Live;
        let Some(home) = reads.home() else {
            return ScopeListing::unread("no home folder");
        };
        ScopeListing {
            receipt: spotlight_scope::listed(&reads, spotlight_scope::RECEIPT),
            refused: spotlight_scope::listed(&reads, spotlight_scope::REFUSED).unwrap_or_default(),
            wanted: spotlight_scope::wanted(&reads, &home),
            cache: Some(aterm_toolchain::build_cache::cache_root(&home)),
            yours: spotlight_scope::listed(&reads, spotlight_scope::YOURS).unwrap_or_default(),
            // The slot reads the pause record from its own folder ([`ScopeSlot::tick`]).
            pause: None,
            home: Some(home),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        ScopeListing::unread("unsupported")
    }
}

/// Whether Spotlight's index holds an item inside `dir`: `mdfind -onlyin <dir>` stopped at its
/// first line, under [`PROBE_WAIT`]. Spotlight answers from its index; nothing reads `dir`.
fn mdfind_found(dir: &Path) -> Found {
    #[cfg(target_os = "macos")]
    {
        use aterm_uds::deadline::ChildOutput;
        let mut find = aterm_uds::spawnfd::command(MDFIND);
        find.arg("-onlyin").arg(dir).arg(QUERY);
        let seen = std::cell::Cell::new(false);
        let ran = aterm_uds::deadline::stream_until_stopped(
            &mut find,
            Some(Instant::now() + PROBE_WAIT),
            |_| {},
            |stream, chunk| {
                if stream == ChildOutput::Stdout && !chunk.is_empty() {
                    seen.set(true);
                }
                Ok(())
            },
            || seen.get(),
        );
        let found = match ran {
            _ if seen.get() => Found::Item,
            Ok(Some(status)) if status.success() => Found::Nothing,
            _ => Found::Unread,
        };
        aterm_log::info!(
            "System Resources: Spotlight probe of {}: {found:?}",
            dir.display()
        );
        found
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (dir, MDFIND, QUERY);
        Found::Unread
    }
}

#[derive(Default)]
struct SlotState {
    ticked: Option<Instant>,
    running: bool,
    answers: BTreeMap<PathBuf, (Instant, Found)>,
    reading: Option<Arc<ScopeReading>>,
    rev: u64,
    /// The data volume's last reading as the host last noted it ([`ScopeSlot::note_volume`]).
    volume: Option<(bool, i64)>,
    /// What the last press of the document's buttons did.
    said: Option<String>,
}

impl SlotState {
    /// Publish `listing` with the answers for its targets, dropping any other folder's answer.
    fn publish(&mut self, listing: ScopeListing) {
        let targets: BTreeSet<PathBuf> = listing
            .targets()
            .into_iter()
            .map(|(path, _)| path)
            .collect();
        self.answers.retain(|path, _| targets.contains(path));
        let found = self
            .answers
            .iter()
            .map(|(path, (_, found))| (path.clone(), *found))
            .collect();
        self.reading = Some(Arc::new(ScopeReading { listing, found }));
        self.rev = self.rev.wrapping_add(1);
    }
}

/// The folders of `listing` a tick at `now` probes: at most [`PROBES_A_TICK`] of those with no
/// answer still standing, the build cache and the folders the receipt does not name ahead of
/// those it names, and within each the folders never asked first, then those asked longest ago,
/// so every folder is asked in turn however many there are.
fn due(
    listing: &ScopeListing,
    answers: &BTreeMap<PathBuf, (Instant, Found)>,
    now: Instant,
) -> Vec<PathBuf> {
    let mut due: Vec<(Duration, Option<Instant>, PathBuf)> = listing
        .targets()
        .into_iter()
        .filter_map(|(path, kept)| {
            let asked = answers.get(&path).map(|(at, _)| *at);
            asked
                .is_none_or(|at| now.saturating_duration_since(at) >= kept)
                .then_some((kept, asked, path))
        })
        .collect();
    due.sort_by_key(|(kept, asked, _)| (*kept, *asked));
    due.into_iter()
        .take(PROBES_A_TICK)
        .map(|(_, _, path)| path)
        .collect()
}

/// The page's probe: its last reading and when it last ticked. One a host
/// ([`crate::vitals_host::VitalsHost`]).
pub(crate) struct ScopeSlot {
    port: ScopePort,
    /// The folder the person's pause record lives in (aterm's state folder; none in a test's
    /// host unless the test gives one).
    pause_dir: Option<PathBuf>,
    state: Mutex<SlotState>,
}

impl Default for ScopeSlot {
    fn default() -> Self {
        Self::new(ScopePort::host())
    }
}

impl ScopeSlot {
    pub(crate) fn new(port: ScopePort) -> Self {
        Self {
            port,
            pause_dir: if cfg!(test) {
                None
            } else {
                aterm_types::dirs::state_dir()
            },
            state: Mutex::default(),
        }
    }

    /// The same slot reading its pause record in `dir`.
    #[cfg(all(test, target_os = "macos"))]
    #[must_use]
    pub(crate) fn with_pause_dir(mut self, dir: PathBuf) -> Self {
        self.pause_dir = Some(dir);
        self
    }

    /// Note the data volume's last reading (whether indexing read enabled, and when its run
    /// started, unix ms), which the next tick judges the pause by.
    pub(crate) fn note_volume(&self, volume: Option<(bool, i64)>) {
        self.lock().volume = volume;
    }

    /// Read again at the next service, whatever the cadence: one of the document's root steps
    /// ended, and the folders kept out or the pause may have moved.
    #[cfg(target_os = "macos")]
    pub(crate) fn relist_now(&self) {
        self.lock().ticked = None;
    }

    /// Say what the last press of the document's buttons did.
    pub(crate) fn say(&self, words: String) {
        self.lock().said = Some(words);
    }

    /// What the last press of the document's buttons did.
    pub(crate) fn said(&self) -> Option<String> {
        self.lock().said.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SlotState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The last reading and its revision, which moves with each tick's listing and answers.
    pub(crate) fn reading(&self) -> Option<(Arc<ScopeReading>, u64)> {
        let state = self.lock();
        state.reading.clone().map(|reading| (reading, state.rev))
    }

    /// At `now`, with the page `shown` on screen: tick where none ran within [`PROBE_EVERY`] and
    /// none is running. A tick re-reads the listing, publishes it, then probes the folders
    /// [`due`] picks and publishes their answers, on a background thread (inline in a test).
    /// Nothing while the page is not shown. Whether a tick started.
    pub(crate) fn service(self: &Arc<Self>, shown: bool, now: Instant) -> bool {
        if !shown {
            return false;
        }
        {
            let mut state = self.lock();
            if state.running
                || state
                    .ticked
                    .is_some_and(|at| now.saturating_duration_since(at) < PROBE_EVERY)
            {
                return false;
            }
            state.ticked = Some(now);
            state.running = true;
        }
        let slot = Arc::clone(self);
        let tick = move || slot.tick(now);
        if cfg!(test) {
            tick();
            return true;
        }
        match crate::qos::spawn_lane_thread(
            "resources-spotlight-scope",
            crate::qos::Role::Background,
            tick,
        ) {
            Ok(_) => true,
            Err(error) => {
                aterm_log::warn!("System Resources: the Spotlight probe did not start: {error}");
                self.lock().running = false;
                false
            }
        }
    }

    fn tick(&self, now: Instant) {
        let mut listing = (self.port.list)();
        // The person's pause, read here and ended here once a later reading says how indexing
        // stands: never on the App's thread.
        if let Some(dir) = self.pause_dir.as_deref() {
            let record = spotlight_scope::read_pause(dir);
            let volume = self.lock().volume;
            match spotlight_scope::pause(record.as_ref(), volume) {
                (_, true) => {
                    if let Some(record) = &record {
                        spotlight_scope::end_pause(dir, record);
                    }
                }
                _ => listing.pause = record,
            }
        }
        let due = {
            let mut state = self.lock();
            let due = due(&listing, &state.answers, now);
            state.publish(listing.clone());
            due
        };
        let probed: Vec<(PathBuf, Found)> = due
            .into_iter()
            .map(|path| {
                let found = (self.port.found)(&path);
                (path, found)
            })
            .collect();
        let mut state = self.lock();
        for (path, found) in probed {
            state.answers.insert(path, (now, found));
        }
        state.publish(listing);
        state.running = false;
    }
}

#[cfg(test)]
#[path = "resources_spotlight_tests.rs"]
pub(crate) mod tests;
