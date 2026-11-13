// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Controller verified artifact cache, appcast resolution, and release-zip verification.

use super::inventory::Paths;
use crate::baseline::Baseline;
use crate::items::apps::{
    Assess, Gatekeeper, Signed, assess, bundle_info, ditto_unzip, is_sha256, one_app_in,
    verify_team,
};
use crate::items::aterm::{BUNDLE_ID, TEAM_ID, is_version};
use crate::model::Read;
use crate::port::{Scratch, System};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Public GitHub repository slug for official `aterm` releases.
pub const RELEASE_REPO: &str = "alabsystems/aterm";

/// Maximum artifact download size: [`System::download`]'s cap.
pub const MAX_ARTIFACT_BYTES: u64 = 4 << 30;

/// The largest appcast taken: a few dozen KiB in practice. [`System::download_bytes`] refuses a
/// longer body whole rather than cutting it, so a cut-off answer cannot parse as a whole one.
const MAX_APPCAST_BYTES: u64 = 512 * 1024;

/// The largest install.sh taken (install.sh's own bound on a manifest, 5 MB; the script
/// is about 230 KB).
const MAX_SCRIPT_BYTES: u64 = 5_000_000;

/// Serialize this process's cache lookup and publish decisions. Temporary allocations
/// are exclusive across calls, including other processes.
static FETCHING: Mutex<()> = Mutex::new(());

/// One artifact a baseline needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Needed {
    /// Human-readable label (`app.Emacs`, `aterm 0.105.0`).
    pub name: String,
    /// `https://` download URL.
    pub url: String,
    /// Expected lowercase hex SHA-256.
    pub sha256: String,
}

/// Returns the canonical macOS release archive name (`aterm-<version>-mac.zip`).
#[must_use]
pub fn zip_name(version: &str) -> String {
    format!("aterm-{version}-mac.zip")
}

/// Returns the tag-specific GitHub release download URL for `asset`.
#[must_use]
pub fn release_url(version: &str, asset: &str) -> String {
    format!("https://github.com/{RELEASE_REPO}/releases/download/v{version}/{asset}")
}

/// Returns the `aterm-appcast.toml` URL for `version` (or the latest release).
#[must_use]
pub fn appcast_url(version: Option<&str>) -> String {
    match version {
        Some(v) => release_url(v, "aterm-appcast.toml"),
        None => {
            format!("https://github.com/{RELEASE_REPO}/releases/latest/download/aterm-appcast.toml")
        }
    }
}

/// Returns the tag-pinned `tools/install.sh` URL for `version`.
#[must_use]
pub fn install_sh_url(version: &str) -> String {
    format!("https://raw.githubusercontent.com/{RELEASE_REPO}/v{version}/tools/install.sh")
}

/// Collects all unique artifacts required by `baseline`.
#[must_use]
pub fn needed(baseline: &Baseline) -> Vec<Needed> {
    let mut out: Vec<Needed> = Vec::new();
    let mut push = |name: String, url: String, sha256: &str| {
        let sha256 = sha256.to_ascii_lowercase();
        if !out.iter().any(|n| n.sha256 == sha256) {
            out.push(Needed { name, url, sha256 });
        }
    };
    for row in &baseline.apps {
        push(format!("app.{}", row.name), row.url.clone(), &row.sha256);
    }
    if let Some(pkg) = baseline.homebrew.as_ref().and_then(|h| h.pkg.as_ref()) {
        push("homebrew.pkg".into(), pkg.url.clone(), &pkg.sha256);
    }
    if let Some(at) = &baseline.aterm
        && let Some(v) = &at.version
    {
        if let Some(sha) = &at.archive_sha256 {
            push(format!("aterm {v}"), release_url(v, &zip_name(v)), sha);
        }
        if let Some(sha) = &at.install_sh_sha256 {
            push(format!("aterm {v} install.sh"), install_sh_url(v), sha);
        }
    }
    out
}

