// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whether setup's administrator step owes an account's home folder its Spotlight scope, read
//! with no prompt: the one reading `aterm init` (`aterm_machines::root_steps::owed`) and System
//! State's window share, and the receipt reading the root step itself goes by
//! (`aterm_machines::init_spotlight::Scope`).
//!
//! The decision is here, over a port ([`Reads`]): the window does not link `aterm-machines`
//! outside its tests, so a reading kept there would be one the window had to write again.
//! `aterm-machines` answers the port from its `System`, the window from this Mac ([`Live`]).

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

/// The receipt the root step's apply leaves: every folder it has excluded, one path a line, in
/// a root-owned file every account reads. Spotlight's configuration itself is root's alone, so
/// this is what a reading goes by ([`state`]). A folder stays on it after a person takes its
/// entry out of the configuration: that is how the step knows the removal was theirs and never
/// adds the folder again.
pub const RECEIPT: &str = "/Library/Application Support/aterm/spotlight-scope";

/// The folders a failed apply could not exclude, in a root-owned file beside the receipt.
/// While it names every folder an account's reading finds indexed, that account is not owed
/// the step ([`owed`]): a Mac whose configuration cannot be edited is not asked for its
/// password again and again. A new folder in the home is owed again, any root transaction
/// that runs for another step tries once more, and an apply that succeeds removes the file.
pub const REFUSED: &str = "/Library/Application Support/aterm/spotlight-scope.refused";

/// The most a receipt is read for, in bytes.
pub const RECEIPT_MAX: usize = 256 * 1024;

/// The home folder's top-level folders Spotlight keeps indexing. Documents and Desktop are
/// where a person's own files are searched for. Library stays because Mail, Notes and each
/// app's own search read Spotlight's index of it, and it holds no work files. Applications
/// stays so an app installed for one account is still found by name.
pub const INDEXED: [&str; 4] = ["Documents", "Desktop", "Library", "Applications"];

/// Apple's indexing manager: `mdutil -s <volume>` prints the volume's indexing state
/// ([`Reads::volume_status`], [`indexing`]).
pub const MDUTIL: &str = "/usr/bin/mdutil";

/// The writable macOS data volume, which holds every home folder.
pub const DATA_VOLUME: &str = "/System/Volumes/Data";

/// The console device, which the account logged in at this Mac's screen owns.
pub const CONSOLE: &str = "/dev/console";

/// What a reading needs of one path, as `lstat` answers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    /// The owner's uid.
    pub uid: u32,
    /// A folder.
    pub dir: bool,
    /// A symbolic link (never followed).
    pub symlink: bool,
}

/// What the reading reads of a machine. No method may put a question on the screen: only a
/// home's own listing and each entry's metadata are read, never what a folder holds.
pub trait Reads {
    /// Whether this is macOS, the only system with Spotlight.
    fn macos(&self) -> bool;

    /// This process's effective uid.
    fn euid(&self) -> u32;

    /// The home folder of the account this process runs as.
    fn home(&self) -> Option<PathBuf>;

    /// `path`'s own metadata, a final link not followed.
    ///
    /// # Errors
    /// `lstat`'s: [`io::ErrorKind::NotFound`] where nothing is there.
    fn entry(&self, path: &Path) -> io::Result<Entry>;

    /// The names in the folder `dir`.
    ///
    /// # Errors
    /// The folder could not be listed.
    fn names(&self, dir: &Path) -> io::Result<Vec<String>>;

    /// The bytes of root's own file at `path`, at most `max`: a regular file, a final link not
    /// followed, owned by uid 0, that neither its group nor others may write and that carries
    /// no set-id bit.
    ///
    /// # Errors
    /// The file could not be opened or read, is not such a file, or is longer than `max`.
    fn root_file(&self, path: &Path, max: usize) -> io::Result<Vec<u8>>;

    /// What `mdutil -s` printed for the data volume ([`MDUTIL`], [`DATA_VOLUME`]), from a run
    /// that exited 0.
    ///
    /// # Errors
    /// One line: the run failed, timed out or exited non-zero.
    fn volume_status(&self) -> Result<Vec<u8>, String>;
}

