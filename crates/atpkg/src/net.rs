// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The production GitHub-Releases [`Fetcher`](crate::flow::Fetcher) (§5/§9) — the network
//! impl the install flow ([`crate::flow`]) runs against a real repo.
//!
//! It lists `…/releases` PAGINATED ([`paged_releases`]: `per_page=100`, up to
//! [`MAX_RELEASE_PAGES`] pages — the index and app releases share ONE repo, so the index
//! tag drifts down the listing at the app-release cadence and a single unpaginated page
//! lost it within days; `aterm-update`'s catalog walk paginates for the same reason) and,
//! for each release, locates the `<name>` + `<name>.sig` asset pair, then downloads their
//! bytes through `aterm-update-core`'s authenticated `curl` plumbing
//! (`api_get`/`download_bytes`/`download_to` — the SAME proven layer the macOS updater
//! uses). The asset-selection logic ([`find_pair`], [`index_pair_urls`]), the page walk
//! ([`paged_releases`]), and the releases-JSON shape ([`Release`]) are **pure and
//! unit-tested**; the network calls themselves are exercised only against a real release
//! (no fixture can stand in for GitHub), so they are a thin, faithful wrapper.
//!
//! The index lives on `<owner>/aterm` by default ([`crate::discovery::index_repo`],
//! `ATPKG_INDEX_REPO`-overridable);
//! each program's `pkg-*.toml` + artifacts live on that program's own repo (`repo`, §4.2),
//! which the flow threads in. The bytes are handed to the verifier **raw** (no lossy
//! conversion) — verification happens before any parse (§8).

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::select::Candidate;

/// A GitHub Release (subset). Unknown fields ignored.
#[derive(Debug, Deserialize)]
pub struct Release {
    /// The release tag (diagnostics / `Candidate::label`).
    #[serde(default)]
    pub tag_name: String,
    /// The release's assets.
    #[serde(default)]
    pub assets: Vec<Asset>,
}

/// A release asset (subset): its name + the API URL to download its bytes.
#[derive(Debug, Deserialize)]
pub struct Asset {
    /// The asset file name (e.g. `index.toml`, `index.toml.sig`).
    pub name: String,
    /// The asset's API URL (`…/releases/assets/<id>`), used for the octet download.
    pub url: String,
}

/// The release-download base for `slug` (`owner/repo`), or `None` if the slug is not
/// exactly one `owner/repo` pair of non-empty, path-safe segments.
///
/// Deliberately strict. These builders synthesize a URL instead of reading one out of an
/// API response, so a slug carrying a slash, a `..`, an empty half, or a scheme would
/// otherwise splice into a URL pointing somewhere else entirely. Returning `None` costs
/// only the enumeration fallback, which is the behaviour that shipped before.
fn release_download_base(slug: &str) -> Option<String> {
    let (owner, repo) = slug.split_once('/')?;
    let ok = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
    };
    if !ok(owner) || !ok(repo) {
        return None;
    }
    let mut base = String::from("https://github.com/");
    base.push_str(owner);
    base.push('/');
    base.push_str(repo);
    base.push_str("/releases/download/");
    Some(base)
}

/// The `(pkg-<program>-<build>.toml, .sig)` CDN URLs for a build already pinned by the
/// signed index — no API request, no discovery. `None` when the slug or program name is
/// not URL-safe, in which case the caller falls back to release enumeration.
///
/// The publishing tag convention is `atpkg-<program>-<build>` (tools/atpkg-publish.sh),
/// the same string the pack scripts create the release under.
fn direct_manifest_urls(slug: &str, program: &str, build: u64) -> Option<(String, String)> {
    let base = release_download_base(slug)?;
    if program.is_empty()
        || !program
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    let b = crate::dec_u64(build);
    // `atpkg-<program>-<build>/pkg-<program>-<build>.toml`
    let mut toml = base;
    toml.push_str("atpkg-");
    toml.push_str(program);
    toml.push('-');
    toml.push_str(&b);
    toml.push_str("/pkg-");
    toml.push_str(program);
    toml.push('-');
    toml.push_str(&b);
    toml.push_str(".toml");
    let mut sig = toml.clone();
    sig.push_str(".sig");
    Some((toml, sig))
}

/// The CDN URL for a release ASSET whose name already encodes its build
/// (`ty-2973.tar.zst` lives under tag `atpkg-ty-2973`), so the tag is the file name with
/// its extension removed. `None` when the name has no extension to strip or is not
/// URL-safe — again falling back to enumeration rather than guessing.
fn direct_asset_url(slug: &str, asset: &str) -> Option<String> {
    let base = release_download_base(slug)?;
    // Strip the FULL extension: these are `.tar.zst`, and `Path::file_stem` would leave
    // `ty-2973.tar`, naming a tag that does not exist.
    let stem = asset.split_once('.')?.0;
    if stem.is_empty()
        || !stem
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        || !asset
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
    {
        return None;
    }
    let mut url = base;
    url.push_str("atpkg-");
    url.push_str(stem);
    url.push('/');
    url.push_str(asset);
    Some(url)
}

/// Find the `(name, name.sig)` asset-URL pair in a release's assets, if **both** are
/// present. The signed pair is the unit the verifier needs; a release missing either is
/// skipped (no half-signed artifact is ever fetched).
#[must_use]
pub fn find_pair<'a>(assets: &'a [Asset], name: &str) -> Option<(&'a str, &'a str)> {
    let toml = assets.iter().find(|a| a.name == name)?;
    // Manual concat of the previous `format!("{name}.sig")` — byte-identical:
    // the `format!` expansion embeds `fmt::Arguments` construction (with
    // inlined `unsafe`) that the strict Trust gate cannot lower and fails
    // closed on.
    let mut sig_name = String::from(name);
    sig_name.push_str(".sig");
    let sig = assets.iter().find(|a| a.name == sig_name)?;
    Some((toml.url.as_str(), sig.url.as_str()))
}

/// Parse the GitHub `…/releases` JSON body into [`Release`]s.
pub fn parse_releases(body: &[u8]) -> Result<Vec<Release>, String> {
    serde_json::from_slice(body).map_err(|e| {
        // Manual concat of the previous `format!("parse releases JSON: {e}")` —
        // byte-identical (`{e}` is `Display`, which is what `to_string` renders):
        // the `format!` expansion embeds `fmt::Arguments` construction (with
        // inlined `unsafe`) that the strict Trust gate cannot lower and fails
        // closed on.
        let mut m = String::from("parse releases JSON: ");
        m.push_str(&e.to_string());
        m
    })
}

/// Caps (bytes) for the two asset classes — a manifest is a few KB; an artifact is tens of
/// MB up to a multi-GB toolchain bundle. Bound what an attacker-controlled asset can write.
const MANIFEST_CAP: u64 = 5_000_000; // 5 MB
const SIG_CAP: u64 = 4_096; // an Ed25519 detached sig is 64 bytes; cap generously
const ARTIFACT_CAP: u64 = 8u64 << 30; // 8 GiB ceiling for a toolchain bundle
/// A roster is a few hundred bytes per machine and is capped at 16 machines. Same ceiling
/// `aterm-update`'s armed path uses for the identical asset — one document, one bound.
const ROSTER_CAP: u64 = 65_536;

/// One release-listing page (GitHub's maximum) and the page-walk safety cap: 10 pages =
/// 1000 releases, the same bounds as `aterm-update`'s catalog walk (`github.rs`
/// `PER_PAGE`/`MAX_PAGES`). Load-bearing for updates: app releases and index releases
/// ride ONE repo, so every app cut pushes the newest `atpkg-index-*` release one row down
/// — the old single `per_page=20` page lost it in about a week of daily app releases
/// (2026-08-11 audit: atpkg-index-6 sat at row 11 after 9 days), silently starving every
/// client of toolchain updates until a republish.
const RELEASES_PER_PAGE: usize = 100;
const MAX_RELEASE_PAGES: u64 = 10;

