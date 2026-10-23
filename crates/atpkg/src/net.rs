// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The production GitHub-Releases [`Fetcher`](crate::flow::Fetcher) (§5/§9).

use std::path::{Path, PathBuf};

use crate::flow::VendorFetchError;
use crate::select::Candidate;

/// The download URL for asset `name` of release `tag` in `slug` (`owner/repo`), or `None` if
/// the slug is not exactly one `owner/repo` pair or any segment is not path-safe.
fn web_asset_url(slug: &str, tag: &str, name: &str) -> Option<String> {
    let (owner, repo) = slug.split_once('/')?;
    aterm_update_core::cdn::release_download_url(owner, repo, tag, name)
}

/// The one way this fetcher reads bytes from the release download host. Neither it nor the
/// transport takes a credential, so no token can reach `github.com`; an API URL is refused.
fn web_fetch_bytes(url: &str, cap: u64) -> Result<Vec<u8>, String> {
    refuse_api_host(url)?;
    aterm_update_core::download_bytes(url, cap)
}

/// [`web_fetch_bytes`] for the artifact lane: resumable, to `dest`, no credential.
fn web_fetch_to(url: &str, dest: &Path, cap: u64) -> Result<(), String> {
    refuse_api_host(url)?;
    aterm_update_core::download_to_resumable(url, dest, cap)
}

/// The web helpers' own gate: an API URL is not the download host.
fn refuse_api_host(url: &str) -> Result<(), String> {
    if aterm_update_core::cdn::is_api_host(url) {
        let mut msg = String::from("not the release download host: ");
        msg.push_str(url);
        return Err(msg);
    }
    Ok(())
}

/// Why no download URL follows for `what` from `slug`: the slug, a program or asset name,
/// or a tag is not path-safe, or an asset name carries no build number.
fn underivable(slug: &str, what: &str) -> String {
    let mut msg = String::from("no release download URL derives for ");
    msg.push_str(what);
    msg.push_str(" on ");
    msg.push_str(slug);
    msg
}

/// The `(pkg-<program>-<build>.toml, .sig)` download URLs for a build the signed index pins,
/// on the `atpkg-<program>-<build>` release.
fn direct_manifest_urls(slug: &str, program: &str, build: u64) -> Option<(String, String)> {
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
    let toml = web_asset_url(slug, &tag, &name)?;
    let mut sig = toml.clone();
    sig.push_str(".sig");
    Some((toml, sig))
}

/// The download URL for a release asset whose name carries its build (`ty-2973.tar.zst` and
/// `ty-2973-x86_64-unknown-linux-gnu.tar.zst` both live under tag `atpkg-ty-2973`).
fn direct_asset_url(slug: &str, asset: &str) -> Option<String> {
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
    web_asset_url(slug, &tag, asset)
}

/// Byte caps bounding what an attacker-controlled asset can write.
pub const MANIFEST_CAP: u64 = 5_000_000;
pub const SIG_CAP: u64 = 4_096;
const ARTIFACT_CAP: u64 = 8u64 << 30;

/// The byte cap for one artifact transfer: the row's signed `size`, clamped by
/// [`ARTIFACT_CAP`].
const fn artifact_cap(size: u64) -> u64 {
    if size == 0 || size > ARTIFACT_CAP {
        ARTIFACT_CAP
    } else {
        size
    }
}

/// A roster is a few hundred bytes per machine and at most 16 machines; `aterm-update` uses the same ceiling for the same asset.
pub const ROSTER_CAP: u64 = 65_536;

/// How many index releases one resolve fetches the signed quad for, newest first, never below
/// the lowest build the walk knows is published (the store's verified floor).
const INDEX_CANDIDATE_CAP: u64 = 4;

/// The most HEADs one index discovery's walk asks ([`WALK_AHEAD_BUDGET`] bounds the ones asked
/// ahead of it).
const WALK_HEAD_BUDGET: u32 = 48;

/// The highest power of two a store that has verified no index HEADs while it looks for the
/// channel's first build ([`newest_index`]).
const FIRST_INDEX_SEARCH_LIMIT: u64 = 1 << 20;

/// How many requests one discovery round or candidate fetch keeps in flight. Each is still
/// its own `curl` process over the unchanged single-URL calls (`-q` first, `--` before the
/// one URL, https only, capped, retried per URL); twenty of them at once measured the same
/// wall time as one batched `curl -Z` (0.25–0.48 s), where twenty in turn took four seconds
/// — a fresh process and two TLS handshakes each.
const FETCH_CONCURRENCY: usize = 20;

/// How many requests the manifest prefetch keeps in flight: both halves of every pair of a
/// set of up to 32 programs in ONE wave.
const MANIFEST_WAVE: usize = 64;

/// The most HEADs one discovery asks ahead of its walk ([`ahead_of_walk`]), beyond the walk's
/// own ([`WALK_HEAD_BUDGET`]): a host whose answers keep defeating the guess costs at most this
/// many more, and is then walked a HEAD at a time.
const WALK_AHEAD_BUDGET: usize = 2 * WALK_HEAD_BUDGET as usize;

/// `job` over every item, up to [`FETCH_CONCURRENCY`] at once, the answers in item order.
pub(crate) fn fan_out<T: Sync, R: Send>(items: &[T], job: &(dyn Fn(&T) -> R + Sync)) -> Vec<R> {
    fan_out_upto(items, FETCH_CONCURRENCY, job)
}

/// [`fan_out`], up to `width` at once.
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

/// `atpkg-index-<build>` — the index publisher's tag (`atpkg-publish index`).
fn index_tag(build: u64) -> String {
    let mut tag = String::from("atpkg-index-");
    tag.push_str(&build.to_string());
    tag
}

/// The newest published index, by HEADs of its tags alone.
///
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

/// The environment variable a release build compiles [`compiled_index_hint`] from.
pub const INDEX_HINT_ENV: &str = "ATPKG_INDEX_HINT";

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
        let number = |digits: &str| digits.parse::<u64>().ok().filter(|&n| n > 0);
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

/// The published run this binary's release cut compiled in ([`INDEX_HINT_ENV`]).
#[must_use]
pub fn compiled_index_hint() -> Option<IndexHint> {
    option_env!("ATPKG_INDEX_HINT").and_then(IndexHint::parse)
}

/// The shipped channel's published run, found as a store that has verified nothing finds it.
///
/// # Errors
/// The first HEAD that said neither, or `no atpkg-index release is published`.
pub fn published_index_run() -> Result<IndexHint, String> {
    let mut slug = String::from(aterm_update_core::ATPKG_INDEX_OWNER);
    slug.push('/');
    slug.push_str(crate::manifest::INDEX_REPO);
    let (newest, low) = newest_index(0, &mut |build| {
        head_verdict(&slug, build, &Wire).map_err(|(msg, _)| msg)
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
    index: String,
    index_sig: String,
    roster: String,
    roster_sig: String,
}

/// Derive the quad for `tag` in `slug`; `None` when the slug or tag is not URL-safe.
fn index_quad(slug: &str, tag: &str) -> Option<IndexQuad> {
    let roster = aterm_update_core::roster::ROSTER_ASSET;
    let mut roster_sig = String::from(roster);
    roster_sig.push_str(".sig");
    Some(IndexQuad {
        label: tag.to_string(),
        index: web_asset_url(slug, tag, "index.toml")?,
        index_sig: web_asset_url(slug, tag, "index.toml.sig")?,
        roster: web_asset_url(slug, tag, roster)?,
        roster_sig: web_asset_url(slug, tag, &roster_sig)?,
    })
}

/// The quads of `newest` and the builds below it, down to [`INDEX_CANDIDATE_CAP`] and never
/// below `low`, newest first.
fn fetch_run(
    slug: &str,
    (newest, low): (u64, u64),
    fetch: &WebFetch<'_>,
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
    let assets: Vec<(&str, u64)> = quads
        .iter()
        .flat_map(|quad| {
            [
                (quad.index.as_str(), MANIFEST_CAP),
                (quad.index_sig.as_str(), SIG_CAP),
                (quad.roster.as_str(), ROSTER_CAP),
                (quad.roster_sig.as_str(), SIG_CAP),
            ]
        })
        .collect();
    let mut answers = fan_out(&assets, &|&(url, cap)| fetch(url, cap)).into_iter();
    let mut take = || {
        answers
            .next()
            .unwrap_or_else(|| Err(underivable(slug, "an index asset")))
    };
    let mut out = Vec::new();
    for quad in &quads {
        match whole_quad(&quad.label, [take(), take(), take(), take()]) {
            Ok(candidate) => out.push(candidate),
            Err(e) => {
                first_error.get_or_insert(e);
            }
        }
    }
    Ok(out)
}

/// One quad's candidate from its four answers in quad order (index, its signature, roster,
/// its signature), or the first that did not arrive.
fn whole_quad(
    label: &str,
    [index, index_sig, roster, roster_sig]: [Result<Vec<u8>, String>; 4],
) -> Result<Candidate, String> {
    Ok(Candidate {
        label: label.to_string(),
        index_bytes: index?,
        sig: index_sig?,
        roster_bytes: roster?,
        roster_sig: roster_sig?,
    })
}

/// How discovery asks a redirect-refusing HEAD of one derived URL: whole, every attempt
/// (`aterm_update_core::head_no_redirect`), or — in a walk's first round, its link probe —
/// its first attempt apart from the rest, so the round reads the link from every first
/// attempt before any of them backs off ([`GithubFetcher::walk_candidates`]).
trait Heads: Sync {
    /// Every attempt of the HEAD.
    fn whole(
        &self,
        url: &str,
    ) -> Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError>;

    /// Its first attempt alone.
    fn first(&self, url: &str) -> aterm_update_core::FirstHead {
        aterm_update_core::FirstHead::Settled(self.whole(url))
    }

    /// The attempts after a first that said [`aterm_update_core::FirstHead::Again`].
    fn again(
        &self,
        url: &str,
    ) -> Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError> {
        self.whole(url)
    }

    /// Every first attempt of a round reached no host: the link is down for this process
    /// ([`aterm_update_core::note_link_down`]).
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

/// The production [`Heads`]: the transport's own, with its process-wide link verdict.
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
    ) -> Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError> {
        aterm_update_core::head_no_redirect_again(url)
    }

    fn link_down(&self) {
        aterm_update_core::note_link_down();
    }
}

/// The HEADs discovery asks through ([`Heads`]).
type HeadFn<'a> = dyn Heads + 'a;

/// A capped byte GET of one derived URL — [`web_fetch_bytes`] in production, injected by the
/// tests.
type WebFetch<'a> = dyn Fn(&str, u64) -> Result<Vec<u8>, String> + Sync + 'a;

/// One HEAD of an index tag as discovery reads it: whether the release is published, or, when
/// the answer says neither, the message the discovery fails with and whether no host answered
/// (the link down).
type HeadVerdict = Result<bool, (String, bool)>;

/// One HEAD of `atpkg-index-<build>`'s index asset in `slug`, read as the next-index probe
/// reads it ([`read_head_answer`]).
fn head_verdict(slug: &str, build: u64, head: &HeadFn<'_>) -> HeadVerdict {
    let url = index_head_url(slug, build)?;
    read_head_answer(&url, build, head.whole(&url))
}

/// The URL a HEAD of `atpkg-index-<build>` asks in `slug`.
fn index_head_url(slug: &str, build: u64) -> Result<String, (String, bool)> {
    let tag = index_tag(build);
    web_asset_url(slug, &tag, "index.toml").ok_or_else(|| (underivable(slug, &tag), false))
}

/// What the HEAD of `url`, `atpkg-index-<build>`'s index asset, answered, read as the
/// next-index probe reads it ([`crate::index_probe::classify`]): only GitHub's own 302/307 to
/// its release-asset storage is a publication, a 404 its absence, and anything else — a 200
/// from a captive portal, a 429, a 5xx — says nothing.
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
        (msg, link_down)
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
            Err((msg, false))
        }
    }
}

