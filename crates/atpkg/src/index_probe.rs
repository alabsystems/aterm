// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Discover the next published index without spending a Releases API request.
//!
//! The shipped index is a prerelease in the app's repository, so GitHub's
//! `/releases/latest` pointer deliberately names an app release. Its index builds are
//! CONTIGUOUS — `tools/atpkg-index.sh` refuses an `index_build` that is not its baseline's
//! plus one — so the next index is `floor + 1`. One HEAD each of `floor + 1` and
//! `floor + 2`, every thirty seconds, finds it; the second is the one missing number the
//! signed pass's walk also steps over (`net::newest_index`), so a number missing anyway (a
//! first publish numbered by hand, a release deleted and never re-cut) blinds neither. A hit
//! wakes the ordinary update pass — the window's package lane is told the moment the first
//! HEAD answers ([`successor_reporting_near`]), before the second returns; only that pass
//! downloads and verifies the roster, index, manifests and artifacts, and it walks to the
//! newest index itself, however far ahead that is. The machine-wide six-hour walk
//! (`aterm_update_core::pkg_check::FULL_PASS_INTERVAL_SECS`) remains the fallback.
//!
//! Every request this module makes is a HEAD of a download URL on the release host,
//! which is unmetered; it has no API transport at all, and that is the point (owner ruling
//! R3). Until 2026-09-23 a probe whose HEADs all missed also fetched the first page of the
//! anonymous Releases listing on a five-minute stamp — in the steady state every five
//! minutes on every probing machine (measured: every 5m07s), about twelve an hour of the
//! sixty anonymous requests GitHub grants one IP, shared with the signed pass and the
//! release cut, which had died on that budget three times the day before (audit
//! 2026-09-23, PK-2). Two more HEADs beyond the pair, every five minutes, went with it: the
//! publisher's contiguous builds left them nothing to find.
//!
//! Only GitHub's own answer for a published asset — a 302 or 307 to an https URL on its
//! user-content storage ([`is_release_asset_host`]) — reads as published ([`classify`]).
//! A 200, a permanent redirect (a renamed repo answers 301 for every path), or a
//! redirect anywhere else (a captive portal, a proxy's login page) says nothing, so an
//! intermediary can never wake a full pass at every cooldown.
//!
//! A store whose `update` would answer "nothing to update" — nothing installed, the set
//! not being completed, no vendor program tombstoned in place — is not probed at all
//! (audit PK-7): a published index could wake nothing there but that line.

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Seek as _, Write as _};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use aterm_update_core::{HeadAnswer, HttpError};

use crate::store::Layout;

/// The per-process check cadence; a shared stamp also enforces it across processes, and it
/// is the cooldown after a probe that found the next tags missing — and after one that found
/// one published, so an index a pass has not landed yet is offered again promptly.
pub const INTERVAL: Duration = Duration::from_secs(30);
const RETRY_AFTER_ERROR: Duration = Duration::from_secs(5 * 60);
const MAX_STAMP_BYTES: u64 = 64;
/// The store-scoped lock and stamp file every probing process shares.
pub(crate) const NEAR_LOCK: &str = "index-probe.lock";
/// The tags past the floor the probe asks for: the next, and the one after it.
const OFFSETS: [u64; 2] = [1, 2];

/// Whether `host` is where the release host redirects a published asset: GitHub's
/// user-content storage, a host under `githubusercontent.com` — `release-assets.` today,
/// `objects.` before it.
///
/// The DOMAIN, not a list of its hosts. GitHub has moved the asset host once already, and
/// until 2026-09-23 the rule was exactly those two names: the day it moved again, every tag
/// would have read as unpublished, every client's index discovery failed on every pass, and
/// only an app release naming the new host would have mended it. The domain still refuses
/// what the rule is for — a captive portal's or a proxy's redirect to a login page — and it
/// is no trust boundary: every byte a discovery then fetches must verify. The release
/// cutter's readability check (`aterm_release::publish::classify_asset_head`) reads the
/// same host by this same rule.
#[must_use]
pub fn is_release_asset_host(host: &str) -> bool {
    host.strip_suffix(".githubusercontent.com")
        .is_some_and(|sub| sub.split('.').all(|label| !label.is_empty()))
}