/// How many index-carrying releases [`GithubFetcher::index_candidates`] downloads the
/// signed quad for, NEWEST FIRST.
///
/// # This number is the anonymous user's delivery budget
///
/// Each candidate costs FOUR asset downloads (`index.toml`, its `.sig`, the roster,
/// its `.sig`), so this constant multiplies by four into GitHub's **60 requests per
/// hour per IP** anonymous limit — the only budget a user has when they run the
/// advertised `curl … | bash` without `gh` installed. At 20 that was up to 80 requests
/// for candidate gathering alone, before a single package manifest or artifact:
/// roughly 90 against a ceiling of 60, i.e. a DETERMINISTIC failure, and worse on a
/// shared corporate NAT where the whole office draws on one budget.
///
/// Four is chosen because the extra candidates were nearly always dead weight.
/// Selection takes the HIGHEST signed `index_build` within the newest admitted roster
/// generation, releases are listed newest-first, and the publish counter is monotonic
/// — so the winner is the FIRST carrying release essentially always. The remaining
/// three are the genuine fallbacks (a newest release whose index fails verification,
/// sits below the durable floor, or carries a superseded generation), and paying 12
/// requests for those beats paying 64 for sixteen that never win.
///
/// Still below the §14 cache's own candidate cap (`cache::MAX_CACHE_CANDIDATES` = 24),
/// so a full candidate set is never refused by the cache write.
const INDEX_CANDIDATE_CAP: usize = 4;

/// Walk the release listing page by page via `fetch_page(page)` (1-based) until a short
/// page (the listing is exhausted) or [`MAX_RELEASE_PAGES`]. A mid-walk error fails the
/// WHOLE listing — a silently truncated catalog would reintroduce the pushed-off-page
/// blindness this walk exists to close — and an errored listing is never memoized, so a
/// transient page failure stays retryable.
fn paged_releases(
    mut fetch_page: impl FnMut(u64) -> Result<Vec<Release>, String>,
) -> Result<Vec<Release>, String> {
    let mut all = Vec::new();
    for page in 1..=MAX_RELEASE_PAGES {
        let batch = fetch_page(page)?;
        let exhausted = batch.len() < RELEASES_PER_PAGE;
        all.extend(batch);
        if exhausted {
            break;
        }
    }
    Ok(all)
}

/// The four asset URLs one candidate needs, resolved from a release's asset list.
struct CandidateUrls<'a> {
    label: &'a str,
    index: &'a str,
    index_sig: &'a str,
    roster: &'a str,
    roster_sig: &'a str,
}

/// The URL QUAD of each release carrying a COMPLETE authorization unit — `index.toml`,
/// its machine signature, AND the master-signed roster beside them — NEWEST FIRST
/// (listing order), capped at [`INDEX_CANDIDATE_CAP`]. Pure; the download loop above it
/// stays a thin wrapper.
///
/// A release missing ANY of the four contributes no candidate. That is the structural,
/// free half of "no roster, no authority": there is no shape of published release that
/// gets an index verified without the generation that authorized its signer, and no
/// fallback to a roster fetched from somewhere else. Whoever serves the index can withhold
/// it — they could always refuse to serve bytes — but they cannot get an OLDER root
/// honoured instead, because none remains.
fn index_pair_urls(releases: &[Release]) -> Vec<CandidateUrls<'_>> {
    let mut out = Vec::new();
    for r in releases {
        if out.len() >= INDEX_CANDIDATE_CAP {
            break;
        }
        let Some((index, index_sig)) = find_pair(&r.assets, "index.toml") else {
            continue;
        };
        let Some((roster, roster_sig)) =
            find_pair(&r.assets, aterm_update_core::roster::ROSTER_ASSET)
        else {
            continue;
        };
        out.push(CandidateUrls {
            label: r.tag_name.as_str(),
            index,
            index_sig,
            roster,
            roster_sig,
        });
    }
    out
}

