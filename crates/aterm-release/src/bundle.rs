// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! .app assembly (release spec §6 `bundle.rs`): lay out the release's bundle
//! ([`assemble_at`], once, by the build: `artifact.rs`) from
//! the `apps/aterm-mac/Info.plist` template via in-process string substitution
//! (CFBundleShortVersionString, sealed `CFBundleVersion = n`, ATermGitCommit — the
//! release commit's short id, never dirty: the build reads git objects), copy the static
//! resources (Help.html, Credits.html, aterm.icns) and the
//! successor's handoff policy (`publish/handoff-policy.toml`), nest
//! atpkg + aterm-ctl + aterm-cli in Contents/MacOS; prepare this claim's staging
//! directory, `dist/cut-<build>.noindex/` ([`prepare_staging`]), which the cut
//! copies the verified bundle into to sign; and write the
//! `dist/aterm-<ver>-build.txt` provenance record. No `.metadata_never_index` marker
//! (see [`prepare_staging`]: inert for Spotlight, and its one reader is deleted).
//!
//! Port of the layout phase of the retired `apps/aterm-mac/build-app.sh`
//! (steps 2–6c + 8) — that script was deleted with the shell pipeline and is
//! not in this tree.
//! PlistBuddy is replaced by [`stamp_info_plist`] — pure string substitution
//! on the committed template, unit-tested against goldens in
//! `tests/it/plist_stamp.rs` — so the stamp is deterministic and testable off-mac.
//! The binaries arrive PRE-stripped from `buildplan::run` (strip -x is that
//! module's charter); Credits.html is the committed
//! `apps/aterm-mac/Credits.html` (extracted from build-app.sh's heredoc),
//! copied like every other resource instead of being generated inline.

use std::path::{Path, PathBuf};

/// The release bundle's `CFBundleIdentifier` — the identifier a Developer-ID
/// signing/notarization profile binds to.
pub const BUNDLE_ID: &str = "com.aterm.aterm";

/// The argv0 aliases every bundle carries in `Contents/MacOS`, each a symlink onto the
/// one `aterm` binary, which dispatches on the name it is run as. The list is
/// `aterm_types::argv0::ALIASES`, the one every Rust reader shares.
pub use aterm_types::argv0::ALIASES as ARGV0_ALIASES;

