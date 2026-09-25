// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! .app assembly (release spec §6 `bundle.rs`): build this claim's staging
//! bundle, `dist/cut-<build>.noindex/aterm.app`, from
//! the `apps/aterm-mac/Info.plist` template via in-process string substitution
//! (CFBundleShortVersionString, sealed `CFBundleVersion = n`, ATermGitCommit
//! with the `-dirty` rule matching aterm-gui/build.rs), copy the static
//! resources (ShellIntegration/, Help.html, Credits.html, aterm.icns), nest
//! atpkg + aterm-ctl + aterm-cli in Contents/MacOS, and write the
//! `dist/aterm-<ver>-build.txt` provenance record. No `.metadata_never_index` marker
//! (see [`assemble`]: inert for Spotlight, and its one reader is deleted).
//!
//! Port of the layout phase of the retired `apps/aterm-mac/build-app.sh`
//! (steps 2–6c + 8) — that script was deleted with the shell pipeline and is
//! not in this tree.
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
    /// `dist/` — receives this claim's `cut-<build>.noindex/aterm.app` and the build.txt
    /// provenance record.
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

/// The name of the staging directory for claim `build`: `cut-<build>.noindex`.
///
/// Unique per claim, so no cut ever removes a bundle another cut (or a dry run, or
/// the owner) left behind in order to build its own. And `.noindex`, because a
/// directory name ending `.noindex` is the only Spotlight exclusion measured to work
/// (see [`assemble`]'s note on `.metadata_never_index`): a staging bundle that does
/// not appear in Spotlight or Launchpad is one nobody launches by accident, which is
/// how a live aterm came to be running out of `dist/cut-app/` in the first place.
#[must_use]
pub fn staging_dir_name(build: u64) -> String {
    format!("cut-{build}.noindex")
}

