// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The production GitHub-Releases [`Fetcher`](crate::flow::Fetcher) (§5/§9) — the network
//! impl the install flow ([`crate::flow`]) runs against a real repo.
//!
//! ONE TRANSPORT: every byte it reads — the index and its roster, each
//! program's `pkg-*.toml`, every artifact — comes from the release DOWNLOAD host,
//! `https://github.com/<owner>/<repo>/releases/download/<tag>/<name>`, with no credential,
//! and it makes no `api.github.com` request at all. The download host is unmetered; the
//! API's anonymous budget (sixty requests an hour per IP, shared by every machine behind
//! one address and by the release cut) is not this client's to spend, and a credential
//! never reaches the network from here. Authenticity comes from signatures, never from the
//! transport (§8), so the host is only ever a source of bytes.
//!
//! Every URL is DERIVED rather than discovered:
//!
//! * which index is newest is found by HEADs of its tags ([`newest_index`]), under the
//!   publisher's guarantee that index builds are contiguous — `atpkg-publish index`
//!   refuses an `index_build` that is not its baseline's plus one, and no index release is
//!   ever deleted but to be re-cut under the same number;
//! * which build of a program is fetched is named by the signed index's pin, and the
//!   publishing tag convention (`atpkg-<program>-<build>`) names its manifest and artifact.
//!
//! Every fetch is anonymous, so a repointed account (`[packages].account`, a development
//! setting) or a `[packages.links]` fetch override must be publicly readable, as the app's own
//! channel must.
//!
//! The index lives on `<owner>/aterm` ([`crate::discovery::index_repo`]); each program's
//! `pkg-*.toml` + artifacts live on that program's own repo (`repo`, §4.2), which the flow
//! threads in. The bytes are handed to the verifier **raw** (no lossy conversion) —
//! verification happens before any parse (§8).

use std::path::{Path, PathBuf};

use crate::flow::VendorFetchError;
use crate::select::Candidate;

/// The unmetered download URL for asset `name` of release `tag` in `slug`
/// (`owner/repo`), or `None` if the slug is not exactly one `owner/repo` pair or any
/// segment is not path-safe.
///
/// ONE builder for both GitHub clients: this delegates to
/// [`aterm_update_core::cdn::release_download_url`], the same pure function the app
/// updater derives its asset URLs from, so the publisher's convention has exactly one
/// spelling in the tree. Deliberately strict — these URLs are synthesized, not read out
/// of an API response, and a slug or tag carrying a slash, a `..`, an empty half, or a
/// scheme would otherwise splice into a URL pointing somewhere else entirely. `None`
/// fetches nothing.
fn web_asset_url(slug: &str, tag: &str, name: &str) -> Option<String> {
    let (owner, repo) = slug.split_once('/')?;
    aterm_update_core::cdn::release_download_url(owner, repo, tag, name)
}

/// The ONE way this fetcher reads bytes from the release download host — and neither
/// it nor the transport has a credential parameter, so "no token is ever presented to
/// `github.com`" is a fact of the type, not of each call site's discipline. Refuses an
/// API URL outright.
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
/// or a tag is not path-safe, or an asset name carries no build number. Nothing is fetched.
fn underivable(slug: &str, what: &str) -> String {
    let mut msg = String::from("no release download URL derives for ");
    msg.push_str(what);
    msg.push_str(" on ");
    msg.push_str(slug);
    msg
}

