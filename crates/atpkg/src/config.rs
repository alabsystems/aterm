// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The `[packages]` table of the SAME `aterm.toml` the GUI reads (§11) — atpkg's
//! own config reader.
//!
//! The GUI parses the whole file into its `Config` (including a mirror
//! `PackagesConfig` it uses only for the background-loop gate); atpkg reads just
//! the `[packages]` table out of the identical file, so there is exactly ONE
//! user-facing config surface. **Env always wins over config** at every
//! consumption site: `ATPKG_ACCOUNT` beats `account`
//! ([`crate::discovery::resolve_account`] precedence), `ATPKG_REGISTRY` /
//! `ATPKG_INDEX_REPO` / `ATPKG_DISABLE` have no config counterpart and are read
//! directly from the environment. Nothing here is a trust input: the account is
//! slug-validated downstream, `include`/`exclude` are narrowing-only over the
//! SIGNED index ([`crate::manifest::Index::installable`]), and a
//! `[packages.links]` entry can only redirect WHERE bytes are fetched from or
//! suppress registry management — never what verifies (§5/§8: the host is not an
//! authenticity input).
//!
//! A missing file, a file without a `[packages]` table, or a malformed file all
//! yield the defaults (the malformed case loudly — mirroring the GUI's
//! `load_config` posture, and safe here because every default is the inert/
//! narrowest behavior: no auto-install, no links, compiled account).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Maximum `aterm.toml` size consumed by the co-located package verbs.
/// Matches the native config service's 512-KiB admission budget.
pub const MAX_PACKAGES_CONFIG_BYTES: usize = 512 * 1024;

/// The `[packages]` table. All-Option (an absent key and a default-valued key are
/// indistinguishable); defaults live ONLY in the resolver methods below.
#[derive(Debug, Default, Clone, serde::Deserialize)]
#[serde(default)]
pub struct PackagesConfig {
    /// Master for the background tools loop (the GUI's `spawn_pkg_update_check`).
    /// Default TRUE (today's behavior). Read by the GUI, not by atpkg's own
    /// verbs — an explicit `atpkg update` always works regardless.
    pub enabled: Option<bool>,
    /// Run `atpkg update` on the background cadence. Default TRUE (today's
    /// behavior). Read by the GUI loop gate.
    pub auto_update: Option<bool>,
    /// ALSO install missing index default-set members on the `update` pass
    /// (§11 batteries-included bootstrap). Default FALSE — multi-GB toolchains
    /// need explicit consent; the Settings switch is the consent click.
    pub auto_install: Option<bool>,
    /// Index owner override (e.g. `"alabsystems"`). Default = the compiled
    /// owner; `ATPKG_ACCOUNT` env beats this ([`crate::discovery::resolve_account`]).
    /// Slug-validated downstream — a malformed value can never redirect fetches.
    pub account: Option<String>,
    /// The channel whose pin set drives install/update. Default `"stable"`.
    pub channel: Option<String>,
    /// Narrowing-only include filter over the index default set (§5): an entry
    /// the signed index does not name adds NOTHING. Default = every named program.
    pub include: Option<Vec<String>>,
    /// Narrowing-only exclude filter (subtracts after `include`).
    pub exclude: Option<Vec<String>>,
    /// `[packages.links]`: `name = "/path/to/checkout"` (or `~/…`) declares a
    /// managed dev-link ([`crate::linkmode`] — registry management skipped);
    /// `name = "owner/repo"` declares a private-repo FETCH override for that
    /// program's release assets (signature verification UNCHANGED). Anything
    /// else is refused loudly ([`LinkTarget::Invalid`]).
    pub links: BTreeMap<String, String>,
}

impl PackagesConfig {
    /// The channel to resolve pins from — `[packages].channel`, default `stable`.
    /// A blank value is treated as unset (never silently select a "" channel).
    #[must_use]
    pub fn channel(&self) -> &str {
        match self.channel.as_deref().map(str::trim) {
            Some(c) if !c.is_empty() => c,
            _ => "stable",
        }
    }

    /// The `[packages].account` override for [`crate::discovery::resolve_account`]
    /// (`None` ⇒ compiled default; `ATPKG_ACCOUNT` env still beats this).
    #[must_use]
    pub fn account(&self) -> Option<&str> {
        self.account
            .as_deref()
            .map(str::trim)
            .filter(|a| !a.is_empty())
    }

    /// Whether the `update` pass ALSO bootstraps missing default-set members.
    /// Default FALSE (explicit consent — §11).
    #[must_use]
    pub fn auto_install(&self) -> bool {
        self.auto_install.unwrap_or(false)
    }

