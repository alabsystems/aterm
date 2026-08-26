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
    /// Workspace-derived release version, canonical MAJOR.MINOR.PATCH ("0.2.0") →
    /// CFBundleShortVersionString and the `aterm-<ver>-build.txt` name.
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
    /// A VALIDATED toolchain-seed registry to seal into
    /// `Contents/Resources/<atpkg::SEED_DIR_NAME>` (batteries-included, §9.1) — the
    /// caller runs `seedpack::validate` first; `None` ships a seedless bundle
    /// (the explicit `ATERM_SEEDLESS=1` cut). Data payloads only (tarballs +
    /// signed manifests), so nothing here changes the one-Mach-O signing story.
    pub seed: Option<crate::seedpack::SeedStat>,
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
///   * CFBundleShortVersionString ← `short` (human version, MAJOR.MINOR.PATCH)
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
/// The scratch directory the cut ASSEMBLES in, under `dist/`.
///
/// Deliberately NOT `dist/aterm.app`: on the release machine that path is also a
/// live, self-updating install (the owner runs it, and the activation lane watches
/// it). Assembling there means the cut is writing a bundle another process owns,
/// and the consequences are not theoretical — see [`staged_app_path`].
pub const CUT_APP_DIR: &str = "cut-app";

/// Where THIS cut's bundle is built, signed, notarized and packaged.
///
/// The cut used to assemble directly into `dist/aterm.app`. On 2026-08-19 that
/// produced a corrupt release: the bundle step sealed the batteries-included
/// toolchain into it, the running aterm's activation lane took the half-built
/// bundle six minutes later (it was signed by then, and newer than the running
/// build), and the successor's first-run `atpkg seed` pass judged the seal spent
/// on an already-provisioned machine and DELETED it — a gigabyte removed from the
/// artifact the cutter was still packaging. The DMG had been built and kept the
/// seal; the updater zip had not, silently took the seedless path and skipped the
/// gate that proves the stripped bundle still passes Gatekeeper. Only the
/// provenance recount caught it, after two notarizations.
///
/// Assembling somewhere no running process owns fixes that for EVERY client
/// version, including ones already installed — a marker the client must honour
/// would only protect clients new enough to know about it. It also stops
/// [`assemble`]'s `rm -rf` from deleting the bundle a live process is executing
/// out of, which every cut on this machine had been doing.
///
/// The finished bundle is placed at `dist/aterm.app` after the release verifies,
/// so the dev install still takes its own release — just a complete one.
#[must_use]
pub fn staged_app_path(dist: &Path) -> PathBuf {
    dist.join(CUT_APP_DIR).join("aterm.app")
}

/// The DEV INSTALL path: `dist/aterm.app`, what the owner's machine runs and what
/// its updater watches. The cut only ever writes here through
/// [`place_finished_bundle`], and only once the release is live.
#[must_use]
pub fn dev_install_app_path(dist: &Path) -> PathBuf {
    dist.join("aterm.app")
}

/// Read one `<key>K</key><string>V</string>` value out of a stamped Info.plist.
///
/// Local rather than borrowed from `manifest_out`: this module is mounted on its own
/// by several integration tests (`#[path]` module mounts), and a cross-module call
/// would make it uncompilable there for a six-line string scan. It reads what
/// [`set_plist_string`] writes.
fn sealed_plist_string(plist: &str, key: &str) -> Option<String> {
    let key_tag = format!("<key>{key}</key>");
    let after = plist.find(&key_tag)? + key_tag.len();
    let start = plist[after..].find("<string>")? + after + "<string>".len();
    if plist[after..start].contains("<key>") {
        return None;
    }
    let end = plist[start..].find("</string>")? + start;
    Some(plist[start..end].to_string())
}

