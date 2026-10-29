// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What ran for a session's commands ([`Ran`]): the images the kernel mapped in the session's
//! processes, each with the identity of the file it was mapped from, placed in the block whose
//! command ran it ([`During`]): an exec heard while its own job held the terminal's foreground is
//! placed exactly, by the block the parse stage finds running where the shell's output stood
//! when it was heard ([`Spot`], [`Places`]); one placed by no job (a sampled image, the shell's
//! own group) counts by its stamp in the block's [`Window`]. A command's detail names its own
//! program only when the file the shell would exec for its program word is an image that ran in
//! its block, or a `#!` script whose interpreter ran there, compared by file identity and never
//! by a string a process can write ([`Heard::look`], [`crate::session_status::command_detail`]);
//! otherwise the first program its own job ran may stand in ([`Heard::stand_in`]). So a word the
//! person typed reaches `detail=` only when it names a program file that ran, and a stand-in only
//! names a file the command's own job ran, never a prompt hook's or another command's. A ring
//! entry holds only bounded names ([`ExecNames::bounded`]) and is shared with every reader, and
//! the file system is asked only on the lookup worker ([`Worker`]), never on the thread that
//! reads a detail.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How many execs one session keeps, oldest dropped first; as many wait at most to be stamped.
#[cfg(any(unix, test))]
pub(crate) const RAN_MAX: usize = 128;

/// How long before its block's C stamp an exec placed by its stamp alone ([`During::Window`],
/// [`During::Shell`]) still counts as the block's (ms): a block's window opens here unless the
/// block before it still counts. A prompt hook's exec stamped in it confirms only a program word
/// whose file is that hook's program itself, and never stands in: a stand-in is only an exec
/// placed in the block's own job ([`During::Job`]).
pub(crate) const LEAD_MS: u64 = 1_000;

/// How long after its block's D stamp an exec placed by its stamp alone still counts as the
/// block's (ms), unless the next block's C comes first: an exec is stamped once the parse stage
/// has parsed where the shell's output stood when it was heard, so a short program's exec,
/// heard with its D, is stamped just after D. A job's exec is placed exactly and never by this.
pub(crate) const LAG_MS: u64 = 100;

/// The length under which a macOS exec name ([`ExecNames::exec`]) is whole: `p_comm` keeps at
/// most 15 bytes (`MAXCOMLEN` with its NUL) and `p_name` 31, so a shorter one was not cut.
const EXEC_NAME_WHOLE: usize = 15;

/// The longest file name a ring entry keeps (`NAME_MAX`): no file is named longer.
#[cfg(any(unix, test))]
pub(crate) const NAME_MAX: usize = 255;

/// The longest path a ring entry keeps or a lookup resolves (`PATH_MAX`): `execve` refuses a
/// longer one, so nothing longer names a program that ran.
pub(crate) const PATH_MAX: usize = 1_024;

/// How many paths an exec was handed one ring entry keeps ([`ExecNames::handed`]).
#[cfg(any(unix, test))]
pub(crate) const HANDED_MAX: usize = 2;

/// The longest `PATH` an exec's environment steers by; a longer one steers nothing.
pub(crate) const ENV_PATH_MAX: usize = 4_096;

/// How many distinct `PATH`s one session's ring keeps ([`ExecNames::env_path`] shares them).
#[cfg(any(unix, test))]
pub(crate) const PATHS_KEPT: usize = 4;

/// The most of an exec's `KERN_PROCARGS2` record read for its steers, into one buffer per
/// gather: a record longer than this (an argument of hundreds of KB) steers nothing and is never
/// parsed or copied.
#[cfg(target_vendor = "apple")]
const STEER_READ_MAX: usize = 64 * 1_024;

/// macOS's `/bin/sh` is a stub that re-execs the shell `/private/var/select/sh` links to (bash,
/// zsh or dash: sh(1)), so an image of that shell is an image of `/bin/sh` ([`Resolution::stub`]).
#[cfg(any(target_vendor = "apple", test))]
const SH_STUB: Option<(&str, &str)> = Some(("/bin/sh", "/private/var/select/sh"));
#[cfg(not(any(target_vendor = "apple", test)))]
const SH_STUB: Option<(&str, &str)> = None;

/// A file's identity: its device and inode, which every path to it shares (a hard link, a
/// symlink followed), and which no process can give another file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct FileId {
    /// The device.
    pub(crate) dev: u64,
    /// The inode.
    pub(crate) ino: u64,
}

/// What the kernel keeps for one image, and what may steer the lookup of a program word. Only
/// [`ExecNames::image`] and [`ExecNames::exec`] (a bare word equal to one), and
/// [`ExecNames::file`] (the file a word resolves to, by identity), confirm a word; the rest are
/// strings from the process's own memory, which steer where a lookup looks and never confirm.
/// Every field is bounded ([`ExecNames::bounded`]): a ring entry never holds an argument word.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ExecNames {
    /// The canonical executable's file name (macOS `proc_pidpath`, Linux `/proc/<pid>/exe`):
    /// what a stand-in shows, and a bare word equal to it confirms.
    pub(crate) image: Option<Box<str>>,
    /// macOS's `p_name` (else `p_comm`): the name the kernel gave the exec from the file it
    /// mapped, which no process sets afterwards; a bare word equal to it confirms. Never read on
    /// Linux, whose `comm` the process sets (`prctl(PR_SET_NAME)`, perl's `$0`).
    pub(crate) exec: Option<Box<str>>,
    /// The identity of the file the image was mapped from (macOS: the file `proc_pidpath` names;
    /// Linux: `/proc/<pid>/exe`, the mapped file itself).
    pub(crate) file: Option<FileId>,
    /// The image's own `PATH` where the kernel shows it (macOS hides a platform binary's): steers
    /// a bare word's lookup. One of the session's few kept `PATH`s once stamped ([`PATHS_KEPT`]).
    pub(crate) env_path: Option<Arc<str>>,
    /// The absolute paths the exec was given as its file (macOS `KERN_PROCARGS2`'s exec path,
    /// and `argv[0]`, which keeps it once `/bin/sh`'s stub re-execs): each steers a bare word of
    /// its file name to the file the shell's own `PATH` lookup handed the kernel. Only a path
    /// whose file name no confirming name already is, never an argument word after `argv[0]`
    /// (a path handed to a program as its argument is never the program), never shown, stored or
    /// sent.
    pub(crate) handed: Box<[Box<str>]>,
    /// Whether the image's file has other names (macOS, `st_nlink` above one): `proc_pidpath`
    /// then names whichever of them the kernel last cached, so [`Self::image`] may be another
    /// program's name (macOS's developer-tool shim is one file under dozens of names, `python3`
    /// and `git` among them), and a stand-in shows the name it was exec'd by ([`Self::shown`]).
    pub(crate) linked: bool,
}

impl ExecNames {
    /// The names a bare program word may equal: the canonical file's and the kernel's exec name.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &str> {
        self.image
            .as_deref()
            .into_iter()
            .chain(self.exec.as_deref())
    }

    /// The name a detail shows for this image when it stands in: the canonical executable's file
    /// name; for a file with other names ([`Self::linked`]), the kernel's exec name, the one it was
    /// exec'd by, when it is short enough to be whole, else none (never another link's name).
    pub(crate) fn shown(&self) -> Option<&str> {
        if !self.linked {
            return self.image.as_deref();
        }
        self.exec
            .as_deref()
            .filter(|exec| exec.len() < EXEC_NAME_WHOLE)
    }

    /// Whether nothing that confirms was read.
    #[cfg(any(unix, test))]
    pub(crate) fn is_empty(&self) -> bool {
        self.image.is_none() && self.exec.is_none() && self.file.is_none()
    }

    /// Names from an image's path alone.
    #[cfg(any(unix, test))]
    pub(crate) fn of_path(path: &Path) -> Self {
        Self {
            image: file_name(path),
            ..Self::default()
        }
    }

    /// These names as a ring entry keeps them, whatever they were read from: a name longer
    /// than [`NAME_MAX`], a handed path longer than [`PATH_MAX`] (or whose file name a confirming
    /// name already is: it steers nothing new) and a `PATH` longer than [`ENV_PATH_MAX`] are
    /// dropped, and at most [`HANDED_MAX`] handed paths are kept. So an entry's size is bounded
    /// whatever an exec was given.
    #[cfg(any(unix, test))]
    pub(crate) fn bounded(mut self) -> Self {
        let name = |n: Option<Box<str>>| n.filter(|n| n.len() <= NAME_MAX);
        self.image = name(self.image.take());
        self.exec = name(self.exec.take());
        self.env_path = self.env_path.take().filter(|p| p.len() <= ENV_PATH_MAX);
        let mut handed: Vec<Box<str>> = Vec::new();
        for path in std::mem::take(&mut self.handed) {
            let named = Path::new(&*path)
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| self.iter().any(|kept| kept == n));
            if path.len() <= PATH_MAX
                && path.starts_with('/')
                && !named
                && !handed.contains(&path)
                && handed.len() < HANDED_MAX
            {
                handed.push(path);
            }
        }
        self.handed = handed.into();
        self
    }

    /// The bytes these names hold.
    #[cfg(test)]
    pub(crate) fn held_bytes(&self) -> usize {
        self.iter().map(str::len).sum::<usize>()
            + self.handed.iter().map(|h| h.len()).sum::<usize>()
    }
}

/// `path`'s file name, when it has a non-empty one.
#[cfg(any(unix, test))]
fn file_name(path: &Path) -> Option<Box<str>> {
    path.file_name()
        .map(|name| name.to_string_lossy())
        .filter(|name| !name.is_empty())
        .map(Into::into)
}