    /// The narrowing-only include filter (empty ⇒ the whole index default set).
    #[must_use]
    pub fn include(&self) -> &[String] {
        self.include.as_deref().unwrap_or(&[])
    }

    /// The narrowing-only exclude filter.
    #[must_use]
    pub fn exclude(&self) -> &[String] {
        self.exclude.as_deref().unwrap_or(&[])
    }
}

/// What one `[packages.links]` value means. Classified fail-closed: only the two
/// sanctioned shapes act; everything else is [`LinkTarget::Invalid`] and ignored
/// loudly at the consumption site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// A local checkout path (absolute, or `~`-anchored) → managed dev-link.
    Checkout(PathBuf),
    /// A slug-validated `owner/repo` → private-repo fetch override for the
    /// program's release assets (the SIGNED index must still name the program —
    /// reachability §5 is untouched, so this can never install from a bare slug).
    Repo(String),
    /// Neither shape (relative path, `~user`, URL-metacharacter slug, …).
    Invalid,
}

/// Classify one `[packages.links]` value. `home` backs `~` expansion (injected
/// for testability; `None` means a `~` value cannot resolve and is Invalid).
/// A repo value must be exactly `owner/repo` with BOTH parts passing the shared
/// URL-safety allowlist ([`aterm_update_core::is_valid_slug`]) so a malformed
/// value can never redirect a fetch off the GitHub API.
#[must_use]
pub fn classify_link(value: &str, home: Option<&Path>) -> LinkTarget {
    let v = value.trim();
    if let Some(rest) = v.strip_prefix('~') {
        // `~` or `~/…` only — `~user` expansion is not supported (Invalid).
        let Some(home) = home else {
            return LinkTarget::Invalid;
        };
        return match rest.strip_prefix('/') {
            Some(tail) => LinkTarget::Checkout(home.join(tail)),
            None if rest.is_empty() => LinkTarget::Checkout(home.to_path_buf()),
            None => LinkTarget::Invalid,
        };
    }
    if Path::new(v).is_absolute() {
        return LinkTarget::Checkout(PathBuf::from(v));
    }
    if let Some((owner, repo)) = v.split_once('/')
        && aterm_update_core::is_valid_slug(owner)
        && aterm_update_core::is_valid_slug(repo)
    {
        return LinkTarget::Repo(format!("{owner}/{repo}"));
    }
    LinkTarget::Invalid
}

/// The program → `owner/repo` FETCH-override map derived from `[packages.links]`
/// (the [`LinkTarget::Repo`] entries only), consumed by
/// [`crate::GithubFetcher::with_overrides`]. Checkout/Invalid entries are the
/// link-reconciliation path's business, not the fetcher's.
#[must_use]
pub fn repo_overrides(cfg: &PackagesConfig) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (program, value) in &cfg.links {
        // `home: None` is fine here: a `~…` value classifies as Checkout/Invalid
        // either way, never as Repo, and only Repo entries matter for fetches.
        if let LinkTarget::Repo(slug) = classify_link(value, None) {
            out.insert(program.clone(), slug);
        }
    }
    out
}

