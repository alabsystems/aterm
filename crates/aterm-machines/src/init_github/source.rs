// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private local identity of repository inputs. It observes only the parsed GitHub profile,
//! its selected credential and default inventory, so unrelated init settings do not stop work.
//! Readiness observation also tracks the actual admitted Git command and payload.

use super::{Asked, Github, TOKEN_FILE, asked, inventory, profile, read_token};
use crate::model::User;
use crate::port::System;
use std::path::{Path, PathBuf};

type FileVersion = Result<[u8; 32], std::io::ErrorKind>;

/// Local source, pinned-helper and Git changes that can invalidate a completed repository stage.
/// Observing this mark grants no authority over inputs or effects and runs no program.
#[derive(Clone, PartialEq, Eq)]
pub struct RepositoryInputMark {
    source: Result<Stamp, String>,
    helper: Result<(PathBuf, [u8; 32]), std::io::ErrorKind>,
    git: Vec<(PathBuf, FileVersion)>,
    paths: Vec<PathBuf>,
}

impl std::fmt::Debug for RepositoryInputMark {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RepositoryInputMark")
            .finish_non_exhaustive()
    }
}

fn account(sys: &dyn System) -> Result<User, String> {
    let user = sys.identity()?.user;
    if user.uid != sys.euid() || user.uid == 0 {
        return Err("repository inputs belong to this login alone".into());
    }
    Ok(user)
}

/// Observe this login's selected repository inputs, fixed credential helper and admitted Git
/// command/payload. Missing or refused inputs remain stable observations, so removal and
/// later repair each move it.
///
/// # Errors
/// This process's own account cannot be identified.
pub fn repository_input_mark(sys: &dyn System) -> Result<RepositoryInputMark, String> {
    let user = account(sys)?;
    let helper = (|| {
        let path = sys.canonicalize(&crate::git_credential::Helper::launcher(&user))?;
        let version = sys.file_version(&path)?;
        Ok::<_, std::io::Error>((path, version))
    })()
    .map_err(|why| why.kind());
    Ok(RepositoryInputMark {
        source: Stamp::read(sys, &user),
        helper,
        git: git_paths(sys)
            .into_iter()
            .map(|path| {
                let version = sys.file_version(&path).map_err(|why| why.kind());
                (path, version)
            })
            .collect(),
        paths: watch_paths(sys, &user),
    })
}

/// Files whose parent directories can notify a parked repository stage. Its own selected
/// token is included; global credential helpers are excluded because every setup Git command
/// pins aterm's helper instead. The admitted Git command and payload are included, so a
/// same-build shim repair is observable. A malformed profile still watches its repair.
///
/// # Errors
/// This process's own account cannot be identified.
pub fn repository_input_watch_paths(sys: &dyn System) -> Result<Vec<PathBuf>, String> {
    let user = account(sys)?;
    Ok(watch_paths(sys, &user))
}

fn watch_paths(sys: &dyn System, user: &User) -> Vec<PathBuf> {
    let home = Path::new(&user.home);
    let launcher = crate::git_credential::Helper::launcher(user);
    let mut paths = vec![home.join(".config/aterm/init.toml"), launcher.clone()];
    if let Ok(resolved) = sys.canonicalize(&launcher) {
        paths.push(resolved);
    }
    // A launcher can precede its installed target. Keep each declared target observable even
    // when canonicalization cannot reach it yet, including a dangling link in a parent. This
    // reads link metadata only and grants no permission to execute or read the target.
    let mut link = launcher;
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..64 {
        if !seen.insert(link.clone()) {
            break;
        }
        paths.push(link.clone());
        let next = link.ancestors().find_map(|ancestor| {
            let target = sys.read_link(ancestor).ok()?;
            let target = if target.is_absolute() {
                target
            } else {
                ancestor.parent()?.join(target)
            };
            let tail = link.strip_prefix(ancestor).ok()?;
            Some(if tail.as_os_str().is_empty() {
                target
            } else {
                target.join(tail)
            })
        });
        let Some(next) = next else { break };
        link = next;
    }
    match profile(sys, user).and_then(|profile| asked(profile.and_then(|p| p.github), user)) {
        Ok(Asked::Default) => {
            paths.push(home.join(TOKEN_FILE));
            paths.push(home.join(inventory::FILE));
        }
        Ok(Asked::Scoped(source) | Asked::Profile(source)) => paths.push(source.token_file),
        Ok(Asked::Off) | Err(_) => {}
    }
    paths.extend(git_paths(sys));
    paths.sort();
    paths.dedup();
    paths
}

