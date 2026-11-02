// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Admitted Git paths and pinned nested-lock replacement for the release bump.

use std::collections::BTreeSet;
use std::path::{Component, Path};

const PATH_BYTES: usize = 1024 * 1024;
const PATHS: usize = 256;

/// Decode complete `git -z` records without trimming or Git's quoted-path spelling.
/// # Errors
/// Oversized, incomplete, non-UTF-8, duplicate or non-relative path records.
pub(super) fn paths(bytes: &[u8]) -> Result<Vec<String>, String> {
    if bytes.len() > PATH_BYTES {
        return Err("Git paths exceed the 1 MiB admission limit".into());
    }
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let Some(records) = bytes.strip_suffix(&[0]) else {
        return Err("Git paths did not end in a NUL record terminator".into());
    };
    let mut paths = Vec::new();
    for record in records.split(|byte| *byte == 0) {
        if paths.len() == PATHS {
            return Err("Git paths exceed the 256-record admission limit".into());
        }
        paths.push(
            std::str::from_utf8(record)
                .map(str::to_owned)
                .map_err(|_| "Git paths are not UTF-8".to_owned())?,
        );
    }
    admit(&paths)?;
    Ok(paths)
}

fn relative(path: &str) -> Result<(), String> {
    if path.as_bytes().contains(&0)
        || path
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        return Err("Git named an empty, escaping or ambiguous path component".into());
    }
    let path = Path::new(path);
    if path.as_os_str().is_empty() || !path.is_relative() {
        return Err("Git named an empty or absolute path".into());
    }
    for component in path.components() {
        let Component::Normal(_) = component else {
            return Err("Git named a path with a non-normal component".into());
        };
    }
    Ok(())
}

fn admit(paths: &[String]) -> Result<(), String> {
    if paths.len() > PATHS {
        return Err("Git paths exceed the 256-record admission limit".into());
    }
    let mut bytes = 0_usize;
    let mut seen = BTreeSet::new();
    for path in paths {
        bytes = bytes
            .checked_add(path.len().saturating_add(1))
            .filter(|bytes| *bytes <= PATH_BYTES)
            .ok_or("Git paths exceed the 1 MiB admission limit")?;
        relative(path)?;
        if !seen.insert(path) {
            return Err("Git repeated a path".into());
        }
    }
    Ok(())
}

#[cfg(not(unix))]
/// Refuse platforms without this release writer's pinned Unix operations.
/// # Errors
/// Always returns an explicit unsupported-operation refusal.
pub(super) fn bump(
    _: &Path,
    _: Result<Vec<String>, String>,
    _: &str,
    _: &str,
) -> Result<Vec<String>, String> {
    Err("nested lock replacement is unsupported without Unix pinned-directory operations".into())
}

#[cfg(unix)]
/// Stage admitted nested locks, then replace their pinned directory entries.
/// # Errors
/// Refused enumeration, paths, aliases, reads, identities, bounds, staging or publication.
pub(super) fn bump(
    repo: &Path,
    listing: Result<Vec<String>, String>,
    from: &str,
    to: &str,
) -> Result<Vec<String>, String> {
    unix::bump(repo, listing, from, to)
}

