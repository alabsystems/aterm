// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Taps (`Library/Taps`): git clones with formula `.rb` files parsed as data.

use std::path::PathBuf;

use super::fetch;
use super::formula::{Artifact, Cask, Formula};
use super::prefix::{Kind, Prefix};

/// One tap on disk: `user/repo` (repo without the `homebrew-` prefix).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tap {
    pub user: String,
    pub repo: String,
    pub path: PathBuf,
}

impl Tap {
    #[must_use]
    pub fn name(&self) -> String {
        format!("{}/{}", self.user, self.repo)
    }
}

/// Parse `user/repo` or `user/homebrew-repo`.
#[must_use]
pub fn parse_tap_name(s: &str) -> Option<(String, String)> {
    let (user, repo) = s.split_once('/')?;
    let repo = repo.strip_prefix("homebrew-").unwrap_or(repo);
    (super::valid_component(user) && super::valid_component(repo))
        .then(|| (user.to_owned(), repo.to_owned()))
}

/// The tap `old_tap`'s `tap_migrations.json` says `oldname`
/// moved to, when that tap is installed locally (brew's
/// `old_tap.tap_migrations`). Returns the target tap as
/// `user/repo`.
pub(super) fn migration_target_tap(
    prefix: &Prefix,
    old_tap: &str,
    oldname: &str,
) -> Option<String> {
    let (user, repo) = parse_tap_name(old_tap)?;
    let dir = tap_path(prefix, &user, &repo).ok()?;
    let bytes =
        crate::metadata_io::read_bounded_regular(&dir.join("tap_migrations.json"), 1024 * 1024)
            .ok()?;
    let value: aterm_json::Value = aterm_json::from_slice(&bytes).ok()?;
    let target = value.get(oldname)?.as_str()?;
    // `Utils.tap_from_full_name`: a full name's first two parts,
    // else a two-part tap as written.
    let parts: Vec<&str> = target.split('/').collect();
    match parts.len() {
        2 => Some(target.to_string()),
        3.. => Some(format!("{}/{}", parts[0], parts[1])),
        _ => None,
    }
}

fn tap_path(prefix: &Prefix, user: &str, repo: &str) -> Result<PathBuf, String> {
    if !super::valid_component(user) || !super::valid_component(repo) {
        return Err("abrew: invalid tap path component".to_owned());
    }
    let user_path = prefix.taps_dir().join(user);
    let path = user_path.join(format!("homebrew-{repo}"));
    for dir in [
        prefix.root.clone(),
        prefix.root.join("Library"),
        prefix.taps_dir(),
        user_path,
        path.clone(),
    ] {
        match std::fs::symlink_metadata(&dir) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => {
                return Err(format!(
                    "abrew: tap directory is not a real directory: {}",
                    dir.display()
                ));
            }
        }
    }
    Ok(path)
}

fn mutation_path(
    prefix: &Prefix,
    user: &str,
    repo: &str,
    create: bool,
) -> Result<(PathBuf, bool), String> {
    let path = tap_path(prefix, user, repo)?;
    let parent = prefix.taps_dir().join(user);
    for dir in [
        prefix.root.clone(),
        prefix.root.join("Library"),
        prefix.taps_dir(),
        parent,
    ] {
        if create {
            match std::fs::create_dir(&dir) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => {
                    return Err(format!(
                        "abrew: cannot create tap parent {}: {e}",
                        dir.display()
                    ));
                }
            }
        }
        match std::fs::symlink_metadata(&dir) {
            Ok(m) if m.is_dir() => {}
            _ => {
                return Err(format!(
                    "abrew: tap parent must be a physical directory: {}",
                    dir.display()
                ));
            }
        }
    }
    match std::fs::symlink_metadata(&path) {
        Ok(m) if m.is_dir() => {}
        Err(e) if create && e.kind() == std::io::ErrorKind::NotFound => return Ok((path, false)),
        _ => {
            return Err(format!(
                "abrew: tap must be a physical directory: {}",
                path.display()
            ));
        }
    }
    let update = if create {
        match std::fs::symlink_metadata(path.join(".git")) {
            Ok(m) if m.is_dir() => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            _ => {
                return Err(format!(
                    "abrew: tap repository must be a physical directory: {}",
                    path.display()
                ));
            }
        }
    } else {
        false
    };
    Ok((path, update))
}

fn is_local_remote(remote: &str) -> bool {
    remote.starts_with('/')
        || remote.starts_with("./")
        || remote.starts_with("../")
        || remote.starts_with("~/")
        || remote.starts_with("file://")
}

fn git_protocol_args(local: bool) -> Vec<&'static str> {
    let mut args = vec![
        "-c",
        "protocol.allow=never",
        "-c",
        "protocol.https.allow=always",
        "-c",
        "protocol.ssh.allow=always",
    ];
    if local {
        args.extend(["-c", "protocol.file.allow=always"]);
    }
    args
}

const DEPRECATED_OFFICIAL_TAPS: &[&str] = &[
    "aliases",
    "apache",
    "binary",
    "bundle",
    "cask-drivers",
    "cask-eid",
    "cask-fonts",
    "cask-versions",
    "command-not-found",
    "completions",
    "devel-only",
    "dupes",
    "emacs",
    "formula-analytics",
    "fuse",
    "games",
    "gui",
    "head-only",
    "linux-fonts",
    "livecheck",
    "nginx",
    "php",
    "portable-ruby",
    "python",
    "science",
    "services",
    "test-bot",
    "tex",
    "versions",
    "x11",
];

/// Every target of git's `warning: redirecting to <url>` lines, as
/// brew's `GIT_REDIRECT_REMOTE_REGEX` captures them. ASCII case-folding
/// preserves UTF-8 byte offsets when slicing `line` after a non-ASCII prefix.
fn redirected_remotes(stderr: &str) -> impl DoubleEndedIterator<Item = &str> {
    stderr.lines().filter_map(|line| {
        let lower = line.to_ascii_lowercase();
        let at = lower.find("redirecting to ")? + "redirecting to ".len();
        line.get(at..)?.split_whitespace().next()
    })
}

/// The final redirected remote that differs from `current` (for example after an
/// initial `repo` -> `repo.git/` hop followed by a cross-repo redirect), or any
/// intermediate hop that left `current`.
fn unsafe_redirect(current: &str, stderr: &str) -> Option<String> {
    redirected_remotes(stderr)
        .rfind(|remote| !same_remote(current, remote))
        .map(str::to_string)
}

/// Brew's `Tap.normalize_remote`, reduced to the forms a tap
/// remote actually takes: case-fold, canonicalize GitHub SSH
/// forms to HTTPS, drop a trailing slash or `.git`.
fn normalize_remote(remote: &str) -> String {
    let mut out = remote.trim().to_lowercase();
    for prefix in [
        "git@github.com:",
        "ssh://git@github.com/",
        "git://github.com/",
    ] {
        if let Some(rest) = out.strip_prefix(prefix) {
            out = format!("https://github.com/{rest}");
            break;
        }
    }
    let trimmed = out.trim_end_matches('/');
    trimmed
        .strip_suffix(".git")
        .unwrap_or(trimmed)
        .trim_end_matches('/')
        .to_string()
}

fn same_remote(a: &str, b: &str) -> bool {
    let na = normalize_remote(a);
    !na.is_empty() && na == normalize_remote(b)
}

/// Without brew's trust machinery no redirect can be proven
/// safe, so every redirect takes brew's unsafe-redirect
/// refusal verbatim (its remedy — untap, then tap the new
/// location — is native).
fn redirect_refusal(name: &str, remote: &str) -> String {
    format!(
        "Error: Refusing automatic redirect from {name} to {remote}.\n\
         Untap {name}, then tap the new location directly to review its source and trust."
    )
}

fn is_shallow(path: &std::path::Path) -> bool {
    git_out(path, &["rev-parse", "--is-shallow-repository"]).is_ok_and(|out| out == "true")
}

