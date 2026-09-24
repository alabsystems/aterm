// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Discover the next published index with bounded HEADs and a short Releases listing.
//!
//! The shipped index is a prerelease in the app's repository, so GitHub's
//! `/releases/latest` pointer deliberately names an app release. Its index tags are
//! normally consecutive, but the publisher permits a skipped number. HEAD requests
//! for the next two tags run at a thirty-second cadence; two more and one short
//! Releases listing each have independent five-minute store-scoped stamps. The
//! due-park network bound is ten seconds: the package worker checks the near
//! pair while two scoped helpers check the far pair and listing, never on the
//! GUI thread. A
//! published lower tag cannot hide a newer one, and an API rate limit cannot
//! stall HEAD discovery. A hit wakes the ordinary update
//! pass; only that pass downloads and verifies the roster, index, manifests and
//! artifacts. The machine-wide six-hour walk
//! (`aterm_update_core::pkg_check::FULL_PASS_INTERVAL_SECS`) remains the fallback if
//! the hint is absent, unavailable or pushed off the listing's first page.
//!
//! Only GitHub's own answer for a published asset — a 302 or 307 to an https URL on its
//! release-asset storage ([`RELEASE_ASSET_HOSTS`]) — reads as published ([`classify`]).
//! A 200, a permanent redirect (a renamed repo answers 301 for every path), or a
//! redirect anywhere else (a captive portal, a proxy's login page) says nothing, so an
//! intermediary can never wake a full pass at every cooldown.

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Seek as _, Write as _};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use aterm_update_core::{HeadAnswer, HttpError};

use crate::store::Layout;

type ListingFetch<'a> = dyn FnMut(&str) -> Result<Vec<u8>, HttpError> + 'a;
type ConcurrentListingFetch<'a> = dyn FnMut(&str) -> Result<Vec<u8>, HttpError> + Send + 'a;

/// The per-process check cadence; a shared stamp also enforces it across processes.
pub const INTERVAL: Duration = Duration::from_secs(30);
const LOOKAHEAD_INTERVAL: Duration = Duration::from_secs(5 * 60);
const RETRY_AFTER_ERROR: Duration = Duration::from_secs(5 * 60);
const RETRY_AFTER_RATE_LIMIT: Duration = Duration::from_secs(60 * 60);
const MAX_STAMP_BYTES: u64 = 64;
/// The store-scoped lock and stamp files of the two HEAD ranges and the listing.
pub(crate) const NEAR_LOCK: &str = "index-probe.lock";
pub(crate) const LOOKAHEAD_LOCK: &str = "index-probe-lookahead.lock";
pub(crate) const LISTING_LOCK: &str = "index-probe-listing.lock";

/// Where the release host redirects a published asset: GitHub's release-asset storage
/// (the hosts the vendor lane's `SHA256SUMS` pin admits for the same kind of redirect).
pub const RELEASE_ASSET_HOSTS: &[&str] = &[
    "release-assets.githubusercontent.com",
    "objects.githubusercontent.com",
];

/// The probe's only actionable answer. `Published` is an untrusted wake hint, not
/// an index verdict; the usual signed update pass decides what may be installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    Published(u64),
    Missing,
    Deferred,
}

/// Probe the shipped public index source, if this store has a real directory and
/// its registry has not been overridden. Other sources retain the ordinary full
/// update cadence; the public browser-download URL cannot speak for a private or
/// local registry.
pub fn successor(layout: &Layout) -> Probe {
    successor_with_near_hint(layout, &mut |_| {})
}

/// The same bounded probe, with an early untrusted hint when the near HEAD
/// range finds a published index. The calling worker still owns and joins the
/// far HEAD and listing helpers before it returns the highest answer. A GUI
/// package lane can start its independent signed update during that wait.
pub fn successor_reporting_near(layout: &Layout, near_build: &AtomicU64) -> Probe {
    successor_with_near_hint(layout, &mut |build| {
        near_build.fetch_max(build, Ordering::Release);
    })
}

fn successor_with_near_hint(layout: &Layout, on_near: &mut dyn FnMut(u64)) -> Probe {
    if !probes_this_source() {
        return Probe::Deferred;
    }
    let source = crate::discovery::resolve_account(crate::config::cached().account());
    let repo = crate::discovery::index_repo();
    if !standard_public_source(&source.owner, &repo) {
        return Probe::Deferred;
    }
    successor_concurrently_notifying_with(
        layout,
        &source.owner,
        &repo,
        SystemTime::now(),
        &mut aterm_update_core::head_no_redirect_quick,
        &mut aterm_update_core::head_no_redirect_quick,
        &mut aterm_update_core::api_get_classified_quick,
        on_near,
    )
}

/// The highest index build this store has VERIFIED — the durable rollback floor the
/// probe looks past. What the window reads to know whether a published index landed.
#[must_use]
pub fn verified_floor(layout: &Layout) -> u64 {
    crate::sig::Floor::new(layout.floor()).current()
}

fn standard_public_source(owner: &str, repo: &str) -> bool {
    owner == aterm_update_core::ATPKG_INDEX_OWNER && repo == aterm_update_core::DEFAULT_REPO
}

/// One floor and clock snapshot feeds all three independent probe ranges. A
/// positive near HEAD can wake the package lane before the worker gathers the
/// highest answer; a slow listing or second HEAD cannot hold that wake. At
/// most three groups are live for one due probe, including the calling worker;
/// each range still owns its store-scoped lock and stamp policy.
#[cfg(test)]
fn successor_concurrently_with(
    layout: &Layout,
    owner: &str,
    repo: &str,
    now: SystemTime,
    near_head: &mut (dyn FnMut(&str) -> Result<HeadAnswer, HttpError> + Send),
    far_head: &mut (dyn FnMut(&str) -> Result<HeadAnswer, HttpError> + Send),
    list: &mut ConcurrentListingFetch<'_>,
) -> Probe {
    successor_concurrently_notifying_with(
        layout,
        owner,
        repo,
        now,
        near_head,
        far_head,
        list,
        &mut |_| {},
    )
}

#[allow(clippy::too_many_arguments)]
fn successor_concurrently_notifying_with(
    layout: &Layout,
    owner: &str,
    repo: &str,
    now: SystemTime,
    near_head: &mut (dyn FnMut(&str) -> Result<HeadAnswer, HttpError> + Send),
    far_head: &mut (dyn FnMut(&str) -> Result<HeadAnswer, HttpError> + Send),
    list: &mut ConcurrentListingFetch<'_>,
    on_near: &mut dyn FnMut(u64),
) -> Probe {
    let Some(floor) = probe_floor(layout) else {
        return Probe::Deferred;
    };
    // The common all-stamped case needs no worker at all. Recheck each lock
    // through the ordinary path to keep a racing stamp/floor change honest.
    if all_stamps_look_fresh(layout, floor, now) {
        let near = probe_range_with_hint(
            layout,
            owner,
            repo,
            floor,
            now,
            near_head,
            &[1, 2],
            NEAR_LOCK,
            INTERVAL,
            on_near,
        );
        return highest_or_deferred([
            near,
            probe_range(
                layout,
                owner,
                repo,
                floor,
                now,
                far_head,
                &[3, 4],
                LOOKAHEAD_LOCK,
                LOOKAHEAD_INTERVAL,
            ),
            probe_listing(layout, owner, repo, floor, now, list),
        ]);
    }
    std::thread::scope(|scope| {
        let far = std::thread::Builder::new()
            .name("atpkg-index-far".into())
            .spawn_scoped(scope, || {
                probe_range(
                    layout,
                    owner,
                    repo,
                    floor,
                    now,
                    far_head,
                    &[3, 4],
                    LOOKAHEAD_LOCK,
                    LOOKAHEAD_INTERVAL,
                )
            });
        let listing = std::thread::Builder::new()
            .name("atpkg-index-listing".into())
            .spawn_scoped(scope, || {
                probe_listing(layout, owner, repo, floor, now, list)
            });
        let near = probe_range_with_hint(
            layout,
            owner,
            repo,
            floor,
            now,
            near_head,
            &[1, 2],
            NEAR_LOCK,
            INTERVAL,
            on_near,
        );
        // A resource-starved host can refuse to start an optional hint worker.
        // The signed full pass and next probe still run; no background worker
        // dies merely because one parallel discovery hint could not be made.
        highest_or_deferred([
            near,
            far.ok().map_or(Probe::Deferred, |worker| {
                worker.join().unwrap_or(Probe::Deferred)
            }),
            listing.ok().map_or(Probe::Deferred, |worker| {
                worker.join().unwrap_or(Probe::Deferred)
            }),
        ])
    })
}