#[cfg(unix)]
mod unix {
    use super::*;
    use aterm_dirfd::{AtFlags, FileType, Mode, OFlags};
    use std::ffi::{OsStr, OsString};
    use std::fs::{File, Metadata};
    use std::io::{Read as _, Write as _};
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    const LOCK_BYTES: u64 = 32 * 1024 * 1024;
    const BATCH_BYTES: u64 = 64 * 1024 * 1024;
    fn directory_flags() -> OFlags {
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    struct Identity(u64, u64);

    fn identity(file: &File) -> Result<Identity, String> {
        let meta = file.metadata().map_err(|e| e.to_string())?;
        Ok(Identity(meta.dev(), meta.ino()))
    }

    fn named(dir: &File, name: &OsStr) -> Result<aterm_dirfd::Stat, String> {
        aterm_dirfd::fstatat(dir, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|e| e.to_string())
    }

    fn named_identity(dir: &File, name: &OsStr) -> Result<Identity, String> {
        let stat = named(dir, name)?;
        Ok(Identity(stat.st_dev, stat.st_ino))
    }

    fn directory(parent: &File, name: &OsStr) -> Result<File, String> {
        aterm_dirfd::openat(parent, name, directory_flags(), Mode::empty())
            .map(File::from)
            .map_err(|e| e.to_string())
    }

    fn root(path: &Path) -> Result<File, String> {
        let absolute = if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir()
                .map_err(|e| e.to_string())?
                .join(path)
        };
        let mut directory = File::from(
            aterm_dirfd::openat(aterm_dirfd::CWD, "/", directory_flags(), Mode::empty())
                .map_err(|e| e.to_string())?,
        );
        for part in absolute.components() {
            match part {
                Component::RootDir => {}
                Component::Normal(name) => directory = self::directory(&directory, name)?,
                _ => return Err("repository root has a non-normal component".into()),
            }
        }
        Ok(directory)
    }

    fn parent(root: &File, path: &str) -> Result<(File, OsString), String> {
        let mut parts = Path::new(path).components().peekable();
        let mut directory = root.try_clone().map_err(|e| e.to_string())?;
        while let Some(part) = parts.next() {
            let Component::Normal(name) = part else {
                return Err("nested lock has a non-normal component".into());
            };
            if parts.peek().is_none() {
                return Ok((directory, name.to_owned()));
            }
            directory = self::directory(&directory, name)?;
        }
        Err("nested lock has no filename".into())
    }

    #[derive(PartialEq, Eq)]
    struct Snapshot {
        id: Identity,
        mode: u32,
        links: u64,
        len: u64,
        modified: (i64, i64),
        changed: (i64, i64),
    }

    impl Snapshot {
        fn of(meta: &Metadata) -> Result<Self, String> {
            if !meta.is_file() || meta.nlink() != 1 {
                return Err("nested lock is not a regular single-link file".into());
            }
            Ok(Self {
                id: Identity(meta.dev(), meta.ino()),
                mode: meta.mode(),
                links: meta.nlink(),
                len: meta.len(),
                modified: (meta.mtime(), meta.mtime_nsec()),
                changed: (meta.ctime(), meta.ctime_nsec()),
            })
        }
    }

    struct Stage {
        path: String,
        parent: File,
        target: OsString,
        original: File,
        before: Snapshot,
        name: OsString,
        directory: File,
        directory_id: Identity,
        data: Option<File>,
        data_id: Option<Identity>,
    }

    impl Stage {
        fn new(
            path: String,
            parent: File,
            target: OsString,
            original: File,
            before: Snapshot,
            text: &str,
        ) -> Result<Self, String> {
            use ring::rand::SecureRandom as _;

            let mut created = None;
            for _ in 0..8 {
                let mut nonce = [0_u8; 16];
                ring::rand::SystemRandom::new()
                    .fill(&mut nonce)
                    .map_err(|_| "nested lock staging could not obtain randomness")?;
                let name = OsString::from(format!(
                    ".aterm-lock-bump-{:032x}.noindex",
                    u128::from_le_bytes(nonce)
                ));
                match aterm_dirfd::mkdirat(&parent, &name, Mode::RWXU) {
                    Ok(()) => {
                        let observed = named_identity(&parent, &name)?;
                        let directory = self::directory(&parent, &name)?;
                        if identity(&directory)? != observed {
                            return Err("nested lock staging directory changed identity".into());
                        }
                        created = Some((name, directory, observed));
                        break;
                    }
                    Err(error)
                        if std::io::Error::from_raw_os_error(error.raw_os_error()).kind()
                            == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.to_string()),
                }
            }
            let (name, directory, directory_id) =
                created.ok_or("nested lock staging names are occupied")?;
            let mut stage = Self {
                path,
                parent,
                target,
                original,
                before,
                name,
                directory,
                directory_id,
                data: None,
                data_id: None,
            };
            stage.data = Some(File::from(
                aterm_dirfd::openat(
                    &stage.directory,
                    "data",
                    OFlags::WRONLY
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC,
                    Mode::RUSR | Mode::WUSR,
                )
                .map_err(|e| e.to_string())?,
            ));
            let data = stage.data.as_mut().ok_or("nested lock stage has no data")?;
            stage.data_id = Some(identity(data)?);
            data.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
            data.set_permissions(std::fs::Permissions::from_mode(stage.before.mode & 0o777))
                .map_err(|e| e.to_string())?;
            Ok(stage)
        }