/// Put this cut's finished bundle at `dist/aterm.app`, where the dev install runs
/// from — the LAST thing a cut does, after the release is live and verified.
///
/// Ordering is the whole point. The live updater on this machine watches that path
/// and will adopt whatever appears there; before this split it could adopt a bundle
/// mid-assembly. Now the only bundle it can ever see is one that has been signed,
/// notarized, stapled, self-checked, published, verified and mirrored.
///
/// COPIED, not moved: `dist/cut-app/aterm.app` stays as the cut's artifact, so a
/// late `--resume` still has the bytes its journal describes. `cp -Rc` clones on
/// APFS, so a batteries-included bundle costs metadata rather than a second
/// gigabyte, and `cp` (not a hand-rolled walk) is what preserves the symlinks,
/// extended attributes and `_CodeSignature` layout the seal covers.
///
/// The swap is two renames within one directory rather than delete-then-copy, so
/// there is no window in which `dist/aterm.app` does not exist — a launch during
/// that window would simply fail to find the app.
///
/// Returns the number of bytes the placed bundle carries.
pub fn place_finished_bundle(dist: &Path, version: &str, build: u64) -> Result<u64, String> {
    let staged = staged_app_path(dist);
    if !staged.is_dir() {
        return Err(format!("{} is not a bundle", staged.display()));
    }
    // PROVE IT IS THIS CUT'S BUNDLE. `dist/cut-app/` is never cleaned, and two
    // pipelines reach this line without having assembled anything: a RECOVERY of
    // another machine's published release (its journal marks `build` done, so
    // `assemble` never runs) and any resume past `build`. Whatever an earlier
    // `--dry-run` or `--rehearse` left behind would otherwise be copied over the dev
    // install under a transcript line calling it "this cut's verified bundle" —
    // downgrading the machine, or, if that leftover carries a HIGHER provisional
    // build number (a dry run claims `max(tail + 1, now)` and is signed and
    // notarized for real), handing the activation lane a build that was never
    // released. The lane accepts on build number + policy alone; it does not compare
    // against the published manifest (2026-08-19 round-7 audit).
    let plist = std::fs::read_to_string(staged.join("Contents/Info.plist"))
        .map_err(|e| format!("read {}: {e}", staged.join("Contents/Info.plist").display()))?;
    let sealed_version = sealed_plist_string(&plist, "CFBundleShortVersionString");
    let sealed_build = sealed_plist_string(&plist, "CFBundleVersion");
    if sealed_version.as_deref() != Some(version)
        || sealed_build.as_deref() != Some(&build.to_string())
    {
        return Err(format!(
            "{} carries {} build {}, not this cut's {version} build {build} — refusing to \
             hand the dev install a bundle this cut did not produce",
            staged.display(),
            sealed_version.as_deref().unwrap_or("no version"),
            sealed_build.as_deref().unwrap_or("no build"),
        ));
    }
    let live = dev_install_app_path(dist);
    let incoming = dist.join(".aterm.app.incoming");
    let previous = dist.join(".aterm.app.previous");
    for scratch in [&incoming, &previous] {
        if scratch.exists() {
            std::fs::remove_dir_all(scratch)
                .map_err(|e| format!("clear {}: {e}", scratch.display()))?;
        }
    }
    let clone = std::process::Command::new("cp")
        .args(["-Rc"])
        .arg(&staged)
        .arg(&incoming)
        .status();
    let cloned = matches!(clone, Ok(status) if status.success());
    if !cloned {
        // A filesystem that cannot clone is the pre-existing behaviour, not a new
        // failure mode.
        let _ = std::fs::remove_dir_all(&incoming);
        let plain = std::process::Command::new("cp")
            .arg("-R")
            .arg(&staged)
            .arg(&incoming)
            .status()
            .map_err(|e| format!("cp -R into {}: {e}", incoming.display()))?;
        if !plain.success() {
            return Err(format!(
                "cp -R {} -> {}",
                staged.display(),
                incoming.display()
            ));
        }
    }
    let had_live = live.exists();
    if had_live {
        std::fs::rename(&live, &previous)
            .map_err(|e| format!("move the old dev install aside: {e}"))?;
    }
    if let Err(error) = std::fs::rename(&incoming, &live) {
        // Put the old one back rather than leaving the machine with no app.
        if had_live {
            let _ = std::fs::rename(&previous, &live);
        }
        return Err(format!("place {}: {error}", live.display()));
    }
    if had_live {
        let _ = std::fs::remove_dir_all(&previous);
    }
    Ok(dir_bytes(&live))
}

/// Recursive byte total, tolerating anything unreadable — this feeds a transcript
/// line, never a decision.
fn dir_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => dir_bytes(&entry.path()),
            Ok(kind) if kind.is_file() => entry.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