fn git_paths(sys: &dyn System) -> Vec<PathBuf> {
    let Some(git) = crate::items::github::installed_git_execution(sys) else {
        return Vec::new();
    };
    let mut paths = vec![git.program, git.payload];
    paths.sort();
    paths.dedup();
    paths
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_dangling_helper_keeps_its_declared_target_observable() {
        let root = aterm_tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let home = root_path.join("home");
        let alias = root_path.join("alias/aterm");
        let user = User {
            name: "fixture".into(),
            uid: aterm_uds::peer::our_uid(),
            gid: 0,
            home: home.to_string_lossy().into_owned(),
            shell: String::new(),
            admin: false,
        };
        let launcher = crate::git_credential::Helper::launcher(&user);
        std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
        std::fs::create_dir_all(alias.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&alias, &launcher).unwrap();
        let sys = crate::port::Live::new();
        for package in ["first", "second"] {
            if package == "second" {
                std::fs::remove_file(&alias).unwrap();
            }
            let relative = PathBuf::from(format!("../{package}/bin/aterm"));
            std::os::unix::fs::symlink(&relative, &alias).unwrap();
            assert!(sys.canonicalize(&launcher).is_err(), "the target is absent");
            let paths = watch_paths(&sys, &user);
            assert!(paths.contains(&launcher));
            assert!(paths.contains(&alias));
            assert!(
                paths.contains(&alias.parent().unwrap().join(relative)),
                "arrival away from the unchanged launcher must stay observable: {paths:?}"
            );
        }
        // A missing directory behind a parent link has the same arrival boundary.
        std::fs::remove_file(&alias).unwrap();
        std::fs::remove_file(&launcher).unwrap();
        std::fs::remove_dir(launcher.parent().unwrap()).unwrap();
        let package = root_path.join("third/bin");
        std::os::unix::fs::symlink(&package, launcher.parent().unwrap()).unwrap();
        assert!(sys.canonicalize(&launcher).is_err());
        assert!(watch_paths(&sys, &user).contains(&package.join("aterm")));
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Stamp([u8; 32]);

impl Stamp {
    pub(super) fn matches_config(
        sys: &dyn System,
        user: &User,
        expected: &Github,
    ) -> Result<bool, String> {
        Ok(
            match asked(profile(sys, user)?.and_then(|profile| profile.github), user)? {
                Asked::Scoped(current) | Asked::Profile(current) => current == *expected,
                Asked::Default | Asked::Off => false,
            },
        )
    }

    pub(super) fn read(sys: &dyn System, user: &User) -> Result<Self, String> {
        let asked = asked(profile(sys, user)?.and_then(|profile| profile.github), user)?;
        let (kind, source, default) = match asked {
            Asked::Off => return Err("repository setup is disabled in init.toml".into()),
            Asked::Default => (
                b"default".as_slice(),
                Github {
                    account: String::new(),
                    owner: String::new(),
                    token_file: Path::new(&user.home).join(TOKEN_FILE),
                    token_key: None,
                },
                true,
            ),
            Asked::Scoped(source) => (b"scoped".as_slice(), source, false),
            Asked::Profile(source) => (b"profile".as_slice(), source, false),
        };
        let token = read_token(sys, user, &source).map_err(|why| why.why)?;
        let inventory = if default {
            sys.read_owned_file(
                &Path::new(&user.home).join(inventory::FILE),
                user.uid,
                inventory::MAX_BYTES,
                true,
            )
            .map_err(|_| "the private repository inventory is not ready".to_owned())?
        } else {
            Vec::new()
        };
        let mut digest = aterm_digest::Sha256::new();
        digest.update(b"aterm repository inputs/1\0");
        let account = source.account.to_ascii_lowercase();
        let owner = source.owner.to_ascii_lowercase();
        for bytes in [
            kind,
            account.as_bytes(),
            owner.as_bytes(),
            source.token_file.as_os_str().as_encoded_bytes(),
            source.token_key.as_deref().unwrap_or("").as_bytes(),
            token.bytes(),
            inventory.as_slice(),
        ] {
            digest.update((bytes.len() as u64).to_le_bytes());
            digest.update(bytes);
        }
        Ok(Self(digest.finalize()))
    }

    pub(super) fn is_current(&self, sys: &dyn System, user: &User) -> Result<bool, String> {
        Self::read(sys, user).map(|now| now == *self)
    }
}