/// One image the kernel mapped in one of the session's processes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Exec {
    /// When it counts from, in wall ms (the clock the blocks' C and D stamps read): once the
    /// parse stage had parsed where the shell's output stood when it was heard ([`Ran::hear`]).
    pub(crate) at_ms: u64,
    /// The process that mapped it.
    pub(crate) pid: i32,
    /// Its process group as heard (`0`: unread).
    pub(crate) pgid: i32,
    /// Its names.
    pub(crate) names: ExecNames,
    /// The block it ran in, as placed.
    pub(crate) during: During,
}

/// Which block an exec ran in, as the parse stage placed it ([`Ran::heard`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum During {
    /// Placed by its stamp alone, in the [`Window`] of the block that holds it: an image read at
    /// a sampled edge (where no watch hears execs), an exec heard where the PTY's queue cannot
    /// be read, a test's. It may confirm a program word; it never stands in for one.
    #[default]
    Window,
    /// Heard while its group, a job of its own, held the terminal's foreground: the block that
    /// ran where the shell's output stood then (`None`: none ran there, as for a prompt hook's
    /// job, which runs after the block's D or before the next block's C). Exact: the shell writes
    /// a block's C before it starts the block's job and its D only once the job is done, and the
    /// job still held the foreground once the output's place was read.
    #[cfg_attr(
        not(any(unix, test)),
        expect(dead_code, reason = "off unix no exec is heard, so none is placed")
    )]
    Job(Option<u64>),
    /// Heard in the shell's own group (pwsh runs its children there; a command substitution's):
    /// the block that ran where the shell's output stood at most when it was heard, so the exec
    /// came before that block's D, but not surely after its C.
    #[cfg_attr(
        not(any(unix, test)),
        expect(dead_code, reason = "off unix no exec is heard, so none is placed")
    )]
    Shell(Option<u64>),
}

impl Exec {
    /// Whether this exec counts for block `block`, whose window is `window`, in a session whose
    /// shell runs each command as a job of its own (`jobs`: job control) or runs every command in
    /// its own group (pwsh) ([`Ran::runs_jobs`], what aterm launched, never what the session has
    /// heard so far): as an image a program word's
    /// file may be (the first), and as one that may stand in for it (the second, which implies
    /// the first). A job's exec counts for its own block alone; the shell's group's counts by its
    /// stamp in a session with no job control (pwsh), and else only in the block it was placed
    /// in, where it never stands in; a stamp alone never stands in.
    pub(crate) fn counts(&self, block: u64, window: Option<Window>, jobs: bool) -> (bool, bool) {
        let stamped = window.is_some_and(|w| w.holds(self.at_ms));
        match self.during {
            During::Job(at) => {
                let own = at == Some(block);
                (own, own)
            }
            During::Shell(at) => {
                let here = stamped && at == Some(block);
                (here || (stamped && !jobs), here && !jobs)
            }
            During::Window => (stamped, false),
        }
    }
}

/// Where the gather was when it heard an exec ([`Ran::hear`]): with the bytes it had handed on,
/// where the shell's output stood then.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(any(unix, test)),
    expect(dead_code, reason = "off unix no exec is heard, so none is placed")
)]
pub(crate) struct Spot {
    /// The bytes it had read and not yet handed on.
    pub(crate) filled: usize,
    /// The bytes the PTY held, read after the exec was heard and before the foreground was, when
    /// the exec's group still held the foreground then (`None`: it did not, or the PTY's queue
    /// cannot be read here, and the exec is placed by its stamp).
    pub(crate) queued: Option<usize>,
    /// Whether its group was a job of its own, not the shell's.
    pub(crate) job: bool,
}

/// The blocks one batch's parse found running at the offsets the execs it holds wait for
/// ([`Ran::places_in`], [`Ran::parsed_through`]): the parse stage ends a chunk at each, so it
/// reads the engine exactly where the shell's output stood when each exec was heard.
#[derive(Debug, Default)]
pub(crate) struct Places {
    /// The offsets in the batch (ascending, inside it, never its end) an exec waits for.
    cuts: Vec<usize>,
    /// The block running at each cut parsed so far.
    seen: Vec<(usize, Option<u64>)>,
    /// Where the parse stopped last, and the block running there.
    last: Option<(usize, Option<u64>)>,
}

impl Places {
    /// The first offset after `off` (an offset in the batch) the parse must stop at.
    pub(crate) fn next_cut(&self, off: usize) -> Option<usize> {
        self.cuts.iter().copied().find(|cut| *cut > off)
    }

    /// The parse reached `at` (an offset in the batch), `running` the block executing there.
    pub(crate) fn parsed(&mut self, at: usize, running: Option<u64>) {
        if self.cuts.binary_search(&at).is_ok() && !self.seen.iter().any(|(s, _)| *s == at) {
            self.seen.push((at, running));
        }
        self.last = Some((at, running));
    }

    /// The block running at `at`, when the parse stopped there.
    #[cfg(any(unix, test))]
    fn at(&self, at: usize) -> Option<Option<u64>> {
        self.last
            .filter(|(last, _)| *last == at)
            .or_else(|| self.seen.iter().copied().find(|(s, _)| *s == at))
            .map(|(_, running)| running)
    }
}

/// Whether an exec's `KERN_PROCARGS2` record is read for its steers, asked with the file its
/// image was mapped from ([`read_names`], [`RanFeed::exec`]).
#[cfg(unix)]
pub(crate) type Steering<'a> = &'a mut dyn FnMut(Option<FileId>) -> bool;

/// The names of process `pid`'s image, read now ([`ExecNames`]), with its steers when `steer`
/// answers yes for the image's file ([`RanFeed::exec`] says when).
#[cfg(unix)]
pub(crate) fn read_names(pid: i32, steer: Steering<'_>) -> ExecNames {
    let Some(id) = u32::try_from(pid).ok().filter(|p| *p > 1) else {
        return ExecNames::default();
    };
    let exe = aterm_sysprobe::process_path(id);
    let (file, linked) =
        image_file(id, exe.as_deref()).map_or((None, false), |(f, l)| (Some(f), l));
    let (env_path, handed) = if steer(file) {
        exec_args(id)
    } else {
        (None, Box::default())
    };
    ExecNames {
        image: exe.as_deref().and_then(file_name),
        exec: exec_name(pid),
        file,
        env_path: env_path.map(Arc::from),
        handed,
        linked,
    }
}

/// The identity of the file `path` names, links followed.
#[cfg(unix)]
pub(crate) fn file_id(path: &Path) -> Option<FileId> {
    file_and_links(path).map(|(id, _)| id)
}

/// The identity of the file `path` names, links followed, and whether it has other names
/// (`st_nlink` above one).
#[cfg(unix)]
fn file_and_links(path: &Path) -> Option<(FileId, bool)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| {
        (
            FileId {
                dev: m.dev(),
                ino: m.ino(),
            },
            m.nlink() > 1,
        )
    })
}

/// The identity of the file `pid`'s image was mapped from: Linux's `/proc/<pid>/exe` is the
/// mapped file itself, wherever its path went, and names the link it was exec'd by, so it is
/// never [`ExecNames::linked`].
#[cfg(any(target_os = "linux", target_os = "android"))]
fn image_file(pid: u32, _exe: Option<&Path>) -> Option<(FileId, bool)> {
    file_id(Path::new(&format!("/proc/{pid}/exe"))).map(|id| (id, false))
}

/// The identity of the file `proc_pidpath` names for `pid`, and whether it has other names
/// ([`ExecNames::linked`]).
#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn image_file(_pid: u32, exe: Option<&Path>) -> Option<(FileId, bool)> {
    exe.and_then(file_and_links)
}

#[cfg(target_vendor = "apple")]
thread_local! {
    /// The gather's one buffer for [`exec_args`]' reads: never allocated per exec.
    static STEER_BUF: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The `PATH` and the paths of one `KERN_PROCARGS2` read ([`steers_of_procargs`]), when the
/// record is at most [`STEER_READ_MAX`] bytes: a longer one is refused unparsed.
#[cfg(target_vendor = "apple")]
fn exec_args(pid: u32) -> (Option<Box<str>>, Box<[Box<str>]>) {
    STEER_BUF.with_borrow_mut(|buf| {
        if buf.len() != STEER_READ_MAX {
            buf.resize(STEER_READ_MAX, 0);
        }
        aterm_uds::procargs::read_into(pid, buf)
            .and_then(|len| buf.get(..len))
            .map(steers_of_procargs)
            .unwrap_or_default()
    })
}

/// Linux shows a process's environment (`/proc/<pid>/environ`): its `PATH`. No argument word is
/// read.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn exec_args(pid: u32) -> (Option<Box<str>>, Box<[Box<str>]>) {
    let environ = aterm_sysprobe::read_bounded(Path::new(&format!("/proc/{pid}/environ")));
    let path = environ
        .as_deref()
        .and_then(|env| env.split('\0').find_map(|kv| kv.strip_prefix("PATH=")))
        .filter(|p| p.len() <= ENV_PATH_MAX)
        .map(Into::into);
    (path, Box::default())
}

/// Nothing steers a lookup here.
#[cfg(all(
    unix,
    not(any(target_vendor = "apple", target_os = "linux", target_os = "android"))
))]
fn exec_args(_pid: u32) -> (Option<Box<str>>, Box<[Box<str>]>) {
    (None, Box::default())
}

