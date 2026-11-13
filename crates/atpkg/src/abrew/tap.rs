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

/// Whether a trimmed Ruby line opens a `do … end` block.
fn line_opens_block(t: &str) -> bool {
    t.ends_with(" do") || t.contains(" do |")
}

/// One quoted string token at the start of `s` (double quotes,
/// interpolation already refused by the caller).
fn quoted_token(s: &str) -> Option<(String, &str)> {
    let inner = s.strip_prefix('"')?;
    let end = inner.find('"')?;
    Some((inner[..end].to_string(), &inner[end + 1..]))
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

/// The contents and option letters of a regex literal at the
/// start of `s` (`/…/` or `%r{…}`), plus the unparsed tail.
fn regex_literal(s: &str) -> Option<(String, String, &str)> {
    let (open, close, rest) = match s.strip_prefix('/') {
        Some(r) => ('/', '/', r),
        None => ('{', '}', s.strip_prefix("%r{")?),
    };
    let mut depth = 1usize;
    let mut escaped = false;
    for (i, c) in rest.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
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
/// (name, url, explicit version, livecheck block)
pub type ResourceStanza = (
    String,
    String,
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
            out.push((name, url, version, livecheck));
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
pub fn parse_livecheck_block(text: &str) -> Option<super::formula::LivecheckSpec> {
    use super::formula::{LivecheckSpec, LivecheckUrl};
    let mut lines = text.lines();
    lines.find(|l| livecheck_block_open(l))?;
    let mut body: Vec<&str> = Vec::new();
    let mut depth = 1usize;
    for line in lines {
        let t = line.trim();
        if t == "end" {
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
        if t == "end" {
            depth -= 1;
            if let Some((sym, params, lines)) = &mut json_capture {
                if depth == 1 {
                    match sym.as_str() {
                        "xml" => spec.xml_block = parse_xml_shape(params, lines),
                        "yaml" => spec.yaml_block = parse_yaml_shape(params, lines),
                        _ => spec.json_block = parse_json_shape(params, lines),
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
            let allowed = |line: &str| {
                let mut rest = line;
                while let Some(i) = rest.find("#{") {
                    let Some(j) = rest[i + 2..].find('}') else {
                        return false;
                    };
                    let token = &rest[i + 2..i + 2 + j];
                    if !matches!(
                        token,
                        "LATEST_VERSION"
                            | "version"
                            | "version.major"
                            | "version.minor"
                            | "version.patch"
                            | "version.major_minor"
                            | "version.major_minor_patch"
                    ) {
                        return false;
                    }
                    rest = &rest[i + 2 + j + 1..];
                }
                true
            };
            if !(t.starts_with("url ") || t.starts_with("url(")) || !allowed(t) {
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
                    (
                        LivecheckUrl::Literal(
                            s.replace("#{LATEST_VERSION}", "<FORMULA_LATEST_VERSION>"),
                        ),
                        tail,
                    )
                };
                spec.url = Some(url);
                // Url options may continue on following lines
                // while the text ends with a comma.
                let mut opts = tail.trim().trim_end_matches(')').trim().to_string();
                while opts.ends_with(',') {
                    let next = body.get(idx)?;
                    idx += 1;
                    opts.push(' ');
                    opts.push_str(next.trim().trim_end_matches(')').trim());
                }
                let opts = opts.strip_prefix(',').unwrap_or(&opts).trim();
                if !opts.is_empty() {
                    // Split at commas outside quotes.
                    let mut parts: Vec<String> = Vec::new();
                    let mut cur = String::new();
                    let mut quote: Option<char> = None;
                    for c in opts.chars() {
                        match quote {
                            Some(q) if c == q => quote = None,
                            Some(_) => {}
                            None if c == '"' || c == '\'' => quote = Some(c),
                            None if c == ',' => {
                                parts.push(std::mem::take(&mut cur));
                                continue;
                            }
                            None => {}
                        }
                        cur.push(c);
                    }
                    parts.push(cur);
                    for part in parts {
                        let part = part.trim();
                        if part.is_empty() {
                            continue;
                        }
                        let Some((key, value)) = part.split_once(':') else {
                            spec.url_options = true;
                            continue;
                        };
                        if key.trim() != "user_agent" {
                            spec.url_options = true;
                            continue;
                        }
                        let value = value.trim();
                        if let Some(sym) = value.strip_prefix(':') {
                            if sym == "browser" {
                                spec.user_agent = Some(":browser".to_string());
                            } else {
                                spec.url_options = true;
                            }
                        } else if let Some((lit, _)) = quoted_token(value) {
                            spec.user_agent = Some(lit);
                        } else {
                            spec.url_options = true;
                        }
                    }
                }
            } else if let Some(rest) = t.strip_prefix("regex(") {
                let (pattern, flags, tail) = regex_literal(rest)?;
                if tail.trim() != ")" {
                    return None;
                }
                spec.regex = Some(pattern);
                spec.regex_flags = flags;
            } else if let Some(rest) = t.strip_prefix("strategy ") {
                let (sym, tail) = symbol_token(rest.trim_start())?;
                let tail = tail.trim();
                if !(tail.is_empty()
                    || tail == "do"
                    || (tail.starts_with("do |") && tail.ends_with('|')))
                {
                    return None;
                }
                if (sym == "json" || sym == "xml" || sym == "yaml") && opens {
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
fn parse_json_shape(params: &str, lines: &[String]) -> Option<super::formula::JsonBlock> {
    use super::formula::{JsonBlock, JsonKey, JsonShape};
    let (root, has_regex) = block_params(params)?;
    let joined = join_block_lines(lines);
    let body = joined.trim();
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
    if match_form {
        if !cur.is_empty() || !has_regex {
            return None;
        }
        return Some(JsonBlock {
            has_regex,
            shape: JsonShape::DigRegex(path),
        });
    }
    let shape = if cur.is_empty() {
        JsonShape::Dig(path)
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
    } else {
        let rest = strip_map_open(cur)?;
        let (var, inner) = json_map_parts(rest)?;
        if let Some((item_key, is_regex)) = json_map_item(&var, &inner) {
            if is_regex && !has_regex {
                return None;
            }
            if is_regex {
                JsonShape::MapRegex(path, item_key)
            } else {
                JsonShape::MapKey(path, item_key)
            }
        } else {
            let guard = json_map_guarded(&var, &inner)?;
            if guard.regex && !has_regex {
                return None;
            }
            JsonShape::MapGuarded(path, Box::new(guard))
        }
    };
    Some(JsonBlock { has_regex, shape })
}

/// Reduce a `:yaml` strategy block body to a data shape:
/// a `yaml.dig("a", "b")` / index-path walk, optionally
/// with the regex's first capture applied.
fn parse_yaml_shape(params: &str, lines: &[String]) -> Option<super::formula::YamlBlock> {
    use super::formula::{JsonKey, YamlBlock, YamlShape};
    let (root, has_regex) = block_params(params)?;
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
    let shape = if cur.is_empty() {
        YamlShape::Dig(path)
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
    for pre in ["&.map", "&.filter_map", ".map", ".filter_map"] {
        if let Some(rest) = s.strip_prefix(pre) {
            return Some(rest);
        }
    }
    None
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
fn json_map_item(var: &str, inner: &str) -> Option<(Option<String>, bool)> {
    // `m = item["k"]&.match(regex) next if m.blank? m[1]`
    if let Some((mvar, rest)) = inner.split_once(" = ")
        && !mvar.contains(' ')
        && let Some(key) = rest_key(rest, var)
        && rest == format!("{var}[\"{key}\"]&.match(regex) next if {mvar}.blank? {mvar}[1]")
    {
        return Some((Some(key), true));
    }
    let after = inner.strip_prefix(var)?;
    if after.is_empty() {
        return Some((None, false));
    }
    if let Some(r) = after.strip_prefix("[regex, 1]") {
        return r.is_empty().then_some((None, true));
    }
    let (key, tail) = json_bracket_key(after)?;
    match tail {
        "" => Some((Some(key), false)),
        "&.[](regex, 1)" => Some((Some(key), true)),
        _ => None,
    }
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
            && let Some((item_key, is_regex)) = json_map_item(var, left.trim())
            && let Some((cond, rest)) = parse_json_cond(var, right.trim())
            && rest.trim().is_empty()
        {
            return Some(MapGuard {
                item_key,
                regex: is_regex,
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
    let (item_key, is_regex) = json_map_item(var, s)?;
    Some(MapGuard {
        item_key,
        regex: is_regex,
        guards,
    })
}

/// One guard condition at the start of `s`, plus the
/// remainder: field comparisons, truthiness (with `||`
/// chains), and the version/downcase forms.
fn parse_json_cond<'a>(var: &str, s: &'a str) -> Option<(super::formula::JsonCond, &'a str)> {
    use super::formula::JsonCond;
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

fn parse_json_cond_atom<'a>(var: &str, s: &'a str) -> Option<(super::formula::JsonCond, &'a str)> {
    use super::formula::{JsonCond, JsonLit};
    let after = s.strip_prefix(var)?;
    let (field, tail) = json_bracket_key(after)?;
    if let Some(rest) = tail.strip_prefix("&.start_with?(version.major_minor)") {
        return Some((JsonCond::StartsWithMajorMinor(field), rest));
    }
    if let Some(rest) = tail.strip_prefix("&.downcase&.include?(") {
        let (lit, rest) = quoted_token(rest)?;
        let rest = rest.strip_prefix(')')?;
        return Some((JsonCond::DowncaseIncludes(field, lit), rest));
    }
    for (op, is_ne) in [(" == ", false), (" != ", true)] {
        if let Some(rest) = tail.strip_prefix(op) {
            let (lit, rest) = if let Some(r) = rest.strip_prefix("true") {
                (JsonLit::Bool(true), r)
            } else if let Some(r) = rest.strip_prefix("false") {
                (JsonLit::Bool(false), r)
            } else {
                let (lit, r) = quoted_token(rest)?;
                (JsonLit::Str(lit), r)
            };
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
#[path = "tap_tests.rs"]
mod tests;