/// A walk's FIRST round, its link probe: every HEAD's first attempt at once, and only then
/// the rest of each that may still change ([`Heads::again`], each after its own backoff).
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
    let rest: Vec<(usize, String)> = firsts
        .iter()
        .enumerate()
        .filter_map(|(i, first)| match first {
            Ok((url, FirstHead::Again { .. })) if !unreached => Some((i, url.clone())),
            _ => None,
        })
        .collect();
    let mut again = rest
        .iter()
        .map(|(i, _)| *i)
        .zip(fan_out(&rest, &|(_, url)| head.again(url)))
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

/// The builds worth a HEAD in the next discovery round, `need` first: what the walk from
/// `floor` ([`newest_index`], replayed over the answers already `known`) asks after it under
/// every answer it may get, up to `room` builds, none already known.
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
                // A HEAD that said nothing ends this line: the walk fails there.
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

/// `(slug, program, build)`: what fully determines which asset pair a memoized manifest came
/// from.
type ManifestKey = (String, String, u64);

/// The raw, unverified `(manifest, signature)` bytes of one asset pair, shared by `Arc` so a
/// memo hit costs no copy.
type ManifestBytes = std::sync::Arc<(Vec<u8>, Vec<u8>)>;

/// The locked map behind the fetcher's `manifests` memo.
type ManifestMemo = std::sync::Mutex<std::collections::BTreeMap<ManifestKey, ManifestBytes>>;

/// The production fetcher: an `owner` account and the optional per-program `[packages.links]` `owner/repo` fetch overrides.
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
    /// Whether the last discovery's HEAD failed on the link (curl reached no host), which
    /// [`crate::flow::Fetcher::index_link_down`] reads.
    link_down: std::sync::atomic::AtomicBool,
    /// What the last discovery asked (its HEADs, the rounds they went out in, its asset GETs),
    /// for the package log's `index-resolved` line.
    walk_cost: [std::sync::atomic::AtomicU32; 3],
    /// Per-invocation memo of the raw `(pkg-<program>-<build>.toml, .sig)` bytes, keyed by
    /// `(slug, program, build)`.
    manifests: ManifestMemo,
}

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
            link_down: std::sync::atomic::AtomicBool::new(false),
            walk_cost: Default::default(),
            manifests: std::sync::Mutex::new(std::collections::BTreeMap::new()),
        }
    }

    /// Attach the per-program `owner/repo` fetch overrides (from `[packages.links]`, already slug-validated by [`crate::config::repo_overrides`]).
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

    /// Attach what this store may guess the channel publishes ([`IndexHint`]): the discovery rounds guess from it ([`ahead_of_walk`]).
    #[must_use]
    pub const fn with_index_hint(mut self, hint: Option<IndexHint>) -> Self {
        self.index_hint = hint;
        self
    }

    /// `<owner>/<repo>` under this fetcher's account.
    fn slug_of(&self, repo: &str) -> String {
        format!("{}/{repo}", self.owner)
    }

    /// The `<owner>/<repo>` slug `program`'s release fetches go to: its fetch override when one
    /// is declared, else the index-declared `repo` under this fetcher's account.
    fn slug_for(&self, program: &str, repo: &str) -> String {
        match self.overrides.get(program) {
            Some(slug) => slug.clone(),
            None => self.slug_of(repo),
        }
    }

    /// The index repo's own `owner/repo`.
    fn index_slug(&self) -> String {
        self.slug_of(&crate::discovery::index_repo())
    }

    /// [`crate::flow::Fetcher::pkg_manifest`] over an injected byte fetch.
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
        let (toml_url, sig_url) = direct_manifest_urls(&slug, program, build)
            .ok_or_else(|| underivable(&slug, program))?;
        let pair =
            std::sync::Arc::new((fetch(&toml_url, MANIFEST_CAP)?, fetch(&sig_url, SIG_CAP)?));
        if let Ok(mut memo) = self.manifests.lock() {
            memo.insert(key, std::sync::Arc::clone(&pair));
        }
        Ok((*pair).clone())
    }

    /// [`crate::flow::Fetcher::prefetch_manifests`] over an injected byte fetch: every pair
    /// of `pins` the memo does not hold yet, both halves of all of them asked at once
    /// ([`MANIFEST_WAVE`]), each under its own cap.
    fn prefetch_with(&self, pins: &[(String, String, u64)], fetch: &WebFetch<'_>) {
        let mut wanted: Vec<(ManifestKey, String, String)> = Vec::new();
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
            if let Some((toml, sig)) = direct_manifest_urls(&key.0, program, *build) {
                wanted.push((key, toml, sig));
            }
        }
        let urls: Vec<(&str, u64)> = wanted
            .iter()
            .flat_map(|(_, toml, sig)| [(toml.as_str(), MANIFEST_CAP), (sig.as_str(), SIG_CAP)])
            .collect();
        let mut fetched =
            fan_out_upto(&urls, MANIFEST_WAVE, &|&(url, cap)| fetch(url, cap)).into_iter();
        for (key, ..) in wanted {
            if let (Some(Ok(toml)), Some(Ok(sig))) = (fetched.next(), fetched.next())
                && let Ok(mut memo) = self.manifests.lock()
            {
                memo.insert(key, std::sync::Arc::new((toml, sig)));
            }
        }
    }

    /// The index candidates, from the download host alone, over an injected HEAD and byte
    /// fetch: the seam [`crate::flow::Fetcher::index_candidates`] runs.
    fn walk_candidates(
        &self,
        slug: &str,
        head: &HeadFn<'_>,
        fetch: &WebFetch<'_>,
    ) -> Result<Vec<Candidate>, String> {
        self.link_down
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let [heads_asked, rounds, gets] = &self.walk_cost;
        for count in &self.walk_cost {
            count.store(0, std::sync::atomic::Ordering::Relaxed);
        }
        let counted = |url: &str, cap: u64| {
            gets.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            fetch(url, cap)
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
                Some(Err((msg, link_down))) => {
                    if *link_down {
                        self.link_down
                            .store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    Err(msg.clone())
                }
                None => Err(underivable(slug, &index_tag(build))),
            }
        };
        let floor = self.index_floor;
        let run = newest_index(floor, &mut |build| probe_from(floor, build))?;
        let mut first_error: Option<String> = None;
        let mut out = fetch_run(slug, run, fetch, &mut first_error)?;
        if out.is_empty()
            && floor > 0
            && run.0 == floor
            && matches!(probe_from(0, floor), Ok(false))
            && let Ok(found) = newest_index(0, &mut |build| probe_from(0, build))
            && found.0 < floor - 1
        {
            out = fetch_run(slug, found, fetch, &mut first_error)?;
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
        if let Ok(memo) = self.index.lock()
            && let Some(hit) = memo.as_ref()
        {
            return Ok((**hit).clone());
        }
        let walked = self.walk_candidates(&self.index_slug(), &Wire, &web_fetch_bytes);
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

    fn pkg_manifest(
        &self,
        repo: &str,
        program: &str,
        build: u64,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        self.manifest_pair(repo, program, build, &web_fetch_bytes)
    }

    fn prefetch_manifests(&self, pins: &[(String, String, u64)]) {
        self.prefetch_with(pins, &web_fetch_bytes);
    }

    fn shared(&self) -> Option<&(dyn crate::flow::Fetcher + Sync)> {
        Some(self)
    }

    fn download(&self, repo: &str, asset: &str, dest: &Path) -> Result<(), String> {
        let slug = self.slug_of(repo);
        let url = direct_asset_url(&slug, asset).ok_or_else(|| underivable(&slug, asset))?;
        web_fetch_to(&url, dest, ARTIFACT_CAP)
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
        let url = direct_asset_url(&slug, asset).ok_or_else(|| underivable(&slug, asset))?;
        web_fetch_to(&url, dest, artifact_cap(cap))
    }

    fn download_url(&self, url: &str, dest: &Path, cap: u64) -> Result<(), String> {
        // The vendor lane: the URL is the signed row's own, already admitted by
        aterm_update_core::download_to_resumable_https_only(url, dest, cap)
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
}

/// The §14 cache key of the GitHub source under `owner`.
pub(crate) fn github_source_id(owner: &str) -> String {
    format!("github:{owner}/{}", crate::discovery::index_repo())
}

/// A vendor head read as a hint (the window's head watch, and every pass's and door's head read, [`crate::flow::Fetcher::vendor_head`]).
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

/// Refuse a vendor-direct fetch outside `program`'s compiled prefixes, before any spawn.
fn refuse_unpinned_vendor_url(program: &str, url: &str) -> Result<(), VendorFetchError> {
    if crate::vendor::vendor_direct_url_allowed(program, url) {
        Ok(())
    } else {
        Err(VendorFetchError::Refused(format!(
            "refusing a vendor URL outside {program}'s pinned vendor-direct prefixes: {url}"
        )))
    }
}

/// The transport's error, split the way the vendor-direct lane treats it: only a network
/// failure, or a status that says the host is busy (408, 429, 5xx), is unreachable; every
/// other answer is a verdict.
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

/// The transport's answer, in the flow's own type.
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

/// Reject a file-name component that could escape `dir` (defense in depth: the manifest is
/// signed).
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
        std::fs::copy(&src, dest)
            .map(|_| ())
            .map_err(|e| format!("copy {}: {e}", src.display()))
    }

    fn shared(&self) -> Option<&(dyn crate::flow::Fetcher + Sync)> {
        Some(self)
    }

    fn source_id(&self) -> String {
        format!("dir:{}", self.dir.display())
    }
}