/// The probe's only actionable answer. `Published` is an untrusted wake hint, not
/// an index verdict; the usual signed update pass decides what may be installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    Published(u64),
    Missing,
    Deferred,
}

/// Probe the shipped public index source, if this store has a real directory, its
/// registry has not been overridden, and an `update` pass would have work to do. Other
/// sources retain the ordinary full update cadence; the public browser-download URL cannot
/// speak for a private or local registry.
pub fn successor(layout: &Layout) -> Probe {
    successor_with_near_hint(layout, &mut |_| {})
}

/// The same probe, with an early untrusted hint: the moment one HEAD finds a published
/// index, `near_build` is raised to it, before the other HEAD returns — so a GUI package
/// lane can start its signed update during that wait. The final answer is still the
/// highest of both.
pub fn successor_reporting_near(layout: &Layout, near_build: &AtomicU64) -> Probe {
    successor_with_near_hint(layout, &mut |build| {
        near_build.fetch_max(build, Ordering::Release);
    })
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

/// The highest index build this store has VERIFIED — the durable rollback floor the
/// probe looks past. What the window reads to know whether a published index landed.
#[must_use]
pub fn verified_floor(layout: &Layout) -> u64 {
    crate::sig::Floor::new(layout.floor()).current()
}

fn standard_public_source(owner: &str, repo: &str) -> bool {
    owner == aterm_update_core::ATPKG_INDEX_OWNER && repo == aterm_update_core::DEFAULT_REPO
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

/// One store-scoped probe. The HEAD closure is injectable so the URL, response
/// classification and cross-process request bound are tested without network. It is the
/// probe's only transport.
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
    // The prefix was created by the launch seed. A missing or linked prefix is
    // not made writable by a read-side hint; the full update owns that repair.
    if !std::fs::symlink_metadata(&layout.prefix)
        .is_ok_and(|m| m.file_type().is_dir() && !crate::platform::is_reparse(&m))
    {
        return Probe::Deferred;
    }
    probe_next(layout, owner, repo, floor, now, head, on_published)
}

/// One locked attempt: HEAD `atpkg-index-<floor+1>` and `<floor+2>`'s `index.toml` unless
/// the shared stamp says a sibling asked within its cooldown, then stamp the answer for the
/// next reader. `on_published` hears a published build the moment its HEAD answers.
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
    if stamped_recently(&mut file, floor, now) {
        return Probe::Deferred;
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
    // A crash or write failure merely makes the next process retry. The lock
    // remains held until this function returns, including across the HEADs.
    let marker = match answer {
        Probe::Published(_) => 'P',
        Probe::Missing => 'M',
        Probe::Deferred => 'E',
    };
    let _ = stamp(&mut file, floor, marker);
    answer
}

/// Two HEADs' answers as one: the higher published build wins, `Missing` only when both
/// said missing, and anything else is no answer.
fn highest_or_deferred(a: Probe, b: Probe) -> Probe {
    match (a, b) {
        (Probe::Published(a), Probe::Published(b)) => Probe::Published(a.max(b)),
        (Probe::Published(a), _) | (_, Probe::Published(a)) => Probe::Published(a),
        (Probe::Missing, Probe::Missing) => Probe::Missing,
        _ => Probe::Deferred,
    }
}

/// Take the probe's lock without waiting — the ONE way [`probe_next`] takes it. `None` when
/// the file cannot be opened as a regular file or another probe holds it.
///
/// A [`crate::lock::Flock`], never a bare `File`: the lock is released by `LOCK_UN` on
/// every return, so the next probe finds it free at once. Released by the close alone, a
/// child another thread is mid-spawning keeps a copy of the descriptor until its exec, and
/// the next probe reads the lock as a sibling's and defers — the full-suite flake of
/// 2026-09-23. Pinned by `the_probe_lock_is_free_the_moment_its_probe_releases_it`.
fn take_probe_lock(path: &Path) -> Option<crate::lock::Flock> {
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

fn stamped_recently(file: &mut File, floor: u64, now: SystemTime) -> bool {
    let Some((recorded, marker, modified)) = read_stamp(file) else {
        return false;
    };
    if recorded != floor {
        return false;
    }
    let lifetime = match marker {
        'M' | 'P' => INTERVAL,
        'E' => RETRY_AFTER_ERROR,
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

/// The ONE parser of the probe's stamp — `"{floor} {marker}\n"` plus the file's mtime, the
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
        _ => return None,
    };
    Some((recorded.parse().ok()?, marker, metadata.modified().ok()?))
}

/// What the probe last answered, as its stamp records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeAnswer {
    /// A release-asset HEAD found a newer index (an untrusted hint; no signed pass has run).
    Published,
    /// Nothing newer at the next two tags.
    Missing,
    /// No usable answer: a failed request, or one that says nothing ([`classify`] — a 200,
    /// a permanent redirect, a redirect off the release-asset hosts, the host's 429). Every
    /// one is stamped `E`; none is "nothing newer".
    Failed,
}

/// The probe's last answer for the store's CURRENT floor — what the window's
/// thirty-second cadence (or any process that ran [`successor`]) already learned, read back
/// without a request, a lock or a write. `near` is `(answer, when it was written)` for the
/// next two tags, and is `None` when there is no stamp for this floor — never probed, or
/// probed before the floor last moved (a stamp recorded under an older floor answers a
/// question nobody is asking now).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CachedProbe {
    pub floor: u64,
    pub near: Option<(RangeAnswer, SystemTime)>,
}

