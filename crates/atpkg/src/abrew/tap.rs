// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Taps (`Library/Taps`): git clones with formula `.rb` files parsed as data.

use std::path::PathBuf;

use super::fetch;
use super::formula::Formula;
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

/// Clone or verify an installed tap, returning `(tap, newly_tapped)`.
pub fn add(prefix: &Prefix, name: &str, url: Option<&str>) -> Result<(Tap, bool), String> {
    let (user, repo) = parse_tap_name(name)
        .ok_or_else(|| format!("abrew: tap name must be user/repo, got {name}"))?;
    let (path, update) = mutation_path(prefix, &user, &repo, true)?;
    if update {
        if let Some(url) = url {
            let origin = fetch::run_capture(
                aterm_uds::os_path!("git"),
                &["-C", &path.to_string_lossy(), "remote", "get-url", "origin"],
            )
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string());
            match origin.as_deref() {
                Some(existing) if existing == url => {}
                Some(existing) => {
                    return Err(format!(
                        "abrew: tap {user}/{repo} is already tapped from {existing}"
                    ));
                }
                None => {
                    return Err(format!(
                        "abrew: cannot read the existing remote of tap {user}/{repo}"
                    ));
                }
            }
        }
        return Ok((Tap { user, repo, path }, false));
    } else {
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

pub fn remove(prefix: &Prefix, name: &str) -> Result<(), String> {
    let (user, repo) = parse_tap_name(name)
        .ok_or_else(|| format!("abrew: tap name must be user/repo, got {name}"))?;
    if tap_path(prefix, &user, &repo).is_ok_and(|p| std::fs::symlink_metadata(p).is_err()) {
        return Err(format!("abrew: tap {user}/{repo} is not tapped"));
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
    std::fs::remove_dir_all(&path).map_err(|e| format!("abrew: {e}"))
}

/// Every tap on disk.
#[must_use]
pub fn list(prefix: &Prefix) -> Vec<Tap> {
    let mut out = Vec::new();
    let Ok(users) = std::fs::read_dir(prefix.taps_dir()) else {
        return out;
    };
    for u in users.flatten() {
        let Ok(repos) = std::fs::read_dir(u.path()) else {
            continue;
        };
        for r in repos.flatten() {
            let fname = r.file_name().to_string_lossy().into_owned();
            let user = u.file_name().to_string_lossy().into_owned();
            if let Some(repo) = fname.strip_prefix("homebrew-")
                && let Ok(path) = tap_path(prefix, &user, repo)
                && u.file_type().is_ok_and(|t| t.is_dir())
                && r.file_type().is_ok_and(|t| t.is_dir())
                && std::fs::symlink_metadata(r.path().join(".git")).is_ok_and(|m| m.is_dir())
            {
                out.push(Tap {
                    user,
                    repo: repo.to_owned(),
                    path,
                });
            }
        }
    }
    out.sort_by_key(|a| a.name());
    out
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
        let file = formula_file(&tap, name)?;
        let text = std::fs::read_to_string(file).ok()?;
        return Some(parse_rb_as_data(&text, full));
    }
    for tap in list(prefix) {
        if let Some(file) = formula_file(&tap, full)
            && let Ok(text) = std::fs::read_to_string(file)
        {
            return Some(parse_rb_as_data(&text, &format!("{}/{full}", tap.name())));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