/// Conformance of the real tag walk to the derived `AtpkgIndexPublishWalk` model (the reader
/// half; the producer's suite binds the writer half).
#[cfg(test)]
mod index_publish_conformance;

#[cfg(test)]
mod tests {
    use super::*;

    /// The synthesized URLs are the contract with the publisher's tag convention
    /// (`atpkg-<program>-<build>`): a drift sends every fetch to a 404.
    #[test]
    fn direct_urls_match_the_publishing_tag_convention() {
        let (toml, sig) = super::direct_manifest_urls("alabsystems/ty", "ty", 2973).unwrap();
        assert_eq!(
            toml,
            "https://github.com/alabsystems/ty/releases/download/atpkg-ty-2973/pkg-ty-2973.toml"
        );
        assert_eq!(
            sig,
            "https://github.com/alabsystems/ty/releases/download/atpkg-ty-2973/pkg-ty-2973.toml.sig"
        );
        assert_eq!(
            super::direct_asset_url("alabsystems/ty", "ty-2973.tar.zst").unwrap(),
            "https://github.com/alabsystems/ty/releases/download/atpkg-ty-2973/ty-2973.tar.zst"
        );
        assert_eq!(
            super::direct_asset_url("alabsystems/trust-mc", "trust-mc-20011.tar.zst").unwrap(),
            "https://github.com/alabsystems/trust-mc/releases/download/atpkg-trust-mc-20011/trust-mc-20011.tar.zst"
        );
        assert!(
            !super::direct_asset_url("alabsystems/ty", "ty-2973.tar.zst")
                .unwrap()
                .contains(".tar/")
        );
    }

    /// Every triple but `aarch64-apple-darwin` is published as `<prog>-<build>-<triple>.tar.zst`
    /// on the shared `atpkg-<prog>-<build>` release, so the tag comes from the build number,
    /// not the whole stem.
    #[test]
    fn a_triple_suffixed_asset_derives_the_shared_build_tag() {
        for (slug, asset, tag) in [
            (
                "alabsystems/trust",
                "trust-4821-x86_64-unknown-linux-gnu.tar.zst",
                "atpkg-trust-4821",
            ),
            (
                "alabsystems/trust",
                "trust-4821-aarch64-apple-darwin.tar.zst",
                "atpkg-trust-4821",
            ),
            (
                "alabsystems/trust",
                "trust-4821.tar.zst",
                "atpkg-trust-4821",
            ),
            (
                "alabsystems/trust-mc",
                "trust-mc-20011-x86_64-unknown-linux-gnu.tar.zst",
                "atpkg-trust-mc-20011",
            ),
            (
                "alabsystems/ty",
                "ty-2973-aarch64-unknown-linux-musl.tar.zst",
                "atpkg-ty-2973",
            ),
        ] {
            assert_eq!(
                super::direct_asset_url(slug, asset).as_deref(),
                Some(format!("https://github.com/{slug}/releases/download/{tag}/{asset}").as_str()),
                "{asset}"
            );
        }
        assert!(super::direct_asset_url("alabsystems/ty", "ty-latest.tar.zst").is_none());
    }

    /// Anything that could splice a synthesized URL onto another host or path derives no URL.
    #[test]
    fn direct_urls_refuse_unsafe_slugs_and_names() {
        for bad in [
            "alabsystems",
            "alabsystems/ty/extra",
            "alabsystems/",
            "/ty",
            "alabsystems/..",
            "evil.com/x/../../alabsystems/ty",
            "https://evil.com/a",
        ] {
            assert!(
                super::direct_manifest_urls(bad, "ty", 1).is_none(),
                "manifest URL built from unsafe slug {bad:?}"
            );
            assert!(
                super::direct_asset_url(bad, "ty-1.tar.zst").is_none(),
                "asset URL built from unsafe slug {bad:?}"
            );
        }
        assert!(super::direct_manifest_urls("a/b", "../ty", 1).is_none());
        assert!(super::direct_manifest_urls("a/b", "", 1).is_none());
        assert!(super::direct_asset_url("a/b", "../x.tar.zst").is_none());
        assert!(super::direct_asset_url("a/b", "noextension").is_none());
        for tag in ["v1?x", "v1#x", "v1%2f", "v 1", "", ".."] {
            assert!(
                super::web_asset_url("a/b", tag, "index.toml").is_none(),
                "{tag:?}"
            );
        }
        assert_eq!(
            super::web_asset_url("alabsystems/atpkg-index", "atpkg-index-41", "index.toml")
                .as_deref(),
            Some(
                "https://github.com/alabsystems/atpkg-index/releases/download/atpkg-index-41/index.toml"
            )
        );
    }

    /// The artifact cap is the row's signed size, clamped by the ceiling.
    #[test]
    fn artifact_cap_is_the_signed_size_clamped_by_the_ceiling() {
        assert_eq!(super::artifact_cap(30_000_000), 30_000_000);
        assert_eq!(super::artifact_cap(0), super::ARTIFACT_CAP);
        assert_eq!(
            super::artifact_cap(super::ARTIFACT_CAP + 1),
            super::ARTIFACT_CAP
        );
        assert_eq!(super::artifact_cap(u64::MAX), super::ARTIFACT_CAP);
        assert_eq!(
            super::artifact_cap(super::ARTIFACT_CAP),
            super::ARTIFACT_CAP
        );
    }

    /// No credential on the web host, structurally: `web_fetch_bytes` and `web_fetch_to` take
    /// no token (nor does the transport) and refuse an API URL before any request.
    #[test]
    fn direct_fetches_present_no_credential_to_the_web_host() {
        let api = "https://api.github.com/repos/alabsystems/ty/releases/assets/1";
        let web =
            "https://github.com/alabsystems/ty/releases/download/atpkg-ty-2973/ty-2973.tar.zst";
        let dest = std::env::temp_dir().join("atpkg-web-fetch-gate");
        assert!(super::web_fetch_bytes(api, 1024).is_err());
        assert!(super::web_fetch_to(api, &dest, 1024).is_err());
        assert!(super::refuse_api_host(web).is_ok());
        assert!(!dest.exists(), "nothing was spawned");
    }

