// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! .app assembly (release spec §6 `bundle.rs`): build `dist/aterm.app` from
//! the `apps/aterm-mac/Info.plist` template via in-process string substitution
//! (CFBundleShortVersionString, sealed `CFBundleVersion = n`, ATermGitCommit
//! with the `-dirty` rule matching aterm-gui/build.rs), copy the static
//! resources (ShellIntegration/, Help.html, Credits.html, aterm.icns), nest
//! atpkg + aterm-ctl + aterm-cli in Contents/MacOS, drop
//! `.metadata_never_index`, and write the `dist/aterm-<ver>-build.txt`
//! provenance record.
//!
//! Port of the layout phase of `apps/aterm-mac/build-app.sh` (steps 2–6c + 8).
//! PlistBuddy is replaced by [`stamp_info_plist`] — pure string substitution
//! on the committed template, unit-tested against goldens in
//! `tests/plist_stamp.rs` — so the stamp is deterministic and testable off-mac.
//! The binaries arrive PRE-stripped from `buildplan::run` (strip -x is that
//! module's charter); Credits.html is the committed
//! `apps/aterm-mac/Credits.html` (extracted from build-app.sh's heredoc),
//! copied like every other resource instead of being generated inline.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Everything [`assemble`] + [`write_provenance`] need, resolved by the caller.
pub struct BundleSpec {
    /// Workspace root (locates the apps/aterm-mac templates + git).
    pub repo_root: PathBuf,
    /// `dist/` — receives aterm.app and the build.txt provenance record.
    pub out_dir: PathBuf,
    /// Display version, plain MAJOR.MINOR ("0.26") → CFBundleShortVersionString
    /// and the `aterm-<ver>-build.txt` name. (A real patch version is kept
    /// verbatim — the `.0`-strip happens at the caller, matching build-app.sh.)
    pub short_version: String,
    /// The claimed ledger number `n` → sealed CFBundleVersion. macOS/Gatekeeper
    /// require it to increase build-over-build, and the updater's anti-replay
    /// bind requires it to equal the manifest's build_number byte-for-byte.
    pub build_number: u64,
    /// CFBundleIdentifier (default com.aterm.aterm — the identifier a
    /// Developer-ID signing/notarization profile would bind to).
    pub bundle_id: String,
    /// ATermGitCommit stamp — short=12 with the `-dirty` rule; produce it via
    /// [`git_commit_stamp`] so the plist and the binary's own ATERM_GIT_COMMIT
    /// agree byte-for-byte.
    pub git_commit: String,
    /// The ship-ready (universal, stripped) binaries from `buildplan::run`.
    pub aterm_bin: PathBuf,
}

/// Pure `-dirty` rule, EXACTLY matching crates/aterm-gui/build.rs: the suffix
/// is only appended to a REAL commit — an unborn/.git-less tree stamps a bare
/// "unknown", never "unknown-dirty", so the plist and the binary agree.
pub fn commit_stamp(short_commit: Option<&str>, dirty: bool) -> String {
    match short_commit {
        Some(c) if dirty => format!("{c}-dirty"),
        Some(c) => c.to_string(),
        None => "unknown".to_string(),
    }
}