/// The image's `PATH` (at most [`ENV_PATH_MAX`] bytes) and the absolute paths it was given as
/// its file ([`ExecNames::handed`]: the exec path and `argv[0]`, each at most [`PATH_MAX`]
/// bytes) of one `KERN_PROCARGS2` answer; a word that is no UTF-8 names nothing, and no argument
/// word after `argv[0]` is read.
#[cfg(any(target_vendor = "apple", test))]
pub(crate) fn steers_of_procargs(buf: &[u8]) -> (Option<Box<str>>, Box<[Box<str>]>) {
    let Some(args) = aterm_uds::procargs::parse(buf) else {
        return (None, Box::default());
    };
    let env_path = args
        .env()
        .find_map(|kv| kv.strip_prefix(b"PATH="))
        .filter(|p| p.len() <= ENV_PATH_MAX)
        .and_then(|p| std::str::from_utf8(p).ok())
        .map(Into::into);
    let mut handed: Vec<Box<str>> = Vec::new();
    let words = std::iter::once(args.exec_path).chain(args.argv.first().copied());
    for word in words.filter_map(|w| std::str::from_utf8(w).ok()) {
        if word.starts_with('/') && word.len() <= PATH_MAX && !handed.iter().any(|h| **h == *word) {
            handed.push(word.into());
        }
    }
    (env_path, handed.into())
}

/// `p_name` (the exec's file name, up to 32 bytes), else `p_comm` (16).
#[cfg(target_vendor = "apple")]
fn exec_name(pid: i32) -> Option<Box<str>> {
    let info = aterm_uds::process::bsdinfo_uniq(pid)?;
    let text = |raw: &[libc::c_char]| -> Option<Box<str>> {
        let bytes: Vec<u8> = raw
            .iter()
            .map(|c| c.cast_unsigned())
            .take_while(|b| *b != 0)
            .collect();
        (!bytes.is_empty()).then(|| String::from_utf8_lossy(&bytes).into())
    };
    text(&info.pbsd.pbi_name).or_else(|| text(&info.pbsd.pbi_comm))
}

/// No exec name is read here: Linux's `comm` is the process's own to set, so it confirms
/// nothing.
#[cfg(all(unix, not(target_vendor = "apple")))]
const fn exec_name(_pid: i32) -> Option<Box<str>> {
    None
}

/// How the session finds the file a program word names, as `execve` would: the real file
/// system, or a test's table ([`Table`]).
pub(crate) trait Files: Send + Sync + std::fmt::Debug {
    /// The identity of the regular file with an execute bit `path` names, links followed;
    /// `None` for anything else (missing, a directory, a file no one may execute).
    fn program(&self, path: &Path) -> Option<FileId>;
    /// What follows the `#!` of the file `path` names, up to its line's end, when its first two
    /// bytes are `#!`.
    fn shebang(&self, path: &Path) -> Option<Box<str>>;
    /// Whether `path` names a directory, links followed.
    fn directory(&self, path: &Path) -> bool;
}

/// The real file system.
#[derive(Debug, Default)]
#[cfg_attr(
    all(test, not(unix)),
    expect(
        dead_code,
        reason = "a test's ran log reads a table; only the Unix tests read the real files"
    )
)]
pub(crate) struct RealFiles;

/// How many bytes of a script a `#!` line is read from (the longest macOS's kernel reads).
#[cfg(unix)]
const SHEBANG_MAX: usize = 512;

#[cfg(unix)]
impl Files for RealFiles {
    fn program(&self, path: &Path) -> Option<FileId> {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let meta = std::fs::metadata(path).ok()?;
        (meta.is_file() && meta.permissions().mode() & 0o111 != 0).then(|| FileId {
            dev: meta.dev(),
            ino: meta.ino(),
        })
    }

    fn shebang(&self, path: &Path) -> Option<Box<str>> {
        use std::io::Read;
        if !std::fs::metadata(path).is_ok_and(|m| head_is_local(&m)) {
            return None;
        }
        let mut file = std::fs::File::open(path).ok()?;
        let mut buf = [0u8; SHEBANG_MAX];
        let mut n = 0;
        while n < buf.len() {
            match file.read(&mut buf[n..]) {
                Ok(0) => break,
                Ok(k) => n += k,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return None,
            }
        }
        shebang_line(&buf[..n])
    }

    fn directory(&self, path: &Path) -> bool {
        std::fs::metadata(path).is_ok_and(|m| m.is_dir())
    }
}

/// macOS's `SF_DATALESS` (`<sys/stat.h>`): the file's data is in the cloud, and reading it
/// would download it first.
#[cfg(any(target_vendor = "apple", test))]
const SF_DATALESS: u32 = 0x4000_0000;

/// Whether a file's first bytes can be read without fetching it: a dataless file (an iCloud
/// Drive or File Provider placeholder) is never opened for its `#!` line, which would download
/// it on the reading thread; it reads as no script.
#[cfg(target_vendor = "apple")]
fn head_is_local(meta: &std::fs::Metadata) -> bool {
    use std::os::darwin::fs::MetadataExt;
    local_flags(meta.st_flags())
}

/// [`head_is_local`] by the file's `st_flags`.
#[cfg(any(target_vendor = "apple", test))]
const fn local_flags(flags: u32) -> bool {
    flags & SF_DATALESS == 0
}

/// Every file's first bytes are its own here.
#[cfg(all(unix, not(target_vendor = "apple")))]
const fn head_is_local(_meta: &std::fs::Metadata) -> bool {
    true
}

/// Nothing is found here: no exec is heard either.
#[cfg(not(unix))]
impl Files for RealFiles {
    fn program(&self, _path: &Path) -> Option<FileId> {
        None
    }

    fn shebang(&self, _path: &Path) -> Option<Box<str>> {
        None
    }

    fn directory(&self, _path: &Path) -> bool {
        false
    }
}

/// What follows the `#!` of a file whose first bytes are `head`, up to the line's end.
#[cfg(any(unix, test))]
fn shebang_line(head: &[u8]) -> Option<Box<str>> {
    let rest = head.strip_prefix(b"#!")?;
    let line = rest.split(|b| *b == b'\n').next().unwrap_or(rest);
    std::str::from_utf8(line).ok().map(Into::into)
}

/// A test's file system: each path that is a program file, its identity and its `#!` line, and
/// each path that is a directory.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct Table {
    files: std::collections::BTreeMap<PathBuf, (FileId, Option<Box<str>>)>,
    dirs: std::collections::BTreeSet<PathBuf>,
}

#[cfg(test)]
impl Table {
    /// `path` is an image file whose identity is [`test_id`]`(id_of)`.
    pub(crate) fn program(mut self, path: &str, id_of: &str) -> Self {
        self.files.insert(path.into(), (test_id(id_of), None));
        self
    }

    /// `path` is a script file (its own identity) whose first line is `#!` then `line`.
    pub(crate) fn script(mut self, path: &str, line: &str) -> Self {
        let id = test_id(&format!("script:{path}"));
        self.files.insert(path.into(), (id, Some(line.into())));
        self
    }

    /// `path` is a directory.
    pub(crate) fn dir(mut self, path: &str) -> Self {
        self.dirs.insert(path.into());
        self
    }
}

#[cfg(test)]
impl Table {
    /// The oracle of the detail's law: whether some file of the table named `name` is, by
    /// identity, one of `ran`'s images, or a `#!` script whose interpreter file (or `/bin/sh`'s
    /// stub's shell, or an `env` line's program on `/usr/bin`) is.
    pub(crate) fn ran_by_identity(&self, name: &str, ran: &[Exec]) -> bool {
        let files: Vec<FileId> = ran.iter().filter_map(|e| e.names.file).collect();
        let id_of = |path: &str| self.files.get(Path::new(path)).map(|(id, _)| *id);
        let mapped = |id: Option<FileId>| {
            id.is_some_and(|id| {
                files.contains(&id)
                    || (id_of("/bin/sh") == Some(id)
                        && id_of("/private/var/select/sh").is_some_and(|s| files.contains(&s)))
            })
        };
        self.files.iter().any(|(path, (id, line))| {
            path.file_name().is_some_and(|n| n == name)
                && (mapped(Some(*id))
                    || line.as_deref().is_some_and(|line| {
                        let mut words = line.split(' ');
                        let interpreter = words.next().unwrap_or_default();
                        mapped(id_of(interpreter))
                            || (id_of(interpreter)
                                .is_some_and(|id| id_of("/usr/bin/env") == Some(id))
                                && env_program(words)
                                    .is_some_and(|p| mapped(id_of(&format!("/usr/bin/{p}")))))
                    }))
        })
    }
}

#[cfg(test)]
impl Files for Table {
    fn program(&self, path: &Path) -> Option<FileId> {
        self.files.get(path).map(|(id, _)| *id)
    }

    fn shebang(&self, path: &Path) -> Option<Box<str>> {
        self.files.get(path).and_then(|(_, line)| line.clone())
    }

    fn directory(&self, path: &Path) -> bool {
        self.dirs.contains(path)
    }
}

/// A test image's file identity: one per name, the same for every path a table links to it.
#[cfg(test)]
pub(crate) fn test_id(name: &str) -> FileId {
    let ino = name.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    FileId { dev: 7, ino }
}

/// The wall-ms span in which a block's execs count: from its C stamp less [`LEAD_MS`] (never
/// where the block before it still counts: its execs are its own) through its end plus
/// [`LAG_MS`], cut before the next block's C, open while it runs. Its end is its D stamp, else
/// (a D the shell never wrote) the prompt that closed it. Consecutive blocks' windows never
/// overlap, so an exec counts for one block at most. Since H2 the window is the fallback: an
/// exec the parse stage placed in the stream counts by its place ([`During`],
/// [`Exec::counts`]), and only one with no place (no queue read, as on Linux, or a group that
/// lost the foreground before the read) counts by its stamp here, never standing in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Window {
    /// The first wall ms counted.
    pub(crate) from: u64,
    /// The last wall ms counted, `None` while the block runs (before `from`: none counts).
    pub(crate) to: Option<u64>,
}

