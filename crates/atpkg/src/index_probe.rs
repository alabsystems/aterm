// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Lightweight HEAD probe for the next published index ([`successor_reporting_near_with`](crate::index_probe::successor_reporting_near_with),
//! [`is_release_asset_host`](crate::index_probe::is_release_asset_host), [`classify`](crate::index_probe::classify)).

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Seek as _, Write as _};
use std::path::Path;
use std::time::{Duration, SystemTime};

use aterm_update_core::{HeadAnswer, HttpError};

use crate::store::Layout;

/// The per-process check cadence, enforced across processes by a shared stamp.
pub const INTERVAL: Duration = Duration::from_secs(30);
const RETRY_AFTER_ERROR: Duration = Duration::from_secs(60);
const MAX_STAMP_BYTES: u64 = 64;
/// The store-scoped lock and stamp file every probing process shares.
pub(crate) const NEAR_LOCK: &str = "index-probe.lock";
/// The tags past the floor the probe asks for: the next, and the one after it.
const OFFSETS: [u64; 2] = [1, 2];

/// Whether `host` is where the release host redirects a published asset.
#[must_use]
pub fn is_release_asset_host(host: &str) -> bool {
    host.strip_suffix(".githubusercontent.com")
        .is_some_and(|sub| sub.split('.').all(|label| !label.is_empty()))
}

/// The probe's only actionable answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    Published(u64),
    Missing,
    /// Another process already asked this range.
    Suppressed(Duration),
    Deferred,
}

/// Probe for the store's next index build.
pub fn successor_reporting_near_with(layout: &Layout, on_near: &mut dyn FnMut(u64)) -> Probe {
    successor_with_near_hint(layout, on_near)
}

fn successor_with_near_hint(layout: &Layout, on_near: &mut dyn FnMut(u64)) -> Probe {
    if !probes_this_source() || !crate::cli::update_pass_has_work(layout) {
        return Probe::Deferred;
    }
    let source = crate::discovery::resolve_account(crate::config::cached().account());
    let repo = crate::discovery::index_repo();
    successor_notifying_with(
        layout,
        &source.owner,
        &repo,
        SystemTime::now(),
        &mut aterm_update_core::head_no_redirect_quick,
        on_near,
    )
}

/// The highest index build this store has verified.
#[must_use]
pub fn verified_floor(layout: &Layout) -> u64 {
    crate::sig::Floor::new(layout.floor()).current()
}

fn standard_public_source(owner: &str, repo: &str) -> bool {
    owner == aterm_update_core::ATPKG_INDEX_OWNER && repo == aterm_update_core::DEFAULT_REPO
}

/// Whether [`successor_reporting_near_with`] probes this store's index source at all.
#[must_use]
pub fn probes_this_source() -> bool {
    if !crate::enabled() || crate::source::registry_seam().is_some() {
        return false;
    }
    let source = crate::discovery::resolve_account(crate::config::cached().account());
    standard_public_source(&source.owner, &crate::discovery::index_repo())
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
) -> Probe {
    successor_notifying_with(layout, owner, repo, now, head, &mut |_| {})
}

fn successor_notifying_with(
    layout: &Layout,
    owner: &str,
    repo: &str,
    now: SystemTime,
    head: &mut dyn FnMut(&str) -> Result<HeadAnswer, HttpError>,
    on_published: &mut dyn FnMut(u64),
) -> Probe {
    let floor = verified_floor(layout);
    if !std::fs::symlink_metadata(&layout.prefix)
        .is_ok_and(|m| m.file_type().is_dir() && !crate::platform::is_reparse(&m))
    {
        return Probe::Deferred;
    }
    probe_next(layout, owner, repo, floor, now, head, on_published)
}

/// One locked attempt.
fn probe_next(
    layout: &Layout,
    owner: &str,
    repo: &str,
    floor: u64,
    now: SystemTime,
    head: &mut dyn FnMut(&str) -> Result<HeadAnswer, HttpError>,
    on_published: &mut dyn FnMut(u64),
) -> Probe {
    let Some(mut file) = take_probe_lock(&layout.prefix.join(NEAR_LOCK)) else {
        return Probe::Deferred;
    };
    if let Some(remaining) = stamp_remaining(&mut file, floor, now) {
        return Probe::Suppressed(remaining);
    }
    let mut answer = Probe::Missing;
    for offset in OFFSETS {
        let next = floor.checked_add(offset).and_then(|build| {
            let tag = format!("atpkg-index-{build}");
            aterm_update_core::cdn::release_download_url(owner, repo, &tag, "index.toml")
                .map(|url| classify(build, head(&url)))
        });
        let next = next.unwrap_or(Probe::Deferred);
        if let Probe::Published(build) = next {
            // The release is only a wake hint. Report it before the next HEAD waits; the
            // signed pass resolves authority.
            on_published(build);
        }
        answer = highest_or_deferred(answer, next);
    }
    let marker = match answer {
        Probe::Published(_) => 'P',
        Probe::Missing => 'M',
        Probe::Deferred | Probe::Suppressed(_) => 'E',
    };
    let _ = stamp(&mut file, floor, marker);
    answer
}

