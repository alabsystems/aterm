// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The production GitHub-Releases [`Fetcher`](crate::flow::Fetcher) (§5/§9) — the network
//! impl the install flow ([`crate::flow`]) runs against a real repo.
//!
//! ONE TRANSPORT (R3, 2026-09-23): every byte it reads — the index and its roster, each
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
//!   publisher's guarantee that index builds are contiguous — `tools/atpkg-index.sh`
//!   refuses an `index_build` that is not its baseline's plus one, and no index release is
//!   ever deleted but to be re-cut under the same number;
//! * which build of a program is fetched is named by the signed index's pin, and the
//!   publishing tag convention (`atpkg-<program>-<build>`) names its manifest and artifact.
//!
//! Until 2026-09-23 a listing of `…/releases` on the API backed every one of these up — the
//! index discovery, each manifest, each artifact — and the anonymous budget it spent is what
//! the 0.91 cut and a first-launch pass died on (audit 2026-09-23, RC1). A repointed account
//! (`[packages].account`, a development setting) or a `[packages.links]` fetch override must
//! therefore be publicly readable, as the app's own channel must.
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

/// The ONE way this fetcher reads bytes from the release download host — and it has
/// no credential parameter, so "no token is ever presented to `github.com`" is a fact
/// of the type, not of each call site's discipline. Refuses an API URL outright, and the
/// transport refuses the converse (`aterm_update_core` will not pair a token with a
/// non-API host).
fn web_fetch_bytes(url: &str, cap: u64) -> Result<Vec<u8>, String> {
    refuse_api_host(url)?;
    aterm_update_core::download_bytes(url, None, cap)
}