    #[test]
    fn dir_fetcher_rejects_traversal_names() {
        use crate::flow::Fetcher as _;
        let d = crate::testdir::dir("atpkg-dirfetch-");
        let f = DirFetcher::new(d.clone());
        let dest = d.join("out");
        assert!(f.download("r", "../../etc/passwd", &dest).is_err());
        assert!(f.pkg_manifest("r", "../x", 1).is_err());
        assert!(!dest.exists(), "no file read/written outside dir");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn override_redirects_only_the_named_programs_slug() {
        let mut overrides = std::collections::BTreeMap::new();
        overrides.insert("orc".to_string(), "alabsystems/orc-private".to_string());
        let f = GithubFetcher::new("alabsystems".into()).with_overrides(overrides);
        assert_eq!(f.slug_for("orc", "orc"), "alabsystems/orc-private");
        assert_eq!(f.slug_for("orc", "some-repo"), "alabsystems/orc-private");
        assert_eq!(f.slug_for("ay", "ay"), "alabsystems/ay");
        let plain = GithubFetcher::new("alabsystems".into());
        assert_eq!(plain.slug_for("orc", "orc"), "alabsystems/orc");
    }

    #[cfg(unix)]
    #[test]
    fn dir_manifest_fifo_and_symlink_return_without_blocking() {
        use crate::flow::Fetcher as _;
        use std::os::unix::ffi::OsStrExt as _;

        let d = crate::testdir::dir("atpkg-dirfetch-special-");
        let index = d.join("index.toml");
        let index_c = std::ffi::CString::new(index.as_os_str().as_bytes()).unwrap();
        // SAFETY: `index_c` is a live NUL-terminated path in our private fixture.
        assert_eq!(unsafe { libc::mkfifo(index_c.as_ptr(), 0o600) }, 0);
        std::fs::write(d.join("index.toml.sig"), b"sig").unwrap();
        std::fs::write(d.join("aterm-machines.toml"), b"roster").unwrap();
        std::fs::write(d.join("aterm-machines.toml.sig"), b"sig").unwrap();
        let fetcher = DirFetcher::new(d.clone());
        assert!(fetcher.index_candidates().unwrap().is_empty());

        std::fs::remove_file(&index).unwrap();
        let target = d.join("index-target.toml");
        std::fs::write(&target, b"index").unwrap();
        std::os::unix::fs::symlink(&target, &index).unwrap();
        assert!(fetcher.index_candidates().unwrap().is_empty());

        let pkg = d.join("pkg-ay-1.toml");
        std::os::unix::fs::symlink(&target, &pkg).unwrap();
        std::fs::write(d.join("pkg-ay-1.toml.sig"), b"sig").unwrap();
        assert!(fetcher.pkg_manifest("ignored", "ay", 1).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A stale staging hardlink left by a killed run never truncates the registry file on the
    /// retry (see `DirFetcher::download`).
    #[test]
    fn a_stale_staging_hardlink_never_truncates_the_registry_file() {
        use crate::flow::Fetcher as _;
        let dir = crate::testdir::dir("atpkg-hl-");
        std::fs::create_dir_all(dir.join("reg")).unwrap();
        std::fs::create_dir_all(dir.join("staging")).unwrap();
        let payload = b"the registry bytes that must survive";
        let src = dir.join("reg/ay-18.tar.zst");
        std::fs::write(&src, payload).unwrap();
        let dest = dir.join("staging/ay-18.tar.zst");

        let f = DirFetcher::new(dir.join("reg"));
        f.download("r", "ay-18.tar.zst", &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), payload);
        f.download("r", "ay-18.tar.zst", &dest).unwrap();
        assert_eq!(
            std::fs::read(&src).unwrap(),
            payload,
            "the registry file must be byte-intact after a retry"
        );
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            payload,
            "and the staged copy must hold the real bytes, not an empty file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A vendor-direct fetch outside the program's own compiled prefixes is refused before any
    /// spawn, as a verdict (an allow-listed host or another vendor's prefix is not enough), and
    /// the transport's answer carries over field for field.
    #[test]
    fn the_github_fetcher_fetches_vendor_documents_only_under_the_pins() {
        use crate::flow::Fetcher as _;
        let fetcher = GithubFetcher::new("alabsystems".into());
        let dest = std::env::temp_dir().join("atpkg-vendor-unpinned-never-written");
        for (program, url) in [
            ("claude", "https://evil.example/claude-code-releases/latest"),
            (
                "claude",
                "https://github.com/alabsystems/aterm/releases/download/v1/x",
            ),
            ("claude", "https://downloads.claude.ai/other/latest"),
            (
                "claude",
                "http://downloads.claude.ai/claude-code-releases/latest",
            ),
            (
                "claude",
                "https://downloads.claude.ai/claude-code-releases/latest?x=1",
            ),
            (
                "claude",
                "https://releases.openai.com/codex/channels/latest",
            ),
            (
                "codex",
                "https://downloads.claude.ai/claude-code-releases/latest",
            ),
            (
                "ay",
                "https://downloads.claude.ai/claude-code-releases/latest",
            ),
        ] {
            let refused = |e: VendorFetchError| match e {
                VendorFetchError::Refused(_) => {}
                other => panic!("{program} {url}: {other:?}"),
            };
            refused(fetcher.vendor_get(program, url, 64, None).unwrap_err());
            refused(fetcher.vendor_content_length(program, url).unwrap_err());
            refused(
                fetcher
                    .vendor_download(program, url, &dest, 64)
                    .unwrap_err(),
            );
        }
        assert!(!dest.exists(), "a refusal writes nothing");
        assert_eq!(
            vendor_get_from(aterm_update_core::VendorResponse::Body {
                bytes: b"2.1.280\n".to_vec(),
                etag: Some("\"e\"".into()),
                effective_url: "https://downloads.claude.ai/claude-code-releases/latest".into(),
            }),
            crate::flow::VendorGet::Body {
                bytes: b"2.1.280\n".to_vec(),
                etag: Some("\"e\"".into()),
                effective_url: "https://downloads.claude.ai/claude-code-releases/latest".into(),
            }
        );
        assert_eq!(
            vendor_get_from(aterm_update_core::VendorResponse::NotModified {
                etag: "\"e\"".into()
            }),
            crate::flow::VendorGet::NotModified {
                etag: "\"e\"".into()
            }
        );
    }

    /// Only the network and a busy host are unreachable; a size, scheme, certificate or
    /// status verdict, and an unreadable answer, are refusals — so the vendor lane never
    /// files a bad document as a network blip.
    #[test]
    fn a_vendor_transport_error_is_unreachable_only_for_the_network() {
        use aterm_update_core::HttpError;
        const URL: &str = "https://downloads.claude.ai/claude-code-releases/latest";
        let status = |code: u16| HttpError::VendorStatus {
            code,
            url: URL.into(),
        };
        for e in [
            HttpError::Transport("curl GET x failed (exit 6): dns".into()),
            status(408),
            status(429),
            status(500),
            status(503),
        ] {
            let text = e.to_string();
            assert_eq!(vendor_error(e), VendorFetchError::Unreachable(text));
        }
        for e in [
            HttpError::VendorRefused("vendor document x exceeds its 64-byte cap".into()),
            HttpError::Malformed("no usable Content-Length".into()),
            status(304),
            status(403),
            status(404),
        ] {
            let text = e.to_string();
            assert_eq!(vendor_error(e), VendorFetchError::Refused(text));
        }
    }

    /// A name no download URL derives from is refused before any request on the manifest, the
    /// unsized and the sized lanes alike, and nothing is written.
    #[test]
    fn an_underivable_name_is_refused_with_no_fallback() {
        use crate::flow::Fetcher as _;
        let f = GithubFetcher::new("alabsystems".into());
        let dest = std::env::temp_dir().join(format!(
            "atpkg-underivable-never-written-{}",
            std::process::id()
        ));
        assert!(f.pkg_manifest("ty", "../ty", 1).is_err());
        assert!(f.download("ty", "ty-latest.tar.zst", &dest).is_err());
        assert!(
            f.download_for("ty", "ty", "ty-latest.tar.zst", &dest, 1024)
                .is_err()
        );
        assert!(!dest.exists(), "nothing was fetched");
        assert_eq!(
            direct_asset_url(&f.slug_for("ty", "ty"), "ty-2973.tar.zst").as_deref(),
            Some(
                "https://github.com/alabsystems/ty/releases/download/atpkg-ty-2973/ty-2973.tar.zst"
            ),
            "a published name derives its URL"
        );
    }

    /// The build number of an `atpkg-index-<n>/index.toml` download URL, for the recording
    /// HEADs below.
    fn walked_build(url: &str) -> u64 {
        url.strip_prefix("https://github.com/alabsystems/aterm/releases/download/atpkg-index-")
            .and_then(|rest| rest.strip_suffix("/index.toml"))
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("not an index tag HEAD: {url}"))
    }

    /// GitHub's own answer for a published asset.
    fn published() -> Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError> {
        Ok(aterm_update_core::HeadAnswer {
            code: 302,
            location: Some("https://release-assets.githubusercontent.com/object".into()),
        })
    }

    /// Its answer for a tag that does not exist.
    fn absent() -> Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError> {
        Ok(aterm_update_core::HeadAnswer {
            code: 404,
            location: None,
        })
    }

    /// A channel publishing exactly the builds `first..=last`.
    fn channel(
        first: u64,
        last: u64,
    ) -> impl Fn(u64) -> Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError> {
        move |build| {
            if (first..=last).contains(&build) {
                published()
            } else {
                absent()
            }
        }
    }

    /// What one discovery did.
    struct Walked {
        /// The candidates' labels, newest first, or the discovery's error.
        labels: Result<Vec<String>, String>,
        /// The builds the walk asked, in its order: the [`sequential_walk`] reference's HEADs.
        heads: Vec<u64>,
        /// Every build the fetcher HEADed (the walk's own and the ones asked ahead), sorted.
        headed: Vec<u64>,
        /// Every URL the fetcher asked: the HEADs, then the asset downloads.
        asked: Vec<String>,
        /// The asset downloads alone, sorted: the quads are fetched at once, in no order.
        fetched: Vec<String>,
        /// Whether the fetcher now reports the link down.
        link_down: bool,
        /// How many rounds the fetcher's HEADs went out in.
        rounds: u32,
    }

    /// The reference's HEAD: asked in turn, so it may keep its own record.
    type SequentialHead<'a> =
        dyn FnMut(&str) -> Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError> + 'a;

    /// The reference's byte fetch, likewise.
    type SequentialFetch<'a> = dyn FnMut(&str, u64) -> Result<Vec<u8>, String> + 'a;

    /// The sequential reference: discovery with every HEAD asked when the walk asks it and
    /// every asset in turn, a quad given up at its first missing asset.
    fn sequential_walk(
        slug: &str,
        floor: u64,
        head: &mut SequentialHead<'_>,
        fetch: &mut SequentialFetch<'_>,
        link_down: &mut bool,
    ) -> Result<Vec<Candidate>, String> {
        let mut heads = 0u32;
        let mut probe = |build: u64| -> Result<bool, String> {
            heads += 1;
            if heads > WALK_HEAD_BUDGET {
                return Err(format!(
                    "index discovery on {slug} found no end within {WALK_HEAD_BUDGET} HEADs"
                ));
            }
            let url = web_asset_url(slug, &index_tag(build), "index.toml").expect("derivable");
            let answer = head(&url).map_err(|e| {
                *link_down |= matches!(e, aterm_update_core::HttpError::Transport(_));
                format!("HEAD {url}: {e}")
            })?;
            let code = answer.code;
            match crate::index_probe::classify(build, Ok(answer)) {
                crate::index_probe::Probe::Published(_) => Ok(true),
                crate::index_probe::Probe::Missing => Ok(false),
                _ => Err(format!(
                    "HEAD {url} answered {code}, not GitHub's redirect to a release asset"
                )),
            }
        };
        let mut fetch_run = |(newest, low): (u64, u64), first_error: &mut Option<String>| {
            let oldest = newest.saturating_sub(INDEX_CANDIDATE_CAP - 1).max(low);
            let mut out = Vec::new();
            for build in (oldest..=newest).rev() {
                let quad = index_quad(slug, &index_tag(build)).expect("a derivable tag");
                let mut fetch_quad = || -> Result<Candidate, String> {
                    Ok(Candidate {
                        label: quad.label.clone(),
                        index_bytes: fetch(&quad.index, MANIFEST_CAP)?,
                        sig: fetch(&quad.index_sig, SIG_CAP)?,
                        roster_bytes: fetch(&quad.roster, ROSTER_CAP)?,
                        roster_sig: fetch(&quad.roster_sig, SIG_CAP)?,
                    })
                };
                match fetch_quad() {
                    Ok(candidate) => out.push(candidate),
                    Err(e) => {
                        first_error.get_or_insert(e);
                    }
                }
            }
            out
        };
        let run = newest_index(floor, &mut probe)?;
        let mut first_error = None;
        let mut out = fetch_run(run, &mut first_error);
        if out.is_empty()
            && floor > 0
            && run.0 == floor
            && matches!(probe(floor), Ok(false))
            && let Ok(found) = newest_index(0, &mut probe)
            && found.0 < floor - 1
        {
            out = fetch_run(found, &mut first_error);
        }
        if out.is_empty() {
            return Err(first_error.unwrap_or_else(|| underivable(slug, "the index")));
        }
        Ok(out)
    }

    /// One discovery on the shipped index repository from `floor`, every HEAD answered by
    /// `answer` and every asset download succeeding unless its URL ends with one of `missing`,
    /// run by the fetcher and by the [`sequential_walk`] reference: the two must land the same
    /// candidates (or failure) and link verdict, and the fetcher must HEAD every build the walk
    /// asked, none twice, within the walk's and the ahead budgets together.
    fn walk(
        floor: u64,
        answer: &(
             dyn Fn(u64) -> Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError>
                 + Sync
         ),
        missing: &[&str],
    ) -> Walked {
        walk_with(floor, None, answer, missing)
    }

    /// [`walk`] by a fetcher that guesses from `hint` ([`GithubFetcher::with_index_hint`]).
    fn walk_with(
        floor: u64,
        hint: Option<IndexHint>,
        answer: &(
             dyn Fn(u64) -> Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError>
                 + Sync
         ),
        missing: &[&str],
    ) -> Walked {
        use crate::flow::Fetcher as _;
        let download = |url: &str| -> Result<Vec<u8>, String> {
            if missing.iter().any(|m| url.ends_with(m)) {
                let mut msg = String::from("HTTP 404 for ");
                msg.push_str(url);
                return Err(msg);
            }
            Ok(url.as_bytes().to_vec())
        };
        let f = GithubFetcher::new("alabsystems".into())
            .with_index_floor(floor)
            .with_index_hint(hint);
        let headed = std::sync::Mutex::new(Vec::<String>::new());
        let fetched = std::sync::Mutex::new(Vec::<String>::new());
        let labels = f
            .walk_candidates(
                &f.index_slug(),
                &|url: &str| {
                    headed.lock().unwrap().push(url.to_string());
                    answer(walked_build(url))
                },
                &|url: &str, _cap: u64| {
                    fetched.lock().unwrap().push(url.to_string());
                    download(url)
                },
            )
            .map(|candidates| candidates.into_iter().map(|c| c.label).collect::<Vec<_>>());
        let mut heads = Vec::new();
        let mut reference_down = false;
        let reference = sequential_walk(
            &f.index_slug(),
            floor,
            &mut |url: &str| {
                heads.push(walked_build(url));
                answer(walked_build(url))
            },
            &mut |url: &str, _cap: u64| download(url),
            &mut reference_down,
        )
        .map(|candidates| candidates.into_iter().map(|c| c.label).collect::<Vec<_>>());
        assert_eq!(
            labels, reference,
            "from {floor}: the rounds land the reference's set"
        );
        assert_eq!(
            f.index_link_down(),
            reference_down,
            "from {floor}: the link verdict"
        );
        let headed = headed.into_inner().unwrap();
        let mut builds: Vec<u64> = headed.iter().map(|url| walked_build(url)).collect();
        builds.sort_unstable();
        assert!(
            builds.windows(2).all(|pair| pair[0] < pair[1]),
            "from {floor}: no build is HEADed twice: {builds:?}"
        );
        for build in &heads {
            assert!(
                builds.contains(build),
                "from {floor}: the walk's {build} was asked"
            );
        }
        assert!(
            builds.len() <= WALK_HEAD_BUDGET as usize + WALK_AHEAD_BUDGET,
            "from {floor}: {} HEADs",
            builds.len()
        );
        let mut fetched = fetched.into_inner().unwrap();
        let mut asked = headed;
        asked.extend(fetched.iter().cloned());
        fetched.sort();
        Walked {
            labels,
            heads,
            headed: builds,
            asked,
            fetched,
            link_down: f.index_link_down(),
            rounds: f.walk_cost[1].load(std::sync::atomic::Ordering::Relaxed),
        }
    }

    fn labels(builds: &[u64]) -> Result<Vec<String>, String> {
        Ok(builds.iter().map(|b| index_tag(*b)).collect())
    }

    /// Every asset URL of the quads of `builds`, sorted: what [`Walked::fetched`] holds when
    /// each of those quads was asked whole.
    fn quad_assets(builds: &[u64]) -> Vec<String> {
        let mut urls: Vec<String> = builds
            .iter()
            .flat_map(|b| {
                let q = index_quad("alabsystems/aterm", &index_tag(*b)).unwrap();
                [q.index, q.index_sig, q.roster, q.roster_sig]
            })
            .collect();
        urls.sort();
        urls
    }

    /// From a verified floor of 44, with 45 published and 46 onward absent, the walk resolves
    /// 45 (and 44) in three HEADs and eight downloads, all on the release download host.
    #[test]
    fn the_walk_resolves_the_next_index_with_no_api_request() {
        assert!(aterm_update_core::cdn::is_api_host(
            "https://api.github.com/repos/alabsystems/aterm/releases?per_page=100&page=1"
        ));
        let w = walk(44, &channel(6, 45), &[]);
        assert_eq!(w.labels, labels(&[45, 44]));
        assert_eq!(w.heads, vec![45, 46, 47]);
        assert_eq!(
            w.headed,
            vec![45, 46, 47],
            "nothing asked past the walk's own"
        );
        assert_eq!(
            w.asked.len(),
            3 + 8,
            "three HEADs, four assets per candidate"
        );
        assert_eq!(w.fetched, quad_assets(&[45, 44]), "both quads, whole");
        for url in &w.asked {
            assert!(!aterm_update_core::cdn::is_api_host(url), "{url}");
            assert!(
                url.starts_with("https://github.com/alabsystems/aterm/releases/download/"),
                "{url}"
            );
        }
        assert!(!w.link_down);
    }

    /// The walk from every kind of floor: two HEADs and one quad in the steady state; a gallop
    /// and bisection for a store five builds behind; and for a store that has verified nothing,
    /// a search from the first power of two that answers.
    #[test]
    fn the_walk_finds_the_newest_from_any_floor_and_from_none() {
        let steady = walk(45, &channel(6, 45), &[]);
        assert_eq!(steady.labels, labels(&[45]));
        assert_eq!(steady.heads, vec![46, 47], "two HEADs in the steady state");
        assert_eq!(steady.headed, vec![46, 47], "and none asked ahead of them");

        let behind = walk(40, &channel(6, 45), &[]);
        assert_eq!(
            behind.labels,
            labels(&[45, 44, 43, 42]),
            "newest first, capped"
        );

        let fresh = walk(0, &channel(6, 45), &[]);
        assert_eq!(fresh.labels, labels(&[45, 44, 43, 42]));

        let empty = walk(0, &|_| absent(), &[]);
        assert!(empty.labels.is_err());
        assert_eq!(
            empty.headed, empty.heads,
            "the search asks the powers alone"
        );
        assert!(
            empty
                .asked
                .iter()
                .all(|u| !aterm_update_core::cdn::is_api_host(u))
        );
    }

    /// Exhaustively over small contiguous channels: for every floor the walk lands the newest
    /// within its HEAD bound, and the lower bound it reports is published.
    #[test]
    fn the_search_lands_the_newest_of_every_contiguous_channel() {
        for first in 1..=9u64 {
            for newest in first..=80u64 {
                let floors = std::iter::once(0).chain(first..=newest);
                for floor in floors {
                    if floor == 0 && !(first..=newest).any(u64::is_power_of_two) {
                        continue;
                    }
                    let mut probes = 0u32;
                    let found = newest_index(floor, &mut |build| {
                        probes += 1;
                        Ok((first..=newest).contains(&build))
                    });
                    let (hit, low) = found
                        .unwrap_or_else(|e| panic!("channel {first}..={newest} from {floor}: {e}"));
                    assert_eq!(hit, newest, "channel {first}..={newest} from {floor}");
                    assert!(
                        (first..=newest).contains(&low),
                        "{first}..={newest} from {floor}"
                    );
                    let distance = u32::try_from(newest - low + 1).unwrap();
                    assert!(
                        probes <= 2 * (distance.ilog2() + 1) + 22,
                        "{probes} HEADs for channel {first}..={newest} from {floor}"
                    );
                }
            }
        }
    }

    /// A single missing number is stepped over: exhaustively over small channels with every
    /// pattern of non-adjacent gaps, from every floor, the walk lands the newest.
    #[test]
    fn the_search_steps_over_a_missing_number_and_stops_at_two() {
        for first in 1..=3u64 {
            for newest in first..=first + 16 {
                let interior = (newest - first).saturating_sub(1);
                for gaps in 0..(1u64 << interior) {
                    if gaps & (gaps >> 1) != 0 {
                        // two adjacent missing numbers: the control below
                        continue;
                    }
                    let published = |build: u64| {
                        (first..=newest).contains(&build)
                            && (build == first
                                || build == newest
                                || gaps >> (build - first - 1) & 1 == 0)
                    };
                    let floors =
                        std::iter::once(0).chain((first..=newest).filter(|&b| published(b)));
                    for floor in floors {
                        if floor == 0
                            && !(first..=newest).any(|b| b.is_power_of_two() && published(b))
                        {
                            continue;
                        }
                        let mut probes = 0u32;
                        let found = newest_index(floor, &mut |build| {
                            probes += 1;
                            Ok(published(build))
                        });
                        let (hit, low) = found.unwrap_or_else(|e| {
                            panic!("channel {first}..={newest} gaps {gaps:b} from {floor}: {e}")
                        });
                        assert_eq!(
                            hit, newest,
                            "channel {first}..={newest} gaps {gaps:b} from {floor}"
                        );
                        assert!(
                            published(low),
                            "{first}..={newest} gaps {gaps:b} from {floor}"
                        );
                        assert!(probes <= WALK_HEAD_BUDGET, "{probes} HEADs");
                    }
                }
            }
        }
        let gap = |build: u64| {
            if build == 45 {
                absent()
            } else {
                channel(6, 50)(build)
            }
        };
        let w = walk(44, &gap, &[]);
        assert_eq!(w.labels, labels(&[50, 49, 48, 47]));
        assert_eq!(&w.heads[..2], &[45, 46], "the step past the missing number");
        let w = walk(
            44,
            &|b| if b == 48 { absent() } else { channel(6, 50)(b) },
            &["atpkg-index-48/index.toml"],
        );
        assert_eq!(
            w.labels,
            labels(&[50, 49, 47]),
            "a missing number inside the window"
        );
        let double = |build: u64| {
            if build == 45 || build == 46 {
                absent()
            } else {
                channel(6, 50)(build)
            }
        };
        let w = walk(44, &double, &[]);
        assert_eq!(w.labels, labels(&[44]));
        assert_eq!(w.heads, vec![45, 46]);
        for first in 1..=3u64 {
            for newest in first + 3..=first + 16 {
                for floor in first..newest - 2 {
                    let found = newest_index(floor, &mut |build| {
                        Ok((first..=newest).contains(&build)
                            && build != floor + 1
                            && build != floor + 2)
                    });
                    assert_eq!(found, Ok((floor, floor)), "{first}..={newest} from {floor}");
                }
            }
        }
    }

    /// An answer that says nothing fails the discovery, never reads as "not published" (which
    /// would land an older index) nor leads to another host.
    #[test]
    fn an_answer_that_says_nothing_fails_the_discovery() {
        let answer = |code: u16, location: Option<&str>| {
            let location = location.map(str::to_string);
            move |_: u64| {
                Ok(aterm_update_core::HeadAnswer {
                    code,
                    location: location.clone(),
                })
            }
        };
        for (why, reply) in [
            ("a portal page", answer(200, None)),
            (
                "a renamed repo",
                answer(
                    301,
                    Some("https://github.com/new/aterm/releases/download/x/index.toml"),
                ),
            ),
            (
                "a login redirect",
                answer(302, Some("https://sso.example.com/login")),
            ),
            ("a rate limit", answer(429, None)),
            ("an outage", answer(503, None)),
        ] {
            let w = walk(44, &reply, &[]);
            assert!(w.labels.is_err(), "{why}");
            assert_eq!(w.heads, vec![45], "{why}");
            assert!(!w.link_down, "{why}: a host answered");
        }
        let offline = walk(
            44,
            &|_| {
                Err(aterm_update_core::HttpError::Transport(
                    "curl: (6) Could not resolve host".into(),
                ))
            },
            &[],
        );
        assert!(
            offline
                .labels
                .unwrap_err()
                .contains("Could not resolve host")
        );
        assert_eq!(offline.heads, vec![45]);
        assert_eq!(offline.asked.len(), offline.headed.len(), "nothing fetched");
        assert!(offline.link_down, "no host answered");
        use crate::flow::Fetcher as _;
        let f = GithubFetcher::new("alabsystems".into()).with_index_floor(44);
        let slug = f.index_slug();
        let fetch = |url: &str, _: u64| Ok(url.as_bytes().to_vec());
        let dead = |_: &str| {
            Err(aterm_update_core::HttpError::Transport(
                "curl: (6) Could not resolve host".into(),
            ))
        };
        assert!(f.walk_candidates(&slug, &dead, &fetch).is_err());
        assert!(f.index_link_down());
        let busy = |_: &str| {
            Ok(aterm_update_core::HeadAnswer {
                code: 503,
                location: None,
            })
        };
        assert!(f.walk_candidates(&slug, &busy, &fetch).is_err());
        assert!(!f.index_link_down(), "a host answered this walk");
    }

    /// A release whose four assets do not all arrive is left out, not the resolve's failure;
    /// only when no candidate arrives does the resolve fail, naming the first missing asset.
    #[test]
    fn a_release_that_does_not_arrive_whole_is_left_out() {
        let w = walk(44, &channel(6, 46), &["atpkg-index-46/index.toml.sig"]);
        assert_eq!(w.labels, labels(&[45, 44]));
        assert_eq!(
            w.fetched,
            quad_assets(&[46, 45, 44]),
            "every quad of the window asked whole, the incomplete one too"
        );
        let w = walk(44, &channel(6, 46), &["atpkg-index-46/aterm-machines.toml"]);
        assert_eq!(w.labels, labels(&[45, 44]), "or its roster");
        let w = walk(44, &channel(6, 46), &["atpkg-index-45/index.toml.sig"]);
        assert_eq!(w.labels, labels(&[46, 44]), "an older one likewise");
        let w = walk(45, &channel(6, 45), &["/index.toml"]);
        let err = w.labels.unwrap_err();
        assert!(err.contains("atpkg-index-45/index.toml"), "{err}");
    }

    /// A floor above the published run (a rostered machine signed an `index_build` above every
    /// tag): with nothing above it and its own tag absent, the walk finds the run again from
    /// none.
    #[test]
    fn a_floor_above_the_published_run_is_found_again_from_first_contact() {
        let poisoned = i64::MAX.unsigned_abs();
        let unpublished = format!("atpkg-index-{poisoned}/index.toml");
        let w = walk(poisoned, &channel(6, 46), &[unpublished.as_str()]);
        assert_eq!(w.labels, labels(&[46, 45, 44, 43]));
        let w = walk(45, &channel(6, 44), &["atpkg-index-45/index.toml"]);
        let err = w.labels.unwrap_err();
        assert!(err.contains("atpkg-index-45/index.toml"), "{err}");
    }

    /// What [`SignedChannel`] publishes for one pass.
    #[derive(Clone)]
    struct Publication {
        /// The builds whose releases are up; `None` is the link down.
        run: Option<std::ops::RangeInclusive<u64>>,
        /// The first build published beside the next roster generation.
        newer_roster_from: u64,
        /// The first build whose index declares that generation.
        newer_index_from: u64,
        /// The first build whose index a key on no roster signed.
        outsider_from: u64,
    }

    /// The channel as discovery meets it: the real walk from a floor of 47 over HEADs and
    /// downloads of genuinely signed quads (the test roster's machine and paper master).
    struct SignedChannel {
        fetcher: GithubFetcher,
        publication: std::cell::RefCell<Publication>,
        /// The builds up to which an index's freshness window has already lapsed.
        lapsed_through: std::cell::Cell<u64>,
    }

    impl crate::flow::Fetcher for SignedChannel {
        fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
            use crate::sig::testkit;
            let p = self.publication.borrow().clone();
            let lapsed_through = self.lapsed_through.get();
            let up = |build: u64| p.run.as_ref().is_some_and(|run| run.contains(&build));
            let generation =
                |newer_from: u64, build: u64| testkit::SEQ + u64::from(build >= newer_from);
            self.fetcher.walk_candidates(
                &self.fetcher.index_slug(),
                &|url: &str| {
                    if p.run.is_none() {
                        return Err(aterm_update_core::HttpError::Transport(
                            "curl: (6) Could not resolve host".into(),
                        ));
                    }
                    if up(walked_build(url)) {
                        published()
                    } else {
                        absent()
                    }
                },
                &|url: &str, _cap: u64| {
                    let (build, asset) = url
                        .split_once("/atpkg-index-")
                        .and_then(|(_, rest)| rest.split_once('/'))
                        .and_then(|(n, asset)| Some((n.parse::<u64>().ok()?, asset)))
                        .unwrap_or_else(|| panic!("not an index asset: {url}"));
                    if !up(build) {
                        return Err(format!("HTTP 404 for {url}"));
                    }
                    let roster = aterm_update_core::roster::Roster {
                        roster_seq: generation(p.newer_roster_from, build),
                        ..testkit::roster()
                    }
                    .to_toml()
                    .expect("a valid roster emits")
                    .into_bytes();
                    let valid_until = if build <= lapsed_through {
                        "2020-01-01T00:00:00Z"
                    } else {
                        "2099-01-01T00:00:00Z"
                    };
                    let index = format!(
                        "schema = 2\nindex_build = {build}\nvalid_until = \"{valid_until}\"\n\
                         machine_id = \"{}\"\nroster_seq = {}\n[programs.ay]\nrepo = \"ay\"\n",
                        testkit::MACHINE_ID,
                        generation(p.newer_index_from, build),
                    )
                    .into_bytes();
                    Ok(match asset {
                        "index.toml" => index,
                        "index.toml.sig" if build >= p.outsider_from => {
                            testkit::sign(&testkit::OUTSIDER_SEED, &index)
                        }
                        "index.toml.sig" => testkit::sign(&testkit::MACHINE_SEED, &index),
                        "aterm-machines.toml" => roster,
                        _ => testkit::sign(&testkit::MASTER_SEED, &roster),
                    })
                },
            )
        }
        fn index_link_down(&self) -> bool {
            self.fetcher.index_link_down()
        }
        fn pkg_manifest(&self, _: &str, _: &str, _: u64) -> Result<(Vec<u8>, Vec<u8>), String> {
            Err("not asked".into())
        }
        fn download(&self, _: &str, _: &str, _: &std::path::Path) -> Result<(), String> {
            Err("not asked".into())
        }
        fn source_id(&self) -> String {
            github_source_id("alabsystems")
        }
    }

    /// A run found below the floor never displaces the cached floor index: with 47 and then 46
    /// deleted for a re-cut, every candidate of the run found at 45 is refused (same
    /// generation, below the floor), and the resolve serves the cached 47, caches it beside
    /// what was served, and serves it offline after; a refused run above the floor is cached
    /// beside it for `doctor` to name.
    #[test]
    fn a_run_found_below_the_floor_keeps_serving_the_cached_floor_index() {
        use crate::sig::testkit;
        let store = crate::testdir::absent_layout("atpkg-net-below-floor-");
        let f = SignedChannel {
            fetcher: GithubFetcher::new("alabsystems".into()).with_index_floor(47),
            publication: std::cell::RefCell::new(Publication {
                run: None,
                newer_roster_from: u64::MAX,
                newer_index_from: u64::MAX,
                outsider_from: u64::MAX,
            }),
            lapsed_through: std::cell::Cell::new(0),
        };
        let pass = |run: Option<std::ops::RangeInclusive<u64>>,
                    roster_from: u64,
                    index_from: u64,
                    outsider_from| {
            *f.publication.borrow_mut() = Publication {
                run,
                newer_roster_from: roster_from,
                newer_index_from: index_from,
                outsider_from,
            };
            let floor = crate::sig::BuildFloor {
                index_build: 47,
                roster_seq: testkit::SEQ,
            };
            let index = crate::flow::resolve_verified_index(
                &f,
                &store,
                &testkit::anchor(),
                floor,
                testkit::NOW,
            );
            let cached = crate::cache::IndexCache::for_layout(&store)
                .labels(&github_source_id("alabsystems"))
                .unwrap_or_default();
            (
                index.map(|i| i.index_build).map_err(|e| e.to_string()),
                crate::flow::last_resolve().is_some_and(|r| r.reached),
                cached,
            )
        };
        let cached = |builds: &[u64]| labels(builds).unwrap();
        let never = u64::MAX;
        assert_eq!(
            pass(Some(6..=47), never, never, never),
            (Ok(47), true, cached(&[47]))
        );
        assert_eq!(
            crate::sig::Floor::new(store.roster_floor()).current(),
            testkit::SEQ
        );
        let from_cache = (Ok(47), false, cached(&[47]));
        assert_eq!(pass(Some(6..=46), never, never, never), from_cache);
        assert_eq!(pass(None, never, never, never), from_cache);
        let beside_run = (Ok(47), false, cached(&[45, 44, 43, 42, 47]));
        assert_eq!(pass(Some(6..=45), never, never, never), beside_run);
        assert_eq!(pass(None, never, never, never), beside_run);
        assert_eq!(
            pass(Some(6..=47), never, never, never),
            (Ok(47), true, cached(&[47]))
        );
        let beside_refused = (Ok(47), false, cached(&[51, 50, 49, 48, 47]));
        assert_eq!(pass(Some(6..=51), never, never, 48), beside_refused);
        assert_eq!(pass(None, never, never, 48), beside_refused);
        f.lapsed_through.set(47);
        let stale = |builds: &[u64], reached| {
            (
                Err(crate::flow::FlowError::Stale.to_string()),
                reached,
                cached(builds),
            )
        };
        assert_eq!(pass(Some(6..=47), never, never, never), stale(&[47], true));
        assert_eq!(
            pass(Some(6..=51), never, never, 48),
            stale(&[51, 50, 49, 48, 47], false)
        );
        f.lapsed_through.set(0);
        assert_eq!(
            pass(Some(6..=47), never, never, never),
            (Ok(47), true, cached(&[47]))
        );
        let refused = |builds: &[u64]| {
            (
                Err(crate::flow::FlowError::NoIndex.to_string()),
                true,
                cached(builds),
            )
        };
        assert_eq!(pass(Some(6..=48), 48, never, 48), refused(&[48, 47]));
        assert_eq!(
            pass(Some(6..=45), 0, never, never),
            refused(&[45, 44, 43, 42])
        );
        assert_eq!(
            pass(Some(6..=45), 45, 45, never),
            (Ok(45), true, cached(&[45, 44, 43, 42]))
        );
    }

    /// The rounds against the reference over a fixed pseudo-random stream of channels the cases
    /// above do not reach: starting from 1 to 40, up to 80 builds long, with up to three missing
    /// numbers (sometimes adjacent, where the walk's answer depends on where it looks), each
    /// walked from no floor, a floor in the run, its top or above it, with one roster missing.
    #[test]
    fn the_rounds_land_what_the_sequential_walk_lands() {
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = |bound: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % bound
        };
        for _ in 0..300 {
            let first = 1 + next(40);
            let last = first + next(80);
            let holes: Vec<u64> = (0..next(4))
                .map(|_| first + next(last - first + 1))
                .collect();
            let floor = match next(4) {
                0 => 0,
                1 => first + next(last - first + 1),
                2 => last,
                _ => last + 1 + next(1000),
            };
            let missing = format!(
                "atpkg-index-{}/aterm-machines.toml",
                first + next(last - first + 1)
            );
            let up = move |b: u64| (first..=last).contains(&b) && !holes.contains(&b);
            let newest = match next(5) {
                0 => None,
                1 => Some(first + next(last - first + 1)),
                2 => Some(last),
                3 => Some(first.saturating_sub(next(3))),
                _ => Some(last + 1 + next(50)),
            };
            let low = match next(3) {
                0 => None,
                1 => Some(first.next_power_of_two()),
                _ => Some(1 + next(last + 10)),
            };
            let hint = newest.map(|newest| IndexHint { low, newest });
            walk_with(
                floor,
                hint,
                &move |b| if up(b) { published() } else { absent() },
                &[missing.as_str()],
            );
        }
    }