/// Read the data volume's indexing state. Unknown or failed output never means disabled.
///
/// # Errors
/// The command failed or did not report exactly one recognized state.
pub fn indexing(reads: &dyn Reads) -> Result<bool, String> {
    let printed = reads.volume_status()?;
    let text = std::str::from_utf8(&printed).map_err(|error| error.to_string())?;
    match (
        text.lines().any(|line| line.trim() == "Indexing enabled."),
        text.lines().any(|line| line.trim() == "Indexing disabled."),
    ) {
        (true, false) => Ok(true),
        (false, true) => Ok(false),
        _ => Err("Spotlight indexing status is unrecognized".into()),
    }
}

/// The folders of `home` Spotlight should leave out: each top-level folder that is not hidden
/// (Spotlight never indexes a dot-folder), not named `*.noindex` (nor those), no link, and not
/// one of [`INDEXED`]. Only the home's own listing and each entry's metadata are read, never
/// what a folder holds: a read inside Documents, Desktop or Downloads can raise a macOS privacy
/// question. An entry whose metadata cannot be read is left out, as is a name a receipt line
/// cannot carry.
///
/// # Errors
/// The home folder could not be listed.
pub fn wanted(reads: &dyn Reads, home: &Path) -> Result<BTreeSet<PathBuf>, String> {
    let names = reads
        .names(home)
        .map_err(|error| format!("{}: {error}", home.display()))?;
    Ok(names
        .into_iter()
        .filter(|name| {
            !name.starts_with('.')
                && !name.ends_with(".noindex")
                && !name.chars().any(char::is_control)
                && !INDEXED.contains(&name.as_str())
        })
        .map(|name| home.join(name))
        .filter(|path| {
            reads
                .entry(path)
                .is_ok_and(|entry| entry.dir && !entry.symlink)
        })
        .collect())
}

/// The paths root's file at `file` lists ([`RECEIPT`], [`REFUSED`]): none while there is no
/// such file.
///
/// # Errors
/// A file stands there that is not root's own regular file, or it could not be read.
pub fn listed(reads: &dyn Reads, file: &str) -> Result<BTreeSet<PathBuf>, String> {
    let path = Path::new(file);
    match reads.entry(path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => return Err(format!("{file}: {error}")),
    }
    let bytes = reads
        .root_file(path, RECEIPT_MAX)
        .map_err(|error| format!("{file}: {error}"))?;
    let text = String::from_utf8(bytes).map_err(|error| format!("{file}: {error}"))?;
    Ok(text
        .lines()
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect())
}

/// How Spotlight stands for one home folder ([`state`]).
#[derive(Debug, PartialEq, Eq)]
pub enum State {
    /// The data volume is not indexed at all, which the step never turns back on.
    Disabled,
    /// The receipt names every folder [`wanted`] names: how many.
    Scoped(usize),
    /// No receipt names these folders, so the step has never excluded them.
    Indexed(BTreeSet<PathBuf>),
}

/// Read how Spotlight stands for `home`, by the receipt alone, as root and as an ordinary
/// account: a folder the receipt names is done with, whether the configuration still excludes
/// it or a person took its entry out since. Root reads a receipt that is not root's own file
/// as none, which the step's apply then replaces.
///
/// # Errors
/// The indexing state, the home folder, or an ordinary account's receipt could not be read.
pub fn state(reads: &dyn Reads, home: &Path) -> Result<State, String> {
    if !indexing(reads)? {
        return Ok(State::Disabled);
    }
    let wanted = wanted(reads, home)?;
    let noted = if reads.euid() == 0 {
        listed(reads, RECEIPT).unwrap_or_default()
    } else {
        listed(reads, RECEIPT)?
    };
    let indexed: BTreeSet<PathBuf> = wanted.difference(&noted).cloned().collect();
    Ok(if indexed.is_empty() {
        State::Scoped(wanted.len())
    } else {
        State::Indexed(indexed)
    })
}