impl Window {
    /// The window of a block with C stamped at `exec_ms` and its end at `end_ms`, after a block
    /// whose window's last ms was `after_ms`, before a block whose C is stamped at `next_ms`;
    /// `None` for a block with no C stamp, in which no exec is counted (fail closed).
    pub(crate) fn of(
        exec_ms: Option<u64>,
        end_ms: Option<u64>,
        after_ms: Option<u64>,
        next_ms: Option<u64>,
    ) -> Option<Self> {
        let exec_ms = exec_ms?;
        let from = exec_ms
            .saturating_sub(LEAD_MS)
            .max(after_ms.map_or(0, |a| a.saturating_add(1)).min(exec_ms));
        Some(Self {
            from,
            to: Self::last(exec_ms, end_ms, next_ms),
        })
    }

    /// The last ms a block with C at `exec_ms`, ended at `end_ms`, counts: its end plus the lag,
    /// cut before the next block's C at `next_ms`.
    fn last(exec_ms: u64, end_ms: Option<u64>, next_ms: Option<u64>) -> Option<u64> {
        let last = end_ms?.max(exec_ms).saturating_add(LAG_MS);
        Some(next_ms.map_or(last, |next| last.min(next.saturating_sub(1))))
    }

    /// The window of `block`, after `before` (the block before it: its window's end, its own end
    /// being its D, else `block`'s A, the prompt that closed it), ending at its D or, for a block
    /// a newer prompt closed with no D, at `closed_ms`, that prompt's A, and cut before
    /// `next_ms`, the C of the block after it.
    pub(crate) fn of_block(
        block: &aterm_types::OutputBlock,
        before: Option<&aterm_types::OutputBlock>,
        closed_ms: Option<u64>,
        next_ms: Option<u64>,
    ) -> Option<Self> {
        let after = before.and_then(|b| {
            let end = b.command_end_time_ms.or(block.prompt_time_ms);
            match b.command_exec_start_time_ms {
                Some(exec) => Self::last(exec, end, block.command_exec_start_time_ms),
                // A block with no C counted nothing, but it ran no later than its end.
                None => end.map(|end| end.saturating_sub(1)),
            }
        });
        Self::of(
            block.command_exec_start_time_ms,
            block.command_end_time_ms.or(closed_ms),
            after,
            next_ms,
        )
    }

    /// Whether `at_ms` falls in it.
    pub(crate) fn holds(self, at_ms: u64) -> bool {
        at_ms >= self.from && self.to.is_none_or(|to| at_ms <= to)
    }
}

/// One session's execs: those stamped, oldest first and at most [`RAN_MAX`], and those the
/// gather heard that wait for the parse stage to parse where the shell's output stood when each
/// was heard (at most [`RAN_MAX`] too).
#[derive(Debug, Default)]
struct Log {
    /// Shared with every reader ([`Ran::heard`]): a read copies no names.
    execs: VecDeque<Arc<Exec>>,
    /// Execs heard but not yet placed.
    #[cfg(any(unix, test))]
    held: VecDeque<Held>,
    /// The bytes of the session's output the gather has handed on.
    #[cfg(any(unix, test))]
    handed: u64,
    /// The bytes the parse stage has parsed.
    #[cfg(any(unix, test))]
    parsed: u64,
    /// The block running where the parse stage stopped (`None` before any parse: not known).
    #[cfg(any(unix, test))]
    at_parsed: Option<Option<u64>>,
    /// Whether the session's shell runs every command in its own group (pwsh), as aterm launched
    /// it ([`Ran::runs_jobs`]): then an exec of the shell's group may be the command's own.
    /// Otherwise (job control: zsh, bash, fish, and a shell aterm does not know) an exec of its
    /// own group is a command substitution's or a hook's, never a command's job. Never learned
    /// from what the session heard, so the same command reads the same whatever ran before it.
    own_group: bool,
    /// The distinct `PATH`s the newest execs' environments showed, newest first, at most
    /// [`PATHS_KEPT`]: each stamped exec's [`ExecNames::env_path`] is one of them.
    paths: VecDeque<Arc<str>>,
    /// The steers of the newest exec whose record was read ([`Ran::steers_for`]).
    #[cfg(unix)]
    steered: Option<Steered>,
}

/// One exec heard and not yet placed ([`Log::held`]).
#[cfg(any(unix, test))]
#[derive(Debug)]
struct Held {
    /// Where the shell's output stood when it was heard: the offset the parse stage parses
    /// through before placing it.
    until: u64,
    /// How the block running there places it: as its job's (`Some(true)`), its shell group's
    /// (`Some(false)`), or not at all (`None`: by its stamp, [`During::Window`]).
    place: Option<bool>,
    exec: Exec,
}

/// The steers one `KERN_PROCARGS2` read gave an exec of `file` that waited for offset `until`
/// ([`Ran::steers_for`]).
#[cfg(unix)]
#[derive(Clone, Debug)]
struct Steered {
    until: u64,
    file: FileId,
    env_path: Option<Arc<str>>,
    handed: Box<[Box<str>]>,
}

/// A byte count as an offset in the session's output.
#[cfg(any(unix, test))]
fn offset(bytes: usize) -> u64 {
    u64::try_from(bytes).unwrap_or(u64::MAX)
}

#[cfg(any(unix, test))]
impl Log {
    fn stamp(&mut self, exec: Exec, at_ms: u64) {
        let mut exec = Exec {
            at_ms,
            names: exec.names.bounded(),
            ..exec
        };
        if let Some(path) = exec.names.env_path.take() {
            exec.names.env_path = Some(self.keep_path(path));
        }
        self.execs.push_back(Arc::new(exec));
        while self.execs.len() > RAN_MAX {
            self.execs.pop_front();
        }
    }

    /// Where an exec heard at `spot` waits: the bytes handed on, those the gather holds, and
    /// those the PTY held.
    fn until(&self, spot: Spot) -> u64 {
        self.handed
            .saturating_add(offset(spot.filled))
            .saturating_add(offset(spot.queued.unwrap_or(0)))
    }

    /// `path` as one of the kept `PATH`s: an equal one already kept, else `path`, kept newest;
    /// the oldest beyond [`PATHS_KEPT`] goes, and the execs that showed it keep none.
    fn keep_path(&mut self, path: Arc<str>) -> Arc<str> {
        if let Some(kept) = self
            .paths
            .iter()
            .position(|p| **p == *path)
            .and_then(|i| self.paths.remove(i))
        {
            self.paths.push_front(Arc::clone(&kept));
            return kept;
        }
        self.paths.push_front(Arc::clone(&path));
        while self.paths.len() > PATHS_KEPT {
            let Some(gone) = self.paths.pop_back() else {
                break;
            };
            for exec in &mut self.execs {
                if exec
                    .names
                    .env_path
                    .as_ref()
                    .is_some_and(|p| Arc::ptr_eq(p, &gone))
                {
                    Arc::make_mut(exec).names.env_path = None;
                }
            }
        }
        path
    }

    /// Place and stamp, at `now`, every held exec whose offset is parsed, in the order heard,
    /// `running` telling the block running at an offset the parse stopped at (`None`: not
    /// known, and the exec is placed by its stamp).
    fn release(&mut self, now: u64, running: impl Fn(u64) -> Option<Option<u64>>) {
        let mut i = 0;
        while i < self.held.len() {
            if self.held.get(i).is_some_and(|h| h.until <= self.parsed)
                && let Some(held) = self.held.remove(i)
            {
                let during = match (held.place, running(held.until)) {
                    (Some(true), Some(block)) => During::Job(block),
                    (Some(false), Some(block)) => During::Shell(block),
                    _ => During::Window,
                };
                self.stamp(
                    Exec {
                        during,
                        ..held.exec
                    },
                    now,
                );
            } else {
                i += 1;
            }
        }
    }

    /// Whether an exec of `pid` is held or stamped.
    #[cfg(unix)]
    fn heard_pid(&self, pid: i32) -> bool {
        self.execs.iter().any(|e| e.pid == pid) || self.held.iter().any(|h| h.exec.pid == pid)
    }
}

/// How many program-word lookups one session remembers ([`Lookups`]).
const LOOKS_MAX: usize = 32;

/// How long a reader waits for the lookup worker's answer to a lookup it has none for
/// ([`Lookups::resolution`]). A lookup is a few `stat`s and one short read, answered well inside
/// it; a reader is the status sweep, on the UI thread, under the session's term lock, so a file
/// system that stalls (an unanswering network mount) costs it this once per lookup, never more,
/// and the word confirms nothing until the worker answers.
#[cfg(not(test))]
const LOOKUP_WAIT: Duration = Duration::from_millis(10);

/// One session's execs, oldest first, at most [`RAN_MAX`]: the PTY gather hears what its
/// [`RanFeed`] reads, a restore notes the holder images it reads, and every reader of a
/// command's detail reads them back by its block's [`Window`] ([`Ran::heard`]). A session with no
/// source (no process watch and no image read, as under ConPTY) holds none, so its details name
/// only shell builtins. It also holds how the session finds a program word's file: the file
/// system ([`Files`]), the home directory a `~/` names, the `PATH` the shell was spawned with,
/// and the lookups it remembers, which a worker thread makes ([`Lookups`]).
pub(crate) struct Ran {
    log: Mutex<Log>,
    files: Arc<dyn Files>,
    home: Option<Arc<Path>>,
    shell_path: Mutex<Option<Arc<str>>>,
    lookups: Lookups,
}

impl std::fmt::Debug for Ran {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ran")
            .field("execs", &self.lock().execs.len())
            .finish_non_exhaustive()
    }
}