    /// The run a release cut compiles in (`ATPKG_INDEX_HINT`) reads back as that run; a value
    /// that names no run reads as no hint, and the binary walks as a development build does.
    #[test]
    fn a_compiled_index_hint_reads_back_or_is_no_hint() {
        let run = IndexHint {
            low: Some(8),
            newest: 72,
        };
        assert_eq!(IndexHint::parse(&run.to_string()), Some(run));
        assert_eq!(
            IndexHint::parse(" 72\n"),
            Some(IndexHint {
                low: None,
                newest: 72
            })
        );
        for refused in [
            "",
            "0",
            "x",
            "9..8",
            "0..8",
            "8..",
            "..8",
            "-1..8",
            "8..72..80",
        ] {
            assert_eq!(IndexHint::parse(refused), None, "{refused:?}");
        }
    }

    /// How many rounds discovery takes, which is its wall time: a round costs one HEAD's latency
    /// however many it asks.
    #[test]
    fn discovery_takes_a_few_rounds_where_the_walk_takes_many_heads() {
        /// Rounds and HEADs from `floor` to a channel publishing 6 to `newest`, guessing from
        /// `hint`.
        fn hinted_rounds(floor: u64, newest: u64, hint: Option<IndexHint>) -> (usize, usize) {
            let mut known = std::collections::BTreeMap::<u64, HeadVerdict>::new();
            let mut rounds = 0;
            loop {
                let mut need = None;
                let found = newest_index(floor, &mut |b| match known.get(&b) {
                    Some(verdict) => verdict.clone().map_err(|(msg, _)| msg),
                    None => {
                        need = Some(b);
                        Err(String::new())
                    }
                });
                let Some(build) = need else {
                    assert_eq!(found, Ok((newest, if floor == 0 { 8 } else { floor })));
                    return (rounds, known.len());
                };
                rounds += 1;
                for b in ahead_of_walk(floor, build, &known, FETCH_CONCURRENCY, hint) {
                    known.insert(b, Ok((6..=newest).contains(&b)));
                }
            }
        }
        let rounds = |floor: u64, newest: u64| hinted_rounds(floor, newest, None);
        assert_eq!(rounds(71, 71), (1, 2), "the steady state");
        assert_eq!(rounds(70, 71).0, 2, "one index behind");
        assert_eq!(
            rounds(0, 71).0,
            3,
            "first contact with the channel as it stands"
        );
        let mut worst = 0;
        for newest in 8..1000_u64 {
            for floor in [0, newest, newest - 1, newest.saturating_sub(5).max(8), 8] {
                worst = worst.max(rounds(floor, newest).0);
            }
        }
        assert!(worst <= 5, "{worst} rounds");
        assert_eq!(
            walk(71, &channel(6, 71), &[]).rounds,
            1,
            "the fetcher, steady"
        );
        assert_eq!(
            walk(0, &channel(6, 71), &[]).rounds,
            3,
            "the fetcher, first contact"
        );
        let seen = |newest| Some(IndexHint { low: None, newest });
        assert_eq!(
            hinted_rounds(0, 71, seen(71)).0,
            2,
            "first contact, cache current"
        );
        assert_eq!(walk_with(0, seen(71), &channel(6, 71), &[]).rounds, 2);
        for (newest, hint) in [(72, 71), (75, 71), (140, 71), (71, 90), (71, 3), (300, 299)] {
            assert!(
                hinted_rounds(0, newest, seen(hint)).0 <= 5,
                "{newest} hinted {hint}"
            );
        }
        let run = |newest| {
            Some(IndexHint {
                low: Some(8),
                newest,
            })
        };
        assert_eq!(
            hinted_rounds(0, 71, run(71)),
            (1, 20),
            "first contact, release-cut run current"
        );
        assert_eq!(walk_with(0, run(71), &channel(6, 71), &[]).rounds, 1);
        for newest in 72..=103 {
            assert_eq!(hinted_rounds(0, newest, run(71)).0, 2, "moved to {newest}");
        }
        for newest in 71..=103 {
            let want = if newest <= 76 { 2 } else { 3 };
            assert_eq!(
                hinted_rounds(0, newest, seen(71)).0,
                want,
                "cache, {newest}"
            );
        }
        assert_eq!(hinted_rounds(70, 71, seen(71)).0, 1, "one behind, woken");
        assert_eq!(walk_with(70, seen(71), &channel(6, 71), &[]).rounds, 1);
        for hint in [71, 70, 3] {
            assert_eq!(
                hinted_rounds(71, 71, seen(hint)),
                (1, 2),
                "floor 71, hint {hint}"
            );
        }
        assert_eq!(
            walk_with(71, seen(71), &channel(6, 71), &[]).headed,
            [72, 73]
        );
    }