/// Whether `name` names a cut staging directory, `cut-<digits>.noindex`. Nothing
/// else under `dist/` qualifies, so the journal (`cut-state.toml`) and the DMGs are
/// never mistaken for one — and neither is the fixed `dist/cut-app/` the cut
/// assembled in until 2026-09-23: no cut writes, reads or prunes it any more, so a
/// process running out of it is never under a delete this code makes.
#[must_use]
pub fn is_staging_dir_name(name: &str) -> bool {
    name.strip_prefix("cut-")
        .and_then(|rest| rest.strip_suffix(".noindex"))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Where THIS cut's bundle is built, signed, notarized and packaged:
/// `dist/cut-<build>.noindex/aterm.app`.
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
/// Moving assembly to `dist/cut-app/` fixed THAT. What the
/// move did NOT do, although this comment said so until 2026-09-23, is keep
/// [`assemble`]'s `rm -rf` away from a live process: the path was fixed, nothing
/// checked it, and the 2026-09-23 audit found the owner's aterm (pid 85619) running
/// out of `dist/cut-app/aterm.app` — the directory the next cut would have deleted
/// under it, re-attributing the process to a path that no longer resolves, which
/// is how macOS came to reset the owner's TCC grants on 2026-09-21. What keeps a
/// live process out from under the cut NOW is four things together:
///
/// 1. the name is unique per claim ([`staging_dir_name`]), so building never has
///    to remove anybody else's bundle;
/// 2. it ends `.noindex`, so the bundle is not offered by Spotlight or Launchpad;
/// 3. the pre-claim gate (`gates::staged_bundle_liveness_gate`) refuses the cut —
///    for free, before a build number is burned — while any process runs out of
///    ANY staging directory, or when the process list cannot be read;
/// 4. [`assemble`] re-asks at the moment it would `rm -rf` its own directory (a
///    resume that rebuilds), and [`prune_staging_dirs`] removes an older directory
///    only when the process list PROVES nothing runs from it.
///
/// Every step after `build` reads the bundle from here too; nothing copies it
/// anywhere else. The cut used to finish by placing it at `dist/aterm.app`, a dev
/// install its own updater watched — a copy no install path depends on (the dev
/// app is tools/dev-app.sh's; a release installs from the DMG), so it is gone. And
/// once a real or rehearsed cut is done, [`discard_staged_app`] removes this claim's
/// staging directory too (under the same liveness rule), so no finished cut leaves a
/// launchable copy of the app behind; a dry run keeps it to inspect.
#[must_use]
pub fn staged_app_path(dist: &Path, build: u64) -> PathBuf {
    dist.join(staging_dir_name(build)).join("aterm.app")
}

/// One running process: its pid, and the executable path the kernel reports for it.
pub type RunningProcess = (String, String);

/// Every running process, or `None` when the question could not be asked.
///
/// Read from the KERNEL's path for each process (`atpkg::gc::running_processes`:
/// `proc_listallpids` + `proc_pidpath` on macOS, `/proc/<pid>/exe` on Linux), which
/// is the view `tccd` attributes against. Not `ps -Ao comm=`: its `comm` is argv[0],
/// so a relative, symlinked or PATH launch out of a staging bundle would read as
/// running from somewhere else (the 2026-09-23 audit of the placement guard, main
/// 70ba9c074). And not `std::env::current_exe()`, which on macOS is the
/// `execve`-time string and never follows a rename.
///
/// `None` and `Some(vec![])` are deliberately different: the first is "I could
/// not look", the second is "I looked and nothing is running". Only the second
/// licenses deleting or displacing a bundle.
#[must_use]
pub fn running_processes() -> Option<Vec<RunningProcess>> {
    Some(
        atpkg::gc::running_processes()?
            .into_iter()
            .map(|(pid, exe)| (pid.to_string(), exe.to_string_lossy().into_owned()))
            .collect(),
    )
}

/// Every cut staging directory under `dist` that a process in `processes` is
/// executing out of, with the pids — `(directory, pids)`, sorted by directory.
///
/// Read from the PROCESSES, not from a listing of `dist/`: a process whose staging
/// directory the listing would miss (renamed, half-deleted) is still reported. A
/// path counts when it is `<dist>/<name>/…` for a [`is_staging_dir_name`] `<name>`,
/// under `dist` as given OR as the filesystem resolves it, because the kernel
/// reports a resolved path and a caller may hold a symlinked one.
#[must_use]
pub fn live_staging_dirs(dist: &Path, processes: &[RunningProcess]) -> Vec<(PathBuf, Vec<String>)> {
    let mut roots = vec![dist.to_path_buf()];
    if let Ok(resolved) = std::fs::canonicalize(dist)
        && resolved != dist
    {
        roots.push(resolved);
    }
    let mut live: std::collections::BTreeMap<PathBuf, Vec<String>> =
        std::collections::BTreeMap::new();
    for (pid, comm) in processes {
        for root in &roots {
            let prefix = format!("{}/", root.display());
            let Some(rest) = comm.strip_prefix(&prefix) else {
                continue;
            };
            let Some((name, _)) = rest.split_once('/') else {
                continue;
            };
            if is_staging_dir_name(name) {
                live.entry(dist.join(name)).or_default().push(pid.clone());
                break;
            }
        }
    }
    live.into_iter().collect()
}

/// Remove every cut staging directory under `dist` except `keep`, each ONLY when
/// `processes` proves nothing executes out of it. Returns one transcript line per
/// directory it looked at: removed, or kept and why.
///
/// `processes == None` removes nothing: "could not look" never licenses a delete.
/// A live directory is kept and named — the pre-claim gate already refused the cut
/// for it, so reaching here with one means a process started mid-cut, and the
/// answer is to leave it alone rather than pull a bundle out from under it.
pub fn prune_staging_dirs(
    dist: &Path,
    keep: &Path,
    processes: Option<&[RunningProcess]>,
) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dist) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| is_staging_dir_name(&entry.file_name().to_string_lossy()))
        .map(|entry| entry.path())
        .filter(|path| path != keep)
        .collect();
    dirs.sort();
    let Some(processes) = processes else {
        return dirs
            .iter()
            .map(|dir| {
                format!(
                    "kept {}: the process list could not be read, so nothing proves it unused",
                    dir.display()
                )
            })
            .collect();
    };
    let live = live_staging_dirs(dist, processes);
    dirs.iter()
        .map(|dir| {
            if let Some((_, pids)) = live.iter().find(|(live_dir, _)| live_dir == dir) {
                return format!(
                    "kept {}: pid {} runs out of it",
                    dir.display(),
                    pids.join(", ")
                );
            }
            match std::fs::remove_dir_all(dir) {
                Ok(()) => format!("pruned {} (nothing runs out of it)", dir.display()),
                Err(error) => format!("could not prune {}: {error}", dir.display()),
            }
        })
        .collect()
}