/// Whether setup's administrator step owes this account's home folder its Spotlight scope, as
/// the account reads it with no prompt. Only the account at the console is owed it, since that
/// is the home the root step scopes. A state that cannot be read is not owed: no password is
/// asked for want of a reading. Nor are folders an apply already failed to exclude
/// ([`REFUSED`]).
#[must_use]
pub fn owed(reads: &dyn Reads) -> bool {
    if !reads.macos() || reads.euid() == 0 {
        return false;
    }
    if !reads
        .entry(Path::new(CONSOLE))
        .is_ok_and(|console| console.uid == reads.euid())
    {
        return false;
    }
    let Some(home) = reads.home() else {
        return false;
    };
    match state(reads, &home) {
        Ok(State::Indexed(folders)) => {
            !listed(reads, REFUSED).is_ok_and(|refused| folders.is_subset(&refused))
        }
        _ => false,
    }
}

/// How long [`Live`] waits for `mdutil -s` before the state counts as unread.
#[cfg(unix)]
const STATUS_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// This machine, read by this process: `lstat`, a folder listing, a bounded read of an opened
/// file whose own metadata is held to [`Reads::root_file`]'s rule, and one `mdutil -s`.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Live;

#[cfg(unix)]
impl Reads for Live {
    fn macos(&self) -> bool {
        cfg!(target_os = "macos")
    }

    fn euid(&self) -> u32 {
        aterm_uds::peer::our_uid()
    }

    fn home(&self) -> Option<PathBuf> {
        aterm_uds::account::home_of(self.euid())
    }

    fn entry(&self, path: &Path) -> io::Result<Entry> {
        use std::os::unix::fs::MetadataExt as _;
        let meta = std::fs::symlink_metadata(path)?;
        Ok(Entry {
            uid: meta.uid(),
            dir: meta.is_dir(),
            symlink: meta.file_type().is_symlink(),
        })
    }

    fn names(&self, dir: &Path) -> io::Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            names.push(entry?.file_name().to_string_lossy().into_owned());
        }
        Ok(names)
    }

    fn root_file(&self, path: &Path, max: usize) -> io::Result<Vec<u8>> {
        read_owned(path, 0, max)
    }

    fn volume_status(&self) -> Result<Vec<u8>, String> {
        let mut status = aterm_uds::spawnfd::command(MDUTIL);
        status.args(["-s", DATA_VOLUME]);
        let out = aterm_uds::deadline::output_within(&mut status, STATUS_WAIT)
            .map_err(|error| format!("{MDUTIL} -s {DATA_VOLUME}: {error}"))?;
        if out.status.success() {
            Ok(out.stdout)
        } else {
            Err(format!("Spotlight status: {}", out.status))
        }
    }
}

/// The bytes, at most `max`, of the regular file at `path` that `owner` owns, a final link not
/// followed, judged by the opened file's own metadata ([`Reads::root_file`]'s rule for
/// `owner` 0).
#[cfg(unix)]
fn read_owned(path: &Path, owner: u32, max: usize) -> io::Result<Vec<u8>> {
    use std::io::Read as _;
    use std::os::unix::fs::MetadataExt as _;
    let bound = max.checked_add(1).ok_or(io::ErrorKind::InvalidInput)?;
    // Nonblocking and refusing a final link: a FIFO or a link put there waits on nothing.
    let (file, meta) = aterm_uds::regular_file::open(path)?;
    if !meta.is_file() || meta.uid() != owner || meta.mode() & 0o6022 != 0 {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    if meta.len() > max as u64 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut bytes = Vec::new();
    file.take(bound as u64).read_to_end(&mut bytes)?;
    if bytes.len() > max {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(bytes)
}

/// [`owed`] for this account on this Mac ([`Live`]): `false` off macOS.
#[must_use]
pub fn owed_here() -> bool {
    #[cfg(target_os = "macos")]
    {
        owed(&Live)
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

#[cfg(test)]
#[path = "spotlight_scope_tests.rs"]
mod tests;