/// Download `url` into a temporary file `tmp` in the cache directory, bounded by `max`, through
/// [`System::download`]: a release asset by the shared release-asset reader (the download host,
/// then GitHub's API asset route), any other URL from its own host.
fn download(sys: &dyn System, url: &str, tmp: &Path, max: u64) -> Result<(), String> {
    if !url.starts_with("https://") {
        return Err(format!("{url} is not https://; refusing to download it"));
    }
    sys.download(url, tmp, max)
}

fn cache_scratch<'s>(
    sys: &'s dyn System,
    paths: &Paths,
    label: &str,
) -> Result<Scratch<'s>, String> {
    let cache = paths.root.join("cache");
    sys.create_dir_all(&cache, 0o700)
        .map_err(|e| format!("{}: {e}", cache.display()))?;
    Scratch::new_in(sys, &cache, label).map_err(|e| format!("cache scratch: {e}"))
}

/// Ensures `cache/<sha256>` holds a verified copy of `item`, downloading it ([`download`]) if
/// needed.
///
/// # Errors
/// One line: not https, download failed, or the bytes do not hash to the pin (and then
/// nothing is cached).
pub fn ensure(sys: &dyn System, paths: &Paths, item: &Needed) -> Result<PathBuf, String> {
    if !item.url.starts_with("https://") {
        return Err(format!(
            "{}: {} is not https://; refusing to download it",
            item.name, item.url
        ));
    }
    if !is_sha256(&item.sha256) {
        return Err(format!(
            "{}: sha256 {:?} is not 64 hexadecimal digits",
            item.name, item.sha256
        ));
    }
    let _one = FETCHING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dest = paths.cached(&item.sha256);
    match sys.stat(&dest) {
        Ok(_) => {
            if sys
                .sha256(&dest)
                .is_ok_and(|got| got.eq_ignore_ascii_case(&item.sha256))
            {
                return Ok(dest);
            }
            sys.remove(&dest).map_err(|e| {
                format!("{}: removing a corrupt {}: {e}", item.name, dest.display())
            })?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{}: {}: {e}", item.name, dest.display())),
    }
    let scratch = cache_scratch(sys, paths, "download")?;
    let tmp = scratch.path().join("payload");
    download(sys, &item.url, &tmp, MAX_ARTIFACT_BYTES)
        .map_err(|e| format!("{}: {e}", item.name))?;
    let got = match sys.sha256(&tmp) {
        Ok(got) => got,
        Err(e) => {
            return Err(format!("{}: {}: {e}", item.name, tmp.display()));
        }
    };
    if !got.eq_ignore_ascii_case(&item.sha256) {
        return Err(format!(
            "{}: {} hashes to {got}, the baseline pins {}; nothing was cached",
            item.name, item.url, item.sha256
        ));
    }
    sys.rename(&tmp, &dest)
        .map_err(|e| format!("{}: {}: {e}", item.name, dest.display()))?;
    Ok(dest)
}

/// What a release's `aterm-appcast.toml` says about its mac zip. The appcast carries many
/// more fields; only these decide an install.
#[derive(Debug, Deserialize)]
struct Appcast {
    version: String,
    #[serde(default)]
    zip: Option<String>,
    #[serde(default)]
    zip_sha256: Option<String>,
    #[serde(default)]
    team_id: Option<String>,
}

/// Admitted release metadata from [`parse_appcast`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// `MAJOR.MINOR.PATCH`.
    pub version: String,
    /// Lowercase hex SHA-256 of `aterm-<version>-mac.zip`.
    pub zip_sha256: String,
}

