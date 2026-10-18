// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The controller's verified artifact cache: download each pinned artifact once, check its
//! SHA-256, and upload it to hosts that lack it (design §8).
//!
//! Every byte comes through `/usr/bin/curl -q --proto =https --proto-redir =https
//! --tlsv1.2 -fsSL --max-filesize …` in the port's fixed environment (no proxy or CA
//! override from the caller, no `~/.curlrc`), into a temporary file inside `cache/`. Only a
//! file that hashes to its pin is renamed to `cache/<sha256>`, so a name in the cache is a
//! verified artifact and nothing else.
//!
//! aterm's own release is resolved the way install.sh resolves it ([`resolve_aterm`]):
//! the release's `aterm-appcast.toml` names `aterm-<version>-mac.zip` and its
//! `zip_sha256`, and must name Developer ID team A66A9P66Z7, which the official channel
//! pins in the script itself. The zip is then checked here as install.sh checks it after
//! download: one `aterm.app`, `codesign --verify --deep --strict` against that team,
//! Gatekeeper, the bundle id and the version.

use super::inventory::Paths;
use crate::baseline::Baseline;
use crate::items::apps::{
    Assess, Gatekeeper, Signed, assess, bundle_info, ditto_unzip, is_sha256, one_app_in,
    verify_team,
};
use crate::items::aterm::{BUNDLE_ID, TEAM_ID, is_version};
use crate::model::Read;
use crate::port::{Cmd, Scratch, System};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// The public repository every aterm release is published to (install.sh's
/// `OFFICIAL_REPO_SLUG`).
pub const RELEASE_REPO: &str = "alabsystems/aterm";

/// The largest artifact the cache takes (curl's `--max-filesize`). Emacs's image is 179 MB.
pub const MAX_ARTIFACT_BYTES: u64 = 4 << 30;

/// The largest appcast taken: a few dozen KiB in practice, and well under the port's 1 MiB
/// of captured output, so a cut-off answer cannot parse as a whole one.
const MAX_APPCAST_BYTES: u64 = 512 * 1024;

/// The largest install.sh taken (install.sh's own bound on a manifest, 5 MB; the script
/// is about 230 KB).
const MAX_SCRIPT_BYTES: u64 = 5_000_000;

/// The bound on one download: a hang detector over a slow link, not a latency budget.
const DOWNLOAD_BUDGET: Duration = Duration::from_secs(3600);

/// Serialize this process's cache lookup and publish decisions. Temporary allocations
/// are exclusive across calls, including other processes.
static FETCHING: Mutex<()> = Mutex::new(());

/// One artifact a baseline needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Needed {
    /// For messages (`app.Emacs`, `aterm 0.105.0`).
    pub name: String,
    /// `https://` only.
    pub url: String,
    /// Hex.
    pub sha256: String,
}

/// `aterm-<version>-mac.zip`, the release's lean zip (install.sh binds the name to the
/// version exactly).
#[must_use]
pub fn zip_name(version: &str) -> String {
    format!("aterm-{version}-mac.zip")
}

/// The tag-specific download URL of one release asset.
#[must_use]
pub fn release_url(version: &str, asset: &str) -> String {
    format!("https://github.com/{RELEASE_REPO}/releases/download/v{version}/{asset}")
}

/// The appcast of release `version`, or of the newest release.
#[must_use]
pub fn appcast_url(version: Option<&str>) -> String {
    match version {
        Some(v) => release_url(v, "aterm-appcast.toml"),
        None => {
            format!("https://github.com/{RELEASE_REPO}/releases/latest/download/aterm-appcast.toml")
        }
    }
}

/// The install.sh of release `version`, from its tag, never `HEAD`.
///
/// Releases publish no SHA-256 for install.sh, so its pin is the hash of what this URL
/// served when the command resolved it ([`resolve_aterm`]); every envelope that command
/// signs carries that hash, and the agent checks the bytes against it. The baseline file is
/// not rewritten: the next command resolves it again, unless the baseline pins it.
#[must_use]
pub fn install_sh_url(version: &str) -> String {
    format!("https://raw.githubusercontent.com/{RELEASE_REPO}/v{version}/tools/install.sh")
}

/// Every artifact `baseline` installs, after [`resolve_aterm`] has filled `[aterm]`, once
/// each: `[[app]]` rows, Homebrew.pkg, the aterm release zip and its install.sh.
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