/// Remove this claim's staging directory once a real or rehearsed cut is done, so
/// no finished cut leaves a launchable copy of the app behind: a copy left in
/// `dist/` is offered by Spotlight for "aterm", launched through LaunchServices,
/// and then runs as a second copy under the release's bundle id (main 70ba9c074).
/// The DMG and the updater zip carry the same sealed bytes, and no later step reads
/// the bundle. A dry run does not call this: it keeps its bundle to inspect.
///
/// Removed only when the process list PROVES nothing runs out of it — the rule
/// [`prune_staging_dirs`] applies to older claims. "Could not look" and a live
/// process both leave it in place, and the `Err` says which; the caller warns,
/// because the release is already published and the next cut's prune retries.
pub fn discard_staged_app(dist: &Path, build: u64) -> Result<(), String> {
    discard_staged_dir(dist, build, running_processes().as_deref())
}

/// [`discard_staged_app`] over an injected process list, so its three answers are
/// tests.
fn discard_staged_dir(
    dist: &Path,
    build: u64,
    processes: Option<&[RunningProcess]>,
) -> Result<(), String> {
    let dir = dist.join(staging_dir_name(build));
    if !dir.exists() {
        return Ok(());
    }
    let Some(processes) = processes else {
        return Err(format!(
            "the process list could not be read, so nothing proves {} unused",
            dir.display()
        ));
    };
    if let Some((_, pids)) = live_staging_dirs(dist, processes)
        .into_iter()
        .find(|(live, _)| *live == dir)
    {
        return Err(format!(
            "pid {} runs out of {} (deleting a running bundle leaves the process with no \
             code identity macOS can check a privacy grant against)",
            pids.join(", "),
            dir.join("aterm.app").display()
        ));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| format!("rm -rf {}: {e}", dir.display()))
}