/// Clone or verify an installed tap, returning `(tap, newly_tapped)`.
/// Mirrors brew's `Tap#install` decision matrix.
pub fn add(
    prefix: &Prefix,
    name: &str,
    url: Option<&str>,
    custom_remote: bool,
    force: bool,
) -> Result<(Tap, bool), String> {
    let (user, repo) = parse_tap_name(name)
        .ok_or_else(|| format!("abrew: tap name must be user/repo, got {name}"))?;
    let shown = format!("{user}/{repo}");
    let user_l = user.to_ascii_lowercase();
    let repo_l = repo.to_ascii_lowercase();
    if user_l == "homebrew" && DEPRECATED_OFFICIAL_TAPS.contains(&repo_l.as_str()) {
        return Err(format!(
            "Error: {shown} was deprecated. This tap is now empty and all its contents were either deleted or migrated."
        ));
    }
    if user_l == "caskroom" || (user_l == "phinze" && repo_l == "cask") {
        let new_repo = if repo_l == "cask" {
            "cask".to_string()
        } else {
            format!("cask-{repo}")
        };
        return Err(format!(
            "Error: {shown} was moved. Tap homebrew/{new_repo} instead."
        ));
    }
    if custom_remote && url.is_none() {
        return Err(format!(
            "Error: Tap {shown} with option `--custom-remote` but without a remote URL."
        ));
    }
    let (path, update) = mutation_path(prefix, &user, &repo, true)?;
    if update {
        let tap = Tap {
            user: user.clone(),
            repo: repo.clone(),
            path: path.clone(),
        };
        let existing = git_out(&path, &["remote", "get-url", "origin"])
            .ok()
            .filter(|s| !s.is_empty());
        if !custom_remote
            && let Some(want) = url
            && existing.as_deref() != Some(want)
        {
            return Err(format!(
                "Error: Tap {shown} remote mismatch.\n{} != {want}",
                existing.as_deref().unwrap_or_default()
            ));
        }
        if custom_remote
            && let Some(want) = url
            && existing.as_deref() != Some(want)
        {
            fix_remote_configuration(&tap, Some(want))?;
        }
        let _ = git_out(&path, &["config", "--unset", "homebrew.forceautoupdate"]);
        if is_shallow(&path) {
            eprintln!("{}", super::tty::ohai_err(&format!("Unshallowing {shown}")));
            let local = existing.as_deref().or(url).is_some_and(is_local_remote);
            let path_s = path.to_string_lossy();
            let mut args = git_protocol_args(local);
            args.extend(["-C", &path_s, "fetch", "--unshallow"]);
            let fetched = fetch::run_capture(aterm_uds::os_path!("git"), &args)
                .map_err(|e| format!("abrew: git fetch failed for tap {shown}: {e}"))?;
            if !fetched.status.success() {
                return Err(format!("abrew: git fetch failed for tap {shown}"));
            }
            let stderr = String::from_utf8_lossy(&fetched.stderr);
            let current = git_out(&path, &["remote", "get-url", "origin"]).unwrap_or_default();
            if let Some(remote) = unsafe_redirect(&current, &stderr) {
                return Err(redirect_refusal(&shown, &remote));
            }
        }
        return Ok((tap, false));
    }
    let core = (user_l == "homebrew" || user_l == "linuxbrew")
        && (repo_l == "core" || repo_l == "homebrew");
    let core_cask = user_l == "homebrew" && repo_l == "cask";
    if (core || core_cask) && !force {
        return Err(format!(
            "Error: Tapping {shown} is no longer typically necessary.\nAdd --force if you are sure you need it for contributing to Homebrew."
        ));
    }
    {
        let remote = url.map_or_else(
            || format!("https://github.com/{user}/homebrew-{repo}.git"),
            str::to_owned,
        );
        eprintln!(
            "{}",
            super::tty::ohai_err(&format!("Tapping {user}/{repo} ({remote})"))
        );
        let parent = path
            .parent()
            .ok_or_else(|| "abrew: tap directory has no parent".to_owned())?;
        let work = aterm_tempfile::TempDir::new_in(parent)
            .map_err(|e| format!("abrew: cannot create tap staging directory: {e}"))?;
        let staged = work.path().join("tap");
        let staged_s = staged.to_string_lossy();
        let mut git_args: Vec<&str> = git_protocol_args(is_local_remote(&remote));
        git_args.extend(["clone", "--depth=1", "--", &remote, &staged_s]);
        let code = fetch::run_inherit(aterm_uds::os_path!("git"), &git_args);
        if code != 0 {
            return Err(format!(
                "abrew: git clone failed for tap {user}/{repo} ({code})"
            ));
        }
        super::prefix::publish_directory(&staged, &path, false)
            .map_err(|e| format!("abrew: cannot publish tap {user}/{repo}: {e}"))?;
        link_completions_and_manpages(
            prefix,
            &Tap {
                user: user.clone(),
                repo: repo.clone(),
                path: path.clone(),
            },
        );
    }
    Ok((Tap { user, repo, path }, true))
}

/// Update a discovered tap after validating its prefix-relative path.
pub(super) fn update(prefix: &Prefix, tap: &Tap) -> Result<(), String> {
    let (path, _) = mutation_path(prefix, &tap.user, &tap.repo, false)?;
    if !std::fs::symlink_metadata(path.join(".git")).is_ok_and(|m| m.is_dir()) {
        return Err(format!(
            "abrew: tap repository must be a physical directory: {}",
            path.display()
        ));
    }
    pull(&path, &tap.user, &tap.repo)
}

fn pull(path: &std::path::Path, user: &str, repo: &str) -> Result<(), String> {
    eprintln!(
        "{}",
        super::tty::ohai_err(&format!("Updating tap {user}/{repo}"))
    );
    let local = fetch::run_capture(
        aterm_uds::os_path!("git"),
        &["-C", &path.to_string_lossy(), "remote", "get-url", "origin"],
    )
    .is_ok_and(|out| is_local_remote(String::from_utf8_lossy(&out.stdout).trim()));
    let path_s = path.to_string_lossy();
    let mut git_args: Vec<&str> = git_protocol_args(local);
    git_args.extend(["-C", &path_s, "pull", "--ff-only"]);
    let code = fetch::run_inherit(aterm_uds::os_path!("git"), &git_args);
    if code != 0 {
        return Err(format!(
            "abrew: git pull failed for tap {user}/{repo} ({code})"
        ));
    }
    Ok(())
}

/// The tap's origin remote URL, when it is a git checkout.
/// brew's dump prints it when it differs from the GitHub default.
pub(super) fn remote_url(tap: &Tap) -> Option<String> {
    git_out(&tap.path, &["config", "--get", "remote.origin.url"])
        .ok()
        .filter(|s| !s.is_empty())
}

/// brew's `Tap#default_remote` for `user/repo`.
pub(super) fn default_remote(user: &str, repo: &str) -> String {
    format!("https://github.com/{user}/homebrew-{repo}")
}