/// `curl` of `url` to `out` (`-` for standard output), at most `max` bytes.
fn curl(url: &str, out: &Path, max: u64) -> Cmd {
    Cmd::new("/usr/bin/curl")
        .args([
            "-q",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--tlsv1.2",
            "-fsSL",
            "--connect-timeout",
            "20",
            "--max-filesize",
        ])
        .arg(max.to_string())
        .arg("-o")
        .arg(out)
        .arg(url)
        .budget(DOWNLOAD_BUDGET)
}

/// Download `url` into a temporary file `tmp` in the cache directory, bounded by `max`.
fn download(sys: &dyn System, url: &str, tmp: &Path, max: u64) -> Result<(), String> {
    if !url.starts_with("https://") {
        return Err(format!("{url} is not https://; refusing to download it"));
    }
    // The caller holds the exclusive directory containing this tool output until
    // validation and publication finish. No prior partial path is adopted.
    let ran = sys
        .run(&curl(url, tmp, max))
        .map_err(|e| format!("curl {url}: {e}"))?;
    if ran.success() {
        Ok(())
    } else {
        Err(format!("curl {url}: {}", ran.describe()))
    }
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

/// Make sure `cache/<sha256>` holds `item`: download it with `/usr/bin/curl --proto =https`
/// when missing, and check the hash. Returns the cached path. A cached file that no
/// longer hashes to its name is removed and fetched again.
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

/// One release, as [`parse_appcast`] admitted it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// `MAJOR.MINOR.PATCH`.
    pub version: String,
    /// The SHA-256 of `aterm-<version>-mac.zip`, lowercase.
    pub zip_sha256: String,
}

/// Admit an appcast the way install.sh does: a canonical version, equal to `want` when
/// one is pinned; the zip named exactly `aterm-<version>-mac.zip` with a 64-digit
/// SHA-256; and team A66A9P66Z7, refusing a manifest that omits or contradicts it (it
/// rides unsigned, so the pin lives here, not in it).
///
/// # Errors
/// One line naming the field that refused.
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

/// `url`'s body as text, at most `max` bytes.
fn fetch_text(sys: &dyn System, url: &str, max: u64) -> Result<String, String> {
    if !url.starts_with("https://") {
        return Err(format!("{url} is not https://; refusing to download it"));
    }
    let ran = sys
        .run(&curl(url, Path::new("-"), max))
        .map_err(|e| format!("curl {url}: {e}"))?;
    if ran.success() {
        Ok(ran.out())
    } else {
        Err(format!("curl {url}: {}", ran.describe()))
    }
}

