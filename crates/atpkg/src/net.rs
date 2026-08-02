// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The production GitHub-Releases [`Fetcher`](crate::flow::Fetcher) (§5/§9) — the network
//! impl the install flow ([`crate::flow`]) runs against a real repo.
//!
//! It lists `…/releases?per_page=20` and, for each release, locates the
//! `<name>` + `<name>.sig` asset pair, then downloads their bytes through
//! `aterm-update-core`'s authenticated `curl` plumbing (`api_get`/`download_bytes`/
//! `download_to` — the SAME proven layer the macOS updater uses). The asset-selection
//! logic ([`find_pair`]) and the releases-JSON shape ([`Release`]) are **pure and
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

    /// List the recent releases (newest first) of `slug` (`owner/repo`), memoized for the
    /// life of this fetcher (see the `releases` field for the cost model).
    ///
    /// ONLY successes are memoized — an `Err` must stay retryable, so a transient network
    /// failure is never frozen in. The lock is held ONLY around the map lookup/insert,
    /// never across the request, so an in-flight fetch can neither block another lane nor
    /// poison the mutex; a poisoned lock degrades to an uncached (correct) fetch rather
    /// than panicking.
    fn releases_at(&self, slug: &str) -> Result<std::sync::Arc<Vec<Release>>, String> {
        if let Ok(memo) = self.releases.lock()
            && let Some(hit) = memo.get(slug)
        {
            return Ok(std::sync::Arc::clone(hit));
        }
        // Manual concat of the previous
        // `format!("https://api.github.com/repos/{}/releases?per_page=20", ..)`
        // — byte-identical: the `format!` expansion embeds `fmt::Arguments`
        // construction (with inlined `unsafe`) that the strict Trust gate cannot
        // lower and fails closed on.
        let mut url = String::from("https://api.github.com/repos/");
        url.push_str(slug);
        url.push_str("/releases?per_page=20");
        let list = std::sync::Arc::new(parse_releases(&aterm_update_core::api_get(
            &url,
            self.credential(),
        )?)?);
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
        // `select_index` → `verify_index_with` → `parse_index` → floor → freshness.
        if let Ok(memo) = self.index.lock()
            && let Some(hit) = memo.as_ref()
        {
            return Ok((**hit).clone());
        }
        let releases = self.releases(&crate::discovery::index_repo())?;
        let mut out = Vec::new();
        for r in releases.iter() {
            if let Some((toml_url, sig_url)) = find_pair(&r.assets, "index.toml") {
                let index_bytes =
                    aterm_update_core::download_bytes(toml_url, self.credential(), MANIFEST_CAP)?;
                let sig = aterm_update_core::download_bytes(sig_url, self.credential(), SIG_CAP)?;
                out.push(Candidate {
                    label: r.tag_name.clone(),
                    index_bytes,
                    sig,
                });
            }
        }
        // Successes only — a partial fetch that errored above never reaches here, so a
        // transient failure stays retryable.
        let out = std::sync::Arc::new(out);
        if let Ok(mut memo) = self.index.lock() {
            *memo = Some(std::sync::Arc::clone(&out));
        }
        Ok((*out).clone())
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
                return aterm_update_core::download_to(
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
        let releases = self.releases_at(&slug)?;
        for r in releases.iter() {
            if let Some(a) = r.assets.iter().find(|a| a.name == asset) {
                return aterm_update_core::download_to(
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
}

impl DirFetcher {
    /// A fetcher reading assets from `dir` (canonicalized when possible for a stable
    /// `source_id`).
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir: std::fs::canonicalize(&dir).unwrap_or(dir),
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
        let toml = self.dir.join("index.toml");
        let sig = self.dir.join("index.toml.sig");
        match (
            crate::metadata_io::read_bounded_regular(
                &toml,
                usize::try_from(MANIFEST_CAP).unwrap_or(usize::MAX),
            ),
            crate::metadata_io::read_bounded_regular(
                &sig,
                usize::try_from(SIG_CAP).unwrap_or(usize::MAX),
            ),
        ) {
            (Ok(index_bytes), Ok(sig)) => Ok(vec![Candidate {
                label: "dir".into(),
                index_bytes,
                sig,
            }]),
            // Missing pair ⇒ no candidates ⇒ select_index None ⇒ NoIndex downstream.
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
/// wins, [`crate::select_index`]), and per-asset reads try `primary` then fall
/// back to `secondary`. This is how a network registry and the app-bundle seed
/// coexist without a second trust path: a fresher published index outranks the
/// sealed seed by the ordinary monotonic gate, an offline machine still
/// resolves the seed's index, and every byte from EITHER source passes the
/// identical verify-before-parse + floor + freshness + sha256 + `tree_root`
/// gates. The chain is composition, never authenticity: neither side is
/// trusted more than its signatures prove.
pub struct ChainFetcher {
    primary: Box<dyn crate::flow::Fetcher>,
    secondary: Box<dyn crate::flow::Fetcher>,
}

impl ChainFetcher {
    /// Chain `primary` (tried first for every asset) over `secondary`.
    #[must_use]
    pub fn new(
        primary: Box<dyn crate::flow::Fetcher>,
        secondary: Box<dyn crate::flow::Fetcher>,
    ) -> Self {
        Self { primary, secondary }
    }
}

impl crate::flow::Fetcher for ChainFetcher {
    fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
        // The union of both sides' candidates; a side that errors (offline
        // GitHub, say) contributes nothing rather than failing the other side.
        // BOTH failing is a real error — surface both reasons.
        match (
            self.primary.index_candidates(),
            self.secondary.index_candidates(),
        ) {
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
        self.primary
            .pkg_manifest(repo, program, build)
            .or_else(|e1| {
                self.secondary
                    .pkg_manifest(repo, program, build)
                    .map_err(|e2| chain_err(&e1, &e2))
            })
    }

    fn download(&self, repo: &str, asset: &str, dest: &Path) -> Result<(), String> {
        self.primary.download(repo, asset, dest).or_else(|e1| {
            self.secondary
                .download(repo, asset, dest)
                .map_err(|e2| chain_err(&e1, &e2))
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
        // per-program `[packages.links]` fetch override still applies.
        self.primary
            .download_for(program, repo, asset, dest)
            .or_else(|e1| {
                self.secondary
                    .download_for(program, repo, asset, dest)
                    .map_err(|e2| chain_err(&e1, &e2))
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
}

/// Both legs failed; keep both reasons (the second is usually the seed dir,
/// whose "no such file" alone would mislead when the real story is offline).
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
        // Leg A: an index pair + ay's pkg pair. Leg B: an index pair + ny's.
        let a = scratch("a");
        std::fs::write(a.join("index.toml"), b"schema = 1 # a").unwrap();
        std::fs::write(a.join("index.toml.sig"), [1u8; 64]).unwrap();
        std::fs::write(a.join("pkg-ay-1.toml"), b"pkg a").unwrap();
        std::fs::write(a.join("pkg-ay-1.toml.sig"), [2u8; 64]).unwrap();
        let b = scratch("b");
        std::fs::write(b.join("index.toml"), b"schema = 1 # b").unwrap();
        std::fs::write(b.join("index.toml.sig"), [3u8; 64]).unwrap();
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
            candidates[0].index_bytes, b"schema = 1 # a",
            "primary first"
        );
        assert_eq!(candidates[1].index_bytes, b"schema = 1 # b");
        // Primary serves what it has; the fallback serves what primary lacks.
        assert_eq!(chain.pkg_manifest("r", "ay", 1).unwrap().0, b"pkg a");
        assert_eq!(chain.pkg_manifest("r", "ny", 2).unwrap().0, b"pkg b");
        // Both legs missing ⇒ an error carrying both stories.
        let err = chain.pkg_manifest("r", "absent", 9).unwrap_err();
        assert!(err.contains("fallback:"), "both reasons kept: {err}");
        // The chain's cache identity names both legs, never one alone.
        let id = chain.source_id();
        assert!(id.starts_with("chain:dir:") && id.contains('+'), "{id}");

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