/// Parses and validates `aterm-appcast.toml` against optional pinned `want` version.
///
/// # Errors
/// Returns an error naming the first invalid or mismatched field.
pub fn parse_appcast(text: &str, want: Option<&str>) -> Result<Release, String> {
    let cast: Appcast =
        aterm_toml::from_str(text).map_err(|e| format!("aterm-appcast.toml: {e}"))?;
    let v = &cast.version;
    if !is_version(v) {
        return Err(format!(
            "aterm-appcast.toml: version {v:?} is not MAJOR.MINOR.PATCH"
        ));
    }
    if let Some(want) = want
        && v != want
    {
        return Err(format!(
            "aterm-appcast.toml of tag v{want} says version {v}; refusing the mismatch"
        ));
    }
    if cast.zip.as_deref() != Some(zip_name(v).as_str()) {
        return Err(format!(
            "aterm-appcast.toml of {v} names zip {:?}, not {}; a release without the lean zip cannot be installed this way",
            cast.zip.unwrap_or_default(),
            zip_name(v)
        ));
    }
    let sha = cast.zip_sha256.unwrap_or_default();
    if !is_sha256(&sha) {
        return Err(format!(
            "aterm-appcast.toml of {v}: zip_sha256 {sha:?} is not 64 hexadecimal digits"
        ));
    }
    match cast.team_id.as_deref() {
        Some(TEAM_ID) => {}
        Some(other) => {
            return Err(format!(
                "aterm-appcast.toml of {v} names signing team {other:?}, but official releases are signed by team {TEAM_ID}; refusing"
            ));
        }
        None => {
            return Err(format!(
                "aterm-appcast.toml of {v} omits team_id, but official releases are signed by team {TEAM_ID}; refusing"
            ));
        }
    }
    Ok(Release {
        version: v.clone(),
        zip_sha256: sha.to_ascii_lowercase(),
    })
}

/// `url`'s body as text, at most `max` bytes, read as [`download`] reads.
fn fetch_text(sys: &dyn System, url: &str, max: u64) -> Result<String, String> {
    if !url.starts_with("https://") {
        return Err(format!("{url} is not https://; refusing to download it"));
    }
    let bytes = sys.download_bytes(url, max)?;
    String::from_utf8(bytes).map_err(|e| format!("{url} is not UTF-8 text: {e}"))
}

/// The whole-file guard's last line in `text`: `main "$@"; }` in an install.sh whose
/// whole-file group a line of `{` opens at its top (the brace closes it), else the bare
/// `main "$@"` of the releases before that group, which a cut just after `main` ran.
/// The bare line begins the new one, so taking either line from any file passed a
/// new install.sh cut four bytes short.
fn guard_line(text: &str) -> &'static str {
    if text.lines().any(|l| l == "{") {
        "main \"$@\"; }"
    } else {
        "main \"$@\""
    }
}

/// Fetch release `version`'s install.sh into the cache and return its SHA-256. It must
/// look like the official script: the whole-file guard (its `guard_line`, and the newline
/// that ends it, as the file's last bytes, so a cut-off download is refused) and the
/// official Team ID pin. Every release with a guard ends so; a download cut one byte
/// short, the guard line's newline lost, passed a check of the last line alone, and was
/// pinned under a hash no whole download has.
fn fetch_install_sh(sys: &dyn System, paths: &Paths, version: &str) -> Result<String, String> {
    let _one = FETCHING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let url = install_sh_url(version);
    let scratch = cache_scratch(sys, paths, "install-script")?;
    let tmp = scratch.path().join("payload");
    download(sys, &url, &tmp, MAX_SCRIPT_BYTES)?;
    let checked = sys
        .read_file(&tmp)
        .map_err(|e| format!("{}: {e}", tmp.display()))
        .and_then(|bytes| {
            let text = String::from_utf8_lossy(&bytes);
            let guard = guard_line(&text);
            if !text.ends_with(&format!("\n{guard}\n")) {
                return Err(format!(
                    "{url} does not end with install.sh's `{guard}` line"
                ));
            }
            if !text.contains(&format!("OFFICIAL_TEAM_ID=\"{TEAM_ID}\"")) {
                return Err(format!("{url} does not pin team {TEAM_ID}"));
            }
            sys.sha256(&tmp)
                .map_err(|e| format!("{}: {e}", tmp.display()))
        });
    let sha = checked?;
    sys.rename(&tmp, &paths.cached(&sha))
        .map_err(|e| format!("{}: {e}", paths.cached(&sha).display()))?;
    Ok(sha)
}