/// Assemble this claim's staging bundle, [`staged_app_path`] (build-app.sh steps
/// 2–6c). Returns the .app path. Signing is NOT done here — the caller runs `sign::`
/// next (inside-out), then dmg, then [`write_provenance`] (whose binary_sha256 must
/// cover the SIGNED bytes, so it must run after signing — same order as the script).
pub fn assemble(spec: &BundleSpec) -> Result<PathBuf, String> {
    let mac_dir = spec.repo_root.join("apps/aterm-mac");
    let app = staged_app_path(&spec.out_dir, spec.build_number);

    // NO `.metadata_never_index` MARKER, in dist/ or in the assembly directory. As a
    // Spotlight exclusion it is INERT — MEASURED 2026-09-02 on macOS 26.6.2 by A/B test
    // (crates/atpkg/src/noindex.rs): only a directory name ending `.noindex` (`aterm pkg
    // noindex`) keeps a subtree out, so an assembled bundle is indexed with or without it. Its
    // other job, a "build output, not an install" sentinel, had one reader — atpkg's
    // reclaim of a bundled toolchain seed — deleted with the sealed-seed client lane in
    // Phase 5 (docs/DESIGN-atpkg-vendor-direct-updates-2026-09-22.md §5.1). A marker
    // nothing reads is a claim with no witness, so none is written; the directories come
    // into being with the bundle below.
    // The Spotlight exclusion that does work is the per-claim staging directory's own
    // name, which ends `.noindex` ([`staging_dir_name`]).
    let staging_dir = spec.out_dir.join(staging_dir_name(spec.build_number));

    // NO LIVE BUNDLE UNDER THIS `rm -rf`. The pre-claim gate refused the cut if
    // anything ran out of a staging directory, but that answer is a build ago; ask
    // again at the moment of deleting. The directory is this claim's own, so it
    // exists here only on a resume that rebuilds — and a process running out of it
    // (someone launched the half-built bundle) is refused, never deleted under.
    let processes = running_processes();
    if app.exists() {
        match processes.as_deref() {
            None => {
                return Err(format!(
                    "cannot list running processes, so nothing proves {} unused — refusing \
                     to rm -rf it (a bundle deleted under a live process takes that \
                     process's TCC identity with it)",
                    app.display()
                ));
            }
            Some(list) => {
                if let Some((_, pids)) = live_staging_dirs(&spec.out_dir, list)
                    .into_iter()
                    .find(|(dir, _)| *dir == staging_dir)
                {
                    return Err(format!(
                        "pid {} is running out of {} — refusing to rm -rf a bundle a live \
                         process executes out of; quit it, then resume",
                        pids.join(", "),
                        app.display()
                    ));
                }
            }
        }
    }
    // Older staging directories go only when provably unused — see
    // [`prune_staging_dirs`]. Before this claim's own is created, so a failed build
    // leaves exactly one.
    for line in prune_staging_dirs(&spec.out_dir, &staging_dir, processes.as_deref()) {
        println!("==> {line}");
    }
    std::fs::create_dir_all(&staging_dir)
        .map_err(|e| format!("create {}: {e}", staging_dir.display()))?;

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
        // The fabric bridge. It is spawned BY NAME out of `[fabric] command`,
        // which aterm whitespace-splits and exec's, so the name has to exist as
        // an executable in a SHIPPED install — not only in `install.sh`. It was
        // added to the three script lists and missed here, which made the alias
        // work from a source checkout and silently not from a release.
        "aterm-link",
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

    // RETIRED 2026-08-26 (step 6d, the batteries-included toolchain seed sealed
    // under `Contents/Resources/toolchain-seed.lproj`; the client lane that read it
    // followed in Phase 5, 2026-09-23): aterm ships ONE lean
    // self-provisioning bundle — the client's own `atpkg` lane installs the
    // toolchain from the network on first launch, and nothing is sealed here.

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
    // `seed=no` is a CONSTANT since 2026-08-26 (the batteries-included seal is
    // retired): kept as a line rather than dropped so every existing consumer
    // of the provenance record keeps parsing the field it always found.
    let seed_lines = "seed=no\n";
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

#[cfg(test)]
mod alias_tests {
    /// THE ARGV0 ALIAS SET IS HAND-TYPED IN FIVE PLACES, and on 2026-09-12 an
    /// audit found `aterm-link` in four of them. This pins the bundle's copy
    /// against the OTHER lists in the repository, so the next verb to grow an
    /// alias cannot ship working from a source checkout and dead from a release.
    ///
    /// It reads the sibling files rather than importing a roster: `aterm-release`
    /// deliberately does not depend on `aterm-cli`, and a test that asserted a
    /// hard-coded list against a hard-coded list would prove only that one author
    /// typed the same thing twice.
    #[test]
    fn the_bundle_ships_every_alias_the_installers_create() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .expect("crates/aterm-release sits two levels under the root");
        let bundle = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/bundle.rs"),
        )
        .expect("this file");
        for script in [
            "tools/install.sh",
            "tools/atpkg-pack.sh",
            "tools/dev-app.sh",
        ] {
            let text = std::fs::read_to_string(root.join(script))
                .unwrap_or_else(|e| panic!("{script}: {e}"));
            let Some(line) = text.lines().find(|l| l.contains("for alias in aterm-cli")) else {
                panic!("{script} no longer spells its alias loop the way this test reads it");
            };
            for alias in line
                .split_whitespace()
                // `for alias in … aterm-gui; do` — the loop's own punctuation
                // rides on the last word.
                .map(|w| w.trim_end_matches(';'))
                .filter(|w| w.starts_with("aterm-") || *w == "atpkg")
            {
                assert!(
                    bundle.contains(&format!("\"{alias}\"")),
                    "{script} installs the `{alias}` argv0 alias and the app bundle does not \
                     ship it: the name works from a source checkout and is dead in a release"
                );
            }
        }
    }
}