/// Read the probe's stamp back (see [`CachedProbe`]). Offline and side-effect free: the
/// lock file is opened read-only, never created, never locked — a stamp being rewritten at
/// this instant reads as torn, and a torn stamp is simply absent.
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
            _ => RangeAnswer::Failed,
        };
        Some((answer, written))
    };
    CachedProbe {
        floor,
        near: read(NEAR_LOCK),
    }
}

/// The index builds of the signed candidates the last pass that REACHED the channel
/// downloaded for the source [`successor`] watches — the labels of
/// `<prefix>/index-cache.toml` ([`crate::cache::IndexCache::labels`]), newest first. A build
/// here above the floor was downloaded and not landed. `None` when there is no readable
/// cache for this source. Read-only, offline; diagnostics, never an input to any trust
/// decision.
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
/// GitHub's user-content storage ([`is_release_asset_host`]) is the asset, a 404 its absence,
/// and everything else — a 200 (GitHub never serves the asset itself, so that is an
/// intermediary's page), a permanent redirect, a redirect anywhere else, a transport
/// failure — says nothing. Found in review of Phase 3's parallel probe (p3/sched,
/// 2026-09-22) and true of this one until 2026-09-23: any 200 or 3xx read as a
/// publication, so a captive portal or a filtering proxy that answers every URL woke a
/// signed pass at every five-minute published cooldown, for good. The signed pass's index
/// discovery (`net::GithubFetcher::walk_candidates`) reads its HEADs with this same rule.
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
mod tests {
    use super::*;
    use aterm_spec::derive::{
        Model, atpkg_index_probe_cooldown_model, atpkg_index_successor_selection_model,
    };
    use std::cell::Cell;
    use std::collections::BTreeMap;

    // ONE ATTEMPT, NEVER A RETRY. These fixtures used to retry a deferred probe, because a
    // child a sibling test was mid-spawning kept a copy of the probe lock's descriptor
    // after the probe closed it, so the next probe read the lock as held and deferred. The
    // lock releases by `LOCK_UN` since 2026-09-24 ([`take_probe_lock`], pinned by
    // `the_probe_lock_is_free_the_moment_its_probe_releases_it`), so a deferral here is a
    // finding, and the retry loops that absorbed one are gone.