/// The `(pkg-<program>-<build>.toml, .sig)` download URLs for a build already pinned by the
/// signed index — no discovery. `None` when the slug or program name is not URL-safe.
///
/// The publishing tag convention is `atpkg-<program>-<build>`, the release `atpkg-publish`
/// packs upload to.
fn direct_manifest_urls(slug: &str, program: &str, build: u64) -> Option<(String, String)> {
    if program.is_empty()
        || !program
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    let b = build.to_string();
    // `atpkg-<program>-<build>` / `pkg-<program>-<build>.toml`
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

/// The download URL for a release ASSET whose name carries the build it was published under
/// (`ty-2973.tar.zst` and `ty-2973-x86_64-unknown-linux-gnu.tar.zst` BOTH live under tag
/// `atpkg-ty-2973`). `None` when the name has no extension to strip, carries no build
/// number, or is not URL-safe.
///
/// The tag is the build-qualified PREFIX of the name, NOT the whole stem. Only the
/// historical `aarch64-apple-darwin` asset is named `<prog>-<build>.tar.zst`; every other
/// triple is `<prog>-<build>-<triple>.tar.zst` and lands on the SAME
/// `atpkg-<prog>-<build>` release (`atpkg-publish pack`, `pack-bundle` and the Linux
/// lane). Taking the whole stem derived
/// `atpkg-trust-4821-x86_64-unknown-linux-gnu`, a tag that has never existed. The build
/// number is the anchor: it is the first all-digit `-` segment, and neither a program name
/// nor a target triple has one.
fn direct_asset_url(slug: &str, asset: &str) -> Option<String> {
    // Strip the FULL extension: these are `.tar.zst`, and `Path::file_stem` would leave
    // `ty-2973.tar`, naming a tag that does not exist.
    let stem = asset.split_once('.')?.0;
    if stem.is_empty()
        || !stem
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    // `<prog>-<build>`: the stem truncated after its first all-digit segment — which
    // drops a `-<triple>` suffix when there is one and changes nothing when there is not.
    // The stem is ASCII by the check above, so the index is a char boundary. A name with
    // NO build number derives nothing: no such tag can exist.
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

/// Caps (bytes) for the two asset classes — a manifest is a few KB; an artifact is tens of
/// MB up to a multi-GB toolchain bundle. Bound what an attacker-controlled asset can write.
/// Public: the producer reads index and manifest files under the same cap, so it never
/// accepts one a client would refuse to fetch.
pub const MANIFEST_CAP: u64 = 5_000_000; // 5 MB
pub const SIG_CAP: u64 = 4_096; // an Ed25519 detached sig is 64 bytes; cap generously
const ARTIFACT_CAP: u64 = 8u64 << 30; // 8 GiB ceiling for a toolchain bundle

/// The byte cap for ONE artifact transfer: the row's SIGNED `size`, clamped by
/// [`ARTIFACT_CAP`] — never the constant alone.
///
/// The signed number is the one the disk preflight was computed from (`flow`'s
/// `disk_gate(size + disk_installed)`), and that preflight runs ONCE, before the
/// transfer: nothing bounds the bytes while they land but the cap the fetcher is given,
/// and the sha256 gate sees the file only after it is WHOLE. Capping the release lane at
/// the 8 GiB ceiling instead therefore let a mis-uploaded or substituted asset — up to
/// GitHub's 2 GiB per-asset limit — write straight through the free-space floor the
/// preflight had just defended, for a row whose signed `size` said 30 MB, and be
/// discarded only afterwards. The vendor lane (`download_url`) has always taken the
/// signed size exactly; this is the same rule for the release lane, and it is what §9
/// ("Large-artifact handling — caps from the SIGNED manifest, not the API") asks for.
///
/// `0` is the one row that can state no size — [`crate::vendor::check_row`] requires
/// `size > 0` only for the `https`/`pkg` protocols, so a `github-release` row published
/// before that rule may carry none — and it keeps the ceiling rather than capping the
/// transfer at zero bytes, which would refuse every such artifact.
fn artifact_cap(size: u64) -> u64 {
    if size == 0 || size > ARTIFACT_CAP {
        ARTIFACT_CAP
    } else {
        size
    }
}

/// A roster is a few hundred bytes per machine and is capped at 16 machines. Same ceiling
/// `aterm-update`'s armed path uses for the identical asset — one document, one bound.
/// Public, like [`MANIFEST_CAP`]: the producer's anonymous head read takes the same bound.
pub const ROSTER_CAP: u64 = 65_536;

/// How many index releases one resolve fetches the signed quad for, NEWEST FIRST: the
/// newest published index and the ones below it, never below the lowest build the walk
/// knows is published (the store's verified floor, which nothing lower can pass anyway).
///
/// Selection takes the highest signed `index_build` within the newest admitted roster
/// generation, so the newest release wins essentially always; the others are the genuine
/// fallbacks — a newest index that fails verification, or whose assets are still being
/// uploaded. In the steady state the newest IS the floor, and one quad is fetched. Still
/// below the §14 cache's own candidate cap (`cache::MAX_CACHE_CANDIDATES` = 24) with room
/// for one more, so a full candidate set, and one cached beside the cached index that
/// serves, is never refused by the cache write.
const INDEX_CANDIDATE_CAP: u64 = 4;

/// The most HEADs one index discovery spends before it gives up. The walk needs two in the
/// steady state, three when an index was published, about twice log₂ of the distance when a
/// store is far behind (seven for a store five builds back, about twenty for one a thousand
/// back), a few more for each missing number it steps over, and a store that has verified
/// nothing at most twenty-one more to find the channel's first build — so this bounds only a
/// host whose every answer says "published".
const WALK_HEAD_BUDGET: u32 = 48;

/// The highest power of two a store that has verified NO index HEADs while it looks for
/// the first build the channel publishes ([`newest_index`]).
const FIRST_INDEX_SEARCH_LIMIT: u64 = 1 << 20;

/// How many requests one discovery round, candidate fetch or manifest prefetch keeps in
/// flight. Each is still its own `curl` process over the unchanged single-URL calls (`-q`
/// first, `--` before the one URL, https only, capped, retried per URL); twenty of them at
/// once measured the same wall time as one batched `curl -Z` (0.25–0.48 s), where twenty in
/// turn took four seconds — a fresh process and two TLS handshakes each. Twenty is also the
/// ALab set's manifest prefetch (ten programs, a manifest and its signature each) in one wave.
const FETCH_CONCURRENCY: usize = 20;

/// The most HEADs one discovery asks AHEAD of its walk ([`ahead_of_walk`]), beyond the ones
/// the walk itself asks (which [`WALK_HEAD_BUDGET`] bounds): a host whose answers keep
/// defeating the guess costs at most this many more, and is then walked a HEAD at a time.
const WALK_AHEAD_BUDGET: usize = 2 * WALK_HEAD_BUDGET as usize;

/// `job` over every item, up to [`FETCH_CONCURRENCY`] at once, the answers in item order.
/// The calling thread works the queue too, so a thread the system refuses only narrows the
/// fan-out; a job that panics panics here, as it would have called in turn.
pub(crate) fn fan_out<T: Sync, R: Send>(items: &[T], job: &(dyn Fn(&T) -> R + Sync)) -> Vec<R> {
    if items.len() < 2 {
        return items.iter().map(job).collect();
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut answered: Vec<(usize, R)> = std::thread::scope(|scope| {
        // Copy: it holds only references, so every helper takes its own.
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
        let helpers: Vec<_> = (1..items.len().min(FETCH_CONCURRENCY))
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

/// THE NEWEST PUBLISHED INDEX, by HEADs of its tags alone: `probe(n)` answers whether
/// `atpkg-index-<n>` is published (`Err` when the answer says neither). Returns the newest
/// build, and the lowest build known published — the candidates' lower bound.
///
/// It rests on the PUBLISHER'S GUARANTEE that index builds are contiguous: every number from
/// the channel's first index to its newest is published, because `atpkg-publish index`
/// refuses an `index_build` that is not its baseline's plus one, and no index release is
/// deleted but to be re-cut under the same number. The published builds are therefore one
/// run, and a miss above a published build bounds it:
///
/// * the run's lower end is the store's verified `floor` — published when this store
///   verified it, unless a machine signed it above the run, which
///   [`GithubFetcher::walk_candidates`] then walks again from none — or, on a store that
///   has verified nothing, the first power of two that answers (the public channel's first
///   index is atpkg-index-6, so 8 does);
/// * from there the walk GALLOPS — HEAD `low+1`, `low+2`, `low+4`, … — to the first miss,
///   then bisects between it and the last hit, landing a published `hit` whose `hit+1` is
///   not;
/// * and it HEADs `hit+2` before it believes that: published, the walk gallops on from
///   there. So ONE missing number anywhere — a first publish numbered by hand, a release
///   deleted and never re-cut — is stepped over (the probe HEADs only `floor+1`, so a gap
///   there would otherwise hold the store at its floor for good). Two consecutive missing
///   numbers can still end the run — always when they sit directly above the floor — and the
///   builds above them are then reached only once the missing ones are published.
///
/// Two HEADs in the steady state (`floor+1` and `floor+2` absent), three when one index was
/// published. The contiguity guarantee is what lets the walk prove it found the newest
/// without listing releases on the metered API. Public: a keyless producer finds the
/// channel head the same way (`atpkg-publish`'s `quad::head_build`).
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
    // `hit` is always published.
    let mut hit = low;
    loop {
        // Gallop from `base` to a miss; then everything between `hit` and `miss` is one of
        // the two, and bisection lands a published `hit` with `hit+1` absent.
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
        // One missing number is a gap, not the end: the run goes on past it.
        let past = hit.checked_add(2).ok_or_else(overflow)?;
        if !probe(past)? {
            return Ok((hit, low));
        }
        hit = past;
    }
}

/// The environment variable a release build compiles [`compiled_index_hint`] from: the
/// shipped channel's published run as the release cut found it ([`published_index_run`]),
/// spelled as [`IndexHint`] displays it. `ship release` sets it (aterm-release's build
/// plan); a development build has none.
pub const INDEX_HINT_ENV: &str = "ATPKG_INDEX_HINT";

/// What the discovery rounds may GUESS the channel publishes ([`ahead_of_walk`]): the newest
/// build something saw published, and where the run the walk from no floor finds starts, when
/// that is known. Never an answer: the walk asks every build it reads, and a hint the channel
/// has moved past, or one that names no build it publishes, costs HEADs and a round, never a
/// different index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexHint {
    /// The first build [`newest_index`] from no floor found published — the run's first
    /// power of two (8 on the public channel, whose first index is 6), when known.
    pub low: Option<u64>,
    /// The newest build seen published.
    pub newest: u64,
}

impl IndexHint {
    /// `<low>..<newest>`, or a bare `<newest>` ([`std::fmt::Display`]'s spellings). `None`
    /// for anything else, a zero, or a `low` above `newest`.
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

/// The published run this binary's release cut compiled in ([`INDEX_HINT_ENV`]); `None` in
/// a development build, or for a value [`IndexHint::parse`] refuses.
#[must_use]
pub fn compiled_index_hint() -> Option<IndexHint> {
    option_env!("ATPKG_INDEX_HINT").and_then(IndexHint::parse)
}

/// The shipped channel's published run, found as a store that has verified nothing finds it:
/// [`newest_index`] from no floor over HEADs of the download host, each read as discovery
/// reads it — what a release cut compiles in ([`INDEX_HINT_ENV`]). A HEAD at a time: a cut
/// asks once.
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

/// The four DERIVED download URLs of one index release: the index, its machine signature,
/// and the master-signed roster and its signature beside them.
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
/// below `low`, newest first. A release whose four assets do not all arrive is left out,
/// the first such failure (newest first, each quad's assets in order) kept in `first_error`.
///
/// Every asset of every quad is asked AT ONCE ([`fan_out`]), each under its own cap: the
/// older quads are not a fallback to fetch later but selection's input — the newest
/// admitted roster generation across ALL of them decides, which is how a fresh store sees
/// the owner's revoking generation in an older release beside a newer tag a revoked
/// machine signed.
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
/// attempt before any of them backs off ([`GithubFetcher::walk_candidates`]). [`Wire`] in
/// production; a test's closure is a HEAD that answers whole. `Sync`: a round asks several
/// at once.
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
/// tests of [`GithubFetcher::walk_candidates`]. `Sync`: the quads are fetched at once.
type WebFetch<'a> = dyn Fn(&str, u64) -> Result<Vec<u8>, String> + Sync + 'a;

/// What one HEAD of an index tag said, as discovery reads it: whether the release is
/// published — or why the answer says neither: the message the discovery fails with, and
/// whether no host answered at all (the link down).
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
/// When EVERY first attempt reached no host, there is no link to retry: the process holds it
/// down ([`Heads::link_down`]) and the round's verdicts are those attempts' failures — the
/// offline first launch's answer in one attempt's time, where three attempts and their
/// backoff took three seconds. A round any host answered, or that failed any other way (a
/// timeout after the handshake, a TLS refusal, a 5xx), keeps every HEAD's honest retries,
/// the first attempt counted among them.
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
    // The rest of every HEAD that may still change, at once; the ones that settled are read
    // where they stand.
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
/// `floor` ([`newest_index`], replayed over the answers already `known`) asks after it,
/// under every answer it may get, until `room` are named — none already known.
///
/// Correctness never rests on this: the walk itself runs unchanged over the answers, asks
/// for each build in its own order, and a build named here that it never asks is a HEAD
/// spent for nothing. What it buys is rounds. Each replay follows a GUESS where the answers
/// so far support one — nothing published seen yet guesses "absent" (the steady state's two
/// HEADs, and a fresh store's power-of-two search, each in one round); at or below the
/// highest published build seen, "published"; at or past the first miss above it, "absent"
/// — and FORKS where they do not, both answers explored breadth first. So a fresh store's
/// seventeen HEADs in turn are three rounds.
///
/// `hint` ([`GithubFetcher::with_index_hint`]) names a build something saw published — the
/// highest this store's §14 cache holds, the one the release cut compiled in, the one the
/// window's probe woke this pass for — so once a published build at or below it is seen,
/// every build between the two is guessed published (the run is contiguous), and past it,
/// with no answer there yet, "absent" — the steady state's guess, where the walk otherwise
/// forks. And with nothing seen yet it guesses the whole first round where it knows where
/// the run starts: the store's floor, or the run's first power of two the cut compiled in
/// ([`IndexHint::low`]). From there to the hint is published, past it absent, so a fresh
/// store meets a channel that has not moved since its release cut, and a floor one index
/// behind, in one round.
///
/// A hint is usually some builds behind the channel by the time it is asked — a release
/// cut's, days later at a first install — so the room such a guessed round leaves goes to
/// the HEADs the walk would ask next were the channel 1, 2, 4, … builds past the hint, its
/// own gallop, while room lasts. Their answers bracket where the channel went, and the round
/// after them finishes the walk: a fresh store whose cut saw atpkg-index-71 meets any channel
/// from 72 to 103 in two rounds, where the guess alone took three. A hint the channel has
/// since moved past costs HEADs, never a different answer.
fn ahead_of_walk(
    floor: u64,
    need: u64,
    known: &std::collections::BTreeMap<u64, HeadVerdict>,
    room: usize,
    hint: Option<IndexHint>,
) -> Vec<u64> {
    // A hint at or below the floor names nothing the floor does not — the window's probe
    // build a pass verified since it woke this one — and guessing from it would fill the
    // steady state's round of two HEADs to `room`.
    let hint = hint.filter(|h| floor == 0 || h.newest > floor);
    let top = known
        .iter()
        .filter(|(_, v)| matches!(v, Ok(true)))
        .map(|(b, _)| *b)
        .max();
    // Where the published run starts, when known: the walk asks only above a floor, and from
    // none it searches the powers of two for the first published one.
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
    // Each replay ends at its first unguessable build, so the tree is walked a node per
    // replay; the cap only bounds a host whose every answer forks.
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
    // The guesses above followed the hint: the room they left goes to the channel having
    // moved 1, 2, 4, … builds past it, from where the run starts (`low`, or wherever the
    // answers already say once something published is seen).
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

/// The `(slug, program, build)` triple that fully determines which asset pair a
/// memoized manifest was downloaded from.
type ManifestKey = (String, String, u64);

/// The RAW `(manifest, signature)` bytes of one asset pair — unverified wire
/// bytes, shared by `Arc` so a memo hit costs no copy.
type ManifestBytes = std::sync::Arc<(Vec<u8>, Vec<u8>)>;

/// The manifest memo itself: the locked map behind the fetcher's `manifests`
/// field.
type ManifestMemo = std::sync::Mutex<std::collections::BTreeMap<ManifestKey, ManifestBytes>>;

/// The production fetcher: an `owner` account and the optional per-program
/// `[packages.links]` `owner/repo` FETCH overrides. Construct with [`GithubFetcher::new`]
/// (+ [`GithubFetcher::with_overrides`], [`GithubFetcher::with_index_floor`]).
pub struct GithubFetcher {
    owner: String,
    /// program → slug-validated `"owner/repo"`: where THAT program's `pkg-*.toml` +
    /// artifacts are fetched from instead of `<owner>/<repo>` — a development setting, and a
    /// public repo like every other destination. NEVER an authenticity input: the index
    /// fetch is untouched, reachability (§5) still requires the index to name the program,
    /// and every byte still passes the identical signature/sha256/`tree_root` gates — an
    /// override can only redirect WHERE bytes come from, not what verifies.
    overrides: std::collections::BTreeMap<String, String>,
    /// Per-invocation memo of the index candidate set: `install_inner` re-resolves it once
    /// per recursive dependency and `install_default_set` once per ungrouped member, and a
    /// miss is a discovery walk plus four asset downloads per candidate.
    index: std::sync::Mutex<Option<std::sync::Arc<Vec<Candidate>>>>,
    /// The highest index build this store has VERIFIED (its durable floor,
    /// [`crate::index_probe::verified_floor`]) — where the discovery walk counts from. `0`
    /// (the default) is none: the walk then looks for the first build the channel publishes.
    index_floor: u64,
    /// A build something saw published, and where the run starts when that is known
    /// ([`Self::with_index_hint`]): a guess the discovery rounds follow, never an answer.
    index_hint: Option<IndexHint>,
    /// Whether the last discovery's HEAD failed on the LINK (curl reached no host) — what
    /// [`crate::flow::Fetcher::index_link_down`] reads.
    link_down: std::sync::atomic::AtomicBool,
    /// What the last discovery asked: its HEADs, the rounds they went out in, and its asset
    /// GETs — the package log's `index-resolved` line.
    walk_cost: [std::sync::atomic::AtomicU32; 3],
    /// Per-invocation memo of the RAW `(pkg-<program>-<build>.toml, .sig)` bytes, keyed
    /// by `(slug, program, build)` — the triple that fully determines which asset pair is
    /// downloaded. Each miss is two more `download_bytes` round-trips, and the same
    /// member's manifest is fetched 2–3× per apply (preflight, prescan, stage).
    ///
    /// The memo holds BYTES, NOT TRUST (same property as the on-disk [`crate::IndexCache`]):
    /// entries are the unverified wire bytes, and every consumer still runs
    /// `verify_pkg(raw, &sig, …)` → `parse_pkg` and re-binds `program`/`build_number`
    /// itself, so a served entry is gated exactly as a freshly-downloaded one.
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

    /// Attach the per-program `owner/repo` fetch overrides (from
    /// `[packages.links]`, already slug-validated by
    /// [`crate::config::repo_overrides`]).
    #[must_use]
    pub fn with_overrides(mut self, overrides: std::collections::BTreeMap<String, String>) -> Self {
        self.overrides = overrides;
        self
    }

    /// Attach this store's verified index floor, which the discovery walk counts from.
    #[must_use]
    pub fn with_index_floor(mut self, floor: u64) -> Self {
        self.index_floor = floor;
        self
    }

    /// Attach what this store may guess the channel publishes ([`IndexHint`]): the discovery
    /// rounds guess from it ([`ahead_of_walk`]); the walk's answer never rests on it.
    #[must_use]
    pub fn with_index_hint(mut self, hint: Option<IndexHint>) -> Self {
        self.index_hint = hint;
        self
    }

    /// `<owner>/<repo>` under this fetcher's account.
    fn slug_of(&self, repo: &str) -> String {
        format!("{}/{repo}", self.owner)
    }

    /// The `<owner>/<repo>` slug `program`'s release fetches go to: the config
    /// fetch override when one is declared, else the index-declared `repo` under
    /// this fetcher's account.
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
        // The program's manifest rides its release repo — the `[packages.links]` fetch
        // override redirects it when declared. The slug is resolved ONCE: it keys the memo
        // and derives the URLs.
        let slug = self.slug_for(program, repo);
        // `(slug, program, build)` fully determines which asset pair is downloaded, so a
        // hit is the same bytes the network would return. Bytes, not trust: the caller
        // still runs verify_pkg → parse_pkg and re-binds program/build itself.
        let key = (slug.clone(), program.to_string(), build);
        if let Ok(memo) = self.manifests.lock()
            && let Some(hit) = memo.get(&key)
        {
            return Ok((**hit).clone());
        }
        // The release TAG is `atpkg-<program>-<build>` by publishing convention, and `build`
        // arrived here from the SIGNED index's channel `pin` table — so the exact URL is
        // already determined, and the answer to "which build" comes only from bytes the
        // master-rooted chain covers. The bytes are still verified
        // (`verify_pkg` → `parse_pkg`, which re-binds program and build), so the host remains
        // a transport, never an authenticity input (§8).
        let (toml_url, sig_url) = direct_manifest_urls(&slug, program, build)
            .ok_or_else(|| underivable(&slug, program))?;
        let pair =
            std::sync::Arc::new((fetch(&toml_url, MANIFEST_CAP)?, fetch(&sig_url, SIG_CAP)?));
        // Successes only — a failed download stays retryable.
        if let Ok(mut memo) = self.manifests.lock() {
            memo.insert(key, std::sync::Arc::clone(&pair));
        }
        Ok((*pair).clone())
    }

    /// [`crate::flow::Fetcher::prefetch_manifests`] over an injected byte fetch: every pair
    /// of `pins` the memo does not hold yet, both halves of all of them asked at once
    /// ([`fan_out`]), each under its own cap. A pair that arrives WHOLE is memoized, exactly
    /// as [`Self::manifest_pair`] memoizes it; one that does not is left out, so the read
    /// that wants it fetches it again and meets the failure itself.
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
            // An underivable name is left to the read, which names it.
            if let Some((toml, sig)) = direct_manifest_urls(&key.0, program, *build) {
                wanted.push((key, toml, sig));
            }
        }
        let urls: Vec<(&str, u64)> = wanted
            .iter()
            .flat_map(|(_, toml, sig)| [(toml.as_str(), MANIFEST_CAP), (sig.as_str(), SIG_CAP)])
            .collect();
        let mut fetched = fan_out(&urls, &|&(url, cap)| fetch(url, cap)).into_iter();
        for (key, ..) in wanted {
            if let (Some(Ok(toml)), Some(Ok(sig))) = (fetched.next(), fetched.next())
                && let Ok(mut memo) = self.manifests.lock()
            {
                memo.insert(key, std::sync::Arc::new((toml, sig)));
            }
        }
    }

    /// THE INDEX CANDIDATES, from the download host alone, over an injected HEAD and byte
    /// fetch — the seam [`crate::flow::Fetcher::index_candidates`] runs, measurable without a
    /// network.
    ///
    /// [`newest_index`] finds the newest published build from this store's verified floor,
    /// each HEAD read as the next-index probe reads it ([`crate::index_probe::classify`]:
    /// only GitHub's own 302/307 to its release-asset storage is a publication, a 404 its
    /// absence, and anything else — a 200 from a captive portal, a 429, a 5xx — says
    /// nothing, and fails the discovery if the walk asks for it). The walk is unchanged and
    /// sequential; its HEADs are asked in ROUNDS, each the build it needs and the ones it is
    /// likely to need next, at once ([`ahead_of_walk`]). Then the quads of the newest build
    /// and the ones below it, down to [`INDEX_CANDIDATE_CAP`] and never below the lowest
    /// build known published, are fetched at once and kept newest first. A release whose
    /// four assets do not all arrive is LEFT OUT — an index can be up before its signature
    /// or roster while the publisher is still uploading — so that release is simply not a
    /// candidate, as the listing that came before skipped an incomplete release; only when
    /// none arrives does the resolve fail. A floor that is no published tag at all — one a
    /// machine signed above the run — is walked again from first contact. A HEAD that
    /// reached no host marks the link down ([`Self::link_down`]).
    fn walk_candidates(
        &self,
        slug: &str,
        head: &HeadFn<'_>,
        fetch: &WebFetch<'_>,
    ) -> Result<Vec<Candidate>, String> {
        // The link verdict is this walk's own: a failure an earlier walk in this process met
        // says nothing about this one. So is its cost.
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
        // Every HEAD answered so far. The walk reads its answers from here, in its own order;
        // a build it asks that is not here yet is asked in a round with the ones it is likely
        // to ask next ([`ahead_of_walk`]), all at once. A verdict that says nothing is kept
        // too, and fails the walk only if the walk asks for that build.
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
                // The walk's first round is its link probe ([`probe_round`]).
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
                // Unreachable: a round always asks the build it was started for.
                None => Err(underivable(slug, &index_tag(build))),
            }
        };
        let floor = self.index_floor;
        let run = newest_index(floor, &mut |build| probe_from(floor, build))?;
        let mut first_error: Option<String> = None;
        let mut out = fetch_run(slug, run, fetch, &mut first_error)?;
        // THE FLOOR IS A SIGNED NUMBER, NOT A TAG. A rostered machine chose it, so one that
        // signed a build far above the run lifted every store that took it out of the run,
        // and no tag the owner publishes was asked for again: not even the revoking
        // republish of the newer generation that re-bases the floor (`sig::BuildFloor`).
        // Nothing above the floor, its release not arriving and its own tag absent, the walk
        // finds the run as a store that has verified nothing does; selection still holds
        // every candidate to the floor, and a run it refuses whole leaves the resolve on its
        // cache (`flow::resolve_verified_index`). A run that ends directly below the floor
        // is its tag deleted to be re-cut: no second walk, and the failure stands.
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
        // Memoized per invocation: the flow resolves the candidates once per program AND
        // once per transitive dependency. The bytes are returned by value (a few KB of
        // TOML + 64-byte sigs — free next to the network), so the trait signature and
        // every downstream gate are untouched: the same raw bytes still flow through
        // `select_index` → `admit_roster` → `authorize_index` → `parse_index` → floor →
        // freshness.
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
        // Successes only — a failed discovery or fetch stays retryable.
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
        // The UNSIZED lane: this signature holds no row, so the only bound available is
        // the ceiling. The flow's artifact path does not come through here — it calls
        // `download_for` with the row's signed `size` (see `artifact_cap`).
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
        // The artifact rides the SAME release repo as the program's manifest, so the
        // `[packages.links]` fetch override redirects it identically, and the asset name
        // carries the build, so its tag follows from the name alone (`ty-2973.tar.zst` AND
        // its triple-suffixed sibling `ty-2973-x86_64-unknown-linux-gnu.tar.zst` →
        // `atpkg-ty-2973`). THE CAP IS THE ROW'S SIGNED `size` (clamped; see
        // `artifact_cap`). RESUMABLE: this is the request that moves hundreds of megabytes,
        // and the sha256 gate over the complete file is still what makes the bytes
        // acceptable — see `download_to_resumable`.
        let slug = self.slug_for(program, repo);
        let url = direct_asset_url(&slug, asset).ok_or_else(|| underivable(&slug, asset))?;
        web_fetch_to(&url, dest, artifact_cap(cap))
    }

    fn download_url(&self, url: &str, dest: &Path, cap: u64) -> Result<(), String> {
        // THE VENDOR LANE. The URL is the roster-signed row's own (already admitted by
        // `vendor::check_row`: https, allow-listed host), so no slug and no override is
        // consulted — nothing about this fetcher's account reaches the request, and no
        // credential either. `cap` is the signed `size`, exactly. Resumable like every big
        // transfer, and the https pin covers the first hop as well as every redirect.
        aterm_update_core::download_to_resumable_https_only(url, dest, cap)
    }

    fn vendor_get(
        &self,
        program: &str,
        url: &str,
        cap: u64,
        if_none_match: Option<&str>,
    ) -> Result<crate::flow::VendorGet, VendorFetchError> {
        // Only the program's own compiled prefixes, and never a credential (the transport
        // takes none).
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

/// The §14 cache key of the GitHub source under `owner` — [`GithubFetcher`]'s
/// `source_id`, and the key `doctor` reads the cache back under
/// ([`crate::index_probe::held_index_builds`]), so the writer and that reader cannot
/// disagree on which source a cache entry belongs to.
pub(crate) fn github_source_id(owner: &str) -> String {
    format!("github:{owner}/{}", crate::discovery::index_repo())
}

/// A vendor head read as a hint — the window's head watch, and every pass's and door's
/// head read ([`crate::flow::Fetcher::vendor_head`]): [`GithubFetcher`]'s `vendor_get`
/// pins, cap and anonymity, in one short attempt ([`aterm_update_core::vendor_get_hint`]).
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

/// The offline / publisher-test fetcher (§14): a flat directory of the assets
/// `tools/atpkg-pack*.sh` emit (`index.toml`(`.sig`), `pkg-<program>-<build>.toml`(`.sig`),
/// and the artifact tarballs). Pure `std::fs`, no network. The bytes it serves are handed to
/// the flow RAW, so a `dir:` registry gets the IDENTICAL verify-before-parse + floor +
/// freshness gate as `github:` (a dir cache holds bytes, not trust).
pub struct DirFetcher {
    dir: PathBuf,
}

impl DirFetcher {
    /// A fetcher reading assets from `dir` (canonicalized when possible for a stable
    /// `source_id`).
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        Self { dir }
    }
}

/// Reject a file-name component that could escape `dir` on a raw READ. The manifest is
/// signed, but sanitize anyway (defense-in-depth).
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
        // A `dir:` registry must publish the SAME complete authorization unit a release
        // does — index, its machine signature, the roster, the master's signature over the
        // roster. An offline directory is not a weaker tier: it supplies bytes, never
        // trust, so a missing roster here is exactly as fatal as a missing roster there.
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
            // Missing any of the four ⇒ no candidates ⇒ select_index None ⇒ NoIndex.
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
        // UNLINK FIRST — this line prevents data destruction in the registry, and it is
        // not optional.
        //
        // Without it: `hard_link` fails EEXIST when a previous run left a staging
        // link behind (nothing sweeps `staging/`, and an interrupted run is exactly
        // what a retry meets), so the fallback runs `fs::copy(src, dest)` where —
        // because they are the SAME hardlinked inode — dest IS src. Rust's macOS copy
        // opens the destination `O_TRUNC`, truncating the shared inode, then copies the
        // now-empty source and returns `Ok(0)`. Measured on macOS 26.5: `src=0 dst=0`,
        // success reported, and the registry asset zeroed for good (its sha256 can
        // never match again). It was found when the registry was a seed sealed inside
        // the signed app bundle, where the zeroed file also broke the code signature.
        let _ = std::fs::remove_file(dest);
        // Hardlink to avoid duplicating a multi-GB toolchain; a later remove_file(dl) drops
        // only the link, never the registry file. Cross-filesystem hardlink fails ⇒ copy.
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

/// Tier-1 of `AtpkgIndexPublishWalk`, the reader half: the real tag walk against the
/// derived model of the index channel (the producer's suite drives the writer half).
#[cfg(test)]
mod index_publish_conformance;

#[cfg(test)]
mod tests {
    use super::*;

    /// The zero-API URLs are SYNTHESIZED, not read out of an API response, so the exact
    /// strings are the contract with the publisher's tag convention
    /// (`atpkg-<program>-<build>`). A drift here sends every fetch to a 404, and there is
    /// no other lane behind it.
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
        // A hyphenated program name must not confuse the tag: the whole stem is the tag.
        assert_eq!(
            super::direct_asset_url("alabsystems/trust-mc", "trust-mc-20011.tar.zst").unwrap(),
            "https://github.com/alabsystems/trust-mc/releases/download/atpkg-trust-mc-20011/trust-mc-20011.tar.zst"
        );
        // `.tar.zst` is a DOUBLE extension: stripping only the last one would name the
        // non-existent tag `atpkg-ty-2973.tar`.
        assert!(
            !super::direct_asset_url("alabsystems/ty", "ty-2973.tar.zst")
                .unwrap()
                .contains(".tar/")
        );
    }

    /// EVERY triple but the historical `aarch64-apple-darwin` one is published as
    /// `<prog>-<build>-<triple>.tar.zst` on the SHARED `atpkg-<prog>-<build>` release
    /// (the `<prog>-<build>-<triple>.tar.zst` asset `atpkg-publish`'s packers and Linux
    /// lane upload to `atpkg-<prog>-<build>`). A tag taken from
    /// the WHOLE stem named `atpkg-trust-4821-x86_64-unknown-linux-gnu`, which has never
    /// existed, so on Linux and x86_64 macOS every artifact fetch 404'd (and, while a
    /// listing still stood behind it, spent an anonymous metered request per program). The
    /// build number is the anchor, not the stem.
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
            // The historical darwin spelling keeps deriving exactly what it did.
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
        // No build number in the name ⇒ no tag follows from it, and none of the
        // publisher's assets look like this: derive nothing rather than spend a request
        // on a URL that can only 404.
        assert!(super::direct_asset_url("alabsystems/ty", "ty-latest.tar.zst").is_none());
    }

    /// Anything that could splice a synthesized URL onto another host or path must decline
    /// — never emit a URL built from it.
    #[test]
    fn direct_urls_refuse_unsafe_slugs_and_names() {
        for bad in [
            "alabsystems",                     // no repo half
            "alabsystems/ty/extra",            // an extra path segment
            "alabsystems/",                    // empty repo
            "/ty",                             // empty owner
            "alabsystems/..",                  // parent traversal
            "evil.com/x/../../alabsystems/ty", // traversal via a long slug
            "https://evil.com/a",              // a scheme smuggled in as a slug
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
        // The shared builder's own refusals reach here too: a server-supplied tag with
        // a query, a fragment, an escape or whitespace derives nothing.
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

    /// The artifact cap is the ROW'S SIGNED SIZE, clamped by the ceiling — the same
    /// number `disk_gate` bounded the free-space check with. A constant 8 GiB is not a
    /// bound on a 30 MB row: a mis-uploaded or substituted asset fits inside it whole,
    /// lands in `staging/` through the free-space floor, and is refused only afterwards
    /// by the sha256 gate, which cannot run until the file is complete.
    #[test]
    fn artifact_cap_is_the_signed_size_clamped_by_the_ceiling() {
        assert_eq!(super::artifact_cap(30_000_000), 30_000_000);
        // A row that states no size keeps the ceiling — never a zero-byte cap, which
        // would refuse the transfer outright (`check_row` requires `size > 0` only for
        // the https/pkg protocols, so an older github-release row may carry none).
        assert_eq!(super::artifact_cap(0), super::ARTIFACT_CAP);
        // …and a size above the ceiling is clamped DOWN to it, never up: a signed row
        // can lower this lane's bound, never raise it.
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

    /// No credential on the web host, pinned STRUCTURALLY: the only functions this
    /// fetcher reads `github.com` through (`web_fetch_bytes`, `web_fetch_to`) have no
    /// token parameter — nor does the transport underneath — and refuse an API URL
    /// before any request. The refusal needs no network, so it is asserted here.
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

    // The `[packages.links]` owner/repo FETCH override redirects a program's
    // release-repo slug; the index repo and every non-overridden program are
    // untouched. Pure routing — the trust gates downstream are unchanged.
    #[test]
    fn override_redirects_only_the_named_programs_slug() {
        let mut overrides = std::collections::BTreeMap::new();
        overrides.insert("orc".to_string(), "alabsystems/orc-private".to_string());
        let f = GithubFetcher::new("alabsystems".into()).with_overrides(overrides);
        // The overridden program fetches from the declared owner/repo…
        assert_eq!(f.slug_for("orc", "orc"), "alabsystems/orc-private");
        // …even if the index declares a differently-named repo for it…
        assert_eq!(f.slug_for("orc", "some-repo"), "alabsystems/orc-private");
        // …while every other program stays under the fetcher's account + index repo.
        assert_eq!(f.slug_for("ay", "ay"), "alabsystems/ay");
        // No overrides at all: the plain account/repo slug.
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
        // Same non-vacuity as above: the roster is present, so an empty candidate set
        // can only be the FIFO/symlink index being refused.
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

    /// THE INODE-TRUNCATION REGRESSION. `DirFetcher` hardlinks a registry file into
    /// staging. Nothing sweeps staging, so a killed run leaves the link behind; the next
    /// attempt then found `hard_link` EEXIST and fell back to `fs::copy(src, dest)` where
    /// dest IS src — which on macOS opens the shared inode `O_TRUNC` and reports `Ok(0)`,
    /// zeroing the registry file. That kills the asset forever (sha256 can never match).
    ///
    /// The fix is one `remove_file(dest)`; this proves the registry survives a retry.
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
        // First fetch: hardlinks into staging (the multi-minute window a kill lands in).
        f.download("r", "ay-18.tar.zst", &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), payload);
        // The process is killed here — the link survives, nothing sweeps it.
        // Second fetch (the retry) must NOT destroy the source.
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

    /// The production fetcher refuses a vendor-direct fetch outside the program's own
    /// compiled prefixes before any spawn — the allow-listed hosts alone are not enough,
    /// and neither is the other vendor's prefix — as a verdict, never as unreachable; and
    /// it carries the transport's answer over field for field.
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
            // the other vendor's pinned URLs, and an unpinned program
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
    /// files a bad document as a network blip. The message survives either way.
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

    /// A name no download URL derives from is REFUSED before any request — there is no
    /// listing behind the derived URL, so the manifest, the unsized
    /// asset and the sized artifact lanes each answer an underivable name with an error and
    /// write nothing. The control: the same fetcher derives the URL for a published name.
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
        /// The builds the WALK asked about, in its order: the sequential reference's HEADs,
        /// whose answers the round walk reads in the same order ([`walk`] checks the two agree).
        heads: Vec<u64>,
        /// Every build the rounds HEADed — the walk's own and the ones asked ahead — sorted.
        headed: Vec<u64>,
        /// Every URL the rounds asked — the HEADs, then the asset downloads.
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

    /// THE SEQUENTIAL REFERENCE: discovery as it was before its HEADs went out in rounds and
    /// its quads at once — every HEAD asked when the walk asks it, every asset in turn, a
    /// quad given up at its first missing asset. [`walk`] runs it beside the real one.
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
    /// `answer` and every asset download succeeding unless its URL ends with one of
    /// `missing` — run as the fetcher runs it, in rounds and at once, and as the
    /// [`sequential_walk`] reference: the two must land the same candidates (or the same
    /// failure) and the same link verdict, the rounds must have asked every build the walk
    /// asked, and never more than the walk's budget and the ahead budget together.
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

    /// Every asset URL of the quads of `builds`, sorted — what [`Walked::fetched`] holds
    /// when each of those quads was asked whole.
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

    /// THE REQUIRED NET TEST. From a verified floor of 44, `atpkg-index-45`
    /// answering GitHub's own 302 and 46 onward a 404 resolves 45 — with 44 behind it — in
    /// exactly three HEADs and eight asset downloads, every one on the release download host:
    /// no api.github.com request is asked. The negative control is the predicate itself: it
    /// recognises the Releases listing the retired fallback fetched, so its silence over
    /// everything asked below is a finding, not a predicate that matches nothing.
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

    /// The walk from every kind of floor. The steady state (the floor is the newest) is TWO
    /// HEADs — the next number and the one past it — and one quad; a store five builds behind
    /// gallops and bisects to the newest and fetches the four below it; and a store that has
    /// verified NOTHING finds the channel from its first power of two (the public channel starts at
    /// atpkg-index-6, so 8) — none of it with a listing, which is what each of these cases cost
    /// before.
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

        // Nothing published at all: a failure, after the bounded search.
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

    /// The search itself, exhaustively over small channels: for every floor and newest build
    /// of a CONTIGUOUS channel the walk lands the newest, within its HEAD bound, and the lower
    /// bound it reports is published.
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

    /// A MISSING NUMBER IS STEPPED OVER. The publisher refuses a gap, but a first publish
    /// numbered by hand or a release deleted and never re-cut can still leave one, and a gap
    /// directly above a store's verified floor must not hold it there. Exhaustively over small
    /// channels with every pattern of single missing numbers, from every floor, the walk lands the
    /// newest and the lower bound it reports is published. The control is the documented limit, so
    /// the step is not a walk that ignores misses: two consecutive missing numbers directly above
    /// the floor end the run at the floor.
    #[test]
    fn the_search_steps_over_a_missing_number_and_stops_at_two() {
        for first in 1..=3u64 {
            for newest in first..=first + 16 {
                let interior = (newest - first).saturating_sub(1);
                for gaps in 0..(1u64 << interior) {
                    if gaps & (gaps >> 1) != 0 {
                        continue; // two adjacent missing numbers: the control below
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
        // The reported case, through the fetcher: 45 was never published above a floor of 44.
        // The walk HEADs 45 (absent), steps to 46, gallops to 50, and the missing number's
        // own release is simply not a candidate.
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
        // The control: two missing numbers directly above the floor end the run — 45 and 46
        // absent above 44 lands 44, after exactly the two HEADs that found them absent.
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

    /// An answer that says nothing FAILS the discovery — it is never read as "not published"
    /// (which would land an older index as if it were the newest) nor turned into a request
    /// on another host. A captive portal's 200, a renamed repo's 301, a redirect to a login
    /// page, a rate limit and a 5xx are a host that ANSWERED (the link is up, the pass a
    /// failure to surface); curl reaching no host is the link down (an offline pass). No
    /// asset is fetched either way.
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
        // The verdict is the last walk's: the same fetcher meeting a host that answers next
        // is not the link down.
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

    /// A release whose four assets do not all arrive is LEFT OUT, never the resolve's
    /// failure: an index can be up before its signature or roster while the publisher is
    /// still uploading, and the build below it is then the candidate — the listing that came
    /// before skipped an incomplete release the same way. Only when no candidate arrives does
    /// the resolve fail, naming the first asset that did not.
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

    /// A FLOOR ABOVE THE PUBLISHED RUN: the floor is the SIGNED `index_build` of the last
    /// index this store took, which a rostered machine chooses, so one that signed the TOML
    /// integer ceiling into a release lifted every store that took it above every tag. With
    /// nothing above the floor and its own tag answering 404, the walk finds the run again
    /// as a store that has verified nothing does, and the owner's revoking republish (the
    /// newest tag) is a candidate again. The control is the re-cut window: a floor whose
    /// tag was deleted to be re-cut sits directly above the run, and its walk still fails,
    /// so the resolve keeps serving the cached floor index rather than candidates below it.
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
        /// The first build published beside the NEXT roster generation.
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

    /// A RUN FOUND BELOW THE FLOOR NEVER DISPLACES THE CACHED FLOOR INDEX. A store whose
    /// floor is 47 meets the owner deleting 47 to re-cut it, then 46 too: the walk finds the
    /// run again at 45, every candidate of which selection refuses (same generation, below
    /// the floor). The resolve still serves the cached 47, says it ran on the cache, keeps
    /// it cached beside what was served, and serves it offline after; a refused run above
    /// the floor, longer than the walk fetches, is cached beside it the same way, for
    /// `doctor` to name. The controls are the generations: a release
    /// served beside the NEXT roster drops the cached 47 even though its own index is
    /// refused — the source's refusal then stands, and its set replaces the cache — and the
    /// owner's republish of 45 at that generation, the rescue the rediscovery exists for,
    /// is selected live.
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
        // 1. First contact, on a prefix nothing has created yet: 47 live, cached, and the
        //    generation that admitted it recorded in the prefix.
        assert_eq!(
            pass(Some(6..=47), never, never, never),
            (Ok(47), true, cached(&[47]))
        );
        assert_eq!(
            crate::sig::Floor::new(store.roster_floor()).current(),
            testkit::SEQ
        );
        // 2. 47 deleted to be re-cut, 3. then offline: the cache serves.
        let from_cache = (Ok(47), false, cached(&[47]));
        assert_eq!(pass(Some(6..=46), never, never, never), from_cache);
        assert_eq!(pass(None, never, never, never), from_cache);
        // 4. 46 deleted as well, 5. then offline: the run found again at 45 is refused
        //    whole, and the cached floor index still serves, cached beside that run.
        let beside_run = (Ok(47), false, cached(&[45, 44, 43, 42, 47]));
        assert_eq!(pass(Some(6..=45), never, never, never), beside_run);
        assert_eq!(pass(None, never, never, never), beside_run);
        // 6. Restored.
        assert_eq!(
            pass(Some(6..=47), never, never, never),
            (Ok(47), true, cached(&[47]))
        );
        // 6b. Four releases above the floor signed by a key on no roster: the walk fetches
        //     nothing at or below the floor, the cached 47 serves, and the refused run is
        //     cached beside it — `doctor`'s "index 51 was downloaded but not accepted", where
        //     a cache of 47 alone said no complete newer release was there. Offline after,
        //     47 still serves.
        let beside_refused = (Ok(47), false, cached(&[51, 50, 49, 48, 47]));
        assert_eq!(pass(Some(6..=51), never, never, 48), beside_refused);
        assert_eq!(pass(None, never, never, 48), beside_refused);
        // 6c. The same with the floor index's window lapsed: it is refused `Stale` live, and
        //     beside the refused run it still wins and is refused the same way — the run is
        //     cached beside it all the same, for `doctor` to name.
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
        // 7. 48 published beside the next generation, signed by a key on no roster: nothing
        //    selects, the cached 47 least of all, so the source's verdict stands and what it
        //    served is the cache — `doctor`'s "index 48 was downloaded but not accepted".
        let refused = |builds: &[u64]| {
            (
                Err(crate::flow::FlowError::NoIndex.to_string()),
                true,
                cached(builds),
            )
        };
        assert_eq!(pass(Some(6..=48), 48, never, 48), refused(&[48, 47]));
        // 8. The run found below the floor beside that generation, its indexes declaring the
        //    old one: refused the same way.
        assert_eq!(
            pass(Some(6..=45), 0, never, never),
            refused(&[45, 44, 43, 42])
        );
        // 9. The owner republishes 45 at the next generation: selected live.
        assert_eq!(
            pass(Some(6..=45), 45, 45, never),
            (Ok(45), true, cached(&[45, 44, 43, 42]))
        );
    }

    /// THE ROUNDS AGAINST THE REFERENCE, over channels the cases above do not reach: a fixed
    /// stream of channels — starting anywhere from 1 to 40, running up to 80 builds, with up to
    /// three missing numbers (single holes, and now and then two side by side, where the walk's
    /// answer depends on where it happens to look) — each walked from no floor, from a floor in
    /// or beside the run, from its top, and from above it, with one roster missing somewhere.
    /// [`walk`] holds the round walk to the [`sequential_walk`] on every one: the same
    /// candidates or the same failure, the same link verdict, every build the walk asked asked.
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
            // A build seen published, when there is one: in the run, at its top, behind it,
            // or above it — and where the run starts, right, wrong, or unknown. A guess the
            // rounds follow, never the answer.
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

    /// How many ROUNDS discovery takes — what its wall time is made of, since a round costs
    /// one HEAD's latency however many it asks at once. The steady state is one round of two
    /// HEADs; a store one index behind, two; a store that has verified nothing meets the
    /// public channel (atpkg-index-6 to 71 as this is written, the walk alone seventeen HEADs
    /// in turn) in three; and every channel from 6 up to any build below a thousand, from no
    /// floor or from any floor in it, in at most five.
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
        // And the fetcher's own walk goes out in those rounds, not a HEAD at a time.
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
        // A store that verified nothing but holds the §14 cache (a pass that resolved and
        // landed nothing) guesses from the cache's highest build: one round fewer when the
        // channel has not moved since, the same answer and no more than the walk's bound of
        // rounds when it has, or when the hint is no build the channel publishes.
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
        // A fresh install whose release cut compiled in the run it saw (atpkg-index-8 to 71,
        // `ship release`'s ATPKG_INDEX_HINT) guesses its whole first round: one round while the
        // channel has not moved since, two while it moved up to 32 builds on; and a floor one
        // index behind that the window's probe woke for the next one walks in one round, not two.
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
        // The cache's hint buys the same once the channel moved past it: two rounds to 76,
        // three past it, where its guess alone took four from 73 on.
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
        // A probe build at or below the floor (a pass verified it since the probe woke this
        // one) is no hint: the steady state's one round of two HEADs.
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
    /// backs off to try again — the offline first launch's whole wait. One HEAD a host
    /// answered, or failures of any other kind (a timeout after the handshake), and every HEAD
    /// whose first attempt may still change gets the rest of its attempts, and nothing is held
    /// down. Later rounds ask whole.
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
            // One HEAD of the round reached no host on its first attempt and every other
            // answered: it alone gets the rest of its attempts, and the walk lands what the
            // channel publishes.
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
            // Every first attempt failed, none for want of a host (a transfer that timed
            // out): each gets its honest retries.
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
    /// what the round's link verdict leaves a request after it. Nothing without the parent's
    /// directory.
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
            // The class, not curl's words: a link held down fails as a transport error.
            Err(aterm_update_core::HttpError::Transport(_)) => "after err transport".into(),
            Err(e) => format!("after err {e:?}"),
        });
        std::fs::write(PathBuf::from(dir).join("verdicts"), out.join("\n")).unwrap();
    }

    /// PKG-4'S LINK PROBE, COUNTED IN PROCESSES. The first round runs in a child of this test
    /// binary ([`probe_a_first_round_over_counted_curl`]) whose every curl is a wrapper that
    /// logs its URL and answers as the scenario says. On a dead link (every attempt exits 7,
    /// no host) the round spends exactly ONE curl per HEAD, and the link it then holds down
    /// answers the next request in one more. On a live one (two HEADs answer 404, the third
    /// reaches no host) that third HEAD still gets attempts two and three — the round's
    /// answers say the link is up — and the others one each.
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

    /// THE MANIFEST PREFETCH: both halves of every pinned pair asked at once, each under its
    /// own cap, and memoized only WHOLE. So the reads after it fetch nothing; a pair whose
    /// signature did not arrive is fetched again by the read that wants it; and a pair the
    /// prefetch filled still has to verify — the memo holds bytes, not trust, and a tampered
    /// signature it carries is refused exactly as one fresh off the wire.
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
        // Whole pairs serve the reads with nothing fetched — and still have to verify.
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
        // The pair that did not arrive whole is the read's to fetch, and its failure too.
        let err = f.manifest_pair("nn", "nn", 108, &wire).unwrap_err();
        assert!(err.contains("HTTP 503"), "{err}");
        assert_eq!(asked.lock().unwrap().len(), 2, "fetched again on the read");
        // What the memo holds is not asked again.
        asked.lock().unwrap().clear();
        f.prefetch_with(&pins[..2], &wire);
        assert!(asked.lock().unwrap().is_empty());
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

    /// The quad is the four DERIVED download URLs of one tag — the same builder every other
    /// lane uses — and an unsafe tag derives nothing.
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
        // Still below the §14 cache's candidate ceiling with room for the cached index that
        // serves beside a refused set, or a full set could never be persisted for the
        // offline fallback — a ceiling on the SOURCE.
        const { assert!(INDEX_CANDIDATE_CAP < 24) };
    }
}
