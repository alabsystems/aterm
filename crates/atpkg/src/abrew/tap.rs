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

pub(super) fn tap_path(prefix: &Prefix, user: &str, repo: &str) -> Result<PathBuf, String> {
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
            let fetched = fetch::capture(fetch::git_remote(&args))
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
        let code = fetch::inherit(fetch::git_remote(&git_args));
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
    let code = fetch::inherit(fetch::git_remote(&git_args));
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
        let listed = fetch::capture(fetch::git_remote(&args))
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
    let fetched = fetch::capture(fetch::git_remote(&args))
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
    // Brew shards by first letter, with `lib*` formulae
    // together under `Formula/lib/`.
    let shards: Vec<String> = if name.starts_with("lib") {
        vec!["lib".to_string(), name.chars().next()?.to_string()]
    } else {
        vec![name.chars().next()?.to_string()]
    };
    for shard in shards {
        let sharded = tap
            .path
            .join("Formula")
            .join(shard)
            .join(format!("{name}.rb"));
        if inside(&sharded) {
            return Some(sharded);
        }
    }
    None
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
        if f.license.is_empty() {
            f.license = quoted("license").unwrap_or_default();
        }
        if !f.head && line.starts_with("head") {
            f.head = true;
            f.head_url = quoted("head").unwrap_or_default();
        }
        if line.starts_with("keg_only") {
            f.keg_only = true;
        }
        if line.starts_with("deprecate!") || line.starts_with("disable!") {
            let disable = line.starts_with("disable!");
            let date = line
                .split("date:")
                .nth(1)
                .and_then(|s| cask_quoted(s.trim_start(), ""));
            let because = line.split("because:").nth(1).map(|s| {
                let s = s.trim().trim_matches(|ch| ch == '"' || ch == ',');
                s.strip_prefix(':').unwrap_or(s).to_string()
            });
            if disable {
                f.disable = true;
                f.deprecate_disable.disable_date = date;
                f.deprecate_disable.disable_reason = because;
            } else {
                f.deprecate = true;
                f.deprecate_disable.deprecation_date = date;
                f.deprecate_disable.deprecation_reason = because;
            }
        }
        if let Some(dep) = quoted("depends_on")
            && !dep.is_empty()
            && !dep.contains(' ')
        {
            if line.contains(":build") {
                f.build_dependencies.push(dep);
            } else if line.contains(":test") {
                f.test_dependencies.push(dep);
            } else if line.contains(":recommended") {
                f.recommended_dependencies.push(dep);
            } else if line.contains(":optional") {
                f.optional_dependencies.push(dep);
            } else {
                f.dependencies.push(dep);
            }
        }
        if let Some(conflict) = quoted("conflicts_with")
            && !conflict.is_empty()
        {
            f.conflicts.push(conflict);
        }
    }
    if f.version.is_empty() {
        f.version = version_from_url(&f.stable_url);
    }
    f.service = parse_service_block(text, &f.name.clone()).map(Box::new);
    f.livecheck = parse_livecheck_block(text).map(|spec| {
        f.skip_livecheck = spec.skip.is_some();
        Box::new(spec)
    });
    f.no_autobump = text
        .lines()
        .any(|l| l.trim_start().starts_with("no_autobump!"));
    f
}

/// A tap formula's `service do ... end` block as data: the
/// run argv (brew's `$HOMEBREW_PREFIX` placeholder forms),
/// `require_root`, simple `environment_variables` pairs and
/// an explicit macOS name. Anything beyond plain literals
/// and `opt_bin`/`bin`-style joins (interpolation, computed
/// values) yields no service, the data-only limit.
fn parse_service_block(text: &str, name: &str) -> Option<super::formula::ServiceInfo> {
    let mut lines = text.lines().map(str::trim);
    lines.find(|l| *l == "service do" || l.starts_with("service do "))?;
    let mut block: Vec<String> = Vec::new();
    for line in lines {
        if line == "end" {
            break;
        }
        block.push(line.to_string());
    }
    let token = |tok: &str| -> Option<String> {
        let tok = tok.trim().trim_end_matches(',');
        if let Some(inner) = tok
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .filter(|s| !s.contains("#{"))
        {
            return Some(inner.to_string());
        }
        // receiver/"a"/"b" path joins.
        let (recv, rest) = tok.split_once('/')?;
        let base = match recv {
            "opt_bin" => format!("$HOMEBREW_PREFIX/opt/{name}/bin"),
            "opt_sbin" => format!("$HOMEBREW_PREFIX/opt/{name}/sbin"),
            "opt_libexec" => format!("$HOMEBREW_PREFIX/opt/{name}/libexec"),
            "bin" => "$HOMEBREW_PREFIX/bin".to_string(),
            "sbin" => "$HOMEBREW_PREFIX/sbin".to_string(),
            "libexec" => "$HOMEBREW_PREFIX/libexec".to_string(),
            "var" => "$HOMEBREW_PREFIX/var".to_string(),
            "etc" => "$HOMEBREW_PREFIX/etc".to_string(),
            _ => return None,
        };
        let mut out = base;
        let mut rest = rest;
        loop {
            let seg = rest.strip_prefix('"')?;
            let end = seg.find('"')?;
            let inner = &seg[..end];
            if inner.contains("#{") {
                return None;
            }
            out.push('/');
            out.push_str(inner);
            rest = seg[end + 1..].trim_start();
            match rest.strip_prefix('/') {
                Some(more) => rest = more,
                None => break,
            }
        }
        Some(out)
    };
    let mut run: Vec<String> = Vec::new();
    let mut require_root = false;
    let mut environment_variables: Vec<(String, String)> = Vec::new();
    let mut explicit_name: Option<String> = None;
    let mut working_dir: Option<String> = None;
    let mut root_dir: Option<String> = None;
    let mut log_path: Option<String> = None;
    let mut error_log_path: Option<String> = None;
    let mut interval: Option<i64> = None;
    let mut cron: Option<String> = None;
    // A bare directory receiver (`working_dir var`) or a
    // receiver/"seg" join, as a placeholder path.
    let path_token = |tok: &str| -> Option<String> {
        match tok.trim().trim_end_matches(',') {
            "var" => Some("$HOMEBREW_PREFIX/var".to_string()),
            "etc" => Some("$HOMEBREW_PREFIX/etc".to_string()),
            other => token(other),
        }
    };
    let mut it = block.iter().peekable();
    while let Some(line) = it.next() {
        if let Some(rest) = line.strip_prefix("run ") {
            let mut expr = rest.trim().to_string();
            while expr.starts_with('[') && !expr.contains(']') {
                expr.push(' ');
                expr.push_str(it.next()?);
            }
            let items: Vec<&str> =
                if let Some(inner) = expr.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                    inner.split(',').collect()
                } else {
                    vec![expr.as_str()]
                };
            for item in items {
                if item.trim().is_empty() {
                    continue;
                }
                run.push(token(item)?);
            }
        } else if line.starts_with("require_root") {
            require_root = line.contains("true");
        } else if let Some(rest) = line.strip_prefix("environment_variables ") {
            for pair in rest.split(',') {
                if let Some((key, value)) = pair.split_once(':')
                    && let Some(v) = token(value)
                {
                    environment_variables.push((key.trim().to_string(), v));
                }
            }
        } else if let Some(rest) = line.strip_prefix("name ") {
            let macos = rest.split("macos:").nth(1).unwrap_or(rest);
            explicit_name = cask_quoted(macos.trim_start(), "").or(explicit_name);
        } else if let Some(rest) = line.strip_prefix("working_dir ") {
            working_dir = path_token(rest).or(working_dir);
        } else if let Some(rest) = line.strip_prefix("root_dir ") {
            root_dir = path_token(rest).or(root_dir);
        } else if let Some(rest) = line.strip_prefix("log_path ") {
            log_path = path_token(rest).or(log_path);
        } else if let Some(rest) = line.strip_prefix("error_log_path ") {
            error_log_path = path_token(rest).or(error_log_path);
        } else if let Some(rest) = line.strip_prefix("interval ") {
            interval = rest.trim().parse().ok().or(interval);
        } else if let Some(rest) = line.strip_prefix("cron ") {
            cron = cask_quoted(rest.trim_start(), "").or(cron);
        }
    }
    if run.is_empty() {
        return None;
    }
    environment_variables.sort();
    Some(super::formula::ServiceInfo {
        run,
        require_root,
        environment_variables,
        name: explicit_name,
        working_dir,
        root_dir,
        log_path,
        error_log_path,
        interval,
        cron,
    })
}

fn version_from_url(url: &str) -> String {
    // The file name's first token (split on `-`/`_`) that is
    // exactly `v?<digits>(.<digits>)*`, archive suffixes
    // stripped — so `gate403-1.0.tar.gz` is 1.0, not the 403
    // embedded in the name. Falls back to the first digit
    // run when no token is version-shaped.
    let base = url.split(['?', '#']).next().unwrap_or_default();
    let mut name = base.rsplit('/').next().unwrap_or_default().to_string();
    const EXTS: &[&str] = &[
        ".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".tgz", ".tbz2", ".whl", ".zip", ".crate",
        ".tar", ".gz", ".bz2", ".xz", ".7z", ".gem", ".jar",
    ];
    loop {
        let lower = name.to_lowercase();
        let Some(ext) = EXTS.iter().find(|e| lower.ends_with(**e)) else {
            break;
        };
        name.truncate(name.len() - ext.len());
    }
    for tok in name.split(['-', '_']) {
        let t = tok.strip_prefix('v').unwrap_or(tok);
        if !t.is_empty()
            && t.split('.')
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        {
            return t.to_string();
        }
    }
    let digits: String = name
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    digits.trim_end_matches('.').to_string()
}

/// The first quoted string after `key` on a stanza line.
fn cask_quoted(line: &str, key: &str) -> Option<String> {
    let rest = line.strip_prefix(key)?.trim_start();
    // Double-quoted values may carry interpolation
    // with inner quotes (`#{"-#{...}" if ...}`).
    if rest.starts_with('"') {
        return quoted_token(rest).map(|(value, _)| value);
    }
    let inner = rest.strip_prefix('\'')?;
    let end = inner.find('\'')?;
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
fn cask_interpolate(
    text: &str,
    version: &str,
    locals: &[(String, String)],
    unsupported: &mut Option<&'static str>,
) -> String {
    cask_interpolate_depth(text, version, locals, unsupported, 0)
}

fn cask_interpolate_depth(
    text: &str,
    version: &str,
    locals: &[(String, String)],
    unsupported: &mut Option<&'static str>,
    depth: usize,
) -> String {
    if !text.contains("#{") {
        return text.to_string();
    }
    let csv: Vec<&str> = version.split(',').collect();
    let first = csv.first().copied().unwrap_or(version);
    let mut nums: Vec<&str> = first.split('.').collect();
    while nums.len() < 3 {
        nums.push("0");
    }
    // `A || B` over csv parts: a missing csv part is
    // Ruby nil, so the fallback wins (jasp's
    // `version.csv.second || version.csv.first`).
    let csv_part = |expr: &str| -> Option<String> {
        match expr {
            "version.csv.first" => Some(first.to_string()),
            "version.csv.second" => csv.get(1).map(|s| (*s).to_string()),
            "version.csv.third" => csv.get(2).map(|s| (*s).to_string()),
            "version.csv.fourth" => csv.get(3).map(|s| (*s).to_string()),
            "version.csv.fifth" => csv.get(4).map(|s| (*s).to_string()),
            _ => None,
        }
    };
    let value = |expr: &str| -> Option<String> {
        // A quoted conditional string whose predicate
        // the formula evaluator models (its literal
        // may itself interpolate version chains).
        if super::formula::version_conditional_string_syntax(expr) {
            return super::formula::cask_version_token(version, expr);
        }
        // `"-#{version.csv.second}" if version.csv.second`:
        // a quoted string (itself interpolated) kept
        // only when the condition chain is present.
        if expr.starts_with('"') {
            let (inner, tail) = quoted_token(expr)?;
            let tail = tail.trim();
            if tail.is_empty() {
                if depth >= 8 {
                    return None;
                }
                let mut scratch = None;
                let v = cask_interpolate_depth(&inner, version, locals, &mut scratch, depth + 1);
                return if scratch.is_some() { None } else { Some(v) };
            }
            let cond = tail.strip_prefix("if ")?.trim();
            let cond = cond.strip_suffix(".present?").unwrap_or(cond);
            let present =
                super::formula::cask_version_token(version, cond).is_some_and(|v| !v.is_empty());
            if !present {
                return Some(String::new());
            }
            if depth >= 8 {
                return None;
            }
            let mut scratch = None;
            let v = cask_interpolate_depth(&inner, version, locals, &mut scratch, depth + 1);
            return if scratch.is_some() { None } else { Some(v) };
        }
        // Split pipelines first: a map body may itself
        // contain `||` (the indexed zero-pad form),
        // which the fallback split below would sever.
        if expr.contains(".split(")
            && let Some(v) = super::formula::cask_version_token(version, expr)
        {
            return Some(v);
        }
        if let Some((a, b)) = expr.split_once(" || ") {
            // Either side may be a csv selector (nil
            // when the part is missing), bare
            // `version`, or a full chain.
            let side = |s: &str| -> Option<String> {
                let s = s.trim();
                if s == "version" {
                    return Some(version.to_string());
                }
                if let Some((lit, t)) = quoted_token(s)
                    && t.trim().is_empty()
                {
                    return Some(lit);
                }
                // A csv selector is nil exactly when its
                // part is missing (the token evaluator's
                // terminal "" must not win the `||`).
                if matches!(
                    s,
                    "version.csv.first"
                        | "version.csv.second"
                        | "version.csv.third"
                        | "version.csv.fourth"
                        | "version.csv.fifth"
                ) {
                    return csv_part(s);
                }
                super::formula::cask_version_token(version, s)
            };
            return side(a).or_else(|| side(b));
        }
        // Bare conditional chain:
        // `version.csv.second if version.csv.second`
        // keeps the value only when the condition's
        // csv part exists (a missing part is nil).
        if let Some((lhs, cond)) = expr.split_once(" if ") {
            let (lhs, cond) = (lhs.trim(), cond.trim());
            let cond = cond.strip_suffix(".present?").unwrap_or(cond);
            let exists = match cond {
                "version.csv.first" => true,
                "version.csv.second" => csv.len() > 1,
                "version.csv.third" => csv.len() > 2,
                "version.csv.fourth" => csv.len() > 3,
                "version.csv.fifth" => csv.len() > 4,
                _ => {
                    super::formula::cask_version_token(version, cond).is_some_and(|v| !v.is_empty())
                }
            };
            if !exists {
                return Some(String::new());
            }
            return csv_part(lhs)
                .or_else(|| super::formula::cask_version_token(version, lhs))
                .or_else(|| (lhs == "version").then(|| version.to_string()));
        }
        // `version.csv.first.<accessor>`: the numeric
        // accessors already read the first csv part.
        let normalized;
        let expr = if let Some(rest) = expr.strip_prefix("version.csv.first.") {
            normalized = format!("version.{rest}");
            normalized.as_str()
        } else {
            expr
        };
        // Full `Cask::DSL::Version` chains the
        // explicit cases below do not name
        // (`version.csv.second.major`, ...).
        if let Some(v) = super::formula::cask_version_token(version, expr)
            && !matches!(
                expr,
                "version.no_dots"
                    | "version.before_comma"
                    | "version.after_comma"
                    | "version.csv.second"
                    | "version.csv.third"
                    | "version.dots_to_underscores"
                    | "version.dots_to_hyphens"
            )
        {
            return Some(v);
        }
        // `<local>.delete_prefix("x")` chains on cask
        // locals (julia's `arch.delete_prefix("x")`).
        if let Some((name, tail)) = expr.split_once('.')
            && let Some(raw0) = locals
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.clone())
        {
            if depth >= 8 {
                return None;
            }
            let mut scratch = None;
            let mut v = cask_interpolate_depth(&raw0, version, locals, &mut scratch, depth + 1);
            if scratch.is_some() {
                return None;
            }
            let mut rest = format!(".{tail}");
            loop {
                let t = rest.trim().to_string();
                if t.is_empty() {
                    break;
                }
                let lit_op = |t: &str, op: &str| -> Option<(String, String)> {
                    let r = t.strip_prefix(op)?;
                    let (lit, r) = quoted_token(r.trim_start())?;
                    let r = r.trim().strip_prefix(')')?;
                    Some((lit, r.to_string()))
                };
                if let Some((lit, r)) = lit_op(&t, ".delete_prefix(") {
                    if let Some(stripped) = v.strip_prefix(&lit) {
                        v = stripped.to_string();
                    }
                    rest = r;
                } else if let Some((lit, r)) = lit_op(&t, ".delete_suffix(") {
                    if let Some(stripped) = v.strip_suffix(&lit) {
                        v = stripped.to_string();
                    }
                    rest = r;
                } else if let Some((lit, r)) = lit_op(&t, ".delete(") {
                    if lit.contains('\\') || lit.contains('^') {
                        return None;
                    }
                    // A delete set with `a-z` ranges.
                    let cs: Vec<char> = lit.chars().collect();
                    let mut set: Vec<char> = Vec::new();
                    let mut i = 0;
                    while i < cs.len() {
                        if i + 2 < cs.len() && cs[i + 1] == '-' {
                            let (a, b) = (cs[i] as u32, cs[i + 2] as u32);
                            if a > b {
                                return None;
                            }
                            set.extend((a..=b).filter_map(char::from_u32));
                            i += 3;
                        } else {
                            set.push(cs[i]);
                            i += 1;
                        }
                    }
                    v = v.chars().filter(|c| !set.contains(c)).collect();
                    rest = r;
                } else {
                    let word_op = |t: &str, op: &str| -> Option<String> {
                        let r = t.strip_prefix(op)?;
                        if r.is_empty() || r.starts_with('.') {
                            Some(r.to_string())
                        } else {
                            None
                        }
                    };
                    if let Some(r) = word_op(&t, ".strip") {
                        v = v.trim().to_string();
                        rest = r;
                    } else if let Some(r) = word_op(&t, ".downcase") {
                        v = v.to_lowercase();
                        rest = r;
                    } else if let Some(r) = word_op(&t, ".upcase") {
                        v = v.to_uppercase();
                        rest = r;
                    } else if let Some(r) = word_op(&t, ".to_s") {
                        rest = r;
                    } else {
                        return None;
                    }
                }
            }
            return Some(v);
        }
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
            other => {
                // Cask locals (`#{arch}`, `#{os}`,
                // conditional folders/endings): their
                // values interpolate recursively. An
                // unresolved token inside a local makes
                // the whole token unsupported.
                let raw = locals.iter().find(|(n, _)| n == other)?.1.clone();
                if depth >= 8 {
                    return None;
                }
                let mut scratch = None;
                let v = cask_interpolate_depth(&raw, version, locals, &mut scratch, depth + 1);
                if scratch.is_some() {
                    return None;
                }
                v
            }
        })
    };
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("#{") {
        out.push_str(&rest[..start]);
        let inner = &rest[start + 2..];
        // Brace-aware span: nested `#{}` inside a
        // quoted conditional string does not end it.
        // Modes: `None` interpolation code, `Some(q)`
        // inside a quoted string.
        let bytes = inner.as_bytes();
        let mut modes: Vec<Option<u8>> = vec![None];
        let mut i = 0usize;
        let mut end: Option<usize> = None;
        while i < bytes.len() {
            match modes.last().copied().flatten() {
                Some(q) => {
                    if bytes[i] == b'\\' {
                        i += 1;
                    } else if bytes[i] == q {
                        modes.pop();
                    } else if bytes[i] == b'#' && bytes.get(i + 1) == Some(&b'{') {
                        modes.push(None);
                        i += 1;
                    }
                }
                None => match bytes[i] {
                    b'"' | b'\'' => modes.push(Some(bytes[i])),
                    b'{' => modes.push(None),
                    b'}' => {
                        modes.pop();
                        if modes.is_empty() {
                            end = Some(i);
                            break;
                        }
                    }
                    _ => {}
                },
            }
            i += 1;
        }
        let Some(end) = end else {
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

/// Whether a trimmed Ruby line opens a `do … end` block.
/// A full-line comment never opens or closes one, even
/// when it quotes a block opener in prose.
fn line_opens_block(t: &str) -> bool {
    !t.starts_with('#')
        && (t.ends_with(" do")
            || t.contains(" do |")
            || t.starts_with("if ")
            // An assignment-if expression (`v = if …`)
            // opens a block its `end` closes.
            || t.contains(" = if ")
            || t.starts_with("unless ")
            || t.starts_with("while ")
            || t.starts_with("until ")
            || t.starts_with("case ")
            || t.starts_with("for ")
            || t == "begin")
}

/// A line that closes a Ruby block: bare `end`, or
/// `end` with a chained call (`end.flatten`,
/// `end.join(…)`) or a trailing comment.
fn line_closes_block(t: &str) -> bool {
    t == "end"
        || t.strip_prefix("end")
            .is_some_and(|r| r.starts_with(['.', ')', ' ', '#']) || r.starts_with("&."))
}

/// One quoted string token at the start of `s` (double quotes,
/// interpolation already refused by the caller).
fn quoted_token(s: &str) -> Option<(String, &str)> {
    // Percent strings: `%Q(...)`, `%Q{...}`, …
    // (double-quoted semantics).
    if let Some(rest) = s.strip_prefix("%Q") {
        let mut chars = rest.char_indices();
        let (_, open) = chars.next()?;
        let inner = &rest[open.len_utf8()..];
        let close = match open {
            '(' => ')',
            '[' => ']',
            '{' => '}',
            '<' => '>',
            _ => open,
        };
        let nesting = close != open;
        let bytes = inner.as_bytes();
        let mut i = 0usize;
        let mut depth = 1usize;
        while i < bytes.len() {
            match bytes[i] {
                b'\\' => i += 1,
                b if b == open as u32 as u8 && nesting && open.is_ascii() => depth += 1,
                b if b == close as u32 as u8 && close.is_ascii() => {
                    depth -= 1;
                    if depth == 0 {
                        return Some((inner[..i].to_string(), &inner[i + 1..]));
                    }
                }
                _ => {}
            }
            i += 1;
        }
        return None;
    }
    let inner = s.strip_prefix('"')?;
    // Interpolation-aware: `\"` escapes and
    // `#{...}` spans (which may contain quotes,
    // e.g. `#{DateTime.now.strftime("%Q")}`) do
    // not end the string.
    let bytes = inner.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 1,
            b'"' => return Some((inner[..i].to_string(), &inner[i + 1..])),
            b'#' if bytes.get(i + 1) == Some(&b'{') => {
                let mut depth = 1usize;
                let mut quote: Option<u8> = None;
                i += 2;
                while i < bytes.len() && depth > 0 {
                    match quote {
                        Some(q) if bytes[i] == q => quote = None,
                        Some(_) if bytes[i] == b'\\' => i += 1,
                        Some(_) => {}
                        None if bytes[i] == b'"' || bytes[i] == b'\'' => {
                            quote = Some(bytes[i]);
                        }
                        None if bytes[i] == b'{' => depth += 1,
                        None if bytes[i] == b'}' => depth -= 1,
                        _ => {}
                    }
                    i += 1;
                }
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// A `cookies: { "k" => "v", … }` hash as one
/// `k=v;k2=v2` header value (the oracle's
/// `.join(";")`).
fn parse_cookie_header(
    value: &str,
    substitute: &impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let inner = value.trim().strip_prefix('{')?.strip_suffix('}')?.trim();
    let mut pairs = Vec::new();
    if !inner.is_empty() {
        for part in split_top_commas(inner) {
            let (k, rest) = part.trim().split_once("=>")?;
            let (k, _) = quoted_token(k.trim())?;
            let (v, _) = quoted_token(rest.trim())?;
            pairs.push(format!("{}={}", substitute(&k)?, substitute(&v)?));
        }
    }
    Some(pairs.join(";"))
}

/// A `:symbol` token at the start of `s`, colon stripped.
fn symbol_token(s: &str) -> Option<(String, &str)> {
    let inner = s.strip_prefix(':')?;
    let len = inner
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(inner.len());
    if len == 0 {
        return None;
    }
    Some((inner[..len].to_string(), &inner[len..]))
}

/// Net `{` depth outside string literals.
fn brace_depth(s: &str) -> i32 {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '{' => depth += 1,
            None if c == '}' => depth -= 1,
            None => {}
        }
    }
    depth
}

/// Split at commas at brace depth zero outside quotes.
fn split_top_commas(s: &str) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut depth = 0i32;
    for c in s.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '{' => depth += 1,
            None if c == '}' => depth -= 1,
            None if c == ',' && depth == 0 => {
                parts.push(std::mem::take(&mut cur));
                continue;
            }
            None => {}
        }
        cur.push(c);
    }
    parts.push(cur);
    parts
}

/// A `post_form:` hash literal as data: `{ key: "value",
/// ... }` in written order. Values go through the
/// caller's interpolation substitution (the `version`
/// family stays for check time; anything else refuses).
fn parse_post_form(
    value: &str,
    substitute: &impl Fn(&str) -> Option<String>,
    locals: &[(String, String)],
) -> Option<Vec<(String, String)>> {
    let inner = value.trim().strip_prefix('{')?.strip_suffix('}')?.trim();
    let mut out: Vec<(String, String)> = Vec::new();
    for entry in split_top_commas(inner) {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (key, rest) = entry.split_once(':')?;
        let key = key.trim();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
        let rest = rest.trim();
        let value = if rest.starts_with('\'') {
            let (raw, tail) = single_quoted_token(rest)?;
            if !tail.trim().is_empty() {
                return None;
            }
            substitute(&raw)?
        } else if rest.starts_with('"') {
            let (raw, tail) = quoted_token(rest)?;
            if !tail.trim().is_empty() {
                return None;
            }
            substitute(&raw)?
        } else if super::formula::version_chain_syntax(rest) {
            // A bare chain keeps its token for
            // check-time substitution.
            format!("#{{{rest}}}")
        } else if let Some((_, v)) = locals.iter().find(|(n, _)| n == rest) {
            v.clone()
        } else {
            return None;
        };
        out.push((key.to_string(), value));
    }
    (!out.is_empty()).then_some(out)
}

/// Ruby double-quoted escape processing
/// (`\#{` becomes the \u{3} sentinel so the
/// interpolation substitution skips it).
fn ruby_unescape_dq(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('s') => out.push(' '),
            Some('#') => {
                out.push('\u{3}');
                // The `#` was consumed; a following
                // `{` stays as the sentinel's brace
                // is emitted by the restore replace.
                match chars.next() {
                    Some('{') => {}
                    Some(other) => {
                        out.pop();
                        out.push('#');
                        out.push(other);
                    }
                    None => {
                        out.pop();
                        out.push('#');
                    }
                }
            }
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// A `post_json:` hash literal rendered as the
/// oracle's `JSON.generate` text (compact, written
/// order). Values: strings (interpolation
/// substituted), numbers, booleans, nil, symbols,
/// arrays and nested hashes.
fn parse_post_json(value: &str, substitute: &impl Fn(&str) -> Option<String>) -> Option<String> {
    fn value_json(
        s: &str,
        substitute: &impl Fn(&str) -> Option<String>,
    ) -> Option<(String, usize)> {
        let s = s.trim_start();
        if s.starts_with('"') {
            let (raw, tail) = quoted_token(s)?;
            // Ruby escape processing around the
            // interpolation substitution (`\#{` is a
            // literal, never a token).
            let unescaped = ruby_unescape_dq(&raw);
            let v = substitute(&unescaped)?.replace('\u{3}', "#{");
            let mut out = String::from("\"");
            for c in v.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                    c => out.push(c),
                }
            }
            out.push('"');
            Some((out, s.len() - tail.len()))
        } else if s.starts_with('\'') {
            let (raw, tail) = single_quoted_token(s)?;
            let mut out = String::from("\"");
            for c in raw.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    c => out.push(c),
                }
            }
            out.push('"');
            Some((out, s.len() - tail.len()))
        } else if s.starts_with('{') {
            let (obj, used) = hash_json(s, substitute)?;
            Some((obj, used))
        } else if let Some(mut rest) = s.strip_prefix('[') {
            let mut out = String::from("[");
            let mut used = 1usize;
            let mut first = true;
            loop {
                let t = rest.trim_start();
                used += rest.len() - t.len();
                rest = t;
                if let Some(r) = rest.strip_prefix(']') {
                    used += 1;
                    let _ = r;
                    out.push(']');
                    return Some((out, used));
                }
                if !first {
                    rest = rest.strip_prefix(',')?.trim_start();
                    used += 1;
                    let t = rest.trim_start();
                    used += rest.len() - t.len();
                    rest = t;
                    if let Some(r) = rest.strip_prefix(']') {
                        used += 1;
                        let _ = r;
                        out.push(']');
                        return Some((out, used));
                    }
                    out.push(',');
                }
                first = false;
                let (v, n) = value_json(rest, substitute)?;
                out.push_str(&v);
                used += n;
                rest = &rest[n..];
            }
        } else if let Some(rest) = s.strip_prefix(':') {
            // A symbol value generates its name string.
            let len = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            if len == 0 {
                return None;
            }
            Some((format!("\"{}\"", &rest[..len]), 1 + len))
        } else if s.starts_with("version") {
            // A bare version chain: keep the token
            // for check-time substitution as a JSON
            // string value.
            let len = s
                .find([',', '}', ']', ' ', '\t', '\n', '\r'])
                .unwrap_or(s.len());
            let tok = &s[..len];
            let chain = tok.strip_suffix(".to_s").unwrap_or(tok);
            if super::formula::version_chain_syntax(chain) || chain == "version[0]" {
                Some((format!("\"#{{{chain}}}\"",), len))
            } else {
                None
            }
        } else {
            let len = s
                .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+')))
                .unwrap_or(s.len());
            let tok = &s[..len];
            match tok {
                "true" | "false" => Some((tok.to_string(), len)),
                "nil" => Some(("null".to_string(), len)),
                _ => {
                    if tok.parse::<f64>().is_ok() && !tok.is_empty() {
                        Some((tok.to_string(), len))
                    } else {
                        None
                    }
                }
            }
        }
    }
    fn hash_json(s: &str, substitute: &impl Fn(&str) -> Option<String>) -> Option<(String, usize)> {
        let mut rest = s.trim_start().strip_prefix('{')?;
        let mut used = s.len() - rest.len();
        let mut out = String::from("{");
        let mut first = true;
        loop {
            let t = rest.trim_start();
            used += rest.len() - t.len();
            rest = t;
            if let Some(r) = rest.strip_prefix('}') {
                used += 1;
                let _ = r;
                out.push('}');
                return Some((out, used));
            }
            if !first {
                rest = rest.strip_prefix(',')?;
                used += 1;
                let t = rest.trim_start();
                used += rest.len() - t.len();
                rest = t;
                if let Some(r) = rest.strip_prefix('}') {
                    used += 1;
                    let _ = r;
                    out.push('}');
                    return Some((out, used));
                }
                out.push(',');
            }
            first = false;
            // Key: `sym:` / `:"sym"` / `"str" =>` / `'str' =>`.
            let (key, key_used) = if rest.starts_with('"') {
                let (k, tail) = quoted_token(rest)?;
                (k, rest.len() - tail.len())
            } else if rest.starts_with('\'') {
                let (k, tail) = single_quoted_token(rest)?;
                (k, rest.len() - tail.len())
            } else if let Some(r) = rest.strip_prefix(':') {
                // Either `:"sym":` or `:sym =>`.
                if r.starts_with('"') {
                    let (k, tail) = quoted_token(r)?;
                    (k, 1 + (r.len() - tail.len()))
                } else {
                    let len = r
                        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                        .unwrap_or(r.len());
                    if len == 0 {
                        return None;
                    }
                    (r[..len].to_string(), 1 + len)
                }
            } else {
                let len = rest
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(rest.len());
                if len == 0 {
                    return None;
                }
                (rest[..len].to_string(), len)
            };
            used += key_used;
            rest = &rest[key_used..];
            let t = rest.trim_start();
            used += rest.len() - t.len();
            rest = t;
            if let Some(r) = rest.strip_prefix(':') {
                used += 1;
                rest = r;
            } else {
                let r = rest.strip_prefix("=>")?;
                used += 2;
                rest = r;
            }
            let t = rest.trim_start();
            used += rest.len() - t.len();
            rest = t;
            let (v, n) = value_json(rest, substitute)?;
            used += n;
            rest = &rest[n..];
            out.push_str(&format!("\"{key}\":{v}"));
        }
    }
    let (json, _) = hash_json(value, substitute)?;
    Some(json)
}

/// The contents and option letters of a regex literal at the
/// start of `s` (`/…/` or `%r{…}`), plus the unparsed tail.
fn regex_literal(s: &str) -> Option<(String, String, &str)> {
    let (open, close, rest) = match s.strip_prefix('/') {
        Some(r) => ('/', '/', r),
        None => ('{', '}', s.strip_prefix("%r{")?),
    };
    let mut depth = 1usize;
    let mut escaped = false;
    // A `/` delimiter does not close inside a
    // character class (`/[^/]+/`) or an
    // interpolation (`#{arch.gsub(/[._-]/, "…")}`).
    let mut in_class = false;
    let mut class_fresh = false;
    let mut interp = 0usize;
    let mut interp_quote: Option<char> = None;
    let mut prev = '\0';
    for (i, c) in rest.char_indices() {
        if interp > 0 {
            if escaped {
                escaped = false;
            } else if c == '\\' && interp_quote.is_some() {
                escaped = true;
            } else if let Some(q) = interp_quote {
                if c == q {
                    interp_quote = None;
                }
            } else if c == '"' || c == '\'' {
                interp_quote = Some(c);
            } else if c == '{' {
                interp += 1;
            } else if c == '}' {
                interp -= 1;
            }
            prev = c;
            continue;
        }
        if c == '{' && prev == '#' && !in_class {
            interp = 1;
            prev = c;
            continue;
        }
        prev = c;
        if escaped {
            escaped = false;
            class_fresh = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        if open == '/' {
            if in_class {
                if c == ']' && !class_fresh {
                    in_class = false;
                }
                if c != '^' {
                    class_fresh = false;
                }
                continue;
            }
            if c == '[' {
                in_class = true;
                class_fresh = true;
                continue;
            }
        }
        if c == open && open != close {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                let tail = &rest[i + c.len_utf8()..];
                let flen = tail
                    .find(|c: char| !c.is_ascii_alphabetic())
                    .unwrap_or(tail.len());
                return Some((
                    rest[..i].to_string(),
                    tail[..flen].to_string(),
                    &tail[flen..],
                ));
            }
        }
    }
    None
}

/// Parse the package-level `livecheck do … end` block as data
/// (brew's livecheck reads these blocks from tap files; the
/// API JSON never carries them). Accepted, one statement per
/// line: `url` (:sym or "str", trailing options only flagged),
/// `regex(/…/flags)` / `regex(%r{…}flags)`, `strategy :sym`
/// (an attached `do … end` Ruby block is flagged, not read),
/// `skip ["msg"]`, `throttle [n] [days: m]`, `formula` /
/// The stable spec's explicit `version "..."` stanza:
/// class level (exactly two-space indent, tap style)
/// or directly inside a `stable do` block. Nested
/// `version` lines (`fails_with` compiler floors,
/// resource versions, test code) are not it — the
/// crude first-match scan in `parse_rb_as_data` reads
/// those as the formula version.
#[must_use]
pub fn spec_version_stanza(text: &str) -> Option<String> {
    let mut depth = 0usize;
    let mut stable_depth: Option<usize> = None;
    for line in text.lines() {
        let t = line.trim();
        if line_closes_block(t) {
            depth = depth.saturating_sub(1);
            if stable_depth.is_some_and(|d| depth < d) {
                stable_depth = None;
            }
            continue;
        }
        let class_level = line.len() > 2 && line.starts_with("  ") && !line[2..].starts_with(' ');
        let stable_level = stable_depth == Some(depth);
        if (class_level || stable_level)
            && let Some(rest) = t.strip_prefix("version")
        {
            let rest = rest.trim_start();
            let parsed = if rest.starts_with('\'') {
                single_quoted_token(rest)
            } else {
                quoted_token(rest)
            };
            if let Some((value, tail)) = parsed {
                let tail = tail.trim();
                if (tail.is_empty() || tail.starts_with('#')) && !value.contains("#{") {
                    return Some(value);
                }
            }
        }
        if line_opens_block(t) {
            depth += 1;
            if strip_trailing_comment(t).trim() == "stable do" {
                stable_depth = Some(depth);
            }
        }
    }
    None
}

/// The stable URL brew selects on this host when the
/// spec carries platform-conditional overrides
/// (`on_macos do … on_arm do url …`, bic): the
/// innermost matching block's literal `url`, an
/// arch-matched one preferred over a plain OS match.
/// `None` when no conditional URL applies.
#[must_use]
pub fn spec_platform_url(text: &str) -> Option<String> {
    #[derive(PartialEq, Clone, Copy)]
    enum Block {
        Os(bool),
        Arch(bool),
        Resource,
        Other,
    }
    let os_hit = |name: &str| -> Option<bool> {
        match name {
            "on_macos" => Some(cfg!(target_os = "macos")),
            "on_linux" => Some(cfg!(target_os = "linux")),
            _ => None,
        }
    };
    let arch_hit = |name: &str| -> Option<bool> {
        match name {
            "on_arm" => Some(cfg!(target_arch = "aarch64")),
            "on_intel" => Some(cfg!(target_arch = "x86_64")),
            _ => None,
        }
    };
    let mut stack: Vec<Block> = Vec::new();
    let mut os_url: Option<String> = None;
    let mut arch_url: Option<String> = None;
    for line in text.lines() {
        let t = line.trim();
        if line_closes_block(t) {
            stack.pop();
            continue;
        }
        let bare = strip_trailing_comment(t).trim();
        if !stack.contains(&Block::Resource)
            && !stack.contains(&Block::Os(false))
            && !stack.contains(&Block::Arch(false))
            && let Some(rest) = bare.strip_prefix("url ")
            && let Some((url, _)) = quoted_token(rest.trim_start())
            && !url.contains("#{")
        {
            if stack.contains(&Block::Arch(true)) {
                arch_url.get_or_insert(url);
            } else if stack.contains(&Block::Os(true)) {
                os_url.get_or_insert(url);
            }
        }
        if line_opens_block(t) {
            let head = bare.trim_end_matches(" do");
            stack.push(if bare.starts_with("resource ") {
                Block::Resource
            } else if let Some(hit) = os_hit(head) {
                Block::Os(hit)
            } else if let Some(hit) = arch_hit(head) {
                Block::Arch(hit)
            } else if head.starts_with("on_") {
                // An unmodeled conditional (on_system,
                // on_<release>): never a match.
                Block::Os(false)
            } else {
                Block::Other
            });
        }
    }
    arch_url.or(os_url)
}

/// The stable spec's git `tag: "..."`, from the
/// `url` statement naming `stable_url` (same line or
/// comma-continued option lines). Brew detects a
/// git formula's version from its tag, not the URL.
#[must_use]
pub fn spec_git_tag(text: &str, stable_url: &str) -> Option<String> {
    let needle = format!("\"{stable_url}\"");
    let mut lines = text.lines();
    for line in &mut lines {
        let t = line.trim();
        if !t.starts_with("url ") || !t.contains(&needle) {
            continue;
        }
        let mut stmt = t.to_string();
        while stmt.trim_end().ends_with(',') {
            stmt.push(' ');
            stmt.push_str(lines.next()?.trim());
        }
        let at = stmt.find("tag:")?;
        let rest = stmt[at + 4..].trim_start();
        let (tag, _) = if rest.starts_with('\'') {
            single_quoted_token(rest)?
        } else {
            quoted_token(rest)?
        };
        return Some(tag);
    }
    None
}

/// Whether `line` opens the package-level `livecheck` block
/// (exactly two-space indent, tap style).
fn livecheck_block_open(line: &str) -> bool {
    line.strip_prefix("  ").is_some_and(|r| {
        !r.starts_with(' ') && (r == "livecheck do" || r.starts_with("livecheck do "))
    })
}

/// The formula file's `resource` stanzas as data, in file
/// order: (name, url, explicit version). Resources nested in
/// a block for another OS (`on_linux` off Linux, `on_macos`
/// on it) or in the HEAD spec are not collected, matching
/// what a stable load on this machine would see. None when
/// a block's shape is outside the line scanner's subset (no
/// plain `url`, interpolation, unterminated block).
///
/// (name, url, explicit version, git tag, livecheck block)
pub type ResourceStanza = (
    String,
    String,
    Option<String>,
    Option<String>,
    Option<super::formula::LivecheckSpec>,
);

#[must_use]
pub fn parse_resource_stanzas(text: &str) -> Option<Vec<ResourceStanza>> {
    let indent_of = |line: &str| line.len() - line.trim_start().len();
    let mut out: Vec<ResourceStanza> = Vec::new();
    let mut on_stack: Vec<(usize, bool)> = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        let indent = indent_of(line);
        if trimmed == "end"
            && let Some(&(top, _)) = on_stack.last()
            && indent == top
        {
            on_stack.pop();
            continue;
        }
        if trimmed.starts_with("on_") && trimmed.ends_with(" do") {
            let other_os = if cfg!(target_os = "linux") {
                trimmed.starts_with("on_macos")
            } else {
                trimmed.starts_with("on_linux")
            };
            on_stack.push((indent, other_os));
            continue;
        }
        // Resources of the HEAD spec are not the stable
        // formula's; `brew livecheck` checks stable ones.
        if trimmed == "head do" {
            on_stack.push((indent, true));
            continue;
        }
        let Some(after) = trimmed.strip_prefix("resource \"") else {
            continue;
        };
        let name_end = after.find('"')?;
        let name = after[..name_end].to_string();
        if name.contains("#{") || !after[name_end..].contains(" do") {
            return None;
        }
        let collect = !on_stack.iter().any(|&(_, other)| other);
        let mut url: Option<String> = None;
        let mut tag: Option<String> = None;
        let mut version: Option<String> = None;
        let mut livecheck: Option<super::formula::LivecheckSpec> = None;
        let mut lc_seen = false;
        let mut closed = false;
        while let Some(inner) = lines.next() {
            let it = inner.trim();
            let inner_indent = indent_of(inner);
            if it == "end" && inner_indent == indent {
                closed = true;
                break;
            }
            // A resource may carry its own livecheck block;
            // extract it and parse with the package parser
            // (reindented so its opener matches). Its `url`
            // lines are not the resource's.
            if it == "livecheck do" && inner_indent > indent {
                if lc_seen {
                    return None;
                }
                lc_seen = true;
                let mut block = String::from("  livecheck do\n");
                let mut block_closed = false;
                for lcl in lines.by_ref() {
                    if lcl.trim() == "end" && indent_of(lcl) == inner_indent {
                        block_closed = true;
                        break;
                    }
                    block.push_str(lcl);
                    block.push('\n');
                }
                if !block_closed {
                    return None;
                }
                block.push_str("  end\n");
                livecheck = Some(parse_livecheck_block(&block)?);
                continue;
            }
            // The `tag:` spec of a git resource (its URL
            // line or a continuation line): brew detects
            // the resource version from the tag, not the
            // URL (Version.detect fetches :tag first).
            if tag.is_none()
                && let Some(rest) = it
                    .strip_prefix("tag:")
                    .or_else(|| it.split(", tag:").nth(1))
                && let Some(t) = rest
                    .trim_start()
                    .strip_prefix('"')
                    .and_then(|r| r.split('"').next())
            {
                if t.contains("#{") {
                    return None;
                }
                tag = Some(t.to_string());
            }
            if url.is_none()
                && let Some(u) = cask_quoted(it, "url")
            {
                if u.contains("#{") {
                    return None;
                }
                url = Some(u);
            } else if version.is_none()
                && let Some(v) = cask_quoted(it, "version")
            {
                version = Some(v);
            }
        }
        if !closed {
            return None;
        }
        if collect {
            let url = url?;
            out.push((name, url, version, tag, livecheck));
        }
    }
    Some(out)
}

/// Whether the tap file text carries a package-level
/// `livecheck` block at all (brew's `livecheck_defined?`, as
/// far as data can tell) — distinguishes "no block" from
/// "block [`parse_livecheck_block`] refused".
#[must_use]
pub fn livecheck_block_present(text: &str) -> bool {
    text.lines().any(livecheck_block_open)
}

/// `cask` references. Any interpolation or other statement
/// makes the whole block unparseable — fail-closed, the
/// `parse_service_block` discipline. The package-level block
/// is the one indented exactly two spaces (tap style);
/// resource-level blocks nest deeper and are not this.
#[must_use]
/// `key: "value", …` conditional kwargs. A value is
/// a plain quoted string or a nested
/// `on_*_conditional(…)` call, resolved for this
/// host on the spot.
fn conditional_kwargs(s: &str) -> Option<Vec<(String, String)>> {
    let s = s.trim();
    let s = match s.strip_prefix('(') {
        Some(r) => r.strip_suffix(')')?,
        None => s,
    };
    // Split at commas outside quotes and parens.
    let mut parts: Vec<String> = Vec::new();
    let (mut depth, mut quoted, mut cur) = (0i32, false, String::new());
    for c in s.chars() {
        match c {
            '"' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => depth -= 1,
            ',' if !quoted && depth == 0 => {
                parts.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    parts.push(cur);
    let mut out = Vec::new();
    for part in &parts {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (key, value) = part.split_once(':')?;
        let key = key.trim().to_string();
        let value = value.trim();
        let value = if let Some((v, tail)) = quoted_token(value) {
            if !tail.trim().is_empty() {
                return None;
            }
            v
        } else if let Some((kind, kw)) = ["on_arch_conditional", "on_system_conditional"]
            .iter()
            .find_map(|k| value.strip_prefix(k).map(|r| (*k, r)))
        {
            conditional_value(kind, &conditional_kwargs(kw.trim())?)?
        } else {
            return None;
        };
        out.push((key, value));
    }
    Some(out)
}

/// A cask's `on_arch_conditional` / `on_system_conditional`
/// value for this host: an absent side interpolates
/// as empty (Ruby `nil`).
fn conditional_value(kind: &str, kwargs: &[(String, String)]) -> Option<String> {
    let want = match kind {
        "on_arch_conditional" => super::machine::current_arch_symbol(),
        "on_system_conditional" => {
            if cfg!(target_os = "macos") {
                "macos"
            } else {
                "linux"
            }
        }
        _ => return None,
    };
    Some(
        kwargs
            .iter()
            .find(|(k, _)| k == want)
            .map_or_else(String::new, |(_, v)| v.clone()),
    )
}

/// Cask locals a livecheck `url`/`regex` may
/// interpolate, resolved for this host:
/// `<ident> = on_*_conditional …` assignments plus
/// the `arch`/`os` stanzas (as `arch`/`os`).
/// Regexp-valued cask locals declared before the
/// livecheck block: `<ident> = /…/flags` (possibly
/// spanning lines) or `<ident> = on_system_conditional(
/// macos: /…/flags, linux: /…/flags)` (the macOS arm).
/// `value.gsub(/<single character class>/, "repl")`
/// for cask locals: replace every character in the
/// class (ranges and negation supported).
fn gsub_char_class(value: &str, pat: &str, repl: &str) -> Option<String> {
    let inner = pat.strip_prefix('[')?.strip_suffix(']')?;
    let (negated, inner) = match inner.strip_prefix('^') {
        Some(r) => (true, r),
        None => (false, inner),
    };
    let mut set: Vec<char> = Vec::new();
    let chars: Vec<char> = inner.chars().collect();
    let mut i = 0usize;
    let read = |chars: &[char], i: &mut usize| -> Option<char> {
        let c = *chars.get(*i)?;
        *i += 1;
        if c == '\\' {
            let e = *chars.get(*i)?;
            *i += 1;
            Some(match e {
                'n' => '\n',
                't' => '\t',
                other => other,
            })
        } else {
            Some(c)
        }
    };
    while i < chars.len() {
        let lo = read(&chars, &mut i)?;
        if chars.get(i) == Some(&'-') && i + 1 < chars.len() {
            i += 1;
            let hi = read(&chars, &mut i)?;
            if lo > hi {
                return None;
            }
            set.extend(lo..=hi);
        } else {
            set.push(lo);
        }
    }
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if set.contains(&c) != negated {
            out.push_str(repl);
        } else {
            out.push(c);
        }
    }
    Some(out)
}

fn cask_regex_locals(lines: &[&str]) -> Vec<(String, String, String)> {
    let mut out: Vec<(String, String, String)> = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let t = lines[i].trim();
        i += 1;
        let Some((name, rhs)) = t.split_once(" = ") else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        let rhs = rhs.trim();
        // Gather the (possibly multi-line) RHS until a
        // regex literal parses out of it.
        let gathered = |start: &str, i: &mut usize| -> String {
            let mut joined = start.to_string();
            let mut n = 0usize;
            while regex_literal(&joined).is_none() && *i < lines.len() && n < 24 {
                joined.push('\n');
                joined.push_str(lines[*i]);
                *i += 1;
                n += 1;
            }
            joined
        };
        if rhs.starts_with('/') || rhs.starts_with('%') {
            let joined = gathered(rhs, &mut i);
            if let Some((pat, flags, _)) = regex_literal(&joined) {
                out.retain(|(n, _, _)| n != name);
                out.push((name.to_string(), pat, flags));
            }
        } else if rhs.starts_with("on_system_conditional") {
            // Gather only to the call's paren balance
            // (a paren-less call ends at the line end).
            let mut joined = rhs.to_string();
            let depth = |s: &str| -> i64 {
                s.chars()
                    .map(|c| match c {
                        '(' => 1,
                        ')' => -1,
                        _ => 0,
                    })
                    .sum()
            };
            let mut n = 0usize;
            while depth(&joined) > 0 && i < lines.len() && n < 24 {
                joined.push('\n');
                joined.push_str(lines[i]);
                i += 1;
                n += 1;
            }
            let joined = joined;
            if let Some(pos) = joined.find("macos:") {
                let after = joined[pos + "macos:".len()..].trim_start();
                if let Some((pat, flags, _)) = regex_literal(after) {
                    out.retain(|(n, _, _)| n != name);
                    out.push((name.to_string(), pat, flags));
                }
            }
        }
    }
    out
}

fn cask_livecheck_locals(lines: &[&str]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let set = |name: &str, value: String, out: &mut Vec<(String, String)>| {
        // `version` is the DSL stanza, never a cask
        // local: assignments inside strategy blocks
        // (`version = "#{match[2]}…"`) must not shadow
        // `#{version.…}` interpolation.
        if name == "version" {
            return;
        }
        if let Some(slot) = out.iter_mut().find(|(n, _)| n == name) {
            slot.1 = value;
        } else {
            out.push((name.to_string(), value));
        }
    };
    for (line_idx, line) in lines.iter().enumerate() {
        let t = line.trim();
        // `arch arm: "…", intel: "…"` / `os macos: "…", linux: "…"`
        for (stanza, kind) in [
            ("arch", "on_arch_conditional"),
            ("os", "on_system_conditional"),
        ] {
            if let Some(rest) = t
                .strip_prefix(stanza)
                .and_then(|r| r.strip_prefix([' ', '(']))
                && let Some(kwargs) = conditional_kwargs(rest)
                && let Some(v) = conditional_value(kind, &kwargs)
            {
                set(stanza, v, &mut out);
            }
        }
        // The default `language` block's trailing
        // string is the `#{language}` value the API
        // renders (firefox@nightly's "en-US").
        if t.starts_with("language ") && t.contains("default: true") && t.ends_with(" do") {
            let mut value: Option<String> = None;
            for next in lines.iter().skip(line_idx + 1) {
                let n = next.trim();
                if n == "end" {
                    break;
                }
                if let Some((v, tail)) = quoted_token(n)
                    && tail.trim().is_empty()
                {
                    value = Some(v);
                }
            }
            if let Some(v) = value {
                set("language", v, &mut out);
            }
        }
        // `<ident> = "…"` plain strings (kept raw; any
        // interpolation inside them resolves when the
        // local itself is substituted).
        if let Some((name, rhs)) = t.split_once(" = ")
            && let Some((value, tail)) = quoted_token(rhs.trim())
            && tail.trim().is_empty()
        {
            let name = name.trim();
            if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                set(name, value, &mut out);
            }
        }
        // `<ident> = version….split(…).each_with_index.map
        // do |n, i|` + body + `end.join…` (silentknight's
        // no-dot version): keep it as one brace-form
        // interpolation token, which the split pipeline
        // in `cask_version_token` evaluates.
        if let Some((name, rhs)) = t.split_once(" = ") {
            let name = name.trim();
            if !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && let Some((head, params)) = rhs.trim().split_once(".each_with_index.map do |")
                && head.starts_with("version")
                && head.contains(".split(")
                && let Some(params) = params.strip_suffix('|')
                && let Some(body) = lines.get(line_idx + 1)
                && let Some(end) = lines.get(line_idx + 2)
                && let Some(suffix) = end.trim().strip_prefix("end")
                && suffix.starts_with(".join")
            {
                let mut v = String::from("#{");
                v.push_str(head);
                v.push_str(".each_with_index.map { |");
                v.push_str(params);
                v.push_str("| ");
                v.push_str(body.trim());
                v.push_str(" }");
                v.push_str(suffix);
                v.push('}');
                set(name, v, &mut out);
            }
        }
        // `<ident> = on_arch_conditional …` / `on_system_conditional …`
        if let Some((name, rhs)) = t.split_once(" = ") {
            let name = name.trim();
            if !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && let Some((kind, kw)) = ["on_arch_conditional", "on_system_conditional"]
                    .iter()
                    .find_map(|k| rhs.trim().strip_prefix(k).map(|r| (*k, r)))
                && let Some(kwargs) = conditional_kwargs(kw.trim())
                && let Some(v) = conditional_value(kind, &kwargs)
            {
                set(name, v, &mut out);
            }
        }
    }
    out
}

/// The epoch-seconds value of a `#{Time.now.to_i}`
/// cache-buster token (optionally `* N`).
fn time_now_token(token: &str, epoch: u64) -> Option<String> {
    let rest = token.strip_prefix("Time.now.to_i")?.trim();
    if rest.is_empty() {
        return Some(epoch.to_string());
    }
    let n: u64 = rest.strip_prefix('*')?.trim().parse().ok()?;
    Some((epoch * n).to_string())
}

pub fn parse_livecheck_block(text: &str) -> Option<super::formula::LivecheckSpec> {
    use super::formula::{LivecheckSpec, LivecheckUrl};
    let all: Vec<&str> = text.lines().collect();
    let open_idx = all.iter().position(|l| livecheck_block_open(l))?;
    let locals = cask_livecheck_locals(&all[..open_idx]);
    let cask_regex_locals = cask_regex_locals(&all[..open_idx]);
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    // Resolvable interpolations: cask locals and
    // `Time.now.to_i` substitute here, at spec load —
    // the moment brew evaluates the string. The
    // `version` family stays verbatim for check time.
    let token_value = |token: &str| -> Option<Option<String>> {
        if let Some(v) = time_now_token(token, epoch) {
            return Some(Some(v));
        }
        if token == "DateTime.now.strftime(\"%Q\")" {
            return Some(Some((epoch * 1000).to_string()));
        }
        if token == "DateTime.now.strftime(\"%s\")" {
            return Some(Some(epoch.to_string()));
        }
        if let Some((_, v)) = locals.iter().find(|(n, _)| n == token) {
            return Some(Some(v.clone()));
        }
        // A cask local with a single-class gsub
        // (`#{arch.gsub(/[._-]/, "[._-]")}`).
        if let Some((base, args)) = token.split_once(".gsub(")
            && let Some((_, v)) = locals.iter().find(|(n, _)| n == base)
            && let Some((pat, _flags, rest)) = regex_literal(args.trim())
            && let Some(rest) = rest.trim().strip_prefix(',')
            && let Some((repl, tail)) = quoted_token(rest.trim())
            && tail.trim() == ")"
            && let Some(out) = gsub_char_class(v, &pat, &repl)
        {
            return Some(Some(out));
        }
        // A cask local with a case transform
        // (`#{arch.downcase}`).
        for (suffix, lower) in [(".downcase", true), (".upcase", false)] {
            if let Some(base) = token.strip_suffix(suffix)
                && let Some((_, v)) = locals.iter().find(|(n, _)| n == base)
            {
                return Some(Some(if lower {
                    v.to_lowercase()
                } else {
                    v.to_uppercase()
                }));
            }
        }
        let version_chain = super::formula::version_chain_syntax(token)
            || super::formula::version_ternary_syntax(token)
            || super::formula::version_conditional_string_syntax(token)
            || token == "version[0]"
            || token
                .strip_prefix("Regexp.escape(")
                .and_then(|t| t.strip_suffix(')'))
                .is_some_and(super::formula::version_chain_syntax);
        if token == "LATEST_VERSION" || version_chain {
            return Some(None);
        }
        None
    };
    let substitute_once = |s: &str| -> Option<String> {
        let mut out = String::with_capacity(s.len());
        let mut rest = s;
        while let Some(i) = rest.find("#{") {
            let j = rest[i + 2..].find('}')?;
            let token = &rest[i + 2..i + 2 + j];
            out.push_str(&rest[..i]);
            match token_value(token)? {
                Some(v) => out.push_str(&v),
                None => {
                    out.push_str("#{");
                    out.push_str(token);
                    out.push('}');
                }
            }
            rest = &rest[i + 2 + j + 1..];
        }
        out.push_str(rest);
        Some(out)
    };
    // A local's value may itself interpolate another
    // (`os = "_macOS_#{arch}.dmg"`): substitute to a
    // bounded fixpoint.
    let substitute = |s: &str| -> Option<String> {
        let mut cur = s.to_string();
        for _ in 0..8 {
            let next = substitute_once(&cur)?;
            if next == cur {
                return Some(next);
            }
            cur = next;
        }
        Some(cur)
    };
    let lines = all[open_idx + 1..].iter().copied();
    let mut body: Vec<&str> = Vec::new();
    let mut depth = 1usize;
    for line in lines {
        let t = line.trim();
        if line_closes_block(t) {
            depth -= 1;
            if depth == 0 {
                break;
            }
        } else if line_opens_block(t) {
            depth += 1;
        }
        body.push(t);
    }
    if depth != 0 {
        return None;
    }
    let mut spec = LivecheckSpec::default();
    let mut depth = 1usize;
    let mut idx = 0usize;
    // A `:json`/`:xml`/`:yaml` block's body lines, while
    // inside it: (strategy symbol, params, lines).
    let mut json_capture: Option<(String, String, Vec<String>)> = None;
    while idx < body.len() {
        let t = body[idx];
        idx += 1;
        if line_closes_block(t) {
            depth -= 1;
            if let Some((sym, params, lines)) = &mut json_capture {
                if depth == 1 {
                    match sym.as_str() {
                        "xml" => {
                            spec.xml_block = parse_xml_shape(params, lines).or_else(|| {
                                let subst: Vec<String> = lines
                                    .iter()
                                    .map(|l| substitute_local_tokens(l, &locals))
                                    .collect();
                                parse_xml_template_shape(params, &subst)
                            })
                        }
                        "yaml" | "electron_builder" => {
                            spec.yaml_block = parse_yaml_shape(params, lines)
                        }
                        "extract_plist" => spec.plist_block = parse_plist_shape(params, lines),
                        "page_match" | "sourceforge" | "gnome" => {
                            spec.page_block = parse_page_shape(params, lines).or_else(|| {
                                let subst: Vec<String> = lines
                                    .iter()
                                    .map(|l| substitute_local_tokens(l, &locals))
                                    .collect();
                                parse_page_shape(params, &subst)
                            })
                        }
                        "github_releases" => {
                            let subst: Vec<String> = lines
                                .iter()
                                .map(|l| substitute_local_tokens(l, &locals))
                                .collect();
                            spec.ghrel_block = parse_ghrel_shape(params, &subst, &locals)
                        }
                        "git" => spec.git_block = parse_git_shape(params, lines),
                        "github_latest" => spec.latest_block = parse_latest_shape(params, lines),
                        "sparkle" => spec.sparkle_block = parse_sparkle_shape(params, lines),
                        "header_match" => spec.header_block = parse_header_shape(params, lines),
                        _ => {
                            spec.json_block =
                                parse_json_shape(params, lines, &locals).or_else(|| {
                                    let subst: Vec<String> = lines
                                        .iter()
                                        .map(|l| substitute_local_tokens(l, &locals))
                                        .collect();
                                    parse_json_template_shape(params, &subst, &locals)
                                })
                        }
                    }
                    json_capture = None;
                } else {
                    lines.push(t.to_string());
                }
            }
            continue;
        }
        if let Some((_, _, lines)) = &mut json_capture
            && depth >= 2
        {
            lines.push(t.to_string());
        }
        let opens = line_opens_block(t);
        // Interpolation in a statement we read is not data; a
        // strategy block's unread Ruby body may contain it.
        // The narrow exception is a `url` whose interpolations
        // are only `#{LATEST_VERSION}` (rewritten below to the
        // `<FORMULA_LATEST_VERSION>` form the oracle reports)
        // and `#{version}` with a component accessor (kept
        // verbatim; substituted at check time).
        if depth == 1 && t.contains("#{") {
            // A regex may only carry tokens that
            // substitute now (cask locals, Time): the
            // `version` family is check-time state a
            // stored pattern cannot defer.
            let is_regex = t.starts_with("regex(")
                || t.strip_prefix("regex ")
                    .map(str::trim_start)
                    .is_some_and(|r| r.starts_with('/') || r.starts_with("%r{"));
            let allowed = |line: &str| {
                let mut rest = line;
                while let Some(i) = rest.find("#{") {
                    let Some(j) = rest[i + 2..].find('}') else {
                        return false;
                    };
                    let token = &rest[i + 2..i + 2 + j];
                    match token_value(token) {
                        Some(Some(_)) => {}
                        // Version chains stay verbatim
                        // in urls and regexes alike; the
                        // checked unit's version is
                        // substituted at plan time.
                        Some(None) => {}
                        _ => return false,
                    }
                    rest = &rest[i + 2 + j + 1..];
                }
                true
            };
            if !(t.starts_with("url ") || t.starts_with("url(") || is_regex) || !allowed(t) {
                return None;
            }
        }
        if depth == 1 && !t.is_empty() && !t.starts_with('#') {
            if let Some(rest) = t.strip_prefix("url ") {
                let rest = rest
                    .trim_start()
                    .strip_prefix('(')
                    .unwrap_or(rest.trim_start());
                let (url, tail) = if rest.starts_with(':') {
                    let (sym, tail) = symbol_token(rest)?;
                    (LivecheckUrl::Symbol(sym), tail)
                } else {
                    let (s, tail) = quoted_token(rest)?;
                    let s = substitute(&s)?;
                    (
                        LivecheckUrl::Literal(
                            s.replace("#{LATEST_VERSION}", "<FORMULA_LATEST_VERSION>"),
                        ),
                        tail,
                    )
                };
                spec.url = Some(url);
                // Url options may continue on following lines
                // while the text ends with a comma or a hash
                // literal (`post_form: { ... }`) is open.
                let mut opts = tail.trim().trim_end_matches(')').trim().to_string();
                while opts.ends_with(',') || brace_depth(&opts) > 0 {
                    let next = body.get(idx)?;
                    idx += 1;
                    opts.push(' ');
                    opts.push_str(next.trim().trim_end_matches(')').trim());
                }
                let opts = opts.strip_prefix(',').unwrap_or(&opts).trim();
                if !opts.is_empty() {
                    for part in split_top_commas(opts) {
                        let part = part.trim();
                        if part.is_empty() {
                            continue;
                        }
                        let Some((key, value)) = part.split_once(':') else {
                            spec.url_options = true;
                            continue;
                        };
                        let value = value.trim();
                        match key.trim() {
                            "user_agent" => {
                                if let Some(sym) = value.strip_prefix(':') {
                                    if matches!(sym, "browser" | "fake" | "curl" | "default") {
                                        spec.user_agent = Some(format!(":{sym}"));
                                    } else {
                                        spec.url_options = true;
                                    }
                                } else if let Some((lit, _)) = quoted_token(value) {
                                    spec.user_agent = Some(lit);
                                } else {
                                    spec.url_options = true;
                                }
                            }
                            // The fetch sends headers on
                            // the wire identically to the
                            // oracle's `--referer`/`--cookie`
                            // args.
                            "referer" => match quoted_token(value) {
                                Some((lit, _)) => match substitute(&lit) {
                                    Some(v) => spec
                                        .header
                                        .get_or_insert_with(Vec::new)
                                        .push(format!("Referer: {v}")),
                                    None => spec.url_options = true,
                                },
                                None => spec.url_options = true,
                            },
                            "cookies" => match parse_cookie_header(value, &substitute) {
                                Some(h) => spec
                                    .header
                                    .get_or_insert_with(Vec::new)
                                    .push(format!("Cookie: {h}")),
                                None => spec.url_options = true,
                            },
                            // `compressed: false` omits
                            // `--compressed` from the
                            // fetch (brew's default sends
                            // it, and some servers answer
                            // with encodings curl cannot
                            // decode).
                            "compressed" if value == "false" => {
                                spec.compressed_false = true;
                            }
                            "post_form" => match parse_post_form(value, &substitute, &locals) {
                                Some(form) => spec.post_form = Some(form),
                                None => spec.url_options = true,
                            },
                            "post_json" => match parse_post_json(value, &substitute) {
                                Some(json) => spec.post_json = Some(json),
                                None => spec.url_options = true,
                            },
                            "header" => {
                                let headers = if let Some(arr) =
                                    value.strip_prefix('[').and_then(|v| v.strip_suffix(']'))
                                {
                                    let mut out = Vec::new();
                                    for part in split_top_commas(arr) {
                                        let part = part.trim();
                                        if part.is_empty() {
                                            continue;
                                        }
                                        match quoted_token(part) {
                                            Some((lit, _)) => {
                                                out.push(substitute(&lit)?);
                                            }
                                            None => {
                                                spec.url_options = true;
                                                out.clear();
                                                break;
                                            }
                                        }
                                    }
                                    (!out.is_empty()).then_some(out)
                                } else {
                                    quoted_token(value)
                                        .and_then(|(lit, _)| substitute(&lit).map(|v| vec![v]))
                                };
                                match headers {
                                    Some(h) => spec.header = Some(h),
                                    None => spec.url_options = true,
                                }
                            }
                            _ => spec.url_options = true,
                        }
                    }
                }
            } else if let Some(rest) = t.strip_prefix("regex(") {
                // `regex(<ident>)`: a Regexp-valued cask
                // local (bambu-studio@beta).
                if rest
                    .trim_start()
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                {
                    let (ident, tail) = rest.split_once(')')?;
                    let ident = ident.trim();
                    if !tail.trim().is_empty()
                        || ident.is_empty()
                        || !ident.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    {
                        return None;
                    }
                    let Some((_, pat, flags)) =
                        cask_regex_locals.iter().find(|(n, _, _)| n == ident)
                    else {
                        return None;
                    };
                    let pattern = substitute(pat)?.replace(
                        "#{Regexp.escape(version.major_minor)}",
                        super::formula::MAJOR_MINOR_SENTINEL,
                    );
                    spec.regex = Some(pattern);
                    spec.regex_flags = flags.clone();
                    continue;
                }
                // A regex literal may span lines (the
                // `x`-mode alternation in neo4j); join
                // following body lines until it closes.
                // Continuations join raw: the pattern
                // source keeps its indentation (the
                // oracle's `Regexp#inspect` prints it).
                let mut joined = rest.to_string();
                let (pattern, flags, tail) = loop {
                    if let Some(parsed) = regex_literal(joined.trim_start()) {
                        // Complete when the tail is the
                        // call's close (an argument comma
                        // may precede it on its own line).
                        let t = strip_trailing_comment(parsed.2).trim();
                        let t = t.strip_prefix(',').unwrap_or(t).trim();
                        if t == ")" {
                            break (parsed.0, parsed.1, ")".to_string());
                        }
                    }
                    body.get(idx)?;
                    let next = all[open_idx + 1 + idx];
                    idx += 1;
                    joined.push('\n');
                    joined.push_str(next);
                };
                let pattern = substitute(&pattern)?.replace(
                    "#{Regexp.escape(version.major_minor)}",
                    super::formula::MAJOR_MINOR_SENTINEL,
                );
                if strip_trailing_comment(&tail).trim() != ")" {
                    return None;
                }
                spec.regex = Some(pattern);
                spec.regex_flags = flags;
            } else if let Some(rest) = t.strip_prefix("regex ") {
                // The same regex declaration without call
                // parentheses (scribus): `regex %r{...}i`.
                let rest = rest.trim_start();
                if !(rest.starts_with('/') || rest.starts_with("%r{")) {
                    return None;
                }
                let mut joined = rest.to_string();
                let (pattern, flags, tail) = loop {
                    if let Some(parsed) = regex_literal(&joined) {
                        break (parsed.0, parsed.1, parsed.2.to_string());
                    }
                    body.get(idx)?;
                    let next = all[open_idx + 1 + idx];
                    idx += 1;
                    joined.push('\n');
                    joined.push_str(next);
                };
                let pattern = substitute(&pattern)?.replace(
                    "#{Regexp.escape(version.major_minor)}",
                    super::formula::MAJOR_MINOR_SENTINEL,
                );
                if !strip_trailing_comment(&tail).trim().is_empty() {
                    return None;
                }
                spec.regex = Some(pattern);
                spec.regex_flags = flags;
            } else if let Some(rest) = t.strip_prefix("strategy ") {
                let (sym, tail) = symbol_token(rest.trim_start())?;
                let tail = tail.trim();
                // Block-pass symbol form (`strategy
                // :sparkle, &:short_version`): a proc
                // reading one item field.
                if let Some(field) = tail.strip_prefix(", &:").map(|s| {
                    s.split(|c: char| !(c.is_alphanumeric() || c == '_'))
                        .next()
                        .unwrap_or_default()
                }) {
                    if sym == "json" && field == "to_s" {
                        spec.json_block = Some(super::formula::JsonBlock {
                            has_regex: false,
                            shape: super::formula::JsonShape::RootToS,
                        });
                        spec.strategy = Some(sym);
                        spec.strategy_block = true;
                        continue;
                    }
                    if sym == "page_match" && field == "strip" {
                        spec.page_block = Some(super::formula::PageBlock {
                            shape: super::formula::PageShape::PageStrip,
                        });
                        spec.strategy = Some(sym);
                        spec.strategy_block = true;
                        continue;
                    }
                    if sym != "sparkle" {
                        return None;
                    }
                    spec.sparkle_block = Some(super::formula::SparkleBlock {
                        all: false,
                        find: false,
                        channel: None,
                        match_guard: None,
                        present: Vec::new(),
                        needs_spec_regex: false,
                        own_if: None,
                        expr: super::formula::SpExpr::Field(sp_field(field)?),
                    });
                    spec.strategy = Some(sym);
                    spec.strategy_block = true;
                    continue;
                }
                if !(tail.is_empty()
                    || tail == "do"
                    || (tail.starts_with("do |") && tail.ends_with('|')))
                {
                    return None;
                }
                if (sym == "json"
                    || sym == "xml"
                    || sym == "yaml"
                    || sym == "electron_builder"
                    || sym == "github_releases"
                    || sym == "github_latest"
                    || sym == "git"
                    || sym == "page_match"
                    || sym == "gnome"
                    || sym == "sourceforge"
                    || sym == "sparkle"
                    || sym == "header_match"
                    || sym == "extract_plist")
                    && opens
                {
                    json_capture = Some((sym.clone(), tail.to_string(), Vec::new()));
                }
                spec.strategy = Some(sym);
                spec.strategy_block = opens;
            } else if t == "skip" {
                spec.skip = Some(String::new());
            } else if let Some(rest) = t.strip_prefix("skip ").or_else(|| t.strip_prefix("skip(")) {
                let (msg, tail) = quoted_token(rest.trim_start())?;
                if !matches!(tail.trim(), "" | ")") {
                    return None;
                }
                spec.skip = Some(msg);
            } else if let Some(rest) = t.strip_prefix("throttle ") {
                for part in rest.split(',') {
                    let part = part.trim();
                    if let Some(days) = part.strip_prefix("days:") {
                        spec.throttle_days = Some(days.trim().parse().ok()?);
                    } else {
                        spec.throttle = Some(part.parse().ok()?);
                    }
                }
            } else if let Some(rest) = t.strip_prefix("formula ") {
                let rest = rest.trim_start();
                spec.formula_ref = Some(if rest.starts_with(':') {
                    let (sym, tail) = symbol_token(rest)?;
                    if !tail.trim().is_empty() {
                        return None;
                    }
                    format!(":{sym}")
                } else {
                    let (name, tail) = quoted_token(rest)?;
                    if !tail.trim().is_empty() {
                        return None;
                    }
                    name
                });
            } else {
                // The remaining known statement; anything else
                // refuses the block (the `?`).
                let rest = t.strip_prefix("cask ")?;
                let (name, tail) = quoted_token(rest.trim_start())?;
                if !tail.trim().is_empty() {
                    return None;
                }
                spec.cask_ref = Some(name);
            }
        }
        if opens {
            depth += 1;
        }
    }
    Some(spec)
}

/// A strategy block's parameters: the root variable
/// name and whether a `regex` parameter is declared.
fn block_params(params: &str) -> Option<(String, bool)> {
    let inner = params.trim().strip_prefix("do")?.trim();
    let plist: Vec<&str> = if inner.is_empty() {
        Vec::new()
    } else {
        inner
            .strip_prefix('|')?
            .strip_suffix('|')?
            .split(',')
            .map(str::trim)
            .collect()
    };
    let root = *plist.first()?;
    if root.is_empty() || !root.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    Some((root.to_string(), plist.get(1) == Some(&"regex")))
}

/// Join a block's non-comment lines, collapsing
/// whitespace outside string literals.
fn join_block_lines(lines: &[String]) -> String {
    let mut joined = String::new();
    for line in lines {
        let l = line.trim();
        if l.starts_with('#') {
            continue;
        }
        let mut quoted = false;
        let mut prev_space = joined.ends_with(' ') || joined.is_empty();
        for c in l.chars() {
            if c == '"' {
                quoted = !quoted;
            }
            if !quoted && c.is_whitespace() {
                if !prev_space {
                    joined.push(' ');
                    prev_space = true;
                }
            } else {
                joined.push(c);
                prev_space = false;
            }
        }
        if !joined.ends_with(' ') {
            joined.push(' ');
        }
    }
    joined
}

/// Reduce an `:xml` strategy block body to a data shape:
/// `xml.get_elements("//a/b").map(&:text)` and the
/// regex-capture map forms.
fn parse_xml_shape(params: &str, lines: &[String]) -> Option<super::formula::XmlBlock> {
    use super::formula::{XmlBlock, XmlShape};
    let (root, has_regex) = block_params(params)?;
    let joined = join_block_lines(lines);
    let body = joined.trim();
    let rest = body.strip_prefix(&root)?.strip_prefix(".get_elements(")?;
    let (path_str, tail) = quoted_token(rest)?;
    let tail = tail.strip_prefix(')')?;
    let segs: Vec<String> = path_str
        .strip_prefix("//")?
        .split('/')
        .map(str::to_string)
        .collect();
    if segs.is_empty() || segs.iter().any(|s| s.is_empty()) {
        return None;
    }
    let shape = if tail == ".map(&:text)" {
        XmlShape::Texts(segs)
    } else {
        let rest = strip_map_open(tail)?;
        let (var, inner) = xml_map_parts(rest)?;
        let after = inner.strip_prefix(&var)?.trim();
        let ok = after == ".text[regex, 1]" || after == ".text&.[](regex, 1)";
        if !ok || !has_regex {
            return None;
        }
        XmlShape::TextsRegex(segs)
    };
    Some(XmlBlock { has_regex, shape })
}

/// Parse an XPath path (`//a/b[text()='x']`, `/a/b`)
/// in the enumerable subset.
fn xml_path_parse(s: &str) -> Option<super::formula::XmlPathQ> {
    use super::formula::{XmlPathQ, XmlPred, XmlStep};
    if s.contains("..") {
        return None;
    }
    let (absolute, relative, rest) = if let Some(r) = s.strip_prefix("//") {
        (false, false, r)
    } else if let Some(r) = s.strip_prefix('/') {
        (true, false, r)
    } else {
        (false, true, s)
    };
    // Split on '/' outside brackets/quotes.
    let mut steps_str: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    for c in rest.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c == '[' => depth += 1,
            None if c == ']' => depth = depth.checked_sub(1)?,
            None if c == '/' && depth == 0 => {
                steps_str.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    steps_str.push(cur);
    let mut steps = Vec::new();
    for part in steps_str {
        let part = part.trim();
        if part.is_empty() {
            return None;
        }
        let (tag, pred) = match part.find('[') {
            None => (part.to_string(), None),
            Some(open) => {
                let inner = part[open + 1..].strip_suffix(']')?.trim();
                let pred = if let Some((left, right)) =
                    inner.split_once("/following-sibling::*[1][text()=")
                {
                    let (child, child_text) = left
                        .strip_suffix(']')
                        .and_then(|l| l.split_once("[text()="))
                        .and_then(|(tag, v)| Some((tag.trim().to_string(), xml_pred_value(v)?)))?;
                    if child.is_empty()
                        || !child.chars().all(|c| {
                            c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ':' | '.')
                        })
                    {
                        return None;
                    }
                    Some(XmlPred::SiblingAfterChild {
                        child,
                        child_text,
                        text: xml_pred_value(right.strip_suffix(']')?)?,
                    })
                } else if let Ok(n) = inner.parse::<usize>() {
                    Some(XmlPred::Index(n))
                } else if let Some(v) = inner.strip_prefix("text()=") {
                    Some(XmlPred::TextEq(xml_pred_value(v)?))
                } else if inner.starts_with('@') && inner.contains(" and ") {
                    let mut all = Vec::new();
                    for term in inner.split(" and ") {
                        let term = term.trim();
                        if let Some(r) = term.strip_prefix("not(@") {
                            all.push((r.strip_suffix(')')?.trim().to_string(), None));
                        } else {
                            let r = term.strip_prefix('@')?;
                            let (attr, v) = r.split_once('=')?;
                            all.push((attr.trim().to_string(), Some(xml_pred_value(v.trim())?)));
                        }
                    }
                    Some(XmlPred::AttrAll(all))
                } else if let Some(r) = inner
                    .strip_prefix("contains(@")
                    .and_then(|r| r.strip_suffix(')'))
                {
                    let (attr, v) = r.split_once(',')?;
                    Some(XmlPred::AttrContains(
                        attr.trim().to_string(),
                        xml_pred_value(v.trim())?,
                    ))
                } else if let Some(r) = inner.strip_prefix('@') {
                    let (attr, v) = r.split_once('=')?;
                    Some(XmlPred::AttrEq(
                        attr.trim().to_string(),
                        xml_pred_value(v.trim())?,
                    ))
                } else if let Some((child, v)) = inner.split_once("[text()=") {
                    // `[Child[text()='v']]` — a child
                    // element whose text equals `v`.
                    let child = child.trim();
                    if child.is_empty()
                        || !child.chars().all(|c| {
                            c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ':' | '.')
                        })
                    {
                        return None;
                    }
                    Some(XmlPred::ChildTextEq(
                        child.to_string(),
                        xml_pred_value(v.trim().strip_suffix(']')?)?,
                    ))
                } else {
                    let (child, v) = inner.split_once('=')?;
                    let child = child.trim();
                    if child.is_empty()
                        || !child
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ':'))
                    {
                        return None;
                    }
                    Some(XmlPred::ChildTextEq(
                        child.to_string(),
                        xml_pred_value(v.trim())?,
                    ))
                };
                (part[..open].trim().to_string(), pred)
            }
        };
        if tag.is_empty()
            || !tag
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ':' | '.'))
        {
            return None;
        }
        steps.push(XmlStep { tag, pred });
    }
    if steps.is_empty() {
        return None;
    }
    Some(XmlPathQ {
        absolute,
        relative,
        steps,
    })
}

fn xml_pred_value(v: &str) -> Option<String> {
    // `#{version...}` interpolations stay verbatim;
    // plan_urls substitutes them against the checked
    // unit's own version (outside tokens delegate).
    let v = v.trim();
    if let Some(inner) = v.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')) {
        Some(inner.to_string())
    } else {
        v.strip_prefix('"')
            .and_then(|r| r.strip_suffix('"'))
            .map(|inner| inner.to_string())
    }
}

/// A `<root>.elements["PATH"]` / `.get_elements("PATH")`
/// query at the start of `s`: (is_all, path, tail).
fn xml_query<'a>(s: &'a str, root: &str) -> Option<(bool, super::formula::XmlPathQ, &'a str)> {
    let rest = s.trim().strip_prefix(root)?;
    let (all, rest) = if let Some(r) = rest.strip_prefix(".get_elements(") {
        (true, r)
    } else {
        let r = rest.strip_prefix(".elements[")?;
        (false, r)
    };
    let (mut path_str, mut tail) = quoted_token(rest.trim_start())?;
    // Adjacent string literals concatenate (the
    // line-continued `"…" \` `"/…"` form).
    loop {
        let t = tail
            .trim_start()
            .strip_prefix('\\')
            .unwrap_or(tail)
            .trim_start();
        if t.starts_with(['"', '\''])
            && let Some((more, t2)) = quoted_token(t)
        {
            path_str.push_str(&more);
            tail = t2;
        } else {
            tail = t;
            break;
        }
    }
    let tail = tail.trim_start();
    let tail = tail.strip_prefix(',').unwrap_or(tail).trim_start();
    let tail = if all {
        tail.strip_prefix(')')?
    } else {
        tail.strip_prefix(']')?
    };
    Some((all, xml_path_parse(&path_str)?, tail))
}

/// One `x = xml.get_elements("P").map { |item| … }`
/// list local for the zip form.
fn xml_zip_local(stmt: &str, root: &str) -> Option<(String, super::formula::XmlMap)> {
    use super::formula::{XmlMap, XmlProj};
    let (name, rhs) = stmt.split_once(" = ")?;
    let name = name.trim();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let (all, path, tail) = xml_query(rhs.trim(), root)?;
    if !all {
        return None;
    }
    let tail = tail.trim();
    let tail = tail
        .strip_prefix("&.map")
        .or_else(|| tail.strip_prefix(".map"))?
        .trim();
    let inner = tail.strip_prefix('{')?.strip_suffix('}')?.trim();
    let inner = inner.strip_prefix('|')?;
    let var_end = inner.find('|')?;
    let var = inner[..var_end].trim();
    let body = inner[var_end + 1..].trim();
    let after = body.strip_prefix(var)?.trim();
    let proj = if matches!(after, ".text" | ".text&.strip" | ".text.strip") {
        XmlProj::Text
    } else if let Some(t) = after.strip_prefix(".next_element") {
        if !matches!(
            t.trim(),
            "&.text" | "&.text&.strip" | ".text" | ".text&.strip"
        ) {
            return None;
        }
        XmlProj::NextText
    } else {
        let r = after
            .strip_prefix(".attributes[")
            .or_else(|| after.strip_prefix("&.attributes["))?;
        let (attr, t) = quoted_token(r.trim_start())?;
        if t.trim() != "]" {
            return None;
        }
        XmlProj::Attr(attr)
    };
    Some((
        name.to_string(),
        XmlMap {
            path,
            proj,
            grep: false,
            locals: Vec::new(),
        },
    ))
}

/// Two mapped lists reduced to extremes:
/// `versions = …filter_map…match[1]`,
/// `builds = …map { text }`, then
/// `versions.max_by { Version.new }` and
/// `builds.max` in one CSV template.
fn parse_xml_two_list_extreme(
    root: &str,
    has_regex: bool,
    stmts: &[String],
) -> Option<super::formula::XmlShape> {
    use super::formula::{XmlMap, XmlProj, XmlShape};
    if !has_regex || stmts.len() != 10 {
        return None;
    }
    let ident = |v: &str| {
        !v.is_empty()
            && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !v.chars().next().is_some_and(|c| c.is_ascii_digit())
    };
    let (versions_name, rhs) = stmts[0].split_once(" = ")?;
    let versions_name = versions_name.trim();
    if !ident(versions_name) {
        return None;
    }
    let (all, versions_path, tail) = xml_query(rhs.trim(), root)?;
    if !all {
        return None;
    }
    let rest = [
        "&.filter_map do |",
        ".filter_map do |",
        "&.map do |",
        ".map do |",
    ]
    .iter()
    .find_map(|p| tail.trim().strip_prefix(p))?;
    let item_var = rest.strip_suffix('|')?.trim();
    if !ident(item_var) {
        return None;
    }
    let (mv, rhs) = stmts[1].split_once(" = ")?;
    let mv = mv.trim();
    if !ident(mv) {
        return None;
    }
    let rest = rhs.trim().strip_prefix(item_var)?.trim();
    let rest = rest
        .strip_prefix("&.text")
        .or_else(|| rest.strip_prefix(".text"))?
        .trim();
    let rest = rest
        .strip_prefix("&.strip")
        .or_else(|| rest.strip_prefix(".strip"))
        .unwrap_or(rest)
        .trim();
    if rest != "&.match(regex)" && rest != ".match(regex)" {
        return None;
    }
    if stmts[2].trim() != format!("next if {mv}.blank?")
        && stmts[2].trim() != format!("next unless {mv}")
    {
        return None;
    }
    if stmts[3].trim() != format!("{mv}[1]") || stmts[4].trim() != "end" {
        return None;
    }
    let (builds_name, builds) = xml_zip_local(&stmts[5], root)?;
    if builds.proj != XmlProj::Text {
        return None;
    }
    let (version_name, rhs) = stmts[6].split_once(" = ")?;
    let version_name = version_name.trim();
    let rest = rhs
        .trim()
        .strip_prefix(&format!("{versions_name}.max_by {{ |"))?;
    let (vvar, rest) = rest.split_once('|')?;
    let vvar = vvar.trim();
    if !ident(version_name) || !ident(vvar) || rest.trim() != format!("Version.new({vvar}) }}") {
        return None;
    }
    let (build_name, rhs) = stmts[7].split_once(" = ")?;
    let build_name = build_name.trim();
    if !ident(build_name) || rhs.trim() != format!("{builds_name}.max") {
        return None;
    }
    if stmts[8].trim() != format!("next if {version_name}.blank? || {build_name}.blank?") {
        return None;
    }
    let (body, tail) = quoted_token(stmts[9].trim())?;
    if !tail.trim().is_empty() || body != format!("#{{{version_name}}},#{{{build_name}}}") {
        return None;
    }
    Some(XmlShape::TwoListExtreme {
        versions: XmlMap {
            path: versions_path,
            proj: XmlProj::TextMatch(false),
            grep: false,
            locals: Vec::new(),
        },
        builds,
    })
}

/// `a = …map…; b = …map…; a.zip(b).map { |x, y| "…" }`
fn parse_xml_zip_shape(
    root: &str,
    has_regex: bool,
    stmts: &[String],
) -> Option<super::formula::XmlShape> {
    use super::formula::XmlShape;
    if has_regex || stmts.len() < 3 {
        return None;
    }
    let (a_name, a) = xml_zip_local(&stmts[0], root)?;
    let (b_name, b) = xml_zip_local(&stmts[1], root)?;
    let joined = stmts[2..].join(" ");
    let rest = joined
        .trim()
        .strip_prefix(&format!("{a_name}.zip({b_name})"))?;
    let rest = rest.trim().strip_prefix(".map")?.trim();
    let inner = if let Some(r) = rest.strip_prefix('{') {
        r.strip_suffix('}')?
    } else {
        rest.strip_prefix("do ")?.strip_suffix(" end")?
    };
    let inner = inner.trim().strip_prefix('|')?;
    let var_end = inner.find('|')?;
    let vars: Vec<String> = inner[..var_end]
        .split(',')
        .map(|v| v.trim().to_string())
        .collect();
    if vars.len() != 2 || vars.iter().any(|v| v.is_empty()) {
        return None;
    }
    let (body_s, t) = quoted_token(inner[var_end + 1..].trim())?;
    if !t.trim().is_empty() {
        return None;
    }
    let parts = xml_template_parts(&body_s, "", &vars)?;
    Some(XmlShape::ZipPair { a, b, parts })
}

/// A map-body template string over item reads
/// (`#{item.attributes["k"]}`, `#{item.text}`):
/// parts plus deduplicated per-item locals.
fn xml_attr_template_parts(
    body: &str,
    var: &str,
) -> Option<(
    Vec<super::formula::XmlPart>,
    Vec<super::formula::XmlItemLocal>,
)> {
    use super::formula::{XmlItemLocal, XmlPart};
    let (s, t) = quoted_token(body.trim())?;
    if !t.trim().is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    let mut locals: Vec<XmlItemLocal> = Vec::new();
    let mut rest = s.as_str();
    while let Some(i) = rest.find("#{") {
        if i > 0 {
            parts.push(XmlPart::Lit(rest[..i].to_string()));
        }
        // The token's closing brace, quote-aware.
        let inner = &rest[i + 2..];
        let mut j = 0usize;
        let mut quote: Option<u8> = None;
        let bytes = inner.as_bytes();
        while j < bytes.len() {
            match (quote, bytes[j]) {
                (Some(q), c) if c == q => quote = None,
                (Some(_), b'\\') => j += 1,
                (Some(_), _) => {}
                (None, b'"' | b'\'') => quote = Some(bytes[j]),
                (None, b'}') => break,
                _ => {}
            }
            j += 1;
        }
        if j >= bytes.len() {
            return None;
        }
        let token = inner[..j].trim();
        let after = token.strip_prefix(var)?;
        let local = if let Some(r) = after
            .strip_prefix(".attributes[")
            .or_else(|| after.strip_prefix("&.attributes["))
        {
            let (attr, t) = quoted_token(r.trim_start())?;
            if t.trim() != "]" {
                return None;
            }
            XmlItemLocal::Attr(attr)
        } else if matches!(after.trim(), ".text" | "&.text") {
            XmlItemLocal::Text
        } else if let Some(r) = after
            .strip_prefix(".text&.")
            .or_else(|| after.strip_prefix(".text."))
            && r.trim() == "[](regex, 1)"
        {
            XmlItemLocal::TextCapture
        } else {
            return None;
        };
        let idx = match locals.iter().position(|l| *l == local) {
            Some(p) => p,
            None => {
                locals.push(local);
                locals.len() - 1
            }
        };
        parts.push(XmlPart::Local(idx));
        rest = &inner[j + 1..];
    }
    if !rest.is_empty() {
        parts.push(XmlPart::Lit(rest.to_string()));
    }
    if locals.is_empty() {
        return None;
    }
    Some((parts, locals))
}

/// The guarded child-attribute match prefix of a
/// map body: an optional beta-style child-text
/// skip, a child attribute local, and its match.
/// Returns (match var, projection, rest).
fn xml_child_attr_match_prefix(
    body: &str,
    var: &str,
) -> Option<(String, super::formula::XmlProj, String)> {
    use super::formula::XmlProj;
    let ident = |v: &str| {
        !v.is_empty()
            && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !v.chars().next().is_some_and(|c| c.is_ascii_digit())
    };
    fn child_tail<'a>(s: &'a str, var: &str) -> Option<(String, &'a str)> {
        let r = s
            .trim()
            .strip_prefix(var)?
            .strip_prefix(".elements[")
            .or_else(|| s.trim().strip_prefix(var)?.strip_prefix("&.elements["))?;
        let (child, t) = quoted_token(r.trim_start())?;
        Some((child, t.trim().strip_prefix(']')?))
    }
    let mut rest = body.trim();
    let mut skip_child = None;
    if let Some(cond) = rest.strip_prefix("next if ") {
        let (child, t) = child_tail(cond, var)?;
        let t = t
            .trim()
            .strip_prefix("&.text&.include?(")
            .or_else(|| t.trim().strip_prefix(".text.include?("))
            .or_else(|| t.trim().strip_prefix("&.text.include?("))
            .or_else(|| t.trim().strip_prefix(".text&.include?("))?;
        let (lit, t) = quoted_token(t.trim_start())?;
        rest = t.trim().strip_prefix(')')?.trim();
        skip_child = Some((child, lit));
    }
    let (local, rhs) = rest.split_once(" = ")?;
    let local = local.trim();
    if !ident(local) {
        return None;
    }
    let (child, t) = child_tail(rhs.trim(), var)?;
    let t = t
        .trim()
        .strip_prefix("&.attributes&.[](")
        .or_else(|| t.trim().strip_prefix(".attributes&.[]("))
        .or_else(|| t.trim().strip_prefix("&.attributes.[]("))
        .or_else(|| t.trim().strip_prefix(".attributes.[]("))?;
    let (attr, t) = quoted_token(t.trim_start())?;
    let rest = t.trim().strip_prefix(')')?.trim();
    let (mv, rhs) = rest.split_once(" = ")?;
    let mv = mv.trim();
    if !ident(mv) {
        return None;
    }
    let rhs = rhs.trim();
    let rest = rhs
        .strip_prefix(&format!("{local}&.match(regex) if {local}"))
        .or_else(|| rhs.strip_prefix(&format!("{local}.match(regex) if {local}")))
        .or_else(|| rhs.strip_prefix(&format!("{local}&.match(regex)")))
        .or_else(|| rhs.strip_prefix(&format!("{local}.match(regex)")))?;
    Some((
        mv.to_string(),
        XmlProj::ChildAttrMatch {
            child,
            attr,
            skip_child,
        },
        rest.trim().to_string(),
    ))
}

/// A statement-form `:xml` block reduced to data:
/// first-element locals with blank guards and a
/// template, or a `get_elements` map projection.
/// `item = xml.elements["P"]&.text&.strip`, a blank
/// guard, `j = …::Json.parse_json(item)`, `j["k"]`:
/// the element text is an embedded JSON document.
fn parse_xml_json_embed_shape(root: &str, stmts: &[String]) -> Option<super::formula::XmlShape> {
    use super::formula::XmlShape;
    if stmts.len() != 4 {
        return None;
    }
    let (name, rhs) = stmts[0].split_once(" = ")?;
    let name = name.trim();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let (all, path, tail) = xml_query(rhs.trim(), root)?;
    if all
        || !matches!(
            tail.trim(),
            "&.text&.strip" | "&.text" | ".text&.strip" | ".text"
        )
    {
        return None;
    }
    if stmts[1].trim() != format!("next if {name}.blank?")
        && stmts[1].trim() != format!("next unless {name}")
    {
        return None;
    }
    let (jname, rhs) = stmts[2].split_once(" = ")?;
    let jname = jname.trim();
    if rhs.trim() != format!("Homebrew::Livecheck::Strategy::Json.parse_json({name})") {
        return None;
    }
    let rest = stmts[3].trim().strip_prefix(jname)?.strip_prefix('[')?;
    let (key, t) = quoted_token(rest.trim_start())?;
    if t.trim() != "]" {
        return None;
    }
    Some(XmlShape::TextJsonKey { path, key })
}

/// Own-version locals (`version.csv.first.major`,
/// `version.csv.second`), then a map matching each
/// item's child text and forking on the capture's
/// leading digits against the own major (endnote).
fn parse_xml_own_fork_shape(
    root: &str,
    has_regex: bool,
    stmts: &[String],
) -> Option<super::formula::XmlShape> {
    use super::formula::XmlShape;
    if !has_regex || stmts.len() != 13 {
        return None;
    }
    let own_local = |stmt: &str, want: &str| -> Option<String> {
        let (name, rhs) = stmt.split_once(" = ")?;
        let name = name.trim();
        if rhs.trim() != want
            || name.is_empty()
            || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return None;
        }
        Some(name.to_string())
    };
    let a_name = own_local(&stmts[0], "version.csv.first.major")?;
    let b_name = own_local(&stmts[1], "version.csv.second")?;
    let (all, path, tail) = xml_query(stmts[2].trim(), root)?;
    if !all {
        return None;
    }
    let tail = tail.trim().strip_prefix(".map do |")?;
    let var = tail.strip_suffix('|')?.trim();
    if var.is_empty() || !var.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let (u_name, rhs) = stmts[3].split_once(" = ")?;
    let u_name = u_name.trim();
    let rest = rhs.trim().strip_prefix(var)?.strip_prefix(".elements[")?;
    let (child, t) = quoted_token(rest.trim_start())?;
    if !matches!(
        t.trim(),
        "]&.text&.strip" | "]&.text" | "].text&.strip" | "].text"
    ) {
        return None;
    }
    let (m_name, rhs) = stmts[4].split_once(" = ")?;
    let m_name = m_name.trim();
    if rhs.trim() != format!("{u_name}.match(regex) if {u_name}")
        && rhs.trim() != format!("{u_name}.match(regex)")
    {
        return None;
    }
    if stmts[5].trim() != format!("next if {m_name}.blank?") {
        return None;
    }
    let (maj_name, rhs) = stmts[6].split_once(" = ")?;
    let maj_name = maj_name.trim();
    if rhs.trim() != format!("{m_name}[1][/^(\\d+)/, 1]") {
        return None;
    }
    if stmts[7].trim() != format!("if {maj_name} == {a_name} && {b_name}") {
        return None;
    }
    let mut want_then = String::from("\"#{");
    want_then.push_str(m_name);
    want_then.push_str("[1]},#{");
    want_then.push_str(&b_name);
    want_then.push_str("}\"");
    if stmts[8].trim() != want_then
        || stmts[9].trim() != "else"
        || stmts[10].trim() != format!("{m_name}[1]")
        || stmts[11].trim() != "end"
        || stmts[12].trim() != "end"
    {
        return None;
    }
    Some(XmlShape::ChildMatchOwnFork { path, child })
}

/// `url = xml.elements["P"]&.text&.strip`, a blank
/// guard, `page_headers(url)` merged, a header match
/// against the spec regex, and a capture template
/// (canon-eos-utility).
fn parse_xml_header_hop_shape(
    root: &str,
    has_regex: bool,
    stmts: &[String],
) -> Option<super::formula::XmlShape> {
    use super::formula::XmlShape;
    if !has_regex || stmts.len() != 6 {
        return None;
    }
    let (u_name, rhs) = stmts[0].split_once(" = ")?;
    let u_name = u_name.trim();
    let (all, path, tail) = xml_query(rhs.trim(), root)?;
    if all
        || !matches!(
            tail.trim(),
            "&.text&.strip" | "&.text" | ".text&.strip" | ".text"
        )
    {
        return None;
    }
    if stmts[1].trim() != format!("next if {u_name}.blank?") {
        return None;
    }
    let (h_name, rhs) = stmts[2].split_once(" = ")?;
    let h_name = h_name.trim();
    if rhs.trim() != format!("Homebrew::Livecheck::Strategy.page_headers({u_name}).reduce(&:merge)")
    {
        return None;
    }
    let (mv, rhs) = stmts[3].split_once(" = ")?;
    let mv = mv.trim();
    let rest = rhs.trim().strip_prefix(h_name)?.strip_prefix('[')?;
    let (header, t) = quoted_token(rest.trim_start())?;
    if !matches!(t.trim(), "]&.match(regex)" | "].match(regex)") {
        return None;
    }
    if stmts[4].trim() != format!("next if {mv}.blank?") {
        return None;
    }
    let (body_str, t) = quoted_token(stmts[5].trim())?;
    if !t.trim().is_empty() {
        return None;
    }
    let parts = xml_template_parts(&body_str, mv, &[])?;
    Some(XmlShape::HeaderHop {
        path,
        header: header.to_lowercase(),
        parts,
    })
}

/// A `max_by` element pick, version + second-XML URL
/// child locals, a `page_content` fetch re-parsed as
/// XML, and a spec-regex match rendered with the
/// version local (turbotax-2024).
fn parse_xml_hop_match_shape(
    root: &str,
    has_regex: bool,
    stmts: &[String],
) -> Option<super::formula::XmlShape> {
    use super::formula::XmlShape;
    // The pick assignment may wrap after `=` (the
    // query chain then opens the next statement).
    let wrapped;
    let stmts: &[String] = if stmts.len() == 13 && stmts[0].trim().ends_with('=') {
        let mut v = vec![format!("{} {}", stmts[0].trim(), stmts[1].trim())];
        v.extend(stmts[2..].iter().cloned());
        wrapped = v;
        &wrapped
    } else {
        stmts
    };
    if !has_regex || stmts.len() != 12 {
        return None;
    }
    let ident = |s: &str| -> Option<String> {
        let s = s.trim();
        if s.is_empty() || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
        Some(s.to_string())
    };
    let (e_name, rhs) = stmts[0].split_once(" = ")?;
    let e_name = ident(e_name)?;
    let (all, path, tail) = xml_query(rhs.trim(), root)?;
    if !all {
        return None;
    }
    let tail = tail.trim().strip_prefix(".max_by")?.trim();
    let tail = tail.strip_prefix('{')?.strip_suffix('}')?.trim();
    let tail = tail.strip_prefix('|')?;
    let (ivar, body) = tail.split_once('|')?;
    let ivar = ident(ivar)?;
    let rest = body
        .trim()
        .strip_prefix(&ivar)?
        .strip_prefix(".elements[")?;
    let (max_child, t) = quoted_token(rest.trim_start())?;
    if !matches!(t.trim(), "]&.text" | "].text") {
        return None;
    }
    if stmts[1].trim() != format!("next unless {e_name}") {
        return None;
    }
    let child_text = |stmt: &str| -> Option<(String, String, String)> {
        let (name, rhs) = stmt.split_once(" = ")?;
        let name = ident(name)?;
        let rest = rhs
            .trim()
            .strip_prefix(&e_name)?
            .strip_prefix(".elements[")?;
        let (child, t) = quoted_token(rest.trim_start())?;
        Some((name, child, t.trim().to_string()))
    };
    let (v_name, version_child, tail) = child_text(&stmts[2])?;
    let rest = tail.strip_prefix("]&.text&.[](")?;
    let (pat, flags, t) = regex_literal(rest.trim())?;
    if t.trim() != ", 1)" {
        return None;
    }
    let (u_name, url_child, tail) = child_text(&stmts[3])?;
    if !matches!(
        tail.as_str(),
        "]&.text&.strip" | "]&.text" | "].text&.strip"
    ) {
        return None;
    }
    if stmts[4].trim() != format!("next if !{v_name} || !{u_name}") {
        return None;
    }
    let (p_name, rhs) = stmts[5].split_once(" = ")?;
    let p_name = ident(p_name)?;
    if rhs.trim() != format!("Homebrew::Livecheck::Strategy.page_content({u_name})") {
        return None;
    }
    let inner = stmts[6]
        .trim()
        .strip_prefix("next if (")?
        .strip_suffix(").blank?")?;
    let (c_name, rhs) = inner.split_once(" = ")?;
    let c_name = ident(c_name)?;
    if rhs.trim() != format!("{p_name}[:content]") {
        return None;
    }
    let (x_name, rhs) = stmts[7].split_once(" = ")?;
    let x_name = ident(x_name)?;
    if rhs.trim() != format!("Homebrew::Livecheck::Strategy::Xml.parse_xml({c_name})") {
        return None;
    }
    let (l_name, rhs) = stmts[8].split_once(" = ")?;
    let l_name = ident(l_name)?;
    let (all, path2, tail) = xml_query(rhs.trim(), &x_name)?;
    if all
        || !matches!(
            tail.trim(),
            "&.text&.strip" | "&.text" | ".text&.strip" | ".text"
        )
    {
        return None;
    }
    let (mv, rhs) = stmts[9].split_once(" = ")?;
    let mv = ident(mv)?;
    if rhs.trim() != format!("{l_name}&.match(regex)")
        && rhs.trim() != format!("{l_name}.match(regex)")
    {
        return None;
    }
    if stmts[10].trim() != format!("next if {mv}.blank?") {
        return None;
    }
    let (body_str, t) = quoted_token(stmts[11].trim())?;
    if !t.trim().is_empty() {
        return None;
    }
    let parts = xml_template_parts(&body_str, &mv, &[v_name])?;
    Some(XmlShape::HopMatch {
        path,
        max_child,
        version_child,
        pat,
        flags,
        url_child,
        path2,
        parts,
    })
}

fn parse_xml_template_shape(params: &str, lines: &[String]) -> Option<super::formula::XmlBlock> {
    use super::formula::{XmlBlock, XmlLocal, XmlMap, XmlProj, XmlRender, XmlShape, XmlTemplate};
    let (root, has_regex) = block_params(params)?;
    let raw_stmts: Vec<String> = lines
        .iter()
        .map(|l| strip_trailing_comment(l.trim()).trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    // Line-continued chains (`.map { … }` on its own
    // line) join onto the previous statement, as do
    // trailing-`\` and open-bracket continuations
    // (multi-line `xml.elements[ … ]` paths).
    fn bracket_balance(s: &str) -> i32 {
        let mut bal = 0i32;
        let mut quote: Option<char> = None;
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            match quote {
                Some(q) if c == q => quote = None,
                Some(_) if c == '\\' => {
                    chars.next();
                }
                Some(_) => {}
                None if c == '\'' || c == '"' => quote = Some(c),
                None if c == '[' => bal += 1,
                None if c == ']' => bal -= 1,
                _ => {}
            }
        }
        bal
    }
    let mut stmts: Vec<String> = Vec::new();
    for stmt in raw_stmts {
        let joins = stmt.starts_with('.')
            || stmts.last().is_some_and(|prev: &String| {
                prev.trim_end().ends_with('\\') || bracket_balance(prev) > 0
            });
        if joins && let Some(prev) = stmts.last_mut() {
            prev.push(' ');
            prev.push_str(&stmt);
        } else {
            stmts.push(stmt);
        }
    }
    if let Some(shape) = parse_xml_zip_shape(&root, has_regex, &stmts) {
        return Some(XmlBlock { has_regex, shape });
    }
    if let Some(shape) = parse_xml_two_list_extreme(&root, has_regex, &stmts) {
        return Some(XmlBlock { has_regex, shape });
    }
    if let Some(shape) = parse_xml_json_embed_shape(&root, &stmts) {
        return Some(XmlBlock { has_regex, shape });
    }
    if let Some(shape) = parse_xml_own_fork_shape(&root, has_regex, &stmts) {
        return Some(XmlBlock { has_regex, shape });
    }
    if let Some(shape) = parse_xml_header_hop_shape(&root, has_regex, &stmts) {
        return Some(XmlBlock { has_regex, shape });
    }
    if let Some(shape) = parse_xml_hop_match_shape(&root, has_regex, &stmts) {
        return Some(XmlBlock { has_regex, shape });
    }
    let first = stmts.first()?;
    // Map form.
    if let Some((true, path, tail)) = xml_query(first, &root) {
        let tail = tail.trim();
        // `&.map { |item| … }`, `.map do |item| … end`,
        // `filter_map` likewise..
        let (is_filter, inner_all) = if let Some(r) = tail
            .strip_prefix("&.filter_map")
            .or_else(|| tail.strip_prefix(".filter_map"))
        {
            (true, r)
        } else {
            let r = tail
                .strip_prefix("&.map")
                .or_else(|| tail.strip_prefix(".map"))?;
            (false, r)
        };
        let _ = is_filter;
        let inner_all = inner_all.trim_start();
        // Joined body for do-forms: join remaining stmts.
        let mut joined = inner_all.to_string();
        for extra in &stmts[1..] {
            joined.push(' ');
            joined.push_str(extra);
        }
        let grep = joined.ends_with(".grep(regex)");
        let joined = if grep {
            joined
                .trim_end_matches(".grep(regex)")
                .trim_end()
                .to_string()
        } else {
            joined
        };
        // Symbol block-pass: `.map(&:text)`.
        if joined.trim() == "(&:text)" && !grep {
            return Some(XmlBlock {
                has_regex,
                shape: XmlShape::Template(Box::new(XmlTemplate {
                    locals: Vec::new(),
                    elems: Vec::new(),
                    require: Vec::new(),
                    require_elems: Vec::new(),
                    match_local: None,
                    or_pairs: Vec::new(),
                    map: Some(XmlMap {
                        path,
                        proj: XmlProj::Text,
                        grep: false,
                        locals: Vec::new(),
                    }),
                    render: XmlRender::Projection,
                })),
            });
        }
        let inner = if let Some(r) = joined.strip_prefix('{') {
            r.strip_suffix('}')?.trim().to_string()
        } else {
            let r = joined.strip_prefix("do ")?;
            r.strip_suffix(" end")?.trim().to_string()
        };
        let inner = inner.strip_prefix('|')?;
        let var_end = inner.find('|')?;
        let var = inner[..var_end].trim();
        let body = inner[var_end + 1..].trim();
        // Per-item locals + template (sonos form):
        // assignments, a blank guard, a template.
        if body.contains(" = ") && !body.contains(".match(regex)") && stmts.len() >= 3 {
            return xml_map_locals_shape(path, var, &stmts[1..stmts.len() - 1], has_regex);
        }
        // Match form inside the map.
        if body.contains(".match(regex)") {
            let proj_next = body.contains(".next_element");
            let (mv, rest, attr_src, child_proj) =
                if let Some((mv, proj, rest)) = xml_child_attr_match_prefix(body, var) {
                    (mv, rest, None, Some(proj))
                } else {
                    let (mv, rest) = body.split_once(" = ")?;
                    let mv = mv.trim().to_string();
                    let rest = rest.trim().strip_prefix(var)?;
                    let (attr_src, rest) = if let Some(r) = rest
                        .strip_prefix(".attributes[")
                        .or_else(|| rest.strip_prefix("&.attributes["))
                    {
                        let (attr, t) = quoted_token(r.trim_start())?;
                        (Some(attr), t.trim().strip_prefix(']')?)
                    } else {
                        (
                            None,
                            rest.strip_prefix(".text")
                                .or_else(|| rest.strip_prefix(".next_element&.text"))?,
                        )
                    };
                    let strip = rest.starts_with("&.strip");
                    let rest = if strip {
                        rest.strip_prefix("&.strip")?.trim()
                    } else {
                        rest.trim()
                    };
                    let rest = rest
                        .strip_prefix("&.match(regex)")
                        .or_else(|| rest.strip_prefix(".match(regex)"))?
                        .trim()
                        .to_string();
                    (mv, rest, attr_src, None)
                };
            let mv = mv.as_str();
            let rest = rest.as_str();
            let rest = rest
                .strip_prefix(&format!("next if {mv}.blank?"))
                .or_else(|| rest.strip_prefix(&format!("next unless {mv}")))?
                .trim();
            let xml_arm = |s: &str| -> Option<Vec<super::formula::XmlPart>> {
                let s = s.trim();
                if s.starts_with('"') {
                    let (body_s, t) = quoted_token(s)?;
                    if !t.trim().is_empty() {
                        return None;
                    }
                    xml_template_parts(&body_s, mv, &[])
                } else {
                    let r = s.strip_prefix(&format!("{mv}["))?;
                    let close = r.find(']')?;
                    if !r[close + 1..].trim().is_empty() {
                        return None;
                    }
                    Some(vec![super::formula::XmlPart::Cap(r[..close].parse().ok()?)])
                }
            };
            let render = if let Some(q) = json_outside_quotes(rest, " ? ")
                && !rest.starts_with('"')
            {
                let (cond_s, tail) = rest.split_at(q);
                let tail = &tail[3..];
                let c = json_outside_quotes(tail, " : ")?;
                let cap_s = cond_s.trim().strip_suffix(".present?")?;
                let cap = page_cap_index(mv, cap_s)?;
                XmlRender::CapPresentFork {
                    cap,
                    then: xml_arm(&tail[..c])?,
                    els: xml_arm(&tail[c + 3..])?,
                }
            } else if let Some(r) = rest.strip_prefix(&format!("{mv}[")) {
                let close = r.find(']')?;
                if !r[close + 1..].trim().is_empty() {
                    return None;
                }
                XmlRender::CapValue(r[..close].parse().ok()?)
            } else if rest.starts_with('"') {
                let (s, t) = quoted_token(rest)?;
                if !t.trim().is_empty() {
                    return None;
                }
                XmlRender::Template(xml_template_parts(&s, mv, &[])?)
            } else {
                return None;
            };
            return Some(XmlBlock {
                has_regex: true,
                shape: XmlShape::Template(Box::new(XmlTemplate {
                    locals: Vec::new(),
                    elems: Vec::new(),
                    require: Vec::new(),
                    require_elems: Vec::new(),
                    match_local: None,
                    or_pairs: Vec::new(),
                    map: Some(XmlMap {
                        path,
                        proj: match child_proj {
                            Some(proj) => proj,
                            None => match attr_src {
                                Some(attr) => XmlProj::AttrMatch(attr),
                                None => XmlProj::TextMatch(proj_next),
                            },
                        },
                        grep: false,
                        locals: Vec::new(),
                    }),
                    render,
                })),
            });
        }
        // A bare template over item attributes/text
        // (`"#{item.attributes["k"]},…"`).
        if body.starts_with('"')
            && !grep
            && let Some((parts, locals)) = xml_attr_template_parts(body, var)
        {
            return Some(XmlBlock {
                has_regex,
                shape: XmlShape::Template(Box::new(XmlTemplate {
                    locals: Vec::new(),
                    elems: Vec::new(),
                    require: Vec::new(),
                    require_elems: Vec::new(),
                    match_local: None,
                    or_pairs: Vec::new(),
                    map: Some(XmlMap {
                        path,
                        proj: XmlProj::Text,
                        grep: false,
                        locals,
                    }),
                    render: XmlRender::Template(parts),
                })),
            });
        }
        let after = body.strip_prefix(var)?.trim();
        let proj = if after == ".text" || after == ".text&.strip" || after == ".text.strip" {
            XmlProj::Text
        } else if let Some(r) = after
            .strip_prefix(".text&.strip&.delete_suffix(")
            .or_else(|| after.strip_prefix(".text.strip.delete_suffix("))
            .or_else(|| after.strip_prefix(".text&.delete_suffix("))
        {
            let (suffix, t) = quoted_token(r.trim())?;
            if t.trim() != ")" {
                return None;
            }
            XmlProj::TextDeleteSuffix(suffix)
        } else if after.starts_with(".next_element") {
            let t = after.strip_prefix(".next_element")?.trim();
            if !matches!(t, "&.text" | "&.text&.strip" | ".text" | ".text&.strip") {
                return None;
            }
            XmlProj::NextText
        } else {
            let r = after
                .strip_prefix(".attributes[")
                .or_else(|| after.strip_prefix("&.attributes["))?;
            let (attr, t) = quoted_token(r.trim_start())?;
            let t = t.trim();
            let t = t.strip_prefix(']')?.trim();
            if t.is_empty() {
                XmlProj::Attr(attr)
            } else if t == "&.[](regex, 1)" || t == "[regex, 1]" {
                XmlProj::AttrCapture(attr)
            } else {
                return None;
            }
        };
        if grep && !matches!(proj, XmlProj::Text | XmlProj::NextText | XmlProj::Attr(_)) {
            return None;
        }
        return Some(XmlBlock {
            has_regex,
            shape: XmlShape::Template(Box::new(XmlTemplate {
                locals: Vec::new(),
                elems: Vec::new(),
                require: Vec::new(),
                require_elems: Vec::new(),
                match_local: None,
                or_pairs: Vec::new(),
                map: Some(XmlMap {
                    path,
                    proj,
                    grep,
                    locals: Vec::new(),
                }),
                render: XmlRender::Projection,
            })),
        });
    }
    // Single form: locals, guards, match, render.
    let mut locals: Vec<XmlLocal> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let mut elems: Vec<XmlLocal> = Vec::new();
    let mut elem_names: Vec<String> = Vec::new();
    let mut require: Vec<usize> = Vec::new();
    let mut require_elems: Vec<usize> = Vec::new();
    let mut or_pairs: Vec<(usize, usize)> = Vec::new();
    let mut match_local: Option<usize> = None;
    let mut match_var = String::new();
    let mut match_guarded = false;
    // A text read `<base>.elements[…]…&.text`: root reads need an
    // absolute/descendant path, element-local reads a relative one.
    let text_local = |rhs: &str,
                      base: &str,
                      from: Option<usize>,
                      locals: &mut Vec<XmlLocal>,
                      names: &mut Vec<String>|
     -> Option<usize> {
        let (all, path, tail) = xml_query(rhs, base)?;
        if all || path.relative != from.is_some() {
            return None;
        }
        let tail = tail.trim();
        let next = if let Some(t) = tail.strip_prefix("&.next_element") {
            if !matches!(t.trim(), "&.text" | "&.text&.strip" | ".text") {
                return None;
            }
            true
        } else if matches!(tail, "&.text" | "&.text&.strip" | ".text" | ".text&.strip") {
            false
        } else {
            return None;
        };
        locals.push(XmlLocal { path, next, from });
        names.push(String::new());
        Some(locals.len() - 1)
    };
    let expr_local = |rhs: &str, locals: &mut Vec<XmlLocal>, names: &mut Vec<String>| {
        text_local(rhs, &root, None, locals, names)
    };
    for stmt in &stmts[..stmts.len() - 1] {
        let stmt = stmt.as_str();
        if let Some(cond) = stmt.strip_prefix("next if ") {
            if !cond.contains(".blank?") {
                return None;
            }
            for part in cond.split(" || ") {
                let name = part.trim().strip_suffix(".blank?")?;
                if !match_var.is_empty() && name == match_var {
                    match_guarded = true;
                } else if let Some(pos) = elem_names.iter().position(|n| n == name) {
                    require_elems.push(pos);
                } else {
                    require.push(names.iter().position(|n| n == name)?);
                }
            }
        } else if let Some(cond) = stmt.strip_prefix("next unless ") {
            if cond == match_var && !match_var.is_empty() {
                match_guarded = true;
            } else {
                let pos = elem_names.iter().position(|n| n == cond)?;
                require_elems.push(pos);
            }
        } else if let Some((name, rhs)) = stmt.split_once(" ||= ") {
            // `<name> ||= <read>`: a nil fallback.
            let pos = names.iter().position(|n| n == name.trim())?;
            let idx = expr_local(rhs.trim(), &mut locals, &mut names)?;
            names[idx] = name.trim().to_string();
            or_pairs.push((pos, idx));
        } else if let Some((name, rhs)) = stmt.split_once(" = ") {
            let name = name.trim();
            if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return None;
            }
            let rhs = rhs.trim();
            // A trailing modifier `if <local>` only skips the
            // match when the local is nil, which the blank
            // guard then excludes either way.
            let rhs = match rhs.rsplit_once(" if ") {
                Some((b, cond))
                    if names.iter().any(|n| n == cond.trim())
                        && (b.ends_with(".match(regex)") || b.ends_with("&.match(regex)")) =>
                {
                    b.trim()
                }
                _ => rhs,
            };
            // `<match> = <local>[.strip].match(regex) [if <local>]`
            if let Some(base) = rhs
                .strip_suffix("&.match(regex)")
                .or_else(|| rhs.strip_suffix(".match(regex)"))
            {
                let base = base.trim();
                let base = base
                    .strip_suffix("&.strip")
                    .or_else(|| base.strip_suffix(".strip"))
                    .unwrap_or(base)
                    .trim();
                match_local = Some(names.iter().position(|n| n == base)?);
                match_var = name.to_string();
            } else if let Some(r) = rhs
                .strip_prefix("REXML::XPath.first(")
                .and_then(|r| r.strip_prefix(root.as_str()))
                .and_then(|r| r.trim().strip_prefix(','))
            {
                // `REXML::XPath.first(xml, "path")` is the
                // elements[] first read.
                let (path_str, t) = quoted_token(r.trim())?;
                if t.trim() != ")" {
                    return None;
                }
                let path = xml_path_parse(&path_str)?;
                if path.relative {
                    return None;
                }
                elems.push(XmlLocal {
                    path,
                    next: false,
                    from: None,
                });
                elem_names.push(name.to_string());
            } else if let Some((all, path, tail)) = xml_query(rhs, &root)
                && !all
                && !path.relative
                && matches!(tail.trim(), "" | "&.next_element")
            {
                elems.push(XmlLocal {
                    path,
                    next: tail.trim() == "&.next_element",
                    from: None,
                });
                elem_names.push(name.to_string());
            } else if let Some(pos) = elem_names
                .iter()
                .position(|n| rhs.starts_with(&format!("{n}.elements[")))
            {
                let base = elem_names[pos].clone();
                let idx = text_local(rhs, &base, Some(pos), &mut locals, &mut names)?;
                names[idx] = name.to_string();
            } else {
                let idx = expr_local(rhs, &mut locals, &mut names)?;
                names[idx] = name.to_string();
            }
        } else {
            return None;
        }
    }
    let final_stmt = stmts[stmts.len() - 1].trim();
    let render = if final_stmt.starts_with('"') {
        let (body_str, tail) = quoted_token(final_stmt)?;
        if !tail.trim().is_empty() {
            return None;
        }
        XmlRender::Template(xml_template_parts(&body_str, &match_var, &names)?)
    } else if !match_var.is_empty()
        && let Some(r) = final_stmt.strip_prefix(&format!("{match_var}["))
    {
        let close = r.find(']')?;
        if !r[close + 1..].trim().is_empty() {
            return None;
        }
        XmlRender::CapValue(r[..close].parse().ok()?)
    } else if let Some(pos) = names.iter().position(|n| {
        final_stmt == *n
            || final_stmt == format!("{n}.strip")
            || final_stmt == format!("{n}&.strip")
    }) {
        XmlRender::LocalValue(pos, final_stmt.ends_with(".strip"))
    } else if let Some(pos) = elem_names.iter().position(|n| {
        final_stmt.starts_with(&format!("{n}.next_element"))
            || final_stmt.starts_with(&format!("{n}&.next_element"))
    }) {
        // `<elem>.next_element.text`
        let base = elem_names[pos].clone();
        let after = final_stmt[base.len()..].trim();
        let t = after
            .strip_prefix("&.next_element")
            .or_else(|| after.strip_prefix(".next_element"))?;
        let strip = matches!(t.trim(), "&.text&.strip" | ".text.strip");
        if !matches!(
            t.trim(),
            "&.text" | ".text" | "&.text&.strip" | ".text.strip"
        ) {
            return None;
        }
        locals.push(XmlLocal {
            path: super::formula::XmlPathQ {
                absolute: false,
                relative: true,
                steps: Vec::new(),
            },
            next: true,
            from: Some(pos),
        });
        names.push(String::new());
        XmlRender::LocalValue(locals.len() - 1, strip)
    } else {
        // A bare first-element read.
        let idx = expr_local(final_stmt, &mut locals, &mut names)?;
        XmlRender::LocalValue(idx, false)
    };
    let uses_cap = match &render {
        XmlRender::Template(parts) => parts.iter().any(|p| {
            matches!(
                p,
                super::formula::XmlPart::Cap(_) | super::formula::XmlPart::CapTr(..)
            )
        }),
        XmlRender::CapValue(_) => true,
        _ => false,
    };
    if uses_cap && (match_local.is_none() || !match_guarded) {
        return None;
    }
    Some(XmlBlock {
        has_regex,
        shape: XmlShape::Template(Box::new(XmlTemplate {
            locals,
            elems,
            require,
            require_elems,
            match_local,
            or_pairs,
            map: None,
            render,
        })),
    })
}

/// The per-item locals map body: attr/text locals,
/// a blank guard over them, and a template render.
fn xml_map_locals_shape(
    path: super::formula::XmlPathQ,
    var: &str,
    body_stmts: &[String],
    has_regex: bool,
) -> Option<super::formula::XmlBlock> {
    use super::formula::{
        XmlBlock, XmlItemLocal, XmlMap, XmlProj, XmlRender, XmlShape, XmlTemplate,
    };
    let mut locals: Vec<XmlItemLocal> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let mut require: Vec<usize> = Vec::new();
    for stmt in &body_stmts[..body_stmts.len() - 1] {
        let stmt = stmt.as_str();
        if let Some(cond) = stmt.strip_prefix("next if ") {
            if !cond.contains(".blank?") {
                return None;
            }
            for part in cond.split(" || ") {
                let name = part.trim().strip_suffix(".blank?")?;
                require.push(names.iter().position(|n| n == name)?);
            }
        } else if let Some((name, rhs)) = stmt.split_once(" = ") {
            let rhs = rhs.trim();
            let after = rhs.strip_prefix(var)?;
            let local = if let Some(r) = after
                .strip_prefix(".attributes[")
                .or_else(|| after.strip_prefix("&.attributes["))
            {
                let (attr, t) = quoted_token(r.trim_start())?;
                if t.trim() != "]" {
                    return None;
                }
                XmlItemLocal::Attr(attr)
            } else if after.trim() == ".text" {
                XmlItemLocal::Text
            } else if after.trim() == ".text&.[](regex, 1)" {
                XmlItemLocal::TextCapture
            } else {
                return None;
            };
            names.push(name.trim().to_string());
            locals.push(local);
        } else {
            return None;
        }
    }
    let final_stmt = body_stmts[body_stmts.len() - 1].trim();
    let (body_str, tail) = quoted_token(final_stmt)?;
    if !tail.trim().is_empty() {
        return None;
    }
    let parts = xml_template_parts(&body_str, "", &names)?;
    // Locals in the render are item locals; the
    // executor reads them from the map.
    Some(XmlBlock {
        has_regex,
        shape: XmlShape::Template(Box::new(XmlTemplate {
            locals: Vec::new(),
            elems: Vec::new(),
            require,
            require_elems: Vec::new(),
            match_local: None,
            or_pairs: Vec::new(),
            map: Some(XmlMap {
                path,
                proj: XmlProj::Text,
                grep: false,
                locals,
            }),
            render: XmlRender::Template(parts),
        })),
    })
}

/// Parse `"#{local}#{match[N]}…"` parts for xml
/// templates.
fn xml_template_parts(
    body: &str,
    match_var: &str,
    names: &[String],
) -> Option<Vec<super::formula::XmlPart>> {
    use super::formula::XmlPart;
    let mut parts = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.find("#{") {
        if i > 0 {
            parts.push(XmlPart::Lit(rest[..i].to_string()));
        }
        let j = rest[i + 2..].find('}')?;
        let token = rest[i + 2..i + 2 + j].trim();
        let part = if !match_var.is_empty()
            && let Some(r) = token.strip_prefix(&format!("{match_var}["))
        {
            let close = r.find(']')?;
            let n: usize = r[..close].parse().ok()?;
            let tail = r[close + 1..].trim();
            if tail.is_empty() {
                XmlPart::Cap(n)
            } else if let Some((a, b)) = sp_tr_args(tail) {
                XmlPart::CapTr(n, a, b)
            } else {
                return None;
            }
        } else if let Some(base) = token.strip_suffix(".strip") {
            XmlPart::LocalStrip(names.iter().position(|n| n == base)?)
        } else {
            XmlPart::Local(names.iter().position(|n| n == token)?)
        };
        parts.push(part);
        rest = &rest[i + 2 + j + 1..];
    }
    if !rest.is_empty() {
        parts.push(XmlPart::Lit(rest.to_string()));
    }
    Some(parts)
}

/// Split an xml map body into its block variable and
/// inner expression text (brace or `do` form).
fn xml_map_parts(s: &str) -> Option<(String, String)> {
    let s = s.trim();
    let inner = if let Some(r) = s.strip_prefix('{') {
        r.strip_suffix('}')?
    } else {
        s.strip_prefix("do ")?.strip_suffix(" end")?
    };
    let inner = inner.trim().strip_prefix('|')?;
    let var_end = inner.find('|')?;
    let var = inner[..var_end].trim().to_string();
    if var.is_empty() || !var.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    Some((var, inner[var_end + 1..].trim().to_string()))
}

/// Reduce a `:json` strategy block body to a data shape,
/// or `None` for anything beyond the enumerable forms.
/// `params` is the block tail (`do`, `do |json|`,
/// `do |json, regex|`).
/// A trailing `.map { |_, item| item["k"] }` over
/// an object's values: the value key.
fn json_values_key(cur: &str) -> Option<String> {
    let rest = cur
        .strip_prefix("&.map")
        .or_else(|| cur.strip_prefix(".map"))?
        .trim();
    let inner = rest.strip_prefix('{')?.strip_suffix('}')?.trim();
    let inner = inner.strip_prefix('|')?;
    let var_end = inner.find('|')?;
    let vars: Vec<&str> = inner[..var_end].split(',').map(str::trim).collect();
    if vars.len() != 2 || vars[0] != "_" {
        return None;
    }
    let body = inner[var_end + 1..].trim();
    let (field, tail) = jt_field_tail(body, vars[1])?;
    if tail.trim().is_empty() {
        Some(field)
    } else {
        None
    }
}

/// One `<dig>&.match(<arg>)` attempt, where the
/// argument is the block `regex`, a regex local,
/// or a literal regex.
fn jt_match_attempt(
    rhs: &str,
    root: &str,
    cask_locals: &[(String, String)],
    regex_locals: &[(String, String, String)],
) -> Option<super::formula::JsonMatchAttempt> {
    use super::formula::{JsonMatchAttempt, JsonMatchRegex};
    let (path, tail) = jt_dig(rhs.trim(), root, cask_locals)?;
    let arg = tail
        .trim()
        .strip_prefix("&.match(")
        .or_else(|| tail.trim().strip_prefix(".match("))?
        .trim();
    let regex = if arg == "regex)" {
        JsonMatchRegex::Block
    } else if let Some((_, pat, flags)) = regex_locals
        .iter()
        .find(|(name, _, _)| arg == format!("{name})"))
    {
        JsonMatchRegex::Literal(pat.clone(), flags.clone())
    } else {
        let (pat, flags, rest) = regex_literal(arg)?;
        if rest.trim() != ")" || pat.contains("#{") {
            return None;
        }
        JsonMatchRegex::Literal(pat, flags)
    };
    Some(JsonMatchAttempt { path, regex })
}

/// Several named dig matches rendered in one final
/// template (wetype's fallback match,
/// font-oppo-sans's two matches):
/// `[<re> = /…/] <m> = <dig>&.match(…) [<m> ||=
/// <dig>&.match(…)] guards "#{<m>[n]}…"`.
fn parse_json_multi_match(
    root: &str,
    lines: &[String],
    cask_locals: &[(String, String)],
) -> Option<super::formula::JsonShape> {
    use super::formula::{JsonMultiMatch, JsonMultiPart, JsonShape};
    let stmts: Vec<String> = lines
        .iter()
        .map(|l| strip_trailing_comment(l.trim()).trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if stmts.len() < 3 {
        return None;
    }
    let ident = |v: &str| {
        !v.is_empty()
            && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !v.chars().next().is_some_and(|c| c.is_ascii_digit())
    };
    let mut regex_locals: Vec<(String, String, String)> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let mut matches: Vec<JsonMultiMatch> = Vec::new();
    let mut required: Vec<bool> = Vec::new();
    for stmt in &stmts[..stmts.len() - 1] {
        let stmt = stmt.trim();
        if let Some(cond) = stmt.strip_prefix("next if ") {
            let name = cond.trim().strip_suffix(".blank?")?;
            let pos = names.iter().position(|n| n == name)?;
            required[pos] = true;
            continue;
        }
        if let Some(cond) = stmt.strip_prefix("next unless ") {
            let pos = names.iter().position(|n| n == cond.trim())?;
            required[pos] = true;
            continue;
        }
        if let Some((name, rhs)) = stmt.split_once(" ||= ") {
            let pos = names.iter().position(|n| n == name.trim())?;
            // This shape evaluates the fallback attempts before its required-match guard.
            // A Ruby guard that already skipped a missing match cannot be moved after them.
            if required[pos] {
                return None;
            }
            let attempt = jt_match_attempt(rhs, root, cask_locals, &regex_locals)?;
            matches[pos].attempts.push(attempt);
            continue;
        }
        let (name, rhs) = jt_assignment(stmt)?;
        let name = name.trim();
        if !ident(name) || name == root || name == "regex" {
            return None;
        }
        let rhs = rhs.trim();
        if let Some((pat, flags, tail)) = regex_literal(rhs)
            && tail.trim().is_empty()
        {
            if pat.contains("#{")
                || regex_locals.iter().any(|(n, _, _)| n == name)
                || names.iter().any(|n| n == name)
            {
                return None;
            }
            regex_locals.push((name.to_string(), pat, flags));
            continue;
        }
        if names.iter().any(|n| n == name) || regex_locals.iter().any(|(n, _, _)| n == name) {
            return None;
        }
        let attempt = jt_match_attempt(rhs, root, cask_locals, &regex_locals)?;
        names.push(name.to_string());
        matches.push(JsonMultiMatch {
            attempts: vec![attempt],
        });
        required.push(false);
    }
    let multi = matches.len() > 1 || matches.iter().any(|m| m.attempts.len() > 1);
    if !multi || required.iter().any(|r| !*r) {
        return None;
    }
    let (body, tail) = quoted_token(stmts[stmts.len() - 1].trim())?;
    if !tail.trim().is_empty() {
        return None;
    }
    let mut render = Vec::new();
    let mut used = vec![false; names.len()];
    let mut rest = body.as_str();
    while let Some(i) = rest.find("#{") {
        if i > 0 {
            render.push(JsonMultiPart::Lit(rest[..i].to_string()));
        }
        let j = rest[i + 2..].find('}')?;
        let token = rest[i + 2..i + 2 + j].trim();
        let (pos, cap) = names.iter().enumerate().find_map(|(pos, name)| {
            let r = token.strip_prefix(&format!("{name}["))?;
            let cap: usize = r.strip_suffix(']')?.parse().ok()?;
            Some((pos, cap))
        })?;
        used[pos] = true;
        render.push(JsonMultiPart::Cap(pos, cap));
        rest = &rest[i + 2 + j + 1..];
    }
    if !rest.is_empty() {
        render.push(JsonMultiPart::Lit(rest.to_string()));
    }
    if used.iter().any(|u| !*u) {
        return None;
    }
    Some(JsonShape::MultiMatch { matches, render })
}

/// Nested `each` loops over dug arrays taking
/// the first inner field matching the block regex
/// (unity's downloads scan):
/// `match = nil <dig>&.each do |i| <i-dig>&.each
/// do |d| match = d["f"]&.match(regex) break if
/// match end break if match end next unless match
/// <render>`.
/// `<v> = <dig> [|| <dig>…] <mv> = <v>&.match(regex)
/// next if <mv>.blank? <render>` (quarkclouddrive).
fn parse_json_dig_match(
    joined: &str,
    root: &str,
    cask_locals: &[(String, String)],
) -> Option<super::formula::JsonShape> {
    let (v, rest) = assign_parts(joined.trim())?;
    let mut paths = Vec::new();
    let mut tail: &str = "";
    for (i, side) in rest.split(" || ").enumerate() {
        let (path, t) = jt_dig(side.trim(), root, cask_locals)?;
        if path.is_empty() {
            return None;
        }
        if i + 1 < rest.split(" || ").count() && !t.trim().is_empty() {
            return None;
        }
        paths.push(path);
        tail = t;
    }
    let (mv, rhs) = assign_parts(tail.trim())?;
    let after = rhs
        .trim()
        .strip_prefix(&format!("{v}&.match(regex)"))
        .or_else(|| rhs.trim().strip_prefix(&format!("{v}.match(regex)")))?;
    let after = after
        .trim()
        .strip_prefix(&format!("next if {mv}.blank?"))
        .or_else(|| after.trim().strip_prefix(&format!("next unless {mv}")))?;
    let mut locals: Vec<super::formula::JsonLocal> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let render = jt_render(
        after.trim(),
        root,
        root,
        &mv,
        &mut locals,
        &mut names,
        cask_locals,
    )?;
    if !locals.is_empty() {
        return None;
    }
    Some(super::formula::JsonShape::DigMatch {
        paths,
        render: Box::new(render),
    })
}

/// `version = nil <dig>.filter_map do |_k, item|
/// match = item.dig(…)&.match(regex) next unless
/// match version = <render> break end version`
/// (expressvpn).
fn parse_json_entries_first(
    joined: &str,
    root: &str,
    cask_locals: &[(String, String)],
) -> Option<super::formula::JsonShape> {
    let s = joined.trim();
    let (vvar, rest) = s.split_once(" = nil ")?;
    let vvar = vvar.trim();
    if vvar.is_empty() || !vvar.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let (path, rest) = jt_dig(rest.trim(), root, cask_locals)?;
    let rest = rest.trim();
    let rest = [
        ".filter_map do |",
        ".map do |",
        ".each do |",
        "&.filter_map do |",
    ]
    .iter()
    .find_map(|p| rest.strip_prefix(p))?;
    let (params, rest) = rest.split_once('|')?;
    let item = params.rsplit(',').next()?.trim();
    if item.is_empty() || !item.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let (mv, rhs) = assign_parts(rest.trim())?;
    let (dig, tail) = jt_dig(rhs.trim(), item, cask_locals)?;
    let after = tail
        .trim()
        .strip_prefix("&.match(regex)")
        .or_else(|| tail.trim().strip_prefix(".match(regex)"))?;
    let after = after
        .trim()
        .strip_prefix(&format!("next unless {mv}"))
        .or_else(|| after.trim().strip_prefix(&format!("next if {mv}.blank?")))?;
    let (assigned, render) = assign_parts(after.trim())?;
    if assigned != vvar {
        return None;
    }
    let (render, tail) = render.trim().split_once(" break")?;
    if tail.trim() != format!("end {vvar}") {
        return None;
    }
    let mut locals: Vec<super::formula::JsonLocal> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let render = jt_render(
        render.trim(),
        item,
        root,
        &mv,
        &mut locals,
        &mut names,
        cask_locals,
    )?;
    if !locals.is_empty() {
        return None;
    }
    Some(super::formula::JsonShape::EntriesFirstMatch {
        path,
        dig,
        render: Box::new(render),
    })
}

/// `<outer>.map do |item| item<dig>&.filter_map do
/// |a| a["f"]&.[](regex, 1) end end[.flatten]`
/// (rnote).
fn parse_json_nested_collect(
    joined: &str,
    root: &str,
    cask_locals: &[(String, String)],
) -> Option<super::formula::JsonShape> {
    let (outer, rest) = jt_dig(joined.trim(), root, cask_locals)?;
    let rest = rest.trim();
    let rest = [
        ".map do |",
        ".filter_map do |",
        "&.map do |",
        "&.filter_map do |",
    ]
    .iter()
    .find_map(|p| rest.strip_prefix(p))?;
    let (ivar, rest) = rest.split_once('|')?;
    let ivar = ivar.trim();
    let (inner, rest) = jt_dig(rest.trim(), ivar, cask_locals)?;
    let rest = rest.trim();
    let rest = [
        "&.filter_map do |",
        ".filter_map do |",
        "&.map do |",
        ".map do |",
    ]
    .iter()
    .find_map(|p| rest.strip_prefix(p))?;
    let (avar, rest) = rest.split_once('|')?;
    let avar = avar.trim();
    let (field, tail) = jt_field_tail(rest.trim(), avar)?;
    let tail = tail.trim();
    if tail != "&.[](regex, 1) end end.flatten"
        && tail != "&.[](regex, 1) end end"
        && tail != "&.[](regex,1) end end.flatten"
        && tail != "&.[](regex,1) end end"
    {
        return None;
    }
    Some(super::formula::JsonShape::NestedCollect {
        outer,
        inner,
        field,
    })
}

/// `<outer>.map do |g| g<dig>&.map do |item|
/// <guarded projection> end end[&.flatten]`
/// (synology-drive's utility groups).
fn parse_json_nested_map(
    joined: &str,
    root: &str,
    cask_locals: &[(String, String)],
    has_regex: bool,
) -> Option<super::formula::JsonShape> {
    let (outer, rest) = jt_dig(joined.trim(), root, cask_locals)?;
    let rest = [
        ".map do |",
        ".filter_map do |",
        ".flat_map do |",
        "&.map do |",
        "&.filter_map do |",
        "&.flat_map do |",
    ]
    .iter()
    .find_map(|p| rest.trim().strip_prefix(p))?;
    let (gvar, rest) = rest.split_once('|')?;
    let (inner, rest) = jt_dig(rest.trim(), gvar.trim(), cask_locals)?;
    let rest = [
        "&.filter_map do |",
        ".filter_map do |",
        "&.flat_map do |",
        ".flat_map do |",
        "&.map do |",
        ".map do |",
    ]
    .iter()
    .find_map(|p| rest.trim().strip_prefix(p))?;
    let (avar, rest) = rest.split_once('|')?;
    let body = [" end end&.flatten", " end end.flatten", " end end"]
        .iter()
        .find_map(|suffix| rest.strip_suffix(suffix))?;
    let guard = json_map_guarded(avar.trim(), body.trim())?;
    let uses_regex = guard.capture == super::formula::MapCapture::BlockRegex
        || guard
            .guards
            .iter()
            .any(|g| matches!(g.cond, super::formula::JsonCond::NoneMatch(..)));
    if uses_regex && !has_regex {
        return None;
    }
    Some(super::formula::JsonShape::NestedMap {
        outer,
        inner,
        guard: Box::new(guard),
    })
}

/// Expand simple `#{...}` interpolations in a
/// regex fragment: a quoted literal (the form
/// identifier substitution leaves) or a cask
/// local, inserted raw as Ruby does.
fn jt_expand_regex_interpolations(s: &str, locals: &[(String, String)]) -> Option<String> {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("#{") {
        out.push_str(&rest[..i]);
        let j = rest[i + 2..].find('}')?;
        let token = rest[i + 2..i + 2 + j].trim();
        if token.starts_with(['"', '\'']) {
            let (value, tail) = quoted_token(token)?;
            if !tail.trim().is_empty() {
                return None;
            }
            out.push_str(&value);
        } else {
            let value = locals.iter().find(|(n, _)| n == token)?.1.as_str();
            out.push_str(value);
        }
        rest = &rest[i + 2 + j + 1..];
    }
    out.push_str(rest);
    Some(out)
}

/// `<outer>.map do |o| o<dig>&.map do |i| guards;
/// next o["f"] unless i["g"]; "#{o["f"]},#{i["g"]}"
/// end end[&.flatten]` (gimp's release builds).
fn parse_json_nested_template(
    joined: &str,
    root: &str,
    cask_locals: &[(String, String)],
) -> Option<super::formula::JsonShape> {
    use super::formula::{JsonGuard, JsonShape};
    let (outer, rest) = jt_dig(joined.trim(), root, cask_locals)?;
    let rest = [
        ".map do |",
        ".filter_map do |",
        ".flat_map do |",
        "&.map do |",
        "&.filter_map do |",
        "&.flat_map do |",
    ]
    .iter()
    .find_map(|p| rest.trim().strip_prefix(p))?;
    let (gvar, rest) = rest.split_once('|')?;
    let gvar = gvar.trim();
    let (inner, rest) = jt_dig(rest.trim(), gvar, cask_locals)?;
    let rest = [
        "&.filter_map do |",
        ".filter_map do |",
        "&.flat_map do |",
        ".flat_map do |",
        "&.map do |",
        ".map do |",
    ]
    .iter()
    .find_map(|p| rest.trim().strip_prefix(p))?;
    let (avar, rest) = rest.split_once('|')?;
    let avar = avar.trim();
    let body = [" end end&.flatten", " end end.flatten", " end end"]
        .iter()
        .find_map(|suffix| rest.strip_suffix(suffix))?
        .trim();
    let mut rest = body;
    let mut guards = Vec::new();
    loop {
        let (unless, after) = if let Some(r) = rest.strip_prefix("next unless ") {
            (true, r)
        } else if let Some(r) = rest.strip_prefix("next if ") {
            (false, r)
        } else {
            break;
        };
        let boundary = json_outside_quotes(after, " next ")?;
        // The following `next <field>` is the early
        // return, not another guard.
        if after[boundary + 1..].starts_with(&format!("next {gvar}[")) {
            let cond_s = jt_expand_regex_interpolations(&after[..boundary], cask_locals)?;
            let (cond, tail) = parse_json_cond(avar, cond_s.trim())?;
            if !tail.trim().is_empty() {
                return None;
            }
            guards.push(JsonGuard { unless, cond });
            rest = after[boundary + 1..].trim();
            break;
        }
        let cond_s = jt_expand_regex_interpolations(&after[..boundary], cask_locals)?;
        let (cond, tail) = parse_json_cond(avar, cond_s.trim())?;
        if !tail.trim().is_empty() {
            return None;
        }
        guards.push(JsonGuard { unless, cond });
        rest = after[boundary + 1..].trim();
    }
    let rest = rest.strip_prefix("next ")?;
    let (outer_field, tail) = jt_field_tail(rest, gvar)?;
    // This shape records a truthiness fallback, not Ruby's distinct blankness predicate.
    // Empty strings remain truthy, so admitting `if field.blank?` would change its result.
    let tail = tail.trim().strip_prefix("unless ")?;
    let (inner_field, tail) = jt_field_tail(tail, avar)?;
    let (template, tail) = quoted_token(tail.trim())?;
    if !tail.trim().is_empty()
        || template != format!("#{{{gvar}[\"{outer_field}\"]}},#{{{avar}[\"{inner_field}\"]}}")
    {
        return None;
    }
    Some(JsonShape::NestedTemplate {
        outer,
        inner,
        guards,
        outer_field,
        inner_field,
    })
}

fn parse_json_nested_first(
    joined: &str,
    root: &str,
    cask_locals: &[(String, String)],
) -> Option<super::formula::JsonShape> {
    let s = joined.trim();
    let (mv, rest) = s.split_once(" = nil ")?;
    let mv = mv.trim();
    if mv.is_empty() || !mv.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let (outer, rest) = jt_dig(rest.trim(), root, cask_locals)?;
    let rest = rest
        .trim()
        .strip_prefix("&.each do |")
        .or_else(|| rest.trim().strip_prefix(".each do |"))?;
    let (ivar, rest) = rest.split_once('|')?;
    let ivar = ivar.trim();
    let (inner, rest) = jt_dig(rest.trim(), ivar, cask_locals)?;
    let rest = rest
        .trim()
        .strip_prefix("&.each do |")
        .or_else(|| rest.trim().strip_prefix(".each do |"))?;
    let (dvar, rest) = rest.split_once('|')?;
    let dvar = dvar.trim();
    let rest = rest.trim().strip_prefix(&format!("{mv} = "))?;
    let (field, rest) = jt_field_tail(rest.trim(), dvar)?;
    let rest = rest.trim();
    let rest = rest
        .strip_prefix("&.match(regex)")
        .or_else(|| rest.strip_prefix(".match(regex)"))?;
    let rest = rest.trim().strip_prefix(&format!("break if {mv}"))?;
    let rest = rest.trim().strip_prefix("end")?;
    let rest = rest.trim().strip_prefix(&format!("break if {mv}"))?;
    let rest = rest.trim().strip_prefix("end")?;
    let rest = rest.trim().strip_prefix(&format!("next unless {mv}"))?;
    let mut locals: Vec<super::formula::JsonLocal> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let render = jt_render(
        rest.trim(),
        dvar,
        root,
        mv,
        &mut locals,
        &mut names,
        cask_locals,
    )?;
    if !locals.is_empty() {
        return None;
    }
    Some(super::formula::JsonShape::NestedFirstMatch {
        outer,
        inner,
        field,
        render: Box::new(render),
    })
}

/// "<list> = <dig>&.filter_map { |i| i["k"] }&.uniq
/// <v> = list.max_by { Version.new } next unless v
/// <page> = page_content("url")[:content] next if
/// page.blank? <d> = page[regex, N] next unless d
/// \"#{v},#{d}\"" (eclipse-ide).
fn parse_json_list_max_hop(
    joined: &str,
    root: &str,
    cask_locals: &[(String, String)],
) -> Option<super::formula::JsonShape> {
    use super::formula::{JsonHopPart, JsonShape};
    let (list_name, rest) = assign_parts(joined.trim())?;
    let (path, tail) = jt_dig(rest.trim(), root, cask_locals)?;
    if path.is_empty() {
        return None;
    }
    let tail = tail
        .trim()
        .strip_prefix("&.filter_map")
        .or_else(|| tail.trim().strip_prefix(".filter_map"))?
        .trim();
    let tail = tail.strip_prefix('{')?.trim().strip_prefix('|')?;
    let (ivar, tail) = tail.split_once('|')?;
    let ivar = ivar.trim();
    let tail = tail.trim().strip_prefix(ivar)?;
    let (key, tail) = json_bracket_key(tail.trim())?;
    let tail = tail.trim().strip_prefix('}')?.trim();
    let tail = tail.strip_prefix("&.uniq").unwrap_or(tail).trim();
    let (v_name, rest) = assign_parts(tail)?;
    let rest = rest
        .trim()
        .strip_prefix(&format!("{list_name}.max_by"))?
        .trim();
    let rest = rest.strip_prefix('{')?.trim().strip_prefix('|')?;
    let (x, rest) = rest.split_once('|')?;
    let x = x.trim();
    let rest = rest
        .trim()
        .strip_prefix(&format!("Version.new({x})"))?
        .trim()
        .strip_prefix('}')?
        .trim();
    let rest = rest.strip_prefix(&format!("next unless {v_name}"))?.trim();
    let (page_name, rest) = assign_parts(rest)?;
    let rest = rest
        .trim()
        .strip_prefix("Homebrew::Livecheck::Strategy.page_content(")?
        .trim();
    let (url, rest) = quoted_token(rest)?;
    let rest = rest.trim().strip_prefix(',').unwrap_or(rest.trim()).trim();
    let rest = rest.strip_prefix(")[:content]")?.trim();
    let rest = rest
        .strip_prefix(&format!("next if {page_name}.blank?"))?
        .trim();
    let (date_name, rest) = assign_parts(rest)?;
    let rest = rest
        .trim()
        .strip_prefix(&format!("{page_name}[regex,"))?
        .trim();
    let (cap_s, rest) = rest.split_once(']')?;
    let cap: usize = cap_s.trim().parse().ok()?;
    let rest = rest
        .trim()
        .strip_prefix(&format!("next unless {date_name}"))?
        .trim();
    let (body_str, tail) = quoted_token(rest)?;
    if !tail.trim().is_empty() {
        return None;
    }
    let mut parts: Vec<JsonHopPart> = Vec::new();
    let mut rest = body_str.as_str();
    while let Some(i) = rest.find("#{") {
        if i > 0 {
            parts.push(JsonHopPart::Lit(ruby_unescape_dq(&rest[..i])));
        }
        let j = rest[i + 2..].find('}')?;
        let token = rest[i + 2..i + 2 + j].trim();
        parts.push(if token == v_name {
            JsonHopPart::Max
        } else if token == date_name {
            JsonHopPart::Date
        } else {
            return None;
        });
        rest = &rest[i + 2 + j + 1..];
    }
    if !rest.is_empty() {
        parts.push(JsonHopPart::Lit(ruby_unescape_dq(rest)));
    }
    Some(JsonShape::ListMaxPageHop {
        path,
        key,
        url,
        cap,
        parts,
    })
}

/// "<s> = <dig> next if s.blank? <j> =
/// Json.parse_json(s) <m> = j<dig>&.match(regex)
/// next if m.blank? <render>" (kdocs).
fn parse_json_dig_parse_match(
    joined: &str,
    root: &str,
    cask_locals: &[(String, String)],
) -> Option<super::formula::JsonShape> {
    use super::formula::JsonShape;
    let (s_name, rest) = assign_parts(joined.trim())?;
    let (outer, tail) = jt_dig(rest.trim(), root, cask_locals)?;
    if outer.is_empty() {
        return None;
    }
    let rest = tail
        .trim()
        .strip_prefix(&format!("next if {s_name}.blank?"))?
        .trim();
    let (j_name, rest) = assign_parts(rest)?;
    let rest = rest
        .trim()
        .strip_prefix(&format!(
            "Homebrew::Livecheck::Strategy::Json.parse_json({s_name})"
        ))?
        .trim();
    let (mv, rest) = assign_parts(rest)?;
    let (inner, tail) = jt_dig(rest.trim(), &j_name, cask_locals)?;
    if inner.is_empty() {
        return None;
    }
    let rest = tail
        .trim()
        .strip_prefix("&.match(regex)")
        .or_else(|| tail.trim().strip_prefix(".match(regex)"))?
        .trim();
    let rest = rest
        .strip_prefix(&format!("next if {mv}.blank?"))
        .or_else(|| rest.strip_prefix(&format!("next unless {mv}")))?;
    let mut locals: Vec<super::formula::JsonLocal> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let render = jt_render(
        rest.trim(),
        &j_name,
        &j_name,
        &mv,
        &mut locals,
        &mut names,
        cask_locals,
    )?;
    if !locals.is_empty() {
        return None;
    }
    Some(JsonShape::DigParseMatch {
        outer,
        inner,
        render: Box::new(render),
    })
}

/// `<v> = json["K"] next if v.blank? <c> =
/// page_content("pre#{v}post") next if
/// c[:content].blank? <j> = parse_json(c[:content])
/// <b> = j.dig(...)&.[](regex, N) next if b.blank?
/// "<template over v and b>"` (firefox@nightly).
fn parse_json_key_hop_dig_match(
    joined: &str,
    root: &str,
    cask_locals: &[(String, String)],
) -> Option<super::formula::JsonShape> {
    use super::formula::{JsonHopPart, JsonShape};
    let _ = cask_locals;
    let (v_name, rest) = assign_parts(joined.trim())?;
    let rest = rest.trim().strip_prefix(root)?.trim_start();
    let rest = rest.strip_prefix('[')?;
    let (key, rest) = quoted_token(rest.trim())?;
    let rest = rest.trim().strip_prefix(']')?.trim();
    let rest = rest
        .strip_prefix(&format!("next if {v_name}.blank?"))
        .or_else(|| rest.strip_prefix(&format!("next unless {v_name}")))?
        .trim();
    let (c_name, rest) = assign_parts(rest)?;
    let rest = rest
        .trim()
        .strip_prefix("Homebrew::Livecheck::Strategy.page_content(")?
        .trim();
    let (url, rest) = quoted_token(rest)?;
    let rest = rest.trim().strip_prefix(')')?.trim();
    let (url_pre, url_post) = url.split_once(&format!("#{{{v_name}}}"))?;
    if url_post.contains("#{") {
        return None;
    }
    let url_pre = ruby_unescape_dq(url_pre);
    let url_post = ruby_unescape_dq(url_post);
    let rest = rest
        .strip_prefix(&format!("next if {c_name}[:content].blank?"))?
        .trim();
    let (j_name, rest) = assign_parts(rest)?;
    let rest = rest
        .trim()
        .strip_prefix(&format!(
            "Homebrew::Livecheck::Strategy::Json.parse_json({c_name}[:content])"
        ))?
        .trim();
    let (b_name, rest) = assign_parts(rest)?;
    let (path, tail) = jt_dig(rest.trim(), &j_name, &[])?;
    if path.is_empty() {
        return None;
    }
    let tail = tail.trim();
    let tail = tail
        .strip_prefix("&.[](regex,")
        .or_else(|| tail.strip_prefix(".[](regex,"))?;
    let (cap_s, tail) = tail.split_once(')')?;
    let cap: usize = cap_s.trim().parse().ok()?;
    let rest = tail
        .trim()
        .strip_prefix(&format!("next if {b_name}.blank?"))?
        .trim();
    let (tmpl, tail) = quoted_token(rest)?;
    if !tail.trim().is_empty() {
        return None;
    }
    let mut parts: Vec<JsonHopPart> = Vec::new();
    let mut rest_t = tmpl.as_str();
    while let Some(i) = rest_t.find("#{") {
        if i > 0 {
            parts.push(JsonHopPart::Lit(ruby_unescape_dq(&rest_t[..i])));
        }
        let j = rest_t[i + 2..].find('}')?;
        let token = rest_t[i + 2..i + 2 + j].trim();
        parts.push(if token == v_name {
            JsonHopPart::Key
        } else if token == b_name {
            JsonHopPart::Fetched
        } else {
            return None;
        });
        rest_t = &rest_t[i + 2 + j + 1..];
    }
    if !rest_t.is_empty() {
        parts.push(JsonHopPart::Lit(ruby_unescape_dq(rest_t)));
    }
    Some(JsonShape::KeyHopDigMatch {
        key,
        url_pre,
        url_post,
        path,
        cap,
        parts,
    })
}

/// minecraft-server: current-version gate, then a
/// links-document dig for the first typed match.
fn parse_json_key_current_gate(
    joined: &str,
    root: &str,
    cask_locals: &[(String, String)],
) -> Option<super::formula::JsonShape> {
    use super::formula::JsonShape;
    let _ = cask_locals;
    let (v_name, rest) = assign_parts(joined.trim())?;
    let rest = rest.trim().strip_prefix(root)?.trim_start();
    let rest = rest.strip_prefix('[')?;
    let (key, rest) = quoted_token(rest.trim())?;
    let rest = rest.trim().strip_prefix(']')?.trim();
    let rest = rest
        .strip_prefix(&format!("next unless {v_name} "))?
        .trim()
        .strip_prefix(&format!("next version if {v_name} == version.csv.first "))?
        .trim();
    let (c_name, rest) = assign_parts(rest)?;
    let rest = rest
        .trim()
        .strip_prefix("Homebrew::Livecheck::Strategy.page_content(")?
        .trim();
    let (url, rest) = quoted_token(rest)?;
    if url.contains("#{") {
        return None;
    }
    let rest = rest.trim();
    let rest = rest.strip_prefix(',').map_or(rest, str::trim);
    let rest = rest.strip_prefix(")[:content] ")?.trim();
    let rest = rest
        .strip_prefix(&format!("next {v_name} if {c_name}.blank? "))?
        .trim();
    let (j_name, rest) = assign_parts(rest)?;
    let rest = rest
        .trim()
        .strip_prefix(&format!(
            "Homebrew::Livecheck::Strategy::Json.parse_json({c_name})"
        ))?
        .trim();
    let (h_name, rest) = assign_parts(rest)?;
    let rest = rest.trim().strip_prefix("nil ")?.trim();
    let (path, tail) = jt_dig(rest, &j_name, &[])?;
    if path.is_empty() {
        return None;
    }
    let tail = tail.trim().strip_prefix("&.each do |")?;
    let (link, rest) = tail.split_once('|')?;
    let link = link.trim();
    if link.is_empty() || !link.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let rest = rest.trim().strip_prefix(&format!("next if {link}["))?;
    let (type_key, rest) = quoted_token(rest.trim())?;
    let rest = rest.trim().strip_prefix("] != ")?.trim();
    let (type_val, rest) = quoted_token(rest)?;
    let (m_name, rest) = rest.trim().split_once(" = ")?;
    let m_name = m_name.trim();
    if m_name.is_empty()
        || !m_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    let rest = rest.trim().strip_prefix(&format!("{link}["))?;
    let (url_key, rest) = quoted_token(rest.trim())?;
    let rest = rest
        .trim()
        .strip_prefix("]&.match(regex) ")?
        .trim()
        .strip_prefix(&format!("next if {m_name}.blank? "))?
        .trim()
        .strip_prefix(&format!("{h_name} = {m_name}["))?;
    let (cap_s, rest) = rest.split_once(']')?;
    let cap: usize = cap_s.trim().parse().ok()?;
    let rest = rest.trim().strip_prefix("break end ")?.trim();
    if rest != format!("{h_name} ? \"#{{{v_name}}},#{{{h_name}}}\" : {v_name}") {
        return None;
    }
    Some(JsonShape::KeyCurrentGateDigMatch {
        key,
        url: ruby_unescape_dq(&url),
        path,
        type_key,
        type_val,
        url_key,
        cap,
    })
}

fn parse_json_shape(
    params: &str,
    lines: &[String],
    cask_locals: &[(String, String)],
) -> Option<super::formula::JsonBlock> {
    use super::formula::{JsonBlock, JsonKey, JsonShape};
    let (root, has_regex) = block_params(params)?;
    let joined = jt_substitute_idents(&join_block_lines(lines), cask_locals);
    let body = joined.trim();
    if let Some(shape) = parse_json_nested_first(body, &root, cask_locals) {
        return Some(JsonBlock { has_regex, shape });
    }
    if let Some(shape) = parse_json_nested_map(body, &root, cask_locals, has_regex) {
        return Some(JsonBlock { has_regex, shape });
    }
    if let Some(shape) = parse_json_nested_template(body, &root, cask_locals) {
        return Some(JsonBlock { has_regex, shape });
    }
    if has_regex {
        for shape in [
            parse_json_dig_parse_match(body, &root, cask_locals),
            parse_json_list_max_hop(body, &root, cask_locals),
            parse_json_key_hop_dig_match(body, &root, cask_locals),
            parse_json_key_current_gate(body, &root, cask_locals),
            parse_json_multi_match(&root, lines, cask_locals),
            parse_json_dig_match(body, &root, cask_locals),
            parse_json_entries_first(body, &root, cask_locals),
            parse_json_nested_collect(body, &root, cask_locals),
        ] {
            if let Some(shape) = shape {
                return Some(JsonBlock { has_regex, shape });
            }
        }
    }
    let mut s = body;
    // The two-statement match form:
    // `m = <path>&.match(regex) m[1]`.
    let mut match_form = false;
    if let Some((var, rest)) = s.split_once(" = ")
        && !var.contains(' ')
        && let Some(r) = rest.strip_suffix(&format!(" {var}[1]"))
        && let Some(p) = r.strip_suffix("&.match(regex)")
    {
        s = p;
        match_form = true;
    }
    let mut cur = s.strip_prefix(root.as_str())?;
    if let Some(rest) = cur.strip_prefix(".first") {
        let (key, tail) = json_bracket_key(rest)?;
        if !tail.is_empty() {
            return None;
        }
        return Some(JsonBlock {
            has_regex,
            shape: JsonShape::FirstKey(key),
        });
    }
    let mut path: Vec<JsonKey> = Vec::new();
    loop {
        if let Some(rest) = cur
            .strip_prefix(".first")
            .or_else(|| cur.strip_prefix("&.first"))
            && (rest.is_empty() || rest.starts_with(['.', '&', '[']))
        {
            path.push(JsonKey::Idx(0));
            cur = rest;
        } else if let Some(rest) = cur
            .strip_prefix("&.[](")
            .or_else(|| cur.strip_prefix(".[]("))
        {
            match json_bracket_call_key(rest) {
                Some((k, tail)) => {
                    path.push(k);
                    cur = tail;
                }
                None => break,
            }
        } else if let Some(rest) = cur
            .strip_prefix(".dig(")
            .or_else(|| cur.strip_prefix("&.dig("))
        {
            let close = rest.find(')')?;
            for arg in rest[..close].split(',') {
                let arg = arg.trim();
                if let Some(k) = arg.strip_prefix('"').and_then(|a| a.strip_suffix('"')) {
                    if k.contains(['"', '\\']) {
                        return None;
                    }
                    path.push(JsonKey::Key(k.to_string()));
                } else if let Ok(i) = arg.parse::<usize>() {
                    path.push(JsonKey::Idx(i));
                } else {
                    return None;
                }
            }
            cur = &rest[close + 1..];
        } else if cur.starts_with('[') {
            let (key, tail) = json_bracket_key(cur)?;
            path.push(JsonKey::Key(key));
            cur = tail;
        } else {
            break;
        }
    }
    if match_form {
        if !cur.is_empty() || !has_regex {
            return None;
        }
        return Some(JsonBlock {
            has_regex,
            shape: JsonShape::DigRegex(path),
        });
    }
    let cur = cur.trim();
    let shape = if cur.is_empty() {
        JsonShape::Dig(path)
    } else if let Some(rest) = cur
        .strip_prefix("&.join(")
        .or_else(|| cur.strip_prefix(".join("))
    {
        let (sep, tail) = quoted_token(rest.trim_start())?;
        if tail.trim() != ")" {
            return None;
        }
        JsonShape::DigJoin(path, sep)
    } else if let Some(key) = json_values_key(cur) {
        JsonShape::ValuesKey(path, key)
    } else if let Some((pat, flags, tmpl)) = cur
        .strip_prefix(".to_s")
        .and_then(|r| sub_backref_args(r.trim()))
        .filter(|(_, _, tmpl)| !tmpl.contains('\\'))
    {
        JsonShape::DigSubPlain(path, pat, flags, tmpl)
    } else if let Some(rest) = cur
        .strip_prefix("&.match(regex)&.captures")
        .or_else(|| cur.strip_prefix(".match(regex)&.captures"))
        .or_else(|| cur.strip_prefix("&.match(regex).captures"))
        .or_else(|| cur.strip_prefix(".match(regex).captures"))
    {
        if !rest.is_empty() || !has_regex {
            return None;
        }
        JsonShape::DigCaptures(path)
    } else if let Some(rest) = cur
        .strip_prefix("&.scan(regex)")
        .or_else(|| cur.strip_prefix(".scan(regex)"))
    {
        if !has_regex {
            return None;
        }
        let rest = rest.trim();
        let rest = rest
            .strip_prefix("&.map")
            .or_else(|| rest.strip_prefix(".map"))?
            .trim();
        let inner = if let Some(r) = rest.strip_prefix('{') {
            r.strip_suffix('}')?.trim()
        } else {
            let r = rest.strip_prefix("do ")?;
            r.strip_suffix(" end")?.trim()
        };
        let inner = inner.strip_prefix('|')?;
        let var_end = inner.find('|')?;
        let var = inner[..var_end].trim();
        let parts = page_expr_parts(var, inner[var_end + 1..].trim())?;
        JsonShape::DigScan(path, parts)
    } else if let Some(sep) = split_first_tail(cur) {
        JsonShape::DigSplitFirst(path, sep)
    } else if let Some(rest) = cur
        .strip_prefix("&.delete(")
        .or_else(|| cur.strip_prefix(".delete("))
    {
        let (chars, tail) = quoted_token(rest)?;
        if tail.trim() != ")" || chars.contains(['\\', '^']) {
            return None;
        }
        JsonShape::DigDelete(path, chars)
    } else if let Some(rest) = cur
        .strip_prefix("&.[](regex, 1)")
        .or_else(|| cur.strip_prefix(".[](regex, 1)"))
    {
        if !rest.is_empty() || !has_regex {
            return None;
        }
        JsonShape::DigRegex(path)
    } else if let Some(rest) = cur
        .strip_prefix("&.match(regex) { |")
        .or_else(|| cur.strip_prefix(".match(regex) { |"))
    {
        // `….match(regex) { |m| m[1] }`
        let inner = rest.strip_suffix('}')?.trim();
        let (var, expr) = inner.split_once('|')?;
        if expr.trim() != format!("{}[1]", var.trim()) || !has_regex {
            return None;
        }
        JsonShape::DigRegex(path)
    } else if let Some(shape) = parse_values_map_shape(&path, cur) {
        shape
    } else {
        // A trailing `&.max` after the map block.
        let (cur, want_max) = match cur
            .strip_suffix("&.max")
            .or_else(|| cur.strip_suffix(".max"))
        {
            Some(c) if c.trim_end().ends_with("end") || c.trim_end().ends_with('}') => (c, true),
            _ => (cur, false),
        };
        let rest = strip_map_open(cur)?;
        let shaped = if let Some((kvar, vvar, inner)) = json_entries_parts(rest) {
            // Leading key guards, then the value
            // projection under the value variable.
            let mut s = inner.trim();
            let mut guards: Vec<super::formula::JsonGuard> = Vec::new();
            loop {
                let (unless, r) = if let Some(r) = s.strip_prefix("next unless ") {
                    (true, r)
                } else if let Some(r) = s.strip_prefix("next if ") {
                    (false, r)
                } else {
                    break;
                };
                let Some(r) = r
                    .trim()
                    .strip_prefix(kvar.as_str())
                    .and_then(|r| r.strip_prefix(".match?("))
                else {
                    break;
                };
                let Some((pat, flags, t)) = regex_literal(r.trim()) else {
                    break;
                };
                let Some(t) = t.trim().strip_prefix(')') else {
                    break;
                };
                guards.push(super::formula::JsonGuard {
                    unless,
                    cond: super::formula::JsonCond::KeyMatchRe(pat, flags),
                });
                s = t.trim();
            }
            // A key projection (`|version, item|
            // <guards> version`): keys passing
            // value guards, julia-app form.
            let keys_shape = {
                let mut trial = guards.clone();
                let mut rest = s;
                let mut ok = true;
                loop {
                    let (unless, r) = if let Some(r) = rest.strip_prefix("next if ") {
                        (false, r)
                    } else if let Some(r) = rest.strip_prefix("next unless ") {
                        (true, r)
                    } else {
                        break;
                    };
                    match parse_json_cond(&vvar, r) {
                        Some((cond, r)) => {
                            trial.push(super::formula::JsonGuard { unless, cond });
                            rest = r.trim();
                        }
                        None => {
                            ok = false;
                            break;
                        }
                    }
                }
                if ok && rest == kvar.as_str() && !trial.is_empty() {
                    Some(JsonShape::EntriesKeys(path.clone(), trial))
                } else {
                    None
                }
            };
            if let Some(shape) = keys_shape {
                shape
            } else {
                let mut guard = match json_map_guarded(&vvar, s) {
                    Some(g) => g,
                    None => {
                        let (item_key, capture, (tr, sub)) = json_map_item(&vvar, s)?;
                        super::formula::MapGuard {
                            item_key,
                            capture,
                            tr,
                            sub,
                            guards: Vec::new(),
                        }
                    }
                };
                if guard.capture == super::formula::MapCapture::BlockRegex && !has_regex {
                    return None;
                }
                guards.append(&mut guard.guards);
                guard.guards = guards;
                JsonShape::EntriesMap(path, Box::new(guard))
            }
        } else {
            let (var, inner) = json_map_parts(rest)?;
            if let Some((item_key, capture, (tr, sub))) = json_map_item(&var, &inner) {
                if capture == super::formula::MapCapture::BlockRegex && !has_regex {
                    return None;
                }
                if tr.is_none() && sub.is_none() && capture == super::formula::MapCapture::None {
                    JsonShape::MapKey(path, item_key)
                } else if tr.is_none()
                    && sub.is_none()
                    && capture == super::formula::MapCapture::BlockRegex
                {
                    JsonShape::MapRegex(path, item_key)
                } else {
                    JsonShape::MapGuarded(
                        path,
                        Box::new(super::formula::MapGuard {
                            item_key,
                            capture,
                            tr,
                            sub,
                            guards: Vec::new(),
                        }),
                    )
                }
            } else {
                let guard = json_map_guarded(&var, &inner)?;
                let uses_regex = guard.capture == super::formula::MapCapture::BlockRegex
                    || guard
                        .guards
                        .iter()
                        .any(|g| matches!(g.cond, super::formula::JsonCond::NoneMatch(..)));
                if uses_regex && !has_regex {
                    return None;
                }
                JsonShape::MapGuarded(path, Box::new(guard))
            }
        };
        if want_max {
            match shaped {
                JsonShape::MapGuarded(p, g) => JsonShape::MapGuardedMax(p, g),
                JsonShape::MapKey(p, k) => JsonShape::MapGuardedMax(
                    p,
                    Box::new(super::formula::MapGuard {
                        item_key: k,
                        capture: super::formula::MapCapture::None,
                        tr: None,
                        sub: None,
                        guards: Vec::new(),
                    }),
                ),
                _ => return None,
            }
        } else {
            shaped
        }
    };
    Some(JsonBlock { has_regex, shape })
}

/// Substitute `#{<cask-local>}` tokens (with an
/// optional case transform) in one block-body line;
/// unknown tokens stay verbatim.
fn substitute_local_tokens(line: &str, locals: &[(String, String)]) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(i) = rest.find("#{") {
        let Some(j) = rest[i + 2..].find('}') else {
            break;
        };
        let token = &rest[i + 2..i + 2 + j];
        let value = locals
            .iter()
            .find(|(n, _)| n == token)
            .map(|(_, v)| v.clone())
            .or_else(|| {
                token
                    .strip_suffix(".downcase")
                    .and_then(|b| locals.iter().find(|(n, _)| n == b))
                    .map(|(_, v)| v.to_lowercase())
            })
            .or_else(|| {
                token
                    .strip_suffix(".upcase")
                    .and_then(|b| locals.iter().find(|(n, _)| n == b))
                    .map(|(_, v)| v.to_uppercase())
            });
        out.push_str(&rest[..i]);
        match value {
            Some(v) => out.push_str(&v),
            None => {
                out.push_str("#{");
                out.push_str(token);
                out.push('}');
            }
        }
        rest = &rest[i + 2 + j + 1..];
    }
    out.push_str(rest);
    out
}

/// A single literal argument of a bracket call
/// (`("k")` / `(0)` after `.[]`): the key and tail.
/// Non-literal calls (`[](regex, 1)`) are not digs.
fn json_bracket_call_key(rest: &str) -> Option<(super::formula::JsonKey, &str)> {
    use super::formula::JsonKey;
    let rest = rest.trim_start();
    if let Some((k, t)) = quoted_token(rest) {
        if k.contains(['"', '\\']) {
            return None;
        }
        let t = t.trim().strip_prefix(')')?;
        return Some((JsonKey::Key(k), t));
    }
    let end = rest.find(')')?;
    let i: usize = rest[..end].trim().parse().ok()?;
    Some((JsonKey::Idx(i), &rest[end + 1..]))
}

/// A dig path from `<root>` at the start of `s`
/// (`["k"]`, `[0]`, `.dig("a", 0)` segments): the
/// path and remaining tail.
fn jt_dig<'a>(
    s: &'a str,
    root: &str,
    locals: &[(String, String)],
) -> Option<(Vec<super::formula::JsonKey>, &'a str)> {
    use super::formula::JsonKey;
    let mut cur = s.strip_prefix(root)?;
    if !cur.is_empty() && !cur.starts_with(['[', '.', '&', ' ', '}', ')', ',']) {
        return None;
    }
    let mut path = Vec::new();
    loop {
        if let Some(rest) = cur
            .strip_prefix(".first")
            .or_else(|| cur.strip_prefix("&.first"))
            && (rest.is_empty() || rest.starts_with(['.', '&', '[']))
        {
            path.push(JsonKey::Idx(0));
            cur = rest;
        } else if let Some(rest) = cur
            .strip_prefix(".dig(")
            .or_else(|| cur.strip_prefix("&.dig("))
        {
            let close = rest.find(')')?;
            for arg in rest[..close].split(',') {
                let arg = arg.trim();
                if let Some(k) = arg.strip_prefix('"').and_then(|a| a.strip_suffix('"')) {
                    if k.contains(['"', '\\']) {
                        return None;
                    }
                    path.push(JsonKey::Key(k.to_string()));
                } else if let Some((_, v)) = locals.iter().find(|(n, _)| n == arg) {
                    path.push(JsonKey::Key(v.clone()));
                } else if let Ok(i) = arg.parse::<usize>() {
                    path.push(JsonKey::Idx(i));
                } else {
                    return None;
                }
            }
            cur = &rest[close + 1..];
        } else if let Some(rest) = cur
            .strip_prefix("&.[](")
            .or_else(|| cur.strip_prefix(".[]("))
        {
            match json_bracket_call_key(rest) {
                Some((k, tail)) => {
                    path.push(k);
                    cur = tail;
                }
                None => break,
            }
        } else if cur.starts_with('[') && !cur.starts_with("[regex") && !cur.starts_with("[/") {
            // A bare local as the key (`json[os]`).
            if let Some(close) = cur.find(']')
                && let Some((_, v)) = locals.iter().find(|(n, _)| n == cur[1..close].trim())
            {
                path.push(JsonKey::Key(v.clone()));
                cur = &cur[close + 1..];
                continue;
            }
            if let Some(rest) = cur.strip_prefix('[')
                && let Ok(i) = rest
                    .trim_start()
                    .split(']')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .parse::<usize>()
            {
                let close = cur.find(']')?;
                path.push(JsonKey::Idx(i));
                cur = &cur[close + 1..];
                continue;
            }
            let (key, tail) = json_bracket_key(cur)?;
            path.push(JsonKey::Key(key));
            cur = tail;
        } else {
            break;
        }
    }
    Some((path, cur))
}

/// Prepend a base dig path to a local's own path
/// (object-local reads: `v = json.dig(…)` then
/// `v["k"]`).
fn jt_prepend(src: JtSrc, base: &[super::formula::JsonKey]) -> Option<JtSrc> {
    use super::formula::JsonLocal;
    let cat =
        |p: &Vec<super::formula::JsonKey>| base.iter().cloned().chain(p.iter().cloned()).collect();
    Some(match src {
        JtSrc::Match(p) => JtSrc::Match(cat(&p)),
        JtSrc::Local(local) => JtSrc::Local(match local {
            JsonLocal::Dig(p) => JsonLocal::Dig(cat(&p)),
            JsonLocal::DigSplit(p, a, b) => JsonLocal::DigSplit(cat(&p), a, b),
            JsonLocal::DigCapture(p) => JsonLocal::DigCapture(cat(&p)),
            JsonLocal::DigCaptureLit(p, a, b) => JsonLocal::DigCaptureLit(cat(&p), a, b),
            JsonLocal::DigTail(p, t) => JsonLocal::DigTail(cat(&p), t),
            JsonLocal::DigSubRe(p, a, b, c) => JsonLocal::DigSubRe(cat(&p), a, b, c),
            JsonLocal::SplitRowsFind {
                path,
                line_sep,
                field_sep,
                first_suffix,
            } => JsonLocal::SplitRowsFind {
                path: cat(&path),
                line_sep,
                field_sep,
                first_suffix,
            },
            JsonLocal::DigOr(_) | JsonLocal::DigOrTail(..) => return None,
        }),
    })
}

/// A local's source from an assignment RHS, or the
/// match marker for `….match(regex)`.
enum JtSrc {
    Local(super::formula::JsonLocal),
    Match(Vec<super::formula::JsonKey>),
}

fn jt_local_src(rhs: &str, root: &str, locals: &[(String, String)]) -> Option<JtSrc> {
    use super::formula::JsonLocal;
    // `(<dig> || <dig>)&.tail`: a parenthesized
    // or-chain with a string tail.
    let rhs_t = rhs.trim();
    if rhs_t.starts_with('(') {
        let bytes = rhs_t.as_bytes();
        let mut close = None;
        for i in 1..bytes.len() {
            if bytes[i] == b')' && jt_depth_at(rhs_t, i).2 == 1 {
                close = Some(i);
                break;
            }
        }
        if let Some(close) = close {
            let inner = &rhs_t[1..close];
            let tail = rhs_t[close + 1..].trim();
            if inner.contains(" || ") {
                let mut paths = Vec::new();
                for part in inner.split(" || ") {
                    let (path, t) = jt_dig(part.trim(), root, locals)?;
                    if !t.trim().is_empty() {
                        return None;
                    }
                    paths.push(path);
                }
                return Some(JtSrc::Local(JsonLocal::DigOrTail(paths, jt_tail(tail)?)));
            }
            if tail.is_empty() {
                return jt_local_src(inner, root, locals);
            }
            return None;
        }
    }
    // `<dig> || <dig>` chains: first present wins.
    if rhs.contains(" || ") {
        let mut paths = Vec::new();
        for part in rhs.split(" || ") {
            let (path, tail) = jt_dig(part.trim(), root, locals)?;
            if !tail.trim().is_empty() {
                return None;
            }
            paths.push(path);
        }
        return Some(JtSrc::Local(JsonLocal::DigOr(paths)));
    }
    let (path, tail) = jt_dig(rhs.trim(), root, locals)?;
    let tail = tail.trim();
    if let Some((pat, flags, tmpl)) = sub_backref_args(tail) {
        return Some(JtSrc::Local(JsonLocal::DigSubRe(path, pat, flags, tmpl)));
    }
    match tail {
        "" => Some(JtSrc::Local(JsonLocal::Dig(path))),
        "&.match(regex)" | ".match(regex)" => Some(JtSrc::Match(path)),
        "&.[](regex, 1)" | ".[](regex, 1)" | "[regex, 1]" => {
            Some(JtSrc::Local(JsonLocal::DigCapture(path)))
        }
        _ => {
            if let Some(rest) = tail
                .strip_prefix("&.split(")
                .or_else(|| tail.strip_prefix(".split("))
            {
                let (sep, t) = quoted_token(rest.trim_start())?;
                let t = t.trim().strip_prefix(')')?.trim();
                let idx = match t {
                    "&.first" | ".first" => 0,
                    "&.second" | ".second" => 1,
                    _ => return None,
                };
                return Some(JtSrc::Local(JsonLocal::DigSplit(path, sep, idx)));
            }
            if let Some(jt) = jt_tail(tail) {
                return Some(JtSrc::Local(JsonLocal::DigTail(path, jt)));
            }
            let lit_at = tail
                .strip_prefix("&.[](")
                .or_else(|| tail.strip_prefix(".[]("))
                .or_else(|| tail.strip_prefix('['))?;
            let (pat, flags, t) = regex_literal(lit_at.trim_start())?;
            let t = t.trim().strip_prefix(',')?.trim();
            if !(t.starts_with("1]") || t.starts_with("1)")) {
                return None;
            }
            Some(JtSrc::Local(JsonLocal::DigCaptureLit(path, pat, flags)))
        }
    }
}

/// A trailing `.tr`/`.sub`/… transform at the
/// start of `tail` (must consume it whole).
fn jt_tail(tail: &str) -> Option<super::formula::JsonTail> {
    use super::formula::JsonTail;
    let tail = tail.trim();
    if tail.is_empty() {
        return Some(JsonTail::None);
    }
    if matches!(tail, "&.strip" | ".strip") {
        return Some(JsonTail::Strip);
    }
    let (op, rest) = [
        ("to_s", ".to_s"),
        ("tr", ".tr("),
        ("sub", ".sub("),
        ("delete_prefix", ".delete_prefix("),
        ("delete_suffix", ".delete_suffix("),
        ("delete", ".delete("),
    ]
    .iter()
    .find_map(|(name, pat)| {
        tail.strip_prefix(pat)
            .or_else(|| tail.strip_prefix(&format!("&{pat}")))
            .map(|r| (*name, r))
    })?;
    match op {
        "to_s" => {
            if rest.trim().is_empty() {
                Some(JsonTail::ToS)
            } else {
                None
            }
        }
        "tr" | "sub" => {
            let (a, t) = quoted_token(rest.trim_start())?;
            let t = t.trim().strip_prefix(',')?.trim();
            let (b, t) = quoted_token(t)?;
            let t = t.trim().strip_prefix(')')?.trim();
            if !(t.is_empty() || t == ".to_s" || t == "&.to_s") {
                return None;
            }
            Some(if op == "tr" {
                JsonTail::Tr(a, b)
            } else {
                JsonTail::Sub(a, b)
            })
        }
        _ => {
            let (a, t) = quoted_token(rest.trim_start())?;
            if t.trim() != ")" {
                return None;
            }
            Some(match op {
                "delete_prefix" => JsonTail::DeletePrefix(a),
                "delete_suffix" => JsonTail::DeleteSuffix(a),
                _ => JsonTail::DeleteChars(a),
            })
        }
    }
}

/// Depth of unclosed (do, brace, paren) spans at
/// byte `upto` in `s`, outside quotes. Words count
/// with identifier boundaries (`/end/` in a regex
/// does not).
fn jt_depth_at(s: &str, upto: usize) -> (i32, i32, i32) {
    let b = s.as_bytes();
    let end = upto.min(b.len());
    let (mut do_d, mut brace, mut paren) = (0i32, 0i32, 0i32);
    let (mut dq, mut sq) = (false, false);
    let word = |i: usize, w: &str| -> bool {
        let wb = w.as_bytes();
        if i + wb.len() > end || &b[i..i + wb.len()] != wb {
            return false;
        }
        let before_ok =
            i == 0 || matches!(b[i - 1], b' ' | b'\t' | b'&' | b'.' | b'(' | b'|' | b'{');
        let after = b.get(i + wb.len());
        let after_ok =
            after.is_none_or(|c| matches!(c, b' ' | b'\t' | b'|' | b'.' | b')' | b'}' | b','));
        before_ok && after_ok
    };
    let mut i = 0;
    while i < end {
        match b[i] {
            b'\\' if dq || sq => i += 1,
            b'"' if !sq => dq = !dq,
            b'\'' if !dq => sq = !sq,
            b'd' if !dq && !sq && word(i, "do") => {
                do_d += 1;
                i += 1;
            }
            b'e' if !dq && !sq && word(i, "end") => {
                do_d -= 1;
                i += 2;
            }
            b'{' if !dq && !sq => brace += 1,
            b'}' if !dq && !sq => brace -= 1,
            b'(' if !dq && !sq => paren += 1,
            b')' if !dq && !sq => paren -= 1,
            _ => {}
        }
        i += 1;
    }
    (do_d, brace, paren)
}

/// Split a condition on top-level ` && ` (not
/// inside quotes or do/brace/paren spans).
fn jt_split_and(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut from = 0usize;
    while let Some(rel) = s[from..].find(" && ") {
        let idx = from + rel;
        if jt_depth_at(s, idx) == (0, 0, 0) {
            out.push(&s[start..idx]);
            start = idx + 4;
        }
        from = idx + 4;
    }
    out.push(&s[start..]);
    out
}

/// Join continuation statements: a line ending in
/// `&&`/`||`/postfix `if`/`unless`, or leaving a
/// do/brace/paren span open, absorbs the next.
fn jt_join_stmts(body: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for stmt in body {
        let continues = out.last().is_some_and(|prev: &String| {
            let t = prev.trim_end();
            t.ends_with("&&")
                || t.ends_with("||")
                || t.ends_with(" if")
                || t.ends_with(" unless")
                || jt_depth_at(t, t.len()) != (0, 0, 0)
        });
        if continues && let Some(prev) = out.last_mut() {
            prev.push(' ');
            prev.push_str(stmt.trim());
        } else {
            out.push(stmt.clone());
        }
    }
    out
}

/// Replace bare cask-local identifier reads with
/// their quoted host values, outside string text
/// (interpolations included, assignment targets
/// and kwarg keys excluded). Locals carrying
/// quotes/escapes/interpolation stay verbatim.
fn jt_substitute_idents(s: &str, locals: &[(String, String)]) -> String {
    let subs: Vec<(&str, &str)> = locals
        .iter()
        .filter(|(n, v)| !n.is_empty() && !v.contains(['"', '\\', '#']))
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect();
    if subs.is_empty() {
        return s.to_string();
    }
    #[derive(Clone, Copy, PartialEq)]
    enum M {
        Code,
        Dq,
        Sq,
    }
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut mode = M::Code;
    let mut stack: Vec<i32> = Vec::new();
    let mut brace = 0i32;
    let mut i = 0usize;
    while i < b.len() {
        match mode {
            M::Dq => {
                match b[i] {
                    b'\\' => {
                        out.push('\\');
                        if let Some(c) = s[i + 1..].chars().next() {
                            out.push(c);
                            i += c.len_utf8();
                        }
                    }
                    b'"' => {
                        mode = M::Code;
                        out.push('"');
                    }
                    b'#' if b.get(i + 1) == Some(&b'{') => {
                        stack.push(brace);
                        mode = M::Code;
                        out.push_str("#{");
                        i += 1;
                    }
                    _ => {
                        let c = s[i..].chars().next().unwrap_or(' ');
                        out.push(c);
                        i += c.len_utf8();
                        continue;
                    }
                }
                i += 1;
            }
            M::Sq => {
                match b[i] {
                    b'\\' => {
                        out.push('\\');
                        if let Some(c) = s[i + 1..].chars().next() {
                            out.push(c);
                            i += c.len_utf8();
                        }
                    }
                    b'\'' => {
                        mode = M::Code;
                        out.push('\'');
                    }
                    _ => {
                        let c = s[i..].chars().next().unwrap_or(' ');
                        out.push(c);
                        i += c.len_utf8();
                        continue;
                    }
                }
                i += 1;
            }
            M::Code => match b[i] {
                b'"' => {
                    mode = M::Dq;
                    out.push('"');
                    i += 1;
                }
                b'\'' => {
                    mode = M::Sq;
                    out.push('\'');
                    i += 1;
                }
                b'{' => {
                    brace += 1;
                    out.push('{');
                    i += 1;
                }
                b'}' => {
                    if stack.last() == Some(&brace) {
                        stack.pop();
                        mode = M::Dq;
                    } else {
                        brace -= 1;
                    }
                    out.push('}');
                    i += 1;
                }
                c if c.is_ascii_alphabetic() || c == b'_' => {
                    let start = i;
                    while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                        i += 1;
                    }
                    let word = &s[start..i];
                    let prev = out.chars().last();
                    let after = s[i..].trim_start();
                    let is_assign = after.starts_with('=') && !after[1..].starts_with(['=', '~']);
                    let is_kwarg = s[i..].starts_with(':') && !s[i..].starts_with("::");
                    if !is_assign
                        && !is_kwarg
                        && !matches!(prev, Some('.' | ':'))
                        && let Some((_, v)) = subs.iter().find(|(n, _)| *n == word)
                    {
                        out.push('"');
                        out.push_str(v);
                        out.push('"');
                    } else {
                        out.push_str(word);
                    }
                }
                _ => {
                    let c = s[i..].chars().next().unwrap_or(' ');
                    out.push(c);
                    i += c.len_utf8();
                }
            },
        }
    }
    out
}

/// Replace `<cask-local>.to_s` with its quoted
/// host value (locals carrying quotes, escapes or
/// pending interpolation are left verbatim, which
/// keeps the body delegated).
fn jt_substitute_to_s(stmt: &str, locals: &[(String, String)]) -> String {
    let mut out = stmt.to_string();
    for (name, value) in locals {
        if value.contains(['"', '\\', '#']) {
            continue;
        }
        let needle = format!("{name}.to_s");
        let mut rebuilt = String::with_capacity(out.len());
        let mut rest = out.as_str();
        while let Some(i) = rest.find(&needle) {
            let before = rest[..i].chars().last();
            let after = rest[i + needle.len()..].chars().next();
            rebuilt.push_str(&rest[..i]);
            if before.is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_' && c != '.')
                && after.is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_')
            {
                rebuilt.push('"');
                rebuilt.push_str(value);
                rebuilt.push('"');
            } else {
                rebuilt.push_str(&needle);
            }
            rest = &rest[i + needle.len()..];
        }
        rebuilt.push_str(rest);
        out = rebuilt;
    }
    out
}

/// An assignment split tolerating tabs around `=`
/// (`timestamp\t= …`): plain ` = ` first, else a
/// whitespace-flanked bare `=` after an identifier.
fn jt_assignment(s: &str) -> Option<(&str, &str)> {
    if let Some(pair) = s.split_once(" = ") {
        return Some(pair);
    }
    let b = s.as_bytes();
    let (mut dq, mut sq) = (false, false);
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' if dq || sq => i += 1,
            b'"' if !sq => dq = !dq,
            b'\'' if !dq => sq = !sq,
            b'=' if !dq && !sq => {
                let before_ws = i > 0 && b[i - 1].is_ascii_whitespace();
                let prev = s[..i].trim_end().chars().last();
                let next = b.get(i + 1).copied();
                if before_ws
                    && !matches!(
                        prev,
                        Some('=' | '!' | '<' | '>' | '~' | '+' | '-' | '*' | '/')
                    )
                    && !matches!(next, Some(b'=' | b'>' | b'~'))
                    && next.is_some_and(|c| c.is_ascii_whitespace())
                {
                    return Some((&s[..i], s[i + 1..].trim_start()));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// A one-line brace map as do-form statements
/// (`….filter_map { |v| body }` → open/body/end).
fn jt_brace_map_rewrite(stmt: &str) -> Option<Vec<String>> {
    for pat in [
        ".flat_map { |",
        "&.flat_map { |",
        ".filter_map { |",
        "&.filter_map { |",
        ".map { |",
        "&.map { |",
    ] {
        if let Some(idx) = stmt.find(pat) {
            let head = &stmt[..idx + pat.len() - 4];
            let rest = &stmt[idx + pat.len()..];
            let var_end = rest.find('|')?;
            let var = rest[..var_end].trim();
            if var.is_empty() || !var.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return None;
            }
            let inner = rest[var_end + 1..].trim().strip_suffix('}')?.trim();
            if inner.is_empty() {
                return None;
            }
            return Some(vec![
                format!("{head} do |{var}|"),
                inner.to_string(),
                "end".to_string(),
            ]);
        }
    }
    None
}

/// A map over a dug local: `<name> = <dig>` (+ an
/// optional `next if <name>.blank?`) then
/// `<name>.filter_map do |v| … end` as the last
/// statements. Returns (path, map, item_var, body).
fn jt_local_map<'a>(
    stmts: &'a [String],
    root: &str,
    locals: &[(String, String)],
) -> Option<(Vec<super::formula::JsonKey>, bool, String, &'a [String])> {
    if stmts.last().map(String::as_str) != Some("end") || stmts.len() < 3 {
        return None;
    }
    let ident = |v: &str| !v.is_empty() && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    for i in 1..stmts.len() - 1 {
        let stmt = stmts[i].trim();
        let open = [
            ".filter_map do |",
            "&.filter_map do |",
            ".flat_map do |",
            "&.flat_map do |",
            ".map do |",
            "&.map do |",
        ]
        .iter()
        .find_map(|pat| stmt.find(pat).map(|idx| (idx, idx + pat.len())));
        let Some((name_end, var_at)) = open else {
            continue;
        };
        let name = stmt[..name_end].trim();
        let var = stmt[var_at..].strip_suffix('|')?.trim();
        if !ident(name) || !ident(var) || name == root {
            continue;
        }
        let mut path = None;
        let mut ok = true;
        for pre in &stmts[..i] {
            let pre = pre.trim();
            if let Some((lhs, rhs)) = jt_assignment(pre)
                && lhs.trim() == name
                && path.is_none()
            {
                match jt_dig(rhs.trim(), root, locals) {
                    Some((p, tail)) if tail.trim().is_empty() && !p.is_empty() => path = Some(p),
                    _ => {
                        ok = false;
                        break;
                    }
                }
            } else if pre == format!("next if {name}.blank?") && path.is_some() {
                // Subsumed by the map over the dig.
            } else {
                ok = false;
                break;
            }
        }
        if ok && let Some(p) = path {
            let body = &stmts[i + 1..stmts.len() - 1];
            if !body.is_empty() {
                return Some((p, true, var.to_string(), body));
            }
        }
        return None;
    }
    None
}

/// A `<dig>&.split(line)&.map { |v| v.split(col) }`
/// source: (dig path, line separator, field
/// separator).
fn jt_split_rows_src(
    rhs: &str,
    base: &str,
    locals: &[(String, String)],
) -> Option<(Vec<super::formula::JsonKey>, String, String)> {
    let (path, tail) = jt_dig(rhs.trim(), base, locals)?;
    let rest = tail
        .trim()
        .strip_prefix("&.split(")
        .or_else(|| tail.trim().strip_prefix(".split("))?;
    let line_arg = rest.trim_start();
    let (line_sep, t) = quoted_token(line_arg)?;
    let line_sep = if line_arg.starts_with('"') {
        ruby_unescape_dq(&line_sep)
    } else {
        line_sep
    };
    let rest = t
        .trim()
        .strip_prefix(')')?
        .trim()
        .strip_prefix("&.map")
        .or_else(|| t.trim().strip_prefix(')')?.trim().strip_prefix(".map"))?;
    let (block, rest) = take_bracketed(rest.trim(), '{', '}')?;
    if !rest.trim().is_empty() {
        return None;
    }
    let inner = block.strip_prefix('{')?.strip_suffix('}')?.trim();
    let inner = inner.strip_prefix('|')?;
    let (var, body) = inner.split_once('|')?;
    let rest = body
        .trim()
        .strip_prefix(var.trim())?
        .trim()
        .strip_prefix("&.split(")
        .or_else(|| {
            body.trim()
                .strip_prefix(var.trim())?
                .trim()
                .strip_prefix(".split(")
        })?;
    let field_arg = rest.trim_start();
    let (field_sep, t) = quoted_token(field_arg)?;
    let field_sep = if field_arg.starts_with('"') {
        ruby_unescape_dq(&field_sep)
    } else {
        field_sep
    };
    if t.trim() != ")" || line_sep.is_empty() || field_sep.is_empty() {
        return None;
    }
    Some((path, line_sep, field_sep))
}

/// A `.find { |v| v.first.end_with?(suffix) }`
/// over a split-rows source local.
fn jt_split_rows_find(
    rhs: &str,
    sources: &[(String, Vec<super::formula::JsonKey>, String, String)],
) -> Option<super::formula::JsonLocal> {
    for (name, path, line_sep, field_sep) in sources.iter().rev() {
        let mut rest = rhs.trim();
        if let Some(r) = rest.strip_suffix(&format!(" if {name}")) {
            rest = r.trim();
        }
        let Some(r) = rest.strip_prefix(name.as_str()) else {
            continue;
        };
        let Some(r) = r
            .trim()
            .strip_prefix("&.find")
            .or_else(|| r.trim().strip_prefix(".find"))
        else {
            continue;
        };
        let (block, tail) = take_bracketed(r.trim(), '{', '}')?;
        if !tail.trim().is_empty() {
            return None;
        }
        let inner = block.strip_prefix('{')?.strip_suffix('}')?.trim();
        let inner = inner.strip_prefix('|')?;
        let (var, body) = inner.split_once('|')?;
        let var = var.trim();
        let rest = body.trim().strip_prefix(var)?.trim();
        let rest = rest
            .strip_prefix(".first")
            .or_else(|| rest.strip_prefix("[0]"))?
            .trim()
            .strip_prefix("&.end_with?(")
            .or_else(|| {
                rest.strip_prefix(".first")
                    .or_else(|| rest.strip_prefix("[0]"))?
                    .trim()
                    .strip_prefix(".end_with?(")
            })?;
        let (suffix, t) = quoted_token(rest.trim_start())?;
        if t.trim() != ")" {
            return None;
        }
        return Some(super::formula::JsonLocal::SplitRowsFind {
            path: path.clone(),
            line_sep: line_sep.clone(),
            field_sep: field_sep.clone(),
            first_suffix: suffix,
        });
    }
    None
}

/// Reduce a statement-form `:json` block (the
/// cask tap's locals/match/template bodies) to a
/// `JsonTemplate`. `lines` have cask locals
/// already substituted.
fn parse_json_template_shape(
    params: &str,
    lines: &[String],
    cask_locals: &[(String, String)],
) -> Option<super::formula::JsonBlock> {
    use super::formula::{
        JsonBlock, JsonCond, JsonGuard, JsonLocal, JsonPart, JsonRender, JsonShape, JsonTemplate,
        JsonValue,
    };
    let (root, has_regex) = block_params(params)?;
    let raw_stmts: Vec<String> = lines
        .iter()
        .map(|l| strip_trailing_comment(l.trim()).trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    // Line-continued chains join onto the previous
    // statement (`json.dig(…)` then `&.tr(…)`).
    let mut stmts: Vec<String> = Vec::new();
    for stmt in raw_stmts {
        if (stmt.starts_with('.') || stmt.starts_with("&."))
            && let Some(prev) = stmts.last_mut()
        {
            prev.push(' ');
            prev.push_str(&stmt);
        } else {
            stmts.push(stmt);
        }
    }
    // `<cask-local>.to_s` resolves on this host like
    // its `#{...}` form (burp's `arch.to_s`).
    for stmt in &mut stmts {
        *stmt = jt_substitute_to_s(stmt, cask_locals);
    }
    // A one-line brace map (`….filter_map { |v| … }`)
    // rewrites to the do form the wrapper reads.
    if stmts.len() == 1
        && let Some(rewritten) = jt_brace_map_rewrite(&stmts[0])
    {
        stmts = rewritten;
    }
    // Map wrapper?
    let (path, map, values, item_var, body): (
        Vec<super::formula::JsonKey>,
        bool,
        bool,
        String,
        &[String],
    ) = {
        let first = stmts.first()?;
        if let Some((p, tail)) = jt_dig(first, &root, cask_locals) {
            let tail = tail.trim();
            let (values, tail) = match tail
                .strip_prefix("&.values")
                .or_else(|| tail.strip_prefix(".values"))
            {
                Some(r) => (true, r.trim()),
                None => (false, tail),
            };
            let inner = [
                ".map do |",
                "&.map do |",
                ".filter_map do |",
                "&.filter_map do |",
                ".flat_map do |",
                "&.flat_map do |",
            ]
            .iter()
            .find_map(|pat| tail.strip_prefix(pat));
            if let Some(inner) = inner {
                let var = inner.strip_suffix('|')?.trim();
                if var.is_empty()
                    || !var.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    || stmts.last().map(String::as_str) != Some("end")
                {
                    return None;
                }
                (p, true, values, var.to_string(), &stmts[1..stmts.len() - 1])
            } else {
                (Vec::new(), false, false, root.clone(), &stmts[..])
            }
        } else {
            (Vec::new(), false, false, root.clone(), &stmts[..])
        }
    };
    // A map over a dug local (`xs = <dig>` then
    // `xs.filter_map do |item| … end`): the dig
    // becomes the template path; a `next if
    // xs.blank?` before it is subsumed (a missing
    // or empty array yields nothing either way).
    let (path, map, values, item_var, body) =
        if !map && let Some(found) = jt_local_map(&stmts, &root, cask_locals) {
            let (p, m, v, b) = found;
            (p, m, false, v, b)
        } else {
            (path, map, values, item_var, body)
        };
    // Continuation lines (trailing `&&`/`||`/`if`,
    // unclosed do/brace/paren spans) join.
    let body_owned = jt_join_stmts(body);
    let body: &[String] = &body_owned;
    if body.is_empty() {
        return None;
    }
    let mut locals: Vec<JsonLocal> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let mut require: Vec<usize> = Vec::new();
    let mut match_src: Option<Vec<super::formula::JsonKey>> = None;
    let mut match_var = String::new();
    let mut match_guarded = false;
    let mut local_match_guard: Option<usize> = None;
    let mut guards: Vec<JsonGuard> = Vec::new();
    let mut finds: Vec<super::formula::JsonFind> = Vec::new();
    let mut split_rows_sources: Vec<(String, Vec<super::formula::JsonKey>, String, String)> =
        Vec::new();
    let mut find_names: Vec<String> = Vec::new();
    let mut require_find: Vec<(usize, bool)> = Vec::new();
    let mut item_var = item_var;
    let local_idx = |names: &[String], n: &str| names.iter().position(|x| x == n);
    // An `if <conds> … end` wrapper over the whole
    // map body becomes leading guards.
    let mut body = body;
    if map
        && body.len() >= 3
        && body[0].trim().starts_with("if ")
        && body[body.len() - 1].trim() == "end"
    {
        let cond = body[0].trim()["if ".len()..].trim().to_string();
        let mut pending = Vec::new();
        for atom in jt_split_and(&cond) {
            match parse_json_cond(&item_var, atom.trim()) {
                Some((c, rest)) if rest.trim().is_empty() => pending.push(JsonGuard {
                    unless: true,
                    cond: c,
                }),
                _ => return None,
            }
        }
        guards.extend(pending);
        body = &body[1..body.len() - 1];
        if body.is_empty() {
            return None;
        }
    }
    for stmt in &body[..body.len() - 1] {
        let stmt = stmt.as_str();
        if let Some(cond) = stmt.strip_prefix("next if ") {
            if cond == format!("{match_var}.blank?") && !match_var.is_empty() {
                match_guarded = true;
            } else if cond.contains(".blank?") {
                for part in cond.split(" || ") {
                    let name = part.trim().strip_suffix(".blank?")?;
                    if name == match_var && !match_var.is_empty() {
                        match_guarded = true;
                    } else if let Some(pos) = find_names.iter().position(|n| n == name) {
                        require_find.push((pos, true));
                    } else if name == "version" && local_idx(&names, name).is_none() {
                        // The package's own version
                        // (no local of that name):
                        // always present.
                    } else if let Some((dig_path, tail)) = jt_dig(name, &item_var, cask_locals)
                        && tail.trim().is_empty()
                        && dig_path.len() > 1
                    {
                        guards.push(JsonGuard {
                            unless: false,
                            cond: JsonCond::DigBlank(dig_path),
                        });
                    } else if let Some((field, tail)) = jt_field_tail(name, &item_var)
                        && tail.is_empty()
                    {
                        guards.push(JsonGuard {
                            unless: false,
                            cond: JsonCond::FieldBlank(field),
                        });
                    } else {
                        require.push(local_idx(&names, name)?);
                    }
                }
            } else if let Some((field, tail)) = jt_field_tail(cond, &item_var)
                && (tail.starts_with(".match?(") || tail.starts_with("&.match?("))
            {
                let args = tail.split_once(".match?(").map(|(_, r)| r)?;
                let (pat, flags, t) = regex_literal(args.trim())?;
                if t.trim() != ")" {
                    return None;
                }
                guards.push(JsonGuard {
                    unless: false,
                    cond: JsonCond::MatchRe(field, pat, flags),
                });
            } else if cond.split(" || ").all(|p| p.trim().starts_with('!')) {
                // `next if !item["a"] || !item["b"]`:
                // keep only items where all are truthy.
                for part in cond.split(" || ") {
                    let part = part.trim().strip_prefix('!')?.trim();
                    let (field, tail) = jt_field_tail(part, &item_var)?;
                    if !tail.is_empty() {
                        return None;
                    }
                    guards.push(JsonGuard {
                        unless: true,
                        cond: JsonCond::Truthy(field),
                    });
                }
            } else if let Some(or_guards) = jt_next_if_or_guards(&item_var, cond) {
                guards.extend(or_guards);
            } else if let Some((field, ne, lit)) = jt_field_cmp(cond, &item_var) {
                guards.push(JsonGuard {
                    unless: false,
                    cond: if ne {
                        JsonCond::Ne(field, lit)
                    } else {
                        JsonCond::Eq(field, lit)
                    },
                });
            } else {
                return None;
            }
        } else if let Some(cond) = stmt.strip_prefix("next unless ") {
            if let Some(inner) = cond.strip_prefix('(').and_then(|c| c.strip_suffix(')'))
                && let Some((mv, rhs)) = jt_assignment(inner)
                && let Some(JtSrc::Match(p)) = jt_local_src(rhs.trim(), &item_var, cask_locals)
            {
                // `next unless (match = <dig>&.match(regex))`
                match_var = mv.trim().to_string();
                match_src = Some(p);
                match_guarded = true;
            } else if cond == match_var && !match_var.is_empty() {
                match_guarded = true;
            } else if let Some((p, tail)) = jt_dig(cond, &item_var, cask_locals)
                && tail.trim().is_empty()
                && p.len() > 1
            {
                guards.push(JsonGuard {
                    unless: true,
                    cond: JsonCond::DigTruthy(p),
                });
            } else if let Some(pos) = find_names.iter().position(|n| n == cond) {
                require_find.push((pos, false));
            } else if let Some(pos) = local_idx(&names, cond) {
                require.push(pos);
            } else if let Some((c, tail)) = parse_json_cond(&item_var, cond)
                && tail.trim().is_empty()
                && !matches!(c, JsonCond::Truthy(_))
            {
                guards.push(JsonGuard {
                    unless: true,
                    cond: c,
                });
            } else if let Some((field, tail)) = jt_field_tail(cond, &item_var) {
                if tail.is_empty() {
                    guards.push(JsonGuard {
                        unless: true,
                        cond: JsonCond::Truthy(field),
                    });
                } else if let Some(rest) = tail.strip_prefix("&.match?(") {
                    let (lit, t) = quoted_token(rest.trim_start())?;
                    if t.trim() != ")" {
                        return None;
                    }
                    guards.push(JsonGuard {
                        unless: true,
                        cond: JsonCond::MatchLit(field, lit),
                    });
                } else {
                    let rest = tail.strip_prefix("&.include?(")?;
                    let (lit, t) = quoted_token(rest.trim_start())?;
                    if t.trim() != ")" {
                        return None;
                    }
                    guards.push(JsonGuard {
                        unless: true,
                        cond: JsonCond::Includes(field, lit),
                    });
                }
            } else if let Some(pos) = cond
                .strip_suffix("&.match(regex)")
                .and_then(|n| local_idx(&names, n.trim()))
            {
                local_match_guard = Some(pos);
            } else {
                let pos = cond
                    .strip_suffix(".match(regex)")
                    .and_then(|n| local_idx(&names, n.trim()))?;
                local_match_guard = Some(pos);
            }
        } else if let Some((name, rhs)) = jt_assignment(stmt) {
            let name = name.trim();
            if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return None;
            }
            if !map
                && locals.is_empty()
                && match_src.is_none()
                && let Some(find) = jt_find_src(rhs.trim(), &item_var, cask_locals)
            {
                finds.push(find);
                find_names.push(name.to_string());
                item_var = name.to_string();
                continue;
            }
            if let Some((path, line_sep, field_sep)) =
                jt_split_rows_src(rhs, &item_var, cask_locals)
            {
                if names.iter().any(|n| n == name)
                    || split_rows_sources.iter().any(|(n, ..)| n == name)
                {
                    return None;
                }
                split_rows_sources.push((name.to_string(), path, line_sep, field_sep));
                continue;
            }
            let src = match jt_split_rows_find(rhs, &split_rows_sources) {
                Some(local) => JtSrc::Local(local),
                None => jt_local_src(rhs, &item_var, cask_locals).or_else(|| {
                    // An object local as the dig base.
                    names.iter().enumerate().find_map(|(i, n)| {
                        let super::formula::JsonLocal::Dig(base) = &locals[i] else {
                            return None;
                        };
                        jt_local_src(rhs.trim(), n, cask_locals)
                            .and_then(|src| jt_prepend(src, base))
                    })
                })?,
            };
            match src {
                JtSrc::Local(local) => {
                    names.push(name.to_string());
                    locals.push(local);
                }
                JtSrc::Match(p) => {
                    match_var = name.to_string();
                    match_src = Some(p);
                }
            }
        } else {
            return None;
        }
    }
    // Final expression, with an optional trailing
    // `if <dig>&.match(regex)`.
    let mut final_stmt = body[body.len() - 1].as_str();
    // Postfix render guard: `<expr> if <cond>`
    // (a nil yield for items failing the condition).
    if let Some(idx) = json_outside_quotes(final_stmt, " if ") {
        let (expr, cond) = final_stmt.split_at(idx);
        let cond = cond[4..].trim();
        let mut ok = true;
        let mut pending: Vec<JsonGuard> = Vec::new();
        for atom in jt_split_and(cond) {
            let atom = atom.trim();
            if let Some((c, rest)) = parse_json_cond(&item_var, atom)
                && rest.trim().is_empty()
            {
                pending.push(JsonGuard {
                    unless: true,
                    cond: c,
                });
            } else {
                ok = false;
                break;
            }
        }
        if ok {
            guards.extend(pending);
            final_stmt = expr.trim();
        }
    }
    // `<dig>&.match(regex) { |m| <expr> }`: the block
    // value when matched, nil otherwise.
    if let Some((p, tail)) = jt_dig(final_stmt, &item_var, cask_locals)
        && let Some(inner) = tail
            .trim()
            .strip_prefix("&.match(regex) { |")
            .or_else(|| tail.trim().strip_prefix(".match(regex) { |"))
        && let Some(inner) = inner.strip_suffix('}')
        && let Some((var, expr)) = inner.trim().split_once('|')
        && !var.trim().is_empty()
    {
        match_src = Some(p);
        match_guarded = true;
        match_var = var.trim().to_string();
        final_stmt = expr.trim();
    }
    if !final_stmt.starts_with('"')
        && let Some((expr, cond)) = final_stmt.rsplit_once(" if ")
        && let Some(src) = jt_local_src(cond, &item_var, cask_locals)
        && let JtSrc::Match(p) = src
    {
        match_src = Some(p);
        match_guarded = true;
        final_stmt = expr.trim();
        if match_var.is_empty() {
            match_var = "match".to_string();
        }
    }
    let render = jt_render(
        final_stmt,
        &item_var,
        &root,
        &match_var,
        &mut locals,
        &mut names,
        cask_locals,
    )?;
    fn render_uses_cap(render: &JsonRender) -> bool {
        match render {
            JsonRender::Template(parts) => parts.iter().any(|p| matches!(p, JsonPart::Cap(_))),
            JsonRender::Value(JsonValue::Cap(_), _) => true,
            JsonRender::CapFork { .. } | JsonRender::CapEqFork { .. } => true,
            JsonRender::LocalEqFork { then, els, .. } => {
                render_uses_cap(then) || render_uses_cap(els)
            }
            _ => false,
        }
    }
    let uses_cap = render_uses_cap(&render);
    if uses_cap && (match_src.is_none() || !match_guarded) {
        return None;
    }
    let needs_regex = match_src.is_some()
        || local_match_guard.is_some()
        || locals.iter().any(|l| matches!(l, JsonLocal::DigCapture(_)));
    if needs_regex && !has_regex {
        // The body reads `regex` without declaring
        // it: the spec regex feeds it (checked at
        // plan like other shapes).
    }
    Some(JsonBlock {
        has_regex: has_regex || needs_regex,
        shape: JsonShape::Template(Box::new(JsonTemplate {
            path,
            map,
            values,
            locals,
            require,
            match_src,
            local_match_guard,
            guards,
            finds,
            require_find,
            render,
        })),
    })
}

/// A `<base><dig>&.find { |v| <cond> }` source.
fn jt_find_src(
    rhs: &str,
    base: &str,
    locals: &[(String, String)],
) -> Option<super::formula::JsonFind> {
    use super::formula::{JsonFind, JsonFindCond};
    let find_cond = |var: &str, cond_s: &str| -> Option<JsonFindCond> {
        if let Some((field, ne, lit)) = jt_field_cmp(cond_s, var) {
            if ne {
                return None;
            }
            return Some(JsonFindCond::FieldEq(field, lit));
        }
        let (dpath, dtail) = jt_dig(cond_s, var, locals)?;
        let rest = dtail.trim().strip_prefix("&.start_with?(")?;
        let (lit, t) = quoted_token(rest.trim_start())?;
        if t.trim() != ")" {
            return None;
        }
        Some(JsonFindCond::DigStartsWith(dpath, lit))
    };
    let ident = |v: &str| !v.is_empty() && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    let (path, tail) = jt_dig(rhs, base, locals)?;
    let tail = tail.trim();
    // `.select { |v| <cond> }.max_by { |v|
    // Version.new(v["f"]) }`
    if let Some(block) = tail
        .strip_prefix("&.select")
        .or_else(|| tail.strip_prefix(".select"))
    {
        let (block, rest) = take_bracketed(block.trim(), '{', '}')?;
        let inner = block.strip_prefix('{')?.strip_suffix('}')?.trim();
        let inner = inner.strip_prefix('|')?;
        let (var, cond_s) = inner.split_once('|')?;
        let var = var.trim();
        if !ident(var) {
            return None;
        }
        let cond = find_cond(var, cond_s.trim())?;
        let rest = rest
            .trim()
            .strip_prefix("&.max_by")
            .or_else(|| rest.trim().strip_prefix(".max_by"))?;
        let (block, rest) = take_bracketed(rest.trim(), '{', '}')?;
        if !rest.trim().is_empty() {
            return None;
        }
        let inner = block.strip_prefix('{')?.strip_suffix('}')?.trim();
        let inner = inner.strip_prefix('|')?;
        let (vvar, body) = inner.split_once('|')?;
        let body = body.trim().strip_prefix("Version.new(")?;
        let (field, tail) = jt_field_tail(body, vvar.trim())?;
        if tail.trim() != ")" {
            return None;
        }
        return Some(JsonFind {
            path,
            cond,
            max_by: Some(field),
        });
    }
    let inner = tail
        .strip_prefix("&.find { |")
        .or_else(|| tail.strip_prefix(".find { |"))?;
    let inner = inner.strip_suffix('}')?.trim();
    let (var, cond_s) = inner.split_once('|')?;
    let var = var.trim();
    if !ident(var) {
        return None;
    }
    let cond = find_cond(var, cond_s.trim())?;
    Some(JsonFind {
        path,
        cond,
        max_by: None,
    })
}

/// `<var>["field"]` plus tail, for guard parsing.
fn jt_field_tail<'a>(s: &'a str, var: &str) -> Option<(String, &'a str)> {
    let rest = s.trim().strip_prefix(var)?;
    let (key, tail) = json_bracket_key(rest)?;
    Some((key, tail.trim()))
}

/// A comparison literal: quoted string, integer,
/// or boolean, plus the remainder.
fn jt_lit(s: &str) -> Option<(super::formula::JsonLit, &str)> {
    use super::formula::JsonLit;
    let s = s.trim();
    if let Some(r) = s.strip_prefix("true") {
        Some((JsonLit::Bool(true), r))
    } else if let Some(r) = s.strip_prefix("false") {
        Some((JsonLit::Bool(false), r))
    } else if s.starts_with(['"', '\'']) {
        let (lit, r) = quoted_token(s)?;
        Some((JsonLit::Str(lit), r))
    } else {
        let end = s
            .find(|c: char| !(c.is_ascii_digit() || c == '-'))
            .unwrap_or(s.len());
        let n: i64 = s[..end].parse().ok()?;
        Some((JsonLit::Int(n), &s[end..]))
    }
}

/// `<var>["f"] == lit` / `!=`: (field, is_ne, lit).
fn jt_field_cmp(s: &str, var: &str) -> Option<(String, bool, super::formula::JsonLit)> {
    let (field, tail) = jt_field_tail(s, var)?;
    let (ne, rest) = if let Some(r) = tail.strip_prefix("!=") {
        (true, r)
    } else {
        let r = tail.strip_prefix("==")?;
        (false, r)
    };
    let (lit, t) = jt_lit(rest)?;
    if !t.trim().is_empty() {
        return None;
    }
    Some((field, ne, lit))
}

/// First index of `needle` outside double quotes.
fn json_outside_quotes(s: &str, needle: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut quoted = false;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if quoted => i += 1,
            b'"' => quoted = !quoted,
            _ if !quoted && s[i..].starts_with(needle) => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

/// The final expression of a json template body.
fn jt_render(
    stmt: &str,
    item_var: &str,
    root: &str,
    match_var: &str,
    locals: &mut Vec<super::formula::JsonLocal>,
    names: &mut Vec<String>,
    cask_locals: &[(String, String)],
) -> Option<super::formula::JsonRender> {
    use super::formula::{JsonPart, JsonRender, JsonValue};
    let stmt = stmt.trim();
    // Local-equality fork: `(a == b) ? A : B` over
    // two body locals.
    if !stmt.starts_with('"')
        && let Some(q) = json_outside_quotes(stmt, " ? ")
    {
        let (cond, rest) = stmt.split_at(q);
        if let Some(inner) = cond
            .trim()
            .strip_prefix('(')
            .and_then(|c| c.strip_suffix(')'))
            && let Some((a_s, b_s)) = inner.split_once(" == ")
            && let Some(a) = names.iter().position(|n| n == a_s.trim())
            && let Some(b) = names.iter().position(|n| n == b_s.trim())
        {
            let rest = &rest[3..];
            let c = json_outside_quotes(rest, " : ")?;
            return Some(JsonRender::LocalEqFork {
                a,
                b,
                then: Box::new(jt_render(
                    &rest[..c],
                    item_var,
                    root,
                    match_var,
                    locals,
                    names,
                    cask_locals,
                )?),
                els: Box::new(jt_render(
                    &rest[c + 3..],
                    item_var,
                    root,
                    match_var,
                    locals,
                    names,
                    cask_locals,
                )?),
            });
        }
    }
    // Capture forks: `match[N][.present?] ? A : B`,
    // `(match[A] == match[B]) ? A : B`.
    if !match_var.is_empty()
        && !stmt.starts_with('"')
        && let Some(q) = json_outside_quotes(stmt, " ? ")
    {
        let (cond, rest) = stmt.split_at(q);
        let rest = &rest[3..];
        let c = json_outside_quotes(rest, " : ")?;
        let (then_s, els_s) = (&rest[..c], &rest[c + 3..]);
        let cap_of = |s: &str| -> Option<usize> {
            let r = s.trim().strip_prefix(&format!("{match_var}["))?;
            let close = r.find(']')?;
            if !r[close + 1..].trim().is_empty() {
                return None;
            }
            r[..close].parse().ok()
        };
        let cond = cond.trim();
        let then = jt_render(
            then_s,
            item_var,
            root,
            match_var,
            locals,
            names,
            cask_locals,
        )?;
        let els = jt_render(els_s, item_var, root, match_var, locals, names, cask_locals)?;
        if let Some(inner) = cond.strip_prefix('(').and_then(|c| c.strip_suffix(')'))
            && let Some((a_s, b_s)) = inner.split_once(" == ")
        {
            return Some(JsonRender::CapEqFork {
                a: cap_of(a_s)?,
                b: cap_of(b_s)?,
                then: Box::new(then),
                els: Box::new(els),
            });
        }
        let present = cond.ends_with(".present?");
        let base = cond.strip_suffix(".present?").unwrap_or(cond);
        return Some(JsonRender::CapFork {
            cap: cap_of(base)?,
            present,
            then: Box::new(then),
            els: Box::new(els),
        });
    }
    if stmt.starts_with('"') {
        let (body_str, tail) = quoted_token(stmt)?;
        let gsub = if tail.trim().is_empty() {
            None
        } else if let Some(args) = tail
            .trim()
            .strip_prefix("&.gsub(")
            .or_else(|| tail.trim().strip_prefix(".gsub("))
        {
            let (pat, flags, t) = regex_literal(args.trim_start())?;
            let t = t.trim().strip_prefix(',')?.trim();
            let (repl, t) = quoted_token(t)?;
            if t.trim() != ")" || repl.contains('\\') || pat.contains("#{") {
                return None;
            }
            Some((pat, flags, repl))
        } else {
            return None;
        };
        let mut parts = Vec::new();
        let mut rest = body_str.as_str();
        while let Some(i) = rest.find("#{") {
            if i > 0 {
                parts.push(JsonPart::Lit(rest[..i].to_string()));
            }
            let j = rest[i + 2..].find('}')?;
            let token = rest[i + 2..i + 2 + j].trim();
            let part = if let Some(md5) = token
                .strip_prefix("Digest::MD5.hexdigest(")
                .and_then(|inner| inner.strip_suffix(')'))
            {
                let (pos, field) = names.iter().enumerate().find_map(|(i, n)| {
                    if !matches!(locals[i], super::formula::JsonLocal::SplitRowsFind { .. }) {
                        return None;
                    }
                    let r = md5.trim().strip_prefix(&format!("{n}["))?;
                    let field: usize = r.strip_suffix(']')?.parse().ok()?;
                    Some((i, field))
                })?;
                JsonPart::LocalMd5Field(pos, field)
            } else if let Some(pos) = names.iter().position(|n| n == token) {
                JsonPart::Local(pos)
            } else if let Some(pos) = names.iter().enumerate().find_map(|(i, n)| {
                // An object local's field: `v["major"]`.
                let super::formula::JsonLocal::Dig(base) = &locals[i] else {
                    return None;
                };
                let (sub, tail) = jt_dig(token, n, cask_locals)?;
                if !tail.trim().is_empty() || sub.is_empty() {
                    return None;
                }
                let full: Vec<super::formula::JsonKey> = base.iter().cloned().chain(sub).collect();
                Some((i, full))
            }) {
                let (_, full) = pos;
                locals.push(super::formula::JsonLocal::Dig(full));
                names.push(String::new());
                JsonPart::Local(locals.len() - 1)
            } else if let Some(r) = token.strip_prefix(&format!("{match_var}[")) {
                let close = r.find(']')?;
                let n: usize = r[..close].parse().ok()?;
                let tail = r[close + 1..].trim();
                if tail.is_empty() {
                    JsonPart::Cap(n)
                } else if let Some(args) = tail
                    .strip_prefix("&.tr(")
                    .or_else(|| tail.strip_prefix(".tr("))
                {
                    let (from, t) = quoted_token(args.trim_start())?;
                    let t = t.trim().strip_prefix(',')?.trim();
                    let (to, t) = quoted_token(t)?;
                    if t.trim() != ")"
                        || from.contains('\\')
                        || to.contains('\\')
                        || from.starts_with('^')
                    {
                        return None;
                    }
                    JsonPart::CapTr(n, from, to)
                } else {
                    return None;
                }
            } else if root != item_var
                && let Some((p, tail)) = jt_dig(token, root, cask_locals)
                && tail.trim().is_empty()
                && !p.is_empty()
            {
                // A root dig inside a map template
                // (`#{json["shortrev"]}`).
                JsonPart::RootDig(p)
            } else if let Some((p, tail)) = jt_dig(token, item_var, cask_locals)
                && tail.trim().is_empty()
                && p.len() > 1
            {
                // A multi-segment dig localizes.
                locals.push(super::formula::JsonLocal::Dig(p));
                names.push(String::new());
                JsonPart::Local(locals.len() - 1)
            } else if let Some((field, tail)) = jt_field_tail(token, item_var) {
                if !tail.is_empty() {
                    return None;
                }
                JsonPart::Field(field)
            } else {
                return None;
            };
            parts.push(part);
            rest = &rest[i + 2 + j + 1..];
        }
        if !rest.is_empty() {
            parts.push(JsonPart::Lit(rest.to_string()));
        }
        let render = JsonRender::Template(parts);
        return Some(match gsub {
            Some((pat, flags, repl)) => JsonRender::GsubRegex {
                inner: Box::new(render),
                pat,
                flags,
                repl,
            },
            None => render,
        });
    }
    // `match[N]<tail>`
    if !match_var.is_empty()
        && let Some(r) = stmt.strip_prefix(&format!("{match_var}["))
    {
        let close = r.find(']')?;
        let n: usize = r[..close].parse().ok()?;
        return Some(JsonRender::Value(
            JsonValue::Cap(n),
            jt_tail(&r[close + 1..])?,
        ));
    }
    // `<local><tail>` / `<local>[regex, 1]<tail>`
    for (pos, name) in names.iter().enumerate() {
        if let Some(tail) = stmt.strip_prefix(name.as_str()) {
            if tail.is_empty() || tail.starts_with(['.', '&']) {
                return Some(JsonRender::Value(JsonValue::Local(pos), jt_tail(tail)?));
            }
            if let Some(t) = tail
                .strip_prefix("[regex, 1]")
                .or_else(|| tail.strip_prefix("&.[](regex, 1)"))
            {
                return Some(JsonRender::Value(JsonValue::LocalCapture(pos), jt_tail(t)?));
            }
        }
    }
    // A bare dig (optionally captured/tailed).
    if let Some((path, tail)) = jt_dig(stmt, item_var, cask_locals) {
        let tail = tail.trim();
        if tail == "&.[](regex, 1)" || tail == ".[](regex, 1)" || tail == "[regex, 1]" {
            locals.push(super::formula::JsonLocal::DigCapture(path));
            names.push(String::new());
            return Some(JsonRender::Value(
                JsonValue::Local(locals.len() - 1),
                super::formula::JsonTail::None,
            ));
        }
        if let Some(t) = tail
            .strip_prefix("&.[](regex, 1)")
            .or_else(|| tail.strip_prefix(".[](regex, 1)"))
            .or_else(|| tail.strip_prefix("[regex, 1]"))
            && let Some(jt) = jt_tail(t)
        {
            locals.push(super::formula::JsonLocal::DigCapture(path));
            names.push(String::new());
            return Some(JsonRender::Value(JsonValue::Local(locals.len() - 1), jt));
        }
        if let Some(t) = jt_tail(tail) {
            locals.push(super::formula::JsonLocal::Dig(path));
            names.push(String::new());
            return Some(JsonRender::Value(JsonValue::Local(locals.len() - 1), t));
        }
    }
    match jt_local_src(stmt, item_var, cask_locals)? {
        JtSrc::Local(local) => {
            locals.push(local);
            names.push(String::new());
            Some(JsonRender::Value(
                JsonValue::Local(locals.len() - 1),
                super::formula::JsonTail::None,
            ))
        }
        JtSrc::Match(_) => None,
    }
}

/// A single-quoted Ruby string token: only `\\`
/// and `\'` escape inside; the value keeps other
/// backslashes verbatim (regex templates need them).
fn single_quoted_token(s: &str) -> Option<(String, &str)> {
    let rest = s.strip_prefix('\'')?;
    let end = rest.find('\'')?;
    Some((rest[..end].to_string(), &rest[end + 1..]))
}

/// A replacement template for a backreference `sub`:
/// single- or double-quoted; backslashes only as
/// `\0`..`\9` or `\\`.
fn sub_template(tail: &str) -> Option<(String, &str)> {
    let (tmpl, rest) = if tail.starts_with('\'') {
        single_quoted_token(tail)?
    } else {
        quoted_token(tail)?
    };
    let bytes = tmpl.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            match bytes.get(i + 1) {
                Some(b'0'..=b'9' | b'\\') => i += 1,
                _ => return None,
            }
        }
        i += 1;
    }
    Some((tmpl, rest))
}

/// The `(pat, flags, template)` of a `.sub(/pat/flags, '<tmpl>')`
/// call at the start of `after` (after `.sub(`).
fn sub_backref_args(after: &str) -> Option<(String, String, String)> {
    let args = after
        .strip_prefix(".sub(")
        .or_else(|| after.strip_prefix("&.sub("))?;
    let (pat, flags, tail) = regex_literal(args)?;
    let tail = tail.trim().strip_prefix(',')?.trim();
    let (tmpl, tail) = sub_template(tail)?;
    if tail.trim() != ")" {
        return None;
    }
    Some((pat, flags, tmpl))
}

/// Block statements: lines trimmed, comments
/// stripped, continuations joined (a line starting
/// with `.`, or while parens are unbalanced outside
/// double quotes).
fn block_statements(lines: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in lines {
        let line = strip_trailing_comment(raw.trim()).trim();
        if line.is_empty() {
            continue;
        }
        let depth = |s: &str| -> i32 {
            let (mut d, mut quoted) = (0i32, false);
            for c in s.chars() {
                match c {
                    '"' => quoted = !quoted,
                    '(' if !quoted => d += 1,
                    ')' if !quoted => d -= 1,
                    _ => {}
                }
            }
            d
        };
        if (line.starts_with('.') || out.last().is_some_and(|p: &String| depth(p) > 0))
            && let Some(prev) = out.last_mut()
        {
            prev.push(' ');
            prev.push_str(line);
        } else {
            out.push(line.to_string());
        }
    }
    out
}

/// The `(regex_source, tail)` of a `page.scan(…)`
/// argument: the block's `regex` or a literal.
fn scan_regex_arg(cur: &str) -> Option<(Option<(String, String)>, &str)> {
    if let Some(tail) = cur.strip_prefix("regex") {
        return Some((None, tail));
    }
    let (pat, flags, tail) = regex_literal(cur)?;
    Some((Some((pat, flags)), tail))
}

/// Reduce the two-stage directory-walk page blocks
/// (gnu-getopt, texlive, singular) to data. See
/// `DirWalk` for the shape; anything off-template
/// stays block-only.
/// qt-creator's lazy walk: try each scanned
/// directory (in scan order) until one's own scan
/// is non-empty, the directory URL formed by
/// `url.sub(needle, template)`.
fn parse_dirwalk_lazy_shape(root: &str, lines: &[String]) -> Option<super::formula::PageShape> {
    use super::formula::{DirBlank, DirWalk, PageShape};
    let stmts = block_statements(lines);
    if stmts.len() != 7 {
        return None;
    }
    let mvar = stmts[0]
        .strip_prefix(&format!("{root}.scan(regex).lazy.map do |"))?
        .strip_suffix('|')?
        .trim()
        .to_string();
    let (pvar, rhs) = stmts[1].split_once(" = ")?;
    let pvar = pvar.trim();
    let arg = rhs
        .trim()
        .strip_prefix("Homebrew::Livecheck::Strategy.page_content(")?
        .strip_suffix(')')?
        .trim();
    let arg = arg.strip_prefix("url.sub(")?.strip_suffix(')')?;
    let (needle, rest) = quoted_token(arg.trim_start())?;
    let (template, tail) = quoted_token(rest.trim().strip_prefix(',')?.trim())?;
    if !tail.trim().is_empty() {
        return None;
    }
    let slot = format!("#{{{mvar}[0]}}");
    let (pre, post) = template.split_once(&slot)?;
    if pre.contains("#{") || post.contains("#{") {
        return None;
    }
    if stmts[2] != format!("next if {pvar}[:content].blank?") {
        return None;
    }
    let (vers, rhs) = stmts[3].split_once(" = ")?;
    let vers = vers.trim();
    if rhs.trim() != format!("{pvar}[:content].scan(regex).map(&:first)") {
        return None;
    }
    if stmts[4] != format!("next if {vers}.blank?")
        || stmts[5].trim() != vers
        || stmts[6].trim() != "end.compact_blank.first"
    {
        return None;
    }
    Some(PageShape::DirWalk(Box::new(DirWalk {
        stage1_regex: None,
        stage1_tr: None,
        reject_patch_ge: None,
        last_two: false,
        scan_order: true,
        url_sub: Some((needle, pre.to_string(), post.to_string())),
        dir_pre: String::new(),
        dir_tr: None,
        dir_post: String::new(),
        stage2_regex: None,
        blank: DirBlank::NextCandidate,
        empty_scan_stage1: false,
    })))
}

fn parse_dirwalk_shape(root: &str, lines: &[String]) -> Option<super::formula::PageShape> {
    use super::formula::{DirBlank, DirWalk, PageShape};
    let stmts = block_statements(lines);
    let mut i = 0usize;
    let peek = |i: usize| stmts.get(i).map(String::as_str);
    // <v> = page.scan(<re>)<chain>
    let first = peek(i)?;
    let (v, rest) = first.split_once(" = ")?;
    let v = v.trim().to_string();
    if v.is_empty() || !v.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    let rest = rest.strip_prefix(&format!("{root}.scan("))?;
    let (stage1_regex, tail) = scan_regex_arg(rest)?;
    let mut cur = tail.strip_prefix(')')?.trim_start();
    let (mut stage1_tr, mut reject_patch_ge) = (None, None);
    loop {
        cur = cur.trim_start();
        if let Some(r) = cur.strip_prefix(".flatten") {
            cur = r;
        } else if let Some(r) = cur.strip_prefix(".uniq") {
            cur = r;
        } else if let Some(r) = cur.strip_prefix(".map { |") {
            let (x, r) = r.split_once("| ")?;
            let r = r.strip_prefix(&format!("Version.new({x}"))?;
            let r = if let Some(r2) = r.strip_prefix(".tr(") {
                let (from, t) = quoted_token(r2)?;
                let t = t.trim().strip_prefix(',')?.trim();
                let (to, t) = quoted_token(t)?;
                if from.contains('\\') || to.contains('\\') {
                    return None;
                }
                stage1_tr = Some((from, to));
                t.trim().strip_prefix(')')?
            } else {
                r
            };
            cur = r.trim().strip_prefix(") }")?;
        } else if let Some(r) = cur.strip_prefix(".reject { |") {
            let (x, r) = r.split_once("| ")?;
            let r = r.strip_prefix(&format!("{x}.patch >= "))?;
            let (n_str, r) = r.split_once('}')?;
            reject_patch_ge = Some(n_str.trim().parse().ok()?);
            cur = r;
        } else if let Some(r) = cur.strip_prefix(".sort_by { |") {
            let (x, r) = r.split_once("| ")?;
            cur = r
                .strip_prefix(&format!("Version.new({x})"))?
                .trim()
                .strip_prefix('}')?;
        } else if let Some(r) = cur.strip_prefix(".sort") {
            cur = r;
        } else {
            break;
        }
    }
    if !cur.trim().is_empty() {
        return None;
    }
    i += 1;
    // Blank guard (both spellings return nothing).
    match peek(i)? {
        s if s == format!("next {v} if {v}.blank?") || s == format!("next if {v}.blank?") => {
            i += 1;
        }
        _ => return None,
    }
    let pc = "Homebrew::Livecheck::Strategy.page_content(";
    // The directory template inside a page_content arg.
    let dir_template = |arg: &str, wname: &str| -> Option<(String, Option<Pair>, String)> {
        let arg = arg.strip_prefix("URI.join(@url, ")?;
        let arg = arg.trim().strip_suffix(',').unwrap_or(arg).trim();
        let arg = arg.strip_suffix(").to_s")?;
        if arg == wname {
            return Some((String::new(), None, String::new()));
        }
        // The whole arg is the string literal; it
        // may carry inner quotes (a `tr` call), so
        // take first-to-last quote, not a token.
        let inner = arg.strip_prefix('"')?.strip_suffix('"')?;
        let plain = format!("#{{{wname}}}");
        if let Some(pos) = inner.find(&plain) {
            return Some((
                inner[..pos].to_string(),
                None,
                inner[pos + plain.len()..].to_string(),
            ));
        }
        let tr_open = format!("#{{{wname}.to_s.tr(");
        let pos = inner.find(&tr_open)?;
        let rest = &inner[pos + tr_open.len()..];
        let (from, t) = quoted_token(rest)?;
        let t = t.trim().strip_prefix(',')?.trim();
        let (to, t) = quoted_token(t)?;
        let t = t.strip_prefix(")}")?;
        Some((inner[..pos].to_string(), Some((from, to)), t.to_string()))
    };
    let mut walk = DirWalk {
        stage1_regex,
        stage1_tr,
        reject_patch_ge,
        last_two: false,
        scan_order: false,
        url_sub: None,
        dir_pre: String::new(),
        dir_tr: None,
        dir_post: String::new(),
        stage2_regex: None,
        blank: DirBlank::Nothing,
        empty_scan_stage1: false,
    };
    // gnu-getopt: last-two loop.
    if peek(i) == Some("dir_versions = []") {
        let open = peek(i + 1)?;
        let w = open
            .strip_prefix(&format!("{v}[-2..].reverse_each do |"))?
            .strip_suffix('|')?
            .to_string();
        let fetch = peek(i + 2)?;
        let arg = fetch
            .strip_prefix(&format!("dir_page = {pc}"))?
            .strip_suffix(')')?;
        let (pre, tr, post) = dir_template(arg.trim(), &w)?;
        if peek(i + 3)? != format!("next {v} if dir_page[:content].blank?")
            || peek(i + 4)? != "dir_versions = dir_page[:content].scan(regex).flatten"
            || peek(i + 5)? != "break unless dir_versions.empty?"
            || peek(i + 6)? != "end"
            || peek(i + 7)? != format!("dir_versions.presence || {v}")
            || peek(i + 8).is_some()
        {
            return None;
        }
        walk.last_two = true;
        walk.dir_pre = pre;
        walk.dir_tr = tr;
        walk.dir_post = post;
        walk.blank = DirBlank::NextCandidate;
        walk.empty_scan_stage1 = true;
        return Some(PageShape::DirWalk(Box::new(walk)));
    }
    // Newest binding: `<w> = <v>.last[.to_s]`.
    let bind = peek(i)?;
    let (w, rhs) = bind.split_once(" = ")?;
    let w = w.trim().to_string();
    if rhs != format!("{v}.last") && rhs != format!("{v}.last.to_s") {
        return None;
    }
    let fetch = peek(i + 1)?;
    let (pvar, arg) = fetch.split_once(" = ")?;
    let pvar = pvar.trim();
    let arg = arg.strip_prefix(pc)?.strip_suffix(')')?;
    let (pre, tr, post) = dir_template(arg.trim(), &w)?;
    walk.dir_pre = pre;
    walk.dir_tr = tr;
    walk.dir_post = post;
    walk.blank = match peek(i + 2)? {
        s if s == format!("next {v} if {pvar}[:content].blank?") => DirBlank::Stage1,
        s if s == format!("next if {pvar}[:content].blank?") => DirBlank::Nothing,
        _ => return None,
    };
    let scan_stmt = peek(i + 3)?;
    // texlive: bare scan result; singular: assigned + `||`.
    if scan_stmt == format!("{pvar}[:content].scan(regex).flatten") && peek(i + 4).is_none() {
        return Some(PageShape::DirWalk(Box::new(walk)));
    }
    let (dvar, rhs) = scan_stmt.split_once(" = ")?;
    let rhs = rhs.strip_prefix(&format!("{pvar}[:content].scan("))?;
    let (stage2, tail) = scan_regex_arg(rhs)?;
    if tail != ").flatten"
        || peek(i + 4)? != format!("{} || {v}", dvar.trim())
        || peek(i + 5).is_some()
    {
        return None;
    }
    walk.stage2_regex = stage2;
    Some(PageShape::DirWalk(Box::new(walk)))
}

/// Reduce a `:page_match`-family strategy block body
/// to a data shape: `page.scan(regex).map { |match|
/// match[0].tr("from", "to") }` (also written with
/// `match.first` / safe navigation). The `tr`
/// arguments are kept verbatim; negation and escapes
/// do not reduce.
/// Two literal `page.match(/…/)` locals, blank
/// guards, and a template over both.
fn parse_page_two_match(root: &str, lines: &[String]) -> Option<super::formula::PageShape> {
    use super::formula::{PageShape, TwoPart};
    let joined = join_block_lines(lines);
    let body = joined.trim();
    // (var, pat, flags, fixed capture for the
    // `page[regex, N]` bracket form)
    let mut matches: Vec<(String, String, String, Option<usize>)> = Vec::new();
    let mut rest = body;
    for i in 0..2 {
        let (var, rhs) = rest.split_once(" = ")?;
        let var = var.trim();
        if var.is_empty() || !var.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
        // Bracket capture: `page[regex, N]` /
        // `page[/lit/flags, N]`.
        if let Some(r) = rhs.trim().strip_prefix(&format!("{root}[")) {
            let (pat, flags, tail) = if let Some(t) = r.trim().strip_prefix("regex") {
                (String::new(), String::new(), t)
            } else {
                let (p, f, t) = regex_literal(r.trim())?;
                (p, f, t)
            };
            let tail = tail.trim().strip_prefix(',')?.trim();
            let close = tail.find(']')?;
            let cap: usize = tail[..close].trim().parse().ok()?;
            rest = tail[close + 1..].trim();
            if let Some(r2) = rest
                .strip_prefix(&format!("next if {var}.blank?"))
                .or_else(|| rest.strip_prefix(&format!("next unless {var}")))
            {
                rest = r2.trim();
            }
            matches.push((var.to_string(), pat, flags, Some(cap)));
            continue;
        }
        let rhs = rhs.trim().strip_prefix(&format!("{root}.match("))?;
        // The first match may be the block regex
        // (empty pattern marks it).
        if i == 0
            && let Some(t) = rhs.trim().strip_prefix("regex)")
        {
            matches.push((var.to_string(), String::new(), String::new(), None));
            rest = t.trim();
            if let Some(r) = rest
                .strip_prefix(&format!("next if {var}.blank?"))
                .or_else(|| rest.strip_prefix(&format!("next unless {var}")))
            {
                rest = r.trim();
            }
            continue;
        }
        let (pat, flags, tail) = regex_literal(rhs.trim())?;
        rest = tail.trim().strip_prefix(')')?.trim();
        // This match's own blank guard (or the
        // combined guard after the second).
        if let Some(r) = rest
            .strip_prefix(&format!("next if {var}.blank?"))
            .or_else(|| rest.strip_prefix(&format!("next unless {var}")))
        {
            rest = r.trim();
        }
        matches.push((var.to_string(), pat, flags, None));
    }
    if let Some(r) = rest.strip_prefix("next if ") {
        // Combined `<a>.blank? || <b>.blank?`
        let quote = r.find('"')?;
        let cond = r[..quote].trim();
        let want = [
            format!("{}.blank? || {}.blank?", matches[0].0, matches[1].0),
            format!("{}.blank? || {}.blank?", matches[1].0, matches[0].0),
        ];
        if !want.iter().any(|w| w == cond) {
            return None;
        }
        rest = &r[quote..];
    }
    let (body_s, tail) = quoted_token(rest.trim())?;
    if !tail.trim().is_empty() {
        return None;
    }
    let mut parts: Vec<TwoPart> = Vec::new();
    let mut s = body_s.as_str();
    while let Some(i) = s.find("#{") {
        if i > 0 {
            parts.push(TwoPart::Lit(s[..i].to_string()));
        }
        let j = s[i + 2..].find('}')?;
        let token = s[i + 2..i + 2 + j].trim();
        let (which, cap) = matches
            .iter()
            .enumerate()
            .find_map(|(w, (name, _, _, fixed))| match fixed {
                Some(cap) if token == name.as_str() => Some((w, *cap)),
                _ => page_cap_index(name, token).map(|c| (w, c)),
            })?;
        parts.push(TwoPart::Cap(which, cap));
        s = &s[i + 2 + j + 1..];
    }
    if !s.is_empty() {
        parts.push(TwoPart::Lit(s.to_string()));
    }
    let [(_, a_pat, a_flags, _), (_, b_pat, b_flags, _)] = matches.try_into().ok()?;
    Some(PageShape::TwoMatch {
        a_pat,
        a_flags,
        b_pat,
        b_flags,
        parts,
    })
}

/// `.map(&:first)[.uniq].min_by/max_by { |v|
/// Version.new(v) }` after a scan: `Some(true)`
/// for min, `Some(false)` for max.
fn page_scan_extreme(cur: &str) -> Option<bool> {
    let t = cur
        .trim()
        .strip_prefix(".map(&:first)")
        .or_else(|| cur.trim().strip_prefix("&.map(&:first)"))?;
    let t = t
        .trim()
        .strip_prefix(".uniq")
        .or_else(|| t.trim().strip_prefix("&.uniq"))
        .unwrap_or(t.trim())
        .trim();
    let (min, r) = if let Some(r) = t
        .strip_prefix(".min_by")
        .or_else(|| t.strip_prefix("&.min_by"))
    {
        (true, r)
    } else if let Some(r) = t
        .strip_prefix(".max_by")
        .or_else(|| t.strip_prefix("&.max_by"))
    {
        (false, r)
    } else {
        return None;
    };
    let r = r.trim().strip_prefix('{')?.trim().strip_prefix('|')?;
    let (var, r) = r.split_once('|')?;
    let var = var.trim();
    if var.is_empty() || !var.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let r = r.trim().strip_suffix('}')?.trim();
    (r == format!("Version.new({var})")).then_some(min)
}

/// `).first[a..b].chars.join("j")` after a quoted
/// split separator: the split-head char window.
fn page_split_head_chars(sep: String, tail: &str) -> Option<super::formula::PageShape> {
    let t = tail.strip_prefix(')')?.trim();
    let t = t
        .strip_prefix(".first")
        .or_else(|| t.strip_prefix("&.first"))?
        .trim();
    let t = t.strip_prefix('[')?;
    let close = t.find(']')?;
    let range = t[..close].trim();
    let (a, b, inclusive) = if let Some((a, b)) = range.split_once("...") {
        (a.trim(), b.trim(), false)
    } else {
        let (a, b) = range.split_once("..")?;
        (a.trim(), b.trim(), true)
    };
    let start: usize = a.parse().ok()?;
    let end: usize = b.parse().ok()?;
    let len = if inclusive {
        end.checked_sub(start)?.checked_add(1)?
    } else {
        end.checked_sub(start)?
    };
    let t = t[close + 1..].trim();
    let t = t
        .strip_prefix(".chars")
        .or_else(|| t.strip_prefix("&.chars"))?
        .trim();
    let t = t
        .strip_prefix(".join(")
        .or_else(|| t.strip_prefix("&.join("))?;
    let (join, t) = quoted_token(t.trim_start())?;
    if t.trim() != ")" {
        return None;
    }
    Some(super::formula::PageShape::SplitHeadChars {
        sep,
        start,
        len,
        join,
    })
}

/// `page.scan(/pat/).filter_map { |row| match =
/// row[0].match(regex); next unless match; <fork> }`
/// (inkscape's per-row matching).
fn parse_page_section_match(root: &str, lines: &[String]) -> Option<super::formula::PageShape> {
    let joined = join_block_lines(lines);
    let cur = joined.trim().strip_prefix(root)?;
    let rest = cur
        .strip_prefix(".scan(")
        .or_else(|| cur.strip_prefix("&.scan("))?;
    let (pat, flags, tail) = regex_literal(rest.trim())?;
    let tail = tail.trim().strip_prefix(')')?.trim();
    let tail = [
        ".filter_map",
        "&.filter_map",
        ".flat_map",
        "&.flat_map",
        ".map",
        "&.map",
    ]
    .iter()
    .find_map(|p| tail.strip_prefix(p))?;
    let (row, inner) = json_map_parts(tail.trim())?;
    let (mv, rhs) = inner.trim().split_once(" = ")?;
    let mv = mv.trim();
    let rhs = rhs.trim();
    let rest = rhs
        .strip_prefix(&format!("{row}[0].match(regex)"))
        .or_else(|| rhs.strip_prefix(&format!("{row}[0]&.match(regex)")))?
        .trim();
    let rest = rest
        .strip_prefix(&format!("next if {mv}.blank?"))
        .or_else(|| rest.strip_prefix(&format!("next unless {mv}")))?
        .trim();
    let (cond, then, els) = page_fork_parts(mv, rest)?;
    Some(super::formula::PageShape::SectionMatch {
        pat,
        flags,
        cond,
        then,
        els,
    })
}

/// The dosbox scan body: split the first capture
/// and render by field count through a persistent
/// output local.
fn page_scan_split_fork_shape(var: &str, inner: &str) -> Option<super::formula::PageShape> {
    use super::formula::{PageShape, PageSplitArm, PageSplitPart};
    let ident = |v: &str| {
        !v.is_empty()
            && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !v.chars().next().is_some_and(|c| c.is_ascii_digit())
    };
    let (split_var, rhs) = inner.trim().split_once(" = ")?;
    let split_var = split_var.trim();
    if !ident(split_var) {
        return None;
    }
    let rest = rhs.trim();
    let rest = [
        format!("{var}[0].split("),
        format!("{var}[0]&.split("),
        format!("{var}.first.split("),
        format!("{var}.first&.split("),
        format!("{var}&.first.split("),
        format!("{var}&.first&.split("),
    ]
    .iter()
    .find_map(|p| rest.strip_prefix(p.as_str()))?;
    let (sep, tail) = quoted_token(rest.trim_start())?;
    if sep.is_empty() || sep == " " || sep.contains('\\') {
        return None;
    }
    let mut rest = tail.trim().strip_prefix(')')?.trim();
    let mut out_var: Option<&str> = None;
    let mut arms: Vec<PageSplitArm> = Vec::new();
    loop {
        let (lhs, rhs) = rest.split_once(" = ")?;
        let lhs = lhs.trim();
        if !ident(lhs) || out_var.is_some_and(|v| v != lhs) {
            return None;
        }
        out_var = Some(lhs);
        let (template, tail) = quoted_token(rhs.trim())?;
        let tail = tail
            .trim()
            .strip_prefix(&format!("if {split_var}.length =="))
            .or_else(|| tail.trim().strip_prefix(&format!("if {split_var}.size ==")))?;
        let digits: String = tail
            .trim()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if digits.is_empty() {
            return None;
        }
        let len: usize = digits.parse().ok()?;
        if arms.iter().any(|a| a.len == len) {
            return None;
        }
        let mut parts = Vec::new();
        let mut trest = template.as_str();
        while let Some(i) = trest.find("#{") {
            if i > 0 {
                parts.push(PageSplitPart::Lit(trest[..i].to_string()));
            }
            let j = trest[i + 2..].find('}')?;
            let token = trest[i + 2..i + 2 + j].trim();
            let field = token
                .strip_prefix(&format!("{split_var}["))
                .and_then(|r| r.strip_suffix(']'))
                .and_then(|n| n.parse::<usize>().ok())?;
            parts.push(PageSplitPart::Field(field));
            trest = &trest[i + 2 + j + 1..];
        }
        if !trest.is_empty() {
            parts.push(PageSplitPart::Lit(trest.to_string()));
        }
        arms.push(PageSplitArm { len, parts });
        rest = tail.trim().strip_prefix(&digits)?.trim();
        if rest == out_var? {
            break;
        }
        if rest.is_empty() {
            return None;
        }
    }
    (arms.len() > 1).then_some(PageShape::ScanSplitFork { sep, arms })
}

fn parse_page_shape(params: &str, lines: &[String]) -> Option<super::formula::PageBlock> {
    use super::formula::{PageBlock, PageShape};
    let (root, has_regex) = block_params(params)?;
    // An undeclared `regex` still resolves to the
    // livecheck's own regex inside the block
    // (sleep-aid's `|page|` scan runs under the
    // oracle); plan requires the spec regex for
    // shapes that read it.
    let _ = has_regex;
    if let Some(shape) = parse_page_two_match(&root, lines) {
        return Some(PageBlock { shape });
    }
    if let Some(shape) = parse_dirwalk_lazy_shape(&root, lines) {
        return Some(PageBlock { shape });
    }
    if let Some(shape) = parse_dirwalk_shape(&root, lines) {
        return Some(PageBlock { shape });
    }
    if let Some(shape) = parse_first_date_shape(&root, lines) {
        return Some(PageBlock { shape });
    }
    if let Some(shape) = parse_page_json_dig_match(&root, lines) {
        return Some(PageBlock { shape });
    }
    if let Some(shape) = parse_page_scanmax_optcap(&root, lines) {
        return Some(PageBlock { shape });
    }
    if let Some(shape) = parse_page_scan_strip_match(&root, lines) {
        return Some(PageBlock { shape });
    }
    if let Some(shape) = parse_page_scan_dotted_max(&root, lines) {
        return Some(PageBlock { shape });
    }
    if let Some(shape) = parse_page_scan_time_max_throttle(&root, lines) {
        return Some(PageBlock { shape });
    }
    if let Some(shape) = parse_page_header_hop(&root, lines) {
        return Some(PageBlock { shape });
    }
    if let Some(shape) = parse_hopscan_shape(&root, lines) {
        return Some(PageBlock { shape });
    }
    if let Some(shape) = parse_page_section_match(&root, lines) {
        return Some(PageBlock { shape });
    }
    let joined = join_block_lines(lines);
    // `match = page.match(regex)` + guarded template
    // or single capture (MatchData indexing).
    if let Some(shape) = parse_page_match_template(&root, joined.trim()) {
        return Some(PageBlock { shape });
    }
    let cur = joined.trim().strip_prefix(root.as_str())?;
    // `page.split(":").first[0..2].chars.join(".")`.
    if let Some(rest) = cur
        .strip_prefix(".split(")
        .or_else(|| cur.strip_prefix("&.split("))
        && let Some((sep, tail)) = quoted_token(rest.trim_start())
        && !sep.is_empty()
        && let Some(shape) = page_split_head_chars(sep, tail.trim())
    {
        return Some(PageBlock { shape });
    }
    // `page.split(/pat/).filter_map { |section|
    // [next if section.match?(/skip/)] section[regex, 1] }`
    if let Some(rest) = cur.strip_prefix(".split(")
        && let Some((pat, flags, tail)) = regex_literal(rest.trim())
        && let Some(tail) = tail.trim().strip_prefix(')')
    {
        let tail = tail.trim();
        let tail = tail
            .strip_suffix("&.flatten")
            .or_else(|| tail.strip_suffix(".flatten"))
            .unwrap_or(tail)
            .trim();
        let tail = [
            ".filter_map",
            "&.filter_map",
            ".flat_map",
            "&.flat_map",
            ".map",
            "&.map",
        ]
        .iter()
        .find_map(|p| tail.strip_prefix(p))?
        .trim();
        let (var, inner) = json_map_parts(tail)?;
        let inner = inner.trim();
        // `section.scan(regex).map { |match| "…" }`
        if let Some(scan) = inner
            .strip_prefix(&format!("{var}.scan(regex)"))
            .or_else(|| inner.strip_prefix(&format!("{var}&.scan(regex)")))
        {
            let scan = [
                ".filter_map",
                "&.filter_map",
                ".flat_map",
                "&.flat_map",
                ".map",
                "&.map",
            ]
            .iter()
            .find_map(|p| scan.trim().strip_prefix(p))?;
            let (mvar, minner) = json_map_parts(scan.trim())?;
            let (quoted, qtail) = quoted_token(minner.trim())?;
            if !qtail.trim().is_empty() {
                return None;
            }
            let parts = page_template_parts(&quoted, &mvar)?;
            return Some(PageBlock {
                shape: PageShape::SplitScan { pat, flags, parts },
            });
        }
        let (skip, inner) = if let Some(r) = inner.strip_prefix(&format!("next if {var}.match?(")) {
            let (sp, sf, t) = regex_literal(r.trim())?;
            let after = t.trim().strip_prefix(')')?;
            (Some((sp, sf)), after.trim())
        } else {
            (None, inner)
        };
        if inner != format!("{var}[regex, 1]") {
            return None;
        }
        return Some(PageBlock {
            shape: PageShape::SplitCapture { pat, flags, skip },
        });
    }
    let cur = cur
        .strip_prefix(".scan(regex)")
        .or_else(|| cur.strip_prefix("&.scan(regex)"))?;
    // `.map(&:first)[.uniq].min_by/max_by { |v| Version.new(v) }`
    if let Some(min) = page_scan_extreme(cur) {
        return Some(PageBlock {
            shape: PageShape::ScanExtreme(min),
        });
    }
    if let Some(shape) = page_select_shape(cur) {
        return Some(PageBlock { shape });
    }
    let rest = strip_map_open(cur)?;
    let (var, inner) = json_map_parts(rest)?;
    if let Some(shape) = page_scan_split_fork_shape(&var, &inner) {
        return Some(PageBlock { shape });
    }
    if let Some(shape) = page_date_shape(&var, &inner) {
        return Some(PageBlock { shape });
    }
    if let Some(shape) = page_reject_shape(&var, &inner) {
        return Some(PageBlock { shape });
    }
    if let Some(shape) = page_join_shape(&var, &inner) {
        return Some(PageBlock { shape });
    }
    if let Some(shape) = page_scan_template_shape(&var, &inner) {
        return Some(PageBlock { shape });
    }
    let mut after = inner.strip_prefix(var.as_str())?;
    for pre in ["&.first", ".first", "[0]"] {
        if let Some(r) = after.strip_prefix(pre) {
            after = r;
            break;
        }
    }
    // `<capture>.tr("from", "to")`
    if let Some(args) = after
        .strip_prefix(".tr(")
        .or_else(|| after.strip_prefix("&.tr("))
    {
        let (from, tail) = quoted_token(args)?;
        let tail = tail.trim().strip_prefix(',')?.trim();
        let (to, tail) = quoted_token(tail)?;
        let tail = tail.trim();
        if from.contains('\\') || to.contains('\\') || from.starts_with('^') {
            return None;
        }
        if tail == ")" {
            return Some(PageBlock {
                shape: PageShape::ScanTr(from, to),
            });
        }
        // Chained `.tr(…).sub(/pat/, '-\1')` (jxrlib).
        let chained = tail.strip_prefix(')')?;
        let (pat, flags, tmpl) = sub_backref_args(chained)?;
        return Some(PageBlock {
            shape: PageShape::ScanTrSubBackref(from, to, pat, flags, tmpl),
        });
    }
    // `<capture>.gsub("from", "to")` — literal arguments.
    if let Some(args) = after
        .strip_prefix(".gsub(")
        .or_else(|| after.strip_prefix("&.gsub("))
    {
        let (from, tail) = quoted_token(args)?;
        let tail = tail.trim().strip_prefix(',')?.trim();
        let (to, tail) = quoted_token(tail)?;
        if tail.trim() != ")" || from.is_empty() || from.contains('\\') || to.contains('\\') {
            return None;
        }
        return Some(PageBlock {
            shape: PageShape::ScanGsub(from, to),
        });
    }
    // `<capture>.sub(/pat/flags, "to")` — a literal
    // replacement, or a backreference template.
    if let Some(args) = after
        .strip_prefix(".sub(")
        .or_else(|| after.strip_prefix("&.sub("))
    {
        let (pat, flags, tail) = regex_literal(args)?;
        let tail = tail.trim().strip_prefix(',')?.trim();
        let (to, tail) = sub_template(tail)?;
        if tail.trim() != ")" {
            return None;
        }
        return Some(PageBlock {
            shape: if to.contains('\\') {
                PageShape::ScanSubBackref(pat, flags, to)
            } else {
                PageShape::ScanSub(pat, flags, to)
            },
        });
    }
    // `<capture>.split("", N).join("sep")`.
    if let Some(args) = after
        .strip_prefix(".split(\"\", ")
        .or_else(|| after.strip_prefix("&.split(\"\", "))
    {
        let (n_str, tail) = args.split_once(')')?;
        let n: usize = n_str.trim().parse().ok()?;
        let tail = tail.trim().strip_prefix(".join(")?;
        let (sep, tail) = quoted_token(tail.trim())?;
        if tail.trim() != ")" || n == 0 || sep.contains('\\') {
            return None;
        }
        return Some(PageBlock {
            shape: PageShape::ScanSplitChars(n, sep),
        });
    }
    // `<capture>.scan(/\d{N}/).map(&:to_i).join(".")`.
    if let Some(args) = after
        .strip_prefix(".scan(/\\d{")
        .or_else(|| after.strip_prefix("&.scan(/\\d{"))
    {
        let (n_str, tail) = args.split_once("}/)")?;
        let n: usize = n_str.parse().ok()?;
        let tail = tail.trim().strip_prefix(".map(&:to_i).join(")?;
        let (sep, tail) = quoted_token(tail.trim())?;
        if tail.trim() != ")" || n == 0 || sep.contains('\\') {
            return None;
        }
        return Some(PageBlock {
            shape: PageShape::ScanChunks(n, sep),
        });
    }
    None
}

/// The vilistextum reject form:
/// `((v = match.first) == "x") ? nil : v`.
fn page_reject_shape(var: &str, inner: &str) -> Option<super::formula::PageShape> {
    let rest = inner.strip_prefix("((")?;
    let (v, rest) = rest.split_once(" = ")?;
    if v.is_empty() || !v.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    let mut rest = rest;
    for acc in [
        format!("{var}&.first"),
        format!("{var}.first"),
        format!("{var}[0]"),
    ] {
        if let Some(r) = rest.strip_prefix(acc.as_str()) {
            rest = r;
            break;
        }
    }
    let rest = rest.strip_prefix(") == ")?.trim();
    let (lit, tail) = quoted_token(rest)?;
    (tail.trim() == format!(") ? nil : {v}"))
        .then_some(super::formula::PageShape::ScanRejectEq(lit))
}

/// A bracketed span at the start of `s` (which must
/// start with `open`): the span including its
/// brackets, and the rest. Quote-aware (single and
/// double quotes, backslash escapes); only the
/// given bracket pair nests.
fn take_bracketed(s: &str, open: char, close: char) -> Option<(&str, &str)> {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match quote {
            Some(q) => {
                if c == '\\' {
                    escaped = true;
                } else if c == q {
                    quote = None;
                }
            }
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == open => depth += 1,
            None if c == close => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some((&s[..=i], &s[i + c.len_utf8()..]));
                }
            }
            None => {}
        }
    }
    None
}

/// Two strings a scan reads as one: a prefix and suffix, or a key and its value.
type Pair = (String, String);

/// A hop-scan's page-one pick: `page[/lit/, 1]`,
/// `page.scan(regex)&.flatten&.last`, or a literal
/// scan mapped to firsts and maxed by version.
/// Returns the pick data and the unparsed rest.
fn hop_pick<'a>(
    root: &str,
    rhs: &'a str,
) -> Option<(Option<Pair>, super::formula::HopPick, &'a str)> {
    use super::formula::HopPick;
    let after = rhs.trim().strip_prefix(root)?;
    if let Some(rest) = after.strip_prefix(".match(") {
        let (pat, flags, rest) = regex_literal(rest.trim())?;
        let rest = rest.trim().strip_prefix(')')?;
        return Some((Some((pat, flags)), HopPick::MatchFirst, rest));
    }
    if let Some(rest) = after.strip_prefix(".scan(regex).flatten.max_by") {
        let rest = rest.trim_start();
        let (block, rest) = take_bracketed(rest, '{', '}')?;
        let (vvar, vinner) = json_map_parts(block)?;
        if vinner.trim() != format!("Version.new({vvar})") {
            return None;
        }
        return Some((None, HopPick::ScanMax, rest));
    }
    if let Some(rest) = after.strip_prefix(".scan(regex)&.flatten&.last") {
        return Some((None, HopPick::ScanLast, rest));
    }
    if let Some((pat, flags, rest)) = literal_capture_tail(after) {
        return Some((Some((pat, flags)), HopPick::First, rest));
    }
    let inner = after.strip_prefix(".scan(")?;
    let (pat, flags, rest) = regex_literal(inner)?;
    let rest = rest.trim().strip_prefix(')')?.trim();
    if let Some(r) = rest.strip_prefix(".flatten") {
        let r = r.trim().strip_prefix(".uniq")?.trim();
        if let Some(r) = r.strip_prefix(".max")
            && !r.starts_with("_by")
            && r.chars().next().is_none_or(|c| c == ' ')
        {
            return Some((Some((pat, flags)), HopPick::ScanLexMax, r));
        }
        let r = r.strip_prefix(".sort_by")?.trim_start();
        let (block, r) = take_bracketed(r, '{', '}')?;
        let (vvar, vinner) = json_map_parts(block)?;
        if vinner.trim() != format!("Version.new({vvar})") {
            return None;
        }
        return Some((Some((pat, flags)), HopPick::ScanMax, r));
    }
    if let Some(r) = rest.strip_prefix(".max_by") {
        let r = r.trim_start();
        let (block, r) = take_bracketed(r, '{', '}')?;
        let (mvar, minner) = json_map_parts(block)?;
        let inner = minner
            .trim()
            .strip_prefix("Version.new(")?
            .strip_suffix(')')?;
        let k = page_cap_index(&mvar, inner.trim())?;
        let r = r.trim().strip_prefix("&.first")?;
        return Some((Some((pat, flags)), HopPick::ScanMaxKey(k), r));
    }
    let rest = rest.strip_prefix(".map")?.trim_start();
    let (block, rest) = take_bracketed(rest, '{', '}')?;
    let (mvar, minner) = json_map_parts(block)?;
    let minner = minner.trim();
    if minner != format!("{mvar}[0]") && minner != format!("{mvar}.first") {
        return None;
    }
    let rest = rest.trim().strip_prefix(".max_by")?.trim_start();
    let (block, rest) = take_bracketed(rest, '{', '}')?;
    let (vvar, vinner) = json_map_parts(block)?;
    if vinner.trim() != format!("Version.new({vvar})") {
        return None;
    }
    Some((Some((pat, flags)), HopPick::ScanMax, rest))
}

/// A hop-scan's second URL inside `page_content(…)`:
/// `URI.join("lit", v)`, `URI.join(@url, v)` (either
/// with `.to_s`), or a quoted template holding
/// exactly `#{v}`.
fn hop_target(var: &str, arg: &str) -> Option<super::formula::HopTarget> {
    use super::formula::HopTarget;
    let arg = arg.trim().strip_suffix(',').unwrap_or(arg.trim()).trim();
    let slot = format!("#{{{var}}}");
    // Remaining interpolations must be `version`
    // chains (substituted at plan time).
    let only_version_tokens = |s: &str| -> bool {
        let mut rest = s;
        while let Some(i) = rest.find("#{") {
            let Some(j) = rest[i + 2..].find('}') else {
                return false;
            };
            if !super::formula::version_chain_syntax(rest[i + 2..i + 2 + j].trim()) {
                return false;
            }
            rest = &rest[i + 2 + j + 1..];
        }
        true
    };
    let split_slot = |template: &str| -> Option<(String, String)> {
        let (pre, post) = template.split_once(&slot)?;
        if !only_version_tokens(pre) || !only_version_tokens(post) {
            return None;
        }
        Some((pre.to_string(), post.to_string()))
    };
    if let Some(join) = arg.strip_prefix("URI.join(") {
        let (base, rest) = if let Some(r) = join.strip_prefix("@url") {
            (None, r)
        } else if let Some((lit, r)) = quoted_token(join) {
            (Some(lit), r)
        } else {
            return None;
        };
        let rest = rest.trim().strip_prefix(',')?.trim();
        // A quoted template argument joins after
        // substitution; a bare var joins as itself.
        if let Some((template, r)) = quoted_token(rest) {
            let (pre, post) = split_slot(&template)?;
            let r = r.trim().strip_prefix(')')?;
            let r = r.strip_prefix(".to_s").unwrap_or(r);
            return r
                .trim()
                .is_empty()
                .then_some(HopTarget::JoinTemplate { base, pre, post });
        }
        let rest = rest.strip_prefix(var)?.trim().strip_prefix(')')?;
        let rest = rest.strip_prefix(".to_s").unwrap_or(rest);
        let target = match base {
            Some(lit) => HopTarget::JoinLiteral(lit),
            None => HopTarget::JoinSelf,
        };
        return rest.trim().is_empty().then_some(target);
    }
    let (template, tail) = quoted_token(arg)?;
    if !tail.trim().is_empty() {
        return None;
    }
    // `#{url}` is the livecheck URL itself: mark it
    // with a sentinel the evaluator expands.
    let template = template.replace("#{url}", "\u{2}");
    let (pre, post) = split_slot(&template)?;
    Some(HopTarget::Template(pre, post))
}

/// The second fetch's blank guard at the start of
/// `s`; returns the expression the final scan
/// reads (`w`, an assigned content var, or
/// `w[:content]`) and the rest.
fn hop_content_guard<'a>(wvar: &str, took_content: bool, s: &'a str) -> Option<(String, &'a str)> {
    let s = s.trim();
    if took_content {
        for form in [
            format!("next if {wvar}.blank?"),
            format!("next unless {wvar}"),
        ] {
            if let Some(rest) = s.strip_prefix(&form) {
                return Some((wvar.to_string(), rest));
            }
        }
        return None;
    }
    if let Some(rest) = s.strip_prefix(&format!("next if {wvar}[:content].blank?")) {
        return Some((format!("{wvar}[:content]"), rest));
    }
    let inner = s.strip_prefix("next if (")?;
    let close = inner.find(").blank?")?;
    let (cvar, rhs) = assign_parts(&inner[..close])?;
    (rhs == format!("{wvar}[:content]")).then_some((cvar, &inner[close + ").blank?".len()..]))
}

/// The final scan of a hop body: all captures,
/// only the last (`&.flatten&.last`), or a
/// per-match render (parts over scan captures).
/// Returns (last_only, scan parts).
fn hop_final(
    content_root: &str,
    stmt: &str,
) -> Option<(bool, Option<Vec<super::formula::PagePart>>)> {
    use super::formula::PagePart;
    let rest = stmt.trim().strip_prefix(content_root)?;
    let rest = rest
        .strip_prefix("&.scan(regex)")
        .or_else(|| rest.strip_prefix(".scan(regex)"))?;
    let rest = rest.trim();
    if rest == "&.flatten&.last" {
        return Some((true, None));
    }
    if rest == ".map(&:first)" || rest == "&.map(&:first)" {
        return Some((false, None));
    }
    let mrest = strip_map_open(rest)?;
    let (var, inner) = json_map_parts(mrest)?;
    let inner = inner.trim();
    if inner == format!("{var}[0]") || inner == format!("{var}.first") {
        return Some((false, None));
    }
    if inner.starts_with('"') {
        let (body_str, tail) = quoted_token(inner)?;
        if !tail.trim().is_empty() {
            return None;
        }
        return Some((false, Some(page_template_parts(&body_str, &var)?)));
    }
    let r = inner.strip_prefix(&format!("{var}["))?;
    let close = r.find(']')?;
    let n: usize = r[..close].parse().ok()?;
    let part = page_cap_part(n, r[close + 1..].trim())?;
    let _ = PagePart::Lit(String::new());
    Some((false, Some(vec![part])))
}

/// The final single-match render of a hop body:
/// `<mv> = <content>.match(regex)`, a blank guard,
/// and a template over the captures and hop value.
fn hop_final_match(
    content_root: &str,
    hop_var: &str,
    stmt: &str,
) -> Option<Vec<super::formula::HopFinalPart>> {
    use super::formula::HopFinalPart;
    let (mv, rest) = assign_parts(stmt.trim())?;
    let rest = rest
        .trim()
        .strip_prefix(content_root)?
        .strip_prefix(".match(regex)")?
        .trim();
    let rest = rest
        .strip_prefix(&format!("next if {mv}.blank?"))
        .or_else(|| rest.strip_prefix(&format!("next unless {mv}")))?
        .trim();
    let (body_str, tail) = quoted_token(rest)?;
    if !tail.trim().is_empty() {
        return None;
    }
    let mut parts: Vec<HopFinalPart> = Vec::new();
    let mut rest = body_str.as_str();
    while let Some(i) = rest.find("#{") {
        if i > 0 {
            parts.push(HopFinalPart::Lit(ruby_unescape_dq(&rest[..i])));
        }
        let j = rest[i + 2..].find('}')?;
        let token = rest[i + 2..i + 2 + j].trim();
        if token == hop_var {
            parts.push(HopFinalPart::Hop);
        } else if let Some(r) = token.strip_prefix(&format!("{mv}[")) {
            let (n, t) = r.split_once(']')?;
            if !t.trim().is_empty() {
                return None;
            }
            parts.push(HopFinalPart::Cap(n.trim().parse().ok()?));
        } else {
            return None;
        }
        rest = &rest[i + 2 + j + 1..];
    }
    if !rest.is_empty() {
        parts.push(HopFinalPart::Lit(ruby_unescape_dq(rest)));
    }
    Some(parts)
}

/// Render parts for a header-hop final: captures of
/// `mv`, a literal-capture local (as capture 1),
/// and the hop value.
fn header_hop_parts(
    body_str: &str,
    mv: &str,
    cap_var: Option<&str>,
    hop_var: &str,
) -> Option<Vec<super::formula::HopFinalPart>> {
    use super::formula::HopFinalPart;
    let mut parts: Vec<HopFinalPart> = Vec::new();
    let mut rest = body_str;
    while let Some(i) = rest.find("#{") {
        if i > 0 {
            parts.push(HopFinalPart::Lit(ruby_unescape_dq(&rest[..i])));
        }
        let j = rest[i + 2..].find('}')?;
        let token = rest[i + 2..i + 2 + j].trim();
        if token == hop_var {
            parts.push(HopFinalPart::Hop);
        } else if Some(token) == cap_var {
            parts.push(HopFinalPart::Cap(1));
        } else if let Some(r) = token.strip_prefix(&format!("{mv}[")) {
            let (n, t) = r.split_once(']')?;
            if !t.trim().is_empty() {
                return None;
            }
            parts.push(HopFinalPart::Cap(n.trim().parse().ok()?));
        } else {
            return None;
        }
        rest = &rest[i + 2 + j + 1..];
    }
    if !rest.is_empty() {
        parts.push(HopFinalPart::Lit(ruby_unescape_dq(rest)));
    }
    Some(parts)
}

/// Pick a value, fetch the named URL's headers
/// (`page_headers`), and render from a header
/// match — merged (grafx, linqpad) or per response
/// set (platinum-notes).
/// Parts over one match variable's captures for a
/// page template (`#{match[1]},#{match[2]}`).
fn match_template_parts(body_str: &str, mv: &str) -> Option<Vec<super::formula::HopFinalPart>> {
    use super::formula::HopFinalPart;
    let mut parts: Vec<HopFinalPart> = Vec::new();
    let mut rest = body_str;
    while let Some(i) = rest.find("#{") {
        if i > 0 {
            parts.push(HopFinalPart::Lit(ruby_unescape_dq(&rest[..i])));
        }
        let j = rest[i + 2..].find('}')?;
        let token = rest[i + 2..i + 2 + j].trim();
        let r = token.strip_prefix(&format!("{mv}["))?;
        let (n, t) = r.split_once(']')?;
        if !t.trim().is_empty() {
            return None;
        }
        parts.push(HopFinalPart::Cap(n.trim().parse().ok()?));
        rest = &rest[i + 2 + j + 1..];
    }
    if !rest.is_empty() {
        parts.push(HopFinalPart::Lit(ruby_unescape_dq(rest)));
    }
    Some(parts)
}

/// A literal-regex page capture parsed as JSON,
/// dug, and matched against the block regex
/// (popo's JSONP wrapper).
fn parse_page_json_dig_match(root: &str, lines: &[String]) -> Option<super::formula::PageShape> {
    use super::formula::PageShape;
    let body = lines
        .iter()
        .map(|l| strip_trailing_comment(l.trim()).trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let ident = |s: &str| -> Option<String> {
        let s = s.trim();
        if s.is_empty() || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
        Some(s.to_string())
    };
    let mut rest = body.trim();
    let mut lit: Option<(String, String)> = None;
    let mut jr_name: Option<String> = None;
    // Optional `<jr> = /lit/flags` pre-statement.
    if let Some((v, rhs)) = rest.split_once(" = ")
        && let Ok(v) = ident(v).ok_or(())
        && let Some((pat, flags, tail)) = regex_literal(rhs.trim())
    {
        lit = Some((pat, flags));
        jr_name = Some(v);
        rest = tail.trim();
    }
    let (mv, rhs) = rest.split_once(" = ")?;
    let mv = ident(mv)?;
    let rhs = rhs.trim();
    // First capture: `page.match(<jr>|/lit/)` (cap
    // index read at the parse_json call) or
    // `page[/lit/, N]` (the string itself).
    let (cap, rest, bracket): (usize, &str, bool) =
        if let Some(r) = rhs.strip_prefix(&format!("{root}.match(")) {
            let r = r.trim();
            let (l, t) = if r.starts_with('/') || r.starts_with('%') {
                let (pat, flags, tail) = regex_literal(r)?;
                let tail = tail.trim().strip_prefix(')')?;
                ((pat, flags), tail)
            } else {
                let (name, tail) = r.split_once(')')?;
                if Some(name.trim()) != jr_name.as_deref() {
                    return None;
                }
                (lit.clone()?, tail)
            };
            lit = Some(l);
            (usize::MAX, t, false)
        } else if let Some(r) = rhs.strip_prefix(&format!("{root}[")) {
            let (pat, flags, t) = regex_literal(r.trim())?;
            let t = t.trim().strip_prefix(',')?.trim();
            let (n, t) = t.split_once(']')?;
            lit = Some((pat, flags));
            (n.trim().parse().ok()?, t, true)
        } else {
            return None;
        };
    let lit = lit?;
    let rest = rest.trim();
    let rest = rest
        .strip_prefix(&format!("next if {mv}.blank?"))
        .or_else(|| rest.strip_prefix(&format!("next unless {mv}")))?
        .trim();
    let (j_name, rhs) = rest.split_once(" = ")?;
    let j_name = ident(j_name)?;
    let inner = rhs
        .trim()
        .strip_prefix("Homebrew::Livecheck::Strategy::Json.parse_json(")?
        .trim();
    let (cap, rest) = if bracket {
        let rest = inner.strip_prefix(&mv)?.strip_prefix(')')?;
        (cap, rest)
    } else {
        let (arg, rest) = inner.split_once(')')?;
        let c = page_cap_index(&mv, arg.trim())?;
        (c, rest)
    };
    // Nested-map tail (lg-onscreen-control):
    // `<j>.dig(...)&.map do |_, files| files.map do
    // |file| match = file["f"]&.match(regex) next if
    // match.blank? [|| (x = file["g"]).blank?] "…"
    // end end&.flatten`.
    if let Some(shape) = parse_page_json_nested_tail(rest.trim(), &j_name, &lit, cap) {
        return Some(shape);
    }
    let (m2, rhs) = rest.trim().split_once(" = ")?;
    let m2 = ident(m2)?;
    let (path, tail) = jt_dig(rhs.trim(), &j_name, &[])?;
    // An interpolated key (`"#{arch}Url"`) parses
    // only on the substituted-lines retry.
    if path.is_empty()
        || path
            .iter()
            .any(|k| matches!(k, super::formula::JsonKey::Key(s) if s.contains("#{")))
    {
        return None;
    }
    let rest = tail
        .trim()
        .strip_prefix("&.match(regex)")
        .or_else(|| tail.trim().strip_prefix(".match(regex)"))?
        .trim();
    let rest = rest
        .strip_prefix(&format!("next if {m2}.blank?"))
        .or_else(|| rest.strip_prefix(&format!("next unless {m2}")))?
        .trim();
    let (body_str, tail) = quoted_token(rest)?;
    if !tail.trim().is_empty() {
        return None;
    }
    let parts = match_template_parts(&body_str, &m2)?;
    Some(PageShape::MatchJsonDigMatch {
        lit,
        cap,
        path,
        parts,
    })
}

/// The lg-onscreen-control nested-map tail after
/// `parse_json(...)`: dig to a hash of file lists,
/// match each file's field, render.
fn parse_page_json_nested_tail(
    rest: &str,
    j_name: &str,
    lit: &(String, String),
    cap: usize,
) -> Option<super::formula::PageShape> {
    use super::formula::{HopFinalPart, PageShape};
    let (path, tail) = jt_dig(rest, j_name, &[])?;
    if path.is_empty()
        || path
            .iter()
            .any(|k| matches!(k, super::formula::JsonKey::Key(s) if s.contains("#{")))
    {
        return None;
    }
    let tail = tail.trim();
    let tail = tail
        .strip_prefix("&.map do |")
        .or_else(|| tail.strip_prefix(".map do |"))?;
    let (params, tail) = tail.split_once('|')?;
    let params: Vec<&str> = params.split(',').map(str::trim).collect();
    if params.len() != 2 || params.iter().any(|p| p.is_empty()) {
        return None;
    }
    let files_var = params[1];
    let tail = tail.trim();
    let tail = tail
        .strip_prefix(&format!("{files_var}.map do |"))
        .or_else(|| tail.strip_prefix(&format!("{files_var}&.map do |")))?;
    let (file_var, tail) = tail.split_once('|')?;
    let file_var = file_var.trim();
    if file_var.is_empty()
        || !file_var
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    let (m_name, tail) = tail.trim().split_once(" = ")?;
    let m_name = m_name.trim();
    if m_name.is_empty()
        || !m_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    let tail = tail.trim().strip_prefix(&format!("{file_var}["))?;
    let (field, tail) = quoted_token(tail.trim())?;
    let tail = tail.trim().strip_prefix("]&.match(regex)")?;
    let tail = tail
        .trim()
        .strip_prefix(&format!("next if {m_name}.blank?"))?
        .trim();
    let (extra, extra_var, tail) = if let Some(t) = tail.strip_prefix("|| (") {
        let (ev, t) = t.split_once(" = ")?;
        let ev = ev.trim();
        if ev.is_empty() || !ev.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
        let t = t.trim().strip_prefix(&format!("{file_var}["))?;
        let (f2, t) = quoted_token(t.trim())?;
        let t = t.trim().strip_prefix("]).blank?")?;
        (Some(f2), Some(ev.to_string()), t.trim())
    } else {
        (None, None, tail)
    };
    let (tmpl, tail) = quoted_token(tail)?;
    if !matches!(
        tail.trim(),
        "end end&.flatten" | "end end.flatten" | "end end"
    ) {
        return None;
    }
    // Template parts: `#{m[n]}` captures, an
    // optional `#{extra}` (the Hop part), literals.
    let mut parts: Vec<HopFinalPart> = Vec::new();
    let mut rest_t = tmpl.as_str();
    while let Some(i) = rest_t.find("#{") {
        if i > 0 {
            parts.push(HopFinalPart::Lit(rest_t[..i].to_string()));
        }
        let tok = &rest_t[i + 2..];
        let j = tok.find('}')?;
        let token = tok[..j].trim();
        if let Some(idx) = token
            .strip_prefix(&format!("{m_name}["))
            .and_then(|t| t.strip_suffix(']'))
        {
            parts.push(HopFinalPart::Cap(idx.trim().parse().ok()?));
        } else if extra_var.as_deref() == Some(token) {
            parts.push(HopFinalPart::Hop);
        } else {
            return None;
        }
        rest_t = &tok[j + 1..];
    }
    if !rest_t.is_empty() {
        parts.push(HopFinalPart::Lit(rest_t.to_string()));
    }
    Some(PageShape::MatchJsonNestedMap {
        lit: lit.clone(),
        cap,
        path,
        field,
        extra,
        parts,
    })
}

/// ripx: scan sections, repeatedly gsub-strip tags
/// from each capture until stable, then read the
/// block regex capture from the cleaned text.
fn parse_page_scan_strip_match(root: &str, lines: &[String]) -> Option<super::formula::PageShape> {
    use super::formula::PageShape;
    let body = lines
        .iter()
        .map(|l| strip_trailing_comment(l.trim()).trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let ident = |s: &str| -> Option<String> {
        let s = s.trim();
        if s.is_empty() || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
        Some(s.to_string())
    };
    let rest = body.strip_prefix(&format!("{root}.scan("))?;
    let (pat, flags, tail) = regex_literal(rest.trim())?;
    let tail = tail.trim().strip_prefix(").map do |")?;
    let (var, rest) = tail.split_once('|')?;
    let var = ident(var)?;
    let (t, rest) = rest.trim().split_once(" = ")?;
    let t = ident(t)?;
    let rest = rest.trim().strip_prefix(&format!("{var}["))?;
    let (idx, rest) = rest.split_once(']')?;
    let idx: usize = idx.trim().parse().ok()?;
    let (p, rest) = rest.trim().split_once(" = nil ")?;
    let p = ident(p)?;
    let rest = rest
        .trim()
        .strip_prefix(&format!("while {t} != {p} "))?
        .trim();
    let rest = rest
        .strip_prefix(&format!("{p} = {t} "))?
        .trim()
        .strip_prefix(&format!("{t} = {t}.gsub("))?;
    let (spat, sflags, tail) = regex_literal(rest.trim())?;
    let tail = tail.trim().strip_prefix(',')?.trim();
    let (repl, tail) = quoted_token(tail)?;
    if repl.contains("#{") || repl.contains('\\') {
        return None;
    }
    let rest = tail
        .trim()
        .strip_prefix(") end ")?
        .trim()
        .strip_prefix(&format!("{t}[regex, "))?
        .trim();
    let (cap, tail) = rest.split_once(']')?;
    let cap: usize = cap.trim().parse().ok()?;
    if tail.trim() != "end" {
        return None;
    }
    Some(PageShape::ScanStripMatch {
        pat,
        flags,
        idx,
        strip: (spat, sflags),
        repl,
        cap,
    })
}

/// font-kikai: lexical scan maximum, with a
/// dotless-to-dotted remap through a second scan.
fn parse_page_scan_dotted_max(root: &str, lines: &[String]) -> Option<super::formula::PageShape> {
    use super::formula::PageShape;
    let body = lines
        .iter()
        .map(|l| strip_trailing_comment(l.trim()).trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let ident = |s: &str| -> Option<String> {
        let s = s.trim();
        if s.is_empty() || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
        Some(s.to_string())
    };
    let (ver, rest) = body.split_once(" = ")?;
    let ver = ident(ver)?;
    let rest = rest
        .trim()
        .strip_prefix(&format!("{root}.scan(regex).map {{ |"))?;
    let (m1, rest) = rest.split_once('|')?;
    let m1 = ident(m1)?;
    let rest = rest
        .trim()
        .strip_prefix(&format!("{m1}[0] }}.max "))?
        .trim()
        .strip_prefix(&format!("next {ver} if {ver}.include?(\".\") "))?
        .trim();
    let (dotted, rest) = rest.split_once(" = nil ")?;
    let dotted = ident(dotted)?;
    let rest = rest.trim().strip_prefix(&format!("{root}.scan("))?;
    let (pat, flags, tail) = regex_literal(rest.trim())?;
    let tail = tail.trim().strip_prefix(").each do |")?;
    let (m2, rest) = tail.split_once('|')?;
    let m2 = ident(m2)?;
    let (dotless, rest) = rest.trim().split_once(" = ")?;
    let dotless = ident(dotless)?;
    let rest = rest
        .trim()
        .strip_prefix(&format!("DSL::Version.new({m2}[0]).no_dots "))?
        .trim()
        .strip_prefix(&format!("if {ver} == {dotless} "))?
        .trim()
        .strip_prefix(&format!("{dotted} = {m2}[0] break end end "))?
        .trim();
    if rest != dotted {
        return None;
    }
    Some(PageShape::ScanDottedMax { lit: (pat, flags) })
}

/// bettertouchtool@alpha: newest by listing time,
/// throttled against the current build.
fn parse_page_scan_time_max_throttle(
    root: &str,
    lines: &[String],
) -> Option<super::formula::PageShape> {
    use super::formula::PageShape;
    let body = lines
        .iter()
        .map(|l| strip_trailing_comment(l.trim()).trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let ident = |s: &str| -> Option<String> {
        let s = s.trim();
        if s.is_empty() || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
        Some(s.to_string())
    };
    let pair = |s: &str| -> Option<(String, String)> {
        let (a, b) = s.split_once(',')?;
        Some((ident(a)?, ident(b)?))
    };
    let (lhs, rest) = body.split_once(" = version.csv ")?;
    let (cv, cb) = pair(lhs)?;
    let (lhs, rest) = rest.trim().split_once(" = ")?;
    let (v, b) = pair(lhs)?;
    let rest = rest
        .trim()
        .strip_prefix(&format!("{root}.scan(regex).max_by {{ |"))?;
    let (m, rest) = rest.split_once('|')?;
    let m = ident(m)?;
    let rest = rest.trim().strip_prefix(&format!("Time.parse({m}["))?;
    let (time_idx, rest) = rest.split_once("]) }&.first&.split(")?;
    let time_idx: usize = time_idx.trim().parse().ok()?;
    let (sep, rest) = quoted_token(rest.trim())?;
    let rest = rest.trim().strip_prefix(", 2) ")?.trim();
    let rest = rest.strip_prefix(&format!("if {b} && {cb}.to_i + "))?;
    let (step, rest) = rest.split_once(&format!(" > {b}.to_i "))?;
    let step: i64 = step.trim().parse().ok()?;
    let rest = rest
        .trim()
        .strip_prefix(&format!("{v} = {cv} {b} = {cb} end "))?
        .trim();
    if rest != format!("\"#{{{v}}},#{{{b}}}\"") {
        return None;
    }
    Some(PageShape::ScanTimeMaxThrottle {
        time_idx,
        sep,
        step,
    })
}

/// The scan maximum as a Version plus an optional
/// literal capture suffix (hy-rpe2's path date).
fn parse_page_scanmax_optcap(root: &str, lines: &[String]) -> Option<super::formula::PageShape> {
    use super::formula::PageShape;
    let body = lines
        .iter()
        .map(|l| strip_trailing_comment(l.trim()).trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let ident = |s: &str| -> Option<String> {
        let s = s.trim();
        if s.is_empty() || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
        Some(s.to_string())
    };
    let (v, rest) = body.trim().split_once(" = ")?;
    let v = ident(v)?;
    let rest = rest
        .trim()
        .strip_prefix(&format!("{root}.scan(regex)"))?
        .trim()
        .strip_prefix(".map")?
        .trim_start();
    let (block, rest) = take_bracketed(rest, '{', '}')?;
    let (mvar, minner) = json_map_parts(block)?;
    if minner.trim() != format!("{mvar}[0]") {
        return None;
    }
    let rest = rest.trim().strip_prefix(".max_by")?.trim_start();
    let (block, rest) = take_bracketed(rest, '{', '}')?;
    let (xvar, xinner) = json_map_parts(block)?;
    if xinner.trim() != format!("Version.new({xvar})") {
        return None;
    }
    let rest = rest
        .trim()
        .strip_prefix(&format!("next unless {v}"))
        .or_else(|| rest.trim().strip_prefix(&format!("next if {v}.blank?")))?
        .trim();
    let (d, rhs) = rest.split_once(" = ")?;
    let d = ident(d)?;
    let rhs = rhs.trim().strip_prefix(&format!("{root}["))?;
    let (pat, flags, t) = regex_literal(rhs.trim())?;
    let t = t.trim().strip_prefix(',')?.trim();
    let (cap_s, rest) = t.split_once(']')?;
    let cap: usize = cap_s.trim().parse().ok()?;
    let rest = rest.trim().strip_prefix(&format!("{d} ?"))?.trim();
    let (body_str, tail) = quoted_token(rest)?;
    let tail = tail.trim().strip_prefix(':')?.trim();
    if tail != v {
        return None;
    }
    // Template must be `#{v}<sep>#{d}` with an
    // optional tr on the date.
    let after_v = body_str.strip_prefix(&format!("#{{{v}}}"))?;
    let Some(i) = after_v.find("#{") else {
        return None;
    };
    let sep = ruby_unescape_dq(&after_v[..i]);
    let token_full = &after_v[i + 2..];
    let j = token_full.find('}')?;
    if !token_full[j + 1..].is_empty() {
        return None;
    }
    let token = token_full[..j].trim();
    let tr = if token == d {
        None
    } else if let Some(args) = token.strip_prefix(&format!("{d}.tr(")) {
        let (from, t) = quoted_token(args.trim_start())?;
        let (to, t) = quoted_token(t.trim().strip_prefix(',')?.trim())?;
        if t.trim() != ")" || from.contains('\\') || to.contains('\\') {
            return None;
        }
        Some((from, to))
    } else {
        return None;
    };
    Some(PageShape::ScanMaxOptCap {
        pat,
        flags,
        cap,
        sep,
        tr,
    })
}

fn parse_page_header_hop(root: &str, lines: &[String]) -> Option<super::formula::PageShape> {
    use super::formula::{HeaderHop, HopFinalPart, PageShape};
    let body = lines
        .iter()
        .map(|l| strip_trailing_comment(l.trim()).trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let (var, rest) = assign_parts(body.trim())?;
    let (hop_re, pick, rest) = hop_pick(root, rest)?;
    let rest = rest.trim();
    let rest = rest
        .strip_prefix(&format!("next unless {var}"))
        .or_else(|| rest.strip_prefix(&format!("next if {var}.blank?")))?
        .trim();
    // Optional chained second pick (mixed-in-key):
    // fetch a page named by the first pick, bracket
    // a new value out of it; downstream reads the
    // new value as the hop value.
    let mut mid: Option<super::formula::MidHop> = None;
    let mut var = var;
    let mut rest = rest;
    if let Some(parsed) = (|| -> Option<(super::formula::MidHop, String, &str)> {
        let (dp, r) = assign_parts(rest)?;
        let r = r
            .trim()
            .strip_prefix("Homebrew::Livecheck::Strategy.page_content(")?
            .trim();
        let (tmpl, r) = quoted_token(r)?;
        let r = r.trim();
        let r = r.strip_prefix(',').map_or(r, str::trim);
        let r = r.strip_prefix(')')?.trim();
        let (pre, post) = tmpl.split_once(&format!("#{{{var}}}"))?;
        let only_version = |s: &str| -> bool {
            let mut rest_s = s;
            while let Some(i) = rest_s.find("#{") {
                let Some(j) = rest_s[i + 2..].find('}') else {
                    return false;
                };
                if !super::formula::version_chain_syntax(rest_s[i + 2..i + 2 + j].trim()) {
                    return false;
                }
                rest_s = &rest_s[i + 2 + j + 1..];
            }
            true
        };
        if !only_version(pre) || !only_version(post) {
            return None;
        }
        let r = r.strip_prefix("next if (")?.trim();
        let (dpc, r) = assign_parts(r)?;
        let r = r
            .trim()
            .strip_prefix(&format!("{dp}[:content]).blank?"))?
            .trim();
        let (var2, r) = assign_parts(r)?;
        let r = r.trim().strip_prefix(&format!("{dpc}["))?;
        let (pat, flags, t) = regex_literal(r.trim())?;
        let t = t.trim().strip_prefix(',')?.trim();
        let (cap_s, t) = t.split_once(']')?;
        let cap: usize = cap_s.trim().parse().ok()?;
        let t = t
            .trim()
            .strip_prefix(&format!("next if {var2}.blank?"))
            .or_else(|| t.trim().strip_prefix(&format!("next unless {var2}")))?
            .trim();
        Some((
            super::formula::MidHop {
                url_pre: pre.to_string(),
                url_post: post.to_string(),
                lit: (pat, flags),
                cap,
            },
            var2,
            t,
        ))
    })() {
        mid = Some(parsed.0);
        var = parsed.1;
        rest = parsed.2;
    }
    // Per-set form: `page_headers(arg)&.map do |h| … end`.
    if let Some(r) = rest.strip_prefix("Homebrew::Livecheck::Strategy.page_headers") {
        let (parens, rest) = take_bracketed(r.trim_start(), '(', ')')?;
        let target = hop_target(&var, &parens[1..parens.len() - 1])?;
        let rest = rest.trim().strip_prefix("&.map do |")?;
        let (hvar, rest) = rest.split_once('|')?;
        let hvar = hvar.trim();
        let (mv, rest) = assign_parts(rest.trim())?;
        let rest = rest.trim().strip_prefix('(')?;
        let (h1, rest) = header_name(&rest, hvar)?;
        let rest = rest.trim().strip_prefix("||")?.trim();
        let (h2, rest) = header_name(rest, hvar)?;
        let rest = rest
            .trim()
            .strip_prefix(')')?
            .strip_prefix("&.match(regex)")?
            .trim();
        let rest = rest
            .strip_prefix(&format!("next if {mv}.blank?"))
            .or_else(|| rest.strip_prefix(&format!("next unless {mv}")))?
            .trim();
        let (body_str, tail) = quoted_token(rest)?;
        if tail.trim() != "end" {
            return None;
        }
        let parts = header_hop_parts(&body_str, &mv, None, &var)?;
        return Some(PageShape::HeaderHop(Box::new(HeaderHop {
            hop_re,
            pick,
            target,
            header: h1,
            fallback: Some(h2),
            per_set: true,
            lit_cap: None,
            parts,
            mid,
        })));
    }
    let (hvar, rest) = assign_parts(rest)?;
    let rest = rest
        .trim()
        .strip_prefix("Homebrew::Livecheck::Strategy.page_headers")?
        .trim_start();
    let (parens, rest) = take_bracketed(rest, '(', ')')?;
    let target = hop_target(&var, &parens[1..parens.len() - 1])?;
    let rest = rest.trim().strip_prefix(".reduce(&:merge)")?.trim();
    let (mv, rest) = assign_parts(rest)?;
    let (header, rest) = header_name(rest.trim(), &hvar)?;
    let rest = rest.trim();
    let (lit_cap, cap_var, rest): (Option<(String, String)>, Option<&str>, &str) =
        if let Some(r) = rest.strip_prefix("&.match(regex)") {
            (None, None, r)
        } else if let Some(r) = rest
            .strip_prefix("&.[](")
            .or_else(|| rest.strip_prefix(".[]("))
        {
            let (pat, flags, t) = regex_literal(r.trim())?;
            let t = t.trim().strip_prefix(", 1)")?;
            (Some((pat, flags)), Some(mv.as_str()), t)
        } else {
            return None;
        };
    // In the literal-capture form `mv` is the capture
    // local itself; the guard names it either way.
    let rest = rest.trim();
    let rest = rest
        .strip_prefix(&format!("next if {mv}.blank?"))
        .or_else(|| rest.strip_prefix(&format!("next unless {mv}")))?
        .trim();
    let parts = if let Some(r) = rest.strip_prefix(&format!("{mv}[")) {
        let (n, t) = r.split_once(']')?;
        if !t.trim().is_empty() || cap_var.is_some() {
            return None;
        }
        vec![HopFinalPart::Cap(n.trim().parse().ok()?)]
    } else {
        let (body_str, tail) = quoted_token(rest)?;
        if !tail.trim().is_empty() {
            return None;
        }
        header_hop_parts(&body_str, &mv, cap_var, &var)?
    };
    Some(PageShape::HeaderHop(Box::new(HeaderHop {
        hop_re,
        pick,
        target,
        header,
        fallback: None,
        per_set: false,
        lit_cap,
        parts,
        mid,
    })))
}

/// A `headers["name"]` read: the lowercase name and
/// the rest.
fn header_name<'a>(s: &'a str, hvar: &str) -> Option<(String, &'a str)> {
    let rest = s.strip_prefix(hvar)?.strip_prefix('[')?;
    let (name, rest) = quoted_token(rest.trim_start())?;
    let rest = rest.trim().strip_prefix(']')?;
    Some((name.to_lowercase(), rest))
}

/// The second-fetch ("hop") bodies (kytea,
/// libxsd-frontend, gnutls, dovecot): pick a value
/// from page one, fetch the page it names, scan
/// that with the block regex. Parsed as a cursor
/// over the space-joined body (statement joining
/// miscounts parens around regex character classes
/// holding a lone quote).
/// The two-assignment section pick (java@beta):
/// `<sec> = page[/section/]`, a guard, then
/// `<v> = <sec>.scan(/lit/).flatten.uniq
/// .map { Version.new }.max`.
fn hopscan_section_prelude(
    body: &str,
    root: &str,
) -> Option<(
    String,
    Option<(String, String)>,
    super::formula::HopPick,
    String,
)> {
    use super::formula::HopPick;
    let (sec, rest) = assign_parts(body.trim())?;
    let rest = rest.trim().strip_prefix(&format!("{root}["))?;
    let (spat, sflags, rest) = regex_literal(rest.trim())?;
    let rest = rest.trim().strip_prefix(']')?.trim();
    let rest = rest
        .strip_prefix(&format!("next unless {sec}"))
        .or_else(|| rest.strip_prefix(&format!("next if {sec}.blank?")))?
        .trim();
    let (var, rest) = assign_parts(rest)?;
    let rest = rest.trim().strip_prefix(&format!("{sec}.scan("))?.trim();
    let (pat, flags, rest) = regex_literal(rest)?;
    let rest = rest.trim().strip_prefix(')')?.trim();
    let rest = rest.strip_prefix(".flatten")?.trim();
    let rest = rest.strip_prefix(".uniq")?.trim();
    let rest = rest.strip_prefix(".map")?.trim_start();
    let (block, rest) = take_bracketed(rest, '{', '}')?;
    let (x, xinner) = json_map_parts(block)?;
    if xinner.trim() != format!("Version.new({x})") {
        return None;
    }
    let rest = rest.trim().strip_prefix(".max")?;
    if rest.starts_with("_by") {
        return None;
    }
    Some((
        var.to_string(),
        Some((pat, flags)),
        HopPick::SectionScanMax {
            section: (spat, sflags),
        },
        rest.to_string(),
    ))
}

fn parse_hopscan_shape(root: &str, lines: &[String]) -> Option<super::formula::PageShape> {
    use super::formula::{HopScan, PageShape};
    let body = lines
        .iter()
        .map(|l| strip_trailing_comment(l.trim()).trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let (var, hop_re, pick, rest_owned);
    let rest = if let Some((v, hre, pk, r)) = hopscan_section_prelude(&body, root) {
        var = v;
        hop_re = hre;
        pick = pk;
        rest_owned = r;
        rest_owned.as_str()
    } else {
        let (v, r) = assign_parts(body.trim())?;
        let (hre, pk, r) = hop_pick(root, r)?;
        var = v;
        hop_re = hre;
        pick = pk;
        r
    };
    // `<var2> = <var>.last` after a sort_by pick.
    let (var, rest) = match assign_parts(rest.trim()) {
        Some((v2, rhs)) if rhs.trim().starts_with(&format!("{var}.last")) => {
            let after = rhs.trim().strip_prefix(&format!("{var}.last"))?;
            (v2, after)
        }
        _ => (var, rest),
    };
    let rest = rest.trim();
    let rest = rest
        .strip_prefix(&format!("next unless {var}"))
        .or_else(|| rest.strip_prefix(&format!("next if {var}.blank?")))?;
    let (wvar, rest) = assign_parts(rest.trim())?;
    let rest = rest
        .trim()
        .strip_prefix("Homebrew::Livecheck::Strategy.page_content")?
        .trim_start();
    let (parens, rest) = take_bracketed(rest, '(', ')')?;
    let slot_var = if matches!(pick, super::formula::HopPick::MatchFirst) {
        format!("{var}[1]")
    } else {
        var.clone()
    };
    let target = hop_target(&slot_var, &parens[1..parens.len() - 1])?;
    let (rest, took_content) = match rest.trim().strip_prefix("[:content]") {
        Some(r) => (r, true),
        None => (rest, false),
    };
    let (content_root, rest) = match hop_content_guard(&wvar, took_content, rest) {
        Some(pair) => pair,
        // No guard: the final scan reads `w[:content]`
        // directly (safe-navigated).
        None if !took_content => (format!("{wvar}[:content]"), rest),
        None => return None,
    };
    let (last_only, final_match, final_scan) = match hop_final(&content_root, rest) {
        Some((last, parts)) => (last, None, parts),
        None => match hop_final_match(&content_root, &var, rest) {
            Some(parts) => (false, Some(parts), None),
            None => {
                // Bare bracket capture final:
                // `<content>[regex, N]` (keychron).
                let r = rest
                    .trim()
                    .strip_prefix(&content_root)?
                    .strip_prefix("[regex,")?
                    .strip_suffix(']')?;
                let n: usize = r.trim().parse().ok()?;
                (
                    false,
                    Some(vec![super::formula::HopFinalPart::Cap(n)]),
                    None,
                )
            }
        },
    };
    Some(PageShape::HopScan(Box::new(HopScan {
        hop_re,
        pick,
        target,
        last_only,
        final_match,
        final_scan,
    })))
}

/// The first-capture date bodies (objconv, vip):
/// `v = page[regex, 1]` or `v = page.match(regex)`,
/// a blank guard, then `Date.parse(v|v[1])` +
/// `strftime` in basic or dashed form.
fn parse_first_date_shape(root: &str, lines: &[String]) -> Option<super::formula::PageShape> {
    use super::formula::PageShape;
    let stmts = block_statements(lines);
    if stmts.len() != 3 {
        return None;
    }
    let (var, rhs) = assign_parts(&stmts[0])?;
    let capture_of = if rhs == format!("{root}[regex, 1]") {
        var.clone()
    } else if rhs == format!("{root}.match(regex)") {
        format!("{var}[1]")
    } else {
        return None;
    };
    if stmts[1] != format!("next if {var}.blank?") {
        return None;
    }
    let rest = stmts[2]
        .trim()
        .strip_prefix(&format!("Date.parse({capture_of})"))?;
    let rest = rest
        .strip_prefix("&.strftime(")
        .or_else(|| rest.strip_prefix(".strftime("))?;
    let (fmt, tail) = quoted_token(rest)?;
    if tail.trim() != ")" {
        return None;
    }
    match fmt.as_str() {
        "%Y%m%d" => Some(PageShape::FirstDate(false)),
        "%Y-%m-%d" => Some(PageShape::FirstDate(true)),
        _ => None,
    }
}

/// The scan-select body (libgweather's `:gnome`
/// block): `.select { |m| Version.new(m.first) < N }.flatten`
/// after the scan call.
fn page_select_shape(cur: &str) -> Option<super::formula::PageShape> {
    use super::formula::PageShape;
    let rest = cur.trim_start().strip_prefix(".select")?;
    let (var, inner) = json_map_parts(rest.strip_suffix(".flatten")?.trim_end())?;
    let inner = inner.trim().strip_prefix("Version.new(")?;
    let inner = inner
        .strip_prefix(&format!("{var}.first"))
        .or_else(|| inner.strip_prefix(&format!("{var}[0]")))?;
    let bound = inner.strip_prefix(") < ")?.trim();
    if bound.is_empty() || !bound.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(PageShape::ScanSelectLt(bound.to_string()))
}

/// The `Date.parse(<capture>)&.strftime("%Y%m%d")`
/// map bodies (pari-*, http_load): the direct form
/// and the assign-then-ternary form. Only the
/// `%Y%m%d` format reduces.
fn page_date_shape(var: &str, inner: &str) -> Option<super::formula::PageShape> {
    fn finish(rest: &str) -> Option<super::formula::PageShape> {
        let rest = rest
            .strip_prefix("&.strftime(")
            .or_else(|| rest.strip_prefix(".strftime("))?;
        let (fmt, tail) = quoted_token(rest)?;
        (fmt == "%Y%m%d" && tail.trim() == ")").then_some(super::formula::PageShape::ScanDateYmd)
    }
    let accessors = [
        format!("{var}&.first"),
        format!("{var}.first"),
        format!("{var}[0]"),
    ];
    if let Some(rest) = inner.strip_prefix("Date.parse(") {
        for acc in &accessors {
            if let Some(r) = rest
                .strip_prefix(acc.as_str())
                .and_then(|r| r.strip_prefix(')'))
            {
                return finish(r);
            }
        }
        return None;
    }
    // `v = match&.first` then `v ? Date.parse(v)&.strftime(..) : nil`.
    let (v, rest) = inner.split_once(" = ")?;
    if v.is_empty() || !v.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    let mut rest = rest;
    for acc in &accessors {
        if let Some(r) = rest.strip_prefix(acc.as_str()) {
            rest = r;
            break;
        }
    }
    let rest = rest
        .trim()
        .strip_prefix(&format!("{v} ? Date.parse({v})"))?;
    finish(rest.strip_suffix(" : nil")?.trim_end())
}

/// One block line with any trailing `#` comment
/// outside double quotes removed.
fn strip_trailing_comment(line: &str) -> &str {
    let mut quoted = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '#' if !quoted && (i == 0 || line[..i].ends_with(' ')) => {
                return line[..i].trim_end();
            }
            _ => {}
        }
    }
    line
}

/// A sparkle item field name as data.
fn sp_field(name: &str) -> Option<super::formula::SpField> {
    use super::formula::SpField;
    match name {
        "short_version" => Some(SpField::Short),
        "version" => Some(SpField::Full),
        "nice_version" => Some(SpField::Nice),
        "title" => Some(SpField::Title),
        "url" => Some(SpField::Url),
        _ => None,
    }
}

/// `<root>.<field>` at the start of `s`: the field
/// and the remaining tail.
fn sp_field_tail<'a>(s: &'a str, root: &str) -> Option<(super::formula::SpField, &'a str)> {
    let rest = s
        .strip_prefix(root)?
        .strip_prefix("&.")
        .or_else(|| s.strip_prefix(root)?.strip_prefix('.'))?;
    let end = rest
        .find(|c: char| !(c.is_ascii_lowercase() || c == '_'))
        .unwrap_or(rest.len());
    Some((sp_field(&rest[..end])?, &rest[end..]))
}

/// A regex field index and the expression following its matching delimiter.
fn sp_index(tail: &str) -> Option<(usize, &str)> {
    let (inner, close) = if let Some(inner) = tail.strip_prefix("[regex,") {
        (inner, ']')
    } else {
        (
            tail.strip_prefix("&.[](regex,")
                .or_else(|| tail.strip_prefix(".[](regex,"))?,
            ')',
        )
    };
    let inner = inner.trim();
    let digits: usize = inner.find(|c: char| !c.is_ascii_digit())?;
    let (n, rest) = inner.split_at(digits);
    Some((n.parse().ok()?, rest.trim_start().strip_prefix(close)?))
}

/// A complete `[regex, N]` / `[](regex, N)` index tail.
fn sp_index_tail(tail: &str) -> Option<usize> {
    let (n, rest) = sp_index(tail)?;
    rest.trim().is_empty().then_some(n)
}

/// `<expr>.tr("a", "b")` arguments.
fn sp_tr_args(tail: &str) -> Option<(String, String)> {
    let rest = tail
        .trim()
        .strip_prefix(".tr(")
        .or_else(|| tail.trim().strip_prefix("&.tr("))?;
    let (a, t) = quoted_token(rest.trim_start())?;
    let t = t.trim().strip_prefix(',')?.trim();
    let (b, _) = quoted_token(t)?;
    Some((a, b))
}

/// The expression forms that read one item field
/// (`item.<field>` plus its tail).
fn sp_field_expr(field: super::formula::SpField, tail: &str) -> Option<super::formula::SpExpr> {
    use super::formula::SpExpr;
    let tail = tail.trim();
    match tail {
        "" => Some(SpExpr::Field(field)),
        ".split.first" | "&.split&.first" | ".split&.first" | "&.split.first" => {
            Some(SpExpr::FieldSplitFirst(field))
        }
        _ => {
            if let Some(rest) = tail
                .strip_prefix(".chars.join(")
                .or_else(|| tail.strip_prefix("&.chars.join("))
                .or_else(|| tail.strip_prefix(".chars&.join("))
            {
                let (sep, t) = quoted_token(rest.trim_start())?;
                if t.trim() != ")" {
                    return None;
                }
                return Some(SpExpr::FieldCharsJoin(field, sep));
            }
            if let Some((a, b)) = sp_tr_args(tail) {
                return Some(SpExpr::FieldTr(field, a, b));
            }
            if let Some(rest) = tail.strip_prefix(".delete_prefix(") {
                let (p, _) = quoted_token(rest.trim_start())?;
                return Some(SpExpr::FieldDeletePrefix(field, p));
            }
            if let Some(rest) = tail.strip_prefix(".delete_suffix(") {
                let (p, _) = quoted_token(rest.trim_start())?;
                return Some(SpExpr::FieldDeleteSuffix(field, p));
            }
            if let Some((n, rest)) = sp_index(tail) {
                if rest.trim().is_empty() {
                    return Some(SpExpr::FieldIndex(field, n));
                }
                let (a, b) = sp_tr_args(rest.trim())?;
                return Some(SpExpr::FieldIndexTr(field, n, a, b));
            }
            None
        }
    }
}

/// `(item.<f>.count("c") >= N) ? item.<f> :
/// "#{item.<f>}<suffix>"` as a CountPad part.
fn sp_count_pad(rhs: &str, receiver: &str) -> Option<super::formula::SpPart> {
    use super::formula::SpPart;
    let inner = rhs.trim().strip_prefix('(')?;
    let (cond, rest) = inner.split_once(") ? ")?;
    let (field, tail) = sp_field_tail(cond.trim(), receiver)?;
    let r = tail.trim().strip_prefix(".count(")?;
    let (needle, r) = quoted_token(r.trim_start())?;
    if needle.is_empty() || needle.contains(['^', '-', '\\']) {
        return None;
    }
    let r = r.trim().strip_prefix(')')?.trim();
    let min: usize = r.strip_prefix(">=")?.trim().parse().ok()?;
    let (then_s, els_s) = rest.split_once(" : ")?;
    let (tf, tt) = sp_field_tail(then_s.trim(), receiver)?;
    if tf != field || !tt.trim().is_empty() {
        return None;
    }
    let (body_str, t) = quoted_token(els_s.trim())?;
    if !t.trim().is_empty() {
        return None;
    }
    let parts = sp_template_parts(&body_str, receiver, "match", None, None)?;
    let [SpPart::Field(f), SpPart::Lit(suffix)] = parts.as_slice() else {
        return None;
    };
    if *f != field {
        return None;
    }
    Some(SpPart::CountPad {
        field,
        needle,
        min,
        suffix: suffix.clone(),
    })
}

/// Parse a `"#{...}"` template body into parts.
/// `match_var`/`local_name` are the body's capture
/// and precomputed local names. The item receiver is parsed only inside
/// expressions; literal text is never renamed.
fn sp_template_parts(
    body: &str,
    receiver: &str,
    match_var: &str,
    local_name: Option<&str>,
    pad: Option<(&str, &super::formula::SpPart)>,
) -> Option<Vec<super::formula::SpPart>> {
    use super::formula::SpPart;
    // This byte belongs only to ruby_unescape_dq's escaped-interpolation sentinel.
    if body.contains('\u{3}') {
        return None;
    }
    let literal = |text: &str| SpPart::Lit(ruby_unescape_dq(text).replace('\u{3}', "#{"));
    let mut parts = Vec::new();
    let mut rest = body;
    while let Some((i, _)) = rest.match_indices("#{").find(|(i, _)| {
        rest[..*i]
            .bytes()
            .rev()
            .take_while(|byte| *byte == b'\\')
            .count()
            % 2
            == 0
    }) {
        if i > 0 {
            parts.push(literal(&rest[..i]));
        }
        let j = rest[i + 2..].find('}')?;
        let token = rest[i + 2..i + 2 + j].trim();
        let part = if Some(token) == local_name {
            SpPart::Local
        } else if let Some((pad_name, pad_part)) = pad
            && token == pad_name
        {
            pad_part.clone()
        } else if let Some(r) = token.strip_prefix(&format!("{match_var}[")) {
            let close = r.find(']')?;
            let n: usize = r[..close].parse().ok()?;
            let tail = r[close + 1..].trim();
            match tail {
                "" => SpPart::Cap(n),
                _ => {
                    if let Some((a, b)) = sp_tr_args(tail) {
                        SpPart::CapTr(n, a, b)
                    } else {
                        let fb = tail.strip_prefix(".presence ||")?;
                        let (s, _) = quoted_token(fb.trim())?;
                        SpPart::CapOr(n, s)
                    }
                }
            }
        } else if let Some((field, tail)) = sp_field_tail(token, receiver) {
            let tail = tail.trim();
            if tail.is_empty() {
                SpPart::Field(field)
            } else if let Some(rest) = tail
                .strip_prefix(".split(")
                .or_else(|| tail.strip_prefix("&.split("))
            {
                let (sep, t) = quoted_token(rest.trim_start())?;
                let t = t.trim().strip_prefix(')')?;
                let last = match t.trim() {
                    ".first" | "&.first" => false,
                    ".last" | "&.last" => true,
                    _ => return None,
                };
                if sep.contains('\\') {
                    return None;
                }
                SpPart::FieldSplit(field, sep, last)
            } else if let Some(rest) = tail
                .strip_prefix(".sub(")
                .or_else(|| tail.strip_prefix("&.sub("))
            {
                let (a, t) = quoted_token(rest.trim_start())?;
                let t = t.trim().strip_prefix(',')?.trim();
                let (b, t) = quoted_token(t)?;
                if t.trim() != ")" {
                    return None;
                }
                SpPart::FieldSub(field, a, b)
            } else {
                SpPart::FieldIndex(field, sp_index_tail(tail)?)
            }
        } else {
            return None;
        };
        parts.push(part);
        rest = &rest[i + 2 + j + 1..];
    }
    if !rest.is_empty() {
        parts.push(literal(rest));
    }
    Some(parts)
}

/// A channel condition (`item.channel.nil?`,
/// `== "x"`, `!= "x"`) as a keep-gate; `negated`
/// flips it (a `next if` skip vs a keep test).
fn sp_channel_gate(cond: &str, receiver: &str, negated: bool) -> Option<super::formula::SpChannel> {
    use super::formula::SpChannel;
    let rest = cond
        .trim()
        .strip_prefix(&format!("{receiver}.channel"))?
        .trim();
    match rest {
        ".nil?" => {
            if negated {
                None
            } else {
                Some(SpChannel::Nil)
            }
        }
        _ => {
            let (is_eq, val) = if let Some(v) = rest.strip_prefix("==") {
                (true, v)
            } else {
                let v = rest.strip_prefix("!=")?;
                (false, v)
            };
            let (s, _) = quoted_token(val.trim())?;
            Some(if is_eq != negated {
                SpChannel::Eq(s)
            } else {
                SpChannel::Ne(s)
            })
        }
    }
}

/// Reduce a `:sparkle` strategy block to a data
/// shape: field reads, channel gates, regex
/// captures from a field, and interpolation
/// templates (the forms the cask tap enumerates).
/// Anything conditional beyond a presence guard
/// stays block-only.
fn parse_sparkle_shape(params: &str, lines: &[String]) -> Option<super::formula::SparkleBlock> {
    let (root, _has_regex) = block_params(params)?;
    let stmts: Vec<String> = lines
        .iter()
        .map(|l| strip_trailing_comment(l.trim()).trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    match root.as_str() {
        "item" => sp_blank_own_body(&stmts)
            .or_else(|| sp_dotless_fork_body(&stmts))
            .or_else(|| sp_item_body(&stmts, "item", false, None, None)),
        "items" => sp_items_body(&stmts),
        _ => None,
    }
}

/// A bare sparkle render expression as template
/// parts (a capture, a field, or a quoted template).
fn sp_bare_parts(
    expr: &str,
    receiver: &str,
    match_var: &str,
    local_name: Option<&str>,
    pad: Option<(&str, &super::formula::SpPart)>,
) -> Option<Vec<super::formula::SpPart>> {
    use super::formula::SpPart;
    let expr = expr.trim();
    if expr.starts_with('"') {
        let (body_str, tail) = quoted_token(expr)?;
        if !tail.trim().is_empty() {
            return None;
        }
        return sp_template_parts(&body_str, receiver, match_var, local_name, pad);
    }
    if let Some(r) = expr.strip_prefix(&format!("{match_var}[")) {
        let close = r.find(']')?;
        if !r[close + 1..].trim().is_empty() {
            return None;
        }
        return Some(vec![SpPart::Cap(r[..close].parse().ok()?)]);
    }
    let (field, tail) = sp_field_tail(expr, receiver)?;
    if !tail.trim().is_empty() {
        return None;
    }
    Some(vec![SpPart::Field(field)])
}

/// One side of a sparkle `(a == b)` condition.
fn sp_op(tok: &str, receiver: &str, match_var: &str) -> Option<super::formula::SpOp> {
    use super::formula::SpOp;
    let tok = tok.trim();
    if let Some(r) = tok.strip_prefix(&format!("{match_var}[")) {
        let inner = r.strip_suffix(']')?;
        return Some(SpOp::Cap(inner.parse().ok()?));
    }
    let (field, tail) = sp_field_tail(tok, receiver)?;
    if !tail.trim().is_empty() {
        return None;
    }
    Some(SpOp::Field(field))
}

/// A `find` condition as (channel gate, match guard):
/// channel tests (incl. the prisma assignment form),
/// a field regex test, or both joined by `&&`.
fn sp_find_gates(
    cond: &str,
) -> Option<(
    Option<super::formula::SpChannel>,
    Option<super::formula::SpField>,
)> {
    use super::formula::SpChannel;
    let cond = cond.trim();
    // `(item_channel = item.channel) == "x" ||
    // item_channel.nil?` and the direct spelling.
    if let Some((lhs, rhs)) = cond.split_once(" || ") {
        let value_of = |side: &str| -> Option<String> {
            let (_, v) = side.trim().split_once(" == ")?;
            quoted_token(v.trim()).map(|(s, _)| s)
        };
        let is_nil = |side: &str| side.trim().ends_with(".nil?");
        let mentions_channel =
            |side: &str| side.contains("item.channel") || side.contains("_channel");
        if mentions_channel(lhs)
            && is_nil(rhs)
            && let Some(v) = value_of(lhs)
        {
            return Some((Some(SpChannel::EqOrNil(v)), None));
        }
        if mentions_channel(rhs)
            && is_nil(lhs)
            && let Some(v) = value_of(rhs)
        {
            return Some((Some(SpChannel::EqOrNil(v)), None));
        }
        return None;
    }
    if let Some((a, b)) = cond.split_once(" && ") {
        let (ca, ma) = sp_find_gates(a)?;
        let (cb, mb) = sp_find_gates(b)?;
        return Some((ca.or(cb), ma.or(mb)));
    }
    if cond.contains("item.channel") {
        return Some((Some(sp_channel_gate(cond, "item", false)?), None));
    }
    let (field, tail) = sp_field_tail(cond, "item")?;
    if tail.trim() == "&.match(regex)" || tail.trim() == ".match(regex)" {
        return Some((None, Some(field)));
    }
    None
}

/// `<name> = items.find { |item| cond }&.<field>`,
/// a literal-regex match over that field value, a
/// blank guard, and a capture template
/// (plistedit-pro).
fn sp_find_litmatch_body(stmts: &[String]) -> Option<super::formula::SparkleBlock> {
    use super::formula::{SpExpr, SparkleBlock};
    if stmts.len() != 4 {
        return None;
    }
    let (name, rhs) = stmts[0].split_once(" = ")?;
    let name = name.trim();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let rest = rhs.trim().strip_prefix("items.find { |item|")?;
    let (cond, tail) = rest.split_once('}')?;
    let (channel, guard) = sp_find_gates(cond)?;
    let field = sp_field(tail.trim().strip_prefix("&.")?.trim())?;
    let (mv, rhs) = stmts[1].split_once(" = ")?;
    let mv = mv.trim();
    let rhs = rhs
        .trim()
        .strip_prefix(name)?
        .trim_start()
        .strip_prefix("&.match(")
        .or_else(|| {
            rhs.trim()
                .strip_prefix(name)?
                .trim_start()
                .strip_prefix(".match(")
        })?;
    let (pat, flags, tail) = regex_literal(rhs.trim())?;
    if tail.trim() != ")" {
        return None;
    }
    if stmts[2].trim() != format!("next if {mv}.blank?") {
        return None;
    }
    let (body_str, tail) = quoted_token(stmts[3].trim())?;
    if !tail.trim().is_empty() {
        return None;
    }
    let parts = sp_template_parts(&body_str, "item", mv, None, None)?;
    Some(SparkleBlock {
        all: false,
        find: true,
        channel,
        match_guard: guard,
        present: Vec::new(),
        needs_spec_regex: guard.is_some(),
        own_if: None,
        expr: SpExpr::LitMatch {
            field,
            pat,
            flags,
            parts,
        },
    })
}

/// `versions = items.filter_map { Version.new(field) }
/// .uniq.sort` then a stateful patch-bucket throttle
/// map (commander).
fn sp_throttle_uniq_body(stmts: &[String]) -> Option<super::formula::SparkleBlock> {
    use super::formula::{SpExpr, SparkleBlock};
    if stmts.len() != 13 {
        return None;
    }
    let ident = |s: &str| -> Option<String> {
        let s = s.trim();
        if s.is_empty() || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
        Some(s.to_string())
    };
    let (vlist, rhs) = stmts[0].split_once(" = ")?;
    let vlist = ident(vlist)?;
    let item_var = ident(
        rhs.trim()
            .strip_prefix("items.filter_map do |")?
            .strip_suffix('|')?,
    )?;
    let (sv, rhs) = stmts[1].split_once(" = ")?;
    let sv = ident(sv)?;
    let (field, tail) = sp_field_tail(rhs.trim(), &item_var)?;
    if !tail.trim().is_empty() || stmts[2].trim() != format!("next unless {sv}") {
        return None;
    }
    if stmts[3].trim() != format!("Version.new({sv})") || stmts[4].trim() != "end.uniq.sort" {
        return None;
    }
    let (tp, rhs) = stmts[5].split_once(" = ")?;
    let tp = ident(tp)?;
    if rhs.trim() != "nil" {
        return None;
    }
    let sv2 = ident(
        stmts[6]
            .trim()
            .strip_prefix(&format!("{vlist}.map do |"))?
            .strip_suffix('|')?,
    )?;
    let (pvar, rhs) = stmts[7].split_once(" = ")?;
    let pvar = ident(pvar)?;
    if rhs.trim() != format!("{sv2}.patch.to_i") {
        return None;
    }
    let (cur, rhs) = stmts[8].split_once(" = ")?;
    let cur = ident(cur)?;
    let bucket_s = rhs
        .trim()
        .strip_prefix(&format!("{pvar} - ({pvar} % "))?
        .strip_suffix(')')?;
    let bucket: i64 = bucket_s.trim().parse().ok()?;
    if bucket <= 0
        || stmts[9].trim() != format!("next if {cur} == {tp}")
        || stmts[10].trim() != format!("{tp} = {cur}")
        || stmts[11].trim() != sv2
        || stmts[12].trim() != "end"
    {
        return None;
    }
    Some(SparkleBlock {
        all: true,
        find: false,
        channel: None,
        match_guard: None,
        present: Vec::new(),
        needs_spec_regex: false,
        own_if: None,
        expr: SpExpr::ThrottleUniq { field, bucket },
    })
}

/// A blank match yields the own version when a field
/// equals its first CSV part, else a bare render
/// (soundsource@test).
fn sp_blank_own_body(stmts: &[String]) -> Option<super::formula::SparkleBlock> {
    use super::formula::{SpExpr, SparkleBlock};
    if stmts.len() != 6 {
        return None;
    }
    let (mv, rhs) = stmts[0].split_once(" = ")?;
    let mv = mv.trim();
    let (from, tail) = sp_field_tail(rhs.trim(), "item")?;
    if !matches!(tail.trim(), "&.match(regex)" | ".match(regex)") {
        return None;
    }
    if stmts[1].trim() != format!("if {mv}.blank?") {
        return None;
    }
    let cond = stmts[2].trim().strip_prefix("next version if ")?;
    let (field, tail) = sp_field_tail(cond.trim(), "item")?;
    if tail.trim() != "== version.csv.first" {
        return None;
    }
    let blank = sp_bare_parts(
        stmts[3].trim().strip_prefix("next ")?,
        "item",
        mv,
        None,
        None,
    )?;
    if stmts[4].trim() != "end" {
        return None;
    }
    let (body_str, tail) = quoted_token(stmts[5].trim())?;
    if !tail.trim().is_empty() {
        return None;
    }
    let parts = sp_template_parts(&body_str, "item", mv, None, None)?;
    Some(SparkleBlock {
        all: false,
        find: false,
        channel: None,
        match_guard: None,
        present: Vec::new(),
        needs_spec_regex: true,
        own_if: None,
        expr: SpExpr::BlankOwnElse {
            from,
            field,
            blank,
            parts,
        },
    })
}

/// The dotless short-version vs dotless filename
/// fork (moom).
fn sp_dotless_fork_body(stmts: &[String]) -> Option<super::formula::SparkleBlock> {
    use super::formula::{SpExpr, SparkleBlock};
    if stmts.len() != 11 {
        return None;
    }
    let (a_name, rhs) = stmts[0].split_once(" = ")?;
    let a_name = a_name.trim();
    let (short, tail) = sp_field_tail(rhs.trim(), "item")?;
    if sp_tr_args(tail) != Some((".".to_string(), String::new())) {
        return None;
    }
    let short_src = rhs
        .trim()
        .strip_prefix("item.")?
        .split(".tr(")
        .next()?
        .trim();
    if stmts[1].trim() != format!("next if {a_name}.blank?") {
        return None;
    }
    let (b_name, rhs) = stmts[2].split_once(" = ")?;
    let b_name = b_name.trim();
    let (url, tail) = sp_field_tail(rhs.trim(), "item")?;
    let tail = tail.trim();
    let rest = tail.strip_prefix("[regex,")?;
    let (cap_s, rest) = rest.split_once(']')?;
    let cap: usize = cap_s.trim().parse().ok()?;
    if sp_tr_args(rest) != Some((".".to_string(), String::new())) {
        return None;
    }
    if stmts[3].trim() != format!("next if {b_name}.blank?")
        || stmts[4].trim() != format!("if {a_name} == {b_name}")
    {
        return None;
    }
    let (f5, tail) = sp_field_tail(stmts[5].trim(), "item")?;
    if f5 != short || !tail.trim().is_empty() {
        return None;
    }
    if stmts[6].trim() != format!("elsif {a_name}.length < {b_name}.length") {
        return None;
    }
    let mut want = String::from("\"#{item.");
    want.push_str(short_src);
    want.push_str("}.#{");
    want.push_str(b_name);
    want.push_str(".delete_prefix(");
    want.push_str(a_name);
    want.push_str(")}\"");
    if stmts[7].trim() != want
        || stmts[8].trim() != "else"
        || stmts[9].trim() != "next"
        || stmts[10].trim() != "end"
    {
        return None;
    }
    Some(SparkleBlock {
        all: false,
        find: false,
        channel: None,
        match_guard: None,
        present: Vec::new(),
        needs_spec_regex: true,
        own_if: None,
        expr: SpExpr::DotlessFork { short, url, cap },
    })
}

/// The `|items|` body: whole-array `map`/`find`
/// wrappers around an item body.
fn sp_items_body(stmts: &[String]) -> Option<super::formula::SparkleBlock> {
    use super::formula::{SpExpr, SparkleBlock};
    if let Some(blk) = sp_find_litmatch_body(stmts) {
        return Some(blk);
    }
    if let Some(blk) = sp_throttle_uniq_body(stmts) {
        return Some(blk);
    }
    let first = stmts.first()?;
    if stmts.len() == 1 {
        if let Some(rest) = first.strip_prefix("items.map(&:") {
            let name = rest.strip_suffix(')')?;
            return Some(SparkleBlock {
                all: true,
                find: false,
                channel: None,
                match_guard: None,
                present: Vec::new(),
                needs_spec_regex: false,
                own_if: None,
                expr: SpExpr::Field(sp_field(name)?),
            });
        }
        if let Some(rest) = first.strip_prefix("items.map { |item|") {
            let inner = rest.trim().strip_suffix('}')?.trim();
            return sp_item_body(&[inner.to_string()], "item", true, None, None);
        }
        if let Some(rest) = first.strip_prefix("items.find { |item|") {
            let (cond, tail) = rest.split_once('}')?;
            let (channel, match_guard) = sp_find_gates(cond)?;
            // `&.<field>` with an optional field-expr
            // tail (`&.url&.[](regex, 1)`).
            let tail = tail.trim().strip_prefix("&.")?;
            let synthetic = format!("item.{tail}");
            let (field, ftail) = sp_field_tail(&synthetic, "item")?;
            let expr = sp_field_expr(field, ftail)?;
            let needs_spec_regex = match_guard.is_some()
                || matches!(expr, SpExpr::FieldIndex(..) | SpExpr::CapturesJoin(_));
            return Some(SparkleBlock {
                all: false,
                find: true,
                channel,
                match_guard,
                present: Vec::new(),
                needs_spec_regex,
                own_if: None,
                expr,
            });
        }
        return None;
    }
    // Assign-find naming the item `item` (the rest of
    // the body then reads it as a single-item body).
    if let Some(rest) = first.strip_prefix("item = items.find do |item|") {
        if !rest.trim().is_empty() || stmts.get(2).map(String::as_str) != Some("end") {
            return None;
        }
        let (channel, guard) = sp_find_gates(&stmts[1])?;
        let mut blk = sp_item_body(&stmts[3..], "item", false, channel, guard)?;
        blk.find = true;
        if guard.is_some() {
            blk.match_guard = guard;
            blk.needs_spec_regex = true;
        }
        return Some(blk);
    }
    if let Some(rest) = first.strip_prefix("item = items.find { |item|") {
        let (cond, tail) = rest.split_once('}')?;
        if !tail.trim().is_empty() {
            return None;
        }
        let (channel, guard) = sp_find_gates(cond)?;
        let mut blk = sp_item_body(&stmts[1..], "item", false, channel, guard)?;
        blk.find = true;
        if guard.is_some() {
            blk.match_guard = guard;
            blk.needs_spec_regex = true;
        }
        return Some(blk);
    }
    // Parse the selected receiver by name. Its spelling in literal text is data.
    if let Some((name, rhs)) = first.split_once(" = ")
        && let Some(rest) = rhs.trim().strip_prefix("items.find { |item|")
    {
        let name = name.trim();
        let (cond, tail) = rest.split_once('}')?;
        if name.is_empty()
            || name == "item"
            || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            || !tail.trim().is_empty()
        {
            return None;
        }
        let (channel, guard) = sp_find_gates(cond)?;
        let mut blk = sp_item_body(&stmts[1..], name, false, channel, guard)?;
        blk.find = true;
        if guard.is_some() {
            blk.match_guard = guard;
            blk.needs_spec_regex = true;
        }
        return Some(blk);
    }
    let opens_map =
        first.starts_with("items.map do |item|") || first.starts_with("items.filter_map do |item|");
    if opens_map && stmts.last().map(String::as_str) == Some("end") {
        return sp_item_body(&stmts[1..stmts.len() - 1], "item", true, None, None);
    }
    None
}

/// The per-item body. `all` is the `|items|`
/// fan-out (results compact/uniq); single-item
/// bodies evaluate the newest matching item only.
fn sp_item_body(
    stmts: &[String],
    receiver: &str,
    all: bool,
    preset_channel: Option<super::formula::SpChannel>,
    preset_match_guard: Option<super::formula::SpField>,
) -> Option<super::formula::SparkleBlock> {
    use super::formula::{SpExpr, SpPart, SparkleBlock};
    let mut channel = preset_channel;
    let mut match_guard = preset_match_guard;
    let mut present: Vec<super::formula::SpField> = Vec::new();
    let mut own_if: Option<(super::formula::SpField, String)> = None;
    let mut pad_name: Option<String> = None;
    let mut pad_part: Option<SpPart> = None;
    // An early `next <expr> if/unless <cond>` render.
    let mut early: Option<(bool, Option<usize>, Vec<SpPart>)> = None;
    let mut early_cap_blank: Option<(usize, Vec<SpPart>)> = None;
    let mut from: Option<super::formula::SpField> = None;
    let mut local: Option<(super::formula::SpField, usize)> = None;
    let mut local_name: Option<String> = None;
    let mut match_var = "match".to_string();
    let mut guarded = false;
    let mut i = 0usize;
    // A scan expression may be the final statement
    // (one line, or `map do` + template + `end`).
    let scan_at = stmts.iter().position(|s| s.contains(".scan(regex)"));
    let expr_end = scan_at.unwrap_or(stmts.len().saturating_sub(1));
    while i < expr_end {
        let stmt = stmts[i].as_str();
        i += 1;
        if let Some(cond) = stmt.strip_prefix(&format!("next if {receiver}.channel")) {
            channel = Some(sp_channel_gate(
                &format!("{receiver}.channel{cond}"),
                receiver,
                true,
            )?);
        } else if let Some(cond) = stmt.strip_prefix(&format!("next unless {receiver}.channel")) {
            channel = Some(sp_channel_gate(
                &format!("{receiver}.channel{cond}"),
                receiver,
                false,
            )?);
        } else if stmt == format!("next unless {receiver}")
            || stmt == format!("next if {receiver}.nil?")
        {
            // The assign-find miss: the evaluator's
            // gated pick is already empty then.
            if channel.is_none() && match_guard.is_none() {
                return None;
            }
        } else if let Some(cond) = stmt.strip_prefix("next if ") {
            // Blank/present guards, possibly several
            // joined by `||`.
            let mut ok = true;
            for part in cond.split(" || ") {
                let part = part.trim();
                if part == format!("{match_var}.blank?")
                    || part
                        .strip_suffix(".blank?")
                        .is_some_and(|n| Some(n) == local_name.as_deref())
                {
                    guarded = true;
                } else if let Some(f) = part
                    .strip_prefix('!')
                    .and_then(|r| sp_field_tail(r.trim(), receiver))
                    .filter(|(_, tail)| tail.trim().is_empty())
                    .map(|(f, _)| f)
                    .or_else(|| {
                        sp_field_tail(part, receiver)
                            .filter(|(_, tail)| {
                                matches!(tail.trim(), ".blank?" | ".nil?" | "&.blank?")
                            })
                            .map(|(f, _)| f)
                    })
                {
                    if !present.contains(&f) {
                        present.push(f);
                    }
                } else {
                    ok = false;
                }
            }
            if !ok {
                return None;
            }
        } else if let Some(cond) = stmt.strip_prefix("next version if ") {
            // `next version if item.<f> == "lit"`.
            if own_if.is_some() {
                return None;
            }
            let (field, tail) = sp_field_tail(cond.trim(), receiver)?;
            let r = tail.trim().strip_prefix("== ")?;
            let (lit, t) = quoted_token(r.trim())?;
            if !t.trim().is_empty() {
                return None;
            }
            own_if = Some((field, lit));
        } else if let Some(early_stmt) = stmt.strip_prefix("next ")
            && (early_stmt.contains(" if ") || early_stmt.contains(" unless "))
        {
            // `next <expr> if match.blank?` /
            // `next <expr> unless match[n]`.
            if early.is_some() {
                return None;
            }
            let (expr, cond, unless) = if let Some((e, c)) = early_stmt.rsplit_once(" unless ") {
                (e, c, true)
            } else if let Some((e, c)) = early_stmt.rsplit_once(" if ") {
                (e, c, false)
            } else {
                return None;
            };
            let cond = cond.trim();
            // `next <expr> if <mv>.nil? || <mv>[n].blank?`
            if !unless
                && let Some((l, r)) = cond.split_once(" || ")
                && l.trim() == format!("{match_var}.nil?")
                && let Some(n) = r
                    .trim()
                    .strip_prefix(&format!("{match_var}["))
                    .and_then(|x| x.strip_suffix("].blank?"))
                    .and_then(|x| x.parse::<usize>().ok())
            {
                if early_cap_blank.is_some() || early.is_some() {
                    return None;
                }
                let pad_ref = match (&pad_name, &pad_part) {
                    (Some(n), Some(p)) => Some((n.as_str(), p)),
                    _ => None,
                };
                early_cap_blank = Some((
                    n,
                    sp_bare_parts(expr, receiver, &match_var, local_name.as_deref(), pad_ref)?,
                ));
                guarded = true;
                continue;
            }
            let cap = if cond == format!("{match_var}.blank?") {
                if unless {
                    return None;
                }
                None
            } else {
                let r = cond
                    .strip_prefix(&format!("{match_var}["))
                    .and_then(|r| r.strip_suffix(']'))?;
                Some(r.parse().ok()?)
            };
            let pad_here = match (&pad_name, &pad_part) {
                (Some(n), Some(p)) => Some((n.as_str(), p)),
                _ => None,
            };
            early = Some((
                unless,
                cap,
                sp_bare_parts(expr, receiver, &match_var, local_name.as_deref(), pad_here)?,
            ));
            guarded = true;
        } else if let Some(cond) = stmt.strip_prefix("next unless ") {
            if cond == match_var {
                guarded = true;
            } else if let Some((field, tail)) =
                sp_field_tail(cond, receiver).filter(|_| cond.ends_with("match?(regex)"))
            {
                if tail != "&.match?(regex)" && tail != ".match?(regex)" {
                    return None;
                }
                match_guard = Some(field);
            } else {
                return None;
            }
        } else if let Some((var, rhs)) = stmt.split_once(" = ") {
            if let Some(pad) = sp_count_pad(rhs.trim(), receiver) {
                if pad_part.is_some() {
                    return None;
                }
                pad_name = Some(var.trim().to_string());
                pad_part = Some(pad);
                continue;
            }
            let (field, tail) = sp_field_tail(rhs.trim(), receiver)?;
            let tail = tail.trim();
            if tail == "&.match(regex)" || tail == ".match(regex)" {
                match_var = var.trim().to_string();
                from = Some(field);
            } else {
                let n = sp_index_tail(tail)?;
                local_name = Some(var.trim().to_string());
                local = Some((field, n));
            }
        } else {
            return None;
        }
    }
    let pad_ref = match (&pad_name, &pad_part) {
        (Some(n), Some(p)) => Some((n.as_str(), p)),
        _ => None,
    };
    let expr = if let Some(at) = scan_at {
        let stmt = &stmts[at];
        let (field, tail) = sp_field_tail(stmt, receiver)?;
        let after = tail
            .trim()
            .strip_prefix(".scan(regex).map")
            .or_else(|| tail.trim().strip_prefix(".scan(regex)&.map"))?
            .trim();
        // `do |match| if a == b … else … end end`
        let scan_eq = if after.starts_with("do |match|")
            && stmts.len() == at + 7
            && stmts[at + 3].trim() == "else"
            && stmts[at + 5].trim() == "end"
            && stmts[at + 6].trim() == "end"
            && let Some(cond) = stmts[at + 1].trim().strip_prefix("if ")
            && let Some((a, b)) = cond.split_once(" == ")
        {
            Some(SpExpr::ScanEqElse {
                from: field,
                a: sp_op(a, receiver, "match")?,
                b: sp_op(b, receiver, "match")?,
                then: sp_bare_parts(&stmts[at + 2], receiver, "match", None, None)?,
                els: sp_bare_parts(&stmts[at + 4], receiver, "match", None, None)?,
            })
        } else {
            None
        };
        if let Some(eq) = scan_eq {
            eq
        } else {
            let template = if let Some(inner) = after.strip_prefix("{ |match|") {
                inner.trim().strip_suffix('}')?.trim().to_string()
            } else if after.starts_with("do |match|") {
                if stmts.last().map(String::as_str) != Some("end") || at + 2 != stmts.len() - 1 {
                    return None;
                }
                stmts[at + 1].clone()
            } else {
                return None;
            };
            let parts = if template.trim().starts_with('"') {
                let (body_str, tail2) = quoted_token(template.trim())?;
                if !tail2.trim().is_empty() {
                    return None;
                }
                sp_template_parts(&body_str, receiver, "match", None, None)?
            } else {
                let r = template.trim().strip_prefix("match[")?;
                // A bare capture read: `match[0].tr("-", ".")`.
                let close = r.find(']')?;
                let n: usize = r[..close].parse().ok()?;
                let tail = r[close + 1..].trim();
                if tail.is_empty() {
                    vec![SpPart::Cap(n)]
                } else {
                    let (a, b) = sp_tr_args(tail)?;
                    vec![SpPart::CapTr(n, a, b)]
                }
            };
            SpExpr::Scan { from: field, parts }
        }
    } else {
        let stmt = stmts.get(expr_end)?.as_str();
        if stmt == format!("{match_var}.captures.join(\",\")") {
            SpExpr::CapturesJoin(from?)
        } else if let Some(cond) = stmt.strip_prefix('(')
            && let Some((a, rest)) = cond.split_once(" == ")
            && let Some((b, rest)) = rest.split_once(") ? ")
        {
            // `(a == b) ? X : Y` over caps/fields.
            if !guarded || from.is_none() {
                return None;
            }
            let (then_parts, els_str) = if rest.starts_with('"') {
                let (_, tail) = quoted_token(rest)?;
                let used = rest.len() - tail.len();
                (
                    sp_bare_parts(
                        &rest[..used],
                        receiver,
                        &match_var,
                        local_name.as_deref(),
                        pad_ref,
                    )?,
                    tail.trim().strip_prefix(':')?.trim(),
                )
            } else {
                let (then, els) = rest.split_once(" : ")?;
                (
                    sp_bare_parts(then, receiver, &match_var, local_name.as_deref(), pad_ref)?,
                    els,
                )
            };
            SpExpr::EqElse {
                from,
                local,
                a: sp_op(a, receiver, &match_var)?,
                b: sp_op(b, receiver, &match_var)?,
                then: then_parts,
                els: sp_bare_parts(
                    els_str,
                    receiver,
                    &match_var,
                    local_name.as_deref(),
                    pad_ref,
                )?,
            }
        } else if let Some(r) = stmt.strip_prefix(&format!("{match_var}[")) {
            if !guarded {
                return None;
            }
            let close = r.find(']')?;
            SpExpr::Template {
                from,
                local: None,
                parts: vec![SpPart::Cap(r[..close].parse().ok()?)],
            }
        } else if stmt.starts_with('"') {
            let (body_str, tail) = quoted_token(stmt)?;
            if !tail.trim().is_empty() {
                return None;
            }
            let parts = sp_template_parts(
                &body_str,
                receiver,
                &match_var,
                local_name.as_deref(),
                pad_ref,
            )?;
            let uses_cap = parts
                .iter()
                .any(|p| matches!(p, SpPart::Cap(_) | SpPart::CapTr(..) | SpPart::CapOr(..)));
            let uses_local = parts.iter().any(|p| matches!(p, SpPart::Local));
            if (uses_cap && (from.is_none() || !guarded))
                || (uses_local && (local.is_none() || !guarded))
            {
                return None;
            }
            SpExpr::Template {
                from: if uses_cap { from } else { None },
                local: if uses_local { local } else { None },
                parts,
            }
        } else if let Some((field, tail)) = sp_field_tail(stmt, receiver) {
            sp_field_expr(field, tail)?
        } else {
            return None;
        }
    };
    // An early `next` wraps a template final form.
    let expr = match early_cap_blank {
        Some((cap, blank)) => match expr {
            SpExpr::Template { from, parts, .. } => SpExpr::CapBlankElse {
                from,
                cap,
                blank,
                parts,
            },
            _ => return None,
        },
        None => expr,
    };
    let expr = match early {
        None => expr,
        Some((unless, cap, early_parts)) => match expr {
            SpExpr::Template { from, parts, .. } => match cap {
                None => SpExpr::BlankElse {
                    from,
                    blank: early_parts,
                    parts,
                },
                Some(n) => SpExpr::CapElse {
                    from,
                    cap: n,
                    invert: unless,
                    then: early_parts,
                    els: parts,
                },
            },
            _ => return None,
        },
    };
    let needs_spec_regex = match_guard.is_some()
        || match &expr {
            SpExpr::Field(_) | SpExpr::FieldSplitFirst(_) | SpExpr::FieldCharsJoin(..) => false,
            SpExpr::FieldTr(..) | SpExpr::FieldDeletePrefix(..) | SpExpr::FieldDeleteSuffix(..) => {
                false
            }
            SpExpr::Template { from, local, parts } => {
                from.is_some()
                    || local.is_some()
                    || parts.iter().any(|p| matches!(p, SpPart::FieldIndex(..)))
            }
            SpExpr::EqElse {
                from,
                local,
                then,
                els,
                ..
            } => {
                from.is_some()
                    || local.is_some()
                    || then
                        .iter()
                        .chain(els.iter())
                        .any(|p| matches!(p, SpPart::FieldIndex(..)))
            }
            SpExpr::CapElse { from, .. } | SpExpr::BlankElse { from, .. } => from.is_some(),
            _ => true,
        };
    Some(SparkleBlock {
        all,
        find: false,
        channel,
        match_guard,
        present,
        needs_spec_regex,
        own_if,
        expr,
    })
}

/// Reduce a `:github_releases` strategy block to a
/// data shape. The tap grammar is one statement per
/// line: `json.map do |release|`, draft/prerelease
/// and name guards, a `match = …&.match(regex)`
/// capture from the tag (or each asset's download
/// URL with a trailing `end.flatten`), `next if
/// match.blank?`, and a result expression
/// (`match[1]`, with `tr`, or a `Version` kept only
/// when its patch is non-zero).
/// One side of a ghrel `(a == b)` render condition.
fn ghrel_operand(
    tok: &str,
    mvar: Option<&str>,
    local: Option<&str>,
    rel: &str,
    tag_alias: Option<&str>,
) -> Option<super::formula::GhRelOp> {
    use super::formula::GhRelOp;
    let tok = tok.trim();
    if Some(tok) == local {
        return Some(GhRelOp::Local);
    }
    if tok == format!("{rel}[\"tag_name\"]") || Some(tok) == tag_alias {
        return Some(GhRelOp::Tag);
    }
    let mv = mvar?;
    let inner = tok.strip_prefix(&format!("{mv}["))?.strip_suffix(']')?;
    Some(GhRelOp::Cap(inner.parse().ok()?))
}

/// A ghrel render expression (bare operand, tag
/// capture, or interpolation string) as parts.
fn ghrel_parts(
    expr: &str,
    mvar: Option<&str>,
    local: Option<&str>,
    rel: &str,
    tag_alias: Option<&str>,
) -> Option<Vec<super::formula::GhRelPart>> {
    use super::formula::{GhRelOp, GhRelPart};
    let expr = expr.trim();
    if let Some(op) = ghrel_operand(expr, mvar, local, rel, tag_alias) {
        return Some(vec![match op {
            GhRelOp::Cap(n) => GhRelPart::Cap(n),
            GhRelOp::Local => GhRelPart::Local,
            GhRelOp::Tag => GhRelPart::Tag,
        }]);
    }
    // `release["tag_name"]&.[](/lit/flags, 1)` (or
    // the `[/lit/, 1]` bracket spelling).
    if let Some(rest) = expr
        .strip_prefix(&format!("{rel}[\"tag_name\"]&.[]("))
        .or_else(|| expr.strip_prefix(&format!("{rel}[\"tag_name\"].[](")))
        .or_else(|| expr.strip_prefix(&format!("{rel}[\"tag_name\"][")))
    {
        let (pat, flags, tail) = regex_literal(rest.trim_start())?;
        let tail = tail.trim();
        if !(tail == ", 1]" || tail == ", 1)") {
            return None;
        }
        return Some(vec![GhRelPart::TagCapture(pat, flags)]);
    }
    // Bare `release["tag_name"]&.tr("a", "b")`.
    if let Some(r) = expr
        .strip_prefix(&format!("{rel}[\"tag_name\"]&.tr("))
        .or_else(|| expr.strip_prefix(&format!("{rel}[\"tag_name\"].tr(")))
    {
        let (from, t) = quoted_token(r.trim_start())?;
        let t = t.trim().strip_prefix(',')?.trim();
        let (to, t) = quoted_token(t)?;
        if t.trim() != ")" || from.contains('\\') || to.contains('\\') || from.starts_with('^') {
            return None;
        }
        return Some(vec![GhRelPart::TagTr(from, to)]);
    }
    if !expr.starts_with('"') {
        return None;
    }
    let (inner, tail) = quoted_token(expr)?;
    if !tail.trim().is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    let mut rest = inner.as_str();
    while let Some(start) = rest.find("#{") {
        if start > 0 {
            parts.push(GhRelPart::Lit(rest[..start].to_string()));
        }
        let body = &rest[start + 2..];
        // Brace-aware span (quotes may hold braces).
        let mut depth = 1usize;
        let mut quote: Option<char> = None;
        let mut end = None;
        let mut chars = body.char_indices().peekable();
        while let Some((idx, c)) = chars.next() {
            match quote {
                Some(q) => {
                    if c == '\\' {
                        chars.next();
                    } else if c == q {
                        quote = None;
                    }
                }
                None => match c {
                    '"' | '\'' => quote = Some(c),
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(idx);
                            break;
                        }
                    }
                    _ => {}
                },
            }
        }
        let end = end?;
        let tok = body[..end].trim();
        if let Some(op) = ghrel_operand(tok, mvar, local, rel, tag_alias) {
            parts.push(match op {
                GhRelOp::Cap(n) => GhRelPart::Cap(n),
                GhRelOp::Local => GhRelPart::Local,
                GhRelOp::Tag => GhRelPart::Tag,
            });
        } else if let Some(r) = tok
            .strip_prefix(&format!("{rel}[\"tag_name\"]&.tr("))
            .or_else(|| tok.strip_prefix(&format!("{rel}[\"tag_name\"].tr(")))
        {
            let (from, t) = quoted_token(r.trim_start())?;
            let t = t.trim().strip_prefix(',')?.trim();
            let (to, t) = quoted_token(t)?;
            if t.trim() != ")" || from.contains('\\') || to.contains('\\') || from.starts_with('^')
            {
                return None;
            }
            parts.push(GhRelPart::TagTr(from, to));
        } else {
            let mut sub = ghrel_parts(tok, mvar, local, rel, tag_alias)?;
            // A tag capture part (the only other
            // single-part form).
            if sub.len() != 1 || !matches!(sub[0], GhRelPart::TagCapture(..)) {
                return None;
            }
            parts.append(&mut sub);
        }
        rest = &body[end + 1..];
    }
    if !rest.is_empty() {
        parts.push(GhRelPart::Lit(rest.to_string()));
    }
    Some(parts)
}

/// A ghrel guard-side transform tail: nothing,
/// `.split("sep").first`, or `&.sub(/pat/flags,
/// "rep")` with a backref-free replacement. The
/// whole tail must be consumed.
fn ghrel_xform_tail(tail: &str) -> Option<super::formula::GhRelXform> {
    use super::formula::GhRelXform;
    let tail = tail.trim();
    if tail.is_empty() {
        return Some(GhRelXform::Id);
    }
    if let Some(args) = tail
        .strip_prefix("&.split(")
        .or_else(|| tail.strip_prefix(".split("))
    {
        let (sep, rest) = quoted_token(args.trim_start())?;
        let rest = rest.trim().strip_prefix(')')?.trim();
        return (rest == ".first" || rest == "&.first").then_some(GhRelXform::SplitFirst(sep));
    }
    let args = tail
        .strip_prefix("&.sub(")
        .or_else(|| tail.strip_prefix(".sub("))?;
    let (pat, flags, rest) = regex_literal(args.trim())?;
    let rest = rest.trim().strip_prefix(',')?.trim();
    let (rep, rest) = quoted_token(rest)?;
    if rest.trim() != ")" || rep.contains('\\') {
        return None;
    }
    Some(GhRelXform::SubRe(pat, flags, rep))
}

/// `version.csv.<part>` (first..fifth) with an
/// optional guard transform: (index, transform,
/// whole input consumed).
fn ghrel_own_csv(tok: &str) -> Option<(usize, super::formula::GhRelXform)> {
    let rest = tok.trim().strip_prefix("version.csv.")?;
    let (idx, tail) = [
        ("first", 0usize),
        ("second", 1),
        ("third", 2),
        ("fourth", 3),
        ("fifth", 4),
    ]
    .iter()
    .find_map(|(name, idx)| rest.strip_prefix(name).map(|t| (*idx, t)))?;
    Some((idx, ghrel_xform_tail(tail)?))
}

/// A ghrel block's result expression as `GhRelThen`
/// (identity/tr fast paths, `EqElse`, templates).
fn ghrel_then(
    expr: &str,
    mvar: Option<&str>,
    local: Option<&str>,
    rel: &str,
    tag_alias: Option<&str>,
) -> Option<super::formula::GhRelThen> {
    use super::formula::GhRelThen;
    let expr = expr.trim();
    if let Some(mv) = mvar {
        // `match.to_a.compact.drop(1).join("sep")`
        if let Some(args) = expr.strip_prefix(&format!("{mv}.to_a.compact.drop(1).join(")) {
            let (sep, tail) = quoted_token(args.trim_start())?;
            if tail.trim() != ")" {
                return None;
            }
            return Some(GhRelThen::CompactJoin(sep));
        }
        if expr == format!("{mv}[1]") {
            return Some(GhRelThen::Identity);
        }
        if let Some(args) = expr
            .strip_prefix(&format!("{mv}[1]&.tr("))
            .or_else(|| expr.strip_prefix(&format!("{mv}[1].tr(")))
        {
            let (from, tail) = quoted_token(args)?;
            let tail = tail.trim().strip_prefix(',')?.trim();
            let (to, tail) = quoted_token(tail)?;
            if tail.trim() != ")"
                || from.contains('\\')
                || to.contains('\\')
                || from.starts_with('^')
            {
                return None;
            }
            return Some(GhRelThen::Tr(from, to));
        }
    }
    if let Some(cond) = expr.strip_prefix('(') {
        let (a, rest) = cond.split_once(" == ")?;
        let (b, rest) = rest.split_once(") ? ")?;
        let a = ghrel_operand(a, mvar, local, rel, tag_alias)?;
        let b = ghrel_operand(b, mvar, local, rel, tag_alias)?;
        // Then-branch: a quoted string (extent known)
        // or a bare operand up to ` : `.
        let (then_parts, els) = if rest.starts_with('"') {
            let (_, tail) = quoted_token(rest)?;
            let used = rest.len() - tail.len();
            let els = tail.trim().strip_prefix(':')?.trim();
            (
                ghrel_parts(&rest[..used], mvar, local, rel, tag_alias)?,
                els,
            )
        } else {
            let (then, els) = rest.split_once(" : ")?;
            (ghrel_parts(then, mvar, local, rel, tag_alias)?, els)
        };
        let else_parts = ghrel_parts(els, mvar, local, rel, tag_alias)?;
        return Some(GhRelThen::EqElse {
            a,
            b,
            then: then_parts,
            els: else_parts,
        });
    }
    // The same equality ternary without parens
    // (bambu-studio@beta).
    if let Some((a, rest)) = expr.split_once(" == ")
        && let Some((b, rest)) = rest.split_once(" ? ")
        && let Some(a) = ghrel_operand(a, mvar, local, rel, tag_alias)
        && let Some(b) = ghrel_operand(b, mvar, local, rel, tag_alias)
    {
        let (then_parts, els) = if rest.starts_with('"') {
            let (_, tail) = quoted_token(rest)?;
            let used = rest.len() - tail.len();
            let els = tail.trim().strip_prefix(':')?.trim();
            (
                ghrel_parts(&rest[..used], mvar, local, rel, tag_alias)?,
                els,
            )
        } else {
            let (then, els) = rest.split_once(" : ")?;
            (ghrel_parts(then, mvar, local, rel, tag_alias)?, els)
        };
        let else_parts = ghrel_parts(els, mvar, local, rel, tag_alias)?;
        return Some(GhRelThen::EqElse {
            a,
            b,
            then: then_parts,
            els: else_parts,
        });
    }
    // `match[n] ? X : Y` (Ruby truthiness fork).
    if let Some(mv) = mvar
        && let Some(r) = expr.strip_prefix(&format!("{mv}["))
        && let Some((n, rest)) = r.split_once("] ? ")
    {
        let (then_parts, els) = if rest.starts_with('"') {
            let (_, tail) = quoted_token(rest)?;
            let used = rest.len() - tail.len();
            let els = tail.trim().strip_prefix(':')?.trim();
            (
                ghrel_parts(&rest[..used], mvar, local, rel, tag_alias)?,
                els,
            )
        } else {
            let (then, els) = rest.split_once(" : ")?;
            (ghrel_parts(then, mvar, local, rel, tag_alias)?, els)
        };
        return Some(GhRelThen::TruthyElse {
            cap: n.parse().ok()?,
            then: then_parts,
            els: ghrel_parts(els, mvar, local, rel, tag_alias)?,
        });
    }
    Some(GhRelThen::Template(ghrel_parts(
        expr, mvar, local, rel, tag_alias,
    )?))
}

/// The `each` + `break` first-match form (turtl):
/// `version = nil`, nested each loops with a
/// value-if assignment, breaks, and the version.
fn ghrel_each_first(stmts: &[String], root: &str) -> Option<super::formula::GhRelBlock> {
    use super::formula::{GhRelBlock, GhRelOp, GhRelSource, GhRelThen};
    if stmts.len() != 16 {
        return None;
    }
    let ident = |s: &str| -> Option<String> {
        let s = s.trim();
        if s.is_empty() || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
        Some(s.to_string())
    };
    let (ver, rhs) = stmts[0].split_once(" = ")?;
    let ver = ident(ver)?;
    if rhs.trim() != "nil" {
        return None;
    }
    let rel = ident(
        stmts[1]
            .trim()
            .strip_prefix(&format!("{root}.each do |"))?
            .strip_suffix('|')?,
    )?;
    let (skip_draft, skip_prerelease) = match stmts[2]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .as_str()
    {
        s if s == format!("next if {rel}[\"draft\"] || {rel}[\"prerelease\"]") => (true, true),
        s if s == format!("next if {rel}[\"draft\"]") => (true, false),
        s if s == format!("next if {rel}[\"prerelease\"]") => (false, true),
        _ => return None,
    };
    let asset = ident(
        stmts[3]
            .trim()
            .strip_prefix(&format!("{rel}[\"assets\"]&.each do |"))
            .or_else(|| {
                stmts[3]
                    .trim()
                    .strip_prefix(&format!("{rel}[\"assets\"].each do |"))
            })?
            .strip_suffix('|')?,
    )?;
    let (mv, rhs) = stmts[4].split_once(" = ")?;
    let mv = ident(mv)?;
    if rhs.trim() != format!("{asset}[\"browser_download_url\"]&.match(regex)") {
        return None;
    }
    if stmts[5].trim() != format!("next if {mv}.blank?") {
        return None;
    }
    let (v2, rhs) = stmts[6].split_once(" = ")?;
    if ident(v2)? != ver {
        return None;
    }
    let cond = rhs.trim().strip_prefix("if ")?;
    let (b_side, ne) = cond.split_once(" && ")?;
    let b_op = ghrel_operand(b_side, Some(&mv), None, &rel, None)?;
    let (a_side, b2_side) = ne.split_once(" != ")?;
    let a_op = ghrel_operand(a_side, Some(&mv), None, &rel, None)?;
    let b2_op = ghrel_operand(b2_side, Some(&mv), None, &rel, None)?;
    if b_op != b2_op {
        return None;
    }
    let then = ghrel_parts(&stmts[7], Some(&mv), None, &rel, None)?;
    if stmts[8].trim() != "else" {
        return None;
    }
    let els = ghrel_parts(&stmts[9], Some(&mv), None, &rel, None)?;
    if stmts[10].trim() != "end"
        || stmts[11].trim() != "break"
        || stmts[12].trim() != "end"
        || stmts[13].trim() != format!("break if {ver}")
        || stmts[14].trim() != "end"
        || stmts[15].trim() != ver
    {
        return None;
    }
    Some(GhRelBlock {
        skip_draft,
        skip_prerelease,
        require_prerelease: false,
        tag_ne: None,
        tag_prefix: None,
        asset_any: None,
        name_skip: None,
        pre_capture: None,
        source: GhRelSource::AssetDownloadUrl,
        then: GhRelThen::CapNeElse {
            a: a_op,
            b: b_op,
            then,
            els,
        },
        fallback_tag: None,
        blank_emit_local: false,
        limit: None,
        own_guards: Vec::new(),
        first_only: true,
    })
}

fn parse_ghrel_shape(
    params: &str,
    lines: &[String],
    cask_locals: &[(String, String)],
) -> Option<super::formula::GhRelBlock> {
    use super::formula::{GhRelAny, GhRelBlock, GhRelCapture, GhRelPart, GhRelSource, GhRelThen};
    let (root, has_regex) = block_params(params)?;
    if !has_regex {
        return None;
    }
    let stmts: Vec<String> = lines
        .iter()
        .map(|l| strip_trailing_comment(l.trim()).trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if let Some(blk) = ghrel_each_first(&stmts, &root) {
        return Some(blk);
    }
    // Guard lines compare with whitespace runs
    // collapsed (the tap writes `draft"]  ||` too).
    let squash = |l: &str| l.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut i = 0usize;
    let next = |i: &mut usize| -> Option<&str> {
        let s = stmts.get(*i)?.as_str();
        *i += 1;
        Some(s)
    };
    // Optional `file_regex = /…/` pre-statement (the
    // asset-existence guard's pattern).
    let mut file_regex: Option<(String, String, String)> = None;
    if let Some((v, rhs)) = stmts.first().and_then(|s| s.split_once(" = "))
        && !v.is_empty()
        && v.trim().chars().all(|c| c.is_alphanumeric() || c == '_')
        && let Ok((pat, flags, tail)) = regex_literal(rhs.trim()).ok_or(())
        && tail.trim().is_empty()
    {
        file_regex = Some((v.trim().to_string(), pat, flags));
        i = 1;
    }
    // Own-version locals (`version_suffix =
    // version.csv.third&.sub(/^[_-]/, "")`): recorded
    // for the per-asset own guards below.
    let mut own_locals: Vec<(String, usize, super::formula::GhRelXform)> = Vec::new();
    while let Some((v, rhs)) = stmts.get(i).and_then(|s| s.split_once(" = ")) {
        let v = v.trim();
        if v.is_empty()
            || !v.chars().all(|c| c.is_alphanumeric() || c == '_')
            || file_regex.as_ref().is_some_and(|(n, _, _)| n == v)
        {
            break;
        }
        let Some((csv_idx, xform)) = ghrel_own_csv(rhs) else {
            break;
        };
        own_locals.push((v.to_string(), csv_idx, xform));
        i += 1;
    }
    // `<json>.map do |release|` (or filter_map/flat_map),
    // optionally sliced (`json[0...9].map`).
    let open = next(&mut i)?.to_string();
    let (open, limit) =
        if let Some(rest) = open.strip_prefix(&root).and_then(|r| r.strip_prefix("[0")) {
            let (inclusive, rest) = if let Some(r) = rest.strip_prefix("...") {
                (false, r)
            } else if let Some(r) = rest.strip_prefix("..") {
                (true, r)
            } else {
                return None;
            };
            let (n, rest) = rest.split_once(']')?;
            let n: usize = n.trim().parse().ok()?;
            let n = if inclusive { n + 1 } else { n };
            (format!("{root}{rest}"), Some(n))
        } else {
            (open, None)
        };
    let open = open.as_str();
    let (rel, outer_flat) = ["map", "filter_map", "flat_map"].iter().find_map(|m| {
        open.strip_prefix(&format!("{root}.{m} do |"))
            .and_then(|r| r.strip_suffix('|'))
            .map(|r| (r, *m == "flat_map"))
    })?;
    if rel.is_empty() || !rel.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    let rel = rel.to_string();
    let (mut skip_draft, mut skip_prerelease, mut require_prerelease) = (false, false, false);
    let mut name_skip: Option<(String, String)> = None;
    let mut tag_ne: Option<String> = None;
    let mut tag_prefix: Option<String> = None;
    let mut asset_any: Option<GhRelAny> = None;
    // Guards, in tap order (draft/prerelease/tag, then name).
    loop {
        let line = stmts.get(i)?.as_str();
        let flat = squash(line);
        let line = flat.as_str();
        if line == format!("next if {rel}[\"draft\"] || {rel}[\"prerelease\"]") {
            skip_draft = true;
            skip_prerelease = true;
            i += 1;
        } else if line == format!("next if {rel}[\"draft\"] || !{rel}[\"prerelease\"]") {
            skip_draft = true;
            require_prerelease = true;
            i += 1;
        } else if line == format!("next if {rel}[\"draft\"]") {
            skip_draft = true;
            i += 1;
        } else if line == format!("next if {rel}[\"prerelease\"]") {
            skip_prerelease = true;
            i += 1;
        } else if line == format!("next if !{rel}[\"prerelease\"]")
            || line == format!("next unless {rel}[\"prerelease\"]")
        {
            require_prerelease = true;
            i += 1;
        } else if let Some(rest) = line.strip_prefix(&format!("next if {rel}[\"tag_name\"] != ")) {
            let (lit, tail) = quoted_token(rest.trim())?;
            if !tail.trim().is_empty() {
                return None;
            }
            tag_ne = Some(lit);
            i += 1;
        } else if let Some(rest) = line
            .strip_prefix(&format!("next if {rel}[\"name\"]&.match?("))
            .or_else(|| line.strip_prefix(&format!("next if {rel}[\"name\"].match?(")))
        {
            let (pat, flags, tail) = regex_literal(rest)?;
            if tail.trim() != ")" {
                return None;
            }
            name_skip = Some((pat, flags));
            i += 1;
        } else if let Some(rest) = line
            .strip_prefix(&format!("next unless {rel}[\"tag_name\"]&.start_with?("))
            .or_else(|| line.strip_prefix(&format!("next unless {rel}[\"tag_name\"].start_with?(")))
        {
            let (lit, tail) = quoted_token(rest.trim())?;
            if tail.trim() != ")" {
                return None;
            }
            tag_prefix = Some(lit);
            i += 1;
        } else if let Some(rest) = line
            .strip_prefix(&format!("next unless {rel}[\"assets\"]&.any? {{ |"))
            .or_else(|| line.strip_prefix(&format!("next unless {rel}[\"assets\"].any? {{ |")))
        {
            // `{a}| {a}["name"]&.match?(<filevar>|/lit/) }`
            // or `.end_with?("lit"|{local})`.
            let (a, test) = rest.split_once("| ")?;
            let test = test.trim().strip_suffix(" }")?;
            if a.is_empty() || !a.chars().all(|c| c.is_alphanumeric() || c == '_') {
                return None;
            }
            if let Some(m) = test
                .strip_prefix(&format!("{a}[\"name\"]&.match?("))
                .or_else(|| test.strip_prefix(&format!("{a}[\"name\"].match?(")))
            {
                let m = m.trim();
                if m.starts_with('/') || m.starts_with("%r") {
                    let (pat, flags, tail) = regex_literal(m)?;
                    if tail.trim() != ")" {
                        return None;
                    }
                    asset_any = Some(GhRelAny::NameMatch(pat, flags));
                } else {
                    let name = m.strip_suffix(')')?.trim();
                    let (_, pat, flags) = file_regex.as_ref().filter(|(n, _, _)| n == name)?;
                    asset_any = Some(GhRelAny::NameMatch(pat.clone(), flags.clone()));
                }
            } else {
                let e = test
                    .strip_prefix(&format!("{a}[\"name\"]&.end_with?("))
                    .or_else(|| test.strip_prefix(&format!("{a}[\"name\"].end_with?(")))?;
                let e = e.trim();
                if e.starts_with('"') {
                    let (lit, tail) = quoted_token(e)?;
                    if tail.trim() != ")" {
                        return None;
                    }
                    asset_any = Some(GhRelAny::NameEndsWith(lit));
                } else {
                    let name = e.strip_suffix(')')?.trim();
                    let lit = cask_locals
                        .iter()
                        .find(|(n, _)| n == name)
                        .map(|(_, v)| v.clone())?;
                    asset_any = Some(GhRelAny::NameEndsWith(lit));
                }
            }
            i += 1;
        } else {
            break;
        }
    }
    // Release-level locals, in order: an optional
    // raw-tag alias (`tag = release["tag_name"]`)
    // and/or one pre-capture local (a bracket
    // capture off a field or the alias with its
    // blank guard, or a `delete_prefix` field read).
    let mut pre_capture: Option<GhRelCapture> = None;
    let mut local_var: Option<String> = None;
    let mut tag_alias: Option<String> = None;
    for _ in 0..2 {
        let Some(stmt) = stmts.get(i) else { break };
        let Some((v, rhs)) = stmt.split_once(" = ") else {
            break;
        };
        let v = v.trim();
        if v.is_empty() || !v.chars().all(|c| c.is_alphanumeric() || c == '_') {
            break;
        }
        let rhs = rhs.trim();
        // Raw-tag alias.
        if rhs == format!("{rel}[\"tag_name\"]") && tag_alias.is_none() {
            tag_alias = Some(v.to_string());
            i += 1;
            continue;
        }
        // `delete_prefix` field read.
        let del = (|| {
            let rest = rhs
                .strip_prefix(&format!("{rel}[\"tag_name\"]&.delete_prefix("))
                .or_else(|| rhs.strip_prefix(&format!("{rel}[\"tag_name\"].delete_prefix(")))?;
            let (lit, tail) = quoted_token(rest.trim_start())?;
            (tail.trim() == ")").then_some(lit)
        })();
        if let Some(lit) = del {
            if pre_capture.is_some() {
                return None;
            }
            let guard =
                stmts.get(i + 1).map(String::as_str) == Some(&format!("next if {v}.blank?")[..]);
            local_var = Some(v.to_string());
            pre_capture = Some(GhRelCapture {
                field: "tag_name".to_string(),
                lit: None,
                op: super::formula::GhRelLocalOp::DeletePrefix(lit),
                blank_guard: guard,
            });
            i += if guard { 2 } else { 1 };
            continue;
        }
        // Bracket capture off a field or the alias.
        let parsed = (|| {
            let recv_alias = tag_alias.as_deref().filter(|a| {
                rhs.starts_with(&format!("{a}[")) || rhs.starts_with(&format!("{a}&.[]("))
            });
            let (field, arg) = if let Some(a) = recv_alias {
                let arg = rhs
                    .strip_prefix(&format!("{a}&.[]("))
                    .or_else(|| rhs.strip_prefix(&format!("{a}[")))?
                    .trim_start();
                ("tag_name", arg)
            } else {
                let field = ["name", "tag_name"].into_iter().find(|f| {
                    rhs.starts_with(&format!("{rel}[\"{f}\"]&.[]("))
                        || rhs.starts_with(&format!("{rel}[\"{f}\"].[]("))
                        || rhs.starts_with(&format!("{rel}[\"{f}\"]["))
                })?;
                let arg = rhs
                    .strip_prefix(&format!("{rel}[\"{field}\"]&.[]("))
                    .or_else(|| rhs.strip_prefix(&format!("{rel}[\"{field}\"].[](")))
                    .or_else(|| rhs.strip_prefix(&format!("{rel}[\"{field}\"][")))?
                    .trim_start();
                (field, arg)
            };
            let lit = if let Some(t) = arg.strip_prefix("regex") {
                let t = t.trim();
                if !(t == ", 1)" || t == ", 1]" || t == ",1)" || t == ",1]") {
                    return None;
                }
                None
            } else {
                let (pat, flags, tail) = regex_literal(arg)?;
                let tail = tail.trim();
                if !(tail == ", 1)" || tail == ", 1]" || tail == ",1)" || tail == ",1]") {
                    return None;
                }
                Some((pat, flags))
            };
            Some(GhRelCapture {
                field: field.to_string(),
                lit,
                op: super::formula::GhRelLocalOp::Capture,
                blank_guard: true,
            })
        })();
        match parsed {
            Some(cap) => {
                if pre_capture.is_some() {
                    return None;
                }
                if stmts.get(i + 1).map(String::as_str) != Some(&format!("next if {v}.blank?")[..])
                {
                    return None;
                }
                local_var = Some(v.to_string());
                pre_capture = Some(cap);
                i += 2;
            }
            None => break,
        }
    }
    // Capture statement(s): an assets loop, a tag
    // match, or nothing (pre-capture only).
    let mut source: Option<GhRelSource> = None;
    let mut asset_filter = false;
    let mut mvar: Option<String> = None;
    if let Some(open) = stmts.get(i) {
        let assets = ["map", "filter_map"].iter().find_map(|m| {
            open.strip_prefix(&format!("{rel}[\"assets\"]&.{m} do |"))
                .and_then(|r| r.strip_suffix('|'))
                .map(|a| (a, *m == "filter_map"))
        });
        if let Some((a, filter)) = assets {
            if a.is_empty() || !a.chars().all(|c| c.is_alphanumeric() || c == '_') {
                return None;
            }
            asset_filter = filter;
            i += 1;
            let cap = next(&mut i)?;
            let (mv, cap_rhs) = cap.split_once(" = ")?;
            if mv.is_empty() || !mv.chars().all(|c| c.is_alphanumeric() || c == '_') {
                return None;
            }
            source = if cap_rhs == format!("{a}[\"browser_download_url\"]&.match(regex)")
                || cap_rhs == format!("{a}[\"browser_download_url\"].match(regex)")
            {
                Some(GhRelSource::AssetDownloadUrl)
            } else if cap_rhs == format!("{a}[\"name\"]&.match(regex)")
                || cap_rhs == format!("{a}[\"name\"].match(regex)")
            {
                Some(GhRelSource::AssetName)
            } else {
                return None;
            };
            mvar = Some(mv.to_string());
        } else if let Some((mv, cap_rhs)) = open.split_once(" = ")
            && (cap_rhs == format!("{rel}[\"tag_name\"]&.match(regex)")
                || cap_rhs == format!("{rel}[\"tag_name\"].match(regex)")
                || cap_rhs == format!("{rel}[\"name\"]&.match(regex)")
                || cap_rhs == format!("{rel}[\"name\"].match(regex)"))
        {
            if mv.is_empty() || !mv.chars().all(|c| c.is_alphanumeric() || c == '_') {
                return None;
            }
            source = Some(if cap_rhs.contains("[\"name\"]") {
                GhRelSource::Name
            } else {
                GhRelSource::TagName
            });
            mvar = Some(mv.to_string());
            i += 1;
        }
    }
    let source = match (source, &pre_capture) {
        (Some(s), _) => s,
        (None, Some(_)) => GhRelSource::PreCaptureOnly,
        (None, None) => return None,
    };
    let mut blank_emit_local = false;
    let mut own_guards: Vec<super::formula::GhRelMatchGuard> = Vec::new();
    if let Some(mv) = &mvar {
        let guard = squash(next(&mut i)?);
        if guard == format!("next if {mv}.blank?") || guard == format!("next unless {mv}") {
            // Plain blank guard, either spelling.
        } else if local_var.as_deref().is_some_and(|lv| {
            guard == format!("next {lv} if {mv}.blank?")
                || guard == format!("next {lv} unless {mv}")
        }) {
            blank_emit_local = true;
        } else {
            let rest = guard
                .strip_prefix(&format!("next if {mv}.blank? || {rel}[\"name\"]&.match?("))
                .or_else(|| {
                    guard.strip_prefix(&format!("next if {mv}.blank? || {rel}[\"name\"].match?("))
                })?;
            // Blank guard combined with the name skip.
            let (pat, flags, tail) = regex_literal(rest)?;
            if tail.trim() != ")" {
                return None;
            }
            name_skip = Some((pat, flags));
        }
    }
    // Own-version match guards:
    // `next if match[n]<xform> != <own form>`.
    if let Some(mv) = &mvar {
        while let Some(line) = stmts.get(i).map(|s| squash(s))
            && let Some(cond) = line.strip_prefix("next if ")
        {
            let (lhs, rhs) = cond.split_once(" != ")?;
            let lhs = lhs.trim();
            let rest = lhs.strip_prefix(&format!("{mv}["))?;
            let (n, tail) = rest.split_once(']')?;
            let cap: usize = n.parse().ok()?;
            let cap_xform = ghrel_xform_tail(tail)?;
            let rhs = rhs.trim();
            let (own_csv, own_xform) = if let Some((idx, xf)) = ghrel_own_csv(rhs) {
                (idx, xf)
            } else if let Some((_, idx, xf)) = own_locals.iter().find(|(name, _, _)| name == rhs) {
                (*idx, xf.clone())
            } else {
                return None;
            };
            own_guards.push(super::formula::GhRelMatchGuard {
                cap,
                cap_xform,
                own_csv,
                own_xform,
            });
            i += 1;
        }
    }
    // Result expression (an early-`next` equality
    // statement may precede it in an assets loop).
    let expr = next(&mut i)?.to_string();
    let then = if let Some(early) = expr.strip_prefix("next ")
        && let Some((early_expr, cond)) = early.rsplit_once(" if ")
        && mvar.is_some()
    {
        let (a, b) = cond.split_once(" == ")?;
        let els_expr = next(&mut i)?;
        GhRelThen::EqElse {
            a: ghrel_operand(
                a,
                mvar.as_deref(),
                local_var.as_deref(),
                &rel,
                tag_alias.as_deref(),
            )?,
            b: ghrel_operand(
                b,
                mvar.as_deref(),
                local_var.as_deref(),
                &rel,
                tag_alias.as_deref(),
            )?,
            then: ghrel_parts(
                early_expr,
                mvar.as_deref(),
                local_var.as_deref(),
                &rel,
                tag_alias.as_deref(),
            )?,
            els: ghrel_parts(
                els_expr,
                mvar.as_deref(),
                local_var.as_deref(),
                &rel,
                tag_alias.as_deref(),
            )?,
        }
    } else if let Some(mv) = &mvar
        && let Some((v, rhs)) = expr.split_once(" = ")
    {
        // `version = Version.new(match[1])`, a zero-patch
        // guard, then the version itself.
        if rhs != format!("Version.new({mv}[1])")
            || v.is_empty()
            || !v.chars().all(|c| c.is_alphanumeric() || c == '_')
        {
            return None;
        }
        if next(&mut i)? != format!("next if {v}.patch.to_i.zero?") {
            return None;
        }
        if next(&mut i)? != v {
            return None;
        }
        GhRelThen::NonZeroPatch
    } else {
        ghrel_then(
            &expr,
            mvar.as_deref(),
            local_var.as_deref(),
            &rel,
            tag_alias.as_deref(),
        )?
    };
    // Closers: assets form has an inner `end` (or the
    // `.presence ||` fallback statement) then
    // `end.flatten`; other forms a plain `end`.
    // Nothing may follow.
    let tail: Vec<&str> = stmts[i..].iter().map(String::as_str).collect();
    let mut fallback_tag: Option<(String, String)> = None;
    let flat_tail = ["end.flatten", "end.flatten.compact", "end.compact"];
    let closed = match source {
        GhRelSource::TagName | GhRelSource::Name | GhRelSource::PreCaptureOnly => {
            tail == ["end"]
                || (tail.len() == 1 && flat_tail.contains(&tail[0]))
                || (outer_flat && tail == ["end"])
        }
        GhRelSource::AssetDownloadUrl | GhRelSource::AssetName => {
            if (tail.len() == 2 && tail[0] == "end" && flat_tail.contains(&tail[1]))
                || (outer_flat && tail == ["end", "end"])
                || (outer_flat && tail.len() == 2 && tail[0] == "end" && tail[1] == "end.compact")
                || (outer_flat && tail == ["end.compact", "end"])
            {
                true
            } else if asset_filter
                && tail.len() == 2
                && tail[1] == "end.flatten"
                && let Some(rest) = tail[0].strip_prefix("end.presence || ")
            {
                match ghrel_parts(
                    rest,
                    mvar.as_deref(),
                    local_var.as_deref(),
                    &rel,
                    tag_alias.as_deref(),
                )
                .as_deref()
                {
                    Some([GhRelPart::TagCapture(pat, flags)]) => {
                        fallback_tag = Some((pat.clone(), flags.clone()));
                        true
                    }
                    _ => false,
                }
            } else {
                false
            }
        }
    };
    if !closed {
        return None;
    }
    // A render may only read captures/locals that exist.
    let parts_ok = |parts: &[GhRelPart]| {
        parts.iter().all(|p| match p {
            GhRelPart::Cap(_) => mvar.is_some(),
            GhRelPart::Local => pre_capture.is_some(),
            _ => true,
        })
    };
    let then_ok = match &then {
        GhRelThen::Template(parts) => parts_ok(parts),
        GhRelThen::EqElse { then, els, .. } => parts_ok(then) && parts_ok(els),
        GhRelThen::CapNeElse { then, els, .. } => parts_ok(then) && parts_ok(els),
        GhRelThen::TruthyElse { then, els, .. } => parts_ok(then) && parts_ok(els),
        _ => true,
    };
    if !then_ok {
        return None;
    }
    Some(GhRelBlock {
        skip_draft,
        skip_prerelease,
        require_prerelease,
        tag_ne,
        tag_prefix,
        asset_any,
        name_skip,
        pre_capture,
        source,
        then,
        fallback_tag,
        blank_emit_local,
        limit,
        own_guards,
        first_only: false,
    })
}

/// The transform chain after a `:git` block's capture:
/// nothing, `&.tr("a", "b")`, `&.gsub("a", "b")` or
/// `&.gsub(/pat/flags, "template")` (the plain `.`
/// spellings accepted too). The whole tail must be
/// consumed.
fn git_op_chain(tail: &str) -> Option<super::formula::GitOp> {
    use super::formula::GitOp;
    let tail = tail.trim();
    if tail.is_empty() {
        return Some(GitOp::Identity);
    }
    if let Some(args) = tail
        .strip_prefix("&.tr(")
        .or_else(|| tail.strip_prefix(".tr("))
    {
        let (from, rest) = quoted_token(args)?;
        let rest = rest.trim().strip_prefix(',')?.trim();
        let (to, rest) = quoted_token(rest)?;
        if rest.trim() != ")" || from.contains('\\') || to.contains('\\') || from.starts_with('^') {
            return None;
        }
        return Some(GitOp::Tr(from, to));
    }
    let args = tail
        .strip_prefix("&.gsub(")
        .or_else(|| tail.strip_prefix(".gsub("))?;
    if args.starts_with('"') {
        let (from, rest) = quoted_token(args)?;
        let rest = rest.trim().strip_prefix(',')?.trim();
        let (to, rest) = quoted_token(rest)?;
        if rest.trim() != ")" || from.contains('\\') || to.contains('\\') {
            return None;
        }
        return Some(GitOp::GsubLit(from, to));
    }
    let (pat, flags, rest) = regex_literal(args)?;
    let rest = rest.trim().strip_prefix(',')?.trim();
    let (tmpl, rest) = sub_template(rest)?;
    if rest.trim() != ")" {
        return None;
    }
    Some(GitOp::GsubRe(pat, flags, tmpl))
}

/// One `:git` block map body: `<tag>[regex, 1]<chain>`
/// or `<tag>.match(regex)&.[](1)<chain>`.
fn git_map_expr(tag: &str, expr: &str) -> Option<super::formula::GitOp> {
    let expr = expr.trim();
    let tail = expr
        .strip_prefix(&format!("{tag}[regex, 1]"))
        .or_else(|| expr.strip_prefix(&format!("{tag}.match(regex)&.[](1)")))?;
    git_op_chain(tail)
}

/// A tag skip-list assignment:
/// `<var> = ["a", ...]` with an optional `.freeze`.
/// Returns the variable and its entries.
fn git_skip_assign(stmt: &str) -> Option<(String, Vec<String>)> {
    let (var, rest) = stmt.split_once('=')?;
    let var = var.trim();
    if var.is_empty() || !var.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let rest = rest.trim().strip_suffix(".freeze").unwrap_or(rest.trim());
    let mut cur = rest.trim().strip_prefix('[')?;
    let mut tags = Vec::new();
    loop {
        cur = cur.trim_start();
        if let Some(end) = cur.strip_prefix(']') {
            if end.trim().is_empty() && !tags.is_empty() {
                return Some((var.to_string(), tags));
            }
            return None;
        }
        let (tag, tail) = if cur.starts_with('\'') {
            single_quoted_token(cur)?
        } else {
            quoted_token(cur)?
        };
        if tag.contains('\\') {
            return None;
        }
        tags.push(tag);
        cur = tail.trim_start().strip_prefix(',').unwrap_or(tail);
    }
}

/// Reduce a `:git` strategy block body to a data
/// shape: `<tags>.filter_map`/`.map { |tag| … }` over
/// the regex capture with one `tr`/`gsub` chain, or
/// the `do … end` form with a tag skip-list guard.
/// `params` is the block tail (`do |tags|`,
/// `do |tags, regex|`).
fn parse_git_shape(params: &str, lines: &[String]) -> Option<super::formula::GitBlock> {
    use super::formula::GitBlock;
    let (root, has_regex) = block_params(params)?;
    let stmts = block_statements(lines);
    let (skip_var, skip_tags, rest): (Option<String>, Vec<String>, &[String]) =
        match stmts.first().and_then(|s| git_skip_assign(s)) {
            Some((var, tags)) => (Some(var), tags, &stmts[1..]),
            None => (None, Vec::new(), &stmts[..]),
        };
    // The `do … end` map (the skip-list form):
    //   <tags>.map do |tag|
    //     next if <var>.include?(tag)
    //     <expr>
    //   end
    if rest.len() >= 2
        && let Some(open) = rest[0].strip_prefix(&root)
        && let Some(open) = strip_map_open(open)
        && open.trim_start().starts_with("do")
    {
        let var = open
            .trim()
            .strip_prefix("do")?
            .trim()
            .strip_prefix('|')?
            .strip_suffix('|')?
            .trim()
            .to_string();
        if var.is_empty() || !var.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
        let mut idx = 1usize;
        if let Some(svar) = &skip_var {
            if rest.get(idx).map(String::as_str)
                != Some(format!("next if {svar}.include?({var})").as_str())
            {
                return None;
            }
            idx += 1;
        }
        if rest.last().map(String::as_str) != Some("end") {
            return None;
        }
        let inner = &rest[idx..rest.len() - 1];
        // Match-assign form:
        //   match = tag.match(regex)
        //   next if match.blank? [|| match[1].split(..).include?(..)]
        //   "#{match[1]},#{match[2]}" | match[1]
        if inner.len() == 3
            && let Some((mv, rhs)) = assign_parts(&inner[0])
            && (rhs == format!("{var}.match(regex)") || rhs == format!("{var}&.match(regex)"))
        {
            let g = inner[1].trim();
            let mut skip_split_contains = None;
            if g == format!("next if {mv}.blank?") {
            } else if let Some(r) =
                g.strip_prefix(&format!("next if {mv}.blank? || {mv}[1].split("))
            {
                let (sep, r) = quoted_token(r.trim_start())?;
                let r = r.trim().strip_prefix(')')?.trim();
                let r = r
                    .strip_prefix(".include?(")
                    .or_else(|| r.strip_prefix("&.include?("))?;
                let (lit, r) = quoted_token(r.trim_start())?;
                if r.trim() != ")" {
                    return None;
                }
                skip_split_contains = Some((sep, lit));
            } else {
                return None;
            }
            let parts = latest_parts(&inner[2], &mv, None, None)?;
            return Some(GitBlock {
                needs_spec_regex: !has_regex,
                skip_tags,
                op: super::formula::GitOp::Identity,
                match_template: Some(parts),
                skip_split_contains,
            });
        }
        if inner.len() != 1 {
            return None;
        }
        let op = git_map_expr(&var, &inner[0])?;
        return Some(GitBlock {
            needs_spec_regex: !has_regex,
            skip_tags,
            op,
            match_template: None,
            skip_split_contains: None,
        });
    }
    // The brace map, one statement:
    //   <tags>.filter_map { |tag| <expr> }
    if skip_var.is_some() || rest.len() != 1 {
        return None;
    }
    let after = rest[0].strip_prefix(&root)?;
    let after = strip_map_open(after)?;
    let (var, inner) = json_map_parts(after)?;
    let op = git_map_expr(&var, &inner)?;
    Some(GitBlock {
        needs_spec_regex: !has_regex,
        skip_tags,
        op,
        match_template: None,
        skip_split_contains: None,
    })
}

/// `<root>["<field>"]` at the start of `s`: the field
/// name and the tail after `]`.
fn latest_field<'a>(root: &str, s: &'a str) -> Option<(String, &'a str)> {
    let rest = s.strip_prefix(root)?.strip_prefix('[')?;
    let (field, tail) = quoted_token(rest)?;
    Some((field, tail.strip_prefix(']')?))
}

fn latest_source(field: &str) -> Option<super::formula::LatestSource> {
    use super::formula::LatestSource;
    match field {
        "tag_name" => Some(LatestSource::TagName),
        "name" => Some(LatestSource::Name),
        "body" => Some(LatestSource::Body),
        "published_at" => Some(LatestSource::PublishedAt),
        _ => None,
    }
}

/// A `:github_latest` capture's trailing chain:
/// nothing or one `tr` (either navigation spelling).
fn latest_tr(tail: &str) -> Option<Option<(String, String)>> {
    use super::formula::GitOp;
    match git_op_chain(tail)? {
        GitOp::Identity => Some(None),
        GitOp::Tr(from, to) => Some(Some((from, to))),
        GitOp::GsubLit(..) | GitOp::GsubRe(..) => None,
    }
}

/// `<var> = <rhs>` split (the variable validated).
fn assign_parts(stmt: &str) -> Option<(String, &str)> {
    let (var, rhs) = stmt.split_once('=')?;
    let var = var.trim();
    if var.is_empty() || !var.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some((var.to_string(), rhs.trim()))
}

/// Reduce a `:github_latest` strategy block body to
/// a data shape: a scan or first-capture of one
/// release field (or of each asset's name), with an
/// optional `tr` per capture.
/// A `:github_latest` render expression (bare cap
/// or an interpolation string over caps).
fn latest_parts(
    expr: &str,
    var: &str,
    local: Option<&str>,
    picked: Option<&str>,
) -> Option<Vec<super::formula::LatestPart>> {
    use super::formula::LatestPart;
    let expr = expr.trim();
    if Some(expr) == local {
        return Some(vec![LatestPart::Local]);
    }
    // `var[n]` or `var[n].tr("a", "b")` as one part.
    let part_of = |tok: &str| -> Option<LatestPart> {
        let rest = tok.strip_prefix(&format!("{var}["))?;
        let (n, tail) = rest.split_once(']')?;
        let n: usize = n.parse().ok()?;
        let tail = tail.trim();
        if tail.is_empty() {
            return Some(LatestPart::Cap(n));
        }
        match latest_tr(tail)? {
            Some((from, to)) => Some(LatestPart::CapTr(n, from, to)),
            None => None,
        }
    };
    if let Some(part) = part_of(expr) {
        return Some(vec![part]);
    }
    if !expr.starts_with('"') {
        return None;
    }
    let (inner, tail) = quoted_token(expr)?;
    if !tail.trim().is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    let mut rest = inner.as_str();
    while let Some(start) = rest.find("#{") {
        if start > 0 {
            parts.push(LatestPart::Lit(rest[..start].to_string()));
        }
        let body = &rest[start + 2..];
        // Brace-aware token end: a token may carry
        // its own `{ … }` block (recaf's assets map).
        let bytes = body.as_bytes();
        let mut depth = 1i32;
        let mut quote: Option<u8> = None;
        let mut end = None;
        let mut i = 0usize;
        while i < bytes.len() {
            let c = bytes[i];
            match (quote, c) {
                (Some(_), b'\\') => i += 1,
                (Some(q), c2) if c2 == q => quote = None,
                (Some(_), _) => {}
                (None, b'"' | b'\'') => quote = Some(c),
                (None, b'{') => depth += 1,
                (None, b'}') => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i);
                        break;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let end = end?;
        let tok = body[..end].trim();
        if Some(tok) == local {
            parts.push(LatestPart::Local);
        } else if picked.is_some_and(|p| latest_assets_max_digits(tok, p)) {
            parts.push(LatestPart::AssetsMaxUpdatedDigits);
        } else {
            parts.push(part_of(tok)?);
        }
        rest = &body[end + 1..];
    }
    if !rest.is_empty() {
        parts.push(LatestPart::Lit(rest.to_string()));
    }
    Some(parts)
}

/// `<picked>.map { |a| a["updated_at"] }.max
/// .gsub(/\D/, "")` as a whole template token.
fn latest_assets_max_digits(tok: &str, picked: &str) -> bool {
    let inner = (|| {
        let r = tok
            .strip_prefix(picked)?
            .trim()
            .strip_prefix(".map")?
            .trim();
        let r = r.strip_prefix('{')?.trim().strip_prefix('|')?;
        let (a, r) = r.split_once('|')?;
        let a = a.trim();
        if a.is_empty() || !a.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
        let r = r
            .trim()
            .strip_prefix(&format!("{a}[\"updated_at\"]"))?
            .trim();
        let r = r.strip_prefix('}')?.trim().strip_prefix(".max.gsub(")?;
        let (pat, flags, t) = regex_literal(r.trim())?;
        let t = t.trim().strip_prefix(',')?.trim();
        let (rep, t) = quoted_token(t)?;
        (pat == "\\D" && flags.is_empty() && rep.is_empty() && t.trim() == ")").then_some(())
    })();
    inner.is_some()
}

/// One side of a latest equality fork: a capture
/// (optionally `tr`ed or read through
/// `Version.new(…).major_minor_patch`) or the local.
fn latest_op(tok: &str, var: &str, local: Option<&str>) -> Option<super::formula::LatestOp> {
    use super::formula::LatestOp;
    let tok = tok.trim();
    if Some(tok) == local {
        return Some(LatestOp::Local);
    }
    if let Some(inner) = tok
        .strip_prefix("Version.new(")
        .and_then(|r| r.strip_suffix(").major_minor_patch.to_s"))
    {
        let r = inner.trim().strip_prefix(&format!("{var}["))?;
        let n: usize = r.strip_suffix(']')?.parse().ok()?;
        return Some(LatestOp::CapMmp(n));
    }
    let rest = tok.strip_prefix(&format!("{var}["))?;
    let (n, tail) = rest.split_once(']')?;
    let n: usize = n.parse().ok()?;
    let tail = tail.trim();
    if tail.is_empty() {
        return Some(LatestOp::Cap(n));
    }
    match latest_tr(tail)? {
        Some((from, to)) => Some(LatestOp::CapTr(n, from, to)),
        None => None,
    }
}

/// Split a `X ? A : B` render tail (A quoted or bare).
fn latest_fork(
    rest: &str,
    var: &str,
    local: Option<&str>,
    picked: Option<&str>,
) -> Option<(
    Vec<super::formula::LatestPart>,
    Vec<super::formula::LatestPart>,
)> {
    if rest.starts_with('"') {
        let (_, tail) = quoted_token(rest)?;
        let used = rest.len() - tail.len();
        Some((
            latest_parts(&rest[..used], var, local, picked)?,
            latest_parts(tail.trim().strip_prefix(':')?.trim(), var, local, picked)?,
        ))
    } else {
        let (then, els) = rest.split_once(" : ")?;
        Some((
            latest_parts(then, var, local, picked)?,
            latest_parts(els, var, local, picked)?,
        ))
    }
}

/// A match-form render: plain capture(+tr), template,
/// `(m[a] == m[b]) ? …`, or `m[n].present? ? …` as
/// `(render, tr)` (`tr` only for the capture form).
fn latest_match_render(
    expr: &str,
    var: &str,
    local: Option<&str>,
    picked: Option<&str>,
) -> Option<(super::formula::LatestRender, Option<(String, String)>)> {
    use super::formula::LatestRender;
    let expr = expr.trim();
    if let Some(chain) = expr.strip_prefix(&format!("{var}[1]"))
        && let Some(tr) = latest_tr(chain)
    {
        return Some((LatestRender::Capture, tr));
    }
    if let Some(cond) = expr.strip_prefix('(') {
        let (a, rest) = cond.split_once(" == ")?;
        let (b, rest) = rest.split_once(") ? ")?;
        let a = latest_op(a, var, local)?;
        let b = latest_op(b, var, local)?;
        let (then, els) = latest_fork(rest, var, local, picked)?;
        return Some((LatestRender::EqElse { a, b, then, els }, None));
    }
    if let Some(r) = expr.strip_prefix(&format!("{var}[")) {
        if let Some((n, rest)) = r.split_once("].present? ? ") {
            let (then, els) = latest_fork(rest, var, local, picked)?;
            return Some((
                LatestRender::PresentElse {
                    cap: n.parse().ok()?,
                    then,
                    els,
                },
                None,
            ));
        }
        if let Some((n, rest)) = r.split_once("] ? ") {
            let (then, els) = latest_fork(rest, var, local, picked)?;
            return Some((
                LatestRender::TruthyElse {
                    cap: n.parse().ok()?,
                    then,
                    els,
                },
                None,
            ));
        }
    }
    Some((
        LatestRender::Template(latest_parts(expr, var, local, picked)?),
        None,
    ))
}

/// A scan-form inner expression as `(render, tr)`:
/// `{var}[0]<chain>` stays the scan capture; a
/// template or `present?` fork uses scan indexing.
fn latest_scan_render(
    inner: &str,
    var: &str,
) -> Option<(super::formula::LatestRender, Option<(String, String)>)> {
    use super::formula::LatestRender;
    let inner = inner.trim();
    if let Some(chain) = inner.strip_prefix(&format!("{var}[0]"))
        && let Some(tr) = latest_tr(chain)
    {
        return Some((LatestRender::Capture, tr));
    }
    if let Some(r) = inner.strip_prefix(&format!("{var}["))
        && let Some((n, rest)) = r.split_once("].present? ? ")
    {
        let (then, els) = latest_fork(rest, var, None, None)?;
        return Some((
            LatestRender::ScanPresentElse {
                cap: n.parse().ok()?,
                then,
                els,
            },
            None,
        ));
    }
    Some((
        LatestRender::ScanTemplate(latest_parts(inner, var, None, None)?),
        None,
    ))
}

/// Reduce a `:github_latest` strategy block body to
/// a data shape: a scan or first-capture of one
/// release field (or of each asset's name), with an
/// optional `tr` per capture.
fn parse_latest_shape(params: &str, lines: &[String]) -> Option<super::formula::LatestBlock> {
    use super::formula::{
        LatestBlock, LatestOp, LatestPre, LatestPreOp, LatestRender, LatestSource,
    };
    let (root, has_regex) = block_params(params)?;
    let stmts = block_statements(lines);
    let base = |source: LatestSource, scan: bool, tr: Option<(String, String)>| LatestBlock {
        needs_spec_regex: !has_regex,
        source,
        scan,
        tr,
        render: LatestRender::Capture,
        two_b: None,
        pre_capture: None,
        match_pre: false,
        asset_pick: None,
    };
    // The match blank guard, either spelling.
    let blank_guard = |stmt: &str, var: &str| {
        stmt == format!("next if {var}.blank?") || stmt == format!("next unless {var}")
    };
    let with_render =
        |mut b: LatestBlock, render: LatestRender, tr: Option<(String, String)>| -> LatestBlock {
            b.render = render;
            b.tr = tr;
            b
        };
    // Single statement: scan or `[](regex, 1)` over
    // one release field.
    if stmts.len() == 1 {
        let (field, tail) = latest_field(&root, stmts[0].trim())?;
        let source = latest_source(&field)?;
        if let Some(rest) = tail.strip_prefix("&.scan(regex)") {
            let rest = strip_map_open(rest)?;
            let (var, inner) = json_map_parts(rest)?;
            let (render, tr) = latest_scan_render(&inner, &var)?;
            return Some(with_render(base(source, true, None), render, tr));
        }
        let chain = tail.strip_prefix("&.[](regex, 1)")?;
        return Some(base(source, false, latest_tr(chain)?));
    }
    // Scan statement form:
    //   <json>["<field>"]&.scan(regex)&.map do |match|
    //     <expr>
    //   end
    if stmts.len() == 3 && stmts[2] == "end" {
        if let Some((field, tail)) = latest_field(&root, stmts[0].trim())
            && let Some(rest) = tail.strip_prefix("&.scan(regex)")
            && let Some(open) = strip_map_open(rest)
            && let Some(var) = open
                .trim()
                .strip_prefix("do")
                .and_then(|r| r.trim().strip_prefix('|'))
                .and_then(|r| r.strip_suffix('|'))
                .map(str::trim)
                .filter(|v| !v.is_empty() && v.chars().all(|c| c.is_alphanumeric() || c == '_'))
        {
            let source = latest_source(&field)?;
            let (render, tr) = latest_scan_render(&stmts[1], var)?;
            return Some(with_render(base(source, true, None), render, tr));
        }
        // Assets direct-capture form:
        //   <json>["assets"]&.filter_map do |asset|
        //     asset["<key>"]&.[](regex, 1)<chain>
        //   end
        if let Some((field, tail)) = latest_field(&root, stmts[0].trim())
            && field == "assets"
            && let Some(open) = strip_map_open(tail)
            && let Some(avar) = open
                .trim()
                .strip_prefix("do")
                .and_then(|r| r.trim().strip_prefix('|'))
                .and_then(|r| r.strip_suffix('|'))
                .map(str::trim)
                .filter(|v| !v.is_empty() && v.chars().all(|c| c.is_alphanumeric() || c == '_'))
        {
            let (afield, atail) = latest_field(avar, stmts[1].trim())?;
            let source = match afield.as_str() {
                "name" => LatestSource::AssetName,
                "browser_download_url" => LatestSource::AssetDownloadUrl,
                _ => return None,
            };
            let chain = atail.strip_prefix("&.[](regex, 1)")?;
            return Some(base(source, false, latest_tr(chain)?));
        }
    }
    // Match form:
    //   match = <json>["<field>"]&.match(regex)
    //   next if match.blank?
    //   <render>
    if stmts.len() == 3 {
        let (var, rhs) = assign_parts(&stmts[0])?;
        let (field, tail) = latest_field(&root, rhs)?;
        if tail != "&.match(regex)" || !blank_guard(&stmts[1], &var) {
            return None;
        }
        let (render, tr) = latest_match_render(&stmts[2], &var, None, None)?;
        return Some(with_render(
            base(latest_source(&field)?, false, None),
            render,
            tr,
        ));
    }
    // Two-capture form (name vs tag):
    //   a = <json>["<fa>"]&.[](regex, 1)
    //   b = <json>["<fb>"]&.[](regex, 1)
    //   next if a.blank? || b.blank?
    //   (a == b) ? b : "#{a},#{b}"
    if stmts.len() == 4 {
        let cap_src = |stmt: &str| -> Option<(String, LatestSource)> {
            let (var, rhs) = assign_parts(stmt)?;
            let (field, tail) = latest_field(&root, rhs)?;
            if tail != "&.[](regex, 1)" {
                return None;
            }
            Some((var, latest_source(&field)?))
        };
        if let (Some((a, sa)), Some((b, sb))) = (cap_src(&stmts[0]), cap_src(&stmts[1])) {
            let guard = stmts[2].trim();
            if guard != format!("next if {a}.blank? || {b}.blank?")
                && guard != format!("next if {b}.blank? || {a}.blank?")
            {
                return None;
            }
            if stmts[3].trim() != format!("({a} == {b}) ? {b} : \"#{{{a}}},#{{{b}}}\"") {
                return None;
            }
            let mut blk = base(sa, false, None);
            blk.render = LatestRender::TwoEq;
            blk.two_b = Some(sb);
            return Some(blk);
        }
    }
    // Optional pre-capture prefix (assets form only):
    //   <local> = <json>["<field>"]<capture-or-ops>
    //   next if <local>.blank?
    let parse_pre = |stmt: &str| -> Option<(String, LatestPre)> {
        let (var, rhs) = assign_parts(stmt)?;
        let (field, tail) = latest_field(&root, rhs)?;
        let source = latest_source(&field)?;
        // `&.match(regex)` / `&.match(/lit/flags)`
        // (clock-bar's second capture): same first
        // capture as the bracket form.
        if let Some(arg) = tail
            .strip_prefix("&.match(")
            .or_else(|| tail.strip_prefix(".match("))
        {
            let arg = arg.trim_start();
            let lit = if arg == "regex)" {
                None
            } else {
                let (pat, flags, t) = regex_literal(arg)?;
                if t.trim() != ")" {
                    return None;
                }
                Some((pat, flags))
            };
            return Some((
                var,
                LatestPre {
                    source,
                    lit,
                    capture: true,
                    ops: Vec::new(),
                },
            ));
        }
        if let Some(rest) = tail
            .strip_prefix("&.[](")
            .or_else(|| tail.strip_prefix(".[]("))
            .or_else(|| tail.strip_prefix('['))
        {
            let rest = rest.trim_start();
            let lit = if let Some(t) = rest.strip_prefix("regex") {
                let t = t.trim();
                if !(t == ", 1)" || t == ", 1]" || t == ",1)" || t == ",1]") {
                    return None;
                }
                None
            } else {
                let (pat, flags, t) = regex_literal(rest)?;
                let t = t.trim();
                if !(t == ", 1)" || t == ", 1]" || t == ",1)" || t == ",1]") {
                    return None;
                }
                Some((pat, flags))
            };
            return Some((
                var,
                LatestPre {
                    source,
                    lit,
                    capture: true,
                    ops: Vec::new(),
                },
            ));
        }
        // Plain string ops over the field text.
        let mut rest = tail;
        let mut ops = Vec::new();
        while !rest.is_empty() {
            if let Some(r) = rest
                .strip_prefix("&.tr(")
                .or_else(|| rest.strip_prefix(".tr("))
            {
                let (a, t) = quoted_token(r.trim_start())?;
                let t = t.trim().strip_prefix(',')?.trim();
                let (b, t) = quoted_token(t)?;
                if a.contains('\\') || b.contains('\\') || a.starts_with('^') {
                    return None;
                }
                ops.push(LatestPreOp::Tr(a, b));
                rest = t.trim().strip_prefix(')')?;
            } else if let Some(r) = rest
                .strip_prefix("&.sub(")
                .or_else(|| rest.strip_prefix(".sub("))
            {
                let (pat, flags, t) = regex_literal(r.trim_start())?;
                let t = t.trim().strip_prefix(',')?.trim();
                let (rep, t) = quoted_token(t)?;
                if rep.contains('\\') {
                    return None;
                }
                ops.push(LatestPreOp::SubRe(pat, flags, rep));
                rest = t.trim().strip_prefix(')')?;
            } else if let Some(r) = rest
                .strip_prefix("&.delete_prefix(")
                .or_else(|| rest.strip_prefix(".delete_prefix("))
            {
                let (a, t) = quoted_token(r.trim_start())?;
                ops.push(LatestPreOp::DeletePrefix(a));
                rest = t.trim().strip_prefix(')')?;
            } else {
                let r = rest
                    .strip_prefix("&.gsub(")
                    .or_else(|| rest.strip_prefix(".gsub("))?;
                // Only an anchored literal deletion
                // (`gsub(/^v/, "")`) is data.
                let (pat, _flags, t) = regex_literal(r.trim_start())?;
                let lit = pat.strip_prefix('^')?;
                if lit.is_empty() || lit.chars().any(|c| "\\.^$*+?()[]{}|".contains(c)) {
                    return None;
                }
                let t = t.trim().strip_prefix(',')?.trim();
                let (b, t) = quoted_token(t)?;
                if !b.is_empty() {
                    return None;
                }
                ops.push(LatestPreOp::DeletePrefix(lit.to_string()));
                rest = t.trim().strip_prefix(')')?;
            }
        }
        if ops.is_empty() {
            return None;
        }
        Some((
            var,
            LatestPre {
                source,
                lit: None,
                capture: false,
                ops,
            },
        ))
    };
    // Match over a transformed field local
    // (ultimaker-cura):
    //   tag = <json>["tag_name"]&.sub(/^\D+/, "")
    //   match = tag&.match(regex)
    //   next if match.blank?
    //   <render over match + tag>
    if stmts.len() == 4
        && let Some((lv, p)) = parse_pre(stmts[0].trim())
        && !p.capture
        && let Some((var, rhs)) = assign_parts(&stmts[1])
        && (rhs == format!("{lv}&.match(regex)") || rhs == format!("{lv}.match(regex)"))
        && blank_guard(&stmts[2], &var)
    {
        let (render, tr) = latest_match_render(&stmts[3], &var, Some(&lv), None)?;
        let mut blk = with_render(base(p.source, false, None), render, tr);
        let source = p.source;
        blk.source = source;
        blk.pre_capture = Some(p);
        blk.match_pre = true;
        return Some(blk);
    }
    // Two field matches, second as a local
    // (clock-bar):
    //   a = <json>["tag_name"]&.match(regex); guard
    //   b = <json>["body"]&.match(/lit/i); guard
    //   "#{a[1]},#{b}"
    if stmts.len() == 5
        && let Some((var, rhs)) = assign_parts(&stmts[0])
        && let Some((field, tail)) = latest_field(&root, rhs)
        && tail == "&.match(regex)"
        && blank_guard(&stmts[1], &var)
        && let Some((lv, p)) = parse_pre(stmts[2].trim())
        && p.capture
        && blank_guard(&stmts[3], &lv)
    {
        let source = latest_source(&field)?;
        // The local is a MatchData; renders read
        // `<local>[1]`, which is the modeled capture.
        let lv1 = format!("{lv}[1]");
        let (render, tr) = latest_match_render(&stmts[4], &var, Some(&lv1), None)?;
        let mut blk = with_render(base(source, false, None), render, tr);
        blk.pre_capture = Some(p);
        return Some(blk);
    }
    // Asset-name pick with a count guard (recaf):
    //   match = <json>["tag_name"]&.match(regex); guard
    //   assets = <json>["assets"]&.select do |asset|
    //     %w[…].include?(asset["name"])
    //   end
    //   next if assets.blank? || assets.length != N
    //   "#{match[1]},#{assets.map { … }.max.gsub(/\D/, "")}"
    if stmts.len() == 7
        && let Some((var, rhs)) = assign_parts(&stmts[0])
        && let Some((field, tail)) = latest_field(&root, rhs)
        && tail == "&.match(regex)"
        && blank_guard(&stmts[1], &var)
        && let Some((picked, prhs)) = assign_parts(&stmts[2])
        && let Some((pfield, ptail)) = latest_field(&root, prhs)
        && pfield == "assets"
        && let Some(avar) = ptail
            .strip_prefix("&.select do |")
            .or_else(|| ptail.strip_prefix(".select do |"))
            .and_then(|r| r.strip_suffix('|'))
        && let Some(names) = (|| {
            let line = stmts[3].trim();
            let inner = line.strip_prefix("%w[")?;
            let (list, rest) = inner.split_once(']')?;
            if rest.trim() != format!(".include?({avar}[\"name\"])") {
                return None;
            }
            Some(
                list.split_whitespace()
                    .map(str::to_string)
                    .collect::<Vec<_>>(),
            )
        })()
        && !names.is_empty()
        && stmts[4] == "end"
        && let Some(count) = stmts[5]
            .strip_prefix(&format!("next if {picked}.blank? || {picked}.length != "))
            .and_then(|n| n.trim().parse::<usize>().ok())
    {
        let source = latest_source(&field)?;
        let (render, tr) = latest_match_render(&stmts[6], &var, None, Some(&picked))?;
        let mut blk = with_render(base(source, false, None), render, tr);
        blk.asset_pick = Some((names, count));
        return Some(blk);
    }
    let (body, pre): (&[String], Option<(String, LatestPre)>) = if stmts.len() >= 7 {
        match parse_pre(stmts[0].trim()) {
            Some((var, p)) if stmts[1] == format!("next if {var}.blank?") => {
                (&stmts[2..], Some((var, p)))
            }
            _ => (&stmts[..], None),
        }
    } else {
        (&stmts[..], None)
    };
    let local_owned: Option<String> = pre.as_ref().map(|(v, _)| v.clone());
    let local = local_owned.as_deref();
    // Assets form:
    //   <json>["assets"]&.map do |asset|
    //     match = asset["<key>"]&.match(regex)
    //     next if match.blank?
    //     [next <expr> if match[a] == match[b]]
    //     <render>
    //   end
    if (body.len() == 5 || body.len() == 6 || body.len() == 9) && body[body.len() - 1] == "end" {
        let (field, tail) = latest_field(&root, body[0].trim())?;
        if field != "assets" {
            return None;
        }
        let open = strip_map_open(tail)?.trim();
        let avar = open
            .strip_prefix("do")?
            .trim()
            .strip_prefix('|')?
            .strip_suffix('|')?;
        let avar = avar.trim();
        if avar.is_empty() || !avar.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return None;
        }
        let (var, rhs) = assign_parts(&body[1])?;
        let (afield, atail) = latest_field(avar, rhs)?;
        let source = match afield.as_str() {
            "name" => LatestSource::AssetName,
            "browser_download_url" => LatestSource::AssetDownloadUrl,
            _ => return None,
        };
        if atail != "&.match(regex)" || !blank_guard(&body[2], &var) {
            return None;
        }
        let mut blk = base(source, false, None);
        blk.pre_capture = pre.map(|(_, p)| p);
        if body.len() == 6 {
            // `next <expr> if <var>[a] == <var>[b]`
            let early = body[3].trim().strip_prefix("next ")?;
            let (expr, cond) = early.rsplit_once(" if ")?;
            let cap_of = |tok: &str| -> Option<usize> {
                tok.trim()
                    .strip_prefix(&format!("{var}["))?
                    .strip_suffix(']')?
                    .parse()
                    .ok()
            };
            let (a, b) = cond.split_once(" == ")?;
            let render = LatestRender::EqElse {
                a: LatestOp::Cap(cap_of(a)?),
                b: LatestOp::Cap(cap_of(b)?),
                then: latest_parts(expr, &var, local, None)?,
                els: latest_parts(&body[4], &var, local, None)?,
            };
            return Some(with_render(blk, render, None));
        }
        if body.len() == 9 {
            // `if <op> == <op>` / then / `else` / els /
            // `end` (creality-print).
            let cond = body[3].trim().strip_prefix("if ")?;
            let (a, b) = cond.split_once(" == ")?;
            if body[5] != "else" || body[7] != "end" {
                return None;
            }
            let render = LatestRender::EqElse {
                a: latest_op(a, &var, local)?,
                b: latest_op(b, &var, local)?,
                then: latest_parts(&body[4], &var, local, None)?,
                els: latest_parts(&body[6], &var, local, None)?,
            };
            return Some(with_render(blk, render, None));
        }
        let (render, tr) = latest_match_render(&body[3], &var, local, None)?;
        return Some(with_render(blk, render, tr));
    }
    None
}

/// A dig tail of the form `&.split("<sep>")&.first`
/// (either navigation spelling on either call): the
/// literal separator (non-empty, backslash-free).
fn split_first_tail(cur: &str) -> Option<String> {
    let rest = cur
        .strip_prefix("&.split(")
        .or_else(|| cur.strip_prefix(".split("))?;
    let (sep, tail) = quoted_token(rest)?;
    let tail = tail.trim().strip_prefix(')')?;
    if !(tail == "&.first" || tail == ".first") || sep.is_empty() || sep.contains('\\') {
        return None;
    }
    Some(sep)
}

/// The multi-capture map forms: `match.join(".")`
/// and jasp's `match[1] ? "#{match[0]},#{match[1]}"
/// : match[0]`.
fn page_join_shape(var: &str, inner: &str) -> Option<super::formula::PageShape> {
    use super::formula::PageShape;
    if let Some(args) = inner
        .strip_prefix(&format!("{var}.join("))
        .or_else(|| inner.strip_prefix(&format!("{var}&.join(")))
    {
        let (sep, tail) = quoted_token(args)?;
        if tail.trim() != ")" || sep.contains('\\') {
            return None;
        }
        return Some(PageShape::ScanJoin(sep));
    }
    let csv = format!("{var}[1] ? \"#{{{var}[0]}},#{{{var}[1]}}\" : {var}[0]");
    (inner == csv).then_some(PageShape::ScanCsvOpt)
}

/// Reduce a `:header_match` strategy block: a
/// guarded match/scan over one response header
/// rendered as a template.
/// Rewrite `(<name> = <var>[N])…` capture aliases
/// onto `<var>[N]` (identifier-boundary aware).
fn alias_capture_rewrite(var: &str, tail: &str) -> String {
    let Some(rest) = tail.strip_prefix('(') else {
        return tail.to_string();
    };
    let Some((name, rhs)) = rest.split_once(" = ") else {
        return tail.to_string();
    };
    let name = name.trim();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return tail.to_string();
    }
    let Some(close) = rhs.find(')') else {
        return tail.to_string();
    };
    let cap = rhs[..close].trim();
    if page_cap_index(var, cap).is_none() {
        return tail.to_string();
    }
    let body = format!("{cap}{}", &rhs[close + 1..]);
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '.';
    let mut out = String::new();
    let mut rest = body.as_str();
    while let Some(i) = rest.find(name) {
        let before = rest[..i].chars().last();
        let after = rest[i + name.len()..].chars().next();
        out.push_str(&rest[..i]);
        if before.is_none_or(|c| !ident(c))
            && after.is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_')
        {
            out.push_str(cap);
        } else {
            out.push_str(name);
        }
        rest = &rest[i + name.len()..];
    }
    out.push_str(rest);
    out
}

fn parse_header_shape(params: &str, lines: &[String]) -> Option<super::formula::HeaderBlock> {
    use super::formula::{HeaderBlock, HeaderShape, PagePart};
    let (root, has_regex) = block_params(params)?;
    // `all_headers.each` first match (recut):
    // `version = nil all_headers.each do |headers|
    // match = headers["location"]&.match(regex)
    // next unless match version = "…" break end
    // version`.
    if root == "all_headers" && has_regex {
        let joined = join_block_lines(lines);
        let body = joined.trim();
        let (vvar, rest) = body.split_once(" = nil ")?;
        let vvar = vvar.trim();
        if vvar.is_empty() || !vvar.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
        let rest = rest.trim().strip_prefix(&root)?;
        let rest = rest
            .trim()
            .strip_prefix("&.each do |")
            .or_else(|| rest.trim().strip_prefix(".each do |"))?;
        let (hvar, rest) = rest.split_once('|')?;
        let hvar = hvar.trim();
        let (mvar, rest) = assign_parts(rest.trim())?;
        let rest = rest.trim().strip_prefix(hvar)?;
        let (header, tail) = json_bracket_key(rest)?;
        let tail = tail
            .trim()
            .strip_prefix("&.match(regex)")
            .or_else(|| tail.trim().strip_prefix(".match(regex)"))?;
        let tail = tail
            .trim()
            .strip_prefix(&format!("next unless {mvar}"))
            .or_else(|| tail.trim().strip_prefix(&format!("next if {mvar}.blank?")))?;
        let (assigned, render) = assign_parts(tail.trim())?;
        if assigned != vvar {
            return None;
        }
        let render = render.trim();
        // The render ends where the loop close
        // (`break end <vvar>`) begins; a quoted
        // render ends at its closing quote.
        let (parts, tail) = if render.starts_with('"') {
            let (body_str, t) = quoted_token(render)?;
            (page_template_parts(&body_str, &mvar)?, t.trim())
        } else {
            let (head, tail) = render.split_at(render.find(" break ")?);
            let r = head.trim().strip_prefix(&format!("{mvar}["))?;
            let close = r.find(']')?;
            let n: usize = r[..close].parse().ok()?;
            if !r[close + 1..].trim().is_empty() {
                return None;
            }
            (vec![PagePart::Cap(n)], tail.trim_start())
        };
        // Both renders must stop the loop: without a break the final variable
        // holds the last matching response, which this first-match shape cannot model.
        let tail = tail.strip_prefix("break ")?.trim();
        if tail != format!("end {vvar}") {
            return None;
        }
        return Some(HeaderBlock {
            header,
            shape: HeaderShape::AllTemplate(parts),
        });
    }
    if root != "headers" || !has_regex {
        // The bare form declares only `|headers|`.
        if root != "headers" {
            return None;
        }
    }
    let joined = join_block_lines(lines);
    let body = joined.trim();
    // `headers["h"]` head: returns (header, tail).
    let head = |s: &str| -> Option<(String, String)> {
        let rest = s.strip_prefix(&root)?;
        let (header, tail) = json_bracket_key(rest)?;
        Some((header, tail.trim().to_string()))
    };
    // Bare header value.
    if let Some((header, tail)) = head(body)
        && tail.is_empty()
    {
        return Some(HeaderBlock {
            header,
            shape: HeaderShape::Bare,
        });
    }
    // Scan-map form (no assignment).
    if let Some((header, tail)) = head(body)
        && let Some(rest) = tail
            .strip_prefix("&.scan(regex)")
            .or_else(|| tail.strip_prefix(".scan(regex)"))
    {
        let rest = rest.trim();
        let rest = rest
            .strip_prefix("&.map")
            .or_else(|| rest.strip_prefix(".map"))?
            .trim();
        let inner = if let Some(r) = rest.strip_prefix('{') {
            r.strip_suffix('}')?.trim()
        } else {
            let r = rest.strip_prefix("do ")?;
            r.strip_suffix(" end")?.trim()
        };
        let inner = inner.strip_prefix('|')?;
        let var_end = inner.find('|')?;
        let var = inner[..var_end].trim();
        let expr = inner[var_end + 1..].trim();
        let (body_str, t) = quoted_token(expr)?;
        if !t.trim().is_empty() {
            return None;
        }
        return Some(HeaderBlock {
            header,
            shape: HeaderShape::ScanTemplate(page_template_parts(&body_str, var)?),
        });
    }
    // Assignment forms.
    let (var, rest) = body.split_once(" = ")?;
    let var = var.trim();
    if var.is_empty() || !var.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let (header, tail) = head(rest.trim())?;
    let (flatten, tail) = if let Some(t) = tail
        .strip_prefix("&.scan(regex)&.flatten")
        .or_else(|| tail.strip_prefix("&.scan(regex).flatten"))
    {
        (true, t.trim())
    } else {
        let t = tail
            .strip_prefix("&.match(regex)")
            .or_else(|| tail.strip_prefix(".match(regex)"))?;
        (false, t.trim())
    };
    let tail = tail
        .strip_prefix(&format!("next if {var}.blank?"))
        .or_else(|| tail.strip_prefix(&format!("next unless {var}")))?
        .trim();
    // `(version = match[1]).include?(".") ?
    // version : version.chars.join(".")`: rewrite
    // the alias onto the capture, then fork.
    let tail = &alias_capture_rewrite(var, tail);
    let tail = tail.as_str();
    if !flatten && let Some((cond, then, els)) = page_fork_parts(var, tail) {
        return Some(HeaderBlock {
            header,
            shape: HeaderShape::Fork { cond, then, els },
        });
    }
    // CSV-optional form.
    let csv_opt = format!("{var}[2] ? \"#{{{var}[1]}},#{{{var}[2]}}\" : {var}[1]");
    let csv_opt2 = format!("{var}[2].present? ? \"#{{{var}[1]}},#{{{var}[2]}}\" : {var}[1]");
    if !flatten && (tail == csv_opt || tail == csv_opt2) {
        return Some(HeaderBlock {
            header,
            shape: HeaderShape::CsvOpt,
        });
    }
    let parts = if tail.starts_with('"') {
        let (body_str, t) = quoted_token(tail)?;
        if !t.trim().is_empty() {
            return None;
        }
        page_template_parts(&body_str, var)?
    } else {
        let r = tail.strip_prefix(&format!("{var}["))?;
        let close = r.find(']')?;
        let n: usize = r[..close].parse().ok()?;
        let t = r[close + 1..].trim();
        if t.is_empty() {
            vec![PagePart::Cap(n)]
        } else {
            let (a, b) = sp_tr_args(t)?;
            vec![PagePart::CapTr(n, a, b)]
        }
    };
    Some(HeaderBlock {
        header,
        shape: if flatten {
            HeaderShape::ScanFlatten(parts)
        } else {
            HeaderShape::Template(parts)
        },
    })
}

/// Parse a `"#{match[N]}…"` interpolation template
/// into page parts (`var` is the match variable).
/// One capture part from its trailing chain:
/// ``, `.tr(a, b)`, `.delete_prefix("p")`,
/// `.split("", L).join("s")`, `.chars.join("s")`,
/// `.sub(/pat/flags, 'tmpl')`.
fn page_cap_part(n: usize, tail: &str) -> Option<super::formula::PagePart> {
    use super::formula::PagePart;
    let tail = tail.trim();
    if tail.is_empty() {
        return Some(PagePart::Cap(n));
    }
    if let Some(rest) = tail
        .strip_prefix(".split(\"\", ")
        .or_else(|| tail.strip_prefix("&.split(\"\", "))
    {
        let (l_str, t) = rest.split_once(')')?;
        let limit: usize = l_str.trim().parse().ok()?;
        let t = t.trim().strip_prefix(".join(")?;
        let (sep, t) = quoted_token(t.trim())?;
        if t.trim() != ")" || limit == 0 {
            return None;
        }
        return Some(PagePart::CapSplitJoin(n, limit, sep));
    }
    if let Some(rest) = tail
        .strip_prefix(".chars.join(")
        .or_else(|| tail.strip_prefix("&.chars.join("))
    {
        let (sep, t) = quoted_token(rest.trim())?;
        if t.trim() != ")" {
            return None;
        }
        return Some(PagePart::CapSplitJoin(n, 0, sep));
    }
    if let Some(rest) = tail
        .strip_prefix(".delete_prefix(")
        .or_else(|| tail.strip_prefix("&.delete_prefix("))
    {
        let (p, t) = quoted_token(rest.trim())?;
        if t.trim() != ")" {
            return None;
        }
        return Some(PagePart::CapDeletePrefix(n, p));
    }
    if let Some((pat, flags, tmpl)) = sub_backref_args(tail) {
        return Some(PagePart::CapSubBackref(n, pat, flags, tmpl));
    }
    // `[.tr(a, b)].split(sep)[.map(&:to_i)].join(j)`
    {
        let mut t = tail;
        let mut tr = None;
        if let Some(r) = t.strip_prefix(".tr(").or_else(|| t.strip_prefix("&.tr(")) {
            if let Some((a, r1)) = quoted_token(r.trim_start())
                && let Some(r1) = r1.trim().strip_prefix(',')
                && let Some((b, r2)) = quoted_token(r1.trim())
                && let Some(r2) = r2.trim().strip_prefix(')')
            {
                tr = Some((a, b));
                t = r2.trim();
            }
        }
        if let Some(r) = t
            .strip_prefix(".split(")
            .or_else(|| t.strip_prefix("&.split("))
            && let Some((sep, r)) = quoted_token(r.trim_start())
            && let Some(r) = r.trim().strip_prefix(')')
        {
            let r = r.trim();
            let (ints, r) = match r
                .strip_prefix(".map(&:to_i)")
                .or_else(|| r.strip_prefix("&.map(&:to_i)"))
            {
                Some(r) => (true, r.trim()),
                None => (false, r),
            };
            if (tr.is_some() || ints)
                && let Some(r) = r
                    .strip_prefix(".join(")
                    .or_else(|| r.strip_prefix("&.join("))
                && let Some((join, r)) = quoted_token(r.trim_start())
                && r.trim() == ")"
            {
                return Some(PagePart::CapNormInts(n, tr, sep, join, ints));
            }
        }
    }
    let (a, b) = sp_tr_args(tail)?;
    Some(PagePart::CapTr(n, a, b))
}

/// A fork arm: a template string, a bare capture,
/// or a capture with a `tr` chain.
fn page_expr_parts(var: &str, s: &str) -> Option<Vec<super::formula::PagePart>> {
    let s = s.trim();
    if s.starts_with('"') {
        let (body_str, tail) = quoted_token(s)?;
        if !tail.trim().is_empty() {
            return None;
        }
        return page_template_parts(&body_str, var);
    }
    let r = s.strip_prefix(&format!("{var}["))?;
    let close = r.find(']')?;
    let n: usize = r[..close].parse().ok()?;
    Some(vec![page_cap_part(n, &r[close + 1..])?])
}

/// A `<var>[N]` capture index at the whole of `s`.
fn page_cap_index(var: &str, s: &str) -> Option<usize> {
    s.trim()
        .strip_prefix(&format!("{var}["))
        .and_then(|r| r.strip_suffix(']'))
        .and_then(|n| n.trim().parse().ok())
}

/// An enumerable capture condition.
fn page_cond_parse(var: &str, s: &str) -> Option<super::formula::PageCond> {
    use super::formula::PageCond;
    let s = s.trim();
    if let Some((a_s, b_s)) = s.split_once(" && ") {
        return Some(PageCond::And(
            Box::new(page_cond_parse(var, a_s)?),
            Box::new(page_cond_parse(var, b_s)?),
        ));
    }
    let s = s
        .strip_prefix('(')
        .and_then(|r| r.strip_suffix(')'))
        .map_or(s, str::trim);
    // `match[N].count("c") >= K`
    if let Some(idx) = s.find(".count(")
        && let Some(n) = page_cap_index(var, &s[..idx])
    {
        let (lit, t) = quoted_token(s[idx + ".count(".len()..].trim())?;
        let mut chars = lit.chars();
        let (Some(c), None) = (chars.next(), chars.next()) else {
            return None;
        };
        let t = t.trim().strip_prefix(')')?.trim().strip_prefix(">=")?;
        let k: usize = t.trim().parse().ok()?;
        return Some(PageCond::CapCountGe(n, c, k));
    }
    // `match[N].include?("lit")`
    if let Some(idx) = s.find(".include?(")
        && let Some(n) = page_cap_index(var, &s[..idx])
    {
        let (lit, t) = quoted_token(s[idx + ".include?(".len()..].trim())?;
        if t.trim() != ")" {
            return None;
        }
        return Some(PageCond::CapIncludes(n, lit));
    }
    // `match[A] == match[B][/pat/flags]`
    if let Some((a_s, b_s)) = s.split_once(" == ")
        && let Some(a) = page_cap_index(var, a_s)
        && let Some(r) = b_s.trim().strip_prefix(&format!("{var}["))
        && let Some(close) = r.find(']')
        && let Ok(b) = r[..close].parse::<usize>()
        && let Some(lit_args) = r[close + 1..].trim().strip_prefix('[')
        && let Some((pat, flags, t)) = regex_literal(lit_args.trim())
        && t.trim() == "]"
    {
        return Some(PageCond::CapEqSub(a, b, pat, flags));
    }
    // `match[A] == match[B].tr(from, to)`
    if let Some((a_s, b_s)) = s.split_once(" == ")
        && let Some(a) = page_cap_index(var, a_s)
        && let Some(r) = b_s.trim().strip_prefix(&format!("{var}["))
        && let Some(close) = r.find(']')
        && let Ok(b) = r[..close].parse::<usize>()
        && let Some((from, to)) = sp_tr_args(r[close + 1..].trim())
    {
        return Some(PageCond::CapTrEq(a, b, from, to));
    }
    for (op, is_ne) in [(" != ", true), (" == ", false)] {
        if let Some((a_s, b_s)) = s.split_once(op) {
            let a = page_cap_index(var, a_s)?;
            if let Some(b) = page_cap_index(var, b_s) {
                return Some(if is_ne {
                    PageCond::CapNe(a, b)
                } else {
                    PageCond::CapEq(a, b)
                });
            }
            if is_ne {
                return None;
            }
            let (lit, t) = quoted_token(b_s.trim())?;
            if !t.trim().is_empty() {
                return None;
            }
            return Some(PageCond::CapEqLit(a, lit));
        }
    }
    if let Some(n) = s
        .strip_suffix(".blank?")
        .and_then(|r| page_cap_index(var, r))
    {
        return Some(PageCond::CapBlank(n));
    }
    if let Some(n) = s
        .strip_suffix(".present?")
        .and_then(|r| page_cap_index(var, r))
    {
        return Some(PageCond::CapPresent(n));
    }
    page_cap_index(var, s).map(PageCond::CapTruthy)
}

/// A ternary/if-else fork over captures, as
/// parts.
/// The byte end of one atomic page condition at
/// the start of `s` (for the early-`next` fork,
/// whose else arm follows the condition directly).
/// Verified by re-parsing the prefix.
fn page_cond_prefix_end(var: &str, s: &str) -> Option<usize> {
    let cap_end = |from: usize| -> Option<usize> {
        let rest = s[from..].strip_prefix('[')?;
        let close = rest.find(']')?;
        rest[..close].parse::<usize>().ok()?;
        Some(from + 1 + close + 1)
    };
    if !s.starts_with(var) {
        return None;
    }
    let mut end = cap_end(var.len())?;
    let tail = &s[end..];
    if let Some(t) = tail
        .strip_prefix(".present?")
        .or_else(|| tail.strip_prefix(".blank?"))
    {
        end = s.len() - t.len();
    } else if let Some(t) = tail.strip_prefix(".include?(") {
        let (lit, t2) = quoted_token(t.trim_start())?;
        let _ = lit;
        let t2 = t2.trim().strip_prefix(')')?;
        end = s.len() - t2.len();
    } else if let Some(op) = [" == ", " != "].iter().find(|op| tail.starts_with(**op)) {
        let rhs = &tail[op.len()..];
        if rhs.starts_with('"') {
            let (_, t) = quoted_token(rhs)?;
            end = s.len() - t.len();
        } else {
            if !rhs.starts_with(var) {
                return None;
            }
            let rrest = &rhs[var.len()..];
            let ridx = rrest.strip_prefix('[')?;
            let close = ridx.find(']')?;
            ridx[..close].parse::<usize>().ok()?;
            end = s.len() - (ridx.len() - close - 1);
            // Optional `.tr(a, b)` on the right capture.
            let t = &s[end..];
            if t.starts_with(".tr(") || t.starts_with("&.tr(") {
                let bytes = t.as_bytes();
                let mut quote: Option<u8> = None;
                let mut i = 0usize;
                let mut closed = None;
                while i < bytes.len() {
                    match (quote, bytes[i]) {
                        (Some(_), b'\\') => i += 1,
                        (Some(q), c) if c == q => quote = None,
                        (Some(_), _) => {}
                        (None, b'"' | b'\'') => quote = Some(bytes[i]),
                        (None, b')') => {
                            closed = Some(i + 1);
                            break;
                        }
                        _ => {}
                    }
                    i += 1;
                }
                end += closed?;
            }
        }
    }
    page_cond_parse(var, &s[..end])?;
    Some(end)
}

fn page_fork_parts(
    var: &str,
    inner: &str,
) -> Option<(
    super::formula::PageCond,
    Vec<super::formula::PagePart>,
    Vec<super::formula::PagePart>,
)> {
    let inner_owned = alias_capture_rewrite(var, inner.trim());
    let inner = inner_owned.trim();
    // Early-`next` form (scribus):
    // `next <then> if <cond> <els>`.
    if let Some(rest) = inner.strip_prefix("next ")
        && let Some(at) = json_outside_quotes(rest, " if ")
    {
        let then_s = &rest[..at];
        let cond_rest = &rest[at + 4..];
        if let Some(end) = page_cond_prefix_end(var, cond_rest) {
            let cond = page_cond_parse(var, &cond_rest[..end])?;
            let then = page_expr_parts(var, then_s)?;
            let els = page_expr_parts(var, cond_rest[end..].trim())?;
            return Some((cond, then, els));
        }
    }
    if let Some(rest) = inner.strip_prefix("if ") {
        let rest_owned = alias_capture_rewrite(var, rest.trim());
        let rest = rest_owned.trim();
        let c = json_outside_quotes(rest, " else ")?;
        let (left, els_s) = rest.split_at(c);
        let els_s = els_s[6..]
            .trim()
            .strip_suffix(" end")
            .unwrap_or(els_s[6..].trim());
        // The condition ends where the then-arm
        // (a template string or capture) begins; a
        // `.count("c") >= K` condition carries its
        // own quotes, so scan past its integer.
        let start = if let Some(ge) = left.find(">= ") {
            let after = &left[ge + 3..];
            let digits = after
                .find(|ch: char| !ch.is_ascii_digit())
                .unwrap_or(after.len());
            Some(ge + 3 + digits)
        } else {
            left.find('"')
                .or_else(|| left.rfind(&format!(") {var}[")).map(|i| i + 2))
        }?;
        let (cond_s, then_s) = left.split_at(start);
        let cond = page_cond_parse(var, cond_s)?;
        let then = page_expr_parts(var, then_s)?;
        let els = page_expr_parts(var, els_s)?;
        return Some((cond, then, els));
    }
    let q = json_outside_quotes(inner, " ? ")?;
    let (cond_s, rest) = inner.split_at(q);
    let rest = &rest[3..];
    let c = json_outside_quotes(rest, " : ")?;
    let cond = page_cond_parse(var, cond_s)?;
    let then = page_expr_parts(var, &rest[..c])?;
    let els = page_expr_parts(var, &rest[c + 3..])?;
    Some((cond, then, els))
}

fn page_fork(
    var: &str,
    inner: &str,
    wrap: &dyn Fn(
        super::formula::PageCond,
        Vec<super::formula::PagePart>,
        Vec<super::formula::PagePart>,
    ) -> super::formula::PageShape,
) -> Option<super::formula::PageShape> {
    let (cond, then, els) = page_fork_parts(var, inner)?;
    Some(wrap(cond, then, els))
}

fn page_template_parts(body: &str, var: &str) -> Option<Vec<super::formula::PagePart>> {
    use super::formula::PagePart;
    let mut parts = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.find("#{") {
        if i > 0 {
            parts.push(PagePart::Lit(rest[..i].to_string()));
        }
        // Brace/quote-aware token span (nested
        // `#{}` inside a quoted conditional does
        // not end it).
        let inner = &rest[i + 2..];
        let bytes = inner.as_bytes();
        let mut modes: Vec<Option<u8>> = vec![None];
        let mut k = 0usize;
        let mut j: Option<usize> = None;
        while k < bytes.len() {
            match modes.last().copied().flatten() {
                Some(q) => {
                    if bytes[k] == b'\\' {
                        k += 1;
                    } else if bytes[k] == q {
                        modes.pop();
                    } else if bytes[k] == b'#' && bytes.get(k + 1) == Some(&b'{') {
                        modes.push(None);
                        k += 1;
                    }
                }
                None => match bytes[k] {
                    b'"' | b'\'' => modes.push(Some(bytes[k])),
                    b'{' => modes.push(None),
                    b'}' => {
                        modes.pop();
                        if modes.is_empty() {
                            j = Some(k);
                            break;
                        }
                    }
                    _ => {}
                },
            }
            k += 1;
        }
        let j = j?;
        let token = inner[..j].trim();
        // `#{"<lit>" + match[N] if match[N]}` and
        // `#{"<template>" if match[N]}`.
        if token.starts_with('"')
            && let Some((lit, t)) = quoted_token(token)
        {
            let t = t.trim();
            let cond = if let Some(cap_s) = t.strip_prefix("+ ") {
                cap_s.split_once(" if ").and_then(|(a_s, b_s)| {
                    let n = page_cap_index(var, a_s)?;
                    (page_cap_index(var, b_s) == Some(n))
                        .then_some((n, vec![PagePart::Lit(lit.clone()), PagePart::Cap(n)]))
                })
            } else if let Some(b_s) = t.strip_prefix("if ") {
                page_cap_index(var, b_s).and_then(|n| Some((n, page_template_parts(&lit, var)?)))
            } else {
                None
            };
            if let Some((n, inner_parts)) = cond {
                parts.push(PagePart::CondCap(n, inner_parts));
                rest = &rest[i + 2 + j + 1..];
                continue;
            }
        }
        let r = token.strip_prefix(&format!("{var}["))?;
        let close = r.find(']')?;
        let n: usize = r[..close].parse().ok()?;
        let tail = r[close + 1..].trim();
        parts.push(page_cap_part(n, tail)?);
        rest = &rest[i + 2 + j + 1..];
    }
    if !rest.is_empty() {
        parts.push(PagePart::Lit(rest.to_string()));
    }
    Some(parts)
}

/// Scan-template bodies: a bare template string,
/// the `v = match[0].tr(...); match[1].present?
/// ? ... : v` CSV-optional form, and the
/// compact-join after a match[0] tr assignment.
fn page_scan_template_shape(var: &str, inner: &str) -> Option<super::formula::PageShape> {
    use super::formula::PageShape;
    let inner = inner.trim();
    // A leading capture alias statement
    // (`version = match[0] version.…`). Only a
    // plain identifier and a statement boundary
    // after the capture — the parenthesized
    // `(version = match[0]).count(…)` form is
    // rewritten later by `alias_capture_rewrite`.
    // The capture may carry a transform chain
    // (`match[0].tr(..).split(..)...`): extend the
    // capture end over it when it parses as one
    // capture part, so the substituted text
    // re-parses in render position.
    let chain_end = |rhs: &str, cap_end: usize| -> Option<usize> {
        let rest = &rhs[cap_end..];
        if !rest.starts_with('.') && !rest.starts_with("&.") {
            return None;
        }
        let bytes = rhs.as_bytes();
        let mut depth = 0i32;
        let mut quote: Option<u8> = None;
        let mut i = cap_end;
        while i < bytes.len() {
            match (quote, bytes[i]) {
                (Some(q), c) if c == q => quote = None,
                (Some(_), b'\\') => i += 1,
                (Some(_), _) => {}
                (None, b'"' | b'\'') => quote = Some(bytes[i]),
                (None, b'(') => depth += 1,
                (None, b')') => depth -= 1,
                (None, c) if c.is_ascii_whitespace() && depth == 0 => break,
                _ => {}
            }
            i += 1;
        }
        page_expr_parts(var, &rhs[..i])?;
        Some(i)
    };
    let inner_owned;
    let inner = if let Some((name, rhs)) = inner.split_once(" = ")
        && !name.trim().is_empty()
        && name
            .trim()
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        && let Some(cap_end) = rhs
            .trim()
            .strip_prefix(&format!("{var}["))
            .and_then(|r| r.find(']').map(|c| var.len() + 1 + c + 1))
        && page_cap_index(var, &rhs.trim()[..cap_end.min(rhs.trim().len())]).is_some()
        && let Some(cap_end) = rhs
            .trim()
            .get(cap_end..)
            .is_some_and(|t| t.is_empty() || t.starts_with(char::is_whitespace))
            .then_some(cap_end)
            .or_else(|| chain_end(rhs.trim(), cap_end))
    {
        let rhs = rhs.trim();
        let cap_text = &rhs[..cap_end];
        let name = name.trim();
        let ident = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '.';
        let mut out = String::new();
        let mut rest = rhs[cap_end..].trim();
        while let Some(i) = rest.find(name) {
            let before = rest[..i].chars().last();
            let after = rest[i + name.len()..].chars().next();
            out.push_str(&rest[..i]);
            if before.is_none_or(|c| !ident(c))
                && after.is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_')
            {
                out.push_str(cap_text);
            } else {
                out.push_str(name);
            }
            rest = &rest[i + name.len()..];
        }
        out.push_str(rest);
        inner_owned = out;
        inner_owned.trim()
    } else {
        inner
    };
    // `next if match[N]&.match?(/…/) <fork>`
    if let Some(rest) = inner.strip_prefix(&format!("next if {var}["))
        && let Some(close) = rest.find(']')
        && let Ok(skip_cap) = rest[..close].parse::<usize>()
        && let Some(args) = rest[close + 1..]
            .trim()
            .strip_prefix("&.match?(")
            .or_else(|| rest[close + 1..].trim().strip_prefix(".match?("))
        && let Some((skip_pat, skip_flags, t)) = regex_literal(args.trim())
        && let Some(after) = t.trim().strip_prefix(')')
    {
        let (cond, then, els) = page_fork_parts(var, after.trim())?;
        return Some(PageShape::ScanGuardedFork {
            skip_cap,
            skip_pat,
            skip_flags,
            cond,
            then,
            els,
        });
    }
    if let Some(shape) = page_fork(var, inner, &|cond, then, els| PageShape::ScanFork {
        cond,
        then,
        els,
    }) {
        return Some(shape);
    }
    // `match[0] = match[0].tr(a, b) match.compact.join(sep)`
    if let Some(rest) = inner.strip_prefix(&format!("{var}[0] = {var}[0]"))
        && let Some((a, b)) = sp_tr_args(rest.trim())
    {
        let tr_text = format!(".tr(\"{a}\", \"{b}\")");
        let tail = inner.split_once(&tr_text).map(|(_, t)| t.trim())?;
        let tail = tail.strip_prefix(&format!("{var}.compact.join("))?;
        let (sep, t) = quoted_token(tail.trim())?;
        if t.trim() == ")" {
            return Some(PageShape::ScanTrCompactJoin(a, b, sep));
        }
        return None;
    }
    if inner.starts_with('"') {
        let (body_str, tail) = quoted_token(inner)?;
        if !tail.trim().is_empty() {
            return None;
        }
        return Some(PageShape::ScanTemplate(page_template_parts(
            &body_str, var,
        )?));
    }
    // `v = match[0].tr(a, b) match[1].present? ? "#{v},#{match[1]}" : v`
    if let Some((local, rest)) = inner.split_once(" = ") {
        let local = local.trim();
        let after = rest.strip_prefix(&format!("{var}[0]"))?;
        let (a, b) = sp_tr_args(after.trim())?;
        let expect = format!("{var}[1].present? ? \"#{{{local}}},#{{{var}[1]}}\" : {local}");
        // The joined body keeps `match[0].tr("a", "b") <expect>`.
        let tr_text = format!(".tr(\"{a}\", \"{b}\")");
        let tail = inner
            .split_once(&tr_text)
            .map(|(_, t)| t.trim())
            .unwrap_or_default();
        if tail == expect {
            return Some(PageShape::ScanCsvOptTr(a, b));
        }
        return None;
    }
    // `match[1].present? ? "#{match[0]},#{match[1]}" : match[0]`
    // (the `present?` spelling of ScanCsvOpt).
    let csv = format!("{var}[1].present? ? \"#{{{var}[0]}},#{{{var}[1]}}\" : {var}[0]");
    if inner == csv {
        return Some(PageShape::ScanCsvOpt);
    }
    None
}

/// `match = page.match(regex)` bodies: a blank
/// guard then a MatchData template / single
/// capture read.
fn parse_page_match_template(root: &str, body: &str) -> Option<super::formula::PageShape> {
    use super::formula::{PagePart, PageShape};
    // Bare `page[regex, N]` first-capture body.
    if let Some(rest) = body.strip_prefix(&format!("{root}[regex, ")) {
        let n: usize = rest.strip_suffix(']')?.trim().parse().ok()?;
        return Some(PageShape::MatchTemplate(vec![PagePart::Cap(n)]));
    }
    let (var, rest) = body.split_once(" = ")?;
    let var = var.trim();
    if var.is_empty() || !var.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let rest = rest
        .trim()
        .strip_prefix(&format!("{root}.match(regex)"))?
        .trim();
    let rest = rest
        .strip_prefix(&format!("next if {var}.blank?"))
        .or_else(|| rest.strip_prefix(&format!("next unless {var}")))?
        .trim();
    if let Some(shape) = page_fork(var, rest, &|cond, then, els| PageShape::MatchFork {
        cond,
        then,
        els,
    }) {
        return Some(shape);
    }
    // `json = …::Json.parse_json(match[N]) json<dig>`
    if let Some((jname, rhs)) = rest.split_once(" = ")
        && let Some(after) = rhs
            .trim()
            .strip_prefix("Homebrew::Livecheck::Strategy::Json.parse_json(")
        && let Some(close) = after.find(')')
        && let Some(cap) = page_cap_index(var, after[..close].trim())
    {
        let (path, tail) = jt_dig(after[close + 1..].trim(), jname.trim(), &[])?;
        if !tail.trim().is_empty() || path.is_empty() {
            return None;
        }
        return Some(PageShape::MatchJsonDig { cap, path });
    }
    if rest.starts_with('"') {
        let (body_str, tail) = quoted_token(rest)?;
        if !tail.trim().is_empty() {
            return None;
        }
        return Some(PageShape::MatchTemplate(page_template_parts(
            &body_str, var,
        )?));
    }
    // `match[N]` / `match[N].tr(a, b)`.
    let r = rest.strip_prefix(&format!("{var}["))?;
    let close = r.find(']')?;
    let n: usize = r[..close].parse().ok()?;
    let tail = r[close + 1..].trim();
    let part = if tail.is_empty() {
        PagePart::Cap(n)
    } else {
        let (a, b) = sp_tr_args(tail)?;
        PagePart::CapTr(n, a, b)
    };
    Some(PageShape::MatchTemplate(vec![part]))
}

/// Reduce a `:yaml` strategy block body to a data shape:
/// a `yaml.dig("a", "b")` / index-path walk, optionally
/// with the regex's first capture applied.
/// A yaml render expression (bare cap or an
/// interpolation string over caps and root fields).
fn yaml_parts(expr: &str, mvar: &str, root: &str) -> Option<Vec<super::formula::YamlPart>> {
    use super::formula::YamlPart;
    let expr = expr.trim();
    if let Some(n) = expr
        .strip_prefix(&format!("{mvar}["))
        .and_then(|r| r.strip_suffix(']'))
    {
        return Some(vec![YamlPart::Cap(n.parse().ok()?)]);
    }
    if !expr.starts_with('"') {
        return None;
    }
    let (inner, tail) = quoted_token(expr)?;
    if !tail.trim().is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    let mut rest = inner.as_str();
    while let Some(start) = rest.find("#{") {
        if start > 0 {
            parts.push(YamlPart::Lit(rest[..start].to_string()));
        }
        let body = &rest[start + 2..];
        let end = body.find('}')?;
        let tok = body[..end].trim();
        if let Some(n) = tok
            .strip_prefix(&format!("{mvar}["))
            .and_then(|r| r.strip_suffix(']'))
        {
            parts.push(YamlPart::Cap(n.parse().ok()?));
        } else {
            let r = tok.strip_prefix(&format!("{root}["))?;
            let bracketed = format!("[{r}");
            let (key, t) = json_bracket_key(&bracketed)?;
            if !t.is_empty() {
                return None;
            }
            parts.push(YamlPart::Root(key));
        }
        rest = &body[end + 1..];
    }
    if !rest.is_empty() {
        parts.push(YamlPart::Lit(rest.to_string()));
    }
    Some(parts)
}

/// A yaml match render (template or `present?` fork).
fn yaml_render(expr: &str, mvar: &str, root: &str) -> Option<super::formula::YamlRender> {
    use super::formula::YamlRender;
    let expr = expr.trim();
    if let Some(r) = expr.strip_prefix(&format!("{mvar}["))
        && let Some((n, rest)) = r.split_once("].present? ? ")
    {
        let cap: usize = n.parse().ok()?;
        let (then, els) = if rest.starts_with('"') {
            let (_, tail) = quoted_token(rest)?;
            let used = rest.len() - tail.len();
            (
                yaml_parts(&rest[..used], mvar, root)?,
                yaml_parts(tail.trim().strip_prefix(':')?.trim(), mvar, root)?,
            )
        } else {
            let (then, els) = rest.split_once(" : ")?;
            (yaml_parts(then, mvar, root)?, yaml_parts(els, mvar, root)?)
        };
        return Some(YamlRender::PresentElse { cap, then, els });
    }
    Some(YamlRender::Template(yaml_parts(expr, mvar, root)?))
}

/// The statement-form yaml bodies (electron-builder):
/// a `files` map or a root match, each with the
/// blank guard then a render.
fn parse_yaml_stmts(
    root: &str,
    has_regex: bool,
    lines: &[String],
) -> Option<super::formula::YamlBlock> {
    use super::formula::{YamlBlock, YamlShape};
    let stmts: Vec<String> = lines
        .iter()
        .map(|l| strip_trailing_comment(l.trim()).trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    let ident = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_');
    // `match = <recv>["key"]&.match(regex)`.
    let parse_match = |stmt: &str, recv: &str| -> Option<(String, String)> {
        let (mvar, rhs) = stmt.split_once(" = ")?;
        if !ident(mvar) {
            return None;
        }
        let r = rhs.strip_prefix(&format!("{recv}["))?;
        let bracketed = format!("[{r}");
        let (key, tail) = json_bracket_key(&bracketed)?;
        if !(tail == "&.match(regex)" || tail == ".match(regex)") {
            return None;
        }
        Some((mvar.to_string(), key))
    };
    let first = stmts.first()?.as_str();
    let files_open = ["map", "filter_map"].iter().find_map(|m| {
        first
            .strip_prefix(&format!("{root}[\"files\"]&.{m} do |"))
            .and_then(|r| r.strip_suffix('|'))
    });
    if let Some(item) = files_open {
        if !ident(item) || stmts.len() != 5 || stmts[4] != "end" {
            return None;
        }
        let (mvar, key) = parse_match(&stmts[1], item)?;
        if stmts[2] != format!("next if {mvar}.blank?") || !has_regex {
            return None;
        }
        let render = yaml_render(&stmts[3], &mvar, root)?;
        return Some(YamlBlock {
            has_regex,
            shape: YamlShape::FilesMap { key, render },
        });
    }
    if stmts.len() == 3
        && let Some((mvar, key)) = parse_match(first, root)
    {
        if stmts[1] != format!("next if {mvar}.blank?") || !has_regex {
            return None;
        }
        let render = yaml_render(&stmts[2], &mvar, root)?;
        return Some(YamlBlock {
            has_regex,
            shape: YamlShape::RootMatch { key, render },
        });
    }
    None
}

/// `&.map { |item| <next guards> item["k"][&.to_s] }`
/// after a yaml dig path: the projected key and
/// item guards.
fn yaml_map_key(cur: &str) -> Option<(String, Vec<super::formula::JsonGuard>)> {
    let rest = strip_map_open(cur)?;
    let (var, inner) = json_map_parts(rest)?;
    let mut s = inner.trim();
    let mut guards = Vec::new();
    loop {
        let (unless, r) = if let Some(r) = s.strip_prefix("next if ") {
            (false, r)
        } else if let Some(r) = s.strip_prefix("next unless ") {
            (true, r)
        } else {
            break;
        };
        let (cond, r) = parse_json_cond(&var, r)?;
        guards.push(super::formula::JsonGuard { unless, cond });
        s = r.trim();
    }
    let (key, tail) = jt_field_tail(s, &var)?;
    if !matches!(tail.trim(), "" | ".to_s" | "&.to_s") {
        return None;
    }
    Some((key, guards))
}

fn parse_yaml_shape(params: &str, lines: &[String]) -> Option<super::formula::YamlBlock> {
    use super::formula::{JsonKey, YamlBlock, YamlShape, YamlTail};
    let (root, has_regex) = block_params(params)?;
    if let Some(b) = parse_yaml_stmts(&root, has_regex, lines) {
        return Some(b);
    }
    let joined = join_block_lines(lines);
    let body = joined.trim();
    let mut cur = body.strip_prefix(root.as_str())?;
    let mut path: Vec<JsonKey> = Vec::new();
    loop {
        if let Some(rest) = cur.strip_prefix(".dig(") {
            let close = rest.find(')')?;
            for arg in rest[..close].split(',') {
                let arg = arg.trim();
                if let Some(k) = arg.strip_prefix('"').and_then(|a| a.strip_suffix('"')) {
                    if k.contains(['"', '\\']) {
                        return None;
                    }
                    path.push(JsonKey::Key(k.to_string()));
                } else if let Ok(i) = arg.parse::<usize>() {
                    path.push(JsonKey::Idx(i));
                } else {
                    return None;
                }
            }
            cur = &rest[close + 1..];
        } else if cur.starts_with('[') {
            let (key, tail) = json_bracket_key(cur)?;
            path.push(JsonKey::Key(key));
            cur = tail;
        } else {
            break;
        }
    }
    if path.is_empty() {
        return None;
    }
    // Plain string tails on the dug value.
    let yaml_tail = |cur: &str| -> Option<YamlTail> {
        let (op, rest) = [
            ("delete_suffix", "&.delete_suffix("),
            ("delete_prefix", "&.delete_prefix("),
            ("sub", "&.sub("),
        ]
        .iter()
        .find_map(|(name, pat)| cur.strip_prefix(pat).map(|r| (*name, r)))?;
        let rest = rest.trim_start();
        match op {
            "delete_suffix" | "delete_prefix" => {
                let (a, t) = quoted_token(rest)?;
                if t.trim() != ")" {
                    return None;
                }
                Some(if op == "delete_suffix" {
                    YamlTail::DeleteSuffix(a)
                } else {
                    YamlTail::DeletePrefix(a)
                })
            }
            _ => {
                if rest.starts_with('/') || rest.starts_with("%r") {
                    let (pat, flags, t) = regex_literal(rest)?;
                    let t = t.trim().strip_prefix(',')?.trim();
                    let (b, t) = quoted_token(t)?;
                    if t.trim() != ")" || b.contains('\\') {
                        return None;
                    }
                    Some(YamlTail::SubRe(pat, flags, b))
                } else {
                    let (a, t) = quoted_token(rest)?;
                    let t = t.trim().strip_prefix(',')?.trim();
                    let (b, t) = quoted_token(t)?;
                    if t.trim() != ")" || b.contains('\\') {
                        return None;
                    }
                    Some(YamlTail::SubLit(a, b))
                }
            }
        }
    };
    let shape = if cur.is_empty() {
        YamlShape::Dig(path)
    } else if let Some((key, guards)) = yaml_map_key(cur) {
        YamlShape::MapKey { path, key, guards }
    } else if let Some(sep) = split_first_tail(cur) {
        YamlShape::DigSplitFirst(path, sep)
    } else if let Some(tail) = yaml_tail(cur) {
        YamlShape::DigTail(path, tail)
    } else {
        let rest = cur
            .strip_prefix("&.[](regex, 1)")
            .or_else(|| cur.strip_prefix(".[](regex, 1)"))?;
        if !rest.is_empty() || !has_regex {
            return None;
        }
        YamlShape::DigRegex(path)
    };
    Some(YamlBlock { has_regex, shape })
}

/// Reduce an `:extract_plist` strategy block body to
/// data: `items["<bundle-id>"]&.short_version` /
/// `.version`, optionally with the regex's first
/// capture applied.
fn parse_plist_shape(params: &str, lines: &[String]) -> Option<super::formula::PlistBlock> {
    use super::formula::{PlistBlock, PlistProp};
    let (root, has_regex) = block_params(params)?;
    let joined = join_block_lines(lines);
    let body = joined.trim();
    let after = body.strip_prefix(root.as_str())?;
    let (bundle_id, tail) = json_bracket_key(after)?;
    let (prop, rest) = if let Some(r) = tail
        .strip_prefix("&.short_version")
        .or_else(|| tail.strip_prefix(".short_version"))
    {
        (PlistProp::Short, r)
    } else {
        let r = tail
            .strip_prefix("&.version")
            .or_else(|| tail.strip_prefix(".version"))?;
        (PlistProp::Bundle, r)
    };
    let regex = if rest.is_empty() {
        false
    } else if rest == "&.[](regex, 1)" || rest == ".[](regex, 1)" {
        if !has_regex {
            return None;
        }
        true
    } else {
        return None;
    };
    Some(PlistBlock {
        has_regex,
        bundle_id,
        prop,
        regex,
    })
}

/// `["key"]` at the start of `s`: the key and the rest.
fn json_bracket_key(s: &str) -> Option<(String, &str)> {
    let rest = s.strip_prefix("[\"")?;
    let close = rest.find('"')?;
    let key = &rest[..close];
    if key.contains('\\') {
        return None;
    }
    let tail = rest[close + 1..].strip_prefix(']')?;
    Some((key.to_string(), tail))
}

/// Strip a `.map`/`&.map`/`.filter_map` opener, braced or
/// `do`-formed, returning what follows.
fn strip_map_open(s: &str) -> Option<&str> {
    let s = s.trim_start();
    for pre in ["&.map", "&.filter_map", ".map", ".filter_map"] {
        if let Some(rest) = s.strip_prefix(pre) {
            return Some(rest);
        }
    }
    None
}

/// Split an object-entries map body (`|key, item|`)
/// into its key/value variables and inner text.
fn json_entries_parts(s: &str) -> Option<(String, String, String)> {
    let s = s.trim();
    let inner = if let Some(r) = s.strip_prefix('{') {
        r.strip_suffix('}')?
    } else {
        s.strip_prefix("do ")?.strip_suffix(" end")?
    };
    let inner = inner.trim().strip_prefix('|')?;
    let var_end = inner.find('|')?;
    let vars: Vec<&str> = inner[..var_end].split(',').map(str::trim).collect();
    if vars.len() != 2
        || vars
            .iter()
            .any(|v| v.is_empty() || !v.chars().all(|c| c.is_alphanumeric() || c == '_'))
    {
        return None;
    }
    Some((
        vars[0].to_string(),
        vars[1].to_string(),
        inner[var_end + 1..].trim().to_string(),
    ))
}

/// Split a map body into its block variable and inner
/// expression text.
fn json_map_parts(s: &str) -> Option<(String, String)> {
    let s = s.trim();
    let inner = if let Some(r) = s.strip_prefix('{') {
        r.strip_suffix('}')?
    } else {
        s.strip_prefix("do ")?.strip_suffix(" end")?
    };
    let inner = inner.trim().strip_prefix('|')?;
    let var_end = inner.find('|')?;
    let var = inner[..var_end].trim().to_string();
    if var.is_empty() || !var.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    Some((var, inner[var_end + 1..].trim().to_string()))
}

/// The item access of a map body: its key (`None` = the
/// item itself) and whether a regex capture applies.
/// A literal-regex capture tail: `[/pat/flags, 1]` at
/// the start of `after` (the leading `[` included).
/// Returns (source, flags, chain tail).
fn literal_capture_tail(after: &str) -> Option<(String, String, &str)> {
    let inner = after.strip_prefix('[')?;
    let (pat, flags, rest) = regex_literal(inner)?;
    let rest = rest.trim().strip_prefix(',')?.trim();
    let rest = rest.strip_prefix('1')?.strip_prefix(']')?;
    Some((pat, flags, rest))
}

/// A `tr("from", "to")` call at the start of `args`
/// (just after `tr(`): the pair and the tail after `)`.
fn tr_args(args: &str) -> Option<(String, String, &str)> {
    let (from, rest) = quoted_token(args)?;
    let rest = rest.trim().strip_prefix(',')?.trim();
    let (to, rest) = quoted_token(rest)?;
    let rest = rest.trim().strip_prefix(')')?;
    if from.contains('\\') || to.contains('\\') || from.starts_with('^') {
        return None;
    }
    Some((from, to, rest))
}

/// A json projection's trailing chain: an optional
/// `tr`, then an optional backreference `sub`
/// (percona-server's `version[regex, 1]&.sub(...)`).
type JsonChain = (Option<(String, String)>, Option<(String, String, String)>);

fn json_chain(tail: &str) -> Option<JsonChain> {
    let tail = tail.trim();
    if tail.is_empty() {
        return Some((None, None));
    }
    if let Some(args) = tail
        .strip_prefix("&.tr(")
        .or_else(|| tail.strip_prefix(".tr("))
    {
        let (from, to, rest) = tr_args(args)?;
        let (tr2, sub) = json_chain(rest)?;
        if tr2.is_some() {
            return None;
        }
        return Some((Some((from, to)), sub));
    }
    let (pat, flags, tmpl) = sub_backref_args(tail)?;
    Some((None, Some((pat, flags, tmpl))))
}

/// A map projection: (item key, capture, trailing
/// chain). The capture is the block regex
/// (`[regex, 1]`, `&.[](regex, 1)`, the match form) or
/// a literal regex in the projection (`[/lit/, 1]`).
fn json_map_item(
    var: &str,
    inner: &str,
) -> Option<(Option<String>, super::formula::MapCapture, JsonChain)> {
    use super::formula::MapCapture;
    // `m = item["k"]&.match(regex) next if m.blank? m[1]`
    if let Some((mvar, rest)) = inner.split_once(" = ")
        && !mvar.contains(' ')
        && let Some(key) = rest_key(rest, var)
        && rest == format!("{var}[\"{key}\"]&.match(regex) next if {mvar}.blank? {mvar}[1]")
    {
        return Some((Some(key), MapCapture::BlockRegex, (None, None)));
    }
    let after = inner.strip_prefix(var)?;
    if after.is_empty() {
        return Some((None, MapCapture::None, (None, None)));
    }
    if let Some(r) = after.strip_prefix("[regex, 1]") {
        return Some((None, MapCapture::BlockRegex, json_chain(r)?));
    }
    if let Some((pat, flags, r)) = literal_capture_tail(after) {
        return Some((None, MapCapture::Literal(pat, flags), json_chain(r)?));
    }
    let (key, tail) = json_bracket_key(after)?;
    if tail.is_empty() {
        return Some((Some(key), MapCapture::None, (None, None)));
    }
    if let Some(r) = tail
        .strip_prefix("&.[](regex, 1)")
        .or_else(|| tail.strip_prefix("[regex, 1]"))
    {
        return Some((Some(key), MapCapture::BlockRegex, json_chain(r)?));
    }
    if let Some((pat, flags, r)) = literal_capture_tail(tail) {
        return Some((Some(key), MapCapture::Literal(pat, flags), json_chain(r)?));
    }
    Some((Some(key), MapCapture::None, json_chain(tail)?))
}

/// The hash-values map (mariadb-connector-c):
/// `<path>&.map { |_, g| g["k"]&.map { |item| … } }&.flatten`
/// — a guarded projection over each object value's
/// `k` array.
fn parse_values_map_shape(
    path: &[super::formula::JsonKey],
    cur: &str,
) -> Option<super::formula::JsonShape> {
    use super::formula::{JsonShape, MapGuard};
    let rest = strip_map_open(cur)?;
    let rest = rest.trim();
    let rest = rest
        .strip_suffix("&.flatten")
        .or_else(|| rest.strip_suffix(".flatten"))?;
    let rest = rest.trim().strip_prefix("do")?.trim().strip_prefix('|')?;
    let (params, body) = rest.split_once('|')?;
    let mut params = params.split(',');
    let _key_var = params.next()?;
    let group = params.next()?.trim();
    if params.next().is_some()
        || group.is_empty()
        || !group.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    let body = body.trim().strip_suffix("end")?.trim();
    let after = body.strip_prefix(group)?;
    let (inner_key, tail) = json_bracket_key(after)?;
    let rest = strip_map_open(tail)?;
    let (var, inner) = json_map_parts(rest)?;
    let guard = if let Some(guard) = json_map_guarded(&var, &inner) {
        guard
    } else {
        let (item_key, capture, (tr, sub)) = json_map_item(&var, &inner)?;
        MapGuard {
            item_key,
            capture,
            tr,
            sub,
            guards: Vec::new(),
        }
    };
    Some(JsonShape::ValuesMap(
        path.to_vec(),
        inner_key,
        Box::new(guard),
    ))
}

/// A guarded map body: zero or more `next if/unless`
/// guards (or one postfix guard) around a simple item
/// projection.
fn json_map_guarded(var: &str, inner: &str) -> Option<super::formula::MapGuard> {
    use super::formula::MapGuard;
    // Postfix form: `<projection> unless <cond>` /
    // `<projection> if <cond>`.
    for (sep, unless) in [(" unless ", false), (" if ", true)] {
        if let Some((left, right)) = inner.rsplit_once(sep)
            && let Some((item_key, capture, (tr, sub))) = json_map_item(var, left.trim())
            && let Some((cond, rest)) = parse_json_cond(var, right.trim())
            && rest.trim().is_empty()
        {
            return Some(MapGuard {
                item_key,
                capture,
                tr,
                sub,
                guards: vec![super::formula::JsonGuard { unless, cond }],
            });
        }
    }
    // Prefix guards, then the projection.
    let mut s = inner.trim();
    let mut guards = Vec::new();
    loop {
        let (unless, rest) = if let Some(r) = s.strip_prefix("next if ") {
            (false, r)
        } else if let Some(r) = s.strip_prefix("next unless ") {
            (true, r)
        } else {
            break;
        };
        let (cond, rest) = parse_json_cond(var, rest)?;
        guards.push(super::formula::JsonGuard { unless, cond });
        s = rest.trim();
    }
    if guards.is_empty() {
        return None;
    }
    let (item_key, capture, (tr, sub)) = json_map_item(var, s)?;
    Some(MapGuard {
        item_key,
        capture,
        tr,
        sub,
        guards,
    })
}

/// A `next if <atom> || !<atom> || …` guard split
/// into one skip guard per atom (parsed left to
/// right, so a `||` inside a regex literal never
/// splits): at least two atoms, fully consumed.
fn jt_next_if_or_guards(var: &str, cond: &str) -> Option<Vec<super::formula::JsonGuard>> {
    use super::formula::JsonGuard;
    let mut rest = cond.trim();
    let mut out = Vec::new();
    loop {
        let (unless, atom) = match rest.strip_prefix('!') {
            Some(r) => (true, r.trim_start()),
            None => (false, rest),
        };
        let (cond, tail) = parse_json_cond_atom(var, atom)?;
        out.push(JsonGuard { unless, cond });
        rest = tail.trim();
        if rest.is_empty() {
            break;
        }
        rest = rest.strip_prefix("||")?.trim_start();
        if rest.is_empty() {
            return None;
        }
    }
    (out.len() > 1).then_some(out)
}

/// One guard condition at the start of `s`, plus the
/// remainder: field comparisons, truthiness (with `||`
/// chains), and the version/downcase forms.
fn parse_json_cond<'a>(var: &str, s: &'a str) -> Option<(super::formula::JsonCond, &'a str)> {
    use super::formula::JsonCond;
    // `[lit, …].include?(<var>["f"])` does not start
    // with the item variable.
    if let Some(mut rest) = s.trim_start().strip_prefix('[') {
        let mut lits = Vec::new();
        loop {
            let (lit, r) = jt_lit(rest.trim())?;
            lits.push(lit);
            rest = r.trim();
            if let Some(r) = rest.strip_prefix(',') {
                rest = r;
            } else if let Some(r) = rest.strip_prefix(']') {
                rest = r;
                break;
            } else {
                return None;
            }
        }
        let rest = rest.trim().strip_prefix(".include?(")?;
        let (field, tail) = jt_field_tail(rest.trim(), var)?;
        let tail = tail.trim().strip_prefix(')')?;
        return Some((JsonCond::FieldIn(field, lits), tail));
    }
    if let Some((path, tail)) = jt_dig(s.trim_start(), var, &[])
        && path.len() > 1
        && let Some(rest) = tail
            .trim()
            .strip_prefix("&.blank?")
            .or_else(|| tail.trim().strip_prefix(".blank?"))
    {
        return Some((JsonCond::DigBlank(path), rest));
    }
    let (first, mut rest) = parse_json_cond_atom(var, s)?;
    let mut parts = vec![first];
    while let Some(r) = rest.trim_start().strip_prefix("|| ") {
        let (atom, r2) = parse_json_cond_atom(var, r.trim_start())?;
        parts.push(atom);
        rest = r2;
    }
    if parts.len() == 1 {
        return Some((parts.pop()?, rest));
    }
    // Only bare field accesses chain with `||` in the
    // reduced grammar.
    if !parts.iter().all(|p| matches!(p, JsonCond::Truthy(_))) {
        return None;
    }
    Some((JsonCond::Or(parts), rest))
}

/// The body of an `any?` element block after the
/// opening `|`: `<evar>| e["a"] == lit && …` up to
/// `}` (brace form) or `end` (do form). Returns the
/// equality pairs and the tail after the terminator.
fn jt_any_eq_body<'a>(
    rest: &'a str,
    do_form: bool,
) -> Option<(Vec<(String, super::formula::JsonLit)>, &'a str)> {
    let (evar, rest) = rest.split_once('|')?;
    let evar = evar.trim();
    if evar.is_empty() || !evar.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let rest = rest.trim();
    let (body, tail) = if do_form {
        // The first standalone `end` closes the
        // block (the equality grammar contains none).
        let mut from = 0;
        loop {
            let idx = from + rest[from..].find(" end")?;
            let after = &rest[idx + 4..];
            if after.is_empty()
                || !after.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
            {
                break (&rest[..idx], after);
            }
            from = idx + 4;
        }
    } else {
        let idx = rest.find('}')?;
        (&rest[..idx], &rest[idx + 1..])
    };
    let mut pairs = Vec::new();
    for atom in body.split(" && ") {
        let (f, ne, lit) = jt_field_cmp(atom.trim(), evar)?;
        if ne {
            return None;
        }
        pairs.push((f, lit));
    }
    if pairs.is_empty() {
        return None;
    }
    Some((pairs, tail))
}

fn parse_json_cond_atom<'a>(var: &str, s: &'a str) -> Option<(super::formula::JsonCond, &'a str)> {
    use super::formula::JsonCond;
    let after = s.strip_prefix(var)?;
    let (field, tail) = json_bracket_key(after)?;
    if let Some(rest) = tail.strip_prefix("&.start_with?(version.major_minor)") {
        return Some((JsonCond::StartsWithMajorMinor(field), rest));
    }
    if let Some(rest) = tail.strip_prefix("&.include?(") {
        let (lit, rest) = quoted_token(rest)?;
        let rest = rest.strip_prefix(')')?;
        return Some((JsonCond::Includes(field, lit), rest));
    }
    if let Some(rest) = tail
        .strip_prefix("&.end_with?(")
        .or_else(|| tail.strip_prefix(".end_with?("))
    {
        let (lit, rest) = quoted_token(rest)?;
        let rest = rest.strip_prefix(')')?;
        return Some((JsonCond::EndsWith(field, lit), rest));
    }
    if let Some(rest) = tail.strip_prefix("&.downcase&.include?(") {
        let (lit, rest) = quoted_token(rest)?;
        let rest = rest.strip_prefix(')')?;
        return Some((JsonCond::DowncaseIncludes(field, lit), rest));
    }
    if let Some(rest) = tail
        .strip_prefix("&.any?")
        .or_else(|| tail.strip_prefix(".any?"))
        && let Some(parsed) = (|| {
            let (block, rest) = take_bracketed(rest.trim(), '{', '}')?;
            let inner = block.strip_prefix('{')?.strip_suffix('}')?.trim();
            let inner = inner.strip_prefix('|')?;
            let (vvar, body) = inner.split_once('|')?;
            let vvar = vvar.trim();
            let body = body.trim().strip_prefix(vvar)?.trim();
            let arg = body
                .strip_prefix("=~")
                .or_else(|| {
                    body.strip_prefix("&.match?(")
                        .or_else(|| body.strip_prefix(".match?("))
                })?
                .trim();
            if let Some((pat, flags, t)) = regex_literal(arg) {
                // A `=~` argument ends at the block; a
                // `match?` argument at its `)`.
                if !(t.trim().is_empty() || t.trim() == ")") {
                    return None;
                }
                return Some((JsonCond::AnyMatchRe(field.clone(), pat, flags), rest));
            }
            let (pat, t) = quoted_token(arg)?;
            if !(t.trim().is_empty() || t.trim() == ")") {
                return None;
            }
            return Some((
                JsonCond::AnyMatchRe(field.clone(), pat, String::new()),
                rest,
            ));
        })()
    {
        return Some(parsed);
    }
    if let Some(rest) = tail
        .strip_prefix("&.match?(")
        .or_else(|| tail.strip_prefix(".match?("))
    {
        let rest = rest.trim_start();
        if let Some((pat, flags, t)) = regex_literal(rest) {
            let t = t.trim().strip_prefix(')')?;
            return Some((JsonCond::MatchRe(field, pat, flags), t));
        }
        let (lit, rest) = quoted_token(rest)?;
        let rest = rest.trim().strip_prefix(')')?;
        return Some((JsonCond::MatchLit(field, lit), rest));
    }
    let any_rest = tail
        .strip_prefix("&.any? { |")
        .or_else(|| tail.strip_prefix(".any? { |"))
        .map(|r| (r, false))
        .or_else(|| {
            tail.strip_prefix("&.any? do |")
                .or_else(|| tail.strip_prefix(".any? do |"))
                .map(|r| (r, true))
        });
    if let Some((rest, do_form)) = any_rest {
        let (pairs, rest) = jt_any_eq_body(rest, do_form)?;
        return Some((JsonCond::AnyEq(field, pairs), rest));
    }
    if let Some(rest) = tail.strip_prefix(".none? { |") {
        let (evar, rest) = rest.split_once('|')?;
        let evar = evar.trim();
        if evar.is_empty() || !evar.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
        let after = rest.trim_start().strip_prefix(evar)?;
        let (inner_field, tail2) = json_bracket_key(after)?;
        let rest = tail2.strip_prefix(".match?(regex) }")?;
        return Some((JsonCond::NoneMatch(field, inner_field), rest));
    }
    if let Some(rest) = tail
        .strip_prefix("&.blank?")
        .or_else(|| tail.strip_prefix(".blank?"))
    {
        return Some((JsonCond::FieldBlank(field), rest));
    }
    for (op, is_ne) in [(" == ", false), (" != ", true)] {
        if let Some(rest) = tail.strip_prefix(op) {
            let (lit, rest) = jt_lit(rest)?;
            let cond = if is_ne {
                JsonCond::Ne(field, lit)
            } else {
                JsonCond::Eq(field, lit)
            };
            return Some((cond, rest));
        }
    }
    Some((JsonCond::Truthy(field), tail))
}

/// The `["k"]` key right after `var` in `rest`.
fn rest_key(rest: &str, var: &str) -> Option<String> {
    let after = rest.strip_prefix(var)?;
    let (key, _) = json_bracket_key(after)?;
    Some(key)
}

/// Whether an `on_<release>` cask block applies on
/// this host (`:or_newer`/`:or_older` widen the exact
/// release the way brew's `on_system` does). Unknown
/// symbols and non-macOS hosts never match.
fn macos_release_block_matches(t: &str) -> bool {
    fn vkey(v: &str) -> Vec<u64> {
        v.split('.').map(|p| p.parse().unwrap_or(0)).collect()
    }
    let Some(rest) = t.strip_prefix("on_") else {
        return false;
    };
    let sym: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    let Some(want) = macos_symbol_version(&sym) else {
        return false;
    };
    let host = super::machine::Machine::this();
    let Some(have) = host.macos_version() else {
        return false;
    };
    let (mut w, mut h) = (vkey(&want), vkey(have));
    let len = w.len().max(h.len());
    w.resize(len, 0);
    h.resize(len, 0);
    use std::cmp::Ordering;
    match h.cmp(&w) {
        Ordering::Equal => true,
        Ordering::Greater => t.contains(":or_newer"),
        Ordering::Less => t.contains(":or_older"),
    }
}

/// The `version` a host-matching per-system block
/// declares (`on_arm`/`on_intel` by arch, `on_macos`/
/// `on_linux` by OS; any other `on_*` block does not
/// match). All enclosing conditionals must match.
fn cask_host_version(text: &str) -> Option<String> {
    let arch = super::machine::current_arch_symbol();
    let mut stack: Vec<bool> = Vec::new();
    for raw in text.lines() {
        let t = raw.trim();
        if t == "end" {
            stack.pop();
            continue;
        }
        if (t.starts_with("on_") || t.starts_with("language "))
            && (t.ends_with(" do") || t.contains(" do |"))
        {
            let matched = if t.starts_with("on_arm") {
                arch == "arm"
            } else if t.starts_with("on_intel") {
                arch == "intel"
            } else if t.starts_with("on_macos") {
                cfg!(target_os = "macos")
            } else if t.starts_with("on_linux") {
                cfg!(target_os = "linux")
            } else {
                macos_release_block_matches(t)
            };
            stack.push(matched);
            continue;
        }
        if line_opens_block(t) {
            stack.push(true);
            continue;
        }
        if !stack.is_empty()
            && stack.iter().all(|m| *m)
            && t.starts_with("version ")
            && let Some(v) = cask_quoted(t, "version")
        {
            return Some(v);
        }
    }
    None
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
        // The livecheck block's own `url` is not the
        // cask's download URL (it is parsed on its
        // own below).
        if line == "livecheck do" {
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
    // A version declared only inside host-matching
    // `on_arm`/`on_intel`/`on_macos` blocks is still
    // this host's file version (the record stays
    // marked `variations`-unsupported for install).
    if c.version.is_empty() {
        c.version = cask_host_version(text).unwrap_or_default();
    }
    let mut locals = cask_livecheck_locals(&lines);
    locals.push(("token".to_string(), token.to_string()));
    c.url = cask_interpolate(&c.url, &c.version, &locals, &mut unsupported);
    for artifact in &mut c.artifacts {
        interpolate_artifact(artifact, &c.version, &locals, &mut unsupported);
    }
    c.caveats = c.caveats.trim_start_matches('\u{1}').to_string();
    c.livecheck = parse_livecheck_block(text).map(|spec| {
        c.skip_livecheck = spec.skip.is_some();
        Box::new(spec)
    });
    c.no_autobump = text
        .lines()
        .any(|l| l.trim_start().starts_with("no_autobump!"));
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
fn interpolate_artifact(
    a: &mut Artifact,
    version: &str,
    locals: &[(String, String)],
    unsupported: &mut Option<&'static str>,
) {
    let mut go = |s: &mut String| *s = cask_interpolate(s, version, locals, unsupported);
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
#[path = "tap_tests.rs"]
mod tests;
