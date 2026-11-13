// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! How retirement's git reads are kept from fetching an object a partial clone lacks: agh's one
//! answer ([`NoLazyFetch::of`]), asked once per git program in this process, of that program run
//! hermetically ([`Hermetic`]).

use agh::git::lazy_fetch::{self as shared, NoLazyFetch};
use aterm_types::MutexExt as _;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

/// How long one of the question's runs may take, each a local git in a private scratch folder: a
/// hang detector, past which the question is unanswered rather than answered either way.
const RUN_BOUND: Duration = Duration::from_secs(60);

/// How `program`'s reads are kept from fetching lazily ([`NoLazyFetch::of`]), asked of it run
/// [`Hermetic`]ally the first time this process needs it and remembered for the process, so a
/// retirement pass asks once, not once per git run. The answer is remembered for the file the
/// path names ([`Identity`]), so a git replaced at that path is asked again. An unanswered
/// question is not remembered.
///
/// # Errors
/// `program` neither takes `--no-lazy-fetch` nor was seen to obey `GIT_NO_LAZY_FETCH`, so no
/// read of it can be kept from fetching; the file it names cannot be read; or the question could
/// not be asked ([`NoLazyFetch::of`]).
pub(super) fn guard(program: &Path) -> Result<NoLazyFetch, String> {
    static ASKED: Mutex<BTreeMap<Identity, Option<NoLazyFetch>>> = Mutex::new(BTreeMap::new());
    let identity = Identity::of(program)?;
    let mut asked = ASKED.lock_or_recover();
    let answer = match asked.get(&identity) {
        Some(answer) => *answer,
        None => {
            let answer = NoLazyFetch::of(&Hermetic(program))
                .map_err(|why| format!("{}: {why}", program.display()))?;
            asked.insert(identity, answer);
            answer
        }
    };
    answer.ok_or_else(|| {
        format!(
            "{} neither takes --no-lazy-fetch nor obeys GIT_NO_LAZY_FETCH, so its reads cannot \
             be kept from fetching",
            program.display()
        )
    })
}

/// The file a git program's path names now: the path resolved, and the file's device and inode
/// (on Unix), length and modification time, so another file put at the same path is another
/// identity.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Identity {
    path: PathBuf,
    file: (u64, u64),
    len: u64,
    modified: Option<SystemTime>,
}

impl Identity {
    /// The identity of the file `program` names.
    ///
    /// # Errors
    /// The path cannot be resolved or its file read.
    fn of(program: &Path) -> Result<Self, String> {
        let unread = |error: std::io::Error| format!("{}: {error}", program.display());
        let path = std::fs::canonicalize(program).map_err(unread)?;
        let meta = std::fs::metadata(&path).map_err(unread)?;
        #[cfg(unix)]
        let file = {
            use std::os::unix::fs::MetadataExt as _;
            (meta.dev(), meta.ino())
        };
        #[cfg(not(unix))]
        let file = (0, 0);
        Ok(Self {
            path,
            file,
            len: meta.len(),
            modified: meta.modified().ok(),
        })
    }
}

/// `cmd` without any `GIT_*` variable this process holds, in any case (Windows folds names), so
/// no inherited repository, object directory, config or trace setting reaches its git.
pub(super) fn without_git_environment(cmd: &mut aterm_uds::spawnfd::Command) {
    for (name, _) in std::env::vars_os() {
        if name
            .as_encoded_bytes()
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"GIT_"))
        {
            cmd.env_remove(name);
        }
    }
}

/// The git program at this path as the question asks it: with no `GIT_*` variable this process
/// holds ([`without_git_environment`]), no prompt and no optional lock, its input empty, its
/// output complete and bounded, and its scratch a private folder of this process's.
pub(super) struct Hermetic<'a>(pub(super) &'a Path);

impl shared::Git for Hermetic<'_> {
    fn run(&self, args: &[&OsStr], env: &[(&str, &str)]) -> Result<shared::Ran, String> {
        let mut cmd = aterm_uds::spawnfd::command(self.0);
        without_git_environment(&mut cmd);
        cmd.args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0");
        for (key, value) in env {
            cmd.env(key, value);
        }
        let output = aterm_uds::deadline::output_with_input_until(
            &mut cmd,
            aterm_uds::secret::Secret::new(b""),
            Instant::now() + RUN_BOUND,
            aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES,
        )
        .map_err(|error| format!("git did not run to completion: {error}"))?;
        Ok(shared::Ran {
            status: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    fn scratch(&self) -> Result<Box<dyn AsRef<Path> + '_>, String> {
        let scratch = aterm_tempfile::Builder::new()
            .prefix("atpkg-git-lazy-fetch-")
            .tempdir()
            .map_err(|error| format!("cannot make a scratch directory ({error})"))?;
        Ok(Box::new(scratch))
    }

    fn trace_file(&self, path: &Path) -> Result<(), String> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options
            .open(path)
            .map(drop)
            .map_err(|error| format!("cannot prepare trace ({error})"))
    }

    fn read_trace(&self, path: &Path, limit: usize) -> Result<Vec<u8>, String> {
        let file =
            std::fs::File::open(path).map_err(|error| format!("cannot read trace ({error})"))?;
        let regular = file
            .metadata()
            .map_err(|error| format!("cannot read trace ({error})"))?
            .is_file();
        if !regular {
            return Err("cannot read trace (not a regular file)".to_owned());
        }
        let mut bytes = Vec::new();
        file.take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|error| format!("cannot read trace ({error})"))?;
        if bytes.len() > limit {
            return Err(format!("cannot read trace (over {limit} bytes)"));
        }
        Ok(bytes)
    }
}