/// Fetch release `version`'s install.sh into the cache and return its SHA-256. It must
/// look like the official script: the whole-file guard (`main "$@"` as its last line, so
/// a cut-off download is refused) and the official Team ID pin.
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
            let last = text
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("");
            if last.trim() != "main \"$@\"" {
                return Err(format!(
                    "{url} does not end with install.sh's `main \"$@\"` line"
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

/// Check the cached release zip `zip` here, before any host sees it, as install.sh checks
/// its download: one `aterm.app`, signed by team A66A9P66Z7, accepted by Gatekeeper, with
/// the release's bundle id and `version`. Expanded in an exclusively allocated cache
/// scratch, whose ownership is retained through verification and cleanup.
///
/// # Errors
/// One line naming the check that refused.
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

/// What `[aterm]` resolves to: the release and the two pins every envelope carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtermPins {
    /// The release.
    pub version: String,
    /// Its mac zip's SHA-256, from its appcast.
    pub archive_sha256: String,
    /// Its install.sh's SHA-256: the baseline's pin, else what the release's tag served.
    pub install_sh_sha256: String,
}

/// The `[aterm]` pins one command resolved, keyed by what each baseline asked for
/// (`version`, `archive_sha256`, `install_sh_sha256`, any of them absent): every host of one
/// command gets the same release, and the appcast, install.sh and the release zip's checks
/// run once per command, not once per host.
#[derive(Debug, Default)]
pub struct AtermMemo {
    resolved: Vec<(AtermAsked, AtermPins)>,
}

/// `[aterm]`'s three pins as a baseline writes them.
type AtermAsked = (Option<String>, Option<String>, Option<String>);

/// Fill `[aterm] archive_sha256` and `install_sh_sha256` for `[aterm] version` (or the
/// newest release when it is absent) from the release's appcast and assets, verified the
/// way install.sh verifies them. Rewrites the baseline text, keeping every comment; a
/// baseline whose `[aterm]` is already filled, or that has none, comes back unchanged and
/// nothing is fetched.
///
/// The zip is downloaded into the cache and checked ([`verify_release_zip`]) before its
/// hash is written down. install.sh is fetched from the release's tag; a pin the baseline
/// already holds for either is kept, and an archive pin the release contradicts is refused.
///
/// # Errors
/// One line.
pub fn resolve_aterm(
    sys: &dyn System,
    paths: &Paths,
    baseline_text: &str,
) -> Result<String, String> {
    resolve_aterm_once(sys, paths, baseline_text, &mut AtermMemo::default())
}

/// [`resolve_aterm`], reusing what `memo` already resolved in this command for the same
/// `[aterm]` table.
///
/// # Errors
/// One line.
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
    // What was written must read back as what was resolved.
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
mod tests {
    use super::*;
    use crate::items::apps::tests::{SPCTL_KYA_ON, codesign_key, dir, err, ok, sha_of};
    use crate::port::Fake;

    /// The head of the live v0.104.0 appcast (2026-10-16), verbatim up to its changelog.
    const APPCAST: &str = r#"schema = 1
version = "0.104.0"
build_number = 1792088595
commit = "74147aac3d1f292fdae5a900e63fab9f9ffea994"
dmg = "aterm-0.104.0.dmg"
sha256 = "da54bb61b14170781a85a9d12e942b27b958c63a1b6afcd6bd65074a2722b84e"
url = "https://github.com/alabsystems/aterm/releases/download/v0.104.0/aterm-0.104.0.dmg"
zip = "aterm-0.104.0-mac.zip"
zip_sha256 = "457cee7e20d0d406fb6ce43275593d0f553eb0fccd46fcc812dfb8d02c0b427f"
min_os = "11.0"
team_id = "A66A9P66Z7"
pub_date = "2026-10-15T18:52:14Z"
min_build = 1786131079
machine_id = "m3"
roster_seq = 4
changelog = '''
### Added

- **Iosevka Term ships with aterm: `font_family = "Iosevka Term"` selects it on macOS, Linux
  and Windows.**
'''
"#;

    const ZIP_SHA: &str = "457cee7e20d0d406fb6ce43275593d0f553eb0fccd46fcc812dfb8d02c0b427f";

    fn paths() -> Paths {
        Paths {
            root: PathBuf::from("/m"),
        }
    }

    fn curl_key(url: &str, out: &str, max: u64) -> String {
        format!(
            "/usr/bin/curl -q --proto =https --proto-redir =https --tlsv1.2 -fsSL --connect-timeout 20 --max-filesize {max} -o {out} {url}"
        )
    }

    fn part(label: &str, sequence: u64) -> String {
        Fake::scratch_path(Some(Path::new("/m/cache")), label, sequence)
            .join("payload")
            .to_string_lossy()
            .into_owned()
    }

    fn plistbuddy(bundle: &str, key: &str) -> String {
        format!("/usr/libexec/PlistBuddy -c 'Print :{key}' {bundle}/Contents/Info.plist")
    }

    #[test]
    fn every_pinned_artifact_is_needed_once() {
        let b = crate::baseline::parse(&format!(
            r#"schema = 1
[[app]]
name = "Caffeine"
kind = "dmg-app"
url = "https://github.com/IntelliScape/caffeine/releases/download/1.1.4/Caffeine.dmg"
sha256 = "1ad34c3299a0c866873c4feb432c3e11186375adb6f43c5f481ae9d46d06d723"
team_id = "YD6LEYT6WZ"
bundle = "Caffeine.app"
version = "1.1.4"

[[app]]
name = "Caffeine-again"
kind = "dmg-app"
url = "https://github.com/IntelliScape/caffeine/releases/download/1.1.4/Caffeine.dmg"
sha256 = "1AD34C3299A0C866873C4FEB432C3E11186375ADB6F43C5F481AE9D46D06D723"
team_id = "YD6LEYT6WZ"
bundle = "Caffeine.app"
version = "1.1.4"

[homebrew]
pkg = {{ url = "https://github.com/Homebrew/brew/releases/download/7.0.9/Homebrew.pkg", sha256 = "e021295947a4a9b7ca1a8eff310c9ea1db14d923c5acf7d79e325b60a81f1a15", team_id = "927JGANW46" }}

[aterm]
version = "0.104.0"
archive_sha256 = "{ZIP_SHA}"
install_sh_sha256 = "dd92e2224c1c5a8e2c69992c4bafdf79142cf27f36a5bacfbfaa8efe12949b98"
"#
        ))
        .unwrap();
        let n = needed(&b);
        let names: Vec<&str> = n.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "app.Caffeine",
                "homebrew.pkg",
                "aterm 0.104.0",
                "aterm 0.104.0 install.sh"
            ]
        );
        assert_eq!(
            n[2].url,
            "https://github.com/alabsystems/aterm/releases/download/v0.104.0/aterm-0.104.0-mac.zip"
        );
        assert_eq!(
            n[3].url,
            "https://raw.githubusercontent.com/alabsystems/aterm/v0.104.0/tools/install.sh"
        );
    }

    #[test]
    fn a_download_that_is_not_https_is_refused_before_anything_runs() {
        let sys = Fake::root();
        let item = Needed {
            name: "app.X".into(),
            url: "http://example.com/x.dmg".into(),
            sha256: "0".repeat(64),
        };
        assert!(
            ensure(&sys, &paths(), &item)
                .unwrap_err()
                .contains("not https://")
        );
        assert!(sys.calls().is_empty());
    }

    #[test]
    fn a_download_is_cached_under_its_hash_only_when_it_hashes_to_the_pin() {
        let bytes = b"the dmg";
        let sha = sha_of(bytes);
        let url = "https://example.com/x.dmg";
        let item = Needed {
            name: "app.X".into(),
            url: url.into(),
            sha256: sha.clone(),
        };
        let sys = Fake::root();
        sys.on(
            &curl_key(url, &part("download", 0), MAX_ARTIFACT_BYTES),
            ok(""),
        );
        sys.on_write(
            &curl_key(url, &part("download", 0), MAX_ARTIFACT_BYTES),
            Path::new(&part("download", 0)),
            bytes,
        );
        let got = ensure(&sys, &paths(), &item).unwrap();
        assert_eq!(got, PathBuf::from(format!("/m/cache/{sha}")));
        assert_eq!(sys.written(&format!("/m/cache/{sha}")).unwrap(), bytes);
        // Cached now: no second download.
        let before = sys.calls().len();
        ensure(&sys, &paths(), &item).unwrap();
        assert_eq!(sys.calls().len(), before);
        // Bytes that hash to something else are never cached.
        let sys = Fake::root();
        sys.on(
            &curl_key(url, &part("download", 0), MAX_ARTIFACT_BYTES),
            ok(""),
        );
        sys.on_write(
            &curl_key(url, &part("download", 0), MAX_ARTIFACT_BYTES),
            Path::new(&part("download", 0)),
            b"tampered",
        );
        let why = ensure(&sys, &paths(), &item).unwrap_err();
        assert!(why.contains("the baseline pins"), "{why}");
        assert!(sys.written(&format!("/m/cache/{sha}")).is_none());
        assert!(sys.written(&part("download", 0)).is_none());
    }

    #[test]
    fn a_failed_download_says_curl_s_reason() {
        let sha = "0".repeat(64);
        let url = "https://example.com/x.dmg";
        let sys = Fake::root();
        sys.on(
            &curl_key(url, &part("download", 0), MAX_ARTIFACT_BYTES),
            err(22, "curl: (22) The requested URL returned error: 404"),
        );
        let why = ensure(
            &sys,
            &paths(),
            &Needed {
                name: "app.X".into(),
                url: url.into(),
                sha256: sha,
            },
        )
        .unwrap_err();
        assert!(why.contains("exit 22: curl: (22)"), "{why}");
    }

    #[test]
    fn the_appcast_is_admitted_the_way_install_sh_admits_it() {
        assert_eq!(
            parse_appcast(APPCAST, None).unwrap(),
            Release {
                version: "0.104.0".into(),
                zip_sha256: ZIP_SHA.into()
            }
        );
        assert!(parse_appcast(APPCAST, Some("0.104.0")).is_ok());
        assert!(
            parse_appcast(APPCAST, Some("0.103.0"))
                .unwrap_err()
                .contains("mismatch")
        );
        let no_team = APPCAST.replace("team_id = \"A66A9P66Z7\"\n", "");
        assert!(
            parse_appcast(&no_team, None)
                .unwrap_err()
                .contains("omits team_id")
        );
        let other = APPCAST.replace("A66A9P66Z7", "AAAAAAAAAA");
        assert!(
            parse_appcast(&other, None)
                .unwrap_err()
                .contains("names signing team")
        );
        let renamed = APPCAST.replace("zip = \"aterm-0.104.0-mac.zip\"", "zip = \"aterm.zip\"");
        assert!(
            parse_appcast(&renamed, None)
                .unwrap_err()
                .contains("not aterm-0.104.0-mac.zip")
        );
    }

    #[test]
    fn a_filled_or_absent_aterm_table_is_left_alone_and_nothing_is_fetched() {
        let sys = Fake::root();
        let text = "schema = 1\n";
        assert_eq!(resolve_aterm(&sys, &paths(), text).unwrap(), text);
        let text = format!(
            "schema = 1\n[aterm]\nversion = \"0.104.0\"\narchive_sha256 = \"{ZIP_SHA}\"\ninstall_sh_sha256 = \"{ZIP_SHA}\"\n"
        );
        assert_eq!(resolve_aterm(&sys, &paths(), &text).unwrap(), text);
        assert!(sys.calls().is_empty());
    }

    /// Script the newest release (v0.104.0) on `sys`: its appcast, zip, the zip's checks and
    /// install.sh. Returns the zip's and install.sh's SHA-256 and the expansion it checks.
    fn scripted_release(sys: &Fake) -> (String, String, String) {
        let zip = b"the release zip";
        let zip_sha = sha_of(zip);
        let appcast = APPCAST.replace(ZIP_SHA, &zip_sha);
        sys.on(
            &curl_key(&appcast_url(None), "-", MAX_APPCAST_BYTES),
            ok(&appcast),
        );
        let zip_url = release_url("0.104.0", "aterm-0.104.0-mac.zip");
        sys.on(
            &curl_key(&zip_url, &part("download", 0), MAX_ARTIFACT_BYTES),
            ok(""),
        );
        sys.on_write(
            &curl_key(&zip_url, &part("download", 0), MAX_ARTIFACT_BYTES),
            Path::new(&part("download", 0)),
            zip,
        );
        let verify = Fake::scratch_path(Some(Path::new("/m/cache")), "verify", 1)
            .to_string_lossy()
            .into_owned();
        sys.on(
            &format!("/usr/bin/ditto -x -k --noacl --noqtn /m/cache/{zip_sha} {verify}"),
            ok(""),
        );
        let app = format!("{verify}/aterm.app");
        let unpack = format!("/usr/bin/ditto -x -k --noacl --noqtn /m/cache/{zip_sha} {verify}");
        sys.on_write(&unpack, Path::new(&app), b"");
        sys.on_stat(&unpack, Path::new(&app), dir());
        sys.on_write(
            &unpack,
            Path::new(&format!("{app}/Contents/Info.plist")),
            b"",
        );
        sys.on(&codesign_key(&app, TEAM_ID), ok(""));
        sys.on(
            &format!("/usr/sbin/spctl -a -vv -t exec {app}"),
            err(0, SPCTL_KYA_ON),
        );
        sys.on(
            &plistbuddy(&app, "CFBundleIdentifier"),
            ok("com.aterm.aterm\n"),
        );
        sys.on(
            &plistbuddy(&app, "CFBundleShortVersionString"),
            ok("0.104.0\n"),
        );
        let script = b"#!/usr/bin/env bash\nOFFICIAL_TEAM_ID=\"A66A9P66Z7\"\nmain \"$@\"\n";
        let script_sha = sha_of(script);
        sys.on(
            &curl_key(
                &install_sh_url("0.104.0"),
                &part("install-script", 2),
                MAX_SCRIPT_BYTES,
            ),
            ok(""),
        );
        sys.on_write(
            &curl_key(
                &install_sh_url("0.104.0"),
                &part("install-script", 2),
                MAX_SCRIPT_BYTES,
            ),
            Path::new(&part("install-script", 2)),
            script,
        );
        (zip_sha, script_sha, verify)
    }

    #[test]
    fn resolving_the_newest_release_checks_its_zip_and_pins_both_hashes_keeping_comments() {
        let sys = Fake::root();
        let (zip_sha, script_sha, verify) = scripted_release(&sys);
        let text = "schema = 1\n\n# The lab runs the newest release.\n[aterm]\nowner = \"admin\" # owns /Applications/aterm.app\ntoolset = true\n";
        let out = resolve_aterm(&sys, &paths(), text).unwrap();
        assert!(out.starts_with("schema = 1\n\n# The lab runs the newest release.\n[aterm]\nowner = \"admin\" # owns /Applications/aterm.app\ntoolset = true\n"), "{out}");
        let b = crate::baseline::parse(&out).unwrap().aterm.unwrap();
        assert_eq!(b.version.as_deref(), Some("0.104.0"));
        assert_eq!(b.archive_sha256.as_deref(), Some(zip_sha.as_str()));
        assert_eq!(b.install_sh_sha256.as_deref(), Some(script_sha.as_str()));
        assert_eq!(
            sys.written(&format!("/m/cache/{script_sha}")).unwrap(),
            b"#!/usr/bin/env bash\nOFFICIAL_TEAM_ID=\"A66A9P66Z7\"\nmain \"$@\"\n"
        );
        // The expansion it checked is gone.
        assert!(sys.stat(Path::new(&verify)).is_err());
        assert!(
            sys.written(&format!("{verify}/aterm.app/Contents/Info.plist"))
                .is_none()
        );
    }

    #[test]
    fn one_command_resolves_aterm_once_and_every_host_gets_the_same_release() {
        let sys = Fake::root();
        let (zip_sha, script_sha, _) = scripted_release(&sys);
        let mut memo = AtermMemo::default();
        let lab_07 = "schema = 1\nusers = [\"lab07\"]\n[aterm]\nowner = \"admin\"\n";
        let lab_08 = "schema = 1\nusers = [\"lab08\"]\n[aterm]\nowner = \"admin\"\n";
        resolve_aterm_once(&sys, &paths(), lab_07, &mut memo).unwrap();
        let fetched = sys.calls().len();
        let second = resolve_aterm_once(&sys, &paths(), lab_08, &mut memo).unwrap();
        assert_eq!(
            sys.calls().len(),
            fetched,
            "nothing fetched or checked again"
        );
        let b = crate::baseline::parse(&second).unwrap();
        assert_eq!(b.users, ["lab08"]);
        let at = b.aterm.unwrap();
        assert_eq!(at.version.as_deref(), Some("0.104.0"));
        assert_eq!(at.archive_sha256.as_deref(), Some(zip_sha.as_str()));
        assert_eq!(at.install_sh_sha256.as_deref(), Some(script_sha.as_str()));
    }

    #[test]
    fn an_archive_pin_the_release_contradicts_is_refused() {
        let sys = Fake::root();
        sys.on(
            &curl_key(&appcast_url(Some("0.104.0")), "-", MAX_APPCAST_BYTES),
            ok(APPCAST),
        );
        let text = format!(
            "schema = 1\n[aterm]\nversion = \"0.104.0\"\narchive_sha256 = \"{}\"\n",
            "a".repeat(64)
        );
        let why = resolve_aterm(&sys, &paths(), &text).unwrap_err();
        assert!(why.contains("refusing"), "{why}");
    }

    #[test]
    fn an_install_sh_that_is_cut_off_or_unpinned_is_not_cached() {
        let sys = Fake::root();
        sys.on(
            &curl_key(
                &install_sh_url("0.104.0"),
                &part("install-script", 0),
                MAX_SCRIPT_BYTES,
            ),
            ok(""),
        );
        sys.on_write(
            &curl_key(
                &install_sh_url("0.104.0"),
                &part("install-script", 0),
                MAX_SCRIPT_BYTES,
            ),
            Path::new(&part("install-script", 0)),
            b"#!/usr/bin/env bash\nOFFICIAL_TEAM_ID=\"A66A9P66Z7\"\nmain() {\n",
        );
        let why = fetch_install_sh(&sys, &paths(), "0.104.0").unwrap_err();
        assert!(why.contains("main \"$@\""), "{why}");
        assert!(sys.written(&part("install-script", 0)).is_none());
    }
}