fn all_stamps_look_fresh(layout: &Layout, floor: u64, now: SystemTime) -> bool {
    [
        (NEAR_LOCK, INTERVAL),
        (LOOKAHEAD_LOCK, LOOKAHEAD_INTERVAL),
        (LISTING_LOCK, LOOKAHEAD_INTERVAL),
    ]
    .into_iter()
    .all(|(name, interval)| {
        open_probe_lock(&layout.prefix.join(name))
            .is_some_and(|mut file| stamped_recently(&mut file, floor, now, interval, interval))
    })
}

fn probe_floor(layout: &Layout) -> Option<u64> {
    let floor = verified_floor(layout);
    // The prefix was created by the launch seed. A missing or linked prefix is
    // not made writable by a read-side hint; the full update owns that repair.
    std::fs::symlink_metadata(&layout.prefix)
        .is_ok_and(|m| m.file_type().is_dir() && !crate::platform::is_reparse(&m))
        .then_some(floor)
}

/// One store-scoped probe. The HEAD closure is injectable so the URL, response
/// classification and cross-process request bound are tested without network.
#[cfg(test)]
pub(crate) fn successor_with(
    layout: &Layout,
    owner: &str,
    repo: &str,
    now: SystemTime,
    head: &mut dyn FnMut(&str) -> Result<HeadAnswer, HttpError>,
    list: &mut ListingFetch<'_>,
) -> Probe {
    let Some(floor) = probe_floor(layout) else {
        return Probe::Deferred;
    };
    let near = probe_range(
        layout,
        owner,
        repo,
        floor,
        now,
        head,
        &[1, 2],
        NEAR_LOCK,
        INTERVAL,
    );
    let lookahead = probe_range(
        layout,
        owner,
        repo,
        floor,
        now,
        head,
        &[3, 4],
        LOOKAHEAD_LOCK,
        LOOKAHEAD_INTERVAL,
    );
    let listing = probe_listing(layout, owner, repo, floor, now, list);
    highest_or_deferred([near, lookahead, listing])
}

/// The hint is actionable if any independent range found a published build.
/// A negative answer is trustworthy only when ALL three ranges actually ran
/// and found nothing: a fresh cross-process stamp is `Deferred`, not absence.
fn highest_or_deferred(probes: [Probe; 3]) -> Probe {
    let highest = probes
        .iter()
        .filter_map(|probe| match probe {
            Probe::Published(build) => Some(*build),
            Probe::Missing | Probe::Deferred => None,
        })
        .max();
    match highest {
        Some(build) => Probe::Published(build),
        None if probes.iter().all(|probe| *probe == Probe::Missing) => Probe::Missing,
        None => Probe::Deferred,
    }
}

#[allow(clippy::too_many_arguments)]
fn probe_range(
    layout: &Layout,
    owner: &str,
    repo: &str,
    floor: u64,
    now: SystemTime,
    head: &mut dyn FnMut(&str) -> Result<HeadAnswer, HttpError>,
    offsets: &[u64],
    lock_name: &str,
    missing_interval: Duration,
) -> Probe {
    probe_range_with_hint(
        layout,
        owner,
        repo,
        floor,
        now,
        head,
        offsets,
        lock_name,
        missing_interval,
        &mut |_| {},
    )
}

#[allow(clippy::too_many_arguments)]
fn probe_range_with_hint(
    layout: &Layout,
    owner: &str,
    repo: &str,
    floor: u64,
    now: SystemTime,
    head: &mut dyn FnMut(&str) -> Result<HeadAnswer, HttpError>,
    offsets: &[u64],
    lock_name: &str,
    missing_interval: Duration,
    on_published: &mut dyn FnMut(u64),
) -> Probe {
    let Some(mut file) = take_range_lock(&layout.prefix.join(lock_name)) else {
        return Probe::Deferred;
    };
    if stamped_recently(&mut file, floor, now, missing_interval, missing_interval) {
        return Probe::Deferred;
    }
    let mut answer = Probe::Missing;
    for offset in offsets {
        let Some(build) = floor.checked_add(*offset) else {
            if answer == Probe::Missing {
                answer = Probe::Deferred;
            }
            continue;
        };
        let tag = format!("atpkg-index-{build}");
        let Some(url) =
            aterm_update_core::cdn::release_download_url(owner, repo, &tag, "index.toml")
        else {
            if answer == Probe::Missing {
                answer = Probe::Deferred;
            }
            continue;
        };
        let next = classify(build, head(&url));
        if let Probe::Published(build) = next {
            // The release is only a wake hint. Report it before the next HEAD
            // or unrelated helper waits; the signed pass resolves authority.
            on_published(build);
        }
        answer = highest_or_deferred_two(answer, next);
    }
    // A crash or write failure merely makes the next process retry. The lock
    // remains held until this function returns, including across the HEAD.
    let marker = match answer {
        Probe::Published(_) => 'P',
        Probe::Missing => 'M',
        Probe::Deferred => 'E',
    };
    let _ = stamp(&mut file, floor, marker);
    answer
}

fn highest_or_deferred_two(a: Probe, b: Probe) -> Probe {
    match (a, b) {
        (Probe::Published(a), Probe::Published(b)) => Probe::Published(a.max(b)),
        (Probe::Published(a), _) | (_, Probe::Published(a)) => Probe::Published(a),
        (Probe::Missing, Probe::Missing) => Probe::Missing,
        _ => Probe::Deferred,
    }
}

fn probe_listing(
    layout: &Layout,
    owner: &str,
    repo: &str,
    floor: u64,
    now: SystemTime,
    list: &mut ListingFetch<'_>,
) -> Probe {
    let Some(mut file) = take_range_lock(&layout.prefix.join(LISTING_LOCK)) else {
        return Probe::Deferred;
    };
    if stamped_recently(
        &mut file,
        floor,
        now,
        LOOKAHEAD_INTERVAL,
        LOOKAHEAD_INTERVAL,
    ) {
        return Probe::Deferred;
    }
    let (answer, marker) = match listing_hint(owner, repo, floor, list) {
        Ok(answer @ Probe::Published(_)) => (answer, 'P'),
        Ok(Probe::Missing) => (Probe::Missing, 'M'),
        Ok(Probe::Deferred) => (Probe::Deferred, 'E'),
        Err(HttpError::RateLimited { .. }) => (Probe::Deferred, 'R'),
        Err(_) => (Probe::Deferred, 'E'),
    };
    let _ = stamp(&mut file, floor, marker);
    answer
}

/// The API listing is an untrusted change hint. Require the same four asset
/// names the signed pass can actually use, and only wake for an index above the
/// durable floor. A page may omit the newest index; absence is never authority.
fn listing_hint(
    owner: &str,
    repo: &str,
    floor: u64,
    list: &mut ListingFetch<'_>,
) -> Result<Probe, HttpError> {
    if !aterm_update_core::is_valid_slug(owner) || !aterm_update_core::is_valid_slug(repo) {
        return Ok(Probe::Deferred);
    }
    let url = format!("https://api.github.com/repos/{owner}/{repo}/releases?per_page=100&page=1");
    let body = list(&url)?;
    let Ok(releases) = crate::net::parse_releases(&body) else {
        return Ok(Probe::Deferred);
    };
    let newest = releases
        .iter()
        .filter(|release| {
            crate::net::find_pair(&release.assets, "index.toml").is_some()
                && crate::net::find_pair(&release.assets, aterm_update_core::roster::ROSTER_ASSET)
                    .is_some()
        })
        .filter_map(|release| {
            let digits = release.tag_name.strip_prefix("atpkg-index-")?;
            if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            digits.parse::<u64>().ok()
        })
        .filter(|build| *build > floor)
        .max();
    Ok(newest.map_or(Probe::Missing, Probe::Published))
}

