// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The production GitHub-Releases [`Fetcher`](crate::flow::Fetcher) (§5/§9).

use std::path::{Path, PathBuf};

use aterm_update_core::release_asset::{Asset, AssetError, Reader, Release};

use crate::flow::VendorFetchError;
use crate::select::Candidate;

/// One release asset as atpkg names it (`atpkg-<program>-<build>`'s `pkg-<program>-<build>.toml`
/// and the like): what a read asks the shared release-asset reader for
/// ([`aterm_update_core::release_asset`]), which takes the web route and, when that fails, the
/// API route. The reader is anonymous: atpkg runs on every client and holds no credential.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Named {
    owner: String,
    repo: String,
    tag: String,
    name: String,
}

impl Named {
    /// Asset `name` of release `tag` in `slug`; `None` unless `slug` is `owner/repo` and every
    /// segment is URL-safe.
    fn new(slug: &str, tag: &str, name: &str) -> Option<Named> {
        let asset = Asset::in_slug(slug, Release::Tag(tag), name)?;
        Some(Named {
            owner: asset.owner.to_string(),
            repo: asset.repo.to_string(),
            tag: tag.to_string(),
            name: name.to_string(),
        })
    }

    fn asset(&self) -> Asset<'_> {
        Asset::tagged(&self.owner, &self.repo, &self.tag, &self.name)
    }

    /// Its web route, which index discovery HEADs.
    fn web_url(&self) -> String {
        self.asset().web_url()
    }
}

fn underivable(slug: &str, what: &str) -> String {
    let mut msg = String::from("no release download URL derives for ");
    msg.push_str(what);
    msg.push_str(" on ");
    msg.push_str(slug);
    msg
}

/// `pkg-<program>-<build>.toml` and its `.sig` on release `atpkg-<program>-<build>` of `slug`.
fn manifest_assets(slug: &str, program: &str, build: u64) -> Option<(Named, Named)> {
    if program.is_empty()
        || !program
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    let b = build.to_string();
    let mut tag = String::from("atpkg-");
    tag.push_str(program);
    tag.push('-');
    tag.push_str(&b);
    let mut name = String::from("pkg-");
    name.push_str(program);
    name.push('-');
    name.push_str(&b);
    name.push_str(".toml");
    let toml = Named::new(slug, &tag, &name)?;
    name.push_str(".sig");
    let sig = Named::new(slug, &tag, &name)?;
    Some((toml, sig))
}

/// Archive `asset` on the release of its stem up to its first all-digit segment
/// (`ty-2973-<triple>.tar.zst` is on `atpkg-ty-2973`) of `slug`.
fn unit_asset(slug: &str, asset: &str) -> Option<Named> {
    let stem = asset.split_once('.')?.0;
    if stem.is_empty()
        || !stem
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    let mut end = None;
    let mut pos = 0usize;
    for seg in stem.split('-') {
        let seg_end = pos + seg.len();
        if !seg.is_empty() && seg.bytes().all(|b| b.is_ascii_digit()) {
            end = Some(seg_end);
            break;
        }
        pos = seg_end + 1;
    }
    let mut tag = String::from("atpkg-");
    tag.push_str(stem.get(..end?)?);
    Named::new(slug, &tag, asset)
}

/// Byte caps bounding what an attacker-controlled asset can write.
pub const MANIFEST_CAP: u64 = 5_000_000;
pub const SIG_CAP: u64 = 4_096;
const ARTIFACT_CAP: u64 = 8u64 << 30;

const fn artifact_cap(size: u64) -> u64 {
    if size == 0 || size > ARTIFACT_CAP {
        ARTIFACT_CAP
    } else {
        size
    }
}

/// A roster is a few hundred bytes per machine and at most 16 machines.
pub const ROSTER_CAP: u64 = 65_536;

const INDEX_CANDIDATE_CAP: u64 = 4;

const WALK_HEAD_BUDGET: u32 = 48;

const FIRST_INDEX_SEARCH_LIMIT: u64 = 1 << 20;

const FETCH_CONCURRENCY: usize = 20;

const MANIFEST_WAVE: usize = 64;

const WALK_AHEAD_BUDGET: usize = 2 * WALK_HEAD_BUDGET as usize;

/// `job` over every item, up to [`FETCH_CONCURRENCY`] at once, the answers in item order.
pub(crate) fn fan_out<T: Sync, R: Send>(items: &[T], job: &(dyn Fn(&T) -> R + Sync)) -> Vec<R> {
    fan_out_upto(items, FETCH_CONCURRENCY, job)
}

fn fan_out_upto<T: Sync, R: Send>(
    items: &[T],
    width: usize,
    job: &(dyn Fn(&T) -> R + Sync),
) -> Vec<R> {
    if items.len() < 2 {
        return items.iter().map(job).collect();
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut answered: Vec<(usize, R)> = std::thread::scope(|scope| {
        let numbered = || {
            let mut mine = Vec::new();
            loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(item) = items.get(i) else {
                    break mine;
                };
                mine.push((i, job(item)));
            }
        };
        let helpers: Vec<_> = (1..items.len().min(width))
            .filter_map(|_| {
                std::thread::Builder::new()
                    .name("atpkg-fetch".into())
                    .spawn_scoped(scope, numbered)
                    .ok()
            })
            .collect();
        let mut all = numbered();
        for helper in helpers {
            all.extend(
                helper
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            );
        }
        all
    });
    answered.sort_unstable_by_key(|(i, _)| *i);
    answered.into_iter().map(|(_, r)| r).collect()
}

fn index_tag(build: u64) -> String {
    let mut tag = String::from("atpkg-index-");
    tag.push_str(&build.to_string());
    tag
}