impl Default for Ran {
    /// The real file system, looked up on the shared worker; an unavailable worker confirms no
    /// file. In a test, an empty table is looked up in line (the inert arm: no word resolves).
    fn default() -> Self {
        let home = aterm_types::dirs::home_dir().map(Into::into);
        #[cfg(not(test))]
        let ran = {
            let how = Resolving::for_worker(Worker::shared(), LOOKUP_WAIT);
            Self::looked_up(Arc::new(RealFiles), home, how)
        };
        #[cfg(test)]
        let ran = Self::looked_up(Arc::new(Table::default()), home, Resolving::Inline);
        ran
    }
}

impl Ran {
    /// A test's ran log that finds files with `files` in line, `~/` being `home`.
    #[cfg(test)]
    pub(crate) fn with_files(files: Box<dyn Files>, home: Option<Box<Path>>) -> Self {
        Self::looked_up(Arc::from(files), home.map(Arc::from), Resolving::Inline)
    }

    /// A test's ran log that finds files with `files` on `worker`, a reader waiting `wait`.
    #[cfg(test)]
    pub(crate) fn on_worker(files: Arc<dyn Files>, worker: Arc<Worker>, wait: Duration) -> Self {
        Self::looked_up(files, None, Resolving::Worker { worker, wait })
    }

    fn looked_up(files: Arc<dyn Files>, home: Option<Arc<Path>>, how: Resolving) -> Self {
        Self {
            log: Mutex::default(),
            files,
            home,
            shell_path: Mutex::default(),
            lookups: Lookups {
                memo: Arc::default(),
                how,
            },
        }
    }

    fn lock(&self) -> MutexGuard<'_, Log> {
        self.log.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Whether the session's shell, as aterm launched or adopted it, runs each command as a job
    /// of its own (`jobs`: zsh, bash, fish) or every command in its own group (pwsh, `false`): an
    /// exec of the shell's group then counts and stands in by where it was placed ([`Exec::counts`]).
    /// Until told, and for a shell aterm does not know, the session's shell runs jobs, so an exec
    /// of its group never stands in (fail closed): a nested shell's jobs, a restore and the
    /// session's history change nothing.
    pub(crate) fn runs_jobs(&self, jobs: bool) {
        self.lock().own_group = !jobs;
    }

    /// The `PATH` the session's shell was spawned with: what a bare word's lookup uses where no
    /// exec's environment shows one.
    pub(crate) fn spawned_with(&self, path: Option<&str>) {
        *self.shell_path.lock().unwrap_or_else(|p| p.into_inner()) =
            path.filter(|p| p.len() <= ENV_PATH_MAX).map(Into::into);
    }

    /// Note one exec, stamped as given (a restore's read, a test's); one with no name is dropped.
    #[cfg(any(unix, test))]
    pub(crate) fn note(&self, exec: Exec) {
        if exec.names.is_empty() {
            return;
        }
        let at = exec.at_ms;
        self.lock().stamp(exec, at);
    }

    /// Hear one exec the gather read at `spot`: it is placed and stamped once the parse stage
    /// has parsed where the shell's output stood when it was heard ([`Log::until`]), so every
    /// mark the shell wrote before it (its block's C, the next command's) is read first, and the
    /// block then running places it ([`During`]); at once when the parse stage is there. One
    /// with no name is dropped, and beyond [`RAN_MAX`] waiting the oldest waiting is (the ring
    /// would drop it first: a parse stage that never catches up confirms nothing with it).
    #[cfg(unix)]
    pub(crate) fn hear(&self, exec: Exec, spot: Spot) {
        self.hear_at(exec, spot, wall_ms());
    }

    /// [`Ran::hear`] at wall ms `now`.
    #[cfg(any(unix, test))]
    pub(crate) fn hear_at(&self, exec: Exec, spot: Spot, now: u64) {
        if exec.names.is_empty() {
            return;
        }
        let mut log = self.lock();
        let until = log.until(spot);
        log.held.push_back(Held {
            until,
            place: spot.queued.map(|_| spot.job),
            exec,
        });
        while log.held.len() > RAN_MAX {
            log.held.pop_front();
        }
        let (parsed, at_parsed) = (log.parsed, log.at_parsed);
        log.release(now, |until| at_parsed.filter(|_| until == parsed));
    }

    /// Whether an exec of `pid` was heard (held or stamped): a later one of the same process is
    /// its own re-exec, not the shell's.
    #[cfg(unix)]
    fn heard_pid(&self, pid: i32) -> bool {
        self.lock().heard_pid(pid)
    }

    /// The steers an exec of `file` heard at `spot` takes from the newest exec whose record was
    /// read, when that exec mapped the same file and waits for the same offset: the two are
    /// placed in the same block, whose lookups take every exec's steers together, so sharing can
    /// only miss a steer the later exec would have shown, never add one its block lacks.
    #[cfg(unix)]
    fn steers_for(&self, file: Option<FileId>, spot: Spot) -> Option<Steered> {
        let log = self.lock();
        let until = log.until(spot);
        log.steered
            .as_ref()
            .filter(|s| s.until == until && Some(s.file) == file)
            .cloned()
    }

    /// `names` were read with their steers at `spot`: an exec of the same file heard at the same
    /// offset takes them ([`Ran::steers_for`]).
    #[cfg(unix)]
    fn steered(&self, names: &ExecNames, spot: Spot) {
        let mut log = self.lock();
        let until = log.until(spot);
        log.steered = names.file.map(|file| Steered {
            until,
            file,
            env_path: names.env_path.clone(),
            handed: names.handed.clone(),
        });
    }

    /// The gather handed on a batch of `len` bytes: the offset of its end in the session's
    /// output, which the parse stage hands back ([`Ran::places_in`], [`Ran::parsed_through`]).
    #[cfg(any(unix, test))]
    pub(crate) fn handed_on(&self, len: usize) -> u64 {
        let mut log = self.lock();
        log.handed = log.handed.saturating_add(offset(len));
        log.handed
    }

    /// The offsets inside the batch of `len` bytes that ends at `end` at which a held exec
    /// waits, into `places`: the parse ends a chunk at each ([`Places::next_cut`]). Every exec
    /// waiting inside a batch was heard before the batch was handed on, so the list is whole.
    #[cfg(any(unix, test))]
    pub(crate) fn places_in(&self, end: u64, len: usize, places: &mut Places) {
        places.cuts.clear();
        places.seen.clear();
        places.last = None;
        let start = end.saturating_sub(offset(len));
        let log = self.lock();
        for held in log.held.iter().filter(|h| h.place.is_some()) {
            if held.until > start
                && held.until < end
                && let Ok(cut) = usize::try_from(held.until - start)
            {
                places.cuts.push(cut);
            }
        }
        drop(log);
        places.cuts.sort_unstable();
        places.cuts.dedup();
    }

    /// The parse stage parsed the batch of `len` bytes that ends at `end`, finding at each of
    /// its cuts and at its end the block `places` holds: place and stamp, now, every exec that
    /// waited for it.
    #[cfg(unix)]
    pub(crate) fn parsed_through(&self, end: u64, len: usize, places: &Places) {
        self.parsed_at(end, len, places, wall_ms());
    }

    /// [`Ran::parsed_through`] at wall ms `now`.
    #[cfg(any(unix, test))]
    pub(crate) fn parsed_at(&self, end: u64, len: usize, places: &Places, now: u64) {
        let start = end.saturating_sub(offset(len));
        let mut log = self.lock();
        let before = (log.parsed, log.at_parsed);
        log.parsed = log.parsed.max(end);
        log.at_parsed = places.at(len);
        log.release(now, |until| {
            if until == before.0 {
                return before.1;
            }
            until
                .checked_sub(start)
                .and_then(|at| usize::try_from(at).ok())
                .and_then(|at| places.at(at))
        });
    }

    /// The execs stamped in `window`, oldest first, however placed: the ring's own entries,
    /// shared, so a read copies no names.
    #[cfg(test)]
    pub(crate) fn within(&self, window: Option<Window>) -> Vec<Arc<Exec>> {
        let Some(window) = window else {
            return Vec::new();
        };
        self.lock()
            .execs
            .iter()
            .filter(|exec| window.holds(exec.at_ms))
            .map(Arc::clone)
            .collect()
    }

    /// What block `block`, whose window is `window`, run in `cwd` (aterm's own record of its
    /// working directory), is confirmed against: the execs that count for it ([`Exec::counts`]),
    /// and those of its own job, which may stand in.
    pub(crate) fn heard(&self, block: u64, window: Option<Window>, cwd: Option<&str>) -> Heard<'_> {
        let (execs, stand_ins, newest) = {
            let log = self.lock();
            let mut execs = Vec::new();
            let mut stand_ins = Vec::new();
            for exec in &log.execs {
                let (counts, stands) = exec.counts(block, window, !log.own_group);
                if counts {
                    execs.push(Arc::clone(exec));
                }
                if stands {
                    stand_ins.push(Arc::clone(exec));
                }
            }
            (execs, stand_ins, log.paths.front().cloned())
        };
        let mut paths: Vec<Arc<str>> = Vec::new();
        let mut steer = |path: Option<&Arc<str>>| {
            if let Some(path) = path.filter(|p| !paths.contains(p)) {
                paths.push(Arc::clone(path));
            }
        };
        for exec in execs.iter().rev() {
            steer(exec.names.env_path.as_ref());
        }
        steer(newest.as_ref());
        steer(
            self.shell_path
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_ref(),
        );
        Heard {
            execs,
            stand_ins,
            cwd: cwd.filter(|c| c.len() <= PATH_MAX).map(Into::into),
            paths,
            block,
            ran: self,
        }
    }

    /// How many execs wait for the parse stage.
    #[cfg(test)]
    pub(crate) fn waiting(&self) -> usize {
        self.lock().held.len()
    }

    /// The bytes the ring holds in names and `PATH`s (each `PATH` once, however many execs
    /// share it): bounded whatever the execs were given.
    #[cfg(test)]
    pub(crate) fn held_bytes(&self) -> usize {
        let log = self.lock();
        let names = log.execs.iter().map(|e| &e.names);
        let names = names.chain(log.held.iter().map(|h| &h.exec.names));
        let mut paths: Vec<&Arc<str>> = log.paths.iter().collect();
        let mut bytes = 0;
        for names in names {
            bytes += names.held_bytes();
            if let Some(path) = &names.env_path
                && !paths.iter().any(|p| Arc::ptr_eq(p, path))
            {
                paths.push(path);
            }
        }
        bytes + paths.iter().map(|p| p.len()).sum::<usize>()
    }
}

