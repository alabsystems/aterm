// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Output;

/// What a read's `git` is: its folder, arguments, global and offline settings, home,
/// `XDG_CONFIG_HOME`, the bytes fed to its standard input and the index it reads in place of
/// the repository's.
pub(super) type Key = (
    PathBuf,
    Vec<String>,
    bool,
    bool,
    PathBuf,
    Option<PathBuf>,
    Option<Vec<u8>>,
    Option<PathBuf>,
);

/// How a read meets this thread's memo.
pub(super) enum Read {
    /// No [`Memo`] is held: the read starts its `git`.
    Off,
    /// A [`Memo`] is held but could not read back what the read feeds its `git`: the read
    /// starts its `git`, is counted in [`Memo::missed`] and is not kept.
    Unnamed,
    /// A [`Memo`] is held and the read is this key.
    Named(Key),
}

std::thread_local! {
    static MEMORY: RefCell<Option<HashMap<Key, Output>>> = const { RefCell::new(None) };
    static MISSED: Cell<u32> = const { Cell::new(0) };
    static CHECKING: Cell<bool> = const { Cell::new(false) };
}

/// Held, this thread's reads are memoized, from an empty memo; dropped, none is.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) struct Memo(());

/// Memoize this thread's reads until the [`Memo`] is dropped.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn on() -> Memo {
    assert!(
        MEMORY.with(|m| m.borrow().is_none()),
        "a memo is already held on this thread"
    );
    MEMORY.with(|m| *m.borrow_mut() = Some(HashMap::new()));
    MISSED.with(|s| s.set(0));
    CHECKING.with(|c| c.set(false));
    Memo(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Memo {
    /// The reads that came to this memo while it was held and that it could not answer, each
    /// of which started its `git` (a read cut or refused before its start never comes to it).
    pub(crate) fn missed(&self) -> u32 {
        MISSED.with(Cell::get)
    }

    /// `run`'s value, with each read this memo answers during it also starting its `git`
    /// again, whose output (exit status, standard output and standard error) must equal the
    /// answer: a read whose answer differs, or whose `git` gives no output to compare,
    /// panics naming its key.
    pub(crate) fn checking<T>(&self, run: impl FnOnce() -> T) -> T {
        struct Done;
        impl Drop for Done {
            fn drop(&mut self) {
                CHECKING.with(|c| c.set(false));
            }
        }
        CHECKING.with(|c| c.set(true));
        let _done = Done;
        run()
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Drop for Memo {
    fn drop(&mut self) {
        MEMORY.with(|m| *m.borrow_mut() = None);
        CHECKING.with(|c| c.set(false));
    }
}

/// How a read of `args` in `dir`, fed `feed` where given, meets this thread's memo.
pub(super) fn read(
    dir: &Path,
    args: &[&str],
    reads: &super::Reads<'_>,
    global: bool,
    offline: bool,
    feed: Option<&std::fs::File>,
    index: Option<&Path>,
) -> Read {
    if MEMORY.with(|m| m.borrow().is_none()) {
        return Read::Off;
    }
    let fed = match feed.map(fed) {
        None => None,
        Some(Some(bytes)) => Some(bytes),
        Some(None) => return Read::Unnamed,
    };
    Read::Named((
        dir.to_path_buf(),
        args.iter().map(|a| (*a).to_string()).collect(),
        global,
        offline,
        reads.home.to_path_buf(),
        reads.xdg.map(Path::to_path_buf),
        fed,
        index.map(Path::to_path_buf),
    ))
}

/// The whole of `file` from its start, read without moving its offset, so the `git` it feeds
/// still reads all of it; `None` where it cannot be read.
#[cfg(unix)]
pub(super) fn fed(file: &std::fs::File) -> Option<Vec<u8>> {
    use std::os::unix::fs::FileExt as _;
    let mut body = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let at = u64::try_from(body.len()).ok()?;
        let n = file.read_at(&mut chunk, at).ok()?;
        if n == 0 {
            return Some(body);
        }
        body.extend_from_slice(chunk.get(..n)?);
    }
}

/// No read back here: a fed read is [`Read::Unnamed`].
#[cfg(not(unix))]
pub(super) fn fed(_: &std::fs::File) -> Option<Vec<u8>> {
    None
}

/// What the read gave before, where the memo holds it (each such answer checked against a
/// fresh `run` under [`Memo::checking`], and otherwise given without calling `run`); else
/// `run`'s output, which a [`Read::Named`] read keeps for the next read with the same key
/// where its `git` exited (one a signal ended is not kept).
pub(super) fn through<E: std::fmt::Debug>(
    read: Read,
    run: impl FnOnce() -> Result<Output, E>,
) -> Result<Output, E> {
    let key = match read {
        Read::Off => return run(),
        Read::Unnamed => {
            MISSED.with(|s| s.set(s.get().saturating_add(1)));
            return run();
        }
        Read::Named(key) => key,
    };
    let known = MEMORY.with(|m| m.borrow().as_ref().and_then(|m| m.get(&key).cloned()));
    if let Some(known) = known {
        if CHECKING.with(Cell::get) {
            match run() {
                Ok(again) => assert_eq!(
                    again, known,
                    "the memo's answer to {key:?} is what its git gives again"
                ),
                Err(e) => panic!("the memo's answer to {key:?}: its git gave no output: {e:?}"),
            }
        }
        return Ok(known);
    }
    MISSED.with(|s| s.set(s.get().saturating_add(1)));
    let output = run()?;
    if output.status.code().is_some() {
        MEMORY.with(|m| {
            if let Some(m) = m.borrow_mut().as_mut() {
                m.insert(key, output.clone());
            }
        });
    }
    Ok(output)
}