/// The newest published index, by HEADs of its tags alone.
/// # Errors
/// The first probe that said neither, or `no atpkg-index release is published`.
pub fn newest_index(
    floor: u64,
    probe: &mut dyn FnMut(u64) -> Result<bool, String>,
) -> Result<(u64, u64), String> {
    let overflow = || String::from("index discovery ran past the largest build number");
    let low = if floor > 0 {
        floor
    } else {
        let mut n = 1u64;
        loop {
            if probe(n)? {
                break n;
            }
            n = n
                .checked_mul(2)
                .filter(|&next| next <= FIRST_INDEX_SEARCH_LIMIT)
                .ok_or_else(|| String::from("no atpkg-index release is published"))?;
        }
    };
    let mut hit = low;
    loop {
        let base = hit;
        let mut step = 1u64;
        let mut miss = loop {
            let next = base.checked_add(step).ok_or_else(overflow)?;
            if !probe(next)? {
                break next;
            }
            hit = next;
            step = step.checked_mul(2).ok_or_else(overflow)?;
        };
        while miss - hit > 1 {
            let mid = hit + (miss - hit) / 2;
            if probe(mid)? {
                hit = mid;
            } else {
                miss = mid;
            }
        }
        let past = hit.checked_add(2).ok_or_else(overflow)?;
        if !probe(past)? {
            return Ok((hit, low));
        }
        hit = past;
    }
}

/// What the discovery rounds may GUESS the channel publishes ([`ahead_of_walk`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexHint {
    /// The first build [`newest_index`] from no floor found published.
    pub low: Option<u64>,
    /// The newest build seen published.
    pub newest: u64,
}

impl IndexHint {
    /// `<low>..<newest>`, or a bare `<newest>` ([`std::fmt::Display`]'s spellings).
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let number = |s: &str| crate::store::parse_build_name(s).filter(|&n| n > 0);
        let (low, newest) = match text.trim().split_once("..") {
            Some((low, newest)) => (Some(number(low)?), number(newest)?),
            None => (None, number(text.trim())?),
        };
        low.is_none_or(|low| low <= newest)
            .then_some(Self { low, newest })
    }
}

impl std::fmt::Display for IndexHint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.low {
            Some(low) => write!(f, "{low}..{}", self.newest),
            None => write!(f, "{}", self.newest),
        }
    }
}

/// The published run this binary's release cut stamped into its identity record
/// ([`aterm_stamp::Stamp::index_hint`]); a binary built outside a release carries none.
#[must_use]
pub fn stamped_index_hint() -> Option<IndexHint> {
    IndexHint::parse(aterm_stamp::current().index_hint)
}

/// The whole optional published-run walk gets five seconds, with no transport retries.
pub const INDEX_HINT_BOUND: std::time::Duration = std::time::Duration::from_secs(5);

/// The shipped channel's published run, found as a store that has verified nothing finds it.
/// This is an optional observation: every HEAD shares [`INDEX_HINT_BOUND`], with no retries.
/// # Errors
/// The first HEAD that said neither, or `no atpkg-index release is published`.
pub fn published_index_run() -> Result<IndexHint, String> {
    published_index_run_with(
        std::time::Instant::now() + INDEX_HINT_BOUND,
        &aterm_update_core::head_no_redirect_until,
    )
}

fn published_index_run_with(
    deadline: std::time::Instant,
    head: &impl Fn(
        &str,
        std::time::Instant,
    ) -> Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError>,
) -> Result<IndexHint, String> {
    let mut slug = String::from(aterm_update_core::ATPKG_INDEX_OWNER);
    slug.push('/');
    slug.push_str(crate::manifest::INDEX_REPO);
    let (newest, low) = newest_index(0, &mut |build| {
        let url = index_head_url(&slug, build).map_err(|(msg, _, _)| msg)?;
        read_head_answer(&url, build, head(&url, deadline)).map_err(|(msg, _, _)| msg)
    })?;
    Ok(IndexHint {
        low: Some(low),
        newest,
    })
}

/// The four derived download URLs of one index release: the index, its machine signature,
/// and the master-signed roster and its signature.
#[derive(Debug)]
struct IndexQuad {
    label: String,
    index: Named,
    index_sig: Named,
    roster: Named,
    roster_sig: Named,
}

fn index_quad(slug: &str, tag: &str) -> Option<IndexQuad> {
    let roster = aterm_update_core::roster::ROSTER_ASSET;
    let mut roster_sig = String::from(roster);
    roster_sig.push_str(".sig");
    Some(IndexQuad {
        label: tag.to_string(),
        index: Named::new(slug, tag, "index.toml")?,
        index_sig: Named::new(slug, tag, "index.toml.sig")?,
        roster: Named::new(slug, tag, roster)?,
        roster_sig: Named::new(slug, tag, &roster_sig)?,
    })
}