/// Verifies a cached release zip's bundle layout, Developer ID signature, Gatekeeper, and version.
///
/// # Errors
/// Returns an error naming the check that failed.
pub fn verify_release_zip(
    sys: &dyn System,
    paths: &Paths,
    zip: &Path,
    version: &str,
) -> Result<(), String> {
    let mut scratch = cache_scratch(sys, paths, "verify")?;
    let checked = check_release_app(sys, zip, scratch.path(), version);
    let removed = scratch
        .close()
        .map_err(|e| format!("verification scratch cleanup: {e}"));
    checked?;
    removed
}

fn check_release_app(
    sys: &dyn System,
    zip: &Path,
    dir: &Path,
    version: &str,
) -> Result<(), String> {
    ditto_unzip(sys, zip, dir)?;
    let found = one_app_in(sys, dir)?;
    if found != "aterm.app" {
        return Err(format!("the release zip holds {found}, not aterm.app"));
    }
    let app = dir.join(found);
    match verify_team(sys, &app, TEAM_ID) {
        Signed::Team => {}
        Signed::Other(why) => return Err(format!("the release's aterm.app: {why}")),
        Signed::Unusable(why) => return Err(why),
    }
    match assess(sys, &app, Assess::Exec) {
        Gatekeeper::Accepted(_) | Gatekeeper::Overridden { .. } => {}
        Gatekeeper::Rejected(why) => {
            return Err(format!(
                "Gatekeeper rejected the release's aterm.app ({why})"
            ));
        }
        Gatekeeper::Unusable(why) => return Err(why),
    }
    for (key, want) in [
        ("CFBundleIdentifier", BUNDLE_ID),
        ("CFBundleShortVersionString", version),
    ] {
        match bundle_info(sys, &app, key) {
            Read::Value(have) if have == want => {}
            Read::Value(have) => {
                return Err(format!(
                    "the release's aterm.app has {key} {have}, not {want}"
                ));
            }
            Read::Absent => return Err(format!("the release's aterm.app has no {key}")),
            Read::Unusable(why) => return Err(why),
        }
    }
    Ok(())
}

/// Set the string `key` of `table` to `value`, keeping the spacing and comment an existing
/// value was written with.
fn set_string(table: &mut dyn aterm_toml::edit::TableLike, key: &str, value: &str) {
    let mut new = aterm_toml::edit::Value::from(value);
    if let Some(old) = table.get(key).and_then(aterm_toml::edit::Item::as_value) {
        *new.decor_mut() = old.decor().clone();
    }
    table.insert(key, aterm_toml::edit::Item::Value(new));
}

/// Resolved `[aterm]` release version and asset SHA-256 pins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtermPins {
    /// Release version.
    pub version: String,
    /// SHA-256 of the macOS release zip from the appcast.
    pub archive_sha256: String,
    /// SHA-256 of the release tag's `install.sh`.
    pub install_sh_sha256: String,
}

/// Per-command memo of resolved `[aterm]` pins keyed by requested baseline fields.
#[derive(Debug, Default)]
pub struct AtermMemo {
    resolved: Vec<(AtermAsked, AtermPins)>,
}

/// `[aterm]`'s three pins as a baseline writes them.
type AtermAsked = (Option<String>, Option<String>, Option<String>);

/// Resolves and verifies `[aterm]` pins for `baseline_text`, rewriting the TOML in place.
///
/// # Errors
/// Returns an error if appcast/asset retrieval, verification, or TOML rewriting fails.
pub fn resolve_aterm(
    sys: &dyn System,
    paths: &Paths,
    baseline_text: &str,
) -> Result<String, String> {
    resolve_aterm_once(sys, paths, baseline_text, &mut AtermMemo::default())
}