pub fn assemble(spec: &BundleSpec) -> Result<PathBuf, String> {
    let mac_dir = spec.repo_root.join("apps/aterm-mac");
    let app = staged_app_path(&spec.out_dir);

    // Keep the BUILD-OUTPUT bundle out of Spotlight: dist/aterm.app is a real,
    // launchable .app, so without this it shows up as a SECOND "aterm" in
    // Spotlight/Launchpad next to the installed /Applications copy.
    // `.metadata_never_index` tells Spotlight to skip this directory (and
    // everything under it), so only the installed app is offered.
    std::fs::create_dir_all(&spec.out_dir)
        .map_err(|e| format!("create {}: {e}", spec.out_dir.display()))?;
    let _ = std::fs::write(spec.out_dir.join(".metadata_never_index"), "");
    // The assembly directory needs its own marker: Spotlight's exclusion is
    // per-directory, and this bundle is even less of an install than the dev one
    // beside it — for most of its life it is not yet signed.
    let staging_dir = spec.out_dir.join(CUT_APP_DIR);
    std::fs::create_dir_all(&staging_dir)
        .map_err(|e| format!("create {}: {e}", staging_dir.display()))?;
    let _ = std::fs::write(staging_dir.join(".metadata_never_index"), "");

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

    // --- 6d. toolchain seed (batteries-included) ----------------------------
    // The VALIDATED signed registry (seedpack::validate ran in step_build) is
    // sealed under Resources/ as plain data — the client's bundled-seed lane
    // installs from it offline through atpkg's ordinary signature gates. Flat
    // copy of regular files only; validate() already refused anything else.
    // Runs before sign::sign_app by pipeline order, so the seal covers it.
    if let Some(seed) = &spec.seed {
        // The ONE spelling of the name, shared with the client that probes for
        // it (`atpkg::bundled`) — a cutter that sealed `toolchain-seed` while
        // the client looked for anything else would ship a silent no-op.
        let dst = resources.join(atpkg::SEED_DIR_NAME);
        std::fs::create_dir_all(&dst).map_err(|e| format!("create {}: {e}", dst.display()))?;
        for entry in
            std::fs::read_dir(&seed.dir).map_err(|e| format!("read {}: {e}", seed.dir.display()))?
        {
            let entry = entry.map_err(|e| format!("read {}: {e}", seed.dir.display()))?;
            std::fs::copy(entry.path(), dst.join(entry.file_name()))
                .map_err(|e| format!("copy {}: {e}", entry.path().display()))?;
        }
        println!(
            "    bundled toolchain-seed: {} file(s), {}, index_build={}, programs [{}]",
            seed.files,
            // The client's own formatter, not a mirror of it: the cut transcript
            // and Settings ▸ Packages then report a seed's size in the same
            // units and rounding by construction rather than by discipline.
            atpkg::human_bytes(seed.bytes),
            seed.index_build,
            seed.programs
                .iter()
                .map(|(p, b)| format!("{p}@{b}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
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
    // In-process sha256 (aterm-digest): the digest on record is provably the digest of
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
    // Batteries-included seed lines (keys appended after the original fields so
    // every existing consumer keeps parsing): what the seal carries, from the
    // VALIDATED stat — plus a paranoid recount of the copied dir so the record
    // describes the actual bundle, not the intent. A seedless cut keeps the
    // honest constant "seed=no" the retirement era emitted.
    let seed_lines = match &spec.seed {
        Some(seed) => {
            let copied = app.join("Contents/Resources").join(atpkg::SEED_DIR_NAME);
            let n = std::fs::read_dir(&copied)
                .map(|it| it.filter_map(Result::ok).count())
                .unwrap_or(0);
            if n != seed.files {
                return Err(format!(
                    "provenance: bundled seed holds {n} file(s) but the validated registry \
                     had {} — the copy is not what was audited",
                    seed.files
                ));
            }
            format!(
                "seed=yes\nseed_files={}\nseed_bytes={}\nseed_index_build={}\nseed_roster_seq={}\nseed_valid_until={}\nseed_programs={}\n",
                seed.files,
                seed.bytes,
                seed.index_build,
                seed.roster_seq,
                seed.valid_until,
                seed.programs
                    .iter()
                    .map(|(p, b)| format!("{p}@{b}"))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
        None => "seed=no\n".to_string(),
    };
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
         has_aliases={}\n\
         {seed_lines}",
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
/// record never depends on the host `date` flavor — the shared
/// `aterm_types::rfc3339` civil-calendar math, exact for all of Unix time.
pub fn epoch_to_rfc3339(epoch: u64) -> String {
    aterm_types::rfc3339::format_rfc3339(epoch)
}

/// Streaming in-process SHA-256 of a file (shared shape with dmg.rs — kept
/// module-local so each file stays self-contained for the #[path] test mounts).
fn sha256_hex(path: &Path) -> Result<String, String> {
    use aterm_digest::Sha256;
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