fn fetch_run(
    slug: &str,
    (newest, low): (u64, u64),
    fetch: &WebFetch<'_>,
    cache: Option<&crate::signed_artifact_cache::SignedArtifactCache<'_>>,
    cache_source: Option<&str>,
    first_error: &mut Option<String>,
) -> Result<Vec<Candidate>, String> {
    let oldest = newest.saturating_sub(INDEX_CANDIDATE_CAP - 1).max(low);
    let quads = (oldest..=newest)
        .rev()
        .map(|build| {
            let tag = index_tag(build);
            index_quad(slug, &tag).ok_or_else(|| underivable(slug, &tag))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let cached: Vec<Option<Candidate>> = quads
        .iter()
        .map(|quad| {
            let cache = cache?;
            let source = cache_source?;
            let key = crate::signed_artifact_cache::SignedIndexKey::release(source, &quad.label)?;
            cache.load_index(
                &key,
                usize::try_from(MANIFEST_CAP).unwrap_or(usize::MAX),
                usize::try_from(SIG_CAP).unwrap_or(usize::MAX),
                usize::try_from(ROSTER_CAP).unwrap_or(usize::MAX),
                usize::try_from(SIG_CAP).unwrap_or(usize::MAX),
            )
        })
        .collect();
    let assets: Vec<(&Named, u64)> = quads
        .iter()
        .zip(&cached)
        .filter(|(_, candidate)| candidate.is_none())
        .flat_map(|(quad, _)| {
            [
                (&quad.index, MANIFEST_CAP),
                (&quad.index_sig, SIG_CAP),
                (&quad.roster, ROSTER_CAP),
                (&quad.roster_sig, SIG_CAP),
            ]
        })
        .collect();
    let mut answers = fan_out(&assets, &|&(asset, cap)| fetch(asset, cap)).into_iter();
    let mut take = || {
        answers
            .next()
            .unwrap_or_else(|| Err(AssetError::Refused(underivable(slug, "an index asset"))))
    };
    let mut out = Vec::new();
    for (quad, cached) in quads.iter().zip(cached) {
        let candidate = cached.map_or_else(
            || whole_quad(&quad.label, [take(), take(), take(), take()]),
            Ok,
        );
        match candidate {
            Ok(candidate) => out.push(candidate),
            Err(e) => {
                // Only absence permits omitting this release. A failed acquisition is not
                // evidence that an older release is the source's current answer.
                if !e.is_absent() {
                    return Err(e.to_string());
                }
                first_error.get_or_insert_with(|| e.to_string());
            }
        }
    }
    Ok(out)
}

fn whole_quad(
    label: &str,
    answers: [Result<Vec<u8>, AssetError>; 4],
) -> Result<Candidate, AssetError> {
    // An absent half must not hide another half's acquisition failure. Retain the first
    // non-absence error; no partial quad reaches selection.
    let mut error: Option<AssetError> = None;
    let [index_bytes, sig, roster_bytes, roster_sig] = answers.map(|answer| {
        answer.unwrap_or_else(|next| {
            if error
                .as_ref()
                .is_none_or(|held| held.is_absent() && !next.is_absent())
            {
                error = Some(next);
            }
            Vec::new()
        })
    });
    if let Some(error) = error {
        return Err(error);
    }
    Ok(Candidate {
        label: label.to_string(),
        index_bytes,
        sig,
        roster_bytes,
        roster_sig,
    })
}

trait Heads: Sync {
    fn whole(
        &self,
        url: &str,
    ) -> Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError>;

    fn first(&self, url: &str) -> aterm_update_core::FirstHead {
        aterm_update_core::FirstHead::Settled(self.whole(url))
    }

    fn again(
        &self,
        url: &str,
        _cause: aterm_types::http_retry::Cause,
    ) -> Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError> {
        self.whole(url)
    }

    fn link_down(&self) {}
}

impl<F> Heads for F
where
    F: Fn(&str) -> Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError> + Sync,
{
    fn whole(
        &self,
        url: &str,
    ) -> Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError> {
        self(url)
    }
}

struct Wire;

impl Heads for Wire {
    fn whole(
        &self,
        url: &str,
    ) -> Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError> {
        aterm_update_core::head_no_redirect(url)
    }

    fn first(&self, url: &str) -> aterm_update_core::FirstHead {
        aterm_update_core::head_no_redirect_first(url)
    }

    fn again(
        &self,
        url: &str,
        cause: aterm_types::http_retry::Cause,
    ) -> Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError> {
        aterm_update_core::head_no_redirect_again(url, cause)
    }

    fn link_down(&self) {
        aterm_update_core::note_link_down();
    }
}

type HeadFn<'a> = dyn Heads + 'a;

type WebFetch<'a> = dyn Fn(&Named, u64) -> Result<Vec<u8>, AssetError> + Sync + 'a;

/// `Err(message, link_down, transport_failed)` for one freshness probe.
type HeadVerdict = Result<bool, (String, bool, bool)>;

fn head_verdict(slug: &str, build: u64, head: &HeadFn<'_>) -> HeadVerdict {
    let url = index_head_url(slug, build)?;
    read_head_answer(&url, build, head.whole(&url))
}

fn index_head_url(slug: &str, build: u64) -> Result<String, (String, bool, bool)> {
    let tag = index_tag(build);
    Named::new(slug, &tag, "index.toml")
        .map(|index| index.web_url())
        .ok_or_else(|| (underivable(slug, &tag), false, false))
}

fn read_head_answer(
    url: &str,
    build: u64,
    answer: Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError>,
) -> HeadVerdict {
    use crate::index_probe::{Probe, classify};
    let answer = answer.map_err(|e| {
        let link_down = matches!(e, aterm_update_core::HttpError::Transport(_));
        let mut msg = String::from("HEAD ");
        msg.push_str(url);
        msg.push_str(": ");
        msg.push_str(&e.to_string());
        (msg, link_down, true)
    })?;
    let code = answer.code;
    match classify(build, Ok(answer)) {
        Probe::Published(_) => Ok(true),
        Probe::Missing => Ok(false),
        Probe::Deferred | Probe::Suppressed(_) => {
            let mut msg = String::from("HEAD ");
            msg.push_str(url);
            msg.push_str(" answered ");
            msg.push_str(&code.to_string());
            msg.push_str(", not GitHub's redirect to a release asset");
            Err((msg, false, true))
        }
    }
}

fn probe_round(slug: &str, round: &[u64], head: &HeadFn<'_>) -> Vec<HeadVerdict> {
    use aterm_update_core::FirstHead;
    let firsts = fan_out(round, &|&build| {
        index_head_url(slug, build).map(|url| {
            let first = head.first(&url);
            (url, first)
        })
    });
    let unreached = firsts.iter().all(|first| {
        matches!(
            first,
            Ok((
                _,
                FirstHead::Again {
                    unreached: true,
                    ..
                }
            ))
        )
    });
    if unreached {
        head.link_down();
    }
    let rest: Vec<(usize, String, aterm_types::http_retry::Cause)> = firsts
        .iter()
        .enumerate()
        .filter_map(|(i, first)| match first {
            Ok((url, FirstHead::Again { cause, .. })) if !unreached => {
                Some((i, url.clone(), *cause))
            }
            _ => None,
        })
        .collect();
    let mut again = rest
        .iter()
        .map(|(i, _, _)| *i)
        .zip(fan_out(&rest, &|(_, url, cause)| head.again(url, *cause)))
        .collect::<std::collections::BTreeMap<_, _>>();
    round
        .iter()
        .zip(firsts)
        .enumerate()
        .map(|(i, (&build, first))| {
            let (url, first) = first?;
            let answer = match first {
                FirstHead::Settled(answer) => answer,
                FirstHead::Again { answer, .. } => again.remove(&i).unwrap_or(answer),
            };
            read_head_answer(&url, build, answer)
        })
        .collect()
}

fn ahead_of_walk(
    floor: u64,
    need: u64,
    known: &std::collections::BTreeMap<u64, HeadVerdict>,
    room: usize,
    hint: Option<IndexHint>,
) -> Vec<u64> {
    let hint = hint.filter(|h| floor == 0 || h.newest > floor);
    let top = known
        .iter()
        .filter(|(_, v)| matches!(v, Ok(true)))
        .map(|(b, _)| *b)
        .max();
    let run = hint.and_then(|h| {
        let low = if floor > 0 { Some(floor) } else { h.low };
        low.map(|low| low..=h.newest)
    });
    let guess = |build: u64| -> Option<bool> {
        let Some(top) = top else {
            return Some(run.as_ref().is_some_and(|run| run.contains(&build)));
        };
        let hinted = hint.map(|h| h.newest).filter(|&h| h >= top);
        if build <= hinted.unwrap_or(top) {
            return Some(true);
        }
        let past_a_miss = known
            .range(top..=build)
            .any(|(_, v)| matches!(v, Ok(false)));
        (past_a_miss || hinted.is_some()).then_some(false)
    };
    let mut want = vec![need];
    let mut forks = std::collections::VecDeque::from([std::collections::BTreeMap::new()]);
    let mut replays = 0usize;
    while want.len() < room && replays < 4 * room {
        let Some(assumed) = forks.pop_front() else {
            break;
        };
        replays += 1;
        let mut fork = None;
        let mut asked = 0u32;
        let _ = newest_index(floor, &mut |build| {
            asked += 1;
            if asked > WALK_HEAD_BUDGET {
                return Err(String::new());
            }
            if let Some(verdict) = known.get(&build) {
                return verdict.clone().map_err(|_| String::new());
            }
            if let Some(&answer) = assumed.get(&build) {
                return Ok(answer);
            }
            if !want.contains(&build) {
                want.push(build);
            }
            guess(build).ok_or_else(|| {
                fork = Some(build);
                String::new()
            })
        });
        if let Some(build) = fork {
            for answer in [true, false] {
                let mut next = assumed.clone();
                next.insert(build, answer);
                forks.push_back(next);
            }
        }
    }
    let moved_from = match (hint, top) {
        (Some(h), None) => run.as_ref().map(|run| (*run.start(), h.newest)),
        (Some(h), Some(top)) if h.newest >= top => Some((0, h.newest)),
        _ => None,
    };
    if let Some((low, newest)) = moved_from {
        let mut moved = 1u64;
        while want.len() < room && moved <= FIRST_INDEX_SEARCH_LIMIT {
            let past = newest.saturating_add(moved);
            let mut asked = 0u32;
            let _ = newest_index(floor, &mut |build| {
                asked += 1;
                if asked > WALK_HEAD_BUDGET {
                    return Err(String::new());
                }
                if let Some(verdict) = known.get(&build) {
                    return verdict.clone().map_err(|_| String::new());
                }
                if !want.contains(&build) {
                    if want.len() >= room {
                        return Err(String::new());
                    }
                    want.push(build);
                }
                Ok((low..=past).contains(&build))
            });
            moved = moved.saturating_mul(2);
        }
    }
    want.truncate(room.max(1));
    want
}

type ManifestKey = (String, String, u64);

type ManifestBytes = std::sync::Arc<(Vec<u8>, Vec<u8>)>;

type ManifestMemo = std::sync::Mutex<std::collections::BTreeMap<ManifestKey, ManifestBytes>>;

/// The production fetcher.
pub struct GithubFetcher {
    owner: String,
    /// program → slug-validated `"owner/repo"`: where that program's `pkg-*.toml` and
    /// artifacts are fetched from instead of `<owner>/<repo>` (a development setting).
    overrides: std::collections::BTreeMap<String, String>,
    /// Per-invocation memo of the index candidate set: the flow re-resolves it per dependency
    /// and per default-set member, and a miss is a discovery walk plus four downloads per
    /// candidate.
    index: std::sync::Mutex<Option<std::sync::Arc<Vec<Candidate>>>>,
    /// The highest index build this store has verified (its durable floor,
    /// [`crate::index_probe::verified_floor`]), where the discovery walk counts from.
    index_floor: u64,
    /// A build something saw published, and where the run starts when that is known
    /// ([`Self::with_index_hint`]): a guess the discovery rounds follow, never an answer.
    index_hint: Option<IndexHint>,
    /// The package layout whose content-addressed signed-document cache can answer exact index
    /// releases after the live HEAD walk selects their immutable labels. Ad-hoc/test fetchers
    /// have none and retain the transport-only behavior.
    artifact_layout: Option<crate::store::Layout>,
    /// Whether the last discovery's HEAD failed on the link (curl reached no host), which
    /// [`crate::flow::Fetcher::index_link_down`] reads.
    link_down: std::sync::atomic::AtomicBool,
    /// Whether the last candidate discovery failed while reading channel freshness rather than
    /// receiving a source verdict. Kept separately from `link_down`: an HTTP 5xx reached a host
    /// but still did not answer the freshness question.
    transport_failed: std::sync::atomic::AtomicBool,
    /// What the last discovery asked (its HEADs, the rounds they went out in, its asset GETs),
    /// for the package log's `index-resolved` line.
    walk_cost: [std::sync::atomic::AtomicU32; 3],
    /// Per-invocation memo of the raw `(pkg-<program>-<build>.toml, .sig)` bytes, keyed by
    /// `(slug, program, build)`.
    manifests: ManifestMemo,
    /// The LAN peers an archive download races ([`crate::peers::unit_peers`]: `[packages]
    /// peers`, else this machine's fleet members; [`crate::peers::race`]).
    unit_peers: Vec<String>,
    /// Every asset read of this invocation: anonymous, web route first, API route when the web
    /// route fails, one patience for the whole pass, and at most [`API_RELEASES`] releases
    /// listed on the API route.
    reader: Reader,
}

/// The releases one fetcher may list on the API route. Its reads are anonymous and draw on the
/// allowance every machine behind this address shares (60 an hour); a fallback for every
/// failing program once emptied it. One release keeps a pass's recovery to one package (its
/// manifest pair and archive, which share a release) or the index, never one per program.
/// Claimed before the listing and never given back; reads the web route serves cost nothing.
const API_RELEASES: u32 = 1;

impl GithubFetcher {
    /// A fetcher for `owner`'s public releases.
    #[must_use]
    pub fn new(owner: String) -> Self {
        Self {
            owner,
            overrides: std::collections::BTreeMap::new(),
            index: std::sync::Mutex::new(None),
            index_floor: 0,
            index_hint: None,
            artifact_layout: None,
            link_down: std::sync::atomic::AtomicBool::new(false),
            transport_failed: std::sync::atomic::AtomicBool::new(false),
            walk_cost: Default::default(),
            manifests: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            unit_peers: Vec::new(),
            reader: Reader::anonymous().api_releases(API_RELEASES),
        }
    }

    /// Read every asset through `reader` (a loopback fixture's), bounded as [`Self::new`]'s.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn with_reader(mut self, reader: Reader) -> Self {
        self.reader = reader.api_releases(API_RELEASES);
        self
    }

    /// The bytes of `asset`, at most `cap`.
    fn read(&self, asset: &Named, cap: u64) -> Result<Vec<u8>, AssetError> {
        self.reader.bytes(&asset.asset(), cap)
    }

    /// `asset` into `dest`, at most `cap` bytes, ending early once `stop` is set.
    fn read_to(
        &self,
        asset: &Named,
        dest: &Path,
        cap: u64,
        stop: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<(), String> {
        match stop {
            Some(stop) => self.reader.to_file_until(&asset.asset(), dest, cap, stop),
            None => self.reader.to_file(&asset.asset(), dest, cap),
        }
        .map_err(|e| e.to_string())
    }

    /// Race each archive download against `peers`, LAN peers that may hold its unit
    /// ([`crate::peers::race`]).
    #[must_use]
    pub fn with_unit_peers(mut self, peers: Vec<String>) -> Self {
        self.unit_peers = peers;
        self
    }

    /// Attach the per-program `owner/repo` fetch overrides.
    #[must_use]
    pub fn with_overrides(mut self, overrides: std::collections::BTreeMap<String, String>) -> Self {
        self.overrides = overrides;
        self
    }

    /// Attach this store's verified index floor, which the discovery walk counts from.
    #[must_use]
    pub const fn with_index_floor(mut self, floor: u64) -> Self {
        self.index_floor = floor;
        self
    }

    /// Attach what this store may guess the channel publishes ([`IndexHint`]).
    #[must_use]
    pub const fn with_index_hint(mut self, hint: Option<IndexHint>) -> Self {
        self.index_hint = hint;
        self
    }

    /// Attach the layout that owns durable, verified signed-artifact copies.
    #[must_use]
    pub fn with_artifact_layout(mut self, layout: &crate::store::Layout) -> Self {
        self.artifact_layout = Some(layout.clone());
        self
    }

    fn slug_of(&self, repo: &str) -> String {
        format!("{}/{repo}", self.owner)
    }

    fn slug_for(&self, program: &str, repo: &str) -> String {
        match self.overrides.get(program) {
            Some(slug) => slug.clone(),
            None => self.slug_of(repo),
        }
    }

    fn index_slug(&self) -> String {
        self.slug_of(&crate::discovery::index_repo())
    }

    fn manifest_pair(
        &self,
        repo: &str,
        program: &str,
        build: u64,
        fetch: &WebFetch<'_>,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        let slug = self.slug_for(program, repo);
        let key = (slug.clone(), program.to_string(), build);
        if let Ok(memo) = self.manifests.lock()
            && let Some(hit) = memo.get(&key)
        {
            return Ok((**hit).clone());
        }
        let (toml, sig) =
            manifest_assets(&slug, program, build).ok_or_else(|| underivable(&slug, program))?;
        // Bytes only: signature and parser verdicts come downstream and never select another
        // route. Both halves start in one wave; each rides the download host's outages on the
        // API route inside the reader.
        let halves = [(&toml, MANIFEST_CAP), (&sig, SIG_CAP)];
        let mut fetched = fan_out_upto(&halves, 2, &|&(asset, cap)| fetch(asset, cap)).into_iter();
        let mut half = |what: &str, asset: &Named| {
            fetched
                .next()
                .expect("each half has a result")
                .map_err(|why| {
                    format!(
                        "{program} build {build} package {what} on {slug} tag {} asset {}: {why}",
                        asset.tag, asset.name
                    )
                })
        };
        let raw = half("manifest", &toml)?;
        let signature = half("signature", &sig)?;
        let pair = std::sync::Arc::new((raw, signature));
        if let Ok(mut memo) = self.manifests.lock() {
            memo.insert(key, std::sync::Arc::clone(&pair));
        }
        Ok((*pair).clone())
    }

    fn prefetch_with(&self, pins: &[(String, String, u64)], fetch: &WebFetch<'_>) {
        let mut wanted: Vec<(ManifestKey, Named, Named)> = Vec::new();
        for (repo, program, build) in pins {
            let slug = self.slug_for(program, repo);
            let key = (slug, program.clone(), *build);
            let held = self
                .manifests
                .lock()
                .is_ok_and(|memo| memo.contains_key(&key));
            if held || wanted.iter().any(|(k, ..)| *k == key) {
                continue;
            }
            if let Some((toml, sig)) = manifest_assets(&key.0, program, *build) {
                wanted.push((key, toml, sig));
            }
        }
        let assets: Vec<(&Named, u64)> = wanted
            .iter()
            .flat_map(|(_, toml, sig)| [(toml, MANIFEST_CAP), (sig, SIG_CAP)])
            .collect();
        let mut fetched =
            fan_out_upto(&assets, MANIFEST_WAVE, &|&(asset, cap)| fetch(asset, cap)).into_iter();
        for (key, ..) in wanted {
            if let (Some(Ok(toml)), Some(Ok(sig))) = (fetched.next(), fetched.next())
                && let Ok(mut memo) = self.manifests.lock()
            {
                memo.insert(key, std::sync::Arc::new((toml, sig)));
            }
        }
    }

    fn walk_candidates(
        &self,
        slug: &str,
        head: &HeadFn<'_>,
        fetch: &WebFetch<'_>,
    ) -> Result<Vec<Candidate>, String> {
        self.link_down
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.transport_failed
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let [heads_asked, rounds, gets] = &self.walk_cost;
        for count in &self.walk_cost {
            count.store(0, std::sync::atomic::Ordering::Relaxed);
        }
        let counted = |asset: &Named, cap: u64| {
            gets.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let answer = fetch(asset, cap);
            if answer
                .as_ref()
                .is_err_and(signed_index_asset_error_is_transport)
            {
                self.transport_failed
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
            answer
        };
        let fetch: &WebFetch<'_> = &counted;
        let mut known = std::collections::BTreeMap::<u64, HeadVerdict>::new();
        let mut heads = 0u32;
        let mut asked_ahead = 0usize;
        let mut probe_from = |walk_floor: u64, build: u64| -> Result<bool, String> {
            heads += 1;
            if heads > WALK_HEAD_BUDGET {
                let mut msg = String::from("index discovery on ");
                msg.push_str(slug);
                msg.push_str(" found no end within ");
                msg.push_str(&WALK_HEAD_BUDGET.to_string());
                msg.push_str(" HEADs");
                return Err(msg);
            }
            if !known.contains_key(&build) {
                let room = FETCH_CONCURRENCY.min(WALK_AHEAD_BUDGET.saturating_sub(asked_ahead) + 1);
                let round = ahead_of_walk(walk_floor, build, &known, room, self.index_hint);
                asked_ahead += round.len().saturating_sub(1);
                let first = rounds.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0;
                heads_asked.fetch_add(
                    u32::try_from(round.len()).unwrap_or(u32::MAX),
                    std::sync::atomic::Ordering::Relaxed,
                );
                let verdicts = if first {
                    probe_round(slug, &round, head)
                } else {
                    fan_out(&round, &|&b| head_verdict(slug, b, head))
                };
                known.extend(round.into_iter().zip(verdicts));
            }
            match known.get(&build) {
                Some(Ok(published)) => Ok(*published),
                Some(Err((msg, link_down, transport_failed))) => {
                    if *link_down {
                        self.link_down
                            .store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    if *transport_failed {
                        self.transport_failed
                            .store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    Err(msg.clone())
                }
                None => Err(underivable(slug, &index_tag(build))),
            }
        };
        let floor = self.index_floor;
        let run = newest_index(floor, &mut |build| probe_from(floor, build))?;
        let artifact_cache = self
            .artifact_layout
            .as_ref()
            .map(crate::signed_artifact_cache::SignedArtifactCache::for_layout);
        let cache_source = github_source_id(&self.owner);
        let mut first_error: Option<String> = None;
        let mut out = fetch_run(
            slug,
            run,
            fetch,
            artifact_cache.as_ref(),
            Some(&cache_source),
            &mut first_error,
        )?;
        if out.is_empty()
            && floor > 0
            && run.0 == floor
            && matches!(probe_from(0, floor), Ok(false))
            && let Ok(found) = newest_index(0, &mut |build| probe_from(0, build))
            && found.0 < floor - 1
        {
            out = fetch_run(
                slug,
                found,
                fetch,
                artifact_cache.as_ref(),
                Some(&cache_source),
                &mut first_error,
            )?;
        }
        if out.is_empty() {
            return Err(first_error.unwrap_or_else(|| underivable(slug, "the index")));
        }
        Ok(out)
    }
}

impl crate::flow::Fetcher for GithubFetcher {
    fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
        // Memoized per invocation (the `index` field); the same raw bytes still flow through
        // every gate.
        if let Ok(memo) = self.index.lock()
            && let Some(hit) = memo.as_ref()
        {
            return Ok((**hit).clone());
        }
        let walked = self.walk_candidates(&self.index_slug(), &Wire, &|asset, cap| {
            self.read(asset, cap)
        });
        let [heads, rounds, gets] = self
            .walk_cost
            .each_ref()
            .map(|c| c.load(std::sync::atomic::Ordering::Relaxed));
        crate::packages_log::append_in_pass(&crate::packages_log::Event::IndexResolved {
            heads,
            rounds,
            gets,
            found: match &walked {
                Ok(candidates) => candidates.first().map_or("", |c| c.label.as_str()),
                Err(e) => e,
            },
        });
        let out = walked?;
        let out = std::sync::Arc::new(out);
        if let Ok(mut memo) = self.index.lock() {
            *memo = Some(std::sync::Arc::clone(&out));
        }
        Ok((*out).clone())
    }

    fn index_link_down(&self) -> bool {
        self.link_down.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn index_error_is_transport(&self, _error: &str) -> bool {
        self.transport_failed
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    fn cache_verified_index(&self, index_build: u64, candidate: &Candidate) {
        let Some(layout) = &self.artifact_layout else {
            return;
        };
        if candidate.label != index_tag(index_build) {
            eprintln!(
                "atpkg: verified index {} was not cached under noncanonical label {}",
                index_build, candidate.label
            );
            return;
        }
        let source = github_source_id(&self.owner);
        let Some(key) =
            crate::signed_artifact_cache::SignedIndexKey::release(&source, &candidate.label)
        else {
            eprintln!(
                "atpkg: verified index {} has no immutable cache key",
                candidate.label
            );
            return;
        };
        let cache = crate::signed_artifact_cache::SignedArtifactCache::for_layout(layout);
        if let Err(error) = cache.store_verified_index(
            &key,
            candidate,
            usize::try_from(MANIFEST_CAP).unwrap_or(usize::MAX),
            usize::try_from(SIG_CAP).unwrap_or(usize::MAX),
            usize::try_from(ROSTER_CAP).unwrap_or(usize::MAX),
            usize::try_from(SIG_CAP).unwrap_or(usize::MAX),
        ) {
            eprintln!(
                "atpkg: verified index {} signed documents were not cached ({error})",
                candidate.label
            );
        }
    }

    fn pkg_manifest(
        &self,
        repo: &str,
        program: &str,
        build: u64,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        self.manifest_pair(repo, program, build, &|asset, cap| self.read(asset, cap))
    }

    fn prefetch_manifests(&self, pins: &[(String, String, u64)]) {
        self.prefetch_with(pins, &|asset, cap| self.read(asset, cap));
    }

    fn shared(&self) -> Option<&(dyn crate::flow::Fetcher + Sync)> {
        Some(self)
    }

    fn download(&self, repo: &str, asset: &str, dest: &Path) -> Result<(), String> {
        let slug = self.slug_of(repo);
        let named = unit_asset(&slug, asset).ok_or_else(|| underivable(&slug, asset))?;
        self.read_to(&named, dest, ARTIFACT_CAP, None)
    }

    fn download_for(
        &self,
        program: &str,
        repo: &str,
        asset: &str,
        dest: &Path,
        cap: u64,
    ) -> Result<(), String> {
        let slug = self.slug_for(program, repo);
        let named = unit_asset(&slug, asset).ok_or_else(|| underivable(&slug, asset))?;
        self.read_to(&named, dest, artifact_cap(cap), None)
    }

    fn download_unit(
        &self,
        program: &str,
        repo: &str,
        artifact: &crate::manifest::Artifact,
        dest: &Path,
    ) -> Result<(), String> {
        let slug = self.slug_for(program, repo);
        let named = unit_asset(&slug, &artifact.asset)
            .ok_or_else(|| underivable(&slug, &artifact.asset))?;
        let cap = artifact_cap(artifact.size);
        if self.unit_peers.is_empty() {
            return self.read_to(&named, dest, cap, None);
        }
        crate::peers::race(
            &self.unit_peers,
            &artifact.sha256,
            artifact.size,
            dest,
            |dest, stop| self.read_to(&named, dest, cap, Some(stop)),
        )
    }

    fn download_url(&self, url: &str, dest: &Path, cap: u64) -> Result<(), String> {
        // A vendor URL that names a GitHub release asset is read as one, so it rides the
        // download host's outages on the API route; any other URL is the vendor's own.
        self.reader
            .to_file_at(url, dest, cap)
            .map_err(|e| e.to_string())
    }

    fn vendor_get(
        &self,
        program: &str,
        url: &str,
        cap: u64,
        if_none_match: Option<&str>,
    ) -> Result<crate::flow::VendorGet, VendorFetchError> {
        refuse_unpinned_vendor_url(program, url)?;
        aterm_update_core::vendor_get(url, cap, if_none_match)
            .map(vendor_get_from)
            .map_err(vendor_error)
    }

    fn vendor_head(
        &self,
        program: &str,
        url: &str,
        cap: u64,
        if_none_match: Option<&str>,
    ) -> Result<crate::flow::VendorGet, VendorFetchError> {
        vendor_hint_get(program, url, cap, if_none_match)
    }

    fn vendor_content_length(&self, program: &str, url: &str) -> Result<u64, VendorFetchError> {
        refuse_unpinned_vendor_url(program, url)?;
        aterm_update_core::vendor_content_length(url).map_err(vendor_error)
    }

    fn vendor_download(
        &self,
        program: &str,
        url: &str,
        dest: &Path,
        cap: u64,
    ) -> Result<(), VendorFetchError> {
        refuse_unpinned_vendor_url(program, url)?;
        aterm_update_core::vendor_download_to(url, dest, cap).map_err(vendor_error)
    }

    fn source_id(&self) -> String {
        github_source_id(&self.owner)
    }
    fn package_source_id(&self, repo: &str, program: &str) -> Option<String> {
        Some(format!("github:{}", self.slug_for(program, repo)))
    }
}

/// An immutable index asset's read failed in transport: neither route delivered it
/// ([`AssetError::Unavailable`]), which a rate limit or an HTTP 5xx is even though a host
/// answered. An acquisition refusal also ends the candidate read, but is not a transport
/// failure; only [`AssetError::Absent`] permits omitting that release.
const fn signed_index_asset_error_is_transport(error: &AssetError) -> bool {
    matches!(error, AssetError::Unavailable { .. })
}

/// The §14 cache key of the GitHub source under `owner`.
pub fn github_source_id(owner: &str) -> String {
    format!("github:{owner}/{}", crate::discovery::index_repo())
}

/// A vendor head read as a hint.
pub(crate) fn vendor_hint_get(
    program: &str,
    url: &str,
    cap: u64,
    if_none_match: Option<&str>,
) -> Result<crate::flow::VendorGet, VendorFetchError> {
    refuse_unpinned_vendor_url(program, url)?;
    aterm_update_core::vendor_get_hint(url, cap, if_none_match)
        .map(vendor_get_from)
        .map_err(vendor_error)
}

fn refuse_unpinned_vendor_url(program: &str, url: &str) -> Result<(), VendorFetchError> {
    if crate::vendor::vendor_direct_url_allowed(program, url) {
        Ok(())
    } else {
        Err(VendorFetchError::Refused(format!(
            "refusing a vendor URL outside {program}'s pinned vendor-direct prefixes: {url}"
        )))
    }
}

fn vendor_error(e: aterm_update_core::HttpError) -> VendorFetchError {
    use aterm_update_core::HttpError;
    match e {
        HttpError::Transport(_)
        | HttpError::VendorStatus {
            code: 408 | 429 | 500..=599,
            ..
        } => VendorFetchError::Unreachable(e.to_string()),
        _ => VendorFetchError::Refused(e.to_string()),
    }
}

fn vendor_get_from(response: aterm_update_core::VendorResponse) -> crate::flow::VendorGet {
    match response {
        aterm_update_core::VendorResponse::Body {
            bytes,
            etag,
            effective_url,
        } => crate::flow::VendorGet::Body {
            bytes,
            etag,
            effective_url,
        },
        aterm_update_core::VendorResponse::NotModified { etag } => {
            crate::flow::VendorGet::NotModified { etag }
        }
    }
}

/// The offline / publisher-test fetcher (§14).
pub struct DirFetcher {
    dir: PathBuf,
}

impl DirFetcher {
    /// A fetcher reading assets from `dir` (canonicalized when possible for a stable `source_id`).
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        Self { dir }
    }
}

fn safe_name(n: &str) -> bool {
    !(n.is_empty()
        || n == "."
        || n == ".."
        || n.contains('/')
        || n.contains('\\')
        || n.contains('\0'))
}

impl crate::flow::Fetcher for DirFetcher {
    fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
        let cap = |n: u64| usize::try_from(n).unwrap_or(usize::MAX);
        let read = |name: &str, bound: u64| {
            crate::metadata_io::read_bounded_regular(&self.dir.join(name), cap(bound))
        };
        match (
            read("index.toml", MANIFEST_CAP),
            read("index.toml.sig", SIG_CAP),
            read(aterm_update_core::roster::ROSTER_ASSET, ROSTER_CAP),
            read(aterm_update_core::roster::ROSTER_SIG_ASSET, SIG_CAP),
        ) {
            (Ok(index_bytes), Ok(sig), Ok(roster_bytes), Ok(roster_sig)) => Ok(vec![Candidate {
                label: "dir".into(),
                index_bytes,
                sig,
                roster_bytes,
                roster_sig,
            }]),
            _ => Ok(vec![]),
        }
    }

    fn pkg_manifest(
        &self,
        _repo: &str,
        program: &str,
        build: u64,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        if !safe_name(program) {
            return Err(format!("unsafe program name {program:?}"));
        }
        let name = format!("pkg-{program}-{build}.toml");
        let toml = self.dir.join(&name);
        let sig = self.dir.join(format!("{name}.sig"));
        let raw = crate::metadata_io::read_bounded_regular(
            &toml,
            usize::try_from(MANIFEST_CAP).unwrap_or(usize::MAX),
        )
        .map_err(|e| format!("read {}: {e}", toml.display()))?;
        let sig = crate::metadata_io::read_bounded_regular(
            &sig,
            usize::try_from(SIG_CAP).unwrap_or(usize::MAX),
        )
        .map_err(|e| format!("read {}.sig: {e}", name))?;
        Ok((raw, sig))
    }

    fn download(&self, _repo: &str, asset: &str, dest: &Path) -> Result<(), String> {
        if !safe_name(asset) {
            return Err(format!("unsafe asset name {asset:?}"));
        }
        let src = self.dir.join(asset);
        let _ = std::fs::remove_file(dest);
        if std::fs::hard_link(&src, dest).is_ok() {
            return Ok(());
        }
        crate::metadata_io::copy_regular_file(&src, dest)
            .map(|_| ())
            .map_err(|e| format!("copy {}: {e}", src.display()))
    }

    fn shared(&self) -> Option<&(dyn crate::flow::Fetcher + Sync)> {
        Some(self)
    }

    fn source_id(&self) -> String {
        format!("dir:{}", self.dir.display())
    }

    fn package_source_id(&self, _repo: &str, _program: &str) -> Option<String> {
        Some(self.source_id())
    }
}

/// Conformance of the real tag walk to the derived `AtpkgIndexPublishWalk` model (the reader
/// half; the producer's suite binds the writer half).
#[cfg(test)]
mod index_publish_conformance;

/// A test's fetch of a URL, at most a cap of bytes.
#[cfg(test)]
type UrlFetch<'a> = dyn Fn(&str, u64) -> Result<Vec<u8>, String> + Sync + 'a;

/// A test's URL-keyed fetch as the asset-keyed one the walk and the manifest reads take: each
/// asset by its web route, and each failure as the reader reports it: a fixture's `HTTP 404`
/// answer is the asset absent, any other failure the read unavailable on both routes.
#[cfg(test)]
fn by_url<'a>(
    fetch: &'a UrlFetch<'a>,
) -> impl Fn(&Named, u64) -> Result<Vec<u8>, AssetError> + Sync + 'a {
    move |asset: &Named, cap: u64| {
        fetch(&asset.web_url(), cap).map_err(|why| {
            if why.starts_with("HTTP 404") {
                AssetError::Absent(why)
            } else {
                AssetError::Unavailable {
                    web: why,
                    api: "this fixture serves no API route".into(),
                }
            }
        })
    }
}

#[cfg(test)]
#[path = "net_tests.rs"]
mod tests;