/// Take a range's lock without waiting — the ONE way [`probe_range`] takes it. `None` when
/// the file cannot be opened as a regular file or another probe holds it.
///
/// A [`crate::lock::Flock`], never a bare `File`: the lock is released by `LOCK_UN` on
/// every return, so the next probe finds it free at once. Released by the close alone, a
/// child another thread is mid-spawning keeps a copy of the descriptor until its exec, and
/// the next probe reads the range as a sibling's and defers — the full-suite flake of
/// 2026-09-23. Pinned by `a_range_lock_is_free_the_moment_its_probe_releases_it`.
fn take_range_lock(path: &Path) -> Option<crate::lock::Flock> {
    crate::lock::Flock::try_lock(open_probe_lock(path)?).ok()
}

fn open_probe_lock(path: &Path) -> Option<File> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.file_type().is_file() || crate::platform::is_reparse(&metadata) {
        return None;
    }
    Some(file)
}

fn stamped_recently(
    file: &mut File,
    floor: u64,
    now: SystemTime,
    missing_interval: Duration,
    published_interval: Duration,
) -> bool {
    let Some((recorded, marker, modified)) = read_stamp(file) else {
        return false;
    };
    if recorded != floor {
        return false;
    }
    let lifetime = match marker {
        'M' => missing_interval,
        'P' => published_interval,
        'E' => RETRY_AFTER_ERROR,
        'R' => RETRY_AFTER_RATE_LIMIT,
        _ => return false,
    };
    match now.duration_since(modified) {
        Ok(age) => age < lifetime,
        // A small clock correction still honours the stamp. A far-future mtime
        // is treated as damaged state so it cannot suppress the probe forever.
        Err(_) => modified
            .duration_since(now)
            .is_ok_and(|ahead| ahead < lifetime),
    }
}

/// The ONE parser of a range's stamp — `"{floor} {marker}\n"` plus the file's mtime, the
/// instant the answer was written — shared by the cooldown ([`stamped_recently`]) and the
/// report ([`cached_answer`]), so what the doctor shows is exactly what the probe reads.
/// Anything else (oversized, a third field, an unknown marker, a non-number) is `None`.
fn read_stamp(file: &mut File) -> Option<(u64, char, SystemTime)> {
    let metadata = file.metadata().ok()?;
    if metadata.len() > MAX_STAMP_BYTES {
        return None;
    }
    let mut text = String::new();
    file.seek(std::io::SeekFrom::Start(0)).ok()?;
    file.take(MAX_STAMP_BYTES).read_to_string(&mut text).ok()?;
    let mut fields = text.split_ascii_whitespace();
    let (Some(recorded), Some(marker), None) = (fields.next(), fields.next(), fields.next()) else {
        return None;
    };
    let marker = match marker {
        "M" => 'M',
        "P" => 'P',
        "E" => 'E',
        "R" => 'R',
        _ => return None,
    };
    Some((recorded.parse().ok()?, marker, metadata.modified().ok()?))
}

/// What the probe last answered for one range, as its stamp records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeAnswer {
    /// A release-asset HEAD or Releases listing found a newer index (an untrusted hint;
    /// no signed pass has run).
    Published,
    /// Nothing newer in this range.
    Missing,
    /// No usable answer: a failed request, or one that says nothing ([`classify`] — a 200,
    /// a permanent redirect, a redirect off the release-asset hosts) or an unparseable
    /// listing. Every one is stamped `E`; none is "nothing newer".
    Failed,
    /// The Releases listing was rate-limited.
    RateLimited,
}

/// The probe's last answers for the store's CURRENT floor — what the window's
/// thirty-second cadence (or any process that ran [`successor`]) already learned,
/// read back without a request, a lock or a write. `near` is the next two tags,
/// `lookahead` the two after them,
/// and `listing` the Releases listing. Each is `(answer, when it was written)`, and is `None` when
/// that range has no stamp for this floor — never probed, or probed before the floor last
/// moved (a stamp recorded under an older floor answers a question nobody is asking now).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CachedProbe {
    pub floor: u64,
    pub near: Option<(RangeAnswer, SystemTime)>,
    pub lookahead: Option<(RangeAnswer, SystemTime)>,
    pub listing: Option<(RangeAnswer, SystemTime)>,
}

/// Read the probe's stamps back (see [`CachedProbe`]). Offline and side-effect free: the
/// lock files are opened read-only, never created, never locked — a stamp being rewritten
/// at this instant reads as torn, and a torn stamp is simply absent.
#[must_use]
pub fn cached_answer(layout: &Layout) -> CachedProbe {
    let floor = verified_floor(layout);
    let read = |name: &str| {
        let mut file = open_stamp_read_only(&layout.prefix.join(name))?;
        let (recorded, marker, written) = read_stamp(&mut file)?;
        (recorded == floor).then_some(())?;
        let answer = match marker {
            'P' => RangeAnswer::Published,
            'M' => RangeAnswer::Missing,
            'E' => RangeAnswer::Failed,
            _ => RangeAnswer::RateLimited,
        };
        Some((answer, written))
    };
    CachedProbe {
        floor,
        near: read(NEAR_LOCK),
        lookahead: read(LOOKAHEAD_LOCK),
        listing: read(LISTING_LOCK),
    }
}

/// The index builds of the signed candidates the last pass that REACHED the listing
/// downloaded for the source [`successor`] watches — the labels of `<prefix>/index-cache.toml`
/// ([`crate::cache::IndexCache::labels`]), newest first. That pass writes the cache from the
/// listing's newest complete quads BEFORE it verifies and selects, so a build here above the
/// floor was downloaded and not landed. `None` when there is no readable cache for this
/// source. Read-only, offline; diagnostics, never an input to any trust decision.
#[must_use]
pub fn held_index_builds(layout: &Layout) -> Option<Vec<u64>> {
    let source = crate::net::github_source_id(aterm_update_core::ATPKG_INDEX_OWNER);
    let labels = crate::cache::IndexCache::for_layout(layout).labels(&source)?;
    Some(
        labels
            .iter()
            .filter_map(|label| {
                let digits = label.strip_prefix("atpkg-index-")?;
                if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                    return None;
                }
                digits.parse::<u64>().ok()
            })
            .collect(),
    )
}

/// Whether [`successor`] probes this store's index source at all — the manager armed, no
/// registry seam, the shipped public source. For any other source there is no probe and so
/// nothing cached to report.
#[must_use]
pub fn probes_this_source() -> bool {
    if !crate::enabled() || crate::cli::registry_seam().is_some() {
        return false;
    }
    let source = crate::discovery::resolve_account(crate::config::cached().account());
    standard_public_source(&source.owner, &crate::discovery::index_repo())
}

fn open_stamp_read_only(path: &Path) -> Option<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.file_type().is_file() || crate::platform::is_reparse(&metadata) {
        return None;
    }
    Some(file)
}

fn stamp(file: &mut File, floor: u64, marker: char) -> std::io::Result<()> {
    file.set_len(0)?;
    file.seek(std::io::SeekFrom::Start(0))?;
    writeln!(file, "{floor} {marker}")
}