/// IO wrapper for [`commit_stamp`]: probe git the same way build.rs does
/// (`rev-parse --short=12 HEAD` + `status --porcelain`; every probe
/// best-effort → "unknown" rather than failing).
pub fn git_commit_stamp(repo_root: &Path) -> String {
    let commit = git_out(repo_root, &["rev-parse", "--short=12", "HEAD"]);
    let dirty = git_out(repo_root, &["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
    commit_stamp(commit.as_deref(), dirty)
}

fn git_out(repo_root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Stamp the Info.plist TEMPLATE (apps/aterm-mac/Info.plist) in-process — the
/// PlistBuddy replacement. Replaces the value of an existing `<key>` /
/// `<string>` pair, or inserts the pair before the closing `</dict>` when the
/// key is absent (PlistBuddy's `Add … || Set …` fallback, in one step):
///   * CFBundleShortVersionString ← `short` (human version, MAJOR.MINOR)
///   * CFBundleVersion            ← `build_number` (the monotonic ledger n)
///   * CFBundleIdentifier         ← `bundle_id`
///   * ATermGitCommit             ← `git_commit` (self-describing bundles:
///     `plutil -p` answers "which source built this?" without launching it)
///   * CFBundleIconFile           ← `icon` — only when Some, mirroring
///     build-app.sh, which stamps it only when aterm.icns exists.
pub fn stamp_info_plist(
    template: &str,
    short: &str,
    build_number: u64,
    bundle_id: &str,
    git_commit: &str,
    icon: Option<&str>,
) -> Result<String, String> {
    let mut plist = template.to_string();
    plist = set_plist_string(&plist, "CFBundleShortVersionString", short)?;
    plist = set_plist_string(&plist, "CFBundleVersion", &build_number.to_string())?;
    plist = set_plist_string(&plist, "CFBundleIdentifier", bundle_id)?;
    plist = set_plist_string(&plist, "ATermGitCommit", git_commit)?;
    if let Some(icon) = icon {
        plist = set_plist_string(&plist, "CFBundleIconFile", icon)?;
    }
    Ok(plist)
}

/// Replace-or-insert one `<key>K</key><string>V</string>` pair. Textual on
/// purpose: the committed template is trusted, tab-indented XML; a full plist
/// parser would be a new dependency for zero gain (spec: in-process string
/// substitution). Values are XML-escaped so a stamp can never break the plist.
fn set_plist_string(plist: &str, key: &str, value: &str) -> Result<String, String> {
    let value = xml_escape(value);
    let key_tag = format!("<key>{key}</key>");
    if let Some(kpos) = plist.find(&key_tag) {
        // Existing key: replace the CONTENT of the next <string> element.
        let after = kpos + key_tag.len();
        let sstart = plist[after..]
            .find("<string>")
            .map(|i| after + i + "<string>".len())
            .ok_or_else(|| format!("Info.plist template: no <string> after {key_tag}"))?;
        let send = plist[sstart..]
            .find("</string>")
            .map(|i| sstart + i)
            .ok_or_else(|| format!("Info.plist template: unterminated <string> for {key}"))?;
        // Guard: the <string> must belong to THIS key, not a later one — a
        // template drift where the key held e.g. <true/> would silently stamp
        // the wrong element otherwise.
        if plist[after..sstart].contains("<key>") {
            return Err(format!(
                "Info.plist template: {key} is not a <string> value"
            ));
        }
        Ok(format!("{}{}{}", &plist[..sstart], value, &plist[send..]))
    } else {
        // Absent key: insert before the final </dict>, tab-indented like the
        // committed template (PlistBuddy's Add path).
        let dict_end = plist
            .rfind("</dict>")
            .ok_or_else(|| "Info.plist template: no closing </dict>".to_string())?;
        Ok(format!(
            "{}\t{key_tag}\n\t<string>{value}</string>\n{}",
            &plist[..dict_end],
            &plist[dict_end..]
        ))
    }
}

/// Minimal XML escaping for plist string content (stamped values are versions,
/// hashes and reverse-DNS ids — this is belt-and-braces, not a feature).
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Assemble `dist/aterm.app` (build-app.sh steps 2–6c). Returns the .app path.
/// Signing is NOT done here — the caller runs `sign::` next (inside-out), then
/// dmg, then [`write_provenance`] (whose binary_sha256 must cover the SIGNED
/// bytes, so it must run after signing — same order as the script).
pub fn assemble(spec: &BundleSpec) -> Result<PathBuf, String> {
    let mac_dir = spec.repo_root.join("apps/aterm-mac");
    let app = spec.out_dir.join("aterm.app");

    // Keep the BUILD-OUTPUT bundle out of Spotlight: dist/aterm.app is a real,
    // launchable .app, so without this it shows up as a SECOND "aterm" in
    // Spotlight/Launchpad next to the installed /Applications copy.
    // `.metadata_never_index` tells Spotlight to skip this directory (and
    // everything under it), so only the installed app is offered.
    std::fs::create_dir_all(&spec.out_dir)
        .map_err(|e| format!("create {}: {e}", spec.out_dir.display()))?;
    let _ = std::fs::write(spec.out_dir.join(".metadata_never_index"), "");

    // --- 2. lay out the bundle -------------------------------------------
    println!("==> assembling {}", app.display());
    if app.exists() {
        std::fs::remove_dir_all(&app).map_err(|e| format!("rm -rf {}: {e}", app.display()))?;
    }
    let macos = app.join("Contents/MacOS");
    let resources = app.join("Contents/Resources");
    std::fs::create_dir_all(&macos).map_err(|e| format!("create {}: {e}", macos.display()))?;
    std::fs::create_dir_all(&resources)
        .map_err(|e| format!("create {}: {e}", resources.display()))?;

    // --- 3. THE executable (pre-stripped by buildplan; symbols in the dSYM) —
    // plus argv0 compat SYMLINKS. One Mach-O carries the window, the session,
    // and every verb; the symlinks keep every pre-one-binary name resolving
    // (old installs' ~/.local/bin/aterm -> aterm-cli, in-session `aterm-ctl`
    // scripts, $ATERM_CTL, aterm-nest's aterm-gui lookup, direct atpkg calls).
    // The binary dispatches on argv[0], so each alias IS that tool. Symlinks
    // are not Mach-Os: nothing extra to sign, and the sealed bundle covers
    // them as resources.
    copy_exe(&spec.aterm_bin, &macos.join("aterm"))?;
    #[cfg(unix)]
    for alias in [
        "aterm-cli",
        "aterm-ctl",
        "atpkg",
        "aterm-fleet",
        "aterm-drive",
        "aterm-gui",
    ] {
        let link = macos.join(alias);
        std::os::unix::fs::symlink("aterm", &link)
            .map_err(|e| format!("symlink {} -> aterm: {e}", link.display()))?;
    }

    // --- 4. Info.plist + version/commit stamp ------------------------------
    // CFBundleIconFile is stamped only when the icon ships (template parity
    // with build-app.sh step 6).
    let icns = mac_dir.join("aterm.icns");
    let icon = icns.is_file().then_some("aterm");
    let template_path = mac_dir.join("Info.plist");
    let template = std::fs::read_to_string(&template_path)
        .map_err(|e| format!("read {}: {e}", template_path.display()))?;
    let stamped = stamp_info_plist(
        &template,
        &spec.short_version,
        spec.build_number,
        &spec.bundle_id,
        &spec.git_commit,
        icon,
    )?;
    std::fs::write(app.join("Contents/Info.plist"), stamped)
        .map_err(|e| format!("write Info.plist: {e}"))?;
    println!(
        "    version={} build={} commit={} bundle-id={}",
        spec.short_version, spec.build_number, spec.git_commit, spec.bundle_id
    );

    // --- 5. shell-integration resources ------------------------------------
    let shell_src = mac_dir.join("Sources/ATermMac/Resources/ShellIntegration");
    if shell_src.is_dir() {
        let dst = resources.join("ShellIntegration");
        std::fs::create_dir_all(&dst).map_err(|e| format!("create {}: {e}", dst.display()))?;
        for entry in std::fs::read_dir(&shell_src)
            .map_err(|e| format!("read {}: {e}", shell_src.display()))?
        {
            let entry = entry.map_err(|e| format!("read {}: {e}", shell_src.display()))?;
            std::fs::copy(entry.path(), dst.join(entry.file_name()))
                .map_err(|e| format!("copy {}: {e}", entry.path().display()))?;
        }
    }

    // --- 6. icon (optional — its plist stamp already handled above) --------
    if icns.is_file() {
        std::fs::copy(&icns, resources.join("aterm.icns"))
            .map_err(|e| format!("copy aterm.icns: {e}"))?;
    }

    // --- 6b. About-panel credits -------------------------------------------
    // The standard macOS About panel (App menu ▸ About aterm, wired in
    // menu.rs) auto-loads Contents/Resources/Credits.html. Copied from the
    // committed apps/aterm-mac/Credits.html (the old build-app.sh heredoc,
    // extracted so it is editable + reviewable like every other resource).
    let credits = mac_dir.join("Credits.html");
    std::fs::copy(&credits, resources.join("Credits.html"))
        .map_err(|e| format!("copy {}: {e}", credits.display()))?;

    // --- 6c. in-app Help page ------------------------------------------------
    // Self-contained features guide (Help ▸ aterm Help → opens this bundled
    // file in the browser, fully offline). A no-op if absent — Help then falls
    // back to the project URL, same as the script.
    let help = mac_dir.join("Help.html");
    if help.is_file() {
        std::fs::copy(&help, resources.join("Help.html"))
            .map_err(|e| format!("copy Help.html: {e}"))?;
        println!("    bundled Help.html");
    }

    Ok(app)
}

fn copy_exe(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::copy(src, dst)
        .map_err(|e| format!("copy {} -> {}: {e}", src.display(), dst.display()))?;
    // .app bundles are only assembled on Unix hosts; the exec bit has no
    // Windows equivalent, so the chmod is Unix-gated for cross-compilation.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(dst)
            .map_err(|e| format!("stat {}: {e}", dst.display()))?
            .permissions();
        perms.set_mode(perms.mode() | 0o755);
        std::fs::set_permissions(dst, perms)
            .map_err(|e| format!("chmod {}: {e}", dst.display()))?;
    }
    Ok(())
}

/// Write `dist/aterm-<ver>-build.txt` — the per-artifact provenance record
/// (build-app.sh step 8, same KEY=value fields in the same order). MUST run
/// AFTER signing: binary_sha256 covers the shipped Contents/MacOS/aterm
/// bytes, and codesign rewrites them.
pub fn write_provenance(spec: &BundleSpec, app: &Path, signed_by: &str) -> Result<PathBuf, String> {
    let shipped = app.join("Contents/MacOS/aterm");
    // In-process sha256 (sha2): the digest on record is provably the digest of
    // the bytes on disk, not of whatever a shelled hasher happened to read.
    let binary_sha256 = sha256_hex(&shipped)?;
    // build-app.sh emits the BARE short commit here (its `commit=` line runs
    // rev-parse without the -dirty suffix; only ATermGitCommit carries it).
    let bare_commit = spec.git_commit.trim_end_matches("-dirty");
    let dwarf = spec
        .out_dir
        .join("aterm.dSYM/Contents/Resources/DWARF/aterm");
    let has_dsym = std::fs::metadata(&dwarf)
        .map(|m| m.len() > 0)
        .unwrap_or(false);
    // One binary: the verb surface is compiled in; the aliases are symlinks.
    let has_aliases = app.join("Contents/MacOS/aterm-ctl").is_symlink();

    let path = spec
        .out_dir
        .join(format!("aterm-{}-build.txt", spec.short_version));
    let body = format!(
        "name=aterm\n\
         version={}\n\
         build={}\n\
         commit={}\n\
         built={}\n\
         bundle_id={}\n\
         binary_sha256={}\n\
         signed_by={}\n\
         has_dsym={}\n\
         layout=one-binary\n\
         has_aliases={}\n",
        spec.short_version,
        spec.build_number,
        bare_commit,
        epoch_to_rfc3339(spec.build_number),
        spec.bundle_id,
        binary_sha256,
        signed_by,
        yes_no(has_dsym),
        yes_no(has_aliases),
    );
    std::fs::write(&path, body).map_err(|e| format!("write {}: {e}", path.display()))?;
    println!("==> wrote {}", path.display());
    Ok(path)
}

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

/// Unix epoch seconds → "YYYY-MM-DDTHH:MM:SSZ" (build-app.sh's
/// `date -u -r $SOURCE_DATE_EPOCH`), computed in-process so the provenance
/// record never depends on the host `date` flavor. Civil-date conversion via
/// Howard Hinnant's days algorithm — exact for all of Unix time.
pub fn epoch_to_rfc3339(epoch: u64) -> String {
    let days = epoch / 86_400;
    let secs = epoch % 86_400;
    // civil_from_days, specialized to day 0 = 1970-01-01 (era math keeps every
    // intermediate positive; correct across all Gregorian leap rules).
    let z = days + 719_468; // shift epoch from 1970-01-01 to 0000-03-01
    let era = z / 146_097;
    let doe = z % 146_097; // day-of-era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // year-of-era
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day-of-year, Mar-based
    let mp = (5 * doy + 2) / 153; // month, Mar=0
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// Streaming in-process SHA-256 of a file (shared shape with dmg.rs — kept
/// module-local so each file stays self-contained for the #[path] test mounts).
fn sha256_hex(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>())
}