#[cfg(test)]
mod staging_tests {
    //! PER-CLAIM STAGING (2026-09-23): each claim builds in its own
    //! `dist/cut-<build>.noindex/`, and an older staging directory is removed only
    //! when the process list PROVES nothing runs out of it.

    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aterm-release-staging-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dist");
        // Resolved, because the kernel reports resolved paths and /tmp is a symlink.
        std::fs::canonicalize(&dir).expect("canonical scratch dist")
    }

    fn bundle_in(dist: &Path, name: &str) -> PathBuf {
        let app = dist.join(name).join("aterm.app/Contents/MacOS");
        std::fs::create_dir_all(&app).expect("create staging bundle");
        dist.join(name)
    }

    #[test]
    fn the_staging_name_is_per_claim_and_recognised() {
        assert_eq!(
            staged_app_path(Path::new("/r/dist"), 1_790_000_000),
            PathBuf::from("/r/dist/cut-1790000000.noindex/aterm.app")
        );
        assert_ne!(
            staged_app_path(Path::new("/r/dist"), 1),
            staged_app_path(Path::new("/r/dist"), 2),
            "two claims never share a staging bundle"
        );
        for name in ["cut-1.noindex", "cut-1790000000.noindex"] {
            assert!(is_staging_dir_name(name), "{name}");
        }
        // The fixed directory the cut assembled in until 2026-09-23 is no longer
        // one: nothing reads, writes or prunes it, so a process running out of it
        // is never under this cutter's delete.
        for name in [
            "cut-app",
            "cut-state.toml",
            "cut-.noindex",
            "cut-12x.noindex",
            "cut-app-old",
            "aterm.app",
            "cut-1.noindex.bak",
        ] {
            assert!(!is_staging_dir_name(name), "{name}");
        }
    }

    #[test]
    fn pruning_removes_only_what_is_provably_unused() {
        let dist = scratch("prune");
        let keep = bundle_in(&dist, &staging_dir_name(3));
        let legacy = bundle_in(&dist, "cut-app");
        let old = bundle_in(&dist, &staging_dir_name(1));
        let live = bundle_in(&dist, &staging_dir_name(2));
        std::fs::write(dist.join("cut-state.toml"), "journal").unwrap();
        let dev = dist.join("aterm.app");
        std::fs::create_dir_all(&dev).unwrap();

        // "Could not look" removes NOTHING.
        let lines = prune_staging_dirs(&dist, &keep, None);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines.iter().all(|l| l.starts_with("kept ")), "{lines:?}");
        assert!(legacy.is_dir() && old.is_dir() && live.is_dir());

        let processes = vec![(
            "85619".to_string(),
            format!("{}/aterm.app/Contents/MacOS/aterm", live.display()),
        )];
        let lines = prune_staging_dirs(&dist, &keep, Some(&processes));
        assert!(
            legacy.is_dir(),
            "the retired fixed directory is not this cutter's to delete: {lines:?}"
        );
        assert!(!old.exists(), "an unused older claim is pruned: {lines:?}");
        assert!(live.is_dir(), "a live one is KEPT: {lines:?}");
        assert!(
            lines
                .iter()
                .any(|l| l.contains("kept") && l.contains("85619")),
            "{lines:?}"
        );
        assert!(keep.is_dir(), "this claim's own directory is never pruned");
        assert!(dist.join("cut-state.toml").is_file() && dev.is_dir());
        let _ = std::fs::remove_dir_all(&dist);
    }

    /// A finished cut takes its own staging directory with it — the bundle a
    /// launchable copy would otherwise be — but never from under a live process,
    /// and never on a process list it could not read.
    #[test]
    fn a_finished_cut_discards_its_staging_bundle_only_when_provably_unused() {
        let dist = scratch("discard");
        let own = bundle_in(&dist, &staging_dir_name(7));
        let other = bundle_in(&dist, &staging_dir_name(6));
        let exe = format!("{}/aterm.app/Contents/MacOS/aterm", own.display());

        let unknown = discard_staged_dir(&dist, 7, None);
        assert!(unknown.is_err() && own.is_dir(), "{unknown:?}");

        let live = vec![("4242".to_string(), exe)];
        let refused = discard_staged_dir(&dist, 7, Some(&live));
        assert!(
            refused.as_ref().is_err_and(|e| e.contains("4242")) && own.is_dir(),
            "{refused:?}"
        );

        assert_eq!(discard_staged_dir(&dist, 7, Some(&[])), Ok(()));
        assert!(!own.exists(), "the finished claim's bundle is gone");
        assert!(
            other.is_dir(),
            "another claim's directory is the prune's, not this"
        );
        assert_eq!(
            discard_staged_dir(&dist, 7, Some(&[])),
            Ok(()),
            "nothing left to discard is not a failure"
        );
        let _ = std::fs::remove_dir_all(&dist);
    }
}