/// Two HEADs' answers as one.
fn highest_or_deferred(a: Probe, b: Probe) -> Probe {
    match (a, b) {
        (Probe::Published(a), Probe::Published(b)) => Probe::Published(a.max(b)),
        (Probe::Published(a), _) | (_, Probe::Published(a)) => Probe::Published(a),
        (Probe::Missing, Probe::Missing) => Probe::Missing,
        _ => Probe::Deferred,
    }
}

/// Take the probe's lock without waiting ([`crate::lock::Flock`]).
fn take_probe_lock(path: &Path) -> Option<crate::lock::Flock> {
    crate::lock::Flock::try_lock(open_private(path, true)?).ok()
}

/// Open one of the store's private rendezvous files.
pub(crate) fn open_private(path: &Path, create: bool) -> Option<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    if create {
        options.write(true).create(true).truncate(false);
    }
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
    (metadata.file_type().is_file() && !crate::platform::is_reparse(&metadata)).then_some(file)
}

fn stamp_remaining(file: &mut File, floor: u64, now: SystemTime) -> Option<Duration> {
    let (recorded, marker, modified) = read_stamp(file)?;
    if recorded != floor {
        return None;
    }
    let lifetime = match marker {
        'M' | 'P' => INTERVAL,
        'E' => RETRY_AFTER_ERROR,
        _ => return None,
    };
    match now.duration_since(modified) {
        Ok(age) => (age < lifetime).then(|| lifetime - age),
        Err(_) => modified
            .duration_since(now)
            .ok()
            .filter(|ahead| *ahead < lifetime)
            .map(|ahead| lifetime + ahead),
    }
}

/// Parse the probe stamp (`"{floor} {marker}\n"`, [`stamp_remaining`], [`cached_answer`]).
fn read_stamp(file: &mut File) -> Option<(u64, char, SystemTime)> {
    let metadata = file.metadata().ok()?;
    if metadata.len() > MAX_STAMP_BYTES {
        return None;
    }
    let mut text =
        String::with_capacity(usize::try_from(metadata.len().saturating_add(1)).unwrap_or(0));
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
        _ => return None,
    };
    if recorded.is_empty() || !recorded.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((recorded.parse().ok()?, marker, metadata.modified().ok()?))
}

/// What the probe last answered, as its stamp records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeAnswer {
    /// A release-asset HEAD found a newer index (an untrusted hint; no signed pass has run).
    Published,
    /// Nothing newer at the next two tags.
    Missing,
    /// No usable answer: a failed request, or one that says nothing ([`classify`]).
    Failed,
}

/// The probe's last answer for the store's current floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CachedProbe {
    pub floor: u64,
    pub near: Option<(RangeAnswer, SystemTime)>,
}

/// Read the probe's stamp back (see [`CachedProbe`]).
#[must_use]
pub fn cached_answer(layout: &Layout) -> CachedProbe {
    let floor = verified_floor(layout);
    let read = |name: &str| {
        let mut file = open_private(&layout.prefix.join(name), false)?;
        let (recorded, marker, written) = read_stamp(&mut file)?;
        (recorded == floor).then_some(())?;
        let answer = match marker {
            'P' => RangeAnswer::Published,
            'M' => RangeAnswer::Missing,
            _ => RangeAnswer::Failed,
        };
        Some((answer, written))
    };
    CachedProbe {
        floor,
        near: read(NEAR_LOCK),
    }
}

/// Index builds cached for the probed source ([`crate::cache::IndexCache::labels`]).
#[must_use]
pub fn held_index_builds(layout: &Layout) -> Option<Vec<u64>> {
    cached_index_builds(
        layout,
        &crate::net::github_source_id(aterm_update_core::ATPKG_INDEX_OWNER),
    )
}

/// [`held_index_builds`] for the cache of `source` (a fetcher's `source_id`).
#[must_use]
pub(crate) fn cached_index_builds(layout: &Layout, source: &str) -> Option<Vec<u64>> {
    let labels = crate::cache::IndexCache::for_layout(layout).labels(source)?;
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

fn stamp(file: &mut File, floor: u64, marker: char) -> std::io::Result<()> {
    file.set_len(0)?;
    file.seek(std::io::SeekFrom::Start(0))?;
    writeln!(file, "{floor} {marker}")
}

/// Classify a redirect-refusing HEAD response ([`is_release_asset_host`]).
pub(crate) fn classify(next: u64, answer: Result<HeadAnswer, HttpError>) -> Probe {
    match answer {
        Ok(HeadAnswer {
            code: 302 | 307,
            location: Some(location),
        }) if crate::vendor::https_host(&location).is_some_and(is_release_asset_host) => {
            Probe::Published(next)
        }
        Ok(HeadAnswer { code: 404, .. }) => Probe::Missing,
        _ => Probe::Deferred,
    }
}

#[cfg(test)]
#[path = "index_probe_tests.rs"]
mod tests;
