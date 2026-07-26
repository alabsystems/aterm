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

    /// List the recent releases (newest first) of `slug` (`owner/repo`).
    fn releases_at(&self, slug: &str) -> Result<Vec<Release>, String> {
        // Manual concat of the previous
        // `format!("https://api.github.com/repos/{}/releases?per_page=20", ..)`
        // — byte-identical: the `format!` expansion embeds `fmt::Arguments`
        // construction (with inlined `unsafe`) that the strict Trust gate cannot
        // lower and fails closed on.
        let mut url = String::from("https://api.github.com/repos/");
        url.push_str(slug);
        url.push_str("/releases?per_page=20");
        parse_releases(&aterm_update_core::api_get(&url, self.credential())?)
    }

    /// List a repo's recent releases under this fetcher's own account.
    fn releases(&self, repo: &str) -> Result<Vec<Release>, String> {
        let mut slug = self.owner.clone();
        slug.push('/');
        slug.push_str(repo);
        self.releases_at(&slug)
    }
}

impl crate::flow::Fetcher for GithubFetcher {
    fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
        let releases = self.releases(&crate::discovery::index_repo())?;
        let mut out = Vec::new();
        for r in &releases {
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
        Ok(out)
    }

    fn pkg_manifest(
        &self,
        repo: &str,
        program: &str,
        build: u64,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
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
        // The program's manifest rides its release repo — the `[packages.links]`
        // fetch override redirects it (with the same token) when declared.
        for r in &self.releases_at(&self.slug_for(program, repo))? {
            if let Some((toml_url, sig_url)) = find_pair(&r.assets, &name) {
                let toml =
                    aterm_update_core::download_bytes(toml_url, self.credential(), MANIFEST_CAP)?;
                let sig = aterm_update_core::download_bytes(sig_url, self.credential(), SIG_CAP)?;
                return Ok((toml, sig));
            }
        }
        let mut msg = String::from("no signed ");
        msg.push_str(&name);
        msg.push_str(" in ");
        msg.push_str(&self.slug_for(program, repo));
        msg.push_str(" releases");
        Err(msg)
    }

    fn download(&self, repo: &str, asset: &str, dest: &Path) -> Result<(), String> {
        for r in &self.releases(repo)? {
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
        // `[packages.links]` fetch override redirects it identically (same token).
        for r in &self.releases_at(&self.slug_for(program, repo))? {
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
        msg.push_str(&self.slug_for(program, repo));
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
}