    struct ModeledCase {
        label: &'static str,
        action: &'static str,
        code: u16,
        location: Option<&'static str>,
        marker: char,
        /// The cooldown in the model's thirty-second ticks ([`INTERVAL`]).
        cooldown_ticks: u32,
        answer: Probe,
    }

    fn modeled_attempt(
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
            // The historical no-stamp-check mutant would spend another request here. Real
            // code must agree with the healthy guard.
            let buggy = aterm_spec::interp::with_buggy(model, 1);
            assert!(buggy.action_enabled(case.action, state));
        }
        let floor = crate::sig::Floor::new(layout.floor()).current();
        let requests = Cell::new(0usize);
        let mut head = |url: &str| {
            requests.set(requests.get() + 1);
            assert!(!metered(url), "{url}");
            assert!(
                url.ends_with(&format!("/atpkg-index-{}/index.toml", floor + 1))
                    || url.ends_with(&format!("/atpkg-index-{}/index.toml", floor + 2)),
                "{url}"
            );
            Ok(HeadAnswer {
                code: case.code,
                location: case.location.map(str::to_string),
            })
        };
        // One locked attempt is the pair of HEADs, floor+1 and floor+2.
        let budget = if admitted { OFFSETS.len() } else { 0 };
        let observed = probe_next(
            layout,
            "alabsystems",
            "aterm",
            floor,
            at,
            &mut head,
            &mut |_| {},
        );
        assert_eq!(requests.get(), budget, "{} wire budget", case.label);
        if admitted {
            assert_eq!(observed, case.answer);
            assert!(model.fire(case.action, state));
            assert_eq!(
                std::fs::read_to_string(layout.prefix.join(NEAR_LOCK)).unwrap(),
                format!("{floor} {}\n", case.marker),
                "the real writer must publish the marker the next reader uses"
            );
            stamped_at(layout, at);
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
    /// a synthetic clock from `t0`: so the stamp a probe just wrote is pinned to the
    /// synthetic instant it was written at, and the clock's ticks are its age exactly.
    /// Unpinned, a loaded machine that stalled the test for more than a case's margin
    /// between `t0` and the write left the stamp younger than the clock said — the cooldown
    /// "over" at `t0 + 70 s` still ran — and the derived-model conformance went red at
    /// random (found in review of p3/reconciled, 2026-09-23).
    fn stamped_at(layout: &Layout, at: SystemTime) {
        File::options()
            .write(true)
            .open(layout.prefix.join(NEAR_LOCK))
            .and_then(|file| file.set_modified(at))
            .unwrap();
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
            // The download host's rate limit is an ordinary error: five minutes, like a 503.
            ModeledCase {
                label: "rate-limited",
                action: "ProbeError",
                code: 429,
                location: None,
                marker: 'E',
                cooldown_ticks: 10,
                answer: Probe::Deferred,
            },
            // Both tags answer as published here; the higher is the answer.
            ModeledCase {
                label: "published",
                action: "ProbePublished",
                code: 302,
                location: Some("https://release-assets.githubusercontent.com/object"),
                marker: 'P',
                cooldown_ticks: 1,
                answer: Probe::Published(45),
            },
        ];
        for case in cases {
            let layout = layout(&format!("modeled-{}", case.label));
            std::fs::write(layout.floor(), "43").unwrap();
            let t0 = SystemTime::now();
            let mut state = model.init_state();
            modeled_attempt(&model, &mut state, &layout, t0, &case, true);
            modeled_attempt(
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
            modeled_attempt(
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
                modeled_attempt(
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
        let requests = std::cell::RefCell::new(Vec::<String>::new());
        let newer_ready = Cell::new(false);
        let tag = |build: u64| {
            format!(
                "https://github.com/alabsystems/aterm/releases/download/atpkg-index-{build}/index.toml"
            )
        };
        let mut head = |url: &str| {
            requests.borrow_mut().push(url.to_string());
            let published = url == tag(44) || (newer_ready.get() && url == tag(45));
            Ok(HeadAnswer {
                code: if published { 302 } else { 404 },
                location: published
                    .then(|| "https://release-assets.githubusercontent.com/object".into()),
            })
        };
        let asked = || requests.borrow().len();
        assert_eq!(
            successor_with(&layout, "alabsystems", "aterm", t0, &mut head),
            Probe::Published(44)
        );
        assert_eq!(
            *requests.borrow(),
            vec![tag(44), tag(45)],
            "the pair, in order"
        );
        stamped_at(&layout, t0);
        assert_eq!(
            successor_with(
                &layout,
                "alabsystems",
                "aterm",
                t0 + INTERVAL - Duration::from_secs(1),
                &mut head
            ),
            Probe::Deferred,
            "a second process reuses the shared stamp"
        );
        assert_eq!(asked(), 2);
        newer_ready.set(true);
        assert_eq!(
            successor_with(
                &layout,
                "alabsystems",
                "aterm",
                t0 + INTERVAL + Duration::from_secs(1),
                &mut head,
            ),
            Probe::Published(45),
            "a newer tag behind an unlanded 44 is found on the next probe"
        );
        assert_eq!(asked(), 4);
        std::fs::remove_dir_all(layout.prefix).unwrap();
    }

    /// THE EARLY HINT: the first HEAD to find a published index reports it before the
    /// second HEAD returns, so the window's package lane can start its signed pass during
    /// that wait; the final answer is still the higher of the pair.
    #[test]
    fn a_published_index_is_reported_before_the_second_head_returns() {
        let layout = layout("near-early");
        std::fs::write(layout.floor(), "43").unwrap();
        let reported = std::cell::RefCell::new(Vec::<u64>::new());
        let order = std::cell::RefCell::new(Vec::<String>::new());
        let mut head = |url: &str| {
            order.borrow_mut().push(format!("head {url}"));
            let found = url.ends_with("/atpkg-index-44/index.toml")
                || url.ends_with("/atpkg-index-45/index.toml");
            Ok(HeadAnswer {
                code: if found { 302 } else { 404 },
                location: found.then(|| "https://release-assets.githubusercontent.com/o".into()),
            })
        };
        let answer = successor_notifying_with(
            &layout,
            "alabsystems",
            "aterm",
            SystemTime::now(),
            &mut head,
            &mut |build| {
                reported.borrow_mut().push(build);
                order.borrow_mut().push(format!("hint {build}"));
            },
        );
        assert_eq!(answer, Probe::Published(45));
        assert_eq!(*reported.borrow(), vec![44, 45]);
        let order = order.into_inner();
        let hint44 = order.iter().position(|e| e == "hint 44").unwrap();
        let head45 = order
            .iter()
            .position(|e| e.ends_with("atpkg-index-45/index.toml"))
            .unwrap();
        assert!(
            hint44 < head45,
            "44 is reported before 45 is asked: {order:?}"
        );
        std::fs::remove_dir_all(layout.prefix).unwrap();
    }

    #[test]
    fn a_sibling_probe_holding_the_store_lock_prevents_duplicate_heads() {
        let layout = layout("locked");
        std::fs::write(layout.floor(), "43").unwrap();
        let model = atpkg_index_probe_cooldown_model();
        let mut state = model.init_state();
        let held = open_probe_lock(&layout.prefix.join(NEAR_LOCK)).unwrap();
        held.try_lock().unwrap();
        assert!(model.fire("Acquire", &mut state));
        assert!(!model.action_enabled("Acquire", &state));
        let mut head = |_: &str| -> Result<HeadAnswer, HttpError> {
            panic!("a sibling probe already owns the network request")
        };
        assert_eq!(
            successor_with(
                &layout,
                "alabsystems",
                "aterm",
                SystemTime::now(),
                &mut head,
            ),
            Probe::Deferred
        );
        drop(held);
        assert!(model.fire("Abort", &mut state));
        assert!(model.action_enabled("Acquire", &state));
        std::fs::remove_dir_all(layout.prefix).unwrap();
    }

    /// THE PROBE LOCK IS FREE THE MOMENT ITS PROBE RELEASES IT, whatever copies of its
    /// descriptor live on. `copy` stands in for a child another test thread is mid-spawning,
    /// which holds every descriptor until its exec. Were the lock released by the close
    /// alone, the copy would keep it held and the next probe would defer with nothing on the
    /// wire — the 2026-09-23 full-suite flake. A regression of [`take_probe_lock`] to a bare
    /// `File` fails both assertions after the release. Negative control first: a lock
    /// actually held defers the probe, so the pass after the release is not vacuous.
    #[test]
    fn the_probe_lock_is_free_the_moment_its_probe_releases_it() {
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
            probe_next(
                &layout,
                "alabsystems",
                "aterm",
                floor,
                SystemTime::now(),
                head,
                &mut |_| {},
            )
        };

        let holder = take_probe_lock(&path).expect("a free probe lock is taken");
        assert!(
            take_probe_lock(&path).is_none(),
            "a held probe lock refuses"
        );
        assert_eq!(probe(&mut head), Probe::Deferred);
        assert_eq!(requests.get(), 0, "a held lock spends no request");

        let copy = holder.try_clone().expect("a copy of the descriptor");
        // Released by `LOCK_UN` (the guard's drop) while the copy is still open.
        drop(holder);
        assert!(
            take_probe_lock(&path).is_some(),
            "a released probe lock must be free while a copy of its descriptor lives"
        );
        assert_eq!(probe(&mut head), Probe::Missing);
        assert_eq!(
            requests.get(),
            OFFSETS.len(),
            "the probe after a release goes to the wire"
        );
        drop(copy);
        std::fs::remove_dir_all(&layout.prefix).unwrap();
    }

    /// The next tags missing are asked again after thirty seconds, never sooner, and a floor
    /// that moved (the pass landed an index) makes the old stamp moot at once.
    #[test]
    fn missing_retries_after_thirty_seconds_and_floor_change_bypasses_old_stamp() {
        assert_eq!(INTERVAL, Duration::from_secs(30));
        let layout = layout("missing");
        std::fs::write(layout.floor(), "44").unwrap();
        let t0 = SystemTime::now();
        let requested = std::cell::RefCell::new(Vec::<String>::new());
        let mut head = |url: &str| {
            requested.borrow_mut().push(url.to_string());
            Ok(HeadAnswer {
                code: 404,
                location: None,
            })
        };
        let asked = || requested.borrow().len();
        // ONE probe per instant, its HEADs counted exactly: no retry, because the lock is
        // released by `LOCK_UN` ([`take_probe_lock`]), so a sibling test's spawn cannot
        // make a free range defer.
        let mut at = |at: SystemTime, asks: usize| {
            let before = asked();
            let answer = successor_with(&layout, "alabsystems", "aterm", at, &mut head);
            assert_eq!(asked() - before, asks, "the HEADs at {at:?}");
            answer
        };
        assert_eq!(at(t0, 2), Probe::Missing);
        stamped_at(&layout, t0);
        assert_eq!(at(t0 + Duration::from_secs(15), 0), Probe::Deferred);
        assert_eq!(at(t0 + Duration::from_secs(31), 2), Probe::Missing);
        stamped_at(&layout, t0 + Duration::from_secs(31));
        std::fs::write(layout.floor(), "45").unwrap();
        assert_eq!(
            at(t0 + Duration::from_secs(32), 2),
            Probe::Missing,
            "a changed floor invalidates the stamp"
        );
        let tag = |build: u64| {
            format!(
                "https://github.com/alabsystems/aterm/releases/download/atpkg-index-{build}/index.toml"
            )
        };
        assert_eq!(
            *requested.borrow(),
            vec![tag(45), tag(46), tag(45), tag(46), tag(46), tag(47)]
        );
        std::fs::remove_dir_all(layout.prefix).unwrap();
    }

    /// THE PAIR'S ANSWER REFINES `AtpkgIndexSuccessorSelection`: for every pair of answers
    /// the two HEADs can give — missing, no usable answer (a 503), published — the real
    /// probe's answer is the model's choice after observing them in the same order: the
    /// higher published build over a lower one and over uncertainty, and `Missing` only when
    /// both answered missing. The model's `Buggy=1` (first hit, false missing) disagrees
    /// with the real probe on the replayed defects, so the agreement is not vacuous.
    #[test]
    fn the_real_head_pair_selection_refines_the_derived_model() {
        #[derive(Clone, Copy)]
        enum Answer {
            Missing,
            Deferred,
            Published,
        }
        let model = atpkg_index_successor_selection_model();
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let answers = [Answer::Missing, Answer::Deferred, Answer::Published];
        let mut disagreed = 0;
        for first in answers {
            for second in answers {
                let layout = layout("selection");
                std::fs::write(layout.floor(), "43").unwrap();
                let asked = Cell::new(0usize);
                let mut head = |url: &str| {
                    asked.set(asked.get() + 1);
                    let answer = if url.ends_with("/atpkg-index-44/index.toml") {
                        first
                    } else {
                        second
                    };
                    Ok(match answer {
                        Answer::Missing => HeadAnswer {
                            code: 404,
                            location: None,
                        },
                        Answer::Deferred => HeadAnswer {
                            code: 503,
                            location: None,
                        },
                        Answer::Published => HeadAnswer {
                            code: 302,
                            location: Some("https://release-assets.githubusercontent.com/o".into()),
                        },
                    })
                };
                let real = successor_with(
                    &layout,
                    "alabsystems",
                    "aterm",
                    SystemTime::now(),
                    &mut head,
                );
                let observe = |answer: Answer, high: bool| match answer {
                    Answer::Missing => "ObserveMissing",
                    Answer::Deferred => "ObserveDeferred",
                    Answer::Published if high => "ObserveHigh",
                    Answer::Published => "ObserveLow",
                };
                let chosen = |m: &Model| {
                    let mut state = m.init_state();
                    for action in [observe(first, false), observe(second, true), "Choose"] {
                        assert!(m.fire(action, &mut state), "{action}");
                    }
                    match state["chosen"] {
                        1 => Probe::Missing,
                        2 => Probe::Deferred,
                        3 => Probe::Published(44),
                        5 => Probe::Published(45),
                        other => panic!("unmapped choice {other}"),
                    }
                };
                assert_eq!(real, chosen(&model), "the model's choice");
                if chosen(&buggy) != real {
                    disagreed += 1;
                }
                std::fs::remove_dir_all(layout.prefix).unwrap();
            }
        }
        assert!(disagreed > 0, "the historical choice must differ somewhere");
    }

    /// What the doctor reads back: the probe's last answer for the CURRENT floor, and
    /// nothing for a stamp an older floor wrote.
    #[test]
    fn the_cached_answer_is_the_stamp_for_the_current_floor() {
        let layout = layout("cached");
        std::fs::write(layout.floor(), "43").unwrap();
        assert_eq!(cached_answer(&layout).near, None, "never probed");
        let head = |_: &str| {
            Ok(HeadAnswer {
                code: 404,
                location: None,
            })
        };
        let t0 = SystemTime::now();
        let asked = Cell::new(0);
        let mut counting = |url: &str| {
            asked.set(asked.get() + 1);
            head(url)
        };
        assert_eq!(
            successor_with(&layout, "alabsystems", "aterm", t0, &mut counting),
            Probe::Missing
        );
        stamped_at(&layout, t0);
        let cached = cached_answer(&layout);
        assert_eq!(cached.floor, 43);
        assert_eq!(cached.near, Some((RangeAnswer::Missing, t0)));
        std::fs::write(layout.floor(), "44").unwrap();
        assert_eq!(cached_answer(&layout).near, None, "an older floor's stamp");
        std::fs::remove_dir_all(layout.prefix).unwrap();
    }

    /// A request on GitHub's metered REST API: the anonymous budget of sixty an hour per IP
    /// that the signed pass and the release cut share. The probe must never make one.
    fn metered(url: &str) -> bool {
        url.starts_with("https://api.github.com/")
    }

    /// PK-2 (audit 2026-09-23): in the steady state — the floor current and the next tags
    /// absent — the probe once fetched a Releases listing at every five-minute stamp, about
    /// twelve anonymous API requests an hour per machine. Drive the real probe for a hundred
    /// ticks, thirty-one seconds apart, with every HEAD answering 404 on a recording
    /// transport, the probe's only one: it asked exactly the next two index tags per tick —
    /// and nothing on api.github.com.
    #[test]
    fn a_hundred_steady_state_ticks_spend_only_unmetered_heads() {
        // The negative control: the predicate names the request the retired listing made,
        // so its absence below is not a predicate that matches nothing.
        assert!(metered(
            "https://api.github.com/repos/alabsystems/aterm/releases?per_page=100&page=1"
        ));
        assert!(!metered(
            "https://github.com/alabsystems/aterm/releases/download/atpkg-index-45/index.toml"
        ));
        let layout = layout("steady-state");
        std::fs::write(layout.floor(), "44").unwrap();
        let t0 = SystemTime::now();
        let requested = std::cell::RefCell::new(Vec::<String>::new());
        let mut head = |url: &str| {
            requested.borrow_mut().push(url.to_string());
            Ok(HeadAnswer {
                code: 404,
                location: None,
            })
        };
        let asked = || requested.borrow().len();
        for tick in 0..100u64 {
            let at = t0 + Duration::from_secs(tick * 31);
            let before = asked();
            let answer = successor_with(&layout, "alabsystems", "aterm", at, &mut head);
            assert_eq!(asked() - before, 2, "tick {tick}: the pair of HEADs");
            assert_eq!(answer, Probe::Missing, "tick {tick}");
            stamped_at(&layout, at);
        }
        let requested = requested.into_inner();
        assert_eq!(requested.len(), 200);
        let next = |build: u64| {
            format!(
                "https://github.com/alabsystems/aterm/releases/download/atpkg-index-{build}/index.toml"
            )
        };
        assert!(
            requested
                .iter()
                .all(|url| *url == next(45) || *url == next(46)),
            "only the next two tags, and never a metered request: {:?}",
            requested.iter().find(|url| metered(url))
        );
        std::fs::remove_dir_all(layout.prefix).unwrap();
    }

    #[test]
    fn only_the_standard_public_source_is_probed() {
        assert!(standard_public_source("alabsystems", "aterm"));
        assert!(!standard_public_source("private-owner", "aterm"));
        assert!(!standard_public_source("alabsystems", "private-repo"));
    }

    /// Only GitHub's own answer for a published asset wakes the loop: a 302 or 307 to an
    /// https URL on its user-content storage — any host under `githubusercontent.com`, so a
    /// host GitHub has not named yet (`release-objects.` below, the positive control for
    /// the day it moves the storage again) counts as the two it has used. A 200 (an
    /// intermediary's page), a permanent or see-other redirect, a redirect anywhere else — a
    /// login page, a look-alike host, the bare domain, an empty label, plain http, userinfo
    /// — a redirect with no Location, and an error say nothing; a 404 is the one answer that
    /// says "not yet".
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
        for host in [
            "release-assets.githubusercontent.com",
            "objects.githubusercontent.com",
            "release-objects.githubusercontent.com",
            "eu.release-assets.githubusercontent.com",
        ] {
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
            (302, Some("https://githubusercontent.com/x")),
            (302, Some("https://evilgithubusercontent.com/x")),
            (302, Some("https://.githubusercontent.com/x")),
            (302, Some("https://a..githubusercontent.com/x")),
            (
                302,
                Some("https://release-objects.githubusercontent.com.evil.com/x"),
            ),
            (302, Some("http://release-objects.githubusercontent.com/x")),
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