/// How a program word was typed, for the lookup of its file ([`Heard::look`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Word<'a> {
    /// A word with a `/`, as typed: the shell execs that path, from the block's directory.
    Path(&'a str),
    /// A bare name: the shell execs the first file of that name on its `PATH`.
    Bare(&'a str),
}

/// What a program word's file is ([`Heard::look`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Look {
    /// The file the shell would exec is an image an exec in the window mapped, or a `#!` script
    /// whose interpreter one mapped: the word names a program that ran.
    Ran,
    /// A `#!` script whose interpreter was not found among the window's images: its
    /// interpreter may stand in for it until its own name is confirmed.
    Script,
    /// Nothing that ran: a missing path, a file no one may execute, or an image no exec in the
    /// window mapped.
    Nothing,
    /// A directory, and no program file of its name: the shell changes into it (zsh's
    /// `AUTO_CD`, bash's `autocd`, fish's implicit cd), with its own `cd`, so no program of the
    /// command's own runs and none stands in (one run then is a directory hook's, zsh's
    /// `chpwd` or a fish `PWD` handler, never the command's).
    Cd,
    /// Not yet known: the lookup worker has not answered (a file system that stalls). It
    /// confirms nothing, like [`Look::Nothing`], until a later read finds the answer, and gives
    /// no stand-in, like [`Look::Cd`], since the word may name a directory.
    Pending,
}

/// What a program word names on the file system, whatever ran ([`resolve`]): each file the
/// shell would exec for it, in the order it looks, by identity, each with, for a `#!` script,
/// the identities its interpreter may run as; and macOS's `/bin/sh` stub with the shell it
/// re-execs. Judged against a window's execs without touching the file system ([`judge`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Resolution {
    files: Box<[Found]>,
    /// `/bin/sh`'s identity and the identity of the shell its stub re-execs ([`SH_STUB`]).
    stub: Option<(FileId, FileId)>,
    /// No program file, and the word names a directory ([`Look::Cd`]).
    dir: bool,
}

/// One program file a word names ([`Resolution`]).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Found {
    id: FileId,
    /// For a `#!` script, the files its `#!` line's interpreter is: the line's own, and for an
    /// `env` line the program `env` would find on each steering `PATH`.
    script: Option<Box<[FileId]>>,
}

/// What `res` makes of a window whose execs are `execs` ([`Look`]): a file of the word that is
/// an image one of them mapped (for `/bin/sh`, also the shell its stub re-execs) ran, as does a
/// `#!` script whose interpreter's file is one; a script whose interpreter is none is a script.
fn judge(res: &Resolution, execs: &[Arc<Exec>]) -> Look {
    let mapped = |id: FileId| execs.iter().any(|e| e.names.file == Some(id));
    let ran_file = |id: FileId| {
        mapped(id)
            || res
                .stub
                .is_some_and(|(stub, shell)| stub == id && mapped(shell))
    };
    let mut script = false;
    for found in &res.files {
        if ran_file(found.id) {
            return Look::Ran;
        }
        if let Some(interpreters) = &found.script {
            if interpreters.iter().any(|id| ran_file(*id)) {
                return Look::Ran;
            }
            script = true;
        }
    }
    if script {
        Look::Script
    } else if res.dir {
        Look::Cd
    } else {
        Look::Nothing
    }
}

/// One lookup a session remembers: what the word was typed as and judged from (how it was
/// typed, the directory, the steering `PATH`s, the paths the window's execs were handed as the
/// word's file, and the block), never anything an exec was handed as an argument.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Key {
    path: bool,
    word: Box<str>,
    cwd: Option<Box<str>>,
    paths: Box<[Arc<str>]>,
    handed: Box<[Box<str>]>,
    /// The block's id: a new block looks again.
    block: u64,
}

/// One remembered lookup ([`Memo`]).
#[derive(Debug)]
struct Entry {
    key: Key,
    /// The newest answer, with the exec files the window held when it was asked for: it holds
    /// for a window whose exec files are among them (a new image may be a file rebuilt since).
    done: Option<(Resolution, Box<[FileId]>)>,
    /// The request the worker has not answered: the exec files it was asked for, and when.
    asked: Option<(Box<[FileId]>, Instant)>,
}

/// One session's remembered lookups, newest last, at most [`LOOKS_MAX`], and the signal the
/// worker gives when it answers one.
#[derive(Debug, Default)]
struct Memo {
    entries: Mutex<VecDeque<Entry>>,
    answered: Condvar,
}

impl Memo {
    fn lock(&self) -> MutexGuard<'_, VecDeque<Entry>> {
        self.entries.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The worker's answer `res` to `key`, asked for a window holding `seen`.
    fn answer(&self, key: &Key, res: Resolution, seen: Box<[FileId]>) {
        let mut entries = self.lock();
        if let Some(entry) = entries.iter_mut().find(|e| e.key == *key) {
            if entry
                .done
                .as_ref()
                .is_none_or(|(_, fresh)| covers(&seen, fresh))
            {
                entry.done = Some((res, seen.clone()));
            }
            if entry
                .asked
                .as_ref()
                .is_some_and(|(asked, _)| *asked == seen)
            {
                entry.asked = None;
            }
        }
        drop(entries);
        self.answered.notify_all();
    }
}

/// Whether every file of `seen` is in `fresh` (both sorted).
fn covers(fresh: &[FileId], seen: &[FileId]) -> bool {
    seen.iter().all(|id| fresh.binary_search(id).is_ok())
}

/// How a session's lookups reach the file system.
#[derive(Debug)]
enum Resolving {
    /// On the reading thread, for a test's inert table only.
    #[cfg(test)]
    Inline,
    /// No worker could start. File-based confirmation stays pending without reader-thread I/O.
    Unavailable,
    /// On the lookup worker, each reader waiting at most `wait` for an answer.
    Worker { worker: Arc<Worker>, wait: Duration },
}

impl Resolving {
    /// The production choice when the shared lookup worker could or could not start.
    fn for_worker(worker: Option<Arc<Worker>>, wait: Duration) -> Self {
        worker.map_or(Self::Unavailable, |worker| Self::Worker { worker, wait })
    }
}

/// One session's lookups: what each program word it was asked about names, remembered, made by
/// [`Resolving`].
#[derive(Debug)]
struct Lookups {
    memo: Arc<Memo>,
    how: Resolving,
}

/// One lookup for the worker.
struct Job {
    memo: Arc<Memo>,
    files: Arc<dyn Files>,
    home: Option<Arc<Path>>,
    key: Key,
    seen: Box<[FileId]>,
}

/// The thread that makes every session's lookups ([`resolve`]): the file system is never touched
/// on the thread that reads a detail. A lookup the file system stalls holds up the ones behind
/// it, which then confirm nothing until it returns (they fail closed).
#[derive(Debug)]
pub(crate) struct Worker {
    jobs: Mutex<std::sync::mpsc::Sender<Job>>,
}

impl Worker {
    /// A worker on a thread of its own; `None` when no thread starts.
    pub(crate) fn start() -> Option<Arc<Self>> {
        let (jobs, inbox) = std::sync::mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("aterm-ran-lookup".into())
            .spawn(move || {
                while let Ok(job) = inbox.recv() {
                    let res = resolve(&*job.files, job.home.as_deref(), &job.key);
                    job.memo.answer(&job.key, res, job.seen);
                }
            })
            .ok()?;
        Some(Arc::new(Self {
            jobs: Mutex::new(jobs),
        }))
    }

    /// The process's one worker, started on first use.
    #[cfg(not(test))]
    fn shared() -> Option<Arc<Self>> {
        static SHARED: std::sync::OnceLock<Option<Arc<Worker>>> = std::sync::OnceLock::new();
        SHARED.get_or_init(Self::start).clone()
    }

    /// Hand `job` to the thread; `false` when it is gone.
    fn post(&self, job: Job) -> bool {
        self.jobs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .send(job)
            .is_ok()
    }
}