/// [`web_fetch_bytes`] for the artifact lane: resumable, to `dest`, no credential.
fn web_fetch_to(url: &str, dest: &Path, cap: u64) -> Result<(), String> {
    refuse_api_host(url)?;
    aterm_update_core::download_to_resumable(url, None, dest, cap)
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
/// The publishing tag convention is `atpkg-<program>-<build>` (tools/atpkg-publish.sh),
/// the same string the pack scripts create the release under.
fn direct_manifest_urls(slug: &str, program: &str, build: u64) -> Option<(String, String)> {
    if program.is_empty()
        || !program
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    let b = crate::dec_u64(build);
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
/// `atpkg-<prog>-<build>` release (tools/atpkg-pack.sh, tools/atpkg-pack-bundle.sh,
/// tools/linux-auto-atpkg.sh). Taking the whole stem derived
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
const MANIFEST_CAP: u64 = 5_000_000; // 5 MB
const SIG_CAP: u64 = 4_096; // an Ed25519 detached sig is 64 bytes; cap generously
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
const ROSTER_CAP: u64 = 65_536;

/// How many index releases one resolve fetches the signed quad for, NEWEST FIRST: the
/// newest published index and the ones below it, never below the lowest build the walk
/// knows is published (the store's verified floor, which nothing lower can pass anyway).
///
/// Selection takes the highest signed `index_build` within the newest admitted roster
/// generation, so the newest release wins essentially always; the others are the genuine
/// fallbacks — a newest index that fails verification, or whose assets are still being
/// uploaded. In the steady state the newest IS the floor, and one quad is fetched. Still
/// below the §14 cache's own candidate cap (`cache::MAX_CACHE_CANDIDATES` = 24), so a full
/// candidate set is never refused by the cache write.
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

/// `atpkg-index-<build>` — the index publisher's tag (`tools/atpkg-index.sh`).
fn index_tag(build: u64) -> String {
    let mut tag = String::from("atpkg-index-");
    tag.push_str(&crate::dec_u64(build));
    tag
}

/// THE NEWEST PUBLISHED INDEX, by HEADs of its tags alone: `probe(n)` answers whether
/// `atpkg-index-<n>` is published (`Err` when the answer says neither). Returns the newest
/// build, and the lowest build known published — the candidates' lower bound.
///
/// It rests on the PUBLISHER'S GUARANTEE that index builds are contiguous: every number from
/// the channel's first index to its newest is published, because `tools/atpkg-index.sh`
/// refuses an `index_build` that is not its baseline's plus one, and no index release is
/// deleted but to be re-cut under the same number. The published builds are therefore one
/// run, and a miss above a published build bounds it:
///
/// * the run's lower end is the store's verified `floor` — published when this store
///   verified it — or, on a store that has verified nothing, the first power of two that
///   answers (the public channel's first index is atpkg-index-6, so 8 does);
/// * from there the walk GALLOPS — HEAD `low+1`, `low+2`, `low+4`, … — to the first miss,
///   then bisects between it and the last hit, landing a published `hit` whose `hit+1` is
///   not;
/// * and it HEADs `hit+2` before it believes that: published, the walk gallops on from
///   there. So ONE missing number anywhere — a first publish numbered by hand, a release
///   deleted and never re-cut — is stepped over, where one directly above a store's floor
///   used to hold that store at its floor for good (the probe HEADs only `floor+1`, and
///   this walk stopped at the same miss). Two consecutive missing numbers can still end
///   the run — always when they sit directly above the floor — and the builds above them
///   are then reached only once the missing ones are published.
///
/// Two HEADs in the steady state (`floor+1` and `floor+2` absent), three when one index was
/// published. Before 2026-09-23 a walk that could not prove it had found the newest listed
/// the releases on the metered API instead; the guarantee is what lets it prove it.
fn newest_index(
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

/// The four assets of `quad`, whole, or the first that did not arrive.
fn fetch_quad(quad: &IndexQuad, fetch: &mut WebFetch<'_>) -> Result<Candidate, String> {
    Ok(Candidate {
        label: quad.label.clone(),
        index_bytes: fetch(&quad.index, MANIFEST_CAP)?,
        sig: fetch(&quad.index_sig, SIG_CAP)?,
        roster_bytes: fetch(&quad.roster, ROSTER_CAP)?,
        roster_sig: fetch(&quad.roster_sig, SIG_CAP)?,
    })
}

/// A redirect-refusing HEAD of one derived URL — `aterm_update_core::head_no_redirect` in
/// production, injected by the tests of [`GithubFetcher::walk_candidates`].
type HeadFn<'a> =
    dyn FnMut(&str) -> Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError> + 'a;

/// A capped byte GET of one derived URL — [`web_fetch_bytes`] in production, injected by the
/// tests of [`GithubFetcher::walk_candidates`].
type WebFetch<'a> = dyn FnMut(&str, u64) -> Result<Vec<u8>, String> + 'a;

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
    /// Whether the last discovery's HEAD failed on the LINK (curl reached no host) — what
    /// [`crate::flow::Fetcher::index_link_down`] reads.
    link_down: std::sync::atomic::AtomicBool,
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
            link_down: std::sync::atomic::AtomicBool::new(false),
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

    /// `<owner>/<repo>` under this fetcher's account (manual concat — `format!` expands to
    /// `fmt::Arguments` construction the strict Trust gate cannot lower).
    fn slug_of(&self, repo: &str) -> String {
        let mut slug = self.owner.clone();
        slug.push('/');
        slug.push_str(repo);
        slug
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

    /// THE INDEX CANDIDATES, from the download host alone, over an injected HEAD and byte
    /// fetch — the seam [`crate::flow::Fetcher::index_candidates`] runs, measurable without a
    /// network.
    ///
    /// [`newest_index`] finds the newest published build from this store's verified floor,
    /// each HEAD read as the next-index probe reads it ([`crate::index_probe::classify`]:
    /// only GitHub's own 302/307 to its release-asset storage is a publication, a 404 its
    /// absence, and anything else — a 200 from a captive portal, a 429, a 5xx — says
    /// nothing, and fails the discovery). Then the quads of the newest build and the ones
    /// below it, down to [`INDEX_CANDIDATE_CAP`] and never below the lowest build known
    /// published, are fetched newest first. A release whose four assets do not all arrive
    /// is LEFT OUT — an index can be up before its signature or roster while the publisher
    /// is still uploading — so that release is simply not a candidate, as the listing that
    /// came before skipped an incomplete release; only when none arrives does the resolve
    /// fail. A HEAD that reached no host marks the link down ([`Self::link_down`]).
    fn walk_candidates(
        &self,
        slug: &str,
        head: &mut HeadFn<'_>,
        fetch: &mut WebFetch<'_>,
    ) -> Result<Vec<Candidate>, String> {
        use crate::index_probe::{Probe, classify};
        // The link verdict is this walk's own: a failure an earlier walk in this process met
        // says nothing about this one.
        self.link_down
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let mut heads = 0u32;
        let mut probe = |build: u64| -> Result<bool, String> {
            heads += 1;
            if heads > WALK_HEAD_BUDGET {
                let mut msg = String::from("index discovery on ");
                msg.push_str(slug);
                msg.push_str(" found no end within ");
                msg.push_str(&crate::dec_u64(u64::from(WALK_HEAD_BUDGET)));
                msg.push_str(" HEADs");
                return Err(msg);
            }
            let tag = index_tag(build);
            let url =
                web_asset_url(slug, &tag, "index.toml").ok_or_else(|| underivable(slug, &tag))?;
            let answer = head(&url).map_err(|e| {
                if matches!(e, aterm_update_core::HttpError::Transport(_)) {
                    self.link_down
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                }
                let mut msg = String::from("HEAD ");
                msg.push_str(&url);
                msg.push_str(": ");
                msg.push_str(&e.to_string());
                msg
            })?;
            let code = answer.code;
            match classify(build, Ok(answer)) {
                Probe::Published(_) => Ok(true),
                Probe::Missing => Ok(false),
                Probe::Deferred => {
                    let mut msg = String::from("HEAD ");
                    msg.push_str(&url);
                    msg.push_str(" answered ");
                    msg.push_str(&crate::dec_u64(u64::from(code)));
                    msg.push_str(", not GitHub's redirect to a release asset");
                    Err(msg)
                }
            }
        };
        let (newest, low) = newest_index(self.index_floor, &mut probe)?;
        let oldest = newest.saturating_sub(INDEX_CANDIDATE_CAP - 1).max(low);
        let mut out = Vec::new();
        let mut first_error: Option<String> = None;
        for build in (oldest..=newest).rev() {
            let tag = index_tag(build);
            let quad = index_quad(slug, &tag).ok_or_else(|| underivable(slug, &tag))?;
            match fetch_quad(&quad, fetch) {
                Ok(candidate) => out.push(candidate),
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
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
        let out = self.walk_candidates(
            &self.index_slug(),
            &mut aterm_update_core::head_no_redirect,
            &mut web_fetch_bytes,
        )?;
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
        // master-rooted chain covers. The bytes are still verified exactly as before
        // (`verify_pkg` → `parse_pkg`, which re-binds program and build), so the host remains
        // a transport, never an authenticity input (§8).
        let (toml_url, sig_url) = direct_manifest_urls(&slug, program, build)
            .ok_or_else(|| underivable(&slug, program))?;
        let pair = std::sync::Arc::new((
            web_fetch_bytes(&toml_url, MANIFEST_CAP)?,
            web_fetch_bytes(&sig_url, SIG_CAP)?,
        ));
        // Successes only — a failed download stays retryable.
        if let Ok(mut memo) = self.manifests.lock() {
            memo.insert(key, std::sync::Arc::clone(&pair));
        }
        Ok((*pair).clone())
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
        aterm_update_core::download_to_resumable_https_only(url, None, dest, cap)
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

    fn source_id(&self) -> String {
        format!("dir:{}", self.dir.display())
    }
}

/// Tier-1 of `AtpkgIndexPublishWalk`: the real tag walk and `tools/atpkg-index.sh` against
/// the derived model of the index channel.
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
    /// (the `$PROG-$BUILD-$TRIPLE.tar.zst` ASSET of tools/atpkg-pack.sh and
    /// tools/atpkg-pack-bundle.sh, and tools/linux-auto-atpkg.sh's `gh release upload
    /// atpkg-$prog-$b`). A tag taken from
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

    /// Every direct URL is on the UNMETERED web host and none is on the API host — the
    /// property that makes the direct lane free — and the same string the app updater
    /// would derive for the same four inputs (one builder, one convention).
    #[test]
    fn direct_urls_are_on_the_web_host_never_the_api_and_match_the_shared_builder() {
        let (toml, sig) = super::direct_manifest_urls("alabsystems/ty", "ty", 2973).unwrap();
        let artifact = super::direct_asset_url("alabsystems/ty", "ty-2973.tar.zst").unwrap();
        for url in [&toml, &sig, &artifact] {
            assert!(url.starts_with("https://github.com/"), "{url}");
            assert!(!aterm_update_core::cdn::is_api_host(url), "{url}");
        }
        assert_eq!(
            Some(toml.as_str()),
            aterm_update_core::cdn::release_download_url(
                "alabsystems",
                "ty",
                "atpkg-ty-2973",
                "pkg-ty-2973.toml"
            )
            .as_deref()
        );
        assert_eq!(
            Some(artifact.as_str()),
            aterm_update_core::cdn::release_download_url(
                "alabsystems",
                "ty",
                "atpkg-ty-2973",
                "ty-2973.tar.zst"
            )
            .as_deref()
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

    /// No credential on the web host, pinned STRUCTURALLY from both sides: the only
    /// functions this fetcher reads `github.com` through (`web_fetch_bytes`,
    /// `web_fetch_to`) have no token parameter and refuse an API URL before any
    /// request; and the transport underneath refuses a token paired with a non-API
    /// host. Neither refusal needs the network, so both are asserted here.
    #[test]
    fn direct_fetches_present_no_credential_to_the_web_host() {
        let api = "https://api.github.com/repos/alabsystems/ty/releases/assets/1";
        let web =
            "https://github.com/alabsystems/ty/releases/download/atpkg-ty-2973/ty-2973.tar.zst";
        let dest = std::env::temp_dir().join("atpkg-web-fetch-gate");
        let refused = super::web_fetch_bytes(api, 1024).unwrap_err();
        assert!(
            refused.contains("not the release download host"),
            "{refused}"
        );
        let refused = super::web_fetch_to(api, &dest, 1024).unwrap_err();
        assert!(
            refused.contains("not the release download host"),
            "{refused}"
        );
        assert!(super::refuse_api_host(web).is_ok());
        // The converse gate, in the transport every lane shares.
        let refused = aterm_update_core::download_bytes(web, Some("ghp_x"), 1024).unwrap_err();
        assert!(refused.contains("non-API host"), "{refused}");
        let refused =
            aterm_update_core::download_to_resumable(web, Some("ghp_x"), &dest, 1024).unwrap_err();
        assert!(refused.contains("non-API host"), "{refused}");
        assert!(!dest.exists(), "nothing was spawned");
    }

    #[test]
    fn dir_fetcher_rejects_traversal_names() {
        use crate::flow::Fetcher as _;
        let d = std::env::temp_dir().join(format!("atpkg-dirfetch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
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

    #[test]
    fn dir_source_id_is_canonical() {
        use crate::flow::Fetcher as _;
        let d = std::env::temp_dir().join(format!("atpkg-dirsrc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let canon = std::fs::canonicalize(&d).unwrap();
        let f = DirFetcher::new(d.clone());
        assert_eq!(f.source_id(), format!("dir:{}", canon.display()));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn dir_manifest_sparse_oversize_is_rejected_without_allocation() {
        use crate::flow::Fetcher as _;

        let d = std::env::temp_dir().join(format!("atpkg-dirfetch-sparse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let index = std::fs::File::create(d.join("index.toml")).unwrap();
        index.set_len(MANIFEST_CAP + 1).unwrap();
        std::fs::write(d.join("index.toml.sig"), b"sig").unwrap();
        // Publish a roster too, so the empty result below is the OVERSIZE index and not
        // merely the missing roster — the refusal under test must be the one named.
        std::fs::write(d.join("aterm-machines.toml"), b"roster").unwrap();
        std::fs::write(d.join("aterm-machines.toml.sig"), b"sig").unwrap();
        assert!(
            DirFetcher::new(d.clone())
                .index_candidates()
                .unwrap()
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(d);
    }

    #[cfg(unix)]
    #[test]
    fn dir_manifest_fifo_and_symlink_return_without_blocking() {
        use crate::flow::Fetcher as _;
        use std::os::unix::ffi::OsStrExt as _;

        let d = std::env::temp_dir().join(format!("atpkg-dirfetch-special-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
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
        let _ = std::fs::remove_dir_all(d);
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
        let dir = std::env::temp_dir().join(format!("atpkg-hl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
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
                VendorFetchError::Refused(m) => {
                    assert!(m.contains("pinned vendor-direct prefixes"), "{url}: {m}");
                }
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
    /// listing behind the derived URL any more (R3, 2026-09-23), so the manifest, the unsized
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
        let err = f.pkg_manifest("ty", "../ty", 1).unwrap_err();
        assert!(err.starts_with("no release download URL derives"), "{err}");
        let err = f.download("ty", "ty-latest.tar.zst", &dest).unwrap_err();
        assert!(err.starts_with("no release download URL derives"), "{err}");
        let err = f
            .download_for("ty", "ty", "ty-latest.tar.zst", &dest, 1024)
            .unwrap_err();
        assert!(err.starts_with("no release download URL derives"), "{err}");
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
        /// The build of every HEAD, in order.
        heads: Vec<u64>,
        /// Every URL asked — the HEADs, then the asset downloads.
        asked: Vec<String>,
        /// Whether the fetcher now reports the link down.
        link_down: bool,
    }

    /// One discovery on the shipped index repository from `floor`, every HEAD answered by
    /// `answer` and every asset download succeeding unless its URL ends with one of
    /// `missing`.
    fn walk(
        floor: u64,
        answer: &dyn Fn(u64) -> Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError>,
        missing: &[&str],
    ) -> Walked {
        use crate::flow::Fetcher as _;
        let f = GithubFetcher::new("alabsystems".into()).with_index_floor(floor);
        let asked = std::cell::RefCell::new(Vec::<String>::new());
        let mut heads = Vec::new();
        let labels = f
            .walk_candidates(
                &f.index_slug(),
                &mut |url: &str| {
                    asked.borrow_mut().push(url.to_string());
                    let build = walked_build(url);
                    heads.push(build);
                    answer(build)
                },
                &mut |url: &str, _cap: u64| {
                    asked.borrow_mut().push(url.to_string());
                    if missing.iter().any(|m| url.ends_with(m)) {
                        let mut msg = String::from("HTTP 404 for ");
                        msg.push_str(url);
                        return Err(msg);
                    }
                    Ok(url.as_bytes().to_vec())
                },
            )
            .map(|candidates| candidates.into_iter().map(|c| c.label).collect());
        Walked {
            labels,
            heads,
            asked: asked.into_inner(),
            link_down: f.index_link_down(),
        }
    }

    fn labels(builds: &[u64]) -> Result<Vec<String>, String> {
        Ok(builds.iter().map(|b| index_tag(*b)).collect())
    }

    /// THE REQUIRED NET TEST (R3, 2026-09-23). From a verified floor of 44, `atpkg-index-45`
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
            w.asked.len(),
            3 + 8,
            "three HEADs, four assets per candidate"
        );
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
    /// HEADs — the next number and the one past it — and one quad; a store five builds behind gallops and bisects to the newest and
    /// fetches the four below it; and a store that has verified NOTHING finds the channel
    /// from its first power of two (the public channel starts at atpkg-index-6, so 8) — none
    /// of it with a listing, which is what each of these cases cost before.
    #[test]
    fn the_walk_finds_the_newest_from_any_floor_and_from_none() {
        let steady = walk(45, &channel(6, 45), &[]);
        assert_eq!(steady.labels, labels(&[45]));
        assert_eq!(steady.heads, vec![46, 47], "two HEADs in the steady state");

        let behind = walk(40, &channel(6, 45), &[]);
        assert_eq!(
            behind.labels,
            labels(&[45, 44, 43, 42]),
            "newest first, capped"
        );
        assert_eq!(behind.heads, vec![41, 42, 44, 48, 46, 45, 47]);

        let fresh = walk(0, &channel(6, 45), &[]);
        assert_eq!(fresh.labels, labels(&[45, 44, 43, 42]));
        assert_eq!(
            &fresh.heads[..4],
            &[1, 2, 4, 8],
            "the first power of two that answers"
        );
        assert_eq!(fresh.heads.len(), 17);

        // Nothing published at all: a named failure, after the bounded search.
        let empty = walk(0, &|_| absent(), &[]);
        let err = empty.labels.unwrap_err();
        assert_eq!(err, "no atpkg-index release is published");
        assert_eq!(empty.heads.len(), 21, "1, 2, 4, … 2^20");
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

    /// A MISSING NUMBER IS STEPPED OVER (review of the one-transport walk, 2026-09-23). The
    /// publisher refuses a gap, but a first publish numbered by hand or a release deleted and
    /// never re-cut can still leave one, and a gap directly above a store's verified floor
    /// used to hold that store at its floor for good: the walk HEADed `floor+1`, met the
    /// 404, and landed the floor on every pass. Exhaustively over small channels with every
    /// pattern of single missing numbers, from every floor, the walk lands the newest and the
    /// lower bound it reports is published. The control is the documented limit, so the step
    /// is not a walk that ignores misses: two consecutive missing numbers directly above the
    /// floor end the run at the floor.
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
            let err = w.labels.expect_err(why);
            assert!(
                err.contains("not GitHub's redirect to a release asset"),
                "{why}: {err}"
            );
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
        assert_eq!(offline.asked.len(), 1, "one HEAD, nothing fetched");
        assert!(offline.link_down, "no host answered");
        // The verdict is the last walk's: the same fetcher meeting a host that answers next
        // is not the link down.
        use crate::flow::Fetcher as _;
        let f = GithubFetcher::new("alabsystems".into()).with_index_floor(44);
        let slug = f.index_slug();
        let mut fetch = |url: &str, _: u64| Ok(url.as_bytes().to_vec());
        let mut dead = |_: &str| {
            Err(aterm_update_core::HttpError::Transport(
                "curl: (6) Could not resolve host".into(),
            ))
        };
        assert!(f.walk_candidates(&slug, &mut dead, &mut fetch).is_err());
        assert!(f.index_link_down());
        let mut busy = |_: &str| {
            Ok(aterm_update_core::HeadAnswer {
                code: 503,
                location: None,
            })
        };
        assert!(f.walk_candidates(&slug, &mut busy, &mut fetch).is_err());
        assert!(!f.index_link_down(), "a host answered this walk");
    }

    /// The walk reads a HEAD by the probe's rule, so a storage host GitHub has not used yet
    /// is a publication to it too: the day the release-asset storage moves again, discovery
    /// finds the newest index as before rather than failing every pass. The negative controls
    /// are the portal and login redirects of the test above, which still fail it.
    #[test]
    fn a_redirect_to_a_storage_host_not_yet_used_is_still_a_publication() {
        let moved = |build: u64| {
            if (6..=45).contains(&build) {
                Ok(aterm_update_core::HeadAnswer {
                    code: 302,
                    location: Some("https://release-objects.githubusercontent.com/o".into()),
                })
            } else {
                absent()
            }
        };
        let w = walk(44, &moved, &[]);
        assert_eq!(w.labels, labels(&[45, 44]));
        assert_eq!(w.heads, vec![45, 46, 47]);
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
        let w = walk(44, &channel(6, 46), &["atpkg-index-46/aterm-machines.toml"]);
        assert_eq!(w.labels, labels(&[45, 44]), "or its roster");
        let w = walk(44, &channel(6, 46), &["atpkg-index-45/index.toml.sig"]);
        assert_eq!(w.labels, labels(&[46, 44]), "an older one likewise");
        let w = walk(45, &channel(6, 45), &["/index.toml"]);
        let err = w.labels.unwrap_err();
        assert!(err.contains("atpkg-index-45/index.toml"), "{err}");
    }

    /// A host whose every answer is "published" cannot hold the walk: it stops at its HEAD
    /// budget with a named failure, and fetches nothing.
    #[test]
    fn the_walk_stops_at_its_head_budget() {
        let w = walk(44, &|_| published(), &[]);
        let err = w.labels.unwrap_err();
        assert!(err.contains("found no end within 48 HEADs"), "{err}");
        assert_eq!(w.heads.len(), usize::try_from(WALK_HEAD_BUDGET).unwrap());
        assert_eq!(w.asked.len(), w.heads.len(), "no asset was fetched");
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
        // Still below the §14 cache's candidate ceiling, or a full set could never be
        // persisted for the offline fallback — a ceiling on the SOURCE.
        const { assert!(INDEX_CANDIDATE_CAP <= 24) };
    }
}