        fn check(&self, root: &File) -> Result<(), String> {
            let (current, _) = parent(root, &self.path)?;
            if identity(&current)? != identity(&self.parent)? {
                return Err("nested lock parent changed identity".into());
            }
            let now = Snapshot::of(&self.original.metadata().map_err(|e| e.to_string())?)?;
            let entry = named(&self.parent, &self.target)?;
            if now != self.before
                || !FileType::from_raw_mode(entry.st_mode).is_file()
                || entry.st_nlink != 1
                || Identity(entry.st_dev, entry.st_ino) != self.before.id
            {
                return Err("nested lock changed after it was read".into());
            }
            let data = self.data.as_ref().ok_or("nested lock stage has no data")?;
            if named_identity(&self.directory, OsStr::new("data"))? != identity(data)?
                || data.metadata().map_err(|e| e.to_string())?.nlink() != 1
            {
                return Err("nested lock staged data changed identity".into());
            }
            Ok(())
        }

        fn publish(&self) -> Result<(), String> {
            aterm_dirfd::renameat(&self.directory, "data", &self.parent, &self.target)
                .map_err(|e| e.to_string())
        }
    }

    impl Drop for Stage {
        fn drop(&mut self) {
            if self.data_id.is_some_and(|id| {
                named_identity(&self.directory, OsStr::new("data")).ok() == Some(id)
            }) {
                let _ = aterm_dirfd::unlinkat(&self.directory, "data", AtFlags::empty());
            }
            if named_identity(&self.parent, &self.name).ok() == Some(self.directory_id) {
                let _ = aterm_dirfd::unlinkat(&self.parent, &self.name, AtFlags::REMOVEDIR);
            }
        }
    }

    /// Prepare and publish with the ordinary no-op observation boundary.
    /// # Errors
    /// Any admission, read, staging, identity or replacement refusal.
    pub(super) fn bump(
        repo: &Path,
        listing: Result<Vec<String>, String>,
        from: &str,
        to: &str,
    ) -> Result<Vec<String>, String> {
        bump_on(repo, listing, from, to, || {})
    }

    /// Observe completed staging before every target's final identity checks.
    /// # Errors
    /// The same filesystem and input refusals as [`bump`].
    /// # Panics
    /// The supplied observer panics.
    pub(super) fn bump_on(
        repo: &Path,
        listing: Result<Vec<String>, String>,
        from: &str,
        to: &str,
        before_publish: impl FnOnce(),
    ) -> Result<Vec<String>, String> {
        let paths = listing?;
        admit(&paths)?;
        for path in &paths {
            let path = Path::new(path);
            if path.file_name() != Some(OsStr::new("Cargo.lock")) || path.components().count() < 2 {
                return Err("Git did not name a nested Cargo.lock".into());
            }
        }
        let root = root(repo).map_err(|why| format!("pin repository {repo:?}: {why}"))?;
        let mut stages = Vec::new();
        let mut remaining = BATCH_BYTES;
        let mut writable = BATCH_BYTES;
        for path in paths {
            let (parent, target) =
                parent(&root, &path).map_err(|why| format!("admit {path:?}: {why}"))?;
            let file = File::from(
                aterm_dirfd::openat(
                    &parent,
                    &target,
                    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(|e| format!("read {path}: {e}"))?,
            );
            let before = Snapshot::of(&file.metadata().map_err(|e| e.to_string())?)
                .map_err(|why| format!("admit {path:?}: {why}"))?;
            if before.len > LOCK_BYTES || before.len > remaining {
                return Err(
                    "nested locks exceed the 32 MiB/file or 64 MiB/batch read limit".into(),
                );
            }
            let mut bytes = Vec::new();
            (&file)
                .take(LOCK_BYTES.min(remaining) + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| format!("read {path}: {e}"))?;
            let now = Snapshot::of(&file.metadata().map_err(|e| e.to_string())?)?;
            if now != before || u64::try_from(bytes.len()).unwrap_or(u64::MAX) != before.len {
                return Err(format!("{path} changed while it was read"));
            }
            remaining -= before.len;
            let text = std::str::from_utf8(&bytes).map_err(|_| format!("{path} is not UTF-8"))?;
            if let Some(moved) = super::super::lock_follows_bump(text, from, to) {
                let size =
                    u64::try_from(moved.len()).map_err(|_| "nested lock output is too large")?;
                if size > LOCK_BYTES || size > writable {
                    return Err(
                        "nested locks exceed the 32 MiB/file or 64 MiB/batch stage limit".into(),
                    );
                }
                writable -= size;
                stages.push(
                    Stage::new(path.clone(), parent, target, file, before, &moved)
                        .map_err(|why| format!("stage {path:?}: {why}"))?,
                );
            }
        }
        before_publish();
        // All reads and staged writes precede every publication. These checks observe identity;
        // they do not make the subsequent renames an atomic compare-and-swap or batch transaction.
        for stage in &stages {
            stage
                .check(&root)
                .map_err(|why| format!("recheck {:?}: {why}", stage.path))?;
        }
        let mut followed = Vec::new();
        for stage in &stages {
            stage
                .check(&root)
                .map_err(|why| format!("recheck {:?}: {why}", stage.path))?;
            stage
                .publish()
                .map_err(|why| format!("replace {:?}: {why}", stage.path))?;
            followed.push(stage.path.clone());
        }
        Ok(followed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_records_are_complete_relative_and_bounded() {
        assert_eq!(paths(b"").unwrap(), Vec::<String>::new());
        assert_eq!(
            paths(b"nested/Cargo.lock\0").unwrap(),
            ["nested/Cargo.lock"]
        );
        for refused in [
            &b"nested/Cargo.lock"[..],
            &b"\0"[..],
            &b"a\0\0"[..],
            &b"a\0a\0"[..],
            &b"../outside/Cargo.lock\0"[..],
            &b"/outside/Cargo.lock\0"[..],
            &b"nested/../Cargo.lock\0"[..],
            &b"nested//Cargo.lock\0"[..],
            &b"nested/\xff/Cargo.lock\0"[..],
        ] {
            assert!(paths(refused).is_err(), "{refused:?}");
        }
        assert!(paths(&vec![b'x'; PATH_BYTES + 1]).is_err());
        let too_many = (0..=PATHS)
            .flat_map(|index| format!("nested{index}/Cargo.lock\0").into_bytes())
            .collect::<Vec<_>>();
        assert!(paths(&too_many).is_err());
    }

    #[cfg(unix)]
    mod filesystem {
        use super::*;
        use std::time::{Duration, Instant};

        const BEFORE: &str = "[[package]]\nname = \"aterm-local\"\nversion = \"0.147.0\"\n";
        const AFTER: &str = "[[package]]\nname = \"aterm-local\"\nversion = \"0.148.0\"\n";

        fn git(repo: &Path, args: &[&str]) -> Vec<u8> {
            let program = std::path::PathBuf::from(aterm_uds::fixture_git::git().get_program());
            let mut command = aterm_uds::spawnfd::command(aterm_uds::deadline::stand_in(&program));
            // Preserve the requested command's identity when the kept stand-in has a salted name.
            command.arg0("git");
            for (key, _) in std::env::vars_os() {
                if key.to_string_lossy().starts_with("GIT_") {
                    command.env_remove(key);
                }
            }
            crate::credential_wait::ask_no_one(&mut command)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_AUTHOR_NAME", "Fixture")
                .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
                .env("GIT_COMMITTER_NAME", "Fixture")
                .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
                .arg("-C")
                .arg(repo)
                .args(["-c", "commit.gpgsign=false"])
                .args(args);
            let out = aterm_uds::deadline::output_until_complete_stdout(
                &mut command,
                Instant::now() + Duration::from_secs(60),
            )
            .expect("private local Git fixture must finish");
            assert!(out.status.success(), "{args:?}: {:?}", out.stderr);
            out.stdout
        }

        #[test]
        fn actual_git_paths_with_quotes_tabs_and_newlines_survive_the_bump_comparison() {
            let scratch = aterm_tempfile::tempdir().unwrap();
            let repo = scratch.path().canonicalize().unwrap();
            let nested = "odd space\t\"quoted\"\nline/Cargo.lock";
            let path = repo.join(nested);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, BEFORE).unwrap();
            git(&repo, &["init", "--quiet"]);
            git(&repo, &["add", "--", nested]);
            git(&repo, &["commit", "--quiet", "-m", "inert fixture"]);
            let listed = paths(&git(&repo, &["ls-files", "-z", "--", "*/Cargo.lock"])).unwrap();
            assert_eq!(listed, [nested]);
            let followed = bump(&repo, Ok(listed), "0.147.0", "0.148.0").unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), AFTER);
            let changed = paths(&git(&repo, &["diff", "--name-only", "-z", "HEAD"])).unwrap();
            assert_eq!(
                changed, followed,
                "the producer and comparison use the same names"
            );
            let unexpected = "unexpected\nfile.txt";
            std::fs::write(repo.join(unexpected), "keep this stray\n").unwrap();
            let untracked = paths(&git(
                &repo,
                &["ls-files", "--others", "--exclude-standard", "-z"],
            ))
            .unwrap();
            assert!(
                untracked
                    .iter()
                    .any(|path| path == unexpected && !followed.contains(path))
            );
        }

        #[test]
        fn linked_parents_and_path_escapes_never_reach_outside_payloads() {
            let scratch = aterm_tempfile::tempdir().unwrap();
            let home = scratch.path().canonicalize().unwrap();
            let repo = home.join("repo");
            let outside = home.join("outside");
            std::fs::create_dir(&repo).unwrap();
            std::fs::create_dir(&outside).unwrap();
            std::fs::write(outside.join("Cargo.lock"), BEFORE).unwrap();
            std::os::unix::fs::symlink(&outside, repo.join("linked")).unwrap();
            for path in [
                "linked/Cargo.lock".to_owned(),
                "../outside/Cargo.lock".to_owned(),
                outside.join("Cargo.lock").display().to_string(),
            ] {
                assert!(bump(&repo, Ok(vec![path]), "0.147.0", "0.148.0").is_err());
                assert_eq!(
                    std::fs::read_to_string(outside.join("Cargo.lock")).unwrap(),
                    BEFORE
                );
            }
            assert_eq!(std::fs::read_link(repo.join("linked")).unwrap(), outside);
        }

        #[test]
        fn oversized_locks_hold_the_batch_without_replacing_earlier_files() {
            let scratch = aterm_tempfile::tempdir().unwrap();
            let repo = scratch.path().canonicalize().unwrap();
            for name in ["good", "huge"] {
                std::fs::create_dir(repo.join(name)).unwrap();
            }
            let good = repo.join("good/Cargo.lock");
            std::fs::write(&good, BEFORE).unwrap();
            let huge = std::fs::File::create(repo.join("huge/Cargo.lock")).unwrap();
            huge.set_len(32 * 1024 * 1024 + 1).unwrap();
            assert!(
                bump(
                    &repo,
                    Ok(vec!["good/Cargo.lock".into(), "huge/Cargo.lock".into()]),
                    "0.147.0",
                    "0.148.0"
                )
                .is_err()
            );
            assert_eq!(std::fs::read_to_string(good).unwrap(), BEFORE);
            assert_eq!(huge.metadata().unwrap().len(), 32 * 1024 * 1024 + 1);
            assert!(
                std::fs::read_dir(repo.join("good"))
                    .unwrap()
                    .all(|entry| { entry.unwrap().file_name() == "Cargo.lock" })
            );
        }

        #[test]
        fn an_observed_replacement_refuses_before_any_target_is_published() {
            let scratch = aterm_tempfile::tempdir().unwrap();
            let repo = scratch.path().canonicalize().unwrap();
            for name in ["first", "later"] {
                std::fs::create_dir(repo.join(name)).unwrap();
                std::fs::write(repo.join(name).join("Cargo.lock"), BEFORE).unwrap();
            }
            let result = super::super::unix::bump_on(
                &repo,
                Ok(vec!["first/Cargo.lock".into(), "later/Cargo.lock".into()]),
                "0.147.0",
                "0.148.0",
                || {
                    std::fs::rename(repo.join("first/Cargo.lock"), repo.join("saved-original"))
                        .unwrap();
                    std::fs::write(repo.join("first/Cargo.lock"), "concurrent replacement\n")
                        .unwrap();
                },
            );
            assert!(result.is_err());
            assert_eq!(
                std::fs::read_to_string(repo.join("first/Cargo.lock")).unwrap(),
                "concurrent replacement\n"
            );
            assert_eq!(
                std::fs::read_to_string(repo.join("saved-original")).unwrap(),
                BEFORE
            );
            assert_eq!(
                std::fs::read_to_string(repo.join("later/Cargo.lock")).unwrap(),
                BEFORE
            );
            for name in ["first", "later"] {
                assert!(
                    std::fs::read_dir(repo.join(name))
                        .unwrap()
                        .all(|entry| { entry.unwrap().file_name() == "Cargo.lock" })
                );
            }
        }

        #[cfg(any(target_os = "macos", target_os = "linux"))]
        #[test]
        fn a_writerless_fifo_is_refused_without_waiting_for_a_writer() {
            use std::os::unix::ffi::OsStrExt as _;
            #[cfg(target_os = "macos")]
            type PipeMode = u16;
            #[cfg(target_os = "linux")]
            type PipeMode = u32;
            unsafe extern "C" {
                fn mkfifo(path: *const std::ffi::c_char, mode: PipeMode) -> i32;
            }
            let scratch = aterm_tempfile::tempdir().unwrap();
            let repo = scratch.path().canonicalize().unwrap();
            std::fs::create_dir(repo.join("pipe")).unwrap();
            let path = std::ffi::CString::new(repo.join("pipe/Cargo.lock").as_os_str().as_bytes())
                .unwrap();
            // SAFETY: the owned NUL-terminated path outlives this call; mode_t is u16 on macOS
            // and u32 on Linux, the only platforms compiling this fixture.
            assert_eq!(unsafe { mkfifo(path.as_ptr(), 0o600) }, 0);
            let (send, receive) = std::sync::mpsc::sync_channel(1);
            let worker = std::thread::spawn(move || {
                let _ = send.send(bump(
                    &repo,
                    Ok(vec!["pipe/Cargo.lock".into()]),
                    "0.147.0",
                    "0.148.0",
                ));
            });
            let refused = receive
                .recv_timeout(Duration::from_secs(60))
                .expect("the owned FIFO refusal must finish; this is a hang detector");
            worker.join().unwrap();
            assert!(refused.is_err());
        }
    }
}