impl Lookups {
    /// What `key` names, fresh for a window holding the exec files `seen` (or already found to
    /// name a program the window ran, `ran`): remembered, else looked up (on the worker, waited
    /// on until the first reader's wait for it ends; `None` once it has).
    fn resolution(
        &self,
        ran: &Ran,
        key: Key,
        seen: Box<[FileId]>,
        settled: impl Fn(&Resolution) -> bool,
    ) -> Option<Resolution> {
        let holds = |entry: &Entry| {
            entry
                .done
                .as_ref()
                .filter(|(res, fresh)| covers(fresh, &seen) || settled(res))
                .map(|(res, _)| res.clone())
        };
        let mut entries = self.memo.lock();
        let at = match entries.iter().position(|e| e.key == key) {
            Some(at) => at,
            None => {
                entries.push_back(Entry {
                    key: key.clone(),
                    done: None,
                    asked: None,
                });
                while entries.len() > LOOKS_MAX {
                    entries.pop_front();
                }
                entries.len() - 1
            }
        };
        if let Some(res) = entries.get(at).and_then(holds) {
            return Some(res);
        }
        let (worker, wait) = match &self.how {
            Resolving::Unavailable => return None,
            #[cfg(test)]
            Resolving::Inline => {
                drop(entries);
                let res = resolve(&*ran.files, ran.home.as_deref(), &key);
                self.memo.answer(&key, res.clone(), seen);
                return Some(res);
            }
            Resolving::Worker { worker, wait } => (worker, *wait),
        };
        let entry = entries.get_mut(at)?;
        let deadline = match &entry.asked {
            Some((asked, when)) if covers(asked, &seen) => *when + wait,
            _ => {
                let now = Instant::now();
                entry.asked = Some((seen.clone(), now));
                let job = Job {
                    memo: Arc::clone(&self.memo),
                    files: Arc::clone(&ran.files),
                    home: ran.home.clone(),
                    key: key.clone(),
                    seen: seen.clone(),
                };
                if !worker.post(job) {
                    return None;
                }
                now + wait
            }
        };
        loop {
            let left = deadline.checked_duration_since(Instant::now())?;
            if left.is_zero() {
                return None;
            }
            entries = self
                .memo
                .answered
                .wait_timeout(entries, left)
                .unwrap_or_else(|p| p.into_inner())
                .0;
            if let Some(res) = entries.iter().find(|e| e.key == key).and_then(holds) {
                return Some(res);
            }
        }
    }
}

/// What `key`'s word names on `files` ([`Resolution`]), `~/` being `home`: the I/O of a lookup,
/// made on the lookup worker.
fn resolve(files: &dyn Files, home: Option<&Path>, key: &Key) -> Resolution {
    let found: Box<[Found]> = files_for(files, home, key)
        .into_iter()
        .map(|(file, id)| {
            let script = files
                .shebang(&file)
                .map(|line| interpreters(files, &line, &key.paths));
            Found { id, script }
        })
        .collect();
    let stub = SH_STUB.and_then(|(stub, shell)| {
        Some((
            files.program(Path::new(stub))?,
            files.program(Path::new(shell))?,
        ))
    });
    let dir = found.is_empty()
        && directory_for(home, key)
            .filter(|dir| dir.as_os_str().len() <= PATH_MAX)
            .is_some_and(|dir| files.directory(&dir));
    Resolution {
        files: found,
        stub,
        dir,
    }
}

/// The prefixes of a typed path the shell replaces with the home directory before it looks.
const HOME_PREFIXES: [&str; 3] = ["~/", "$HOME/", "${HOME}/"];

/// The file a path typed as `key`'s word names: from the block's directory, a leading `~/`,
/// `$HOME/` or `${HOME}/` from `home` (the shell expands each before it looks); `None` for a
/// path the shell rewrites otherwise (`~user/`, another variable), which is never guessed.
fn path_of(home: Option<&Path>, key: &Key) -> Option<PathBuf> {
    let text = key.word.replace("\\ ", " ");
    if let Some(rest) = HOME_PREFIXES.iter().find_map(|p| text.strip_prefix(p)) {
        return home.map(|home| home.join(rest));
    }
    if text.starts_with(['~', '$']) {
        return None;
    }
    if text.starts_with('/') {
        return Some(PathBuf::from(&text));
    }
    key.cwd
        .as_deref()
        .filter(|cwd| cwd.starts_with('/'))
        .map(|cwd| Path::new(cwd).join(&text))
}

/// The directory a shell that changes into one typed as a command would change into for
/// `key`'s word: the path it names, or a bare name in the block's directory.
fn directory_for(home: Option<&Path>, key: &Key) -> Option<PathBuf> {
    if key.path {
        return path_of(home, key);
    }
    key.cwd
        .as_deref()
        .filter(|cwd| cwd.starts_with('/'))
        .map(|cwd| Path::new(cwd).join(&*key.word))
}

/// The files the shell would exec for `key`'s word, each with its identity: a path
/// ([`path_of`]); a bare name, the first program file of that name on each steering `PATH`, and
/// each path an exec in the window was handed as its file whose file name it is.
fn files_for(files: &dyn Files, home: Option<&Path>, key: &Key) -> Vec<(PathBuf, FileId)> {
    if key.path {
        return path_of(home, key)
            .into_iter()
            .filter(|file| file.as_os_str().len() <= PATH_MAX)
            .filter_map(|file| files.program(&file).map(|id| (file, id)))
            .collect();
    }
    let mut found = Vec::new();
    for path in &key.paths {
        found.extend(first_on_path(files, path, &key.word));
    }
    found.extend(key.handed.iter().filter_map(|path| {
        let file = PathBuf::from(&**path);
        if file.as_os_str().len() > PATH_MAX {
            return None;
        }
        files.program(&file).map(|id| (file, id))
    }));
    found.dedup();
    found
}

/// The candidates for `name` on one `PATH`, in its order ([`first_on_path`] takes the first
/// that is a program file). An empty or relative directory ends the search: this reader cannot
/// establish what it names, so no later directory proves which executable the shell chose.
fn on_path<'a>(path: &'a str, name: &'a str) -> impl Iterator<Item = PathBuf> + 'a {
    (!name.is_empty() && !name.contains('/'))
        .then(|| {
            path.split(':')
                .take_while(|dir| dir.starts_with('/'))
                .map(move |dir| Path::new(dir).join(name))
        })
        .into_iter()
        .flatten()
}

/// The first program file named `name` on `path`, as `execvp` finds it.
fn first_on_path(files: &dyn Files, path: &str, name: &str) -> Option<(PathBuf, FileId)> {
    on_path(path, name)
        .filter(|file| file.as_os_str().len() <= PATH_MAX)
        .find_map(|file| files.program(&file).map(|id| (file, id)))
}

/// The files the interpreter of a `#!` line (what follows `#!`) is: its own file; for
/// the known `/usr/bin/env` file (also reached through a link) with `[-S] [-flags] [NAME=V…]
/// prog`, also the file `prog` names (an absolute one, or the first on each steering `PATH`, as
/// `env` finds it). A custom interpreter named `env` establishes no such semantics.
fn interpreters(files: &dyn Files, line: &str, paths: &[Arc<str>]) -> Box<[FileId]> {
    let mut words = line.split([' ', '\t', '\r']).filter(|w| !w.is_empty());
    let Some(interpreter) = words.next().filter(|i| i.starts_with('/')) else {
        return Box::default();
    };
    let interpreter_id = files.program(Path::new(interpreter));
    let mut ids: Vec<FileId> = interpreter_id.into_iter().collect();
    let env = interpreter_id.is_some_and(|id| files.program(Path::new("/usr/bin/env")) == Some(id));
    if let Some(program) = env_program(words).filter(|_| env) {
        if program.starts_with('/') {
            ids.extend(files.program(Path::new(program)));
        } else if !program.contains('/') {
            ids.extend(
                paths
                    .iter()
                    .filter_map(|p| first_on_path(files, p, program).map(|(_, id)| id)),
            );
        }
    }
    ids.into()
}

/// What one block's detail is confirmed against ([`Ran::heard`]): the execs that count for it
/// ([`Exec::counts`]), those of its own job, which may stand in, the directory it ran in, and the
/// `PATH`s a bare word's lookup is steered by (each exec's own environment where the kernel
/// showed it, newest first; the newest any exec showed; the shell's as spawned).
pub(crate) struct Heard<'a> {
    /// The execs that count for the block, oldest first.
    pub(crate) execs: Vec<Arc<Exec>>,
    /// Those of them its own job ran ([`During::Job`], or pwsh's own group's placed in it),
    /// oldest first.
    pub(crate) stand_ins: Vec<Arc<Exec>>,
    cwd: Option<Box<str>>,
    paths: Vec<Arc<str>>,
    block: u64,
    ran: &'a Ran,
}

impl<'a> Heard<'a> {
    /// The block a test's [`Heard::of`] reads for.
    #[cfg(test)]
    pub(crate) const BLOCK: u64 = 1;

    /// A test's: `execs` against `ran`'s files, run in `cwd`, the shell spawned with `path`, the
    /// block [`Heard::BLOCK`] of a session whose shell runs jobs, every ms in its window. An exec
    /// placed by its stamp alone ([`During::Window`], as [`test_exec`]'s are) is taken for one
    /// of the block's own job; one placed elsewhere counts as [`Exec::counts`] says.
    #[cfg(test)]
    pub(crate) fn of(
        ran: &'a Ran,
        execs: Vec<Exec>,
        cwd: Option<&str>,
        path: Option<&str>,
    ) -> Self {
        Self::of_shell(ran, execs, cwd, path, true)
    }

    /// [`Self::of`] in a session whose shell runs jobs (`jobs`) or runs every command in its own
    /// group (pwsh, `jobs` false).
    #[cfg(test)]
    pub(crate) fn of_shell(
        ran: &'a Ran,
        execs: Vec<Exec>,
        cwd: Option<&str>,
        path: Option<&str>,
        jobs: bool,
    ) -> Self {
        let every = Some(Window { from: 0, to: None });
        let mut own = Vec::new();
        let mut stand_ins = Vec::new();
        for exec in execs {
            let exec = Arc::new(exec.bounded());
            let during = match exec.during {
                During::Window => During::Job(Some(Self::BLOCK)),
                placed => placed,
            };
            let placed = Exec {
                during,
                ..(*exec).clone()
            };
            let (counts, stands) = placed.counts(Self::BLOCK, every, jobs);
            if counts {
                own.push(Arc::clone(&exec));
            }
            if stands {
                stand_ins.push(exec);
            }
        }
        Self {
            execs: own,
            stand_ins,
            cwd: cwd.map(Into::into),
            paths: path.into_iter().map(Into::into).collect(),
            block: Self::BLOCK,
            ran,
        }
    }