/// The path of the user config file — EXACTLY the GUI's resolution
/// (`app_config.rs::config_path`, mirrored so the two can never read different
/// files): `$XDG_CONFIG_HOME/aterm/aterm.toml`, else (Windows)
/// `%APPDATA%\aterm\aterm.toml`, else `$HOME/.config/aterm/aterm.toml`.
#[must_use]
pub fn config_path() -> Option<PathBuf> {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|x| !x.is_empty()) {
        return Some(PathBuf::from(x).join("aterm").join("aterm.toml"));
    }
    #[cfg(windows)]
    if let Some(appdata) = std::env::var_os("APPDATA").filter(|a| !a.is_empty()) {
        return Some(PathBuf::from(appdata).join("aterm").join("aterm.toml"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/aterm/aterm.toml"))
}

/// Deserialization wrapper: ONLY the `[packages]` table is read out of the whole
/// `aterm.toml`; every other table/key is ignored (the GUI owns that schema).
#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct RootConfig {
    packages: Option<PackagesConfig>,
}

/// Parse the `[packages]` table out of full `aterm.toml` text. A file without the
/// table ⇒ defaults; a malformed file ⇒ loud defaults (never aborts a verb — every
/// default is the inert/narrowest behavior, so a broken config can only DISABLE
/// bootstrap/links, never widen anything).
#[must_use]
pub fn parse_packages(text: &str) -> PackagesConfig {
    match toml::from_str::<RootConfig>(text) {
        Ok(root) => root.packages.unwrap_or_default(),
        Err(e) => {
            eprintln!("atpkg: ignoring malformed aterm.toml [packages] config: {e}");
            PackagesConfig::default()
        }
    }
}

/// Load the `[packages]` table from the real config file (missing/unreadable ⇒
/// defaults, malformed ⇒ loud defaults via [`parse_packages`]).
#[must_use]
pub fn load() -> PackagesConfig {
    let Some(path) = config_path() else {
        return PackagesConfig::default();
    };
    load_from_path(&path)
}

fn load_from_path(path: &Path) -> PackagesConfig {
    // The config file itself may legitimately be a dotfile-manager symlink.
    // Resolve only that bounded final-link chain, then admit/read the selected
    // regular target through one non-blocking, bounded handle.
    let Ok(text) = crate::metadata_io::read_bounded_regular_utf8_follow_final_links(
        path,
        MAX_PACKAGES_CONFIG_BYTES,
    ) else {
        return PackagesConfig::default(); // not present / unreadable → defaults
    };
    parse_packages(&text)
}

/// The process-wide `[packages]` config, read ONCE per invocation (atpkg is a
/// short-lived CLI; there is no reload seam to keep coherent). Env always wins
/// over these values at each consumption site — `ATPKG_ACCOUNT` /
/// `ATPKG_REGISTRY` / `ATPKG_INDEX_REPO` / `ATPKG_DISABLE` are read directly
/// from the environment, never through here.
#[must_use]
pub fn cached() -> &'static PackagesConfig {
    static CFG: std::sync::OnceLock<PackagesConfig> = std::sync::OnceLock::new();
    CFG.get_or_init(load)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_table_yields_inert_defaults() {
        let cfg = parse_packages("font_px = 12.0\n[matrix_rain]\nenabled = true\n");
        assert_eq!(cfg.channel(), "stable");
        assert_eq!(cfg.account(), None);
        assert!(
            !cfg.auto_install(),
            "auto_install must default OFF (consent)"
        );
        assert!(cfg.include().is_empty());
        assert!(cfg.exclude().is_empty());
        assert!(cfg.links.is_empty());
        // The GUI-facing loop flags default to today's behavior (on).
        assert_eq!(cfg.enabled, None);
        assert_eq!(cfg.auto_update, None);
    }

    #[test]
    fn full_table_parses_every_key() {
        let cfg = parse_packages(
            "[packages]\nenabled = true\nauto_update = false\nauto_install = true\n\
             account = \"alabsystems\"\nchannel = \"nightly\"\n\
             include = [\"ay\", \"trust\"]\nexclude = [\"trust\"]\n\
             [packages.links]\nay = \"~/ay\"\norc = \"alabsystems/orc\"\n",
        );
        assert_eq!(cfg.enabled, Some(true));
        assert_eq!(cfg.auto_update, Some(false));
        assert!(cfg.auto_install());
        assert_eq!(cfg.account(), Some("alabsystems"));
        assert_eq!(cfg.channel(), "nightly");
        assert_eq!(cfg.include(), ["ay".to_string(), "trust".to_string()]);
        assert_eq!(cfg.exclude(), ["trust".to_string()]);
        assert_eq!(cfg.links.get("ay").map(String::as_str), Some("~/ay"));
        assert_eq!(
            cfg.links.get("orc").map(String::as_str),
            Some("alabsystems/orc")
        );
    }

    // Malformed config ⇒ loud DEFAULTS (inert/narrowest), never a panic/abort.
    #[test]
    fn malformed_config_falls_back_to_defaults() {
        let cfg = parse_packages("[packages\nnot toml");
        assert_eq!(cfg.channel(), "stable");
        assert!(!cfg.auto_install());
        assert!(cfg.links.is_empty());
        // A [packages] table of the WRONG SHAPE (scalar) also fails to defaults.
        let cfg = parse_packages("packages = 3\n");
        assert_eq!(cfg.channel(), "stable");
    }

    // Channel resolver: default, explicit, and blank-is-unset.
    #[test]
    fn channel_resolves_with_default_and_blank_guard() {
        assert_eq!(parse_packages("").channel(), "stable");
        assert_eq!(
            parse_packages("[packages]\nchannel = \"nightly\"\n").channel(),
            "nightly"
        );
        assert_eq!(
            parse_packages("[packages]\nchannel = \"  \"\n").channel(),
            "stable",
            "a blank channel is treated as unset, never a \"\" channel lookup"
        );
    }

    // Account precedence env > config > default, via the pure discovery split
    // (no process-env mutation).
    #[test]
    fn account_precedence_env_beats_config_beats_default() {
        let cfg = parse_packages("[packages]\naccount = \"alabsystems\"\n");
        // config beats the compiled default…
        assert_eq!(
            crate::resolve_account_with(None, cfg.account()).owner,
            "alabsystems"
        );
        // …env beats config…
        assert_eq!(
            crate::resolve_account_with(Some("env-org"), cfg.account()).owner,
            "env-org"
        );
        // …and an invalid config account can never redirect (falls to default).
        // PUBLISH_OWNER, not DEFAULT_OWNER: the package index is account-bound and
        // deliberately does NOT follow the updater's public-mirror channel.
        let bad = parse_packages("[packages]\naccount = \"evil.com/x\"\n");
        assert_eq!(
            crate::resolve_account_with(None, bad.account()).owner,
            aterm_update_core::PUBLISH_OWNER
        );
        // Blank config account is treated as unset.
        let blank = parse_packages("[packages]\naccount = \"\"\n");
        assert_eq!(blank.account(), None);
    }

    #[cfg(unix)]
    #[test]
    fn config_fifo_returns_defaults_and_symlinked_config_remains_supported() {
        use std::os::unix::ffi::OsStrExt as _;

        let root =
            std::env::temp_dir().join(format!("atpkg-config-admission-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let fifo = root.join("fifo.toml");
        let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: `fifo_c` is a live NUL-terminated path in our private fixture.
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        assert_eq!(
            load_from_path(&fifo).channel(),
            "stable",
            "a writerless config FIFO must return the finite default immediately"
        );

        let target = root.join("target.toml");
        let logical = root.join("aterm.toml");
        std::fs::write(&target, "[packages]\nchannel = \"nightly\"\n").unwrap();
        std::os::unix::fs::symlink(&target, &logical).unwrap();
        assert_eq!(load_from_path(&logical).channel(), "nightly");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn oversized_sparse_config_returns_bounded_defaults() {
        let root =
            std::env::temp_dir().join(format!("atpkg-config-oversized-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("aterm.toml");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len((MAX_PACKAGES_CONFIG_BYTES + 1) as u64)
            .unwrap();
        assert_eq!(load_from_path(&path).channel(), "stable");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn classify_link_covers_the_three_shapes() {
        let home = Path::new("/home/u");
        // Absolute path → checkout.
        assert_eq!(
            classify_link("/src/ay", Some(home)),
            LinkTarget::Checkout(PathBuf::from("/src/ay"))
        );
        // ~ expansion (against the injected home).
        assert_eq!(
            classify_link("~/ay", Some(home)),
            LinkTarget::Checkout(PathBuf::from("/home/u").join("ay"))
        );
        assert_eq!(
            classify_link("~", Some(home)),
            LinkTarget::Checkout(PathBuf::from("/home/u"))
        );
        // ~ with no resolvable home fails CLOSED, and ~user is unsupported.
        assert_eq!(classify_link("~/ay", None), LinkTarget::Invalid);
        assert_eq!(classify_link("~bob/ay", Some(home)), LinkTarget::Invalid);
        // owner/repo → validated fetch override.
        assert_eq!(
            classify_link("alabsystems/orc", Some(home)),
            LinkTarget::Repo("alabsystems/orc".into())
        );
        // Everything else is Invalid: relative paths, deep slugs, URL metacharacters.
        assert_eq!(classify_link("src/ay/x", Some(home)), LinkTarget::Invalid);
        assert_eq!(
            classify_link("evil.com?x/y", Some(home)),
            LinkTarget::Invalid
        );
        assert_eq!(classify_link("a b/repo", Some(home)), LinkTarget::Invalid);
        assert_eq!(classify_link("", Some(home)), LinkTarget::Invalid);
    }

    #[test]
    fn repo_overrides_extracts_only_validated_slug_entries() {
        let cfg = parse_packages(
            "[packages.links]\nay = \"/src/ay\"\norc = \"alabsystems/orc\"\n\
             bad = \"evil host/x\"\n",
        );
        let map = repo_overrides(&cfg);
        assert_eq!(map.len(), 1, "only the validated owner/repo entry survives");
        assert_eq!(map.get("orc").map(String::as_str), Some("alabsystems/orc"));
    }
}
