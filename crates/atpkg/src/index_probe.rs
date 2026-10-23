// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Lightweight HEAD probe for the next published index ([`successor_reporting_near_with`],
//! [`is_release_asset_host`], [`classify`]).

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
    if !crate::enabled() || crate::cli::registry_seam().is_some() {
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
mod tests {
    use super::*;
    use aterm_models_atpkg::{
        atpkg_index_probe_cooldown_model, atpkg_index_successor_selection_model,
    };
    use aterm_spec::derive::Model;
    use std::cell::Cell;
    use std::collections::BTreeMap;

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
            assert_eq!(observed, Probe::Suppressed(Duration::from_secs(1)));
            assert!(model.fire("SkipFresh", state));
        }
        assert!(model.fire("Release", state));
        assert!(model.check_invariant("NoDuplicateRangeInsideCooldown", state));
    }

    fn layout(name: &str) -> crate::testdir::ScratchLayout {
        crate::testdir::layout(&format!("atpkg-index-probe-{name}-"))
    }

    /// Pin the stamp's mtime to the synthetic instant `at`: its age is read from the real
    /// mtime, so a test stalled before the write would leave it younger than the clock says.
    fn stamped_at(layout: &Layout, at: SystemTime) {
        File::options()
            .write(true)
            .open(layout.prefix.join(NEAR_LOCK))
            .and_then(|file| file.set_modified(at))
            .unwrap();
    }

    /// A second host takes the shared stamp's remaining wait, not another
    /// complete local interval.
    #[test]
    fn a_suppressed_probe_reports_the_exact_shared_cooldown_without_a_head() {
        for (marker, lifetime) in [('M', INTERVAL), ('P', INTERVAL), ('E', RETRY_AFTER_ERROR)] {
            let layout = layout(&format!("shared-handoff-{marker}"));
            std::fs::write(layout.floor(), "43").unwrap();
            std::fs::write(layout.prefix.join(NEAR_LOCK), format!("43 {marker}\n")).unwrap();
            let stamped = SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000);
            stamped_at(&layout, stamped);
            let requests = Cell::new(0);
            let mut head = |_: &str| {
                requests.set(requests.get() + 1);
                Ok(HeadAnswer {
                    code: 404,
                    location: None,
                })
            };
            assert_eq!(
                successor_with(
                    &layout,
                    "alabsystems",
                    "aterm",
                    stamped + lifetime - Duration::from_secs(5),
                    &mut head,
                ),
                Probe::Suppressed(Duration::from_secs(5))
            );
            assert_eq!(requests.get(), 0);
            assert_eq!(
                successor_with(
                    &layout,
                    "alabsystems",
                    "aterm",
                    stamped - Duration::from_secs(5),
                    &mut head,
                ),
                Probe::Suppressed(lifetime + Duration::from_secs(5))
            );
            assert_eq!(requests.get(), 0);
            assert_eq!(
                successor_with(
                    &layout,
                    "alabsystems",
                    "aterm",
                    stamped + lifetime,
                    &mut head,
                ),
                Probe::Missing
            );
            assert_eq!(
                requests.get(),
                2,
                "only the stamp expiry spends a HEAD pair"
            );
            std::fs::remove_dir_all(&layout.prefix).unwrap();
        }
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
                cooldown_ticks: 2,
                answer: Probe::Deferred,
            },
            ModeledCase {
                label: "rate-limited",
                action: "ProbeError",
                code: 429,
                location: None,
                marker: 'E',
                cooldown_ticks: 2,
                answer: Probe::Deferred,
            },
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
            std::fs::remove_dir_all(&layout.prefix).unwrap();
        }
    }

    /// The first HEAD to find a published index reports it before the second returns; the
    /// final answer is still the higher of the pair.
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
        std::fs::remove_dir_all(&layout.prefix).unwrap();
    }

    /// The probe lock is free the moment its probe releases it, whatever copies of its
    /// descriptor live on (`copy` stands in for a child mid-spawn on another thread).
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

    /// The pair's answer refines `AtpkgIndexSuccessorSelection`: for every pair of HEAD
    /// answers (missing, a 503, published) the probe answers what the model chooses — the
    /// higher published build over a lower one and over uncertainty, `Missing` only when
    /// both missed.
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
                std::fs::remove_dir_all(&layout.prefix).unwrap();
            }
        }
        assert!(disagreed > 0, "the historical choice must differ somewhere");
    }

    /// What the doctor reads back: the probe's last answer for the current floor, and
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
        std::fs::remove_dir_all(&layout.prefix).unwrap();
    }

    /// A request on GitHub's metered REST API, which the probe must never make.
    fn metered(url: &str) -> bool {
        url.starts_with("https://api.github.com/")
    }

    #[test]
    fn only_the_standard_public_source_is_probed() {
        assert!(standard_public_source("alabsystems", "aterm"));
        assert!(!standard_public_source("private-owner", "aterm"));
        assert!(!standard_public_source("alabsystems", "private-repo"));
    }

    /// Only a 302 or 307 to an https URL under `githubusercontent.com` is a publication,
    /// including a host GitHub has not used yet (`release-objects.`); a 404 says "not yet",
    /// and every other answer (a 200, a permanent or see-other redirect, a redirect anywhere
    /// else, no Location, an error) says nothing.
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