/// Everything [`assemble_at`] + [`write_provenance`] need, resolved by the caller.
pub struct BundleSpec {
    /// The tree the bundle's resources come from (the apps/aterm-mac templates, the
    /// handoff policy): the build's snapshot of the release commit's objects.
    pub repo_root: PathBuf,
    /// `dist/` — receives the build.txt provenance record, beside the dSYM
    /// [`write_provenance`] looks for.
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
    /// ATermGitCommit stamp — short=12, the spelling the build pins as the binary's
    /// own ATERM_GIT_COMMIT, so the plist and the binary agree byte-for-byte.
    pub git_commit: String,
    /// The ship-ready (universal, stripped) binary from `buildplan::run`.
    pub aterm_bin: PathBuf,
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
pub(crate) fn set_plist_string(plist: &str, key: &str, value: &str) -> Result<String, String> {
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
/// never mistaken for one — and neither is the retired fixed `dist/cut-app/`, so a
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
/// A fixed assembly path once let the running aterm adopt a half-built bundle, and
/// later put the owner's aterm under the next cut's `rm -rf` (which is how macOS
/// reset its TCC grants). What keeps a live process out from under the cut is
/// four things together:
///
/// 1. the name is unique per claim ([`staging_dir_name`]), so building never has
///    to remove anybody else's bundle;
/// 2. it ends `.noindex`, so the bundle is not offered by Spotlight or Launchpad;
/// 3. the pre-claim gate (`gates::staged_bundle_liveness_gate`) refuses the cut —
///    for free, before a build number is burned — while any process runs out of
///    ANY staging directory, or when the process list cannot be read;
/// 4. [`prepare_staging`] re-asks at the moment it would `rm -rf` its own
///    directory (a resume that copies the verified bundle in again), and
///    [`prune_staging_dirs`] removes an older directory only when the process list
///    PROVES nothing runs from it.
///
/// Every step after `sign` reads the bundle from here too; nothing copies it
/// anywhere else (a release installs from the DMG; the dev app is `ship
/// dev-app`'s). Once a real cut is done,
/// [`discard_staged_app`] removes this claim's staging directory too (under the
/// same liveness rule), so no finished cut leaves a launchable copy of the app
/// behind; a dry run keeps it to inspect.
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
/// running from somewhere else. And not `std::env::current_exe()`, which on macOS is the
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

/// Remove this claim's staging directory once a real cut is done, so
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

/// Prepare THIS claim's staging directory and return where its bundle goes,
/// [`staged_app_path`] — absent on return: the cut copies the verified bundle
/// there to sign it. Older staging directories go only when provably unused
/// ([`prune_staging_dirs`]), before this claim's own is made, so a failed cut
/// leaves exactly one.
///
/// NO LIVE BUNDLE UNDER THIS `rm -rf`. The pre-claim gate refused the cut if
/// anything ran out of a staging directory, but that answer is a build ago; this
/// asks again at the moment of deleting. The directory is this claim's own, so it
/// holds a bundle here only on a resume that signs again — and a process running
/// out of it (someone launched the half-signed bundle) is refused, never deleted
/// under.
///
/// NO `.metadata_never_index` MARKER: as a Spotlight exclusion it is inert
/// (MEASURED on macOS 26.6.2 by atpkg's since-deleted noindex probe) and nothing
/// else reads it. The exclusion that works is the staging directory's `.noindex`
/// name ([`staging_dir_name`]).
pub fn prepare_staging(dist: &Path, build: u64) -> Result<PathBuf, String> {
    let app = staged_app_path(dist, build);
    let staging_dir = dist.join(staging_dir_name(build));
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
                if let Some((_, pids)) = live_staging_dirs(dist, list)
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
    for line in prune_staging_dirs(dist, &staging_dir, processes.as_deref()) {
        println!("==> {line}");
    }
    std::fs::create_dir_all(&staging_dir)
        .map_err(|e| format!("create {}: {e}", staging_dir.display()))?;
    if app.exists() {
        std::fs::remove_dir_all(&app).map_err(|e| format!("rm -rf {}: {e}", app.display()))?;
    }
    Ok(app)
}

/// Lay out the release's bundle at `app` (build-app.sh steps 2–6c), which must not
/// exist yet. Signing is NOT done here: the build lays out an unsigned bundle once,
/// `verify release` judges it, and the cut signs a byte copy of it (inside-out), then
/// packages it and runs [`write_provenance`] (whose binary_sha256 must cover the
/// SIGNED bytes, so it must run after signing — same order as the script).
pub fn assemble_at(spec: &BundleSpec, app: &Path) -> Result<(), String> {
    let mac_dir = spec.repo_root.join("apps/aterm-mac");
    if app.exists() {
        return Err(format!(
            "{} already exists — a bundle is laid out once, into a fresh place",
            app.display()
        ));
    }

    // --- 2. lay out the bundle -------------------------------------------
    println!("==> assembling {}", app.display());
    let macos = app.join("Contents/MacOS");
    let resources = app.join("Contents/Resources");
    std::fs::create_dir_all(&macos).map_err(|e| format!("create {}: {e}", macos.display()))?;
    std::fs::create_dir_all(&resources)
        .map_err(|e| format!("create {}: {e}", resources.display()))?;

    // --- 3. THE executable (pre-stripped by buildplan; symbols in the dSYM) —
    // plus argv0 compat SYMLINKS. One Mach-O carries the window, the session,
    // and every verb; the symlinks keep every pre-one-binary name resolving
    // (old installs' ~/.local/bin/aterm -> aterm-cli, in-session `aterm-ctl`
    // scripts, the drive client's sibling lookup, direct atpkg calls).
    // The binary dispatches on argv[0], so each alias IS that tool. Symlinks
    // are not Mach-Os: nothing extra to sign, and the sealed bundle covers
    // them as resources.
    copy_exe(&spec.aterm_bin, &macos.join("aterm"))?;
    #[cfg(unix)]
    for alias in ARGV0_ALIASES {
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

    // --- 6e. the successor's handoff policy (plan P0-5) ----------------------
    // HERE, BEFORE `sign::sign_app`, and that order is the whole point: the
    // signature seals `Contents/`, so the file the outgoing build reads from
    // this bundle — only after this bundle passed its codesign check — is the
    // one this cut wrote. Copied byte for byte from the checked-in source,
    // after the same strict read the pre-claim gate ran.
    let policy = place_handoff_policy(&spec.repo_root, app)?;
    println!("    handoff policy: {policy}");

    Ok(())
}

/// Read the checked-in handoff policy
/// ([`aterm_update_core::handoff_policy::SOURCE_PATH`]) the way the cutter
/// must: strictly — an unknown key is a typo that would ship as a policy no
/// producer follows, and the reserved `seamless` key is refused by name — and
/// return its exact text with what it says. Shared by the pre-claim gate and
/// the bundle step, so both judge the same bytes by the same rule.
pub fn handoff_policy_for_cut(
    repo_root: &Path,
) -> Result<(String, aterm_update_core::handoff_policy::PolicyFile), String> {
    let path = repo_root.join(aterm_update_core::handoff_policy::SOURCE_PATH);
    let shown = path.display().to_string();
    let text = std::fs::read_to_string(&path)
        .map_err(|e| handoff_policy_unreadable(&format!("read {shown}: {e}")))?;
    let policy = handoff_policy_of_text(&text, &shown)?;
    Ok((text, policy))
}

/// The strict read [`handoff_policy_for_cut`] applies, over the policy's TEXT;
/// `shown` names where it came from in a refusal. `ship check` feeds it the
/// blob a commit holds (`gates::handoff_policy_gate_at`), so the check, the
/// pre-claim gate and the bundle step judge by one rule.
pub fn handoff_policy_of_text(
    text: &str,
    shown: &str,
) -> Result<aterm_update_core::handoff_policy::PolicyFile, String> {
    use aterm_update_core::handoff_policy::{MAX_POLICY_BYTES, PolicyFile};
    if u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_POLICY_BYTES {
        return Err(format!(
            "{shown} is {} bytes; producers ignore a policy over {MAX_POLICY_BYTES}",
            text.len()
        ));
    }
    PolicyFile::parse_for_cut(text)
        .map_err(|why| format!("{shown} is not a policy producers can follow: {why}"))
}

/// A policy that could not be read at all (`why`), with what every release
/// needs instead.
pub fn handoff_policy_unreadable(why: &str) -> String {
    format!(
        "{why} — every release seals a handoff policy into its bundle (empty is `schema = 1` \
         alone)"
    )
}

/// Write the checked-in handoff policy into the bundle at
/// [`aterm_update_core::handoff_policy::BUNDLE_PATH`], byte for byte, after
/// [`handoff_policy_for_cut`] judged it. Returns what it says.
pub fn place_handoff_policy(
    repo_root: &Path,
    app: &Path,
) -> Result<aterm_update_core::handoff_policy::PolicyFile, String> {
    let (text, policy) = handoff_policy_for_cut(repo_root)?;
    let dest = app.join(aterm_update_core::handoff_policy::BUNDLE_PATH);
    std::fs::write(&dest, text.as_bytes()).map_err(|e| format!("write {}: {e}", dest.display()))?;
    Ok(policy)
}

/// The bundle executable's mode: runnable, and writable by nobody. `~/.local/bin/aterm` is
/// a link to this file, so a `cp build ~/.local/bin/aterm` with a write bit here opens the
/// RUNNING app's own inode: the kernel kills every aterm window (CODESIGNING, Invalid
/// Page) and a copy cut short with them leaves an app that cannot start (2026-10-19).
/// Without one, `cp` is refused and `cp -f` replaces the link instead. Every later step
/// keeps it: codesign writes a new inode in the same mode, `cp -RX` and `ditto` carry it
/// into the DMG and the zip, and the updater only renames and deletes whole bundles. The
/// mode is not part of the code seal (measured: `codesign --verify --deep --strict` and
/// `spctl` pass). Never the directory: the updater's cross-parent bundle renames and
/// every `remove_dir_all` of a bundle need `Contents/MacOS` writable.
#[cfg(unix)]
const EXE_MODE: u32 = 0o555;

fn copy_exe(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::copy(src, dst)
        .map_err(|e| format!("copy {} -> {}: {e}", src.display(), dst.display()))?;
    // .app bundles are only assembled on Unix hosts; the exec bit has no
    // Windows equivalent, so the chmod is Unix-gated for cross-compilation.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dst, std::fs::Permissions::from_mode(EXE_MODE))
            .map_err(|e| format!("chmod {}: {e}", dst.display()))?;
    }
    Ok(())
}

/// Write `dist/aterm-<ver>-build.txt` — the per-artifact provenance record
/// (build-app.sh step 8, same KEY=value fields in the same order). MUST run
/// AFTER signing: binary_sha256 covers the shipped Contents/MacOS/aterm
/// bytes, and codesign rewrites them. `pre_sign_sha256` is the digest of those
/// bytes BEFORE signing — the executable `verify release` judged (build once): the record
/// binds what shipped to what was verified.
pub fn write_provenance(
    spec: &BundleSpec,
    app: &Path,
    signed_by: &str,
    pre_sign_sha256: &str,
) -> Result<PathBuf, String> {
    let shipped = app.join("Contents/MacOS/aterm");
    // In-process sha256 (aterm-digest): the digest on record is provably the digest of
    // the bytes on disk, not of whatever a shelled hasher happened to read.
    let binary_sha256 = crate::dmg::sha256_file(&shipped)?;
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
    // `seed=no` is a constant (no toolchain seed ships), kept so every consumer of
    // the provenance record still finds the field.
    let seed_lines = "seed=no\n";
    let body = format!(
        "name=aterm\n\
         version={}\n\
         build={}\n\
         commit={}\n\
         built={}\n\
         bundle_id={}\n\
         binary_sha256={}\n\
         pre_sign_sha256={pre_sign_sha256}\n\
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

#[cfg(test)]
mod staging_tests {
    //! PER-CLAIM STAGING: each claim builds in its own
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
        // The retired fixed `cut-app` is not one: a process running out of it is
        // never under this cutter's delete.
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
        prune_staging_dirs(&dist, &keep, None);
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
        assert!(refused.is_err() && own.is_dir(), "{refused:?}");

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

#[cfg(all(test, unix))]
mod exe_mode_tests {
    //! WRITABLE BY NOBODY: a copy aimed at `aterm` on PATH never lands in the installed
    //! app's own executable.

    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// The laid executable is exactly r-x, so a copy through a link to it (what
    /// `cp build ~/.local/bin/aterm` is) is refused and leaves its bytes, and the bundle
    /// holding it still deletes whole, as the updater and `ship dev-app` delete bundles.
    #[test]
    fn a_copy_through_a_link_to_the_bundle_executable_never_rewrites_it() {
        let scratch = aterm_tempfile::TempDir::new().expect("owned bundle scratch");
        let dir = scratch.path();
        let macos = dir.join("aterm.app/Contents/MacOS");
        std::fs::create_dir_all(&macos).expect("create bundle");
        let built = dir.join("built");
        std::fs::write(&built, b"release").expect("write build");
        let exe = macos.join("aterm");
        copy_exe(&built, &exe).expect("copy_exe");
        let mode = std::fs::metadata(&exe).expect("stat").permissions().mode();
        assert_eq!(mode & 0o7777, EXE_MODE, "{mode:o}");

        let link = dir.join("aterm");
        std::os::unix::fs::symlink(&exe, &link).expect("link");
        let debug = dir.join("debug-aterm");
        std::fs::write(&debug, b"a debug build").expect("write debug build");
        let refused = std::fs::copy(&debug, &link).expect_err("a write through the link");
        assert_eq!(refused.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(std::fs::read(&exe).expect("read"), b"release");

        std::fs::remove_dir_all(dir.join("aterm.app")).expect("the bundle deletes whole");
    }
}