    /// HEADs whose first attempts are scripted apart from the rest ([`Heads`]), each call
    /// recorded: what a walk's first round asks of the transport.
    struct ScriptedHeads<'a> {
        first: &'a (dyn Fn(u64) -> aterm_update_core::FirstHead + Sync),
        rest: &'a (
                dyn Fn(u64) -> Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError>
                    + Sync
            ),
        firsts: std::sync::Mutex<Vec<u64>>,
        agains: std::sync::Mutex<Vec<u64>>,
        wholes: std::sync::Mutex<Vec<u64>>,
        downs: std::sync::atomic::AtomicU32,
    }

    impl<'a> ScriptedHeads<'a> {
        fn new(
            first: &'a (dyn Fn(u64) -> aterm_update_core::FirstHead + Sync),
            rest: &'a (
                    dyn Fn(
                u64,
            )
                -> Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError>
                        + Sync
                ),
        ) -> Self {
            Self {
                first,
                rest,
                firsts: std::sync::Mutex::default(),
                agains: std::sync::Mutex::default(),
                wholes: std::sync::Mutex::default(),
                downs: std::sync::atomic::AtomicU32::new(0),
            }
        }

        fn sorted(calls: &std::sync::Mutex<Vec<u64>>) -> Vec<u64> {
            let mut calls = calls.lock().unwrap().clone();
            calls.sort_unstable();
            calls
        }
    }

    impl Heads for ScriptedHeads<'_> {
        fn whole(
            &self,
            url: &str,
        ) -> Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError> {
            self.wholes.lock().unwrap().push(walked_build(url));
            (self.rest)(walked_build(url))
        }

        fn first(&self, url: &str) -> aterm_update_core::FirstHead {
            self.firsts.lock().unwrap().push(walked_build(url));
            (self.first)(walked_build(url))
        }

        fn again(
            &self,
            url: &str,
        ) -> Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError> {
            self.agains.lock().unwrap().push(walked_build(url));
            (self.rest)(walked_build(url))
        }

        fn link_down(&self) {
            self.downs
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// A WALK'S FIRST ROUND IS ITS LINK PROBE. When every first attempt in it reached no host,
    /// the walk fails then and there: the link is held down once for the process, and no HEAD
    /// backs off to try again — the offline first launch's whole wait.
    #[test]
    fn a_walk_whose_first_round_reaches_no_host_is_offline_at_once() {
        use crate::flow::Fetcher as _;
        use aterm_update_core::{FirstHead, HttpError};
        let refused =
            || HttpError::Transport("curl: (7) Failed to connect to 127.0.0.1 port 9".into());
        let fetch = |url: &str, _: u64| Ok(url.as_bytes().to_vec());
        let up = channel(6, 71);
        for floor in [0, 71] {
            let f = GithubFetcher::new("alabsystems".into()).with_index_floor(floor);
            let slug = f.index_slug();
            let unreached = |_| FirstHead::Again {
                answer: Err(refused()),
                unreached: true,
            };
            let dead = |_| Err(refused());
            let heads = ScriptedHeads::new(&unreached, &dead);
            let err = f.walk_candidates(&slug, &heads, &fetch).unwrap_err();
            assert!(err.contains("Failed to connect"), "{err}");
            assert!(f.index_link_down(), "from {floor}");
            assert_eq!(heads.downs.load(std::sync::atomic::Ordering::Relaxed), 1);
            assert!(!heads.firsts.lock().unwrap().is_empty());
            assert!(
                heads.agains.lock().unwrap().is_empty() && heads.wholes.lock().unwrap().is_empty(),
                "from {floor}: no HEAD retried a link that is not there"
            );
            let lone = if floor == 0 { 8 } else { 72 };
            let one_unreached = |b: u64| {
                if b == lone {
                    FirstHead::Again {
                        answer: Err(refused()),
                        unreached: true,
                    }
                } else {
                    FirstHead::Settled(up(b))
                }
            };
            let heads = ScriptedHeads::new(&one_unreached, &up);
            let labels = f
                .walk_candidates(&slug, &heads, &fetch)
                .map(|c| c.into_iter().map(|c| c.label).collect::<Vec<_>>());
            assert_eq!(labels, walk(floor, &up, &[]).labels, "from {floor}");
            assert_eq!(ScriptedHeads::sorted(&heads.agains), vec![lone]);
            assert_eq!(heads.downs.load(std::sync::atomic::Ordering::Relaxed), 0);
            assert!(!f.index_link_down());
            let timed_out = |_| FirstHead::Again {
                answer: Err(HttpError::Transport(
                    "curl: (28) Operation timed out".into(),
                )),
                unreached: false,
            };
            let heads = ScriptedHeads::new(&timed_out, &up);
            assert!(f.walk_candidates(&slug, &heads, &fetch).is_ok());
            assert_eq!(
                ScriptedHeads::sorted(&heads.agains),
                ScriptedHeads::sorted(&heads.firsts),
                "from {floor}"
            );
            assert_eq!(heads.downs.load(std::sync::atomic::Ordering::Relaxed), 0);
        }
    }

    /// The scratch directory [`probe_a_first_round_over_counted_curl`] runs in, set only for
    /// the child [`a_first_rounds_curls_are_one_per_head_offline_and_whole_online`] starts.
    #[cfg(unix)]
    const LINK_PROBE_ENV: &str = "ATPKG_TEST_LINK_PROBE_DIR";
    /// That probe's name, as `--exact` wants it.
    #[cfg(unix)]
    const LINK_PROBE: &str = "net::tests::probe_a_first_round_over_counted_curl";

    /// The child's half: a first round of three HEADs over the PRODUCTION transport ([`Wire`],
    /// [`aterm_update_core::head_no_redirect_first`]), its curl the counting wrapper the
    /// parent laid ([`aterm_update_core::CURL_SEAM`]), then one whole HEAD of a fourth build —
    /// what the round's link verdict leaves a request after it.
    #[cfg(unix)]
    #[test]
    fn probe_a_first_round_over_counted_curl() {
        let Some(dir) = std::env::var_os(LINK_PROBE_ENV) else {
            return;
        };
        let slug = "alabsystems/aterm";
        let round = probe_round(slug, &[1, 2, 4], &Wire);
        let after = aterm_update_core::head_no_redirect(&index_head_url(slug, 8).unwrap());
        let said = |verdict: &HeadVerdict| match verdict {
            Ok(published) => format!("ok {published}"),
            Err((_, link_down)) => format!("err link_down={link_down}"),
        };
        let mut out: Vec<String> = round.iter().map(said).collect();
        out.push(match after {
            Ok(answer) => format!("after {}", answer.code),
            Err(aterm_update_core::HttpError::Transport(_)) => "after err transport".into(),
            Err(e) => format!("after err {e:?}"),
        });
        std::fs::write(PathBuf::from(dir).join("verdicts"), out.join("\n")).unwrap();
    }

    /// PKG-4'S LINK PROBE, COUNTED IN PROCESSES. The first round runs in a child of this test
    /// binary ([`probe_a_first_round_over_counted_curl`]) whose every curl is a wrapper that
    /// logs its URL and answers as the scenario says.
    #[cfg(unix)]
    #[test]
    fn a_first_rounds_curls_are_one_per_head_offline_and_whole_online() {
        use std::os::unix::fs::PermissionsExt as _;
        let run = |scenario: &str| -> (std::collections::BTreeMap<u64, usize>, Vec<String>) {
            let dir = aterm_tempfile::tempdir().unwrap();
            let wrapper = dir.path().join("curl");
            std::fs::write(
                &wrapper,
                format!(
                    "#!/bin/sh\nfor a; do url=$a; done\necho \"$url\" >> '{log}'\n\
                     case {scenario}:$url in\n\
                     offline:*|online:*/atpkg-index-4/*) echo 'curl: (7) Failed to connect' >&2; exit 7 ;;\n\
                     esac\nprintf 'HTTP/2 404\\r\\n\\r\\n\\n404 0.042'\n",
                    log = dir.path().join("calls").display(),
                ),
            )
            .unwrap();
            std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
            let out = aterm_uds::spawnfd::command(std::env::current_exe().unwrap())
                .args(["--exact", "--nocapture", LINK_PROBE])
                .env(LINK_PROBE_ENV, dir.path())
                .env(aterm_update_core::CURL_SEAM, &wrapper)
                .env(aterm_types::domain::ENV_DEV_FAST_TIMERS, "1")
                .stdin(aterm_uds::spawnfd::Stdio::null())
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(
                out.status.success() && stdout.contains("1 passed"),
                "{scenario}: {}\n{stdout}\n{}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            );
            let mut curls = std::collections::BTreeMap::new();
            for url in std::fs::read_to_string(dir.path().join("calls"))
                .unwrap()
                .lines()
            {
                *curls.entry(walked_build(url)).or_insert(0) += 1;
            }
            let verdicts = std::fs::read_to_string(dir.path().join("verdicts")).unwrap();
            (curls, verdicts.lines().map(str::to_string).collect())
        };
        let (curls, verdicts) = run("offline");
        assert_eq!(
            curls,
            [(1, 1), (2, 1), (4, 1), (8, 1)].into(),
            "offline: one curl per HEAD, then the link is held down"
        );
        assert_eq!(
            verdicts,
            [
                "err link_down=true",
                "err link_down=true",
                "err link_down=true",
                "after err transport",
            ]
        );
        let (curls, verdicts) = run("online");
        assert_eq!(
            curls,
            [(1, 1), (2, 1), (4, 3), (8, 1)].into(),
            "online: the unreached HEAD keeps attempts two and three"
        );
        assert_eq!(
            verdicts,
            ["ok false", "ok false", "err link_down=true", "after 404"]
        );
    }

    /// The manifest prefetch asks both halves of every pinned pair, each under its own cap, and
    /// memoizes only whole pairs: the reads after it fetch nothing, a pair whose signature did
    /// not arrive is fetched again by the read that wants it, and a prefetched pair still has to
    /// verify (the memo holds bytes, not trust, so a tampered signature is refused as one fresh
    /// off the wire).
    #[test]
    fn the_manifest_prefetch_fills_the_memo_with_bytes_not_trust() {
        use crate::sig::testkit;
        let f = GithubFetcher::new("alabsystems".into());
        let pins: Vec<(String, String, u64)> = [("ay", 8256), ("ty", 3007), ("nn", 108)]
            .iter()
            .map(|(p, b)| ((*p).to_string(), (*p).to_string(), *b))
            .collect();
        let asked = std::sync::Mutex::new(Vec::<(String, u64)>::new());
        let wire = |url: &str, cap: u64| -> Result<Vec<u8>, String> {
            asked.lock().unwrap().push((url.to_string(), cap));
            match url.strip_suffix(".sig") {
                Some(_) if url.contains("/pkg-nn-") => Err(format!("HTTP 503 for {url}")),
                Some(toml) => {
                    let mut sig = testkit::sign(&testkit::MACHINE_SEED, toml.as_bytes());
                    if url.contains("/pkg-ty-") {
                        sig[0] ^= 1;
                    }
                    Ok(sig)
                }
                None => Ok(url.as_bytes().to_vec()),
            }
        };
        f.prefetch_with(&pins, &wire);
        let mut first: Vec<(String, u64)> = std::mem::take(&mut *asked.lock().unwrap());
        first.sort();
        let mut expected: Vec<(String, u64)> = pins
            .iter()
            .flat_map(|(repo, program, build)| {
                let (toml, sig) =
                    direct_manifest_urls(&format!("alabsystems/{repo}"), program, *build).unwrap();
                [(toml, MANIFEST_CAP), (sig, SIG_CAP)]
            })
            .collect();
        expected.sort();
        assert_eq!(
            first, expected,
            "both halves of every pair, each under its cap"
        );

        let index = testkit::trusted_index_at(
            testkit::SEQ,
            format!(
                "schema = 2\nindex_build = 47\nvalid_until = \"2099-01-01T00:00:00Z\"\n\
                 machine_id = \"{}\"\nroster_seq = {}\n[programs.ay]\nrepo = \"ay\"\n",
                testkit::MACHINE_ID,
                testkit::SEQ,
            )
            .into_bytes(),
        );
        let (raw, sig) = f.manifest_pair("ay", "ay", 8256, &wire).unwrap();
        assert!(
            index.verify_pkg(raw, &sig).is_ok(),
            "the genuine pair verifies"
        );
        let (raw, sig) = f.manifest_pair("ty", "ty", 3007, &wire).unwrap();
        assert!(
            index.verify_pkg(raw, &sig).is_err(),
            "a tampered signature the prefetch carried is refused"
        );
        assert!(asked.lock().unwrap().is_empty(), "both read from the memo");
        let err = f.manifest_pair("nn", "nn", 108, &wire).unwrap_err();
        assert!(err.contains("HTTP 503"), "{err}");
        assert_eq!(asked.lock().unwrap().len(), 2, "fetched again on the read");
        asked.lock().unwrap().clear();
        f.prefetch_with(&pins[..2], &wire);
        assert!(asked.lock().unwrap().is_empty());
    }

    /// THE PREFETCH IS ONE WAVE: a set with more pairs than the shared fan-out keeps in
    /// flight has every request out at once, so no program's manifest waits for another's to
    /// land — the eleventh program's pair was a second round before the announcement.
    #[test]
    fn the_manifest_prefetch_asks_every_pair_in_one_wave() {
        let f = GithubFetcher::new("alabsystems".into());
        let pins: Vec<(String, String, u64)> = (0..=FETCH_CONCURRENCY / 2)
            .map(|i| (format!("p{i}"), format!("p{i}"), 1))
            .collect();
        let want = 2 * pins.len();
        let arrived = (std::sync::Mutex::new(0usize), std::sync::Condvar::new());
        let wire = |url: &str, _cap: u64| -> Result<Vec<u8>, String> {
            let (count, all_in) = &arrived;
            let mut n = count.lock().unwrap();
            *n += 1;
            all_in.notify_all();
            let timed_out = all_in
                .wait_timeout_while(n, aterm_spec::wait::MUST_HAPPEN, |n| *n < want)
                .unwrap()
                .1
                .timed_out();
            if timed_out {
                Err(format!(
                    "{url}: answered before all {want} requests were out"
                ))
            } else {
                Ok(url.as_bytes().to_vec())
            }
        };
        f.prefetch_with(&pins, &wire);
        assert_eq!(
            f.manifests.lock().unwrap().len(),
            pins.len(),
            "every pair of {want} requests in flight at once"
        );
    }

    /// A host whose every answer is "published" cannot hold the walk: it stops at its HEAD
    /// budget with a named failure, and fetches nothing.
    #[test]
    fn the_walk_stops_at_its_head_budget() {
        let w = walk(44, &|_| published(), &[]);
        assert!(w.labels.is_err());
        assert_eq!(w.heads.len(), usize::try_from(WALK_HEAD_BUDGET).unwrap());
        assert!(w.fetched.is_empty(), "no asset was fetched");
    }

    /// The quad is the four derived download URLs of one tag; an unsafe tag derives nothing.
    #[test]
    fn the_index_quad_is_the_four_derived_download_urls() {
        let q = index_quad("alabsystems/aterm", "atpkg-index-41").unwrap();
        let base = "https://github.com/alabsystems/aterm/releases/download/atpkg-index-41/";
        assert_eq!(q.label, "atpkg-index-41");
        assert_eq!(q.index, format!("{base}index.toml"));
        assert_eq!(q.index_sig, format!("{base}index.toml.sig"));
        assert_eq!(q.roster, format!("{base}aterm-machines.toml"));
        assert_eq!(q.roster_sig, format!("{base}aterm-machines.toml.sig"));
        assert!(index_quad("a/b/c", "atpkg-index-41").is_none());
        assert!(index_quad("alabsystems/aterm", "atpkg-index-4/1").is_none());
        const { assert!(INDEX_CANDIDATE_CAP < 24) };
    }
}
