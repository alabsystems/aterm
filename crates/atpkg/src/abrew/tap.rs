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
        if let Some((a, b)) = expr.split_once(" || ") {
            // Either side may be a csv selector (nil
            // when the part is missing), bare
            // `version`, or a full chain.
            let side = |s: &str| -> Option<String> {
                let s = s.trim();
                if s == "version" {
                    return Some(version.to_string());
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
fn line_opens_block(t: &str) -> bool {
    t.ends_with(" do") || t.contains(" do |")
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
                    None => out.push('#'),
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
        } else if s.starts_with('[') {
            let mut out = String::from("[");
            let mut rest = &s[1..];
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
                .find(|c: char| matches!(c, ',' | '}' | ']' | ' ' | '\t' | '\n' | '\r'))
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
            } else if let Some(r) = rest.strip_prefix("=>") {
                used += 2;
                rest = r;
            } else {
                return None;
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
    // character class (`/[^/]+/`).
    let mut in_class = false;
    let mut class_fresh = false;
    for (i, c) in rest.char_indices() {
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
fn cask_livecheck_locals(lines: &[&str]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let set = |name: &str, value: String, out: &mut Vec<(String, String)>| {
        if let Some(slot) = out.iter_mut().find(|(n, _)| n == name) {
            slot.1 = value;
        } else {
            out.push((name.to_string(), value));
        }
    };
    for line in lines {
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
                            spec.page_block = parse_page_shape(params, lines)
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
                            spec.json_block = parse_json_shape(params, lines).or_else(|| {
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
            let is_regex = t.starts_with("regex(");
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
                            // abrew's fetch never sends
                            // `--compressed`, which is
                            // exactly brew's `compressed:
                            // false` behavior; the explicit
                            // opt-out is a no-op here.
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
                // A regex literal may span lines (the
                // `x`-mode alternation in neo4j); join
                // following body lines until it closes.
                // Continuations join raw: the pattern
                // source keeps its indentation (the
                // oracle's `Regexp#inspect` prints it).
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
                if strip_trailing_comment(&tail).trim() != ")" {
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
    if s.contains("following-sibling") || s.contains("..") {
        return None;
    }
    let (absolute, rest) = if let Some(r) = s.strip_prefix("//") {
        (false, r)
    } else if let Some(r) = s.strip_prefix('/') {
        (true, r)
    } else {
        return None;
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
                let pred = if let Some(v) = inner.strip_prefix("text()=") {
                    Some(XmlPred::TextEq(xml_pred_value(v)?))
                } else if inner.starts_with('@') && inner.contains(" and ") {
                    let mut all = Vec::new();
                    for term in inner.split(" and ") {
                        let term = term.trim();
                        if let Some(r) = term.strip_prefix("not(@") {
                            all.push((r.strip_suffix(')')?.trim().to_string(), None));
                        } else if let Some(r) = term.strip_prefix('@') {
                            let (attr, v) = r.split_once('=')?;
                            all.push((attr.trim().to_string(), Some(xml_pred_value(v.trim())?)));
                        } else {
                            return None;
                        }
                    }
                    Some(XmlPred::AttrAll(all))
                } else if let Some(r) = inner.strip_prefix('@') {
                    let (attr, v) = r.split_once('=')?;
                    Some(XmlPred::AttrEq(
                        attr.trim().to_string(),
                        xml_pred_value(v.trim())?,
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
    Some(XmlPathQ { absolute, steps })
}

fn xml_pred_value(v: &str) -> Option<String> {
    let v = v.trim();
    if let Some(inner) = v.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')) {
        Some(inner.to_string())
    } else if let Some(inner) = v.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
        Some(inner.to_string())
    } else {
        None
    }
}

/// A `<root>.elements["PATH"]` / `.get_elements("PATH")`
/// query at the start of `s`: (is_all, path, tail).
fn xml_query<'a>(s: &'a str, root: &str) -> Option<(bool, super::formula::XmlPathQ, &'a str)> {
    let rest = s.trim().strip_prefix(root)?;
    let (all, rest) = if let Some(r) = rest.strip_prefix(".get_elements(") {
        (true, r)
    } else if let Some(r) = rest.strip_prefix(".elements[") {
        (false, r)
    } else {
        return None;
    };
    let (path_str, tail) = quoted_token(rest.trim_start())?;
    let tail = tail.trim_start();
    let tail = if all {
        tail.strip_prefix(')')?
    } else {
        tail.strip_prefix(']')?
    };
    Some((all, xml_path_parse(&path_str)?, tail))
}

/// A statement-form `:xml` block reduced to data:
/// first-element locals with blank guards and a
/// template, or a `get_elements` map projection.
fn parse_xml_template_shape(params: &str, lines: &[String]) -> Option<super::formula::XmlBlock> {
    use super::formula::{XmlBlock, XmlLocal, XmlMap, XmlProj, XmlRender, XmlShape, XmlTemplate};
    let (root, has_regex) = block_params(params)?;
    let stmts: Vec<String> = lines
        .iter()
        .map(|l| strip_trailing_comment(l.trim()).trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
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
        } else if let Some(r) = tail
            .strip_prefix("&.map")
            .or_else(|| tail.strip_prefix(".map"))
        {
            (false, r)
        } else {
            return None;
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
        let inner = if let Some(r) = joined.strip_prefix('{') {
            r.strip_suffix('}')?.trim().to_string()
        } else if let Some(r) = joined.strip_prefix("do ") {
            r.strip_suffix(" end")?.trim().to_string()
        } else {
            return None;
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
            let (mv, rest) = body.split_once(" = ")?;
            let mv = mv.trim();
            let rest = rest.trim().strip_prefix(&format!("{var}"))?;
            let rest = rest
                .strip_prefix(".text")
                .or_else(|| rest.strip_prefix(".next_element&.text"))?;
            let strip = rest.starts_with("&.strip");
            let rest = if strip {
                rest.strip_prefix("&.strip")?.trim()
            } else {
                rest.trim()
            };
            let proj_next = body.contains(".next_element");
            let rest = rest
                .strip_prefix("&.match(regex)")
                .or_else(|| rest.strip_prefix(".match(regex)"))?
                .trim();
            let rest = rest
                .strip_prefix(&format!("next if {mv}.blank?"))
                .or_else(|| rest.strip_prefix(&format!("next unless {mv}")))?
                .trim();
            let render = if let Some(r) = rest.strip_prefix(&format!("{mv}[")) {
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
                    require: Vec::new(),
                    match_local: None,
                    map: Some(XmlMap {
                        path,
                        proj: XmlProj::TextMatch(proj_next),
                        grep: false,
                        locals: Vec::new(),
                    }),
                    render,
                })),
            });
        }
        let after = body.strip_prefix(var)?.trim();
        let proj = if after == ".text" || after == ".text&.strip" || after == ".text.strip" {
            XmlProj::Text
        } else if after.starts_with(".next_element") {
            let t = after.strip_prefix(".next_element")?.trim();
            if !matches!(t, "&.text" | "&.text&.strip" | ".text" | ".text&.strip") {
                return None;
            }
            XmlProj::NextText
        } else if let Some(r) = after
            .strip_prefix(".attributes[")
            .or_else(|| after.strip_prefix("&.attributes["))
        {
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
        } else {
            return None;
        };
        if grep && !matches!(proj, XmlProj::Text | XmlProj::NextText | XmlProj::Attr(_)) {
            return None;
        }
        return Some(XmlBlock {
            has_regex,
            shape: XmlShape::Template(Box::new(XmlTemplate {
                locals: Vec::new(),
                require: Vec::new(),
                match_local: None,
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
    let mut require: Vec<usize> = Vec::new();
    let mut match_local: Option<usize> = None;
    let mut match_var = String::new();
    let mut match_guarded = false;
    let expr_local =
        |rhs: &str, locals: &mut Vec<XmlLocal>, names: &mut Vec<String>| -> Option<usize> {
            let (all, path, tail) = xml_query(rhs, &root)?;
            if all {
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
            locals.push(XmlLocal { path, next });
            names.push(String::new());
            Some(locals.len() - 1)
        };
    for stmt in &stmts[..stmts.len() - 1] {
        let stmt = stmt.as_str();
        if let Some(cond) = stmt.strip_prefix("next if ") {
            if cond == format!("{match_var}.blank?") && !match_var.is_empty() {
                match_guarded = true;
            } else if cond.contains(".blank?") {
                for part in cond.split(" || ") {
                    let name = part.trim().strip_suffix(".blank?")?;
                    require.push(names.iter().position(|n| n == name)?);
                }
            } else {
                return None;
            }
        } else if let Some(cond) = stmt.strip_prefix("next unless ") {
            if cond == match_var && !match_var.is_empty() {
                match_guarded = true;
            } else {
                return None;
            }
        } else if let Some((name, rhs)) = stmt.split_once(" = ") {
            let name = name.trim();
            if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return None;
            }
            let rhs = rhs.trim();
            // `<match> = <local>.match(regex) [if <local>]`
            if let Some(base) = rhs
                .strip_suffix("&.match(regex)")
                .or_else(|| rhs.strip_suffix(".match(regex)"))
            {
                let base = base.trim();
                let base = base.rsplit_once(" if ").map_or(base, |(b, _)| b.trim());
                match_local = Some(names.iter().position(|n| n == base)?);
                match_var = name.to_string();
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
    } else {
        // A bare first-element read.
        let idx = expr_local(final_stmt, &mut locals, &mut names)?;
        XmlRender::LocalValue(idx, false)
    };
    let uses_cap = match &render {
        XmlRender::Template(parts) => parts
            .iter()
            .any(|p| matches!(p, super::formula::XmlPart::Cap(_))),
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
            require,
            match_local,
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
            require,
            match_local: None,
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
            if !r[close + 1..].trim().is_empty() {
                return None;
            }
            XmlPart::Cap(r[..close].parse().ok()?)
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
    } else if let Some(shape) = parse_values_map_shape(&path, cur) {
        shape
    } else {
        let rest = strip_map_open(cur)?;
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
        } else if cur.starts_with('[') && !cur.starts_with("[regex") && !cur.starts_with("[/") {
            // A bare local as the key (`json[os]`).
            if let Some(close) = cur.find(']')
                && let Some((_, v)) = locals.iter().find(|(n, _)| n == cur[1..close].trim())
            {
                path.push(JsonKey::Key(v.clone()));
                cur = &cur[close + 1..];
                continue;
            }
            if let Some(rest) = cur.strip_prefix('[') {
                if let Ok(i) = rest
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

/// A local's source from an assignment RHS, or the
/// match marker for `….match(regex)`.
enum JtSrc {
    Local(super::formula::JsonLocal),
    Match(Vec<super::formula::JsonKey>),
}

fn jt_local_src(rhs: &str, root: &str, locals: &[(String, String)]) -> Option<JtSrc> {
    use super::formula::JsonLocal;
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
        JsonBlock, JsonCond, JsonGuard, JsonLit, JsonLocal, JsonPart, JsonRender, JsonShape,
        JsonTemplate, JsonValue,
    };
    let (root, has_regex) = block_params(params)?;
    let stmts: Vec<String> = lines
        .iter()
        .map(|l| strip_trailing_comment(l.trim()).trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    // Map wrapper?
    let (path, map, item_var, body): (Vec<super::formula::JsonKey>, bool, String, &[String]) = {
        let first = stmts.first()?;
        if let Some((p, tail)) = jt_dig(first, &root, cask_locals) {
            let tail = tail.trim();
            let inner = [
                ".map do |",
                "&.map do |",
                ".filter_map do |",
                "&.filter_map do |",
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
                (p, true, var.to_string(), &stmts[1..stmts.len() - 1])
            } else {
                (Vec::new(), false, root.clone(), &stmts[..])
            }
        } else {
            (Vec::new(), false, root.clone(), &stmts[..])
        }
    };
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
    let local_idx = |names: &[String], n: &str| names.iter().position(|x| x == n);
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
                    } else if name == "version" && local_idx(&names, name).is_none() {
                        // The package's own version
                        // (no local of that name):
                        // always present.
                    } else {
                        require.push(local_idx(&names, name)?);
                    }
                }
            } else if let Some((field, ne, lit)) = jt_field_cmp(cond, &item_var) {
                guards.push(JsonGuard {
                    unless: false,
                    cond: if ne {
                        JsonCond::Ne(field, JsonLit::Str(lit))
                    } else {
                        JsonCond::Eq(field, JsonLit::Str(lit))
                    },
                });
            } else {
                return None;
            }
        } else if let Some(cond) = stmt.strip_prefix("next unless ") {
            if cond == match_var && !match_var.is_empty() {
                match_guarded = true;
            } else if let Some(pos) = local_idx(&names, cond) {
                require.push(pos);
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
                    return None;
                }
            } else if let Some(pos) = cond
                .strip_suffix("&.match(regex)")
                .and_then(|n| local_idx(&names, n.trim()))
            {
                local_match_guard = Some(pos);
            } else if let Some(pos) = cond
                .strip_suffix(".match(regex)")
                .and_then(|n| local_idx(&names, n.trim()))
            {
                local_match_guard = Some(pos);
            } else {
                return None;
            }
        } else if let Some((name, rhs)) = stmt.split_once(" = ") {
            let name = name.trim();
            if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return None;
            }
            match jt_local_src(rhs, &item_var, cask_locals)? {
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
        &match_var,
        &mut locals,
        &mut names,
        cask_locals,
    )?;
    let uses_cap = match &render {
        JsonRender::Template(parts) => parts.iter().any(|p| matches!(p, JsonPart::Cap(_))),
        JsonRender::Value(JsonValue::Cap(_), _) => true,
        _ => false,
    };
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
            locals,
            require,
            match_src,
            local_match_guard,
            guards,
            render,
        })),
    })
}

/// `<var>["field"]` plus tail, for guard parsing.
fn jt_field_tail<'a>(s: &'a str, var: &str) -> Option<(String, &'a str)> {
    let rest = s.trim().strip_prefix(var)?;
    let (key, tail) = json_bracket_key(rest)?;
    Some((key, tail.trim()))
}

/// `<var>["f"] == "lit"` / `!=`: (field, is_ne, lit).
fn jt_field_cmp(s: &str, var: &str) -> Option<(String, bool, String)> {
    let (field, tail) = jt_field_tail(s, var)?;
    let (ne, rest) = if let Some(r) = tail.strip_prefix("!=") {
        (true, r)
    } else if let Some(r) = tail.strip_prefix("==") {
        (false, r)
    } else {
        return None;
    };
    let (lit, t) = quoted_token(rest.trim())?;
    if !t.trim().is_empty() {
        return None;
    }
    Some((field, ne, lit))
}

/// The final expression of a json template body.
fn jt_render(
    stmt: &str,
    item_var: &str,
    match_var: &str,
    locals: &mut Vec<super::formula::JsonLocal>,
    names: &mut Vec<String>,
    cask_locals: &[(String, String)],
) -> Option<super::formula::JsonRender> {
    use super::formula::{JsonPart, JsonRender, JsonValue};
    let stmt = stmt.trim();
    if stmt.starts_with('"') {
        let (body_str, tail) = quoted_token(stmt)?;
        if !tail.trim().is_empty() {
            return None;
        }
        let mut parts = Vec::new();
        let mut rest = body_str.as_str();
        while let Some(i) = rest.find("#{") {
            if i > 0 {
                parts.push(JsonPart::Lit(rest[..i].to_string()));
            }
            let j = rest[i + 2..].find('}')?;
            let token = rest[i + 2..i + 2 + j].trim();
            let part = if let Some(pos) = names.iter().position(|n| n == token) {
                JsonPart::Local(pos)
            } else if let Some(r) = token.strip_prefix(&format!("{match_var}[")) {
                let close = r.find(']')?;
                if !r[close + 1..].trim().is_empty() {
                    return None;
                }
                JsonPart::Cap(r[..close].parse().ok()?)
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
        return Some(JsonRender::Template(parts));
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
    let dir_template =
        |arg: &str, wname: &str| -> Option<(String, Option<(String, String)>, String)> {
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
fn parse_page_shape(params: &str, lines: &[String]) -> Option<super::formula::PageBlock> {
    use super::formula::{PageBlock, PageShape};
    let (root, has_regex) = block_params(params)?;
    if !has_regex {
        return None;
    }
    if let Some(shape) = parse_dirwalk_shape(&root, lines) {
        return Some(PageBlock { shape });
    }
    if let Some(shape) = parse_first_date_shape(&root, lines) {
        return Some(PageBlock { shape });
    }
    if let Some(shape) = parse_hopscan_shape(&root, lines) {
        return Some(PageBlock { shape });
    }
    let joined = join_block_lines(lines);
    // `match = page.match(regex)` + guarded template
    // or single capture (MatchData indexing).
    if let Some(shape) = parse_page_match_template(&root, joined.trim()) {
        return Some(PageBlock { shape });
    }
    let cur = joined.trim().strip_prefix(root.as_str())?;
    let cur = cur.strip_prefix(".scan(regex)")?;
    if let Some(shape) = page_select_shape(cur) {
        return Some(PageBlock { shape });
    }
    let rest = strip_map_open(cur)?;
    let (var, inner) = json_map_parts(rest)?;
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

/// A hop-scan's page-one pick: `page[/lit/, 1]`,
/// `page.scan(regex)&.flatten&.last`, or a literal
/// scan mapped to firsts and maxed by version.
/// Returns the pick data and the unparsed rest.
fn hop_pick<'a>(
    root: &str,
    rhs: &'a str,
) -> Option<(Option<(String, String)>, super::formula::HopPick, &'a str)> {
    use super::formula::HopPick;
    let after = rhs.trim().strip_prefix(root)?;
    if let Some(rest) = after.strip_prefix(".scan(regex)&.flatten&.last") {
        return Some((None, HopPick::ScanLast, rest));
    }
    if let Some((pat, flags, rest)) = literal_capture_tail(after) {
        return Some((Some((pat, flags)), HopPick::First, rest));
    }
    let inner = after.strip_prefix(".scan(")?;
    let (pat, flags, rest) = regex_literal(inner)?;
    let rest = rest.trim().strip_prefix(')')?.trim();
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
    if let Some(join) = arg.strip_prefix("URI.join(") {
        let (target, rest) = if let Some(r) = join.strip_prefix("@url") {
            (HopTarget::JoinSelf, r)
        } else if let Some((lit, r)) = quoted_token(join) {
            (HopTarget::JoinLiteral(lit), r)
        } else {
            return None;
        };
        let rest = rest.trim().strip_prefix(',')?.trim();
        let rest = rest.strip_prefix(var)?.trim().strip_prefix(')')?;
        let rest = rest.strip_prefix(".to_s").unwrap_or(rest);
        return rest.trim().is_empty().then_some(target);
    }
    let (template, tail) = quoted_token(arg)?;
    if !tail.trim().is_empty() {
        return None;
    }
    let slot = format!("#{{{var}}}");
    let (pre, post) = template.split_once(&slot)?;
    if pre.contains("#{") || post.contains("#{") {
        return None;
    }
    Some(HopTarget::Template(pre.to_string(), post.to_string()))
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

/// The final scan of a hop body: all captures, or
/// only the last (`&.flatten&.last`).
fn hop_final(content_root: &str, stmt: &str) -> Option<bool> {
    let rest = stmt
        .trim()
        .strip_prefix(content_root)?
        .strip_prefix(".scan(regex)")?;
    let rest = rest.trim();
    if rest == "&.flatten&.last" {
        return Some(true);
    }
    if rest == ".map(&:first)" {
        return Some(false);
    }
    let mrest = strip_map_open(rest)?;
    let (var, inner) = json_map_parts(mrest)?;
    let inner = inner.trim();
    (inner == format!("{var}[0]") || inner == format!("{var}.first")).then_some(false)
}

/// The second-fetch ("hop") bodies (kytea,
/// libxsd-frontend, gnutls, dovecot): pick a value
/// from page one, fetch the page it names, scan
/// that with the block regex. Parsed as a cursor
/// over the space-joined body (statement joining
/// miscounts parens around regex character classes
/// holding a lone quote).
fn parse_hopscan_shape(root: &str, lines: &[String]) -> Option<super::formula::PageShape> {
    use super::formula::{HopScan, PageShape};
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
        .or_else(|| rest.strip_prefix(&format!("next if {var}.blank?")))?;
    let (wvar, rest) = assign_parts(rest.trim())?;
    let rest = rest
        .trim()
        .strip_prefix("Homebrew::Livecheck::Strategy.page_content")?
        .trim_start();
    let (parens, rest) = take_bracketed(rest, '(', ')')?;
    let target = hop_target(&var, &parens[1..parens.len() - 1])?;
    let (rest, took_content) = match rest.trim().strip_prefix("[:content]") {
        Some(r) => (r, true),
        None => (rest, false),
    };
    let (content_root, rest) = hop_content_guard(&wvar, took_content, rest)?;
    let last_only = hop_final(&content_root, rest)?;
    Some(PageShape::HopScan(Box::new(HopScan {
        hop_re,
        pick,
        target,
        last_only,
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

/// A `[regex, N]` / `[](regex, N)` index tail.
fn sp_index_tail(tail: &str) -> Option<usize> {
    let inner = tail
        .strip_prefix("&.[](regex,")
        .or_else(|| tail.strip_prefix("[regex,"))?;
    let inner = inner.trim();
    let digits: usize = inner.find(|c: char| !c.is_ascii_digit())?;
    let (n, rest) = inner.split_at(digits);
    if !(rest.starts_with(']') || rest.starts_with(')')) {
        return None;
    }
    n.parse().ok()
}

/// `<expr>.tr("a", "b")` arguments.
fn sp_tr_args(tail: &str) -> Option<(String, String)> {
    let rest = tail.trim().strip_prefix(".tr(")?;
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
            sp_index_tail(tail).map(|n| SpExpr::FieldIndex(field, n))
        }
    }
}

/// Parse a `"#{...}"` template body into parts.
/// `match_var`/`local_name` are the body's capture
/// and precomputed local names.
fn sp_template_parts(
    body: &str,
    match_var: &str,
    local_name: Option<&str>,
) -> Option<Vec<super::formula::SpPart>> {
    use super::formula::SpPart;
    let mut parts = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.find("#{") {
        if i > 0 {
            parts.push(SpPart::Lit(rest[..i].to_string()));
        }
        let j = rest[i + 2..].find('}')?;
        let token = rest[i + 2..i + 2 + j].trim();
        let part = if Some(token) == local_name {
            SpPart::Local
        } else if let Some(r) = token.strip_prefix(&format!("{match_var}[")) {
            let close = r.find(']')?;
            let n: usize = r[..close].parse().ok()?;
            let tail = r[close + 1..].trim();
            match tail {
                "" => SpPart::Cap(n),
                _ => {
                    if let Some((a, b)) = sp_tr_args(tail) {
                        SpPart::CapTr(n, a, b)
                    } else if let Some(fb) = tail.strip_prefix(".presence ||") {
                        let (s, _) = quoted_token(fb.trim())?;
                        SpPart::CapOr(n, s)
                    } else {
                        return None;
                    }
                }
            }
        } else if let Some((field, tail)) = sp_field_tail(token, "item") {
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
        parts.push(SpPart::Lit(rest.to_string()));
    }
    Some(parts)
}

/// A channel condition (`item.channel.nil?`,
/// `== "x"`, `!= "x"`) as a keep-gate; `negated`
/// flips it (a `next if` skip vs a keep test).
fn sp_channel_gate(cond: &str, negated: bool) -> Option<super::formula::SpChannel> {
    use super::formula::SpChannel;
    let rest = cond.trim().strip_prefix("item.channel")?.trim();
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
            } else if let Some(v) = rest.strip_prefix("!=") {
                (false, v)
            } else {
                return None;
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
        "item" => sp_item_body(&stmts, false, None),
        "items" => sp_items_body(&stmts),
        _ => None,
    }
}

/// A bare sparkle render expression as template
/// parts (a capture, a field, or a quoted template).
fn sp_bare_parts(
    expr: &str,
    match_var: &str,
    local_name: Option<&str>,
) -> Option<Vec<super::formula::SpPart>> {
    use super::formula::SpPart;
    let expr = expr.trim();
    if expr.starts_with('"') {
        let (body_str, tail) = quoted_token(expr)?;
        if !tail.trim().is_empty() {
            return None;
        }
        return sp_template_parts(&body_str, match_var, local_name);
    }
    if let Some(r) = expr.strip_prefix(&format!("{match_var}[")) {
        let close = r.find(']')?;
        if !r[close + 1..].trim().is_empty() {
            return None;
        }
        return Some(vec![SpPart::Cap(r[..close].parse().ok()?)]);
    }
    let (field, tail) = sp_field_tail(expr, "item")?;
    if !tail.trim().is_empty() {
        return None;
    }
    Some(vec![SpPart::Field(field)])
}

/// One side of a sparkle `(a == b)` condition.
fn sp_op(tok: &str, match_var: &str) -> Option<super::formula::SpOp> {
    use super::formula::SpOp;
    let tok = tok.trim();
    if let Some(r) = tok.strip_prefix(&format!("{match_var}[")) {
        let inner = r.strip_suffix(']')?;
        return Some(SpOp::Cap(inner.parse().ok()?));
    }
    let (field, tail) = sp_field_tail(tok, "item")?;
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
        if mentions_channel(lhs) && is_nil(rhs) {
            if let Some(v) = value_of(lhs) {
                return Some((Some(SpChannel::EqOrNil(v)), None));
            }
        }
        if mentions_channel(rhs) && is_nil(lhs) {
            if let Some(v) = value_of(rhs) {
                return Some((Some(SpChannel::EqOrNil(v)), None));
            }
        }
        return None;
    }
    if let Some((a, b)) = cond.split_once(" && ") {
        let (ca, ma) = sp_find_gates(a)?;
        let (cb, mb) = sp_find_gates(b)?;
        return Some((ca.or(cb), ma.or(mb)));
    }
    if cond.contains("item.channel") {
        return Some((Some(sp_channel_gate(cond, false)?), None));
    }
    let (field, tail) = sp_field_tail(cond, "item")?;
    if tail.trim() == "&.match(regex)" || tail.trim() == ".match(regex)" {
        return Some((None, Some(field)));
    }
    None
}

/// The `|items|` body: whole-array `map`/`find`
/// wrappers around an item body.
fn sp_items_body(stmts: &[String]) -> Option<super::formula::SparkleBlock> {
    use super::formula::{SpExpr, SparkleBlock};
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
                expr: SpExpr::Field(sp_field(name)?),
            });
        }
        if let Some(rest) = first.strip_prefix("items.map { |item|") {
            let inner = rest.trim().strip_suffix('}')?.trim();
            return sp_item_body(&[inner.to_string()], true, None);
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
        let mut blk = sp_item_body(&stmts[3..], false, channel)?;
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
        let mut blk = sp_item_body(&stmts[1..], false, channel)?;
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
        return sp_item_body(&stmts[1..stmts.len() - 1], true, None);
    }
    None
}

/// The per-item body. `all` is the `|items|`
/// fan-out (results compact/uniq); single-item
/// bodies evaluate the newest matching item only.
fn sp_item_body(
    stmts: &[String],
    all: bool,
    preset_channel: Option<super::formula::SpChannel>,
) -> Option<super::formula::SparkleBlock> {
    use super::formula::{SpExpr, SpPart, SparkleBlock};
    let mut channel = preset_channel;
    let mut match_guard = None;
    let mut present: Vec<super::formula::SpField> = Vec::new();
    // An early `next <expr> if/unless <cond>` render.
    let mut early: Option<(bool, Option<usize>, Vec<SpPart>)> = None;
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
        if let Some(cond) = stmt.strip_prefix("next if item.channel") {
            channel = Some(sp_channel_gate(&format!("item.channel{cond}"), true)?);
        } else if let Some(cond) = stmt.strip_prefix("next unless item.channel") {
            channel = Some(sp_channel_gate(&format!("item.channel{cond}"), false)?);
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
                    .and_then(|r| sp_field_tail(r.trim(), "item"))
                    .filter(|(_, tail)| tail.trim().is_empty())
                    .map(|(f, _)| f)
                    .or_else(|| {
                        sp_field_tail(part, "item")
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
        } else if stmt == "next unless item" || stmt == "next if item.nil?" {
            // The assign-find miss: the evaluator's
            // gated pick is already empty then.
            if channel.is_none() && match_guard.is_none() {
                return None;
            }
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
            let cap = if cond == format!("{match_var}.blank?") {
                if unless {
                    return None;
                }
                None
            } else if let Some(r) = cond
                .strip_prefix(&format!("{match_var}["))
                .and_then(|r| r.strip_suffix(']'))
            {
                Some(r.parse().ok()?)
            } else {
                return None;
            };
            early = Some((
                unless,
                cap,
                sp_bare_parts(expr, &match_var, local_name.as_deref())?,
            ));
            guarded = true;
        } else if let Some(cond) = stmt.strip_prefix("next unless ") {
            if cond == match_var {
                guarded = true;
            } else if let Some((field, tail)) =
                sp_field_tail(cond, "item").filter(|_| cond.ends_with("match?(regex)"))
            {
                if tail != "&.match?(regex)" && tail != ".match?(regex)" {
                    return None;
                }
                match_guard = Some(field);
            } else {
                return None;
            }
        } else if let Some((var, rhs)) = stmt.split_once(" = ") {
            let (field, tail) = sp_field_tail(rhs.trim(), "item")?;
            let tail = tail.trim();
            if tail == "&.match(regex)" || tail == ".match(regex)" {
                match_var = var.trim().to_string();
                from = Some(field);
            } else if let Some(n) = sp_index_tail(tail) {
                local_name = Some(var.trim().to_string());
                local = Some((field, n));
            } else {
                return None;
            }
        } else {
            return None;
        }
    }
    let expr = if let Some(at) = scan_at {
        let stmt = &stmts[at];
        let (field, tail) = sp_field_tail(stmt, "item")?;
        let after = tail
            .trim()
            .strip_prefix(".scan(regex).map")
            .or_else(|| tail.trim().strip_prefix(".scan(regex)&.map"))?
            .trim();
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
            sp_template_parts(&body_str, "match", None)?
        } else if let Some(r) = template.trim().strip_prefix("match[") {
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
        } else {
            return None;
        };
        SpExpr::Scan { from: field, parts }
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
                    sp_bare_parts(&rest[..used], &match_var, local_name.as_deref())?,
                    tail.trim().strip_prefix(':')?.trim(),
                )
            } else {
                let (then, els) = rest.split_once(" : ")?;
                (sp_bare_parts(then, &match_var, local_name.as_deref())?, els)
            };
            SpExpr::EqElse {
                from,
                local,
                a: sp_op(a, &match_var)?,
                b: sp_op(b, &match_var)?,
                then: then_parts,
                els: sp_bare_parts(els_str, &match_var, local_name.as_deref())?,
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
            let parts = sp_template_parts(&body_str, &match_var, local_name.as_deref())?;
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
        } else if let Some((field, tail)) = sp_field_tail(stmt, "item") {
            sp_field_expr(field, tail)?
        } else {
            return None;
        }
    };
    // An early `next` wraps a template final form.
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
) -> Option<super::formula::GhRelOp> {
    use super::formula::GhRelOp;
    let tok = tok.trim();
    if Some(tok) == local {
        return Some(GhRelOp::Local);
    }
    if tok == format!("{rel}[\"tag_name\"]") {
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
) -> Option<Vec<super::formula::GhRelPart>> {
    use super::formula::{GhRelOp, GhRelPart};
    let expr = expr.trim();
    if let Some(op) = ghrel_operand(expr, mvar, local, rel) {
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
        if let Some(op) = ghrel_operand(tok, mvar, local, rel) {
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
        } else if let Some(mut sub) = ghrel_parts(tok, mvar, local, rel) {
            // A tag capture part (the only other
            // single-part form).
            if sub.len() != 1 || !matches!(sub[0], GhRelPart::TagCapture(..)) {
                return None;
            }
            parts.append(&mut sub);
        } else {
            return None;
        }
        rest = &body[end + 1..];
    }
    if !rest.is_empty() {
        parts.push(GhRelPart::Lit(rest.to_string()));
    }
    Some(parts)
}

/// A ghrel block's result expression as `GhRelThen`
/// (identity/tr fast paths, `EqElse`, templates).
fn ghrel_then(
    expr: &str,
    mvar: Option<&str>,
    local: Option<&str>,
    rel: &str,
) -> Option<super::formula::GhRelThen> {
    use super::formula::GhRelThen;
    let expr = expr.trim();
    if let Some(mv) = mvar {
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
        let a = ghrel_operand(a, mvar, local, rel)?;
        let b = ghrel_operand(b, mvar, local, rel)?;
        // Then-branch: a quoted string (extent known)
        // or a bare operand up to ` : `.
        let (then_parts, els) = if rest.starts_with('"') {
            let (_, tail) = quoted_token(rest)?;
            let used = rest.len() - tail.len();
            let els = tail.trim().strip_prefix(':')?.trim();
            (ghrel_parts(&rest[..used], mvar, local, rel)?, els)
        } else {
            let (then, els) = rest.split_once(" : ")?;
            (ghrel_parts(then, mvar, local, rel)?, els)
        };
        let else_parts = ghrel_parts(els, mvar, local, rel)?;
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
            (ghrel_parts(&rest[..used], mvar, local, rel)?, els)
        } else {
            let (then, els) = rest.split_once(" : ")?;
            (ghrel_parts(then, mvar, local, rel)?, els)
        };
        return Some(GhRelThen::TruthyElse {
            cap: n.parse().ok()?,
            then: then_parts,
            els: ghrel_parts(els, mvar, local, rel)?,
        });
    }
    Some(GhRelThen::Template(ghrel_parts(expr, mvar, local, rel)?))
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
    if let Some((v, rhs)) = stmts.first().and_then(|s| s.split_once(" = ")) {
        if !v.is_empty()
            && v.trim().chars().all(|c| c.is_alphanumeric() || c == '_')
            && let Ok((pat, flags, tail)) = regex_literal(rhs.trim()).ok_or(())
            && tail.trim().is_empty()
        {
            file_regex = Some((v.trim().to_string(), pat, flags));
            i = 1;
        }
    }
    // `<json>.map do |release|` (or filter_map/flat_map)
    let open = next(&mut i)?;
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
            } else if let Some(e) = test
                .strip_prefix(&format!("{a}[\"name\"]&.end_with?("))
                .or_else(|| test.strip_prefix(&format!("{a}[\"name\"].end_with?(")))
            {
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
            } else {
                return None;
            }
            i += 1;
        } else {
            break;
        }
    }
    // Optional release-level pre-capture local:
    // `<local> = <rel>["name"|"tag_name"]&.[](<re>, 1)`
    // and its blank guard.
    let mut pre_capture: Option<GhRelCapture> = None;
    let mut local_var: Option<String> = None;
    if let Some((v, rhs)) = stmts.get(i).and_then(|s| s.split_once(" = ")) {
        let parsed = (|| {
            if v.is_empty() || !v.chars().all(|c| c.is_alphanumeric() || c == '_') {
                return None;
            }
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
            Some((
                v.to_string(),
                GhRelCapture {
                    field: field.to_string(),
                    lit,
                },
            ))
        })();
        if let Some((v, cap)) = parsed {
            if stmts.get(i + 1).map(String::as_str) != Some(&format!("next if {v}.blank?")[..]) {
                return None;
            }
            local_var = Some(v);
            pre_capture = Some(cap);
            i += 2;
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
    if let Some(mv) = &mvar {
        let guard = squash(next(&mut i)?);
        if guard == format!("next if {mv}.blank?") || guard == format!("next unless {mv}") {
            // Plain blank guard, either spelling.
        } else if let Some(rest) = guard
            .strip_prefix(&format!("next if {mv}.blank? || {rel}[\"name\"]&.match?("))
            .or_else(|| {
                guard.strip_prefix(&format!("next if {mv}.blank? || {rel}[\"name\"].match?("))
            })
        {
            // Blank guard combined with the name skip.
            let (pat, flags, tail) = regex_literal(rest)?;
            if tail.trim() != ")" {
                return None;
            }
            name_skip = Some((pat, flags));
        } else {
            return None;
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
            a: ghrel_operand(a, mvar.as_deref(), local_var.as_deref(), &rel)?,
            b: ghrel_operand(b, mvar.as_deref(), local_var.as_deref(), &rel)?,
            then: ghrel_parts(early_expr, mvar.as_deref(), local_var.as_deref(), &rel)?,
            els: ghrel_parts(els_expr, mvar.as_deref(), local_var.as_deref(), &rel)?,
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
        ghrel_then(&expr, mvar.as_deref(), local_var.as_deref(), &rel)?
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
            {
                true
            } else if asset_filter
                && tail.len() == 2
                && tail[1] == "end.flatten"
                && let Some(rest) = tail[0].strip_prefix("end.presence || ")
            {
                match ghrel_parts(rest, mvar.as_deref(), local_var.as_deref(), &rel).as_deref() {
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
        if rest.len() != idx + 2 || rest[idx + 1] != "end" {
            return None;
        }
        let op = git_map_expr(&var, &rest[idx])?;
        return Some(GitBlock {
            needs_spec_regex: !has_regex,
            skip_tags,
            op,
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
) -> Option<Vec<super::formula::LatestPart>> {
    use super::formula::LatestPart;
    let expr = expr.trim();
    if Some(expr) == local {
        return Some(vec![LatestPart::Local]);
    }
    let cap_of = |tok: &str| -> Option<usize> {
        tok.strip_prefix(&format!("{var}["))
            .and_then(|r| r.strip_suffix(']'))
            .and_then(|n| n.parse().ok())
    };
    if let Some(n) = cap_of(expr) {
        return Some(vec![LatestPart::Cap(n)]);
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
        let end = body.find('}')?;
        let tok = body[..end].trim();
        if Some(tok) == local {
            parts.push(LatestPart::Local);
        } else {
            parts.push(LatestPart::Cap(cap_of(tok)?));
        }
        rest = &body[end + 1..];
    }
    if !rest.is_empty() {
        parts.push(LatestPart::Lit(rest.to_string()));
    }
    Some(parts)
}

/// Split a `X ? A : B` render tail (A quoted or bare).
fn latest_fork(
    rest: &str,
    var: &str,
    local: Option<&str>,
) -> Option<(
    Vec<super::formula::LatestPart>,
    Vec<super::formula::LatestPart>,
)> {
    if rest.starts_with('"') {
        let (_, tail) = quoted_token(rest)?;
        let used = rest.len() - tail.len();
        Some((
            latest_parts(&rest[..used], var, local)?,
            latest_parts(tail.trim().strip_prefix(':')?.trim(), var, local)?,
        ))
    } else {
        let (then, els) = rest.split_once(" : ")?;
        Some((
            latest_parts(then, var, local)?,
            latest_parts(els, var, local)?,
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
) -> Option<(super::formula::LatestRender, Option<(String, String)>)> {
    use super::formula::{LatestOp, LatestRender};
    let expr = expr.trim();
    if let Some(chain) = expr.strip_prefix(&format!("{var}[1]")) {
        if let Some(tr) = latest_tr(chain) {
            return Some((LatestRender::Capture, tr));
        }
    }
    if let Some(cond) = expr.strip_prefix('(') {
        let (a, rest) = cond.split_once(" == ")?;
        let (b, rest) = rest.split_once(") ? ")?;
        let op_of = |tok: &str| -> Option<LatestOp> {
            let tok = tok.trim();
            if Some(tok) == local {
                return Some(LatestOp::Local);
            }
            tok.strip_prefix(&format!("{var}["))
                .and_then(|r| r.strip_suffix(']'))
                .and_then(|n| n.parse().ok())
                .map(LatestOp::Cap)
        };
        let (then, els) = latest_fork(rest, var, local)?;
        return Some((
            LatestRender::EqElse {
                a: op_of(a)?,
                b: op_of(b)?,
                then,
                els,
            },
            None,
        ));
    }
    if let Some(r) = expr.strip_prefix(&format!("{var}[")) {
        if let Some((n, rest)) = r.split_once("].present? ? ") {
            let (then, els) = latest_fork(rest, var, local)?;
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
            let (then, els) = latest_fork(rest, var, local)?;
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
        LatestRender::Template(latest_parts(expr, var, local)?),
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
        let (then, els) = latest_fork(rest, var, None)?;
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
        LatestRender::ScanTemplate(latest_parts(inner, var, None)?),
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
        let (render, tr) = latest_match_render(&stmts[2], &var, None)?;
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
                .strip_prefix("&.delete_prefix(")
                .or_else(|| rest.strip_prefix(".delete_prefix("))
            {
                let (a, t) = quoted_token(r.trim_start())?;
                ops.push(LatestPreOp::DeletePrefix(a));
                rest = t.trim().strip_prefix(')')?;
            } else if let Some(r) = rest
                .strip_prefix("&.gsub(")
                .or_else(|| rest.strip_prefix(".gsub("))
            {
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
            } else {
                return None;
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
    if (body.len() == 5 || body.len() == 6) && body[body.len() - 1] == "end" {
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
                then: latest_parts(expr, &var, local)?,
                els: latest_parts(&body[4], &var, local)?,
            };
            return Some(with_render(blk, render, None));
        }
        let (render, tr) = latest_match_render(&body[3], &var, local)?;
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
fn parse_header_shape(params: &str, lines: &[String]) -> Option<super::formula::HeaderBlock> {
    use super::formula::{HeaderBlock, HeaderShape, PagePart};
    let (root, has_regex) = block_params(params)?;
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
        } else if let Some(r) = rest.strip_prefix("do ") {
            r.strip_suffix(" end")?.trim()
        } else {
            return None;
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
    } else if let Some(t) = tail
        .strip_prefix("&.match(regex)")
        .or_else(|| tail.strip_prefix(".match(regex)"))
    {
        (false, t.trim())
    } else {
        return None;
    };
    let tail = tail
        .strip_prefix(&format!("next if {var}.blank?"))
        .or_else(|| tail.strip_prefix(&format!("next unless {var}")))?
        .trim();
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
    } else if let Some(r) = tail.strip_prefix(&format!("{var}[")) {
        let close = r.find(']')?;
        let n: usize = r[..close].parse().ok()?;
        let t = r[close + 1..].trim();
        if t.is_empty() {
            vec![PagePart::Cap(n)]
        } else {
            let (a, b) = sp_tr_args(t)?;
            vec![PagePart::CapTr(n, a, b)]
        }
    } else {
        return None;
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
fn page_template_parts(body: &str, var: &str) -> Option<Vec<super::formula::PagePart>> {
    use super::formula::PagePart;
    let mut parts = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.find("#{") {
        if i > 0 {
            parts.push(PagePart::Lit(rest[..i].to_string()));
        }
        let j = rest[i + 2..].find('}')?;
        let token = rest[i + 2..i + 2 + j].trim();
        let r = token.strip_prefix(&format!("{var}["))?;
        let close = r.find(']')?;
        let n: usize = r[..close].parse().ok()?;
        let tail = r[close + 1..].trim();
        parts.push(if tail.is_empty() {
            PagePart::Cap(n)
        } else {
            let (a, b) = sp_tr_args(tail)?;
            PagePart::CapTr(n, a, b)
        });
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
        } else if let Some(r) = tok.strip_prefix(&format!("{root}[")) {
            let bracketed = format!("[{r}");
            let (key, t) = json_bracket_key(&bracketed)?;
            if !t.is_empty() {
                return None;
            }
            parts.push(YamlPart::Root(key));
        } else {
            return None;
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
    if let Some(r) = expr.strip_prefix(&format!("{mvar}[")) {
        if let Some((n, rest)) = r.split_once("].present? ? ") {
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
    if stmts.len() == 3 {
        if let Some((mvar, key)) = parse_match(first, root) {
            if stmts[1] != format!("next if {mvar}.blank?") || !has_regex {
                return None;
            }
            let render = yaml_render(&stmts[2], &mvar, root)?;
            return Some(YamlBlock {
                has_regex,
                shape: YamlShape::RootMatch { key, render },
            });
        }
    }
    None
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
    if let Some(rest) = tail.strip_prefix("&.include?(") {
        let (lit, rest) = quoted_token(rest)?;
        let rest = rest.strip_prefix(')')?;
        return Some((JsonCond::Includes(field, lit), rest));
    }
    if let Some(rest) = tail.strip_prefix("&.downcase&.include?(") {
        let (lit, rest) = quoted_token(rest)?;
        let rest = rest.strip_prefix(')')?;
        return Some((JsonCond::DowncaseIncludes(field, lit), rest));
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
                false
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