    /// The program that may stand in for a program word whose own file did not run: the first
    /// image of the block's own job that is no wrapper ([`During::Job`]; `sudo`, `env`, `nice`
    /// and the like run the program after them), by its canonical file name, a name no process
    /// sets. A prompt hook's job, the next or the last command's, and the shell's own group
    /// (where the shell runs jobs) are never the block's job, so they never stand in.
    pub(crate) fn stand_in(&self, wrapper: impl Fn(&str) -> bool) -> Option<&str> {
        self.stand_ins
            .iter()
            .filter_map(|e| e.names.shown())
            .find(|name| !wrapper(name))
    }

    /// What the file the shell would exec for `word` is ([`Look`]): looked up (never on this
    /// thread, unless a test's table), remembered for the block, and judged against the window's
    /// execs here.
    pub(crate) fn look(&self, word: Word<'_>) -> Look {
        if self.execs.is_empty() {
            return Look::Nothing;
        }
        let (path, text) = match word {
            Word::Path(text) => (true, text),
            Word::Bare(name) => (false, name),
        };
        if text.is_empty() || text.len() > PATH_MAX {
            return Look::Nothing;
        }
        let mut handed: Vec<Box<str>> = Vec::new();
        if !path {
            for h in self.execs.iter().flat_map(|e| e.names.handed.iter()) {
                if Path::new(&**h).file_name().is_some_and(|n| n == text) && !handed.contains(h) {
                    handed.push(h.clone());
                }
            }
        }
        let key = Key {
            path,
            word: text.into(),
            cwd: self.cwd.clone(),
            paths: self.paths.clone().into(),
            handed: handed.into(),
            block: self.block,
        };
        let mut seen: Vec<FileId> = self.execs.iter().filter_map(|e| e.names.file).collect();
        seen.sort_unstable();
        seen.dedup();
        self.ran
            .lookups
            .resolution(self.ran, key, seen.into(), |res| {
                judge(res, &self.execs) == Look::Ran
            })
            .map_or(Look::Pending, |res| judge(&res, &self.execs))
    }
}

impl Exec {
    /// This exec with its names as a ring entry keeps them ([`ExecNames::bounded`]).
    #[cfg(test)]
    fn bounded(self) -> Self {
        Self {
            names: self.names.bounded(),
            ..self
        }
    }
}

/// The program an `env` `#!` line runs: its first word that is no flag (`-S`, `-i`; `-u NAME` and
/// `-P DIR` with their values) and no `NAME=VALUE`.
fn env_program<'a>(mut words: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    while let Some(word) = words.next() {
        match word {
            "-u" | "-P" | "-C" | "--unset" | "--chdir" => {
                words.next();
            }
            _ if word.starts_with('-') || word.contains('=') => {}
            _ => return Some(word),
        }
    }
    None
}

/// A test's window of execs: each of `names` mapped by one job leader, in order, its file
/// [`test_id`]`(name)`.
#[cfg(test)]
pub(crate) fn ran_of(names: &[&str]) -> Vec<Exec> {
    names.iter().map(|name| test_exec(0, name)).collect()
}

/// A test's exec of `name` by one job leader, stamped `at_ms`, its file [`test_id`]`(name)`.
#[cfg(test)]
pub(crate) fn test_exec(at_ms: u64, name: &str) -> Exec {
    Exec {
        at_ms,
        pid: 2,
        pgid: 2,
        names: ExecNames {
            image: Some(name.into()),
            file: Some(test_id(name)),
            ..ExecNames::default()
        },
        during: During::Window,
    }
}

#[cfg(test)]
impl Ran {
    /// A test's ran log in which each of `names` ran in every running block's window (heard at
    /// the end of time, by one job leader, placed by its stamp).
    pub(crate) fn running(names: &[&str]) -> Self {
        let ran = Self::default();
        for name in names {
            ran.hear_running(name);
        }
        ran
    }

    /// Note that `name` ran in every running block's window (heard at the end of time).
    pub(crate) fn hear_running(&self, name: &str) {
        self.note(test_exec(u64::MAX, name));
    }
}

/// What one PTY gather hears for its session's [`Ran`]: each `exec` its process watch hears
/// ([`crate::exec_edges::ExecEdges`]) and, where no watch hears them, each holder image it reads
/// at a foreground edge, each held until the parse stage has parsed where the shell's output
/// stood when it was heard ([`Ran::hear`]); and the holder a restore finds in front
/// ([`RanFeed::restored`]). The shell's own images are never a command's, so none is heard.
#[cfg(unix)]
#[derive(Clone, Debug)]
pub(crate) struct RanFeed {
    ran: Arc<Ran>,
    shell: i32,
    /// Reads an `exec`ed process's names, with its steers when asked ([`read_names`]; a test's
    /// own).
    read: fn(i32, Steering<'_>) -> ExecNames,
}

#[cfg(unix)]
impl RanFeed {
    /// The feed of `ran` from the gather of `shell`'s PTY.
    pub(crate) fn new(ran: Arc<Ran>, shell: i32) -> Self {
        Self {
            ran,
            shell,
            read: read_names,
        }
    }

    /// A test's feed, reading names with `read`.
    #[cfg(test)]
    pub(crate) fn reading(
        ran: Arc<Ran>,
        shell: i32,
        read: fn(i32, Steering<'_>) -> ExecNames,
    ) -> Self {
        Self { ran, shell, read }
    }

    /// A test's feed that reads no process's names (a restore then notes an image by its path).
    #[cfg(test)]
    pub(crate) fn blind(ran: Arc<Ran>, shell: i32) -> Self {
        Self::reading(ran, shell, |_, _| ExecNames::default())
    }

    /// The ran log it feeds.
    pub(crate) const fn ran(&self) -> &Arc<Ran> {
        &self.ran
    }

    /// The shell whose PTY it hears: its group is the shell's own.
    pub(crate) const fn shell(&self) -> i32 {
        self.shell
    }

    /// An `exec` by `pid` in group `pgid` was heard at `spot`: hear its names, read now. Its
    /// steers are read only when it is the first heard of `pid` (the shell's own lookup's; a
    /// later one is the program's re-exec, whose record is not read again), and not even then
    /// when it maps the file the newest read exec did and waits for the same offset: it takes
    /// that exec's ([`Ran::steers_for`]), so a loop running one program reads one record, not
    /// one a run.
    pub(crate) fn exec(&self, pid: i32, pgid: i32, spot: Spot) {
        if pid <= 1 || pid == self.shell {
            return;
        }
        let first = !self.ran.heard_pid(pid);
        let mut shared = None;
        let mut names = (self.read)(pid, &mut |file| {
            shared = first.then(|| self.ran.steers_for(file, spot)).flatten();
            first && shared.is_none()
        });
        match shared {
            Some(s) => {
                names.env_path = s.env_path;
                names.handed = s.handed;
            }
            None if first => self.ran.steered(&names, spot),
            None => {}
        }
        self.ran.hear(
            Exec {
                at_ms: 0,
                pid,
                pgid,
                names,
                during: During::Window,
            },
            spot,
        );
    }

    /// A holder image was read at an edge with `filled` bytes gathered: hear its executable's
    /// file name and file, unless it is the shell's, placed by its stamp (a sampled edge knows
    /// no place in the output). Its leader's exec name is not read: off macOS it is the
    /// process's own to set.
    pub(crate) fn image(&self, image: &crate::holder_image::HolderImage, filled: usize) {
        if image.pgid <= 1 || image.pgid == self.shell {
            return;
        }
        let names = ExecNames::of_path(&image.exe);
        if names.image.is_none() {
            return;
        }
        let read = (self.read)(image.pgid, &mut |_| true);
        let file = read.file.filter(|_| read.image == names.image);
        self.ran.hear(
            Exec {
                at_ms: 0,
                pid: image.pgid,
                pgid: image.pgid,
                names: ExecNames {
                    file,
                    env_path: read.env_path,
                    ..names
                },
                during: During::Window,
            },
            Spot {
                filled,
                queued: None,
                job: true,
            },
        );
    }

    /// A restore found `image` in front, the job of the restored block `running` (`None`: no
    /// block runs): note it now as that job's exec, with what an exec heard live records (its
    /// file's identity and the steers its `KERN_PROCARGS2` record shows: its `PATH`, the path it
    /// was given as its file), read again for the leader when it still maps that image; else
    /// with its file's identity alone. So the restored command's typed word is confirmed by its
    /// file's identity, as it was before the update (a `claude` through its versions link). The
    /// shell's own image is no command's.
    pub(crate) fn restored(&self, image: &crate::holder_image::HolderImage, running: Option<u64>) {
        if image.pgid <= 1 || image.pgid == self.shell {
            return;
        }
        let shown = ExecNames::of_path(&image.exe);
        if shown.image.is_none() {
            return;
        }
        let read = (self.read)(image.pgid, &mut |_| true);
        let names = if read.image == shown.image && read.file.is_some() {
            read
        } else {
            ExecNames {
                file: file_id(&image.exe),
                ..shown
            }
        };
        self.ran.note(Exec {
            at_ms: wall_ms(),
            pid: image.pgid,
            pgid: image.pgid,
            names,
            during: During::Job(running),
        });
    }
}

/// The wall clock an exec is stamped with: the one the blocks' stamps read.
#[cfg(unix)]
pub(crate) fn wall_ms() -> u64 {
    aterm_types::current_time_ms().unwrap_or(0)
}

#[cfg(test)]
#[path = "command_ran_tests.rs"]
mod tests;