/// One redirect-refusing HEAD's answer for index `next`: a 302 or 307 to an https URL on
/// the release-asset storage ([`RELEASE_ASSET_HOSTS`]) is the asset, a 404 its absence,
/// and everything else — a 200 (GitHub never serves the asset itself, so that is an
/// intermediary's page), a permanent redirect, a redirect anywhere else, a transport
/// failure — says nothing. Found in review of Phase 3's parallel probe (p3/sched,
/// 2026-09-22) and true of this one until 2026-09-23: any 200 or 3xx read as a
/// publication, so a captive portal or a filtering proxy that answers every URL woke a
/// signed pass at every five-minute published cooldown, for good.
fn classify(next: u64, answer: Result<HeadAnswer, HttpError>) -> Probe {
    match answer {
        Ok(HeadAnswer {
            code: 302 | 307,
            location: Some(location),
        }) if crate::vendor::https_host(&location)
            .is_some_and(|host| RELEASE_ASSET_HOSTS.contains(&host)) =>
        {
            Probe::Published(next)
        }
        Ok(HeadAnswer { code: 404, .. }) => Probe::Missing,
        _ => Probe::Deferred,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_spec::derive::{
        Model, atpkg_index_probe_cooldown_model, atpkg_index_successor_selection_model,
    };

    use std::cell::Cell;
    use std::collections::BTreeMap;
    use std::time::Instant;

    /// A concurrently spawned child can briefly retain an inherited advisory
    /// lock after this test's previous file handle has closed. The production
    /// probe correctly defers that attempt; give the test a bounded chance to
    /// observe the next eligible attempt before judging its wire budget.
    const TRANSIENT_LOCK_GRACE: Duration = Duration::from_secs(10);

    struct ModeledCase {
        label: &'static str,
        action: &'static str,
        code: u16,
        location: Option<&'static str>,
        marker: char,
        cooldown_ticks: u32,
        answer: Probe,
    }

    fn modeled_range_attempt(
        model: &Model,
        state: &mut BTreeMap<&'static str, i64>,
        layout: &Layout,
        at: SystemTime,
        case: &ModeledCase,
        should_request: bool,
    ) {
        assert!(model.fire("Acquire", state));
        let admitted = model.action_enabled(case.action, state);
        assert_eq!(admitted, should_request, "{} model guard", case.label);
        if !admitted {
            // The historical no-stamp-check mutant would spend another range
            // request here. Real code must agree with the healthy guard.
            let buggy = aterm_spec::interp::with_buggy(model, 1);
            assert!(buggy.action_enabled(case.action, state));
        }
        let floor = crate::sig::Floor::new(layout.floor()).current();
        let requests = Cell::new(0);
        let mut head = |url: &str| {
            requests.set(requests.get() + 1);
            assert!(url.ends_with(&format!("/atpkg-index-{}/index.toml", floor + 1)));
            Ok(HeadAnswer {
                code: case.code,
                location: case.location.map(str::to_string),
            })
        };
        let started = Instant::now();
        let observed = loop {
            let observed = probe_range(
                layout,
                "alabsystems",
                "aterm",
                floor,
                at,
                &mut head,
                &[1],
                NEAR_LOCK,
                INTERVAL,
            );
            if !admitted || requests.get() != 0 || started.elapsed() >= TRANSIENT_LOCK_GRACE {
                break observed;
            }
            assert_eq!(observed, Probe::Deferred, "{} lock deferral", case.label);
            assert!(
                lock_still_due(layout, NEAR_LOCK, floor, at, INTERVAL),
                "{} must not retry a fresh stamp",
                case.label
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(
            requests.get(),
            if admitted { 1 } else { 0 },
            "{} wire budget",
            case.label
        );
        if admitted {
            assert_eq!(observed, case.answer);
            assert!(model.fire(case.action, state));
            assert_eq!(
                std::fs::read_to_string(layout.prefix.join(NEAR_LOCK)).unwrap(),
                format!("{floor} {}\n", case.marker),
                "the real writer must publish the marker the next reader uses"
            );
            stamped_at(layout, &[NEAR_LOCK], at);
        } else {
            assert_eq!(observed, Probe::Deferred);
            assert!(model.fire("SkipFresh", state));
        }
        assert!(model.fire("Release", state));
        assert!(model.check_invariant("NoDuplicateRangeInsideCooldown", state));
    }

    fn layout(name: &str) -> Layout {
        let prefix =
            std::env::temp_dir().join(format!("atpkg-index-probe-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&prefix);
        std::fs::create_dir_all(&prefix).unwrap();
        Layout { prefix }
    }

    /// A stamp's age is read from its file's REAL mtime, and these tests drive the probe on
    /// a synthetic clock from `t0`: so the stamps a probe just wrote are pinned to the
    /// synthetic instant they were written at, and the clock's ticks are their age
    /// exactly. Unpinned, a loaded machine that stalled the test for more than a case's
    /// margin between `t0` and the write left the stamp younger than the clock said — the
    /// cooldown "over" at `t0 + 70 s` still ran — and the derived-model conformance went red
    /// at random (found in review of p3/reconciled, 2026-09-23).
    fn stamped_at(layout: &Layout, locks: &[&str], at: SystemTime) {
        for lock in locks {
            File::options()
                .write(true)
                .open(layout.prefix.join(lock))
                .and_then(|file| file.set_modified(at))
                .unwrap();
        }
    }

    /// Distinguish a stamp rewritten by this attempt from an existing stamp that
    /// another range merely read. The synthetic clock in these tests must apply
    /// only to writers; advancing an untouched range would hide a due probe.
    fn stamp_snapshot(layout: &Layout, lock: &str) -> Option<(SystemTime, String)> {
        let path = layout.prefix.join(lock);
        let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
        let contents = std::fs::read_to_string(path).ok()?;
        (!contents.is_empty()).then_some((modified, contents))
    }

    fn lock_still_due(
        layout: &Layout,
        lock: &str,
        floor: u64,
        at: SystemTime,
        interval: Duration,
    ) -> bool {
        let Some(mut file) = open_probe_lock(&layout.prefix.join(lock)) else {
            // An unreadable lock path also makes the real probe defer before
            // reaching the network; it has not recorded a fresh stamp.
            return true;
        };
        !stamped_recently(&mut file, floor, at, interval, interval)
    }

    /// Each independent range may defer once while its advisory lock is briefly
    /// unavailable. Revisit at the same model time, then require the exact wire
    /// budget: a persistent skip or a duplicate request still fails the test.
    fn successor_when_due(
        layout: &Layout,
        at: SystemTime,
        head: &mut dyn FnMut(&str) -> Result<HeadAnswer, HttpError>,
        list: &mut ListingFetch<'_>,
        wire_counts: (&Cell<usize>, &Cell<usize>),
        expected: Probe,
        budget: (usize, usize),
    ) {
        let started = Instant::now();
        let mut saw_expected = false;
        let locks = [NEAR_LOCK, LOOKAHEAD_LOCK, LISTING_LOCK];
        loop {
            let before = locks.map(|lock| stamp_snapshot(layout, lock));
            saw_expected |=
                successor_with(layout, "alabsystems", "aterm", at, head, list) == expected;
            let rewritten: Vec<_> = locks
                .into_iter()
                .zip(before)
                .filter_map(|(lock, prior)| {
                    let after = stamp_snapshot(layout, lock);
                    (after.is_some() && after != prior).then_some(lock)
                })
                .collect();
            stamped_at(layout, &rewritten, at);
            let used = (wire_counts.0.get(), wire_counts.1.get());
            assert!(
                used.0 <= budget.0 && used.1 <= budget.1,
                "wire budget overshot: {used:?} > {budget:?}"
            );
            if used == budget || started.elapsed() >= TRANSIENT_LOCK_GRACE {
                break;
            }
            let floor = verified_floor(layout);
            assert!(
                [
                    (NEAR_LOCK, INTERVAL),
                    (LOOKAHEAD_LOCK, LOOKAHEAD_INTERVAL),
                    (LISTING_LOCK, LOOKAHEAD_INTERVAL),
                ]
                .into_iter()
                .any(|(lock, interval)| lock_still_due(layout, lock, floor, at, interval)),
                "incomplete wire budget without an unstamped due lock"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(saw_expected, "the due probe must observe {expected:?}");
        assert_eq!(
            (wire_counts.0.get(), wire_counts.1.get()),
            budget,
            "wire budget"
        );
    }

    #[test]
    fn real_stamp_writer_reader_and_cooldowns_refine_the_derived_model() {
        let model = atpkg_index_probe_cooldown_model();
        let cases = [
            ModeledCase {
                label: "missing",
                action: "ProbeMissing",
                code: 404,
                location: None,
                marker: 'M',
                cooldown_ticks: 1,
                answer: Probe::Missing,
            },
            ModeledCase {
                label: "error",
                action: "ProbeError",
                code: 503,
                location: None,
                marker: 'E',
                cooldown_ticks: 10,
                answer: Probe::Deferred,
            },
            ModeledCase {
                label: "published",
                action: "ProbePublished",
                code: 302,
                location: Some("https://release-assets.githubusercontent.com/object"),
                marker: 'P',
                cooldown_ticks: 1,
                answer: Probe::Published(44),
            },
        ];
        for case in cases {
            let layout = layout(&format!("modeled-{}", case.label));
            std::fs::write(layout.floor(), "43").unwrap();
            let t0 = SystemTime::now();
            let mut state = model.init_state();
            modeled_range_attempt(&model, &mut state, &layout, t0, &case, true);
            modeled_range_attempt(
                &model,
                &mut state,
                &layout,
                t0 + INTERVAL * case.cooldown_ticks - Duration::from_secs(1),
                &case,
                false,
            );
            for _ in 0..case.cooldown_ticks {
                assert!(model.fire("AdvanceTick", &mut state));
            }
            modeled_range_attempt(
                &model,
                &mut state,
                &layout,
                t0 + INTERVAL * case.cooldown_ticks + Duration::from_secs(10),
                &case,
                true,
            );
            if case.marker == 'M' {
                std::fs::write(layout.floor(), "44").unwrap();
                assert!(model.fire("AdvanceFloor", &mut state));
                modeled_range_attempt(
                    &model,
                    &mut state,
                    &layout,
                    t0 + INTERVAL * case.cooldown_ticks + Duration::from_secs(11),
                    &case,
                    true,
                );
            }
            std::fs::remove_dir_all(layout.prefix).unwrap();
        }
    }

    #[test]
    fn next_index_wakes_once_and_shared_stamp_bounds_other_processes() {
        let layout = layout("successor");
        std::fs::write(layout.floor(), "43").unwrap();
        let t0 = SystemTime::now();
        let requests = Cell::new(0);
        let newer_ready = Cell::new(false);
        let mut head = |url: &str| {
            requests.set(requests.get() + 1);
            let published = url.ends_with("/atpkg-index-44/index.toml")
                || (newer_ready.get() && url.ends_with("/atpkg-index-45/index.toml"));
            Ok(HeadAnswer {
                code: if published { 302 } else { 404 },
                location: published
                    .then(|| "https://release-assets.githubusercontent.com/object".into()),
            })
        };
        let listings = Cell::new(0);
        let mut list = |_: &str| {
            listings.set(listings.get() + 1);
            Ok(b"[]".to_vec())
        };
        assert_eq!(
            successor_with(&layout, "alabsystems", "aterm", t0, &mut head, &mut list),
            Probe::Published(44)
        );
        assert_eq!((requests.get(), listings.get()), (4, 1));
        stamped_at(&layout, &[NEAR_LOCK, LOOKAHEAD_LOCK, LISTING_LOCK], t0);
        assert_eq!(
            successor_with(
                &layout,
                "alabsystems",
                "aterm",
                t0 + INTERVAL - Duration::from_secs(1),
                &mut head,
                &mut list,
            ),
            Probe::Deferred,
            "a second process reuses each shared stamp"
        );
        assert_eq!((requests.get(), listings.get()), (4, 1));
        newer_ready.set(true);
        assert_eq!(
            successor_with(
                &layout,
                "alabsystems",
                "aterm",
                t0 + INTERVAL + Duration::from_secs(1),
                &mut head,
                &mut list,
            ),
            Probe::Published(45),
            "a newer tag behind an unlanded 44 is found on the next near probe"
        );
        assert_eq!((requests.get(), listings.get()), (6, 1));
        stamped_at(
            &layout,
            &[NEAR_LOCK],
            t0 + INTERVAL + Duration::from_secs(1),
        );
        assert_eq!(
            successor_with(
                &layout,
                "alabsystems",
                "aterm",
                t0 + INTERVAL + Duration::from_secs(2),
                &mut head,
                &mut list,
            ),
            Probe::Deferred,
            "a third process does not reissue the just completed near range"
        );
        assert_eq!((requests.get(), listings.get()), (6, 1));
        std::fs::remove_dir_all(layout.prefix).unwrap();
    }

    #[test]
    fn real_head_and_listing_selection_refines_the_derived_model() {
        let model = atpkg_index_successor_selection_model();
        let layout = layout("selection-highest");
        std::fs::write(layout.floor(), "43").unwrap();
        let heads = Cell::new(0);
        let listings = Cell::new(0);
        let mut head = |url: &str| {
            heads.set(heads.get() + 1);
            let published = url.ends_with("/atpkg-index-44/index.toml")
                || url.ends_with("/atpkg-index-46/index.toml");
            Ok(HeadAnswer {
                code: if published { 302 } else { 404 },
                location: published
                    .then(|| "https://release-assets.githubusercontent.com/object".into()),
            })
        };
        let mut list = |_: &str| {
            listings.set(listings.get() + 1);
            Ok(listing_with_index(49))
        };
        let actual = successor_with(
            &layout,
            "alabsystems",
            "aterm",
            SystemTime::now(),
            &mut head,
            &mut list,
        );
        assert_eq!((heads.get(), listings.get()), (4, 1));
        let mut state = model.init_state();
        for action in ["ObserveLow", "ObserveMiddle", "ObserveHigh", "Choose"] {
            assert!(model.fire(action, &mut state), "{action}");
        }
        assert_eq!(state["chosen"], 5);
        assert_eq!(actual, Probe::Published(49));
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let mut mutant = buggy.init_state();
        for action in ["ObserveLow", "ObserveMiddle", "ObserveHigh", "Choose"] {
            assert!(buggy.fire(action, &mut mutant));
        }
        assert_eq!(mutant["chosen"], 3, "first-hit would hide index 49");
        std::fs::remove_dir_all(layout.prefix).unwrap();

        let partial_layout = self::layout("selection-partial");
        std::fs::write(partial_layout.floor(), "43").unwrap();
        let mut head = |_: &str| {
            Ok(HeadAnswer {
                code: 404,
                location: None,
            })
        };
        let mut list = |_: &str| Err(HttpError::Transport("offline".into()));
        let actual = successor_with(
            &partial_layout,
            "alabsystems",
            "aterm",
            SystemTime::now(),
            &mut head,
            &mut list,
        );
        let mut state = model.init_state();
        for action in [
            "ObserveMissing",
            "ObserveMissing",
            "ObserveDeferred",
            "Choose",
        ] {
            assert!(model.fire(action, &mut state), "{action}");
        }
        assert_eq!(state["chosen"], 2);
        assert_eq!(actual, Probe::Deferred);
        std::fs::remove_dir_all(partial_layout.prefix).unwrap();
    }

    #[test]
    fn due_probe_groups_overlap_and_still_choose_the_highest_build() {
        use std::sync::mpsc::{RecvTimeoutError, channel};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        let layout = layout("overlap-highest");
        std::fs::write(layout.floor(), "43").unwrap();
        let (entered_tx, entered_rx) = channel::<&'static str>();
        let (near_tx, near_rx) = channel::<()>();
        let (far_tx, far_rx) = channel::<()>();
        let (listing_tx, listing_rx) = channel::<()>();
        // Each group's first network call waits for this coordinator. A serial
        // probe can report only one entry before the bounded wait expires;
        // overlap reports all three without a wall-time performance assertion.
        let coordinator = std::thread::spawn(move || {
            let mut entered = Vec::new();
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            for _ in 0..3 {
                match entered_rx
                    .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                {
                    Ok(group) => entered.push(group),
                    Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
                }
            }
            for release in [near_tx, far_tx, listing_tx] {
                let _ = release.send(());
            }
            entered
        });
        let near_calls = Arc::new(AtomicUsize::new(0));
        let far_calls = Arc::new(AtomicUsize::new(0));
        let listing_calls = Arc::new(AtomicUsize::new(0));
        let near_counter = near_calls.clone();
        let far_counter = far_calls.clone();
        let listing_counter = listing_calls.clone();
        let near_entered = entered_tx.clone();
        let far_entered = entered_tx.clone();
        let mut near = move |url: &str| {
            if near_counter.fetch_add(1, Ordering::Relaxed) == 0 {
                near_entered.send("near").unwrap();
                near_rx.recv_timeout(Duration::from_secs(20)).unwrap();
            }
            let found = url.ends_with("/atpkg-index-44/index.toml");
            Ok(HeadAnswer {
                code: if found { 302 } else { 404 },
                location: found.then(|| "https://release-assets.githubusercontent.com/x".into()),
            })
        };
        let mut far = move |url: &str| {
            if far_counter.fetch_add(1, Ordering::Relaxed) == 0 {
                far_entered.send("far").unwrap();
                far_rx.recv_timeout(Duration::from_secs(20)).unwrap();
            }
            let found = url.ends_with("/atpkg-index-46/index.toml");
            Ok(HeadAnswer {
                code: if found { 302 } else { 404 },
                location: found.then(|| "https://release-assets.githubusercontent.com/x".into()),
            })
        };
        let mut list = move |_: &str| {
            listing_counter.fetch_add(1, Ordering::Relaxed);
            entered_tx.send("listing").unwrap();
            listing_rx.recv_timeout(Duration::from_secs(20)).unwrap();
            Ok(listing_with_index(49))
        };
        let found = successor_concurrently_with(
            &layout,
            "alabsystems",
            "aterm",
            SystemTime::now(),
            &mut near,
            &mut far,
            &mut list,
        );
        let mut entered = coordinator.join().unwrap();
        entered.sort_unstable();
        assert_eq!(entered, ["far", "listing", "near"]);
        assert_eq!(found, Probe::Published(49));
        assert_eq!(
            (
                near_calls.load(Ordering::Relaxed),
                far_calls.load(Ordering::Relaxed),
                listing_calls.load(Ordering::Relaxed),
            ),
            (2, 2, 1)
        );

        assert!(all_stamps_look_fresh(&layout, 43, SystemTime::now()));
        let mut never_near = |_: &str| panic!("fresh stamp must not do near HEADs");
        let mut never_far = |_: &str| panic!("fresh stamp must not do far HEADs");
        let mut never_list = |_: &str| panic!("fresh stamp must not do network listing");
        assert_eq!(
            successor_concurrently_with(
                &layout,
                "alabsystems",
                "aterm",
                SystemTime::now(),
                &mut never_near,
                &mut never_far,
                &mut never_list,
            ),
            Probe::Deferred
        );
        std::fs::remove_dir_all(layout.prefix).unwrap();
    }

    /// A near release is enough to wake the signed update. The worker still
    /// owns both unrelated helpers and eventually reports their highest build;
    /// neither their latency nor their completion is a prerequisite for the
    /// positive wake hint.
    #[test]
    fn near_published_is_reported_while_far_and_listing_remain_owned() {
        use std::sync::mpsc::channel;

        let layout = layout("near-early-owned");
        std::fs::write(layout.floor(), "43").unwrap();
        let (near_second_started_tx, near_second_started_rx) = channel();
        let (near_second_release_tx, near_second_release_rx) = channel();
        let (far_started_tx, far_started_rx) = channel();
        let (listing_started_tx, listing_started_rx) = channel();
        let (far_release_tx, far_release_rx) = channel();
        let (listing_release_tx, listing_release_rx) = channel();
        let (hint_tx, hint_rx) = channel();
        let worker = std::thread::spawn(move || {
            let mut near = move |url: &str| {
                if url.ends_with("/atpkg-index-45/index.toml") {
                    near_second_started_tx.send(()).unwrap();
                    near_second_release_rx
                        .recv_timeout(Duration::from_secs(30))
                        .unwrap();
                }
                let found = url.ends_with("/atpkg-index-44/index.toml");
                Ok(HeadAnswer {
                    code: if found { 302 } else { 404 },
                    location: found
                        .then(|| "https://release-assets.githubusercontent.com/near".into()),
                })
            };
            let mut far_first = true;
            let mut far = move |_: &str| {
                if std::mem::take(&mut far_first) {
                    far_started_tx.send(()).unwrap();
                    far_release_rx
                        .recv_timeout(Duration::from_secs(30))
                        .unwrap();
                }
                Ok(HeadAnswer {
                    code: 404,
                    location: None,
                })
            };
            let mut list = move |_: &str| {
                listing_started_tx.send(()).unwrap();
                listing_release_rx
                    .recv_timeout(Duration::from_secs(30))
                    .unwrap();
                Ok(listing_with_index(49))
            };
            let final_answer = successor_concurrently_notifying_with(
                &layout,
                "alabsystems",
                "aterm",
                SystemTime::now(),
                &mut near,
                &mut far,
                &mut list,
                &mut |build| hint_tx.send(build).unwrap(),
            );
            (final_answer, layout)
        });
        near_second_started_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        far_started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        listing_started_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert_eq!(hint_rx.recv_timeout(Duration::from_secs(5)), Ok(44));
        assert!(
            !worker.is_finished(),
            "the unrelated helpers are still owned"
        );
        near_second_release_tx.send(()).unwrap();
        far_release_tx.send(()).unwrap();
        listing_release_tx.send(()).unwrap();
        let (final_answer, layout) = worker.join().unwrap();
        assert_eq!(final_answer, Probe::Published(49));
        std::fs::remove_dir_all(layout.prefix).unwrap();
    }

    #[test]
    fn a_fresh_positive_stamp_does_not_turn_other_missing_ranges_into_global_missing() {
        let layout = layout("partial-stamp");
        std::fs::write(layout.floor(), "43").unwrap();
        std::fs::write(layout.prefix.join(NEAR_LOCK), "43 P\n").unwrap();
        let heads = Cell::new(0);
        let mut head = |_: &str| {
            heads.set(heads.get() + 1);
            Ok(HeadAnswer {
                code: 404,
                location: None,
            })
        };
        let mut list = |_: &str| Ok(b"[]".to_vec());
        assert_eq!(
            successor_with(
                &layout,
                "alabsystems",
                "aterm",
                SystemTime::now(),
                &mut head,
                &mut list,
            ),
            Probe::Deferred
        );
        assert_eq!(heads.get(), 2, "the far HEADs still run");
        assert_eq!(
            std::fs::read_to_string(layout.prefix.join(LOOKAHEAD_LOCK)).unwrap(),
            "43 M\n"
        );
        assert_eq!(
            std::fs::read_to_string(layout.prefix.join(LISTING_LOCK)).unwrap(),
            "43 M\n"
        );
        std::fs::remove_dir_all(layout.prefix).unwrap();
    }

    #[test]
    fn a_stamped_far_49_cannot_hide_new_near_45_or_later_listing_50() {
        let layout = layout("far-highwater-sequence");
        std::fs::write(layout.floor(), "43").unwrap();
        let t0 = SystemTime::now();
        let near_ready = Cell::new(false);
        let listed = Cell::new(49);
        let heads = Cell::new(0);
        let listings = Cell::new(0);
        let mut head = |url: &str| {
            heads.set(heads.get() + 1);
            let found = near_ready.get() && url.ends_with("/atpkg-index-45/index.toml");
            Ok(HeadAnswer {
                code: if found { 302 } else { 404 },
                location: found.then(|| "https://release-assets.githubusercontent.com/x".into()),
            })
        };
        let mut list = |_: &str| {
            listings.set(listings.get() + 1);
            Ok(listing_with_index(listed.get()))
        };
        successor_when_due(
            &layout,
            t0,
            &mut head,
            &mut list,
            (&heads, &listings),
            Probe::Published(49),
            (4, 1),
        );
        stamped_at(&layout, &[NEAR_LOCK, LOOKAHEAD_LOCK, LISTING_LOCK], t0);
        near_ready.set(true);
        successor_when_due(
            &layout,
            t0 + Duration::from_secs(61),
            &mut head,
            &mut list,
            (&heads, &listings),
            Probe::Published(45),
            (6, 1),
        );
        stamped_at(&layout, &[NEAR_LOCK], t0 + Duration::from_secs(61));
        listed.set(50);
        successor_when_due(
            &layout,
            t0 + Duration::from_secs(5 * 60 + 2),
            &mut head,
            &mut list,
            (&heads, &listings),
            Probe::Published(50),
            (10, 2),
        );
        std::fs::remove_dir_all(layout.prefix).unwrap();
    }

    #[test]
    fn listing_rate_limit_does_not_stall_far_head_discovery() {
        let layout = layout("rate-limit-head-independence");
        std::fs::write(layout.floor(), "43").unwrap();
        let t0 = SystemTime::now();
        let far_ready = Cell::new(false);
        let heads = Cell::new(0);
        let listings = Cell::new(0);
        let mut head = |url: &str| {
            heads.set(heads.get() + 1);
            let found = far_ready.get() && url.ends_with("/atpkg-index-47/index.toml");
            Ok(HeadAnswer {
                code: if found { 302 } else { 404 },
                location: found.then(|| "https://release-assets.githubusercontent.com/x".into()),
            })
        };
        let mut list = |url: &str| {
            listings.set(listings.get() + 1);
            Err(HttpError::RateLimited {
                code: 429,
                url: url.into(),
                authenticated: false,
            })
        };
        assert_eq!(
            successor_with(&layout, "alabsystems", "aterm", t0, &mut head, &mut list),
            Probe::Deferred
        );
        stamped_at(&layout, &[NEAR_LOCK, LOOKAHEAD_LOCK, LISTING_LOCK], t0);
        far_ready.set(true);
        assert_eq!(
            successor_with(
                &layout,
                "alabsystems",
                "aterm",
                t0 + Duration::from_secs(5 * 60 + 1),
                &mut head,
                &mut list,
            ),
            Probe::Published(47)
        );
        assert_eq!((heads.get(), listings.get()), (8, 1));
        assert_eq!(
            std::fs::read_to_string(layout.prefix.join(LISTING_LOCK)).unwrap(),
            "43 R\n"
        );
        std::fs::remove_dir_all(layout.prefix).unwrap();
    }

    #[test]
    fn a_sibling_probe_holding_the_store_lock_prevents_duplicate_heads() {
        let layout = layout("locked");
        std::fs::write(layout.floor(), "43").unwrap();
        let model = atpkg_index_probe_cooldown_model();
        let mut state = model.init_state();
        let held: Vec<_> = [NEAR_LOCK, LOOKAHEAD_LOCK, LISTING_LOCK]
            .into_iter()
            .map(|name| {
                let file = open_probe_lock(&layout.prefix.join(name)).unwrap();
                file.try_lock().unwrap();
                file
            })
            .collect();
        assert!(model.fire("Acquire", &mut state));
        assert!(!model.action_enabled("Acquire", &state));
        let mut head = |_: &str| -> Result<HeadAnswer, HttpError> {
            panic!("a sibling probe already owns the network request")
        };
        let mut list = |_: &str| -> Result<Vec<u8>, HttpError> {
            panic!("a sibling probe already owns the network request")
        };
        assert_eq!(
            successor_with(
                &layout,
                "alabsystems",
                "aterm",
                SystemTime::now(),
                &mut head,
                &mut list,
            ),
            Probe::Deferred
        );
        drop(held);
        assert!(model.fire("Abort", &mut state));
        assert!(model.action_enabled("Acquire", &state));
        std::fs::remove_dir_all(layout.prefix).unwrap();
    }

    /// A RANGE LOCK IS FREE THE MOMENT ITS PROBE RELEASES IT, whatever copies of its
    /// descriptor live on. `copy` stands in for a child another test thread is mid-spawning,
    /// which holds every descriptor until its exec. Were the lock released by the close
    /// alone, the copy would keep it held and the next probe would defer with nothing on the
    /// wire — the 2026-09-23 full-suite flake, where the derived-model conformance above read
    /// a wire budget of 0 for 1. A regression of [`take_range_lock`] to a bare `File` fails
    /// both assertions after the release. Negative control first: a lock actually held
    /// defers the probe, so the pass after the release is not vacuous.
    #[test]
    fn a_range_lock_is_free_the_moment_its_probe_releases_it() {
        let layout = layout("released");
        std::fs::write(layout.floor(), "46").unwrap();
        let path = layout.prefix.join(NEAR_LOCK);
        let floor = crate::sig::Floor::new(layout.floor()).current();
        let requests = Cell::new(0);
        let mut head = |_: &str| {
            requests.set(requests.get() + 1);
            Ok(HeadAnswer {
                code: 404,
                location: None,
            })
        };
        let probe = |head: &mut dyn FnMut(&str) -> Result<HeadAnswer, HttpError>| {
            probe_range(
                &layout,
                "alabsystems",
                "aterm",
                floor,
                SystemTime::now(),
                head,
                &[1],
                NEAR_LOCK,
                INTERVAL,
            )
        };

        let holder = take_range_lock(&path).expect("a free range lock is taken");
        assert!(
            take_range_lock(&path).is_none(),
            "a held range lock refuses"
        );
        assert_eq!(probe(&mut head), Probe::Deferred);
        assert_eq!(requests.get(), 0, "a held range spends no request");

        let copy = holder.try_clone().expect("a copy of the descriptor");
        // Released by `LOCK_UN` (the guard's drop) while the copy is still open.
        drop(holder);
        assert!(
            take_range_lock(&path).is_some(),
            "a released range lock must be free while a copy of its descriptor lives"
        );
        assert_eq!(probe(&mut head), Probe::Missing);
        assert_eq!(
            requests.get(),
            1,
            "the probe after a release goes to the wire"
        );
        drop(copy);
        std::fs::remove_dir_all(&layout.prefix).unwrap();
    }

    #[test]
    fn missing_retries_after_thirty_seconds_and_floor_change_bypasses_old_stamp() {
        assert_eq!(INTERVAL, Duration::from_secs(30));
        let layout = layout("missing");
        std::fs::write(layout.floor(), "44").unwrap();
        let t0 = SystemTime::now();
        let requests = Cell::new(0);
        let mut head = |url: &str| {
            requests.set(requests.get() + 1);
            assert!(url.contains("/atpkg-index-"));
            Ok(HeadAnswer {
                code: 404,
                location: None,
            })
        };
        let mut list = |_: &str| Ok(b"[]".to_vec());
        assert_eq!(
            successor_with(&layout, "alabsystems", "aterm", t0, &mut head, &mut list),
            Probe::Missing
        );
        stamped_at(&layout, &[NEAR_LOCK, LOOKAHEAD_LOCK, LISTING_LOCK], t0);
        assert_eq!(
            successor_with(
                &layout,
                "alabsystems",
                "aterm",
                t0 + INTERVAL - Duration::from_secs(1),
                &mut head,
                &mut list,
            ),
            Probe::Deferred
        );
        assert_eq!(
            successor_with(
                &layout,
                "alabsystems",
                "aterm",
                t0 + INTERVAL + Duration::from_secs(1),
                &mut head,
                &mut list,
            ),
            Probe::Deferred,
            "the near tags were retried, while the deeper scan stays cooled down"
        );
        assert_eq!(
            requests.get(),
            6,
            "four initial HEADs, then only two near HEADs"
        );
        std::fs::write(layout.floor(), "45").unwrap();
        assert_eq!(
            successor_with(
                &layout,
                "alabsystems",
                "aterm",
                t0 + INTERVAL + Duration::from_secs(2),
                &mut head,
                &mut list,
            ),
            Probe::Missing
        );
        assert_eq!(
            requests.get(),
            10,
            "a changed floor invalidates both stamps"
        );
        std::fs::remove_dir_all(layout.prefix).unwrap();
    }

    #[test]
    fn permitted_number_skips_are_found_without_the_six_hour_scan() {
        for (name, ready) in [("skip-one", 46), ("skip-two", 47)] {
            let layout = layout(name);
            std::fs::write(layout.floor(), "44").unwrap();
            let requests = Cell::new(0);
            let mut head = |url: &str| {
                requests.set(requests.get() + 1);
                let found = url.ends_with(&format!("/atpkg-index-{ready}/index.toml"));
                Ok(HeadAnswer {
                    code: if found { 302 } else { 404 },
                    location: found
                        .then(|| "https://release-assets.githubusercontent.com/object".into()),
                })
            };
            let mut list = |_: &str| Ok(b"[]".to_vec());
            assert_eq!(
                successor_with(
                    &layout,
                    "alabsystems",
                    "aterm",
                    SystemTime::now(),
                    &mut head,
                    &mut list,
                ),
                Probe::Published(ready)
            );
            assert_eq!(requests.get(), 4, "both bounded HEAD ranges are checked");
            std::fs::remove_dir_all(layout.prefix).unwrap();
        }
    }

    fn listing_with_index(build: u64) -> Vec<u8> {
        format!(
            r#"[{{"tag_name":"v0.90.0","assets":[]}},{{"tag_name":"atpkg-index-{build}","assets":[{{"name":"index.toml","url":"i"}},{{"name":"index.toml.sig","url":"is"}},{{"name":"aterm-machines.toml","url":"m"}},{{"name":"aterm-machines.toml.sig","url":"ms"}}]}}]"#
        )
        .into_bytes()
    }

    #[test]
    fn a_large_published_skip_wakes_with_one_shared_listing_per_five_minutes() {
        let layout = layout("large-skip");
        std::fs::write(layout.floor(), "44").unwrap();
        let t0 = SystemTime::now();
        let heads = Cell::new(0);
        let listings = Cell::new(0);
        let mut head = |_: &str| {
            heads.set(heads.get() + 1);
            Ok(HeadAnswer {
                code: 404,
                location: None,
            })
        };
        let mut list = |url: &str| {
            listings.set(listings.get() + 1);
            assert_eq!(
                url,
                "https://api.github.com/repos/alabsystems/aterm/releases?per_page=100&page=1"
            );
            Ok(listing_with_index(49))
        };
        assert_eq!(
            successor_with(&layout, "alabsystems", "aterm", t0, &mut head, &mut list),
            Probe::Published(49),
            "a skip larger than the HEAD window must not wait six hours"
        );
        assert_eq!((heads.get(), listings.get()), (4, 1));
        stamped_at(&layout, &[NEAR_LOCK, LOOKAHEAD_LOCK, LISTING_LOCK], t0);
        assert_eq!(
            successor_with(
                &layout,
                "alabsystems",
                "aterm",
                t0 + Duration::from_secs(61),
                &mut head,
                &mut list,
            ),
            Probe::Deferred,
            "the shared stamp suppresses another API listing"
        );
        assert_eq!((heads.get(), listings.get()), (6, 1));
        stamped_at(&layout, &[NEAR_LOCK], t0 + Duration::from_secs(61));
        assert_eq!(
            successor_with(
                &layout,
                "alabsystems",
                "aterm",
                t0 + Duration::from_secs(5 * 60 + 10),
                &mut head,
                &mut list,
            ),
            Probe::Published(49),
            "an unchanged floor can retry only after the five-minute cooldown"
        );
        assert_eq!((heads.get(), listings.get()), (10, 2));
        std::fs::remove_dir_all(layout.prefix).unwrap();
    }

    #[test]
    fn a_rate_limited_listing_refines_the_model_and_waits_an_hour() {
        let layout = layout("listing-rate-limit");
        std::fs::write(layout.floor(), "44").unwrap();
        let t0 = SystemTime::now();
        let model = atpkg_index_probe_cooldown_model();
        let mut state = model.init_state();
        let requests = Cell::new(0);
        let mut head = |_: &str| {
            Ok(HeadAnswer {
                code: 404,
                location: None,
            })
        };
        let mut list = |url: &str| {
            requests.set(requests.get() + 1);
            Err(HttpError::RateLimited {
                code: 429,
                url: url.to_string(),
                authenticated: false,
            })
        };
        assert!(model.fire("Acquire", &mut state));
        assert!(model.action_enabled("ProbeRateLimited", &state));
        assert_eq!(
            successor_with(&layout, "alabsystems", "aterm", t0, &mut head, &mut list),
            Probe::Deferred
        );
        assert!(model.fire("ProbeRateLimited", &mut state));
        assert!(model.fire("Release", &mut state));
        assert_eq!(requests.get(), 1);
        assert_eq!(
            std::fs::read_to_string(layout.prefix.join(LISTING_LOCK)).unwrap(),
            "44 R\n"
        );
        stamped_at(&layout, &[NEAR_LOCK, LOOKAHEAD_LOCK, LISTING_LOCK], t0);
        // 59 minutes are 118 thirty-second ticks, still inside the hour hold.
        for _ in 0..118 {
            assert!(model.fire("AdvanceTick", &mut state));
        }
        assert!(model.fire("Acquire", &mut state));
        assert!(model.action_enabled("SkipFresh", &state));
        assert_eq!(
            successor_with(
                &layout,
                "alabsystems",
                "aterm",
                t0 + Duration::from_secs(59 * 60),
                &mut head,
                &mut list,
            ),
            Probe::Deferred
        );
        assert_eq!(requests.get(), 1);
        assert!(model.fire("SkipFresh", &mut state));
        assert!(model.fire("Release", &mut state));
        for _ in 0..2 {
            assert!(model.fire("AdvanceTick", &mut state));
        }
        assert!(model.fire("Acquire", &mut state));
        assert!(model.action_enabled("ProbeRateLimited", &state));
        assert_eq!(
            successor_with(
                &layout,
                "alabsystems",
                "aterm",
                t0 + Duration::from_secs(60 * 60 + 10),
                &mut head,
                &mut list,
            ),
            Probe::Deferred
        );
        assert_eq!(requests.get(), 2);
        assert!(model.fire("ProbeRateLimited", &mut state));
        assert!(model.fire("Release", &mut state));
        std::fs::remove_dir_all(layout.prefix).unwrap();
    }

    #[test]
    fn listing_requires_a_complete_newer_index_and_errors_do_not_wake() {
        assert!(standard_public_source("alabsystems", "aterm"));
        assert!(!standard_public_source("private-owner", "aterm"));
        assert!(!standard_public_source("alabsystems", "private-repo"));
        let mut body = |_: &str| Ok(listing_with_index(49));
        assert_eq!(
            listing_hint("alabsystems", "aterm", 49, &mut body).unwrap(),
            Probe::Missing,
            "a listing of the current floor is not a new release"
        );
        let mut invalid = |_: &str| {
            Ok(
                br#"[{"tag_name":"atpkg-index-50","assets":[{"name":"index.toml","url":"i"}]}]"#
                    .to_vec(),
            )
        };
        assert_eq!(
            listing_hint("alabsystems", "aterm", 44, &mut invalid).unwrap(),
            Probe::Missing,
            "a half-uploaded index cannot wake a signed pass"
        );
        let mut no_network = |_: &str| -> Result<Vec<u8>, HttpError> {
            panic!("invalid source must not issue a request")
        };
        assert_eq!(
            listing_hint("private-owner", "bad/repo", 44, &mut no_network).unwrap(),
            Probe::Deferred
        );
        let mut error = |_: &str| Err(HttpError::Transport("offline".into()));
        assert!(matches!(
            listing_hint("alabsystems", "aterm", 44, &mut error),
            Err(HttpError::Transport(_))
        ));
    }

    /// Only GitHub's own answer for a published asset wakes the loop: a 302 or 307 to an
    /// https URL on its release-asset storage. A 200 (an intermediary's page), a permanent
    /// or see-other redirect, a redirect anywhere else — a login page, a look-alike host,
    /// plain http, userinfo — a redirect with no Location, and an error say nothing; a 404
    /// is the one answer that says "not yet".
    #[test]
    fn only_a_redirect_to_the_asset_storage_is_a_publication() {
        let head = |code, location: Option<&str>| {
            classify(
                45,
                Ok(HeadAnswer {
                    code,
                    location: location.map(str::to_string),
                }),
            )
        };
        for host in RELEASE_ASSET_HOSTS {
            for code in [302, 307] {
                assert_eq!(
                    head(code, Some(&format!("https://{host}/object?sig=x"))),
                    Probe::Published(45),
                    "{code} {host}"
                );
            }
        }
        let refused = [
            (200, None),
            (
                301,
                Some(
                    "https://github.com/new-owner/aterm/releases/download/atpkg-index-45/index.toml",
                ),
            ),
            (308, Some("https://release-assets.githubusercontent.com/x")),
            (303, Some("https://release-assets.githubusercontent.com/x")),
            (302, Some("https://sso.example.com/login?next=/releases")),
            (302, Some("http://release-assets.githubusercontent.com/x")),
            (
                302,
                Some("https://release-assets.githubusercontent.com.evil.com/x"),
            ),
            (
                302,
                Some("https://user@release-assets.githubusercontent.com/x"),
            ),
            (302, None),
            (302, Some("")),
            (503, None),
        ];
        for (code, location) in refused {
            assert_eq!(head(code, location), Probe::Deferred, "{code} {location:?}");
        }
        assert_eq!(head(404, None), Probe::Missing);
        assert_eq!(
            classify(45, Err(HttpError::Transport("offline".into()))),
            Probe::Deferred
        );
    }
}