/// The IDENTITY of one candidate: an opaque fingerprint that changes if and only if the
/// four assets behind it change — the cheap question
/// [`crate::flow::Fetcher::index_identities`] exists to answer.
///
/// # Why the listing already knows this
///
/// `Asset.url` IS `https://api.github.com/repos/<owner>/<repo>/releases/assets/<id>`, and
/// that id is minted per UPLOAD. GitHub has no edit-in-place for asset bytes: replacing
/// `index.toml` on a release means deleting the asset and uploading a new one, which
/// mints a new id and therefore a new URL. So the listing response — one request, which
/// the resolve had to make anyway — already carries a per-asset change token for all four
/// blobs. The old code parsed those URLs, downloaded through them once, and threw them
/// away, then spent sixteen more round-trips next pass rediscovering that nothing moved.
///
/// # This is a CHANGE DETECTOR, not a security primitive
///
/// Nothing downstream trusts it. A match only permits reusing bytes that are then handed
/// to `select_index` and face the identical master-signed roster admission, machine
/// signature, durable floor, roster ratchet and `valid_until` window (see the
/// [`crate::cache`] module doc). A mismatch costs a download. So the worst a frozen,
/// forged or colliding fingerprint can buy an adversary is SUPPRESSION — serving an older
/// already-published, already-verified index — which whoever serves the assets could
/// always do by simply not serving the new ones, and which the freshness window bounds.
///
/// The tag is folded in beside the URLs so that a release RETAGGED onto the same assets
/// is still a different candidate: `label` is what the cache entry, the diagnostics and
/// `Candidate::label` carry, and an identity that ignored it would let two publication
/// states share one fingerprint. NUL separators keep the concatenation unambiguous —
/// neither a git tag nor a URL can contain a NUL byte.
fn candidate_identity(u: &CandidateUrls<'_>) -> String {
    let mut h = aterm_digest::Sha256::new();
    for part in [u.label, u.index, u.index_sig, u.roster, u.roster_sig] {
        h.update(part.as_bytes());
        h.update([0u8]);
    }
    crate::tree::hex(&h.finalize())
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

/// The production fetcher: an `owner` account + a per-machine `token` (optional rate-limit
/// / private-repo aid, §5.1) + the optional per-program `[packages.links]` `owner/repo`
/// FETCH overrides. Construct with [`GithubFetcher::new`] (+ [`GithubFetcher::with_overrides`]).
pub struct GithubFetcher {
    owner: String,
    token: String,
    /// program → slug-validated `"owner/repo"`: where THAT program's `pkg-*.toml` +
    /// artifacts are fetched from instead of `<owner>/<index repo>` (a possibly-private
    /// repo, reached with the same token). NEVER an authenticity input: the index fetch
    /// is untouched, reachability (§5) still requires the index to name the program, and
    /// every byte still passes the identical signature/sha256/`tree_root` gates — an
    /// override can only redirect WHERE bytes come from, not what verifies.
    overrides: std::collections::BTreeMap<String, String>,
    /// PER-INVOCATION memo of the release listings, keyed by `owner/repo` slug.
    ///
    /// Every miss is a `curl` SUBPROCESS plus a DNS+TLS+HTTP round-trip to
    /// api.github.com — not an in-process call — and the flow lists the same slug
    /// repeatedly: `group_disk_required` → `stage_member` → `download_for` all resolve
    /// the same program, and a recursive `install_inner` repeats the index listing per
    /// transitive dependency. Memoizing collapses those to one request each, which also
    /// matters functionally: the anonymous lane (`credential()` → `None`) gets ~60
    /// requests/hour per IP and the layer has a dedicated `RateLimited` arm for it.
    releases: std::sync::Mutex<std::collections::BTreeMap<String, std::sync::Arc<Vec<Release>>>>,
    /// Per-invocation memo of the index candidate set — the expensive one: a miss lists
    /// the index repo AND downloads `index.toml` + `.sig` for every release carrying the
    /// pair (up to 20 × 2 asset downloads). `install_inner` re-resolves it once per
    /// recursive dependency and `install_default_set` once per ungrouped member.
    index: std::sync::Mutex<Option<std::sync::Arc<Vec<Candidate>>>>,
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
    /// A fetcher for `owner`, authenticating asset reads with `token` (may be empty for a
    /// public repo over the anonymous API, subject to GitHub's rate limit).
    #[must_use]
    pub fn new(owner: String, token: String) -> Self {
        Self {
            owner,
            token,
            overrides: std::collections::BTreeMap::new(),
            releases: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            index: std::sync::Mutex::new(None),
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

    /// The credential this fetcher presents, or `None` to request anonymously.
    ///
    /// `GithubFetcher::new` documents that an EMPTY token means "public repo over the
    /// anonymous API"; the network layer is now token-optional, so that intent is
    /// expressed directly instead of being smuggled through as an empty `Bearer`
    /// header (which the layer now refuses outright). atpkg's own token resolution is
    /// unchanged — this only maps "no token" onto the anonymous lane.
    fn credential(&self) -> Option<&str> {
        (!self.token.is_empty()).then_some(self.token.as_str())
    }

    /// The `<owner>/<repo>` slug `program`'s release fetches go to: the config
    /// fetch override when one is declared, else the index-declared `repo` under
    /// this fetcher's account.
    fn slug_for(&self, program: &str, repo: &str) -> String {
        if let Some(s) = self.overrides.get(program) {
            return s.clone();
        }
        // Manual concat (see `releases_at` note on `format!` and the Trust gate).
        let mut slug = self.owner.clone();
        slug.push('/');
        slug.push_str(repo);
        slug
    }

    /// List the releases (newest first, PAGINATED via [`paged_releases`]) of `slug`
    /// (`owner/repo`), memoized for the life of this fetcher (see the `releases` field
    /// for the cost model — one repo's walk is one request until it exceeds 100 releases).
    ///
    /// ONLY complete successes are memoized — an `Err` on ANY page fails the whole
    /// listing and must stay retryable, so a transient network failure is never frozen
    /// in (nor a truncated catalog served as complete). The lock is held ONLY around the
    /// map lookup/insert, never across the request, so an in-flight fetch can neither
    /// block another lane nor poison the mutex; a poisoned lock degrades to an uncached
    /// (correct) fetch rather than panicking.
    fn releases_at(&self, slug: &str) -> Result<std::sync::Arc<Vec<Release>>, String> {
        if let Ok(memo) = self.releases.lock()
            && let Some(hit) = memo.get(slug)
        {
            return Ok(std::sync::Arc::clone(hit));
        }
        let list = std::sync::Arc::new(paged_releases(|page| {
            // Manual concat of the previous
            // `format!("https://api.github.com/repos/{}/releases?…", ..)`
            // — byte-identical (`dec_u64` renders exactly as `u64`'s `Display`):
            // the `format!` expansion embeds `fmt::Arguments` construction (with
            // inlined `unsafe`) that the strict Trust gate cannot lower and fails
            // closed on.
            let mut url = String::from("https://api.github.com/repos/");
            url.push_str(slug);
            url.push_str("/releases?per_page=");
            url.push_str(&crate::dec_u64(RELEASES_PER_PAGE as u64));
            url.push_str("&page=");
            url.push_str(&crate::dec_u64(page));
            parse_releases(&aterm_update_core::api_get(&url, self.credential())?)
        })?);
        if let Ok(mut memo) = self.releases.lock() {
            memo.insert(slug.to_string(), std::sync::Arc::clone(&list));
        }
        Ok(list)
    }

    /// List a repo's recent releases under this fetcher's own account.
    fn releases(&self, repo: &str) -> Result<std::sync::Arc<Vec<Release>>, String> {
        let mut slug = self.owner.clone();
        slug.push('/');
        slug.push_str(repo);
        self.releases_at(&slug)
    }
}

impl crate::flow::Fetcher for GithubFetcher {
    fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
        // Memoized per invocation: the flow resolves the candidates once per program AND
        // once per transitive dependency, and each miss is a listing plus TWO asset
        // downloads per carrying release. The bytes are returned by value (a few KB of
        // TOML + 64-byte sigs — free next to the network), so the trait signature and
        // every downstream gate are untouched: the same raw bytes still flow through
        // `select_index` → `admit_roster` → `authorize_index` → `parse_index` → floor →
        // freshness.
        if let Ok(memo) = self.index.lock()
            && let Some(hit) = memo.as_ref()
        {
            return Ok((**hit).clone());
        }
        let releases = self.releases(&crate::discovery::index_repo())?;
        let mut out = Vec::new();
        // Newest-first, capped ([`index_pair_urls`]): the paginated listing may now span
        // hundreds of releases, and only the newest carrying releases can win selection.
        // The index repo's own `owner/repo`, built the same way `slug_for` builds a
        // program's (manual concat — see its note on `format!` and the Trust gate).
        let slug = {
            let mut s = self.owner.clone();
            s.push('/');
            s.push_str(&crate::discovery::index_repo());
            s
        };
        for u in index_pair_urls(&releases) {
            // ZERO-API ASSET FETCH, same derivation as `pkg_manifest`/`download_for`.
            // `u.label` IS the release tag, so each of these four assets has a
            // deterministic CDN URL and none of them needs the assets API. This is the
            // dominant remaining term: the listing above is ONE request, but the four
            // assets were four more PER CANDIDATE, which is what made index resolution
            // cost ~7 requests after the per-program cost went to zero.
            //
            // `direct` falls back to the API URL the listing already handed us whenever
            // the slug is not URL-safe or the download fails, so a private mirror or an
            // unusual asset host keeps working exactly as before.
            let direct = |asset: &str, api_url: &str, cap: u64| -> Result<Vec<u8>, String> {
                if let Some(base) = release_download_base(&slug) {
                    let mut url = base;
                    url.push_str(u.label);
                    url.push('/');
                    url.push_str(asset);
                    if let Ok(bytes) = aterm_update_core::download_bytes(&url, None, cap) {
                        return Ok(bytes);
                    }
                }
                aterm_update_core::download_bytes(api_url, self.credential(), cap)
            };
            let index_bytes = direct("index.toml", u.index, MANIFEST_CAP)?;
            let sig = direct("index.toml.sig", u.index_sig, SIG_CAP)?;
            // The roster rides the SAME release, so it is fetched here rather than once
            // per repo: a candidate is an index PLUS the generation that authorized its
            // signer, and pairing an index with any other generation is the substitution
            // the per-candidate binding exists to refuse.
            let roster_asset = aterm_update_core::roster::ROSTER_ASSET;
            let roster_bytes = direct(roster_asset, u.roster, ROSTER_CAP)?;
            let mut roster_sig_name = String::from(roster_asset);
            roster_sig_name.push_str(".sig");
            let roster_sig = direct(&roster_sig_name, u.roster_sig, SIG_CAP)?;
            out.push(Candidate {
                label: u.label.to_string(),
                index_bytes,
                sig,
                roster_bytes,
                roster_sig,
            });
        }
        // Successes only — a partial fetch that errored above never reaches here, so a
        // transient failure stays retryable.
        let out = std::sync::Arc::new(out);
        if let Ok(mut memo) = self.index.lock() {
            *memo = Some(std::sync::Arc::clone(&out));
        }
        Ok((*out).clone())
    }

    fn index_identities(&self) -> Option<Vec<String>> {
        // ONE request, and it is the request `index_candidates` was going to make anyway:
        // `releases_at` memoizes per process, so whichever of the two runs first pays the
        // listing and the other is free. That is what makes this probe honest — it cannot
        // ADD a round-trip, only remove sixteen.
        //
        // Derived from the SAME `index_pair_urls` walk the download loop runs, over the
        // same memoized listing, so the identities are in the same order and of the same
        // length as the candidates that loop would produce — the pairing contract
        // `Fetcher::index_identities` states. (The download loop `?`-propagates any asset
        // failure, so a partial candidate set never reaches the cache to be stamped.)
        //
        // A listing failure yields `None`, not an error: the caller then takes the
        // historical path, where the real fetch surfaces the real reason (and, failing
        // that, the §14 fallback answers). This probe must never be the thing that turns
        // an offline machine's diagnosis into "no signature-valid index".
        let releases = self.releases(&crate::discovery::index_repo()).ok()?;
        Some(
            index_pair_urls(&releases)
                .iter()
                .map(candidate_identity)
                .collect(),
        )
    }

    fn pkg_manifest(
        &self,
        repo: &str,
        program: &str,
        build: u64,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        // The program's manifest rides its release repo — the `[packages.links]`
        // fetch override redirects it (with the same token) when declared. The slug is
        // resolved ONCE: it keys the memo, drives the listing, and spells the error.
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
        // Manual concat of the previous `format!("pkg-{program}-{build}.toml")`
        // and `format!("no signed {name} in {repo} releases")` — byte-identical
        // (`dec_u64` renders `{build}` exactly as `u64`'s `Display`): the
        // `format!` expansion embeds `fmt::Arguments` construction (with inlined
        // `unsafe`) that the strict Trust gate cannot lower and fails closed on.
        let mut name = String::from("pkg-");
        name.push_str(program);
        name.push('-');
        name.push_str(&crate::dec_u64(build));
        name.push_str(".toml");
        // ZERO-API FAST PATH. The release TAG is `atpkg-<program>-<build>` by publishing
        // convention, and `build` arrived here from the SIGNED index's channel `pin`
        // table — so the exact asset URL is already determined and there is nothing to
        // discover. Enumerating `…/releases` to find an asset whose name was just
        // constructed on the line above costs API requests for information already held.
        //
        // That cost is not academic: `…/releases?per_page=100` is an api.github.com call,
        // the unauthenticated budget is 60/hour PER IP, and a 10-program default set
        // spends all of it — measured 2026-08-19, a clean `install --default-set`
        // installed 7 of 10 and then took HTTP 403 on ny, trust-mc and ty. The advertised
        // install is `curl … | bash`, whose user has no token, so the lane that must work
        // for a stranger was the lane that could not finish.
        //
        // Release DOWNLOADS are not API calls. `…/releases/download/<tag>/<asset>` 302s to
        // release-assets.githubusercontent.com and is CDN-served: measured with the API at
        // `remaining: 0`, that URL still returned HTTP 200. So this path is not merely
        // cheaper, it is unmetered.
        //
        // It is also STRICTER. Enumeration picks a release from an UNSIGNED API listing;
        // this derives the URL from the signed pin, so the answer to "which build" comes
        // only from bytes the master-rooted chain covers. The bytes fetched are still
        // verified exactly as before (`verify_pkg` → `parse_pkg`, which re-binds program
        // and build), so the host remains a transport, never an authenticity input (§8).
        //
        // Falls back to enumeration on ANY failure — a repo whose tags predate the
        // convention, or a private mirror that names releases differently, keeps working.
        if let Some((toml_url, sig_url)) = direct_manifest_urls(&slug, program, build)
            && let Ok(toml) =
                aterm_update_core::download_bytes(&toml_url, self.credential(), MANIFEST_CAP)
            && let Ok(sig) = aterm_update_core::download_bytes(&sig_url, self.credential(), SIG_CAP)
        {
            let pair = std::sync::Arc::new((toml, sig));
            if let Ok(mut memo) = self.manifests.lock() {
                memo.insert(key.clone(), std::sync::Arc::clone(&pair));
            }
            return Ok((*pair).clone());
        }
        let releases = self.releases_at(&slug)?;
        for r in releases.iter() {
            if let Some((toml_url, sig_url)) = find_pair(&r.assets, &name) {
                let toml =
                    aterm_update_core::download_bytes(toml_url, self.credential(), MANIFEST_CAP)?;
                let sig = aterm_update_core::download_bytes(sig_url, self.credential(), SIG_CAP)?;
                // Successes only — a not-found (below) or a failed download stays
                // retryable.
                let pair = std::sync::Arc::new((toml, sig));
                if let Ok(mut memo) = self.manifests.lock() {
                    memo.insert(key, std::sync::Arc::clone(&pair));
                }
                return Ok((*pair).clone());
            }
        }
        let mut msg = String::from("no signed ");
        msg.push_str(&name);
        msg.push_str(" in ");
        msg.push_str(&slug);
        msg.push_str(" releases");
        Err(msg)
    }

    fn download(&self, repo: &str, asset: &str, dest: &Path) -> Result<(), String> {
        let releases = self.releases(repo)?;
        for r in releases.iter() {
            if let Some(a) = r.assets.iter().find(|a| a.name == asset) {
                // RESUMABLE: this is the request that moves hundreds of megabytes, and a
                // stall at 95 % used to discard everything. The sha256 gate over the
                // complete file is unchanged and is still what makes the bytes
                // acceptable — see `download_to_resumable`.
                return aterm_update_core::download_to_resumable(
                    &a.url,
                    self.credential(),
                    dest,
                    ARTIFACT_CAP,
                );
            }
        }
        // Manual concat of the previous `format!("no asset {asset} in {repo} releases")`
        // — byte-identical: the `format!` expansion embeds `fmt::Arguments`
        // construction (with inlined `unsafe`) that the strict Trust gate cannot
        // lower and fails closed on.
        let mut msg = String::from("no asset ");
        msg.push_str(asset);
        msg.push_str(" in ");
        msg.push_str(repo);
        msg.push_str(" releases");
        Err(msg)
    }

    fn download_for(
        &self,
        program: &str,
        repo: &str,
        asset: &str,
        dest: &Path,
    ) -> Result<(), String> {
        // The artifact rides the SAME release repo as the program's manifest, so the
        // `[packages.links]` fetch override redirects it identically (same token). The
        // listing is the memoized one this program's `pkg_manifest` already paid for.
        let slug = self.slug_for(program, repo);
        // Same zero-API derivation as `pkg_manifest` (see its note): the artifact rides
        // the release its manifest does, and the asset name carries the build, so the tag
        // follows from the name alone (`ty-2973.tar.zst` → `atpkg-ty-2973`). This is the
        // request that actually moves hundreds of megabytes, and routing it through the
        // CDN URL rather than the assets API is what takes a default-set install off the
        // 60/hour meter entirely.
        //
        // RESUMABLE on both legs. The two legs are two HOSTS for the SAME signed object,
        // so a prefix left by a failed CDN attempt is a valid prefix for the API-URL
        // attempt that follows it — and if it ever is not, the sha256 gate over the
        // complete file refuses it, which costs exactly what a failed download costs
        // today.
        if let Some(url) = direct_asset_url(&slug, asset)
            && aterm_update_core::download_to_resumable(&url, self.credential(), dest, ARTIFACT_CAP)
                .is_ok()
        {
            return Ok(());
        }
        let releases = self.releases_at(&slug)?;
        for r in releases.iter() {
            if let Some(a) = r.assets.iter().find(|a| a.name == asset) {
                return aterm_update_core::download_to_resumable(
                    &a.url,
                    self.credential(),
                    dest,
                    ARTIFACT_CAP,
                );
            }
        }
        let mut msg = String::from("no asset ");
        msg.push_str(asset);
        msg.push_str(" in ");
        msg.push_str(&slug);
        msg.push_str(" releases");
        Err(msg)
    }

    fn source_id(&self) -> String {
        format!("github:{}/{}", self.owner, crate::discovery::index_repo())
    }
}

/// The offline / publisher-test fetcher (§14): a flat directory of the assets
/// `tools/atpkg-pack*.sh` emit (`index.toml`(`.sig`), `pkg-<program>-<build>.toml`(`.sig`),
/// and the artifact tarballs). Pure `std::fs`, no network. The bytes it serves are handed to
/// the flow RAW, so a `dir:` registry gets the IDENTICAL verify-before-parse + floor +
/// freshness gate as `github:` (a dir cache holds bytes, not trust).
pub struct DirFetcher {
    dir: PathBuf,
    /// Held for the fetcher's lifetime iff `dir` is THIS bundle's sealed seed:
    /// the durable "a live process is reading the seal" record that keeps the
    /// self-updater from swapping the bundle out from under a multi-GB
    /// extraction. Claimed HERE — the one choke point every seal-reading lane
    /// flows through (`cmd_seed`, and the empty-store bootstrap chain leg the
    /// network verbs mount) — so a user-run CLI is guarded exactly like the
    /// GUI's spawn lanes, with the extractor's OWN pid and no cross-process
    /// choreography (see `aterm_update_core::seal_guard`). `None` for every
    /// ordinary `dir:` registry, and always off-macOS.
    _seal_guard: Option<aterm_update_core::seal_guard::SealReadGuard>,
}

impl DirFetcher {
    /// A fetcher reading assets from `dir` (canonicalized when possible for a stable
    /// `source_id`).
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        // Compare canonical-to-canonical: `dir` was just canonicalized, and
        // the bundle path may reach the seal through a symlinked Resources.
        let seal = crate::bundled_seed_dir()
            .map(|seed| std::fs::canonicalize(&seed).unwrap_or(seed))
            .is_some_and(|seed| seed == dir);
        Self {
            dir,
            _seal_guard: seal
                .then(aterm_update_core::seal_guard::SealReadGuard::claim)
                .flatten(),
        }
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
        // UNLINK FIRST — this line prevents data destruction inside a signed app
        // bundle, and it is not optional.
        //
        // Without it: `hard_link` fails EEXIST when a previous run left a staging
        // link behind (nothing sweeps `staging/`, and an interrupted first run is
        // exactly what the resumable seed lane expects), so the fallback runs
        // `fs::copy(src, dest)` where — because they are the SAME hardlinked inode
        // — dest IS src. Rust's macOS copy opens the destination `O_TRUNC`,
        // truncating the shared inode, then copies the now-empty source and
        // returns `Ok(0)`. Measured on macOS 26.5: `src=0 dst=0`, success reported.
        //
        // For a `dir:` registry that source lives in
        // `aterm.app/Contents/Resources/toolchain-seed.lproj/`, so the victim is a
        // file inside the user's installed, notarized bundle. Zeroing it kills that
        // seed asset permanently (sha256 can never match again) AND invalidates the
        // code signature — the `.lproj` optional seal tolerates ABSENCE but not
        // MODIFICATION. An ordinary power-off during the first-run extraction was
        // enough to trigger it.
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

/// Two fetchers, one flow (§9.1 bundled seed): candidates from BOTH sources
/// feed the ONE index selection (highest signature-valid `index_build` ≥ floor
/// wins, [`crate::select_index`]) with the `primary` (network) leg as the
/// AUTHORITY — its candidates come first, so on an equal `index_build` tie it
/// wins, and only it feeds the §14 cache. Per-asset reads run the OTHER way:
/// `secondary` (the co-located seed — local bytes) first, falling back to
/// `primary` — the whole point of a batteries-included cut is that a first
/// run does not re-download the gigabytes the app already carries, and an
/// asset the seed never sealed (a fresher network pin) simply misses by name
/// and falls through. This is how a network registry and the app-bundle seed
/// coexist without a second trust path: a fresher published index outranks
/// the sealed seed by the ordinary monotonic gate, an offline machine still
/// resolves the seed's index, and every byte from EITHER source passes the
/// identical verify-before-parse + floor + freshness + sha256 + `tree_root`
/// gates. The chain is composition, never authenticity: neither side is
/// trusted more than its signatures prove.
pub struct ChainFetcher {
    primary: Box<dyn crate::flow::Fetcher>,
    secondary: Box<dyn crate::flow::Fetcher>,
    /// The primary (network) leg's OWN candidates from the latest
    /// [`crate::flow::Fetcher::index_candidates`] call — `None` when that leg failed.
    /// What `cacheable_candidates` serves, so the §14 cache write is keyed off the
    /// network leg without a second fetch, and a seed-leg success can never refresh or
    /// overwrite the last-good network cache (the cache-masking tooth, adversarial
    /// review 2026-07-30).
    primary_candidates: std::sync::Mutex<Option<Vec<Candidate>>>,
}

impl ChainFetcher {
    /// Chain `primary` (the index/cache authority) over `secondary` (the
    /// local-bytes leg, tried first for every asset).
    #[must_use]
    pub fn new(
        primary: Box<dyn crate::flow::Fetcher>,
        secondary: Box<dyn crate::flow::Fetcher>,
    ) -> Self {
        Self {
            primary,
            secondary,
            primary_candidates: std::sync::Mutex::new(None),
        }
    }
}

impl crate::flow::Fetcher for ChainFetcher {
    fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
        // The union of both sides' candidates; a side that errors (offline
        // GitHub, say) contributes nothing rather than failing the other side.
        // BOTH failing is a real error — surface both reasons.
        let p = self.primary.index_candidates();
        // Record the network leg's own outcome for `cacheable_candidates` BEFORE the
        // union — the §14 cache must never absorb secondary-leg (seed) bytes. A
        // poisoned lock skips the record, which downstream reads as "nothing to
        // cache": conservative, never wrong.
        if let Ok(mut memo) = self.primary_candidates.lock() {
            *memo = p.as_ref().ok().cloned();
        }
        match (p, self.secondary.index_candidates()) {
            (Ok(mut a), Ok(b)) => {
                a.extend(b);
                Ok(a)
            }
            (Ok(a), Err(_)) => Ok(a),
            (Err(_), Ok(b)) => Ok(b),
            (Err(a), Err(b)) => Err(format!(
                "{} failed: {a}; {} failed: {b}",
                self.primary.source_id(),
                self.secondary.source_id()
            )),
        }
    }

    fn pkg_manifest(
        &self,
        repo: &str,
        program: &str,
        build: u64,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        // Local (seed) bytes first — manifest names are build-qualified, so a
        // pin the seed never sealed misses by name and falls to the network;
        // either side's bytes verify identically downstream.
        self.secondary
            .pkg_manifest(repo, program, build)
            .or_else(|e1| {
                self.primary
                    .pkg_manifest(repo, program, build)
                    .map_err(|e2| chain_err(&e2, &e1))
            })
    }

    fn download(&self, repo: &str, asset: &str, dest: &Path) -> Result<(), String> {
        self.secondary.download(repo, asset, dest).or_else(|e1| {
            self.primary
                .download(repo, asset, dest)
                .map_err(|e2| chain_err(&e2, &e1))
        })
    }

    fn download_for(
        &self,
        program: &str,
        repo: &str,
        asset: &str,
        dest: &Path,
    ) -> Result<(), String> {
        // Route through both sides' OWN `download_for` so the primary's
        // per-program `[packages.links]` fetch override still applies on the
        // fallback leg.
        self.secondary
            .download_for(program, repo, asset, dest)
            .or_else(|e1| {
                self.primary
                    .download_for(program, repo, asset, dest)
                    .map_err(|e2| chain_err(&e2, &e1))
            })
    }

    fn source_id(&self) -> String {
        // A chain is not either source alone: a cache written under one leg's
        // id must not satisfy the other, so the id names both.
        format!(
            "chain:{}+{}",
            self.primary.source_id(),
            self.secondary.source_id()
        )
    }

    fn cache_source_id(&self) -> String {
        // The §14 cache identity is the NETWORK (primary) leg's, not the chain's:
        // the cache a bootstrap-time chain writes must serve the post-bootstrap
        // plain-network path (same key), and the `dir:` seed leg must never gain a
        // cache identity of its own through the chain.
        self.primary.cache_source_id()
    }

    fn cacheable_candidates(&self, _resolved: &[Candidate]) -> Option<Vec<Candidate>> {
        // The network leg's own candidates from THIS call's `index_candidates`
        // (recorded there), never the union: a seed-leg success must not mask a
        // network failure into a cache refresh, nor overwrite the last-good
        // network candidates with sealed-seed bytes.
        self.primary_candidates
            .lock()
            .ok()
            .and_then(|memo| memo.clone())
    }
}

/// Both legs failed; keep both reasons, the network (primary) story FIRST
/// regardless of which leg was tried first — the seed dir's "no such file"
/// alone would mislead when the real story is offline.
fn chain_err(primary: &str, secondary: &str) -> String {
    format!("{primary}; fallback: {secondary}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELEASES_JSON: &[u8] = br#"[
      { "tag_name": "v0.2.0", "assets": [
          { "name": "index.toml",     "url": "https://api.github.com/.../assets/1" },
          { "name": "index.toml.sig", "url": "https://api.github.com/.../assets/2" },
          { "name": "noise.txt",      "url": "https://api.github.com/.../assets/3" }
      ]},
      { "tag_name": "v0.1.0", "assets": [
          { "name": "index.toml", "url": "https://api.github.com/.../assets/4" }
      ]}
    ]"#;

    #[test]
    fn parses_releases_and_finds_the_signed_pair() {
        let releases = parse_releases(RELEASES_JSON).unwrap();
        assert_eq!(releases.len(), 2);
        // The first release has both index.toml + .sig.
        let (toml, sig) = find_pair(&releases[0].assets, "index.toml").unwrap();
        assert!(toml.ends_with("/1") && sig.ends_with("/2"));
        // The second release has the toml but NO .sig ⇒ skipped (no half-signed fetch).
        assert!(find_pair(&releases[1].assets, "index.toml").is_none());
    }

    /// The zero-API URLs are SYNTHESIZED, not read out of an API response, so the exact
    /// strings are the contract with the publisher's tag convention
    /// (`atpkg-<program>-<build>`). A drift here silently sends every fetch to a 404 and
    /// falls back to the metered lane, which is the failure this path exists to remove.
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

    /// Anything that could splice a synthesized URL onto another host or path must decline
    /// and take the enumeration fallback — never emit a URL built from it.
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
    }

    #[test]
    fn find_pair_requires_both_and_exact_names() {
        let r = parse_releases(RELEASES_JSON).unwrap();
        // A name with no asset at all.
        assert!(find_pair(&r[0].assets, "pkg-ay-18.toml").is_none());
        // The .sig alone (no toml) would also fail — exact-name match both ways.
        let only_sig = vec![Asset {
            name: "x.toml.sig".into(),
            url: "u".into(),
        }];
        assert!(find_pair(&only_sig, "x.toml").is_none());
    }

    #[test]
    fn malformed_json_fails_closed() {
        assert!(parse_releases(b"not json").is_err());
        assert!(parse_releases(b"{}").is_err()); // object, not the expected array
    }

    /// A page of `n` empty-asset releases labeled `label-<i>`.
    fn page(n: usize, label: &str) -> Vec<Release> {
        (0..n)
            .map(|i| Release {
                tag_name: format!("{label}-{i}"),
                assets: vec![],
            })
            .collect()
    }

    // The page walk that keeps the index findable once app releases push it off the
    // first page: full pages keep walking (in order), a short page ends the listing,
    // an error on ANY page fails the WHOLE walk (a silently truncated catalog would
    // reintroduce the blindness), and the safety cap stops a runaway listing.
    #[test]
    fn paged_releases_walks_to_a_short_page_errors_whole_and_caps() {
        // Short first page: one request, done.
        let one = paged_releases(|p| {
            assert_eq!(p, 1, "a short first page ends the walk");
            Ok(page(3, "only"))
        })
        .unwrap();
        assert_eq!(one.len(), 3);

        // A full page keeps walking; the short second page ends it; order is preserved.
        let two = paged_releases(|p| match p {
            1 => Ok(page(RELEASES_PER_PAGE, "full")),
            2 => Ok(page(2, "tail")),
            _ => panic!("the walk must stop at the short page"),
        })
        .unwrap();
        assert_eq!(two.len(), RELEASES_PER_PAGE + 2);
        assert_eq!(two[0].tag_name, "full-0", "newest-first order preserved");
        assert_eq!(two[RELEASES_PER_PAGE].tag_name, "tail-0");

        // An error mid-walk fails the whole listing (retryable, never truncated).
        let err = paged_releases(|p| match p {
            1 => Ok(page(RELEASES_PER_PAGE, "full")),
            _ => Err("page 2 down".into()),
        })
        .unwrap_err();
        assert!(err.contains("page 2 down"));

        // Runaway listing: the cap bounds the walk.
        let mut calls = 0u64;
        let capped = paged_releases(|_| {
            calls += 1;
            Ok(page(RELEASES_PER_PAGE, "endless"))
        })
        .unwrap();
        assert_eq!(calls, MAX_RELEASE_PAGES);
        assert_eq!(capped.len(), RELEASES_PER_PAGE * MAX_RELEASE_PAGES as usize);
    }

    /// An asset row.
    fn asset(name: &str, url: String) -> Asset {
        Asset {
            name: name.into(),
            url,
        }
    }

    /// A release carrying the COMPLETE authorization unit: index + its machine signature
    /// AND the master-signed roster.
    fn quad(tag: &str) -> Release {
        Release {
            tag_name: tag.into(),
            assets: vec![
                asset("index.toml", format!("u:{tag}")),
                asset("index.toml.sig", format!("s:{tag}")),
                asset("aterm-machines.toml", format!("r:{tag}")),
                asset("aterm-machines.toml.sig", format!("rs:{tag}")),
            ],
        }
    }

    // The candidate scan: releases without the COMPLETE quad are skipped, listing order
    // (newest first) is preserved, and the downloads are capped BELOW the §14 cache's own
    // candidate cap so a deep index history can neither balloon the fetch nor make the
    // cache write refuse a full set.
    #[test]
    fn index_pair_urls_skips_incomplete_keeps_order_and_caps() {
        let bare = |tag: &str| Release {
            tag_name: tag.into(),
            assets: vec![],
        };
        let releases = vec![bare("v9"), quad("idx-6"), bare("v8"), quad("idx-5")];
        let urls = index_pair_urls(&releases);
        let seen: Vec<_> = urls
            .iter()
            .map(|u| (u.label, u.index, u.index_sig, u.roster, u.roster_sig))
            .collect();
        assert_eq!(
            seen,
            vec![
                ("idx-6", "u:idx-6", "s:idx-6", "r:idx-6", "rs:idx-6"),
                ("idx-5", "u:idx-5", "s:idx-5", "r:idx-5", "rs:idx-5"),
            ]
        );

        let deep: Vec<Release> = (0..INDEX_CANDIDATE_CAP + 5)
            .map(|i| quad(&format!("idx-{i}")))
            .collect();
        let capped = index_pair_urls(&deep);
        assert_eq!(capped.len(), INDEX_CANDIDATE_CAP, "downloads are capped");
        assert_eq!(
            capped[0].label, "idx-0",
            "the NEWEST carrying releases are kept"
        );
    }

    /// The identity moves when — and only when — one of the four assets behind the
    /// candidate moves. Both directions, because only the pair is load-bearing: stable
    /// under a repeat listing is what makes the cache a hit path, and unstable under a
    /// re-upload is what stops it being a downgrade oracle.
    #[test]
    fn candidate_identity_is_stable_and_moves_with_any_asset() {
        let base = vec![quad("idx-6")];
        let id = candidate_identity(&index_pair_urls(&base)[0]);
        assert_eq!(id.len(), 64, "a sha256 hex digest");
        let again = vec![quad("idx-6")];
        assert_eq!(
            id,
            candidate_identity(&index_pair_urls(&again)[0]),
            "an unchanged release listing fingerprints identically"
        );

        // Re-uploading ANY of the four mints a new asset id, i.e. a new URL.
        for i in 0..4 {
            let mut one = quad("idx-6");
            one.assets[i].url.push_str("-reuploaded");
            let moved = vec![one];
            assert_ne!(
                id,
                candidate_identity(&index_pair_urls(&moved)[0]),
                "asset {i} moved but the identity did not"
            );
        }
        // A retag onto the same four assets is a different publication state.
        let mut rel = quad("idx-6");
        rel.tag_name = "idx-6-rc2".into();
        let retagged = vec![rel];
        assert_ne!(id, candidate_identity(&index_pair_urls(&retagged)[0]));
    }

    /// NO ROSTER, NO CANDIDATE. A release with a perfectly good signed index but no
    /// master-signed roster beside it contributes NOTHING — the structural, free half of
    /// "the roster is not optional". Whoever serves the index can suppress it and stop
    /// atpkg installing; what they cannot do is get an older root honoured instead.
    ///
    /// Kills the mutation "make the roster lookup an `if let` that falls through": under
    /// it, every assertion below flips and a rosterless release becomes installable.
    #[test]
    fn a_release_without_the_roster_pair_yields_no_candidate() {
        let mut no_roster = quad("idx-1");
        no_roster.assets.retain(|a| !a.name.starts_with("aterm-"));
        assert!(
            index_pair_urls(std::slice::from_ref(&no_roster)).is_empty(),
            "an index with no roster beside it is not a candidate"
        );

        // Half a roster is no roster: the signature alone, or the document alone.
        let mut sig_only = quad("idx-2");
        sig_only.assets.retain(|a| a.name != "aterm-machines.toml");
        assert!(index_pair_urls(std::slice::from_ref(&sig_only)).is_empty());
        let mut doc_only = quad("idx-3");
        doc_only
            .assets
            .retain(|a| a.name != "aterm-machines.toml.sig");
        assert!(index_pair_urls(std::slice::from_ref(&doc_only)).is_empty());

        // NON-VACUITY: restore the pair and the very same release is a candidate again.
        assert_eq!(
            index_pair_urls(std::slice::from_ref(&quad("idx-1"))).len(),
            1
        );
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
        let f = GithubFetcher::new("alabsystems".into(), String::new()).with_overrides(overrides);
        // The overridden program fetches from the declared owner/repo…
        assert_eq!(f.slug_for("orc", "orc"), "alabsystems/orc-private");
        // …even if the index declares a differently-named repo for it…
        assert_eq!(f.slug_for("orc", "some-repo"), "alabsystems/orc-private");
        // …while every other program stays under the fetcher's account + index repo.
        assert_eq!(f.slug_for("ay", "ay"), "alabsystems/ay");
        // No overrides at all: the plain account/repo slug.
        let plain = GithubFetcher::new("alabsystems".into(), String::new());
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

    /// Two registry dirs, one flow: the chain unions index candidates from
    /// both legs, serves per-asset reads from the first leg that has the
    /// bytes, and only errors when BOTH legs fail (with both reasons kept).
    /// THE INODE-TRUNCATION REGRESSION. `DirFetcher`'s source for a bundled seed is a
    /// file inside the user's signed `aterm.app`, and it hardlinks that file into
    /// staging. Nothing sweeps staging, so a killed run leaves the link behind; the
    /// next attempt then found `hard_link` EEXIST and fell back to
    /// `fs::copy(src, dest)` where dest IS src — which on macOS opens the shared
    /// inode `O_TRUNC` and reports `Ok(0)`, zeroing a file inside a notarized bundle.
    /// That kills the asset forever (sha256 can never match) and invalidates the code
    /// signature, because the `.lproj` seal tolerates absence but not modification.
    ///
    /// The fix is one `remove_file(dest)`; this proves the registry survives a retry.
    /// THE ANONYMOUS DELIVERY BUDGET — the FIXED, per-run half of it.
    ///
    /// READ THIS BEFORE TRUSTING IT. This test measures candidate gathering ONLY, and
    /// for a long time its doc comment called that "the dominant term in a first
    /// bootstrap's GitHub API usage". That was wrong, and being both green and confidently
    /// worded is what made it harmful: the dominant term was the PER-PROGRAM discovery
    /// this test never counted — roughly four calls each, ten programs — so the sum it
    /// certified as fitting was never the sum that had to fit.
    ///
    /// Measured on the live channel 2026-08-19: a clean `install --default-set` spent the
    /// entire 60/hour budget and installed 7 of 10, taking HTTP 403 on the last three,
    /// while this test passed throughout.
    ///
    /// The per-program term is now ZERO — `pkg_manifest`/`download_for` derive the CDN
    /// URL from the signed channel pin instead of enumerating releases (see their notes),
    /// and release downloads are not API calls. The same measurement afterwards: 7
    /// requests total for all ten programs. So this bound is once again worth pinning —
    /// but as what it actually is, the fixed per-run cost, not as the whole budget.
    ///
    /// A REAL end-to-end guarantee cannot be asserted in-process; it needs an install
    /// against the live channel with no token. Keep this as the cheap regression on the
    /// fixed term, and do that measurement before shipping a delivery change.
    #[test]
    fn candidate_gathering_fits_the_anonymous_api_budget() {
        const ANONYMOUS_HOURLY_LIMIT: usize = 60;
        const DOWNLOADS_PER_CANDIDATE: usize = 4; // index, index.sig, roster, roster.sig
        let gathering = INDEX_CANDIDATE_CAP * DOWNLOADS_PER_CANDIDATE;
        let listing = MAX_RELEASE_PAGES as usize;
        assert!(
            gathering + listing <= ANONYMOUS_HOURLY_LIMIT / 2,
            "candidate gathering costs {gathering} requests (+{listing} for the listing),              which leaves too little of the {ANONYMOUS_HOURLY_LIMIT}/hour anonymous budget              for the package manifests and artifacts that actually deliver the toolchain"
        );
        // And still under the §14 cache's candidate ceiling, or a full set could never
        // be persisted for the offline fallback. A `const` block rather than a runtime
        // assertion: both operands are compile-time constants, so this is a ceiling on
        // the SOURCE, and it should fail the build that raises the cap rather than wait
        // for someone to run this test.
        const {
            assert!(
                INDEX_CANDIDATE_CAP <= 24,
                "INDEX_CANDIDATE_CAP exceeds the §14 cache's candidate ceiling: a full candidate set could no longer be persisted for the offline fallback"
            )
        };
    }

    #[test]
    fn a_stale_staging_hardlink_never_truncates_the_registry_file() {
        use crate::flow::Fetcher as _;
        let dir = std::env::temp_dir().join(format!("atpkg-hl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("reg")).unwrap();
        std::fs::create_dir_all(dir.join("staging")).unwrap();
        let payload = b"the sealed registry bytes that must survive";
        let src = dir.join("reg/ay-18.tar.zst");
        std::fs::write(&src, payload).unwrap();
        let dest = dir.join("staging/ay-18.tar.zst");

        let f = DirFetcher::new(dir.join("reg"));
        // First fetch: hardlinks into staging (the multi-minute window a kill lands in).
        f.download("r", "ay-18.tar.zst", &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), payload);
        // The process is killed here — the link survives, nothing sweeps it.
        // Second fetch (the resumable lane's retry) must NOT destroy the source.
        f.download("r", "ay-18.tar.zst", &dest).unwrap();
        assert_eq!(
            std::fs::read(&src).unwrap(),
            payload,
            "the registry file inside the app bundle must be byte-intact after a retry"
        );
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            payload,
            "and the staged copy must hold the real bytes, not an empty file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn chain_unions_candidates_and_falls_back_per_asset() {
        use crate::flow::Fetcher as _;

        let scratch = |label: &str| {
            let d =
                std::env::temp_dir().join(format!("atpkg-chain-{label}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            d
        };
        // Each leg publishes the COMPLETE authorization unit (index pair + roster
        // pair) — a `dir:` registry without a roster yields no candidate at all, which
        // is proved separately; here the union is what is under test, so both legs are
        // complete. The bytes are opaque: this test is about routing, and the trust
        // chain runs downstream in `select_index`.
        let unit = |d: &Path, tag: &[u8], sig: u8| {
            std::fs::write(d.join("index.toml"), tag).unwrap();
            std::fs::write(d.join("index.toml.sig"), [sig; 64]).unwrap();
            std::fs::write(d.join("aterm-machines.toml"), b"roster").unwrap();
            std::fs::write(d.join("aterm-machines.toml.sig"), [sig; 64]).unwrap();
        };
        // Leg A: an index unit + ay's pkg pair. Leg B: an index unit + ny's.
        let a = scratch("a");
        unit(&a, b"schema = 2 # a", 1);
        std::fs::write(a.join("pkg-ay-1.toml"), b"pkg a").unwrap();
        std::fs::write(a.join("pkg-ay-1.toml.sig"), [2u8; 64]).unwrap();
        let b = scratch("b");
        unit(&b, b"schema = 2 # b", 3);
        std::fs::write(b.join("pkg-ny-2.toml"), b"pkg b").unwrap();
        std::fs::write(b.join("pkg-ny-2.toml.sig"), [4u8; 64]).unwrap();

        let chain = ChainFetcher::new(
            Box::new(DirFetcher::new(a.clone())),
            Box::new(DirFetcher::new(b.clone())),
        );
        // Union: BOTH legs' index candidates reach the one selection, PRIMARY
        // FIRST — load-bearing, because `select_index` replaces only on a
        // STRICTLY greater index_build, so on a tie the first candidate wins
        // and the network index must beat an equal-build sealed seed.
        let candidates = chain.index_candidates().unwrap();
        assert_eq!(candidates.len(), 2);
        assert_eq!(
            candidates[0].index_bytes, b"schema = 2 # a",
            "primary first"
        );
        assert_eq!(candidates[1].index_bytes, b"schema = 2 # b");
        // Either leg serves what only it holds — a miss falls through by name.
        assert_eq!(chain.pkg_manifest("r", "ay", 1).unwrap().0, b"pkg a");
        assert_eq!(chain.pkg_manifest("r", "ny", 2).unwrap().0, b"pkg b");
        // An asset BOTH legs hold is served from the SECONDARY (local seed)
        // leg — load-bearing for the batteries-included cut: a networked first
        // run must not re-download bytes the app bundle already carries. (Real
        // asset names are build-qualified, so same-name means same signed
        // bytes; the identical sha256/tree_root gates run either way.)
        std::fs::write(a.join("pkg-both-3.toml"), b"from net").unwrap();
        std::fs::write(a.join("pkg-both-3.toml.sig"), [5u8; 64]).unwrap();
        std::fs::write(b.join("pkg-both-3.toml"), b"from seed").unwrap();
        std::fs::write(b.join("pkg-both-3.toml.sig"), [6u8; 64]).unwrap();
        assert_eq!(
            chain.pkg_manifest("r", "both", 3).unwrap().0,
            b"from seed",
            "local seed leg preferred per asset"
        );
        // Both legs missing ⇒ an error carrying both stories.
        let err = chain.pkg_manifest("r", "absent", 9).unwrap_err();
        assert!(err.contains("fallback:"), "both reasons kept: {err}");
        // The chain's SOURCE identity names both legs, never one alone…
        let id = chain.source_id();
        assert!(id.starts_with("chain:dir:") && id.contains('+'), "{id}");
        // …but its §14 CACHE identity is the primary (network) leg's alone, and the
        // cacheable set is the primary's own candidates — the secondary (seed) leg's
        // bytes never reach the last-good cache through the union.
        assert_eq!(
            chain.cache_source_id(),
            DirFetcher::new(a.clone()).source_id()
        );
        let cacheable = chain.cacheable_candidates(&candidates).unwrap();
        assert_eq!(cacheable.len(), 1);
        assert_eq!(cacheable[0].index_bytes, b"schema = 2 # a");

        // download_for routes through each leg's OWN download_for (so the
        // primary's per-program [packages.links] fetch override still
        // applies), and falls back with BOTH reasons when neither has it.
        let out = a.join("fetched.bin");
        chain
            .download_for("ay", "r", "pkg-ay-1.toml", &out)
            .expect("primary serves what it has");
        assert_eq!(std::fs::read(&out).unwrap(), b"pkg a");
        std::fs::remove_file(&out).unwrap();
        chain
            .download_for("ny", "r", "pkg-ny-2.toml", &out)
            .expect("fallback serves what the primary lacks");
        assert_eq!(std::fs::read(&out).unwrap(), b"pkg b");
        std::fs::remove_file(&out).unwrap();
        let err = chain
            .download_for("x", "r", "absent.bin", &out)
            .unwrap_err();
        assert!(err.contains("fallback:"), "{err}");

        // BOTH legs failing on the index is a real error naming both sources
        // (an empty dir yields no candidates, so use two unreadable paths).
        let dead = ChainFetcher::new(
            Box::new(DirFetcher::new(a.join("nope-1"))),
            Box::new(DirFetcher::new(a.join("nope-2"))),
        );
        // A missing dir yields NO candidates (not an Err) by DirFetcher's
        // contract, so the chain reports an empty union rather than failing —
        // pinned here so a future change to that contract is noticed.
        assert!(dead.index_candidates().unwrap().is_empty());

        let _ = std::fs::remove_dir_all(a);
        let _ = std::fs::remove_dir_all(b);
    }
}