/// Resolves `[aterm]` pins like [`resolve_aterm`], reusing `memo` across hosts.
///
/// # Errors
/// Returns an error if resolution or TOML rewriting fails.
pub fn resolve_aterm_once(
    sys: &dyn System,
    paths: &Paths,
    baseline_text: &str,
    memo: &mut AtermMemo,
) -> Result<String, String> {
    let baseline = crate::baseline::parse(baseline_text)?;
    let Some(at) = &baseline.aterm else {
        return Ok(baseline_text.to_owned());
    };
    if at.version.is_some() && at.archive_sha256.is_some() && at.install_sh_sha256.is_some() {
        return Ok(baseline_text.to_owned());
    }
    let asked: AtermAsked = (
        at.version.clone(),
        at.archive_sha256.clone(),
        at.install_sh_sha256.clone(),
    );
    let pins = match memo.resolved.iter().find(|(a, _)| *a == asked) {
        Some((_, pins)) => pins.clone(),
        None => {
            let pins = resolve_pins(sys, paths, at)?;
            memo.resolved.push((asked, pins.clone()));
            pins
        }
    };
    write_pins(baseline_text, &pins)
}

/// Resolve `at` against the release it names (the newest when it names none).
fn resolve_pins(
    sys: &dyn System,
    paths: &Paths,
    at: &crate::items::aterm::Table,
) -> Result<AtermPins, String> {
    if let Some(v) = &at.version
        && !is_version(v)
    {
        return Err(format!("[aterm] version {v:?} is not MAJOR.MINOR.PATCH"));
    }
    let manifest = fetch_text(sys, &appcast_url(at.version.as_deref()), MAX_APPCAST_BYTES)?;
    let release = parse_appcast(&manifest, at.version.as_deref())?;
    let v = &release.version;
    if let Some(pinned) = &at.archive_sha256
        && !pinned.eq_ignore_ascii_case(&release.zip_sha256)
    {
        return Err(format!(
            "[aterm] archive_sha256 pins {pinned}, but release {v}'s appcast says {}; refusing",
            release.zip_sha256
        ));
    }
    let archive = Needed {
        name: format!("aterm {v}"),
        url: release_url(v, &zip_name(v)),
        sha256: release.zip_sha256.clone(),
    };
    let zip = ensure(sys, paths, &archive)?;
    verify_release_zip(sys, paths, &zip, v)?;
    let script = match &at.install_sh_sha256 {
        Some(sha) => sha.clone(),
        None => fetch_install_sh(sys, paths, v)?,
    };
    Ok(AtermPins {
        version: v.clone(),
        archive_sha256: release.zip_sha256,
        install_sh_sha256: script,
    })
}

/// `baseline_text` with `[aterm]`'s three pins set to `pins`, every comment kept.
fn write_pins(baseline_text: &str, pins: &AtermPins) -> Result<String, String> {
    let mut doc: aterm_toml::edit::DocumentMut = baseline_text
        .parse()
        .map_err(|e| format!("baseline: {e}"))?;
    let table = doc
        .as_table_mut()
        .get_mut("aterm")
        .and_then(aterm_toml::edit::Item::as_table_like_mut)
        .ok_or("baseline: [aterm] is not a table")?;
    set_string(table, "version", &pins.version);
    set_string(table, "archive_sha256", &pins.archive_sha256);
    set_string(table, "install_sh_sha256", &pins.install_sh_sha256);
    let text = doc.to_string();
    let back = crate::baseline::parse(&text)?;
    let back = back.aterm.unwrap_or_default();
    if back.version.as_deref() != Some(pins.version.as_str())
        || back.archive_sha256.as_deref() != Some(pins.archive_sha256.as_str())
        || back.install_sh_sha256.as_deref() != Some(pins.install_sh_sha256.as_str())
    {
        return Err("baseline: the rewritten [aterm] does not read back as resolved".into());
    }
    Ok(text)
}

#[cfg(test)]
#[path = "artifacts_tests.rs"]
mod tests;