fn git_out(path: &std::path::Path, args: &[&str]) -> Result<String, String> {
    let path_s = path.to_string_lossy();
    let mut full: Vec<&str> = vec!["-C", &path_s];
    full.extend(args);
    let out = fetch::run_capture(aterm_uds::os_path!("git"), &full).map_err(|e| {
        format!(
            "abrew: git {} failed: {e}",
            args.first().copied().unwrap_or("")
        )
    })?;
    if !out.status.success() {
        return Err(format!(
            "abrew: git {} failed in {}",
            args.first().copied().unwrap_or(""),
            path.display()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn origin_branch_name(path: &std::path::Path) -> Option<String> {
    let refname = git_out(path, &["symbolic-ref", "-q", "refs/remotes/origin/HEAD"]).ok()?;
    refname
        .strip_prefix("refs/remotes/origin/")
        .map(str::to_owned)
        .filter(|name| !name.is_empty())
}

/// brew's `Tap#fix_remote_configuration`: point the tap at a
/// requested remote when given, and follow an upstream
/// default-branch rename. A repository redirect (brew renames
/// the tap through its trust machinery) is refused before any
/// rename.
fn fix_remote_configuration(tap: &Tap, requested_remote: Option<&str>) -> Result<(), String> {
    let path = &tap.path;
    if let Some(want) = requested_remote {
        let previous = git_out(path, &["remote", "get-url", "origin"]).unwrap_or_default();
        git_out(
            path,
            &["remote", "set-url", "origin", "--end-of-options", want],
        )?;
        git_out(
            path,
            &[
                "config",
                "remote.origin.fetch",
                "+refs/heads/*:refs/remotes/origin/*",
            ],
        )?;
        eprintln!(
            "{}",
            super::tty::ohai_err(&format!(
                "{}: changed remote from {previous} to {want}",
                tap.name()
            ))
        );
    }
    let Ok(remote) = git_out(path, &["remote", "get-url", "origin"]) else {
        return Ok(());
    };
    if remote.is_empty() {
        return Ok(());
    }
    let local = is_local_remote(&remote);
    let path_s = path.to_string_lossy();
    let current = origin_branch_name(path);
    if requested_remote.is_none()
        && let Some(branch) = &current
    {
        let mut args = git_protocol_args(local);
        args.extend(["-C", &path_s, "ls-remote", "--heads", "origin", branch]);
        let listed = fetch::run_capture(aterm_uds::os_path!("git"), &args)
            .map_err(|e| format!("abrew: git ls-remote failed for tap {}: {e}", tap.name()))?;
        if listed.status.success() && !listed.stdout.is_empty() {
            return Ok(());
        }
    }
    let mut args = git_protocol_args(local);
    args.extend([
        "-C",
        &path_s,
        "fetch",
        "origin",
        "+refs/heads/*:refs/remotes/origin/*",
    ]);
    let fetched = fetch::run_capture(aterm_uds::os_path!("git"), &args)
        .map_err(|e| format!("abrew: git fetch failed for tap {}: {e}", tap.name()))?;
    if !fetched.status.success() {
        return Err(format!("abrew: git fetch failed for tap {}", tap.name()));
    }
    let stderr = String::from_utf8_lossy(&fetched.stderr);
    let current_remote = git_out(path, &["remote", "get-url", "origin"]).unwrap_or_default();
    if let Some(remote) = unsafe_redirect(&current_remote, &stderr) {
        return Err(redirect_refusal(&tap.name(), &remote));
    }
    git_out(path, &["remote", "set-head", "origin", "--auto"])?;
    let Some(new_branch) = origin_branch_name(path) else {
        return Err(format!(
            "Error: Could not determine the default branch of {}",
            tap.name()
        ));
    };
    let current = current.unwrap_or_else(|| new_branch.clone());
    if new_branch == current {
        return Ok(());
    }
    git_out(
        path,
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    )?;
    git_out(path, &["branch", "-m", &current, &new_branch])?;
    git_out(
        path,
        &["branch", "-u", &format!("origin/{new_branch}"), &new_branch],
    )?;
    eprintln!(
        "{}",
        super::tty::ohai_err(&format!(
            "{}: changed default branch name from {current} to {new_branch}!",
            tap.name()
        ))
    );
    Ok(())
}

/// `brew tap --repair`: relink every tap's manpages and
/// completions, then repair its remote configuration.
pub(super) fn repair(prefix: &Prefix) -> Result<(), String> {
    for tap in list(prefix) {
        link_completions_and_manpages(prefix, &tap);
        fix_remote_configuration(&tap, None)?;
    }
    Ok(())
}

fn tap_installed_names(
    prefix: &Prefix,
    user: &str,
    repo: &str,
) -> Result<(Vec<String>, Vec<String>), String> {
    let mut by_kind = (Vec::new(), Vec::new());
    for (kind, out) in [
        (Kind::Formula, &mut by_kind.0),
        (Kind::Cask, &mut by_kind.1),
    ] {
        for inst in prefix.installed_checked(kind)? {
            for version in &inst.versions {
                let full = prefix.full_name_checked(kind, &inst.name, version)?;
                let (tap, _) = full.rsplit_once('/').ok_or_else(|| {
                    format!("abrew: installed package has no tap identity: {full}")
                })?;
                let (owner, source) = parse_tap_name(tap)
                    .ok_or_else(|| format!("abrew: invalid installed tap identity: {full}"))?;
                if owner == user && source == repo {
                    out.push(format!("{user}/{repo}/{}", inst.name));
                }
            }
        }
        out.sort();
        out.dedup();
    }
    Ok(by_kind)
}

/// brew `TapUnavailableError` for an untapped name.
fn unavailable_error(user: &str, repo: &str) -> String {
    let name = format!("{user}/{repo}");
    if user == "homebrew" && matches!(repo, "core" | "cask") {
        format!("Error: No available tap {name}.\nRun brew tap --force {name} to tap {name}!")
    } else {
        format!(
            "Error: No available tap {name}.\nRun brew tap-new {name} to create a new {name} tap!"
        )
    }
}

/// `brew untap`: brew resolves every name to an installed
/// tap before removing any (`to_installed_taps`).
pub fn remove_all(prefix: &Prefix, names: &[&str]) -> Result<(), String> {
    let mut parsed = Vec::new();
    for name in names {
        let (user, repo) = parse_tap_name(name)
            .ok_or_else(|| format!("abrew: tap name must be user/repo, got {name}"))?;
        if tap_path(prefix, &user, &repo).is_ok_and(|p| std::fs::symlink_metadata(p).is_err()) {
            return Err(unavailable_error(&user, &repo));
        }
        let full = format!("{user}/{repo}");
        if !parsed.contains(&full) {
            parsed.push(full);
        }
    }
    for name in &parsed {
        remove(prefix, name)?;
    }
    Ok(())
}

pub fn remove(prefix: &Prefix, name: &str) -> Result<(), String> {
    let (user, repo) = parse_tap_name(name)
        .ok_or_else(|| format!("abrew: tap name must be user/repo, got {name}"))?;
    if tap_path(prefix, &user, &repo).is_ok_and(|p| std::fs::symlink_metadata(p).is_err()) {
        return Err(unavailable_error(&user, &repo));
    }
    let official = user == "homebrew" && matches!(repo.as_str(), "core" | "cask");
    if !official {
        let (formulae, casks) = tap_installed_names(prefix, &user, &repo).map_err(|error| {
            format!(
                "abrew: refusing to untap {user}/{repo}: installed package census is incomplete: {error}"
            )
        })?;
        if !formulae.is_empty() || !casks.is_empty() {
            let types = if casks.is_empty() {
                "formulae"
            } else if formulae.is_empty() {
                "casks"
            } else {
                "formulae and casks"
            };
            let names: Vec<&str> = formulae.iter().chain(&casks).map(String::as_str).collect();
            return Err(format!(
                "abrew: Refusing to untap {user}/{repo} because it contains the following installed {types}:\n{}",
                names.join("\n")
            ));
        }
    }
    let (path, _) = mutation_path(prefix, &user, &repo, false)?;
    let tap = Tap {
        user: user.clone(),
        repo: repo.clone(),
        path: path.clone(),
    };
    unlink_tap_manpages(prefix, &tap);
    unlink_tap_completions(prefix, &tap);
    std::fs::remove_dir_all(&path).map_err(|e| format!("abrew: {e}"))
}

/// Every tap on disk: brew's `Tap.installed` is any
/// `Taps/*/*/` directory (hidden entries excluded by glob);
/// a `.git` checkout is not required for a directory to be
/// a tap brew resolves names from.
#[must_use]
pub fn list(prefix: &Prefix) -> Vec<Tap> {
    let mut out = Vec::new();
    let Ok(users) = std::fs::read_dir(prefix.taps_dir()) else {
        return out;
    };
    for u in users.flatten() {
        if !u.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Some(user) = u.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if user.starts_with('.') {
            continue;
        }
        let Ok(repos) = std::fs::read_dir(u.path()) else {
            continue;
        };
        for r in repos.flatten() {
            if !r.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let fname = r.file_name();
            if let Some(repo) = fname.to_str().and_then(|s| s.strip_prefix("homebrew-"))
                && !repo.starts_with('.')
                && let Ok(path) = tap_path(prefix, &user, repo)
            {
                out.push(Tap {
                    user: user.clone(),
                    repo: repo.to_owned(),
                    path,
                });
            }
        }
    }
    out.sort_by_key(|a| a.name());
    out
}

/// Brew's completions setting, kept in the prefix's own state
/// (`var/homebrew/linkcompletions`): brew keeps it in its git
/// config, which an abrew prefix is not.
/// One brew Settings-style boolean kept in the prefix's own
/// state (`var/homebrew/<name>`).
pub(super) fn setting_bool(prefix: &Prefix, name: &str) -> bool {
    crate::metadata_io::read_bounded_regular_utf8(
        &prefix.root.join("var/homebrew").join(name),
        1024,
    )
    .is_ok_and(|v| v.trim() == "true")
}

pub(super) fn set_setting_bool(prefix: &Prefix, name: &str, value: bool) -> Result<(), String> {
    use std::io::Write as _;
    let path = prefix.root.join("var/homebrew").join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("abrew: {e}"))?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let mut f = opts.open(&path).map_err(|e| format!("abrew: {e}"))?;
    if !f.metadata().map_err(|e| format!("abrew: {e}"))?.is_file() {
        return Err(format!("abrew: {} is not a regular file", path.display()));
    }
    f.set_len(0).map_err(|e| format!("abrew: {e}"))?;
    f.write_all(if value { b"true" } else { b"false" })
        .map_err(|e| format!("abrew: {e}"))
}

pub(super) fn completions_linked(prefix: &Prefix) -> bool {
    setting_bool(prefix, "linkcompletions")
}

pub(super) fn set_completions_linked(prefix: &Prefix, linked: bool) -> Result<(), String> {
    set_setting_bool(prefix, "linkcompletions", linked)
}

/// The symlink target for `to` as seen from `from_dir`
/// (brew's `make_relative_symlink`).
fn relative_link_target(from_dir: &std::path::Path, to: &std::path::Path) -> std::path::PathBuf {
    let from: Vec<std::ffi::OsString> = from_dir
        .components()
        .map(|c| c.as_os_str().to_os_string())
        .collect();
    let dest: Vec<std::ffi::OsString> = to
        .components()
        .map(|c| c.as_os_str().to_os_string())
        .collect();
    let mut common = 0;
    while common < from.len() && common < dest.len() && from[common] == dest[common] {
        common += 1;
    }
    let mut out = std::path::PathBuf::new();
    for _ in common..from.len() {
        out.push("..");
    }
    for part in &dest[common..] {
        out.push(part);
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// Files under `dir`, recursively (brew's `Pathname#find`
/// without directories).
fn collect_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            collect_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// One shell's link pass for one tap (`Utils::Link`):
/// returns destinations that conflicted.
pub(super) fn link_src_dst(
    src_dir: &std::path::Path,
    dst_dir: &std::path::Path,
) -> Vec<std::path::PathBuf> {
    let mut conflicts = Vec::new();
    if !src_dir.is_dir() {
        return conflicts;
    }
    let mut files = Vec::new();
    collect_files(src_dir, &mut files);
    files.sort();
    for src in files {
        let Ok(rel) = src.strip_prefix(src_dir) else {
            continue;
        };
        let dst = dst_dir.join(rel);
        if dst
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
        {
            if std::fs::canonicalize(&dst).ok() == std::fs::canonicalize(&src).ok() {
                continue;
            }
            let _ = std::fs::remove_file(&dst);
        }
        if dst.exists() {
            conflicts.push(dst);
            continue;
        }
        if let Some(parent) = dst.parent() {
            let _ = std::fs::create_dir_all(parent);
            let target = relative_link_target(parent, &src);
            let _ = symlink_entry(&target, &dst);
        }
    }
    conflicts
}

pub(super) fn unlink_src_dst(src_dir: &std::path::Path, dst_dir: &std::path::Path) {
    if !src_dir.is_dir() {
        return;
    }
    let mut files = Vec::new();
    collect_files(src_dir, &mut files);
    for src in files {
        let Ok(rel) = src.strip_prefix(src_dir) else {
            continue;
        };
        let dst = dst_dir.join(rel);
        if dst
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
            && std::fs::canonicalize(&dst).ok() == std::fs::canonicalize(&src).ok()
        {
            let _ = std::fs::remove_file(&dst);
            if let Some(parent) = dst.parent() {
                let _ = std::fs::remove_dir(parent);
            }
        }
    }
}

/// `brew completions [state|link|unlink]`: whether tap shell
/// completion files are linked into the prefix's completion
/// directories (brew's destinations).
fn symlink_entry(target: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(not(unix))]
    {
        let _ = (target, link);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "symlinks are not supported",
        ))
    }
}

/// The conflict report brew's `Utils::Link` prints.
fn report_link_conflicts(conflicts: &[PathBuf], command: &str) {
    if conflicts.is_empty() {
        return;
    }
    eprintln!(
        "Error: Could not link:\n{}\n\nPlease delete these paths and run:\n  {command}",
        conflicts
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Whether the one-time completions hint has been shown
/// (brew's `completionsmessageshown` setting, same store).
fn completions_message_shown(prefix: &Prefix) -> bool {
    setting_bool(prefix, "completionsmessageshown")
}

fn mark_completions_message_shown(prefix: &Prefix) {
    let _ = set_setting_bool(prefix, "completionsmessageshown", true);
}

/// Destination dirs for linked tap files, brew's layout.
pub(super) fn completion_dirs(prefix: &Prefix) -> [(&'static str, PathBuf); 3] {
    [
        ("bash", prefix.root.join("etc/bash_completion.d")),
        ("zsh", prefix.root.join("share/zsh/site-functions")),
        ("fish", prefix.root.join("share/fish/vendor_completions.d")),
    ]
}

/// Link or unlink one tap's completions per the setting,
/// with manpages always linked (brew's tap-install hook).
pub(super) fn link_completions_and_manpages(prefix: &Prefix, tap: &Tap) {
    let man_conflicts = link_src_dst(
        &tap.path.join("manpages"),
        &prefix.root.join("share/man/man1"),
    );
    report_link_conflicts(&man_conflicts, "brew tap --repair");
    show_completions_message_if_needed(prefix);
    if tap.user == "homebrew" || completions_linked(prefix) {
        let mut conflicts = Vec::new();
        for (shell, dst) in completion_dirs(prefix) {
            conflicts.extend(link_src_dst(
                &tap.path.join("completions").join(shell),
                &dst,
            ));
        }
        report_link_conflicts(&conflicts, "brew tap --repair");
    } else {
        unlink_tap_completions(prefix, tap);
    }
}

/// Remove one tap's linked completion files.
pub(super) fn unlink_tap_completions(prefix: &Prefix, tap: &Tap) {
    for (shell, dst) in completion_dirs(prefix) {
        unlink_src_dst(&tap.path.join("completions").join(shell), &dst);
    }
}

/// Remove one tap's linked manpages.
pub(super) fn unlink_tap_manpages(prefix: &Prefix, tap: &Tap) {
    unlink_src_dst(
        &tap.path.join("manpages"),
        &prefix.root.join("share/man/man1"),
    );
}

fn show_completions_message_if_needed(prefix: &Prefix) {
    if completions_message_shown(prefix) {
        return;
    }
    let to_link = list(prefix).iter().any(|tap| {
        tap.user != "homebrew"
            && ["bash", "zsh", "fish"]
                .iter()
                .any(|shell| tap.path.join("completions").join(shell).is_dir())
    });
    if !to_link {
        return;
    }
    println!(
        "{}",
        super::tty::ohai("Homebrew completions for external commands are unlinked by default!")
    );
    println!(
        "To opt-in to automatically linking external tap shell completion files, run:\n  brew completions link\nThen, follow the directions at https://docs.brew.sh/Shell-Completion"
    );
    mark_completions_message_shown(prefix);
}

/// Locate a formula `.rb` file inside `tap`.
#[must_use]
pub fn formula_file(tap: &Tap, name: &str) -> Option<PathBuf> {
    if !super::valid_component(name) {
        return None;
    }
    let root = tap.path.canonicalize().ok()?;
    let inside =
        |file: &PathBuf| file.is_file() && file.canonicalize().is_ok_and(|p| p.starts_with(&root));
    let direct = [
        tap.path.join("Formula").join(format!("{name}.rb")),
        tap.path.join(format!("{name}.rb")),
    ];
    for c in direct {
        if inside(&c) {
            return Some(c);
        }
    }
    let first = name.chars().next()?.to_string();
    let sharded = tap
        .path
        .join("Formula")
        .join(first)
        .join(format!("{name}.rb"));
    inside(&sharded).then_some(sharded)
}

/// Parse a formula `.rb` as data without evaluating Ruby.
#[must_use]
pub fn parse_rb_as_data(text: &str, full_name: &str) -> Formula {
    let mut f = Formula {
        name: Formula::short_name(full_name).to_string(),
        full_name: full_name.to_string(),
        ..Formula::default()
    };
    for line in text.lines() {
        let line = line.trim();
        let quoted = |key: &str| -> Option<String> {
            let rest = line.strip_prefix(key)?.trim_start();
            let rest = rest.strip_prefix('"').or_else(|| rest.strip_prefix('\''))?;
            let end = rest.find(['"', '\''])?;
            rest.get(..end).map(str::to_owned)
        };
        if f.desc.is_empty() {
            f.desc = quoted("desc").unwrap_or_default();
        }
        if f.homepage.is_empty() {
            f.homepage = quoted("homepage").unwrap_or_default();
        }
        if f.stable_url.is_empty() {
            f.stable_url = quoted("url").unwrap_or_default();
        }
        if f.stable_sha256.is_empty() {
            f.stable_sha256 = quoted("sha256").unwrap_or_default();
        }
        if f.version.is_empty() {
            f.version = quoted("version").unwrap_or_default();
        }
        if let Some(dep) = quoted("depends_on")
            && !dep.is_empty()
            && !dep.contains(' ')
        {
            f.dependencies.push(dep);
        }
    }
    if f.version.is_empty() {
        f.version = version_from_url(&f.stable_url);
    }
    f
}

fn version_from_url(url: &str) -> String {
    let base = url.rsplit('/').next().unwrap_or_default();
    let digits: String = base
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    digits.trim_end_matches('.').to_string()
}

/// The first quoted string after `key` on a stanza line.
fn cask_quoted(line: &str, key: &str) -> Option<String> {
    let rest = line.strip_prefix(key)?.trim_start();
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let inner = &rest[quote.len_utf8()..];
    let end = inner.find(quote)?;
    Some(inner[..end].to_string())
}

/// Every quoted string in a stanza fragment (array members).
fn cask_quoted_all(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(['"', '\'']) {
        let quote = rest[start..].chars().next().unwrap_or('"');
        let inner = &rest[start + quote.len_utf8()..];
        let Some(end) = inner.find(quote) else { break };
        out.push(inner[..end].to_string());
        rest = &inner[end + quote.len_utf8()..];
    }
    out
}

/// Brew's `#{version...}` interpolation subset a cask URL or
/// artifact name uses in practice; anything else marks the
/// record unsupported for install (info still shows it raw).
fn cask_interpolate(text: &str, version: &str, unsupported: &mut Option<&'static str>) -> String {
    if !text.contains("#{") {
        return text.to_string();
    }
    let csv: Vec<&str> = version.split(',').collect();
    let first = csv.first().copied().unwrap_or(version);
    let mut nums: Vec<&str> = first.split('.').collect();
    while nums.len() < 3 {
        nums.push("0");
    }
    let value = |expr: &str| -> Option<String> {
        Some(match expr {
            "version" => version.to_string(),
            "version.major" => nums[0].to_string(),
            "version.minor" => nums[1].to_string(),
            "version.patch" => nums[2].to_string(),
            "version.major_minor" => format!("{}.{}", nums[0], nums[1]),
            "version.major_minor_patch" => format!("{}.{}.{}", nums[0], nums[1], nums[2]),
            "version.no_dots" => first.replace('.', ""),
            "version.before_comma" => first.to_string(),
            "version.after_comma" => csv.get(1).copied().unwrap_or_default().to_string(),
            "version.csv.first" => first.to_string(),
            "version.csv.second" => csv.get(1).copied().unwrap_or_default().to_string(),
            "version.csv.third" => csv.get(2).copied().unwrap_or_default().to_string(),
            "version.dots_to_underscores" => first.replace('.', "_"),
            "version.dots_to_hyphens" => first.replace('.', "-"),
            _ => return None,
        })
    };
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("#{") {
        out.push_str(&rest[..start]);
        let inner = &rest[start + 2..];
        let Some(end) = inner.find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        match value(inner[..end].trim()) {
            Some(v) => out.push_str(&v),
            None => {
                *unsupported = unsupported.or(Some("depends_on"));
                out.push_str(&rest[start..start + 2 + end + 1]);
            }
        }
        rest = &inner[end + 1..];
    }
    out.push_str(rest);
    out
}

/// A macOS release symbol's numeric version (depends_on).
fn macos_symbol_version(symbol: &str) -> Option<String> {
    Some(
        match symbol {
            "golden_gate" => "27",
            "tahoe" => "26",
            "sequoia" => "15",
            "sonoma" => "14",
            "ventura" => "13",
            "monterey" => "12",
            "big_sur" => "11",
            "catalina" => "10.15",
            "mojave" => "10.14",
            "high_sierra" => "10.13",
            "sierra" => "10.12",
            "el_capitan" => "10.11",
            _ => return None,
        }
        .to_string(),
    )
}

/// Parse a cask `.rb` as data without evaluating Ruby, the
/// cask sibling of `parse_rb_as_data`: top-level stanzas
/// line-wise, per-system blocks skipped (and marked
/// unsupported for install), `#{version}` interpolation
/// applied to url and artifact names.
#[must_use]
pub fn parse_cask_rb_as_data(text: &str, token: &str) -> Cask {
    let mut c = Cask {
        token: token.to_string(),
        ..Cask::default()
    };
    let mut unsupported: Option<&'static str> = None;
    let mut skip_depth = 0usize;
    let mut heredoc: Option<String> = None;
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0usize;
    while i < lines.len() {
        let raw = lines[i];
        i += 1;
        if let Some(marker) = &heredoc {
            if raw.trim() == marker.as_str() {
                heredoc = None;
                c.caveats = c.caveats.trim_start_matches('\u{1}').to_string();
            } else {
                if c.caveats.len() > 1 {
                    c.caveats.push('\n');
                }
                c.caveats.push_str(raw.trim());
            }
            continue;
        }
        let line = raw.trim();
        if skip_depth > 0 {
            if line == "end" {
                skip_depth -= 1;
            } else if line.ends_with(" do") || line.contains(" do |") {
                skip_depth += 1;
            }
            continue;
        }
        if (line.starts_with("on_") || line.starts_with("language "))
            && (line.ends_with(" do") || line.contains(" do |"))
        {
            unsupported = unsupported.or(Some("variations"));
            skip_depth = 1;
            continue;
        }
        if line.starts_with("caveats <<~") || line.starts_with("caveats <<-") {
            let marker = line
                .rsplit(['~', '-'])
                .next()
                .unwrap_or("EOS")
                .trim()
                .to_string();
            c.caveats = "\u{1}".to_string();
            heredoc = Some(marker);
            continue;
        }
        if c.version.is_empty() {
            if line.starts_with("version :latest") {
                c.version = "latest".to_string();
            } else if let Some(v) = cask_quoted(line, "version") {
                c.version = v;
            }
        }
        if c.sha256.is_none() && line.starts_with("sha256") {
            if line.contains(":no_check") {
                c.sha256_no_check = true;
            } else if let Some(hex) = cask_quoted(line, "sha256") {
                c.sha256 = Some(hex);
            } else if line.contains("arm:") {
                // `sha256 arm: "…", intel: "…"` (may wrap)
                let mut joined = line.to_string();
                while !joined.contains("intel:") && i < lines.len() {
                    joined.push_str(lines[i].trim());
                    i += 1;
                }
                if let Some(arm) = joined.split("arm:").nth(1) {
                    c.sha256 = cask_quoted_all(arm).into_iter().next();
                }
            }
        }
        if c.url.is_empty()
            && let Some(u) = cask_quoted(line, "url")
        {
            c.url = u;
        }
        if c.name.is_empty()
            && let Some(n) = cask_quoted(line, "name")
        {
            c.name = n;
        }
        if c.desc.is_empty()
            && let Some(d) = cask_quoted(line, "desc")
        {
            c.desc = d;
        }
        if c.homepage.is_empty()
            && let Some(h) = cask_quoted(line, "homepage")
        {
            c.homepage = h;
        }
        if line.starts_with("auto_updates true") {
            c.auto_updates = true;
        }
        if line.starts_with("container type: :naked") {
            c.naked = true;
        }
        if c.caveats.is_empty()
            && let Some(text) = cask_quoted(line, "caveats")
        {
            c.caveats = text;
        }
        if line.starts_with("deprecate!") || line.starts_with("disable!") {
            let disable = line.starts_with("disable!");
            let date = cask_quoted(line.split("date:").nth(1).unwrap_or_default(), "");
            let because = line.split("because:").nth(1).map(|s| {
                s.trim()
                    .trim_matches(|ch| ch == '"' || ch == ',')
                    .to_string()
            });
            let because = because.map(|b| b.strip_prefix(':').map_or(b.clone(), str::to_string));
            if disable {
                c.disabled = true;
                c.deprecate_disable.disable_date = date;
                c.deprecate_disable.disable_reason = because;
            } else {
                c.deprecated = true;
                c.deprecate_disable.deprecation_date = date;
                c.deprecate_disable.deprecation_reason = because;
            }
        }
        parse_cask_depends(line, &mut c, &mut unsupported);
        parse_cask_artifact(line, &mut c);
        if line.starts_with("zap ") || line == "zap" {
            c.zap_present = true;
            let (trash, delete, rmdir, other) = parse_cask_file_stanza(&lines, &mut i, line);
            c.zap_trash.extend(trash);
            c.zap_delete.extend(delete);
            c.zap_rmdir.extend(rmdir);
            c.zap_unsupported = c.zap_unsupported || other;
        }
        if line.starts_with("uninstall ") || line == "uninstall" {
            let (trash, _, rmdir, other) = parse_cask_file_stanza(&lines, &mut i, line);
            c.uninstall_trash.extend(trash);
            c.uninstall_rmdir.extend(rmdir);
            c.uninstall_unsupported = c.uninstall_unsupported || other;
        }
        if line.starts_with("conflicts_with ") {
            c.conflicts.extend(cask_quoted_all(line));
        }
    }
    if c.name.is_empty() {
        c.name = token.to_string();
    }
    c.url = cask_interpolate(&c.url, &c.version, &mut unsupported);
    for artifact in &mut c.artifacts {
        interpolate_artifact(artifact, &c.version, &mut unsupported);
    }
    c.caveats = c.caveats.trim_start_matches('\u{1}').to_string();
    c.unsupported_constraints = unsupported;
    c
}

/// One `depends_on` stanza line of a cask file.
fn parse_cask_depends(line: &str, c: &mut Cask, unsupported: &mut Option<&'static str>) {
    let Some(rest) = line.strip_prefix("depends_on ") else {
        return;
    };
    let values = |key: &str| -> Vec<String> {
        rest.split(key)
            .nth(1)
            .map(cask_quoted_all)
            .unwrap_or_default()
    };
    if rest.starts_with("cask:") {
        c.depends_cask.extend(values("cask:"));
    } else if rest.starts_with("formula:") {
        c.depends_formula.extend(values("formula:"));
    } else if rest.starts_with("macos:") {
        let spec = rest.trim_start_matches("macos:").trim();
        let raw = cask_quoted(spec, "").unwrap_or_default();
        let (cmp, value) = if let Some(v) = raw.strip_prefix(">=") {
            (">=", v.trim())
        } else if let Some(v) = raw.strip_prefix("==") {
            ("==", v.trim())
        } else if raw.starts_with('<') {
            ("<=", "")
        } else {
            ("==", raw.trim())
        };
        let numeric = value
            .strip_prefix(':')
            .and_then(macos_symbol_version)
            .or_else(|| {
                (!value.is_empty() && value.chars().next().is_some_and(|ch| ch.is_ascii_digit()))
                    .then(|| value.to_string())
            });
        match (cmp, numeric) {
            (">=", Some(v)) => c.macos_min = Some(v),
            ("==", Some(v)) => c.macos_exact = Some(v),
            _ => *unsupported = unsupported.or(Some("depends_on")),
        }
    } else if rest.starts_with("arch:") {
        c.arch = rest
            .split(':')
            .nth(2)
            .map(|s| {
                s.trim()
                    .trim_matches(|ch| ch == ',' || ch == ']' || ch == '[')
                    .to_string()
            })
            .filter(|s| !s.is_empty());
    } else if !rest.starts_with("macos:") {
        *unsupported = unsupported.or(Some("depends_on"));
    }
}

/// One artifact stanza line of a cask file.
fn parse_cask_artifact(line: &str, c: &mut Cask) {
    const KINDS: &[&str] = &[
        "app",
        "pkg",
        "binary",
        "font",
        "manpage",
        "suite",
        "keyboard_layout",
        "screen_saver",
        "prefpane",
        "service",
        "artifact",
        "qlplugin",
        "colorpicker",
        "dictionary",
        "input_method",
        "internet_plugin",
        "mdimporter",
        "audio_unit_plugin",
        "vst_plugin",
        "vst3_plugin",
        "bash_completion",
        "zsh_completion",
        "fish_completion",
    ];
    let Some(kind) = KINDS
        .iter()
        .find(|k| line.starts_with(&format!("{k} \"")) || line.starts_with(&format!("{k} '")))
    else {
        return;
    };
    let Some(first) = cask_quoted(line, kind) else {
        return;
    };
    let dest = line
        .split("target:")
        .nth(1)
        .and_then(|t| cask_quoted(t.trim_start(), ""));
    let dest = dest.filter(|d| !d.is_empty() && !d.contains('/'));
    c.artifacts.push(match *kind {
        "app" => Artifact::App(first, dest),
        "binary" => Artifact::Binary(first, dest),
        "font" => Artifact::Font(first, dest),
        "pkg" => Artifact::Pkg(first),
        "manpage" => Artifact::Manpage(first, dest),
        "suite" => Artifact::Suite(first, dest),
        "keyboard_layout" => Artifact::KeyboardLayout(first, dest),
        "screen_saver" => Artifact::ScreenSaver(first, dest),
        "prefpane" => Artifact::PrefPane(first, dest),
        "service" => Artifact::Service(first, dest),
        "artifact" => Artifact::Generic(first, dest),
        "qlplugin" => Artifact::Library(first, dest, "Library/QuickLook".into()),
        "colorpicker" => Artifact::Library(first, dest, "Library/ColorPickers".into()),
        "dictionary" => Artifact::Library(first, dest, "Library/Dictionaries".into()),
        "input_method" => Artifact::Library(first, dest, "Library/Input Methods".into()),
        "internet_plugin" => Artifact::Library(first, dest, "Library/Internet Plug-Ins".into()),
        "mdimporter" => Artifact::Library(first, dest, "Library/Spotlight".into()),
        "audio_unit_plugin" => {
            Artifact::Library(first, dest, "Library/Audio/Plug-Ins/Components".into())
        }
        "vst_plugin" => Artifact::Library(first, dest, "Library/Audio/Plug-Ins/VST".into()),
        "vst3_plugin" => Artifact::Library(first, dest, "Library/Audio/Plug-Ins/VST3".into()),
        "bash_completion" => {
            Artifact::Library(first, dest, "share/bash-completion/completions".into())
        }
        "zsh_completion" => Artifact::Library(first, dest, "share/zsh/site-functions".into()),
        "fish_completion" => {
            Artifact::Library(first, dest, "share/fish/vendor_completions.d".into())
        }
        other => Artifact::Other(format!("{other}:{first}")),
    });
}

/// Interpolate `#{version}` in an artifact's names.
fn interpolate_artifact(a: &mut Artifact, version: &str, unsupported: &mut Option<&'static str>) {
    let mut go = |s: &mut String| *s = cask_interpolate(s, version, unsupported);
    match a {
        Artifact::App(s, _)
        | Artifact::Binary(s, _)
        | Artifact::Font(s, _)
        | Artifact::Pkg(s)
        | Artifact::Manpage(s, _)
        | Artifact::Suite(s, _)
        | Artifact::KeyboardLayout(s, _)
        | Artifact::ScreenSaver(s, _)
        | Artifact::PrefPane(s, _)
        | Artifact::Service(s, _)
        | Artifact::Generic(s, _)
        | Artifact::Library(s, _, _) => go(s),
        Artifact::Wrapper(_, _, _, _) | Artifact::Other(_) => {}
    }
}

/// A `zap`/`uninstall` stanza (possibly multi-line): returns
/// (trash, delete, rmdir, saw-other-directive). Consumes
/// following lines while brackets are unbalanced.
fn parse_cask_file_stanza(
    lines: &[&str],
    i: &mut usize,
    first_line: &str,
) -> (Vec<String>, Vec<String>, Vec<String>, bool) {
    let mut joined = first_line.to_string();
    let depth = |s: &str| -> i64 {
        s.chars().filter(|c| *c == '[').count() as i64
            - s.chars().filter(|c| *c == ']').count() as i64
    };
    while depth(&joined) > 0 && *i < lines.len() {
        joined.push('\n');
        joined.push_str(lines[*i]);
        *i += 1;
    }
    let grab = |key: &str| -> Vec<String> {
        let Some(rest) = joined.split(key).nth(1) else {
            return Vec::new();
        };
        let rest = rest.trim_start();
        if rest.starts_with('[') {
            let end = rest.find(']').map_or(rest.len(), |b| b + 1);
            cask_quoted_all(&rest[..end])
        } else {
            let line = rest.lines().next().unwrap_or_default();
            cask_quoted_all(line)
        }
    };
    let other = [
        "quit:",
        "launchctl:",
        "pkgutil:",
        "script:",
        "signal:",
        "login_item:",
        "kext:",
        "delete:",
    ]
    .iter()
    .any(|k| *k != "delete:" && joined.contains(k));
    (grab("trash:"), grab("delete:"), grab("rmdir:"), other)
}

/// Look up `user/tap/name` (or a bare name in any tap) as data.
pub fn lookup(prefix: &Prefix, full: &str) -> Option<Formula> {
    if !super::valid_name(full) {
        return None;
    }
    let parts: Vec<&str> = full.split('/').collect();
    if parts.len() == 3 {
        let (user, repo, name) = (parts.first()?, parts.get(1)?, parts.get(2)?);
        let (user, repo) = parse_tap_name(&format!("{user}/{repo}"))?;
        let path = tap_path(prefix, &user, &repo).ok()?;
        let tap = Tap { user, repo, path };
        let (file, shown) = match formula_file(&tap, name) {
            Some(file) => (file, full.to_string()),
            None => {
                let (file, canon_name) = alias_file(&tap, name)?;
                (file, format!("{}/{canon_name}", tap.name()))
            }
        };
        let canon = file.canonicalize().ok()?;
        let text = crate::metadata_io::read_bounded_regular_utf8(&canon, 1024 * 1024).ok()?;
        return Some(parse_rb_as_data(&text, &shown));
    }
    for tap in list(prefix) {
        if let Some(file) = formula_file(&tap, full)
            && let Ok(canon) = file.canonicalize()
            && let Ok(text) = crate::metadata_io::read_bounded_regular_utf8(&canon, 1024 * 1024)
        {
            return Some(parse_rb_as_data(&text, &format!("{}/{full}", tap.name())));
        }
        if let Some((file, canon_name)) = alias_file(&tap, full)
            && let Ok(canon) = file.canonicalize()
            && let Ok(text) = crate::metadata_io::read_bounded_regular_utf8(&canon, 1024 * 1024)
        {
            return Some(parse_rb_as_data(
                &text,
                &format!("{}/{canon_name}", tap.name()),
            ));
        }
    }
    None
}

/// A cask file inside a tap, at brew's candidate locations.
pub fn cask_file(tap: &Tap, token: &str) -> Option<PathBuf> {
    if !super::valid_component(token) {
        return None;
    }
    let root = tap.path.canonicalize().ok()?;
    let shard = if let Some(rest) = token.strip_prefix("font-") {
        format!("font/font-{}", rest.chars().next().unwrap_or('x'))
    } else {
        token.chars().next().unwrap_or('x').to_string()
    };
    for cand in [
        tap.path.join("Casks").join(format!("{token}.rb")),
        tap.path
            .join("Casks")
            .join(&shard)
            .join(format!("{token}.rb")),
    ] {
        if let Ok(canon) = cand.canonicalize()
            && canon.starts_with(&root)
        {
            return Some(canon);
        }
    }
    None
}

/// Look up a cask token (qualified or bare) in tap directories
/// as data. Returns the record and its qualified name.
pub fn lookup_cask(prefix: &Prefix, full: &str) -> Option<(Cask, String)> {
    if !super::valid_name(full) {
        return None;
    }
    let finish = |tap: &Tap, file: &std::path::Path, token: &str| -> Option<(Cask, String)> {
        let text = crate::metadata_io::read_bounded_regular_utf8(file, 1024 * 1024).ok()?;
        let mut c = parse_cask_rb_as_data(&text, token);
        c.full_name = format!("{}/{token}", tap.name());
        if let Ok(root) = tap.path.canonicalize()
            && let Ok(rel) = file.strip_prefix(&root)
        {
            c.source_url = Some(format!(
                "https://github.com/{}/homebrew-{}/blob/HEAD/{}",
                tap.user,
                tap.repo,
                rel.display()
            ));
        }
        Some((c, format!("{}/{token}", tap.name())))
    };
    let parts: Vec<&str> = full.split('/').collect();
    if parts.len() == 2 {
        // Two-segment shorthand names the `user/homebrew-user` tap.
        return lookup_cask(prefix, &format!("{}/{}/{}", parts[0], parts[0], parts[1]));
    }
    if parts.len() == 3 {
        let (user, repo, token) = (parts.first()?, parts.get(1)?, parts.get(2)?);
        let (user, repo) = parse_tap_name(&format!("{user}/{repo}"))?;
        let path = tap_path(prefix, &user, &repo).ok()?;
        let tap = Tap { user, repo, path };
        let file = cask_file(&tap, token)?;
        return finish(&tap, &file, token);
    }
    for tap in list(prefix) {
        if let Some(file) = cask_file(&tap, full)
            && let Some(found) = finish(&tap, &file, full)
        {
            return Some(found);
        }
    }
    None
}

/// A tap's `Aliases/<name>` entry resolved to its formula file
/// and canonical name (brew follows alias symlinks).
fn alias_file(tap: &Tap, name: &str) -> Option<(PathBuf, String)> {
    let link = tap.path.join("Aliases").join(name);
    let meta = std::fs::symlink_metadata(&link).ok()?;
    if !meta.file_type().is_symlink() {
        return None;
    }
    let canon = link.canonicalize().ok()?;
    let root = tap.path.canonicalize().ok()?;
    if !canon.starts_with(&root) {
        return None;
    }
    let canon_name = canon.file_stem()?.to_str()?.to_string();
    super::valid_component(&canon_name).then_some((canon, canon_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_ok(args: &[&str]) {
        let out = fetch::run_capture(aterm_uds::os_path!("git"), args).unwrap();
        assert!(
            out.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn tap_add_refusals_match_brew() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        prefix.ensure().unwrap();
        let err = |name: &str, url: Option<&str>, custom: bool, force: bool| {
            add(&prefix, name, url, custom, force).unwrap_err()
        };
        assert!(err("homebrew/services", None, false, false).contains("was deprecated"));
        assert!(err("homebrew/bundle", None, false, false).contains("was deprecated"));
        assert_eq!(
            err("caskroom/fonts", None, false, false),
            "Error: caskroom/fonts was moved. Tap homebrew/cask-fonts instead."
        );
        assert_eq!(
            err("someone/tap", None, true, false),
            "Error: Tap someone/tap with option `--custom-remote` but without a remote URL."
        );
        assert!(err("homebrew/core", None, false, false).contains("--force"));
        assert!(err("homebrew/cask", None, false, false).contains("--force"));
        assert!(err("linuxbrew/core", None, false, false).contains("--force"));
    }

    #[test]
    fn tap_add_mismatch_and_custom_remote() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let origin = tmp.path().join("origin");
        std::fs::create_dir(&origin).unwrap();
        let origin_s = origin.to_string_lossy().into_owned();
        git_ok(&["-C", &origin_s, "init", "-q", "-b", "main"]);
        std::fs::write(origin.join("f.rb"), b"# tap").unwrap();
        git_ok(&["-C", &origin_s, "add", "f.rb"]);
        git_ok(&[
            "-C",
            &origin_s,
            "-c",
            "user.email=t@example.invalid",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "one",
        ]);
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        prefix.ensure().unwrap();
        let (tap, newly) = add(&prefix, "someone/tap", Some(&origin_s), false, false).unwrap();
        assert!(newly);
        // Re-tap with a different URL refuses with brew's mismatch text.
        let error = add(&prefix, "someone/tap", Some("/elsewhere"), false, false).unwrap_err();
        assert_eq!(
            error,
            format!("Error: Tap someone/tap remote mismatch.\n{origin_s} != /elsewhere")
        );
        // --custom-remote repoints the tap at the new remote.
        let mirror = tmp.path().join("mirror");
        let mirror_s = mirror.to_string_lossy().into_owned();
        git_ok(&["clone", "-q", &origin_s, &mirror_s]);
        add(&prefix, "someone/tap", Some(&mirror_s), true, false).unwrap();
        let remote = git_out(&tap.path, &["remote", "get-url", "origin"]).unwrap();
        assert_eq!(remote, mirror_s);
    }

    #[test]
    fn repair_follows_an_upstream_default_branch_rename() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let origin = tmp.path().join("origin");
        std::fs::create_dir(&origin).unwrap();
        let origin_s = origin.to_string_lossy().into_owned();
        git_ok(&["-C", &origin_s, "init", "-q", "-b", "trunk"]);
        std::fs::write(origin.join("f.rb"), b"# tap").unwrap();
        git_ok(&["-C", &origin_s, "add", "f.rb"]);
        git_ok(&[
            "-C",
            &origin_s,
            "-c",
            "user.email=t@example.invalid",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "one",
        ]);
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        prefix.ensure().unwrap();
        let tap_dir = prefix.taps_dir().join("someone/homebrew-tap");
        std::fs::create_dir_all(tap_dir.parent().unwrap()).unwrap();
        let tap_s = tap_dir.to_string_lossy().into_owned();
        git_ok(&["clone", "-q", &origin_s, &tap_s]);
        // Stale state: local branch and origin/HEAD still say master.
        git_ok(&["-C", &tap_s, "branch", "-m", "trunk", "master"]);
        git_ok(&[
            "-C",
            &tap_s,
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/master",
        ]);
        repair(&prefix).unwrap();
        let branches = git_out(&tap_dir, &["branch", "--format=%(refname:short)"]).unwrap();
        assert_eq!(branches, "trunk");
        assert_eq!(origin_branch_name(&tap_dir).as_deref(), Some("trunk"));
        // A second repair is a quiet no-op.
        repair(&prefix).unwrap();
        // No taps at all is fine too.
        let empty = Prefix::at(&tmp.path().join("empty"));
        empty.ensure().unwrap();
        repair(&empty).unwrap();
    }

    #[test]
    fn untap_refuses_while_tap_packages_are_installed() {
        fn payload(p: &Prefix, kind: Kind, version: &str, full: Option<&str>) -> PathBuf {
            let dir = match kind {
                Kind::Formula => p.keg("tool", version),
                Kind::Cask => p.cask_dir("tool", version),
            };
            std::fs::create_dir_all(&dir).unwrap();
            let mut fields = vec![
                ("installed_by", "abrew"),
                (
                    match kind {
                        Kind::Formula => "name",
                        Kind::Cask => "token",
                    },
                    "tool",
                ),
                ("version", version),
            ];
            if let Some(full) = full {
                fields.push(("full_name", full));
            }
            p.write_receipt(&dir, &fields).unwrap();
            dir
        }
        fn tapped(p: &Prefix) -> PathBuf {
            let tap = p.taps_dir().join("someone/homebrew-tap");
            std::fs::create_dir_all(&tap).unwrap();
            std::fs::write(tap.join("KEEP"), b"tap recipes").unwrap();
            tap
        }
        fn refused(p: &Prefix, tap: &std::path::Path) -> String {
            let error = remove(p, "someone/tap").unwrap_err();
            assert_eq!(std::fs::read(tap.join("KEEP")).unwrap(), b"tap recipes");
            error
        }
        let tmp = aterm_tempfile::tempdir().unwrap();
        for kind in [Kind::Formula, Kind::Cask] {
            let p = Prefix::at(&tmp.path().join(kind.word()));
            p.ensure().unwrap();
            let official = match kind {
                Kind::Formula => "homebrew/core/tool",
                Kind::Cask => "homebrew/cask/tool",
            };
            let old = payload(&p, kind, "1.0", Some("someone/tap/tool"));
            let current = payload(&p, kind, "2.0", Some(official));
            let tap = tapped(&p);
            let error = refused(&p, &tap);
            assert!(error.contains("Refusing to untap someone/tap"), "{error}");
            assert!(error.contains("someone/tap/tool"), "{error}");
            assert!(old.is_dir() && current.is_dir());
            payload(&p, kind, "1.0", Some("someone/homebrew-tap/tool"));
            assert!(refused(&p, &tap).contains("someone/tap/tool"));
            std::fs::remove_dir_all(&old).unwrap();
            let receipt = current.join("INSTALL_RECEIPT.json");
            let official_bytes = std::fs::read(&receipt).unwrap();
            remove(&p, "someone/tap").unwrap();
            assert!(
                !tap.exists(),
                "an unrelated installed version is no use of this tap"
            );
            assert_eq!(std::fs::read(&receipt).unwrap(), official_bytes);

            let tap = tapped(&p);
            let key = match kind {
                Kind::Formula => "name",
                Kind::Cask => "token",
            };
            for full in [
                "null",
                "42",
                "\"\"",
                "\"other\"",
                "\"someone/tap/other\"",
                "\"someone/tool\"",
                "\"../tap/tool\"",
            ] {
                let bytes = format!(
                    "{{\"installed_by\":\"abrew\",\"{key}\":\"tool\",\"version\":\"2.0\",\"full_name\":{full}}}"
                );
                std::fs::write(&receipt, bytes.as_bytes()).unwrap();
                let _ = refused(&p, &tap);
                assert_eq!(std::fs::read(&receipt).unwrap(), bytes.as_bytes());
            }
            for bytes in [b"{".as_slice(), b"[]".as_slice()] {
                std::fs::write(&receipt, bytes).unwrap();
                let _ = refused(&p, &tap);
                assert_eq!(std::fs::read(&receipt).unwrap(), bytes);
            }
            std::fs::remove_file(&receipt).unwrap();
            let _ = refused(&p, &tap);
            std::fs::create_dir(&receipt).unwrap();
            let _ = refused(&p, &tap);
            assert!(receipt.is_dir());
            std::fs::remove_dir(&receipt).unwrap();
            let oversized = vec![b' '; 64 * 1024 + 1];
            std::fs::write(&receipt, &oversized).unwrap();
            let _ = refused(&p, &tap);
            assert_eq!(
                std::fs::metadata(&receipt).unwrap().len(),
                oversized.len() as u64
            );
            std::fs::remove_file(&receipt).unwrap();

            #[cfg(unix)]
            {
                let external = tmp.path().join(format!("{}-receipt", kind.word()));
                std::fs::write(&external, &official_bytes).unwrap();
                std::os::unix::fs::symlink(&external, &receipt).unwrap();
                let _ = refused(&p, &tap);
                assert_eq!(std::fs::read_link(&receipt).unwrap(), external);
                assert_eq!(std::fs::read(&external).unwrap(), official_bytes);
                std::fs::remove_file(&receipt).unwrap();
                payload(&p, kind, "2.0", Some(official));
                for version_only in [false, true] {
                    let row = if version_only {
                        current.clone()
                    } else {
                        current.parent().unwrap().to_path_buf()
                    };
                    let held = tmp
                        .path()
                        .join(format!("{}-held-{version_only}", kind.word()));
                    std::fs::rename(&row, &held).unwrap();
                    std::os::unix::fs::symlink(&held, &row).unwrap();
                    let _ = refused(&p, &tap);
                    assert_eq!(std::fs::read_link(&row).unwrap(), held);
                    std::fs::remove_file(&row).unwrap();
                    std::fs::rename(&held, &row).unwrap();
                }
            }
            for full in [None, Some("tool")] {
                payload(&p, kind, "2.0", full);
                let tap = tapped(&p);
                remove(&p, "someone/tap").unwrap();
                assert!(!tap.exists());
                assert!(current.is_dir());
            }
        }
    }

    #[test]
    fn local_remotes_are_the_only_file_transport() {
        for local in ["/tmp/tap", "./tap", "../tap", "~/tap", "file:///tmp/tap"] {
            assert!(is_local_remote(local), "{local}");
            assert!(
                git_protocol_args(true).contains(&"protocol.file.allow=always"),
                "{local}"
            );
        }
        for remote in [
            "https://github.com/me/homebrew-tools.git",
            "git@github.com:me/homebrew-tools.git",
            "ssh://git@example.com/x.git",
            "ext::sh -c true",
            "fd::0",
        ] {
            assert!(!is_local_remote(remote), "{remote}");
        }
        assert!(
            !git_protocol_args(false).contains(&"protocol.file.allow=always"),
            "non-local remotes never allow the file transport"
        );
    }

    #[test]
    fn rb_parse_as_data() {
        let rb = "class Foo < Formula\n  desc \"A foo\"\n  homepage \"https://foo.example\"\n  url \"https://foo.example/foo-2.1.tar.gz\"\n  sha256 \"aaaa\"\n  depends_on \"openssl@3\"\nend\n";
        let f = parse_rb_as_data(rb, "me/tap/foo");
        assert_eq!(f.name, "foo");
        assert_eq!(f.version, "2.1");
        assert_eq!(f.stable_sha256, "aaaa");
        assert_eq!(f.dependencies, ["openssl@3"]);
    }

    #[test]
    fn tap_name_parse() {
        assert_eq!(
            parse_tap_name("me/homebrew-tools"),
            Some(("me".into(), "tools".into()))
        );
        assert_eq!(parse_tap_name("bogus"), None);
        for invalid in [
            "../x",
            "./x",
            "/x",
            "me/..",
            "me/.",
            "me/homebrew-",
            "me/homebrew-..",
            "me/x/y",
            "me\\x/repo",
            "me/repo\\x",
            "me/repo\0",
        ] {
            assert_eq!(parse_tap_name(invalid), None, "{invalid:?}");
        }
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        let p = Prefix::at(&tmp.path().join("prefix"));
        p.ensure().unwrap();
        let outside = p.root.join("Library/homebrew-x");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("marker"), b"keep").unwrap();
        assert!(remove(&p, "../x").is_err());
        assert!(lookup(&p, "../x/marker").is_none());
        assert_eq!(std::fs::read(outside.join("marker")).unwrap(), b"keep");
        let missing = p.taps_dir().join("missing-user");
        assert!(remove(&p, "missing-user/x").is_err());
        assert!(!missing.exists(), "untap never creates a parent");
        let (ordinary, update) = mutation_path(&p, "me", "tools", true).unwrap();
        assert!(!update);
        std::fs::create_dir(&ordinary).unwrap();
        std::fs::create_dir(ordinary.join(".git")).unwrap();
        assert!(mutation_path(&p, "me", "tools", true).unwrap().1);
        std::fs::write(ordinary.join("payload"), b"ordinary").unwrap();
        remove(&p, "me/homebrew-tools").unwrap();
        assert!(!ordinary.exists());
        let regular = p.taps_dir().join("me/homebrew-regular");
        std::fs::write(&regular, b"keep regular").unwrap();
        assert!(remove(&p, "me/regular").is_err());
        assert!(mutation_path(&p, "me", "regular", true).is_err());
        assert_eq!(std::fs::read(&regular).unwrap(), b"keep regular");
        #[cfg(unix)]
        for (label, suffix) in [
            ("root", "Library/Taps/me/homebrew-x"),
            ("Library", "Taps/me/homebrew-x"),
            ("Taps", "me/homebrew-x"),
            ("user", "homebrew-x"),
            ("leaf", ""),
        ] {
            let p = Prefix::at(&tmp.path().join(label));
            p.ensure().unwrap();
            let external = tmp.path().join(format!("external-{label}"));
            let victim = external.join(suffix);
            std::fs::create_dir_all(&victim).unwrap();
            std::fs::write(victim.join("marker"), b"keep").unwrap();
            let alias = match label {
                "root" => p.root.clone(),
                "Library" => p.root.join("Library"),
                "Taps" => p.taps_dir(),
                "user" => p.taps_dir().join("me"),
                _ => p.taps_dir().join("me/homebrew-x"),
            };
            if let Some(parent) = alias.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            if alias.is_dir() {
                std::fs::remove_dir_all(&alias).unwrap();
            }
            std::os::unix::fs::symlink(&external, &alias).unwrap();
            assert!(remove(&p, "me/x").is_err(), "{label}");
            assert!(mutation_path(&p, "me", "x", true).is_err(), "{label}");
            assert_eq!(std::fs::read(victim.join("marker")).unwrap(), b"keep");
            assert_eq!(std::fs::read_link(&alias).unwrap(), external);
        }
        #[cfg(unix)]
        {
            let (path, _) = mutation_path(&p, "me", "worktree", true).unwrap();
            std::fs::create_dir(&path).unwrap();
            let external = tmp.path().join("external-git");
            std::fs::create_dir(&external).unwrap();
            std::fs::write(external.join("marker"), b"keep").unwrap();
            std::os::unix::fs::symlink(&external, path.join(".git")).unwrap();
            assert!(mutation_path(&p, "me", "worktree", true).is_err());
            assert_eq!(std::fs::read(external.join("marker")).unwrap(), b"keep");
        }
        assert_eq!(std::fs::read(outside.join("marker")).unwrap(), b"keep");
    }

    #[test]
    fn migration_target_reads_local_tap_migrations() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let p = Prefix::at(tmp.path());
        p.ensure().unwrap();
        assert_eq!(migration_target_tap(&p, "example/tap", "aztfy"), None);
        let dir = p.taps_dir().join("example/homebrew-tap");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("tap_migrations.json"),
            br#"{"aztfy": "homebrew/core", "old": "other/tap/renamed"}"#,
        )
        .unwrap();
        assert_eq!(
            migration_target_tap(&p, "example/tap", "aztfy").as_deref(),
            Some("homebrew/core")
        );
        assert_eq!(
            migration_target_tap(&p, "example/homebrew-tap", "old").as_deref(),
            Some("other/tap")
        );
        assert_eq!(migration_target_tap(&p, "example/tap", "missing"), None);
    }

    #[cfg(unix)]
    #[test]
    fn tap_link_hook_links_manpages_and_setting_gated_completions() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let p = Prefix::at(tmp.path());
        p.ensure().unwrap();
        let root = p.taps_dir().join("someone/homebrew-tools");
        for dir in ["manpages", "completions/zsh", ".git"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("manpages/tool.1"), b"man").unwrap();
        std::fs::write(root.join("completions/zsh/_tool"), b"#compdef tool").unwrap();
        let tap = Tap {
            user: "someone".to_string(),
            repo: "tools".to_string(),
            path: root.clone(),
        };
        // Unlinked setting: manpages link, completions do not.
        link_completions_and_manpages(&p, &tap);
        assert!(p.root.join("share/man/man1/tool.1").exists());
        assert!(!p.root.join("share/zsh/site-functions/_tool").exists());
        assert!(completions_message_shown(&p));
        // Linked setting: completions follow.
        set_completions_linked(&p, true).unwrap();
        link_completions_and_manpages(&p, &tap);
        assert!(p.root.join("share/zsh/site-functions/_tool").exists());
        // Untap removes both link sets.
        remove(&p, "someone/tools").unwrap();
        assert!(!p.root.join("share/man/man1/tool.1").exists());
        assert!(!p.root.join("share/zsh/site-functions/_tool").exists());
        assert!(!root.exists());
    }

    #[test]
    fn git_redirect_parsing_and_refusal_match_brew() {
        let stderr = "warning: redirecting to https://github.com/New/tools.git/\nFrom https://github.com/old/tools\n";
        assert_eq!(
            redirected_remotes(stderr).collect::<Vec<_>>(),
            vec!["https://github.com/New/tools.git/"]
        );
        // Non-ASCII prefix whose Unicode `.to_lowercase()` expands UTF-8 byte length (`İ` -> `i\u{307}`)
        // must not shift or panic the byte slice into `line`.
        assert_eq!(
            unsafe_redirect(
                "https://github.com/old/tools",
                "İhtar: Redirecting to https://github.com/New/tools.git/\n",
            )
            .as_deref(),
            Some("https://github.com/New/tools.git/")
        );
        // Multi-hop redirect where the first hop canonicalizes `.git/` (`same_remote`) and the
        // second hop crosses to a new repository must still be refused with the new target.
        let multi_hop = "warning: redirecting to https://github.com/old/tools.git/\n\
                         warning: redirecting to https://github.com/New/tools.git/\n";
        assert_eq!(
            unsafe_redirect("https://github.com/old/tools", multi_hop).as_deref(),
            Some("https://github.com/New/tools.git/")
        );
        assert_eq!(
            unsafe_redirect(
                "https://github.com/old/tools",
                "warning: redirecting to https://github.com/old/tools.git/\n",
            ),
            None
        );
        assert_eq!(redirected_remotes("From https://x\n").next(), None);
        assert!(same_remote(
            "git@github.com:Old/Tools.git",
            "https://github.com/old/tools/"
        ));
        assert!(same_remote(
            "https://github.com/old/tools",
            "https://github.com/old/tools.git"
        ));
        assert!(!same_remote(
            "https://github.com/old/tools",
            "https://github.com/new/tools"
        ));
        assert!(!same_remote("", "https://github.com/new/tools"));
        assert!(!same_remote("   ", "   "));
        assert_eq!(
            redirect_refusal("old/tools", "https://github.com/new/tools"),
            "Error: Refusing automatic redirect from old/tools to https://github.com/new/tools.\n\
             Untap old/tools, then tap the new location directly to review its source and trust."
        );
    }
}
