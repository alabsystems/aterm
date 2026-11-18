//! Physical inputs of a command search, NOT a second `which` implementation.
//! The ordinary selector still decides existence/access/order. This owner
//! conservatively holds EVERY candidate, negative prefix and literal symlink
//! reached from the unchanged PATH/cwd before that selector runs. It cannot
//! select a program, authorize argv or issue source, Seed, SDK or GO authority.

use super::*;
use std::collections::VecDeque;
use std::sync::Mutex;

const MAX_ROOTS: usize = 128;
const MAX_NODES: usize = 4096;
const MAX_STEPS: usize = 16384;
const MAX_LINKS: usize = 40;

#[derive(PartialEq, Eq)]
pub(super) struct Credentials {
    ids: (u32, u32, u32, u32),
    groups: Vec<libc::gid_t>,
}
pub(super) fn credentials(end: Instant) -> io::Result<Credentials> {
    time(end)?;
    let ids = || unsafe {
        (
            libc::getuid(),
            libc::geteuid(),
            libc::getgid(),
            libc::getegid(),
        )
    };
    let before = ids();
    let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    if count < 0 {
        return Err(io::Error::last_os_error());
    }
    if count > 1024 {
        return Err(refused("command search credentials exceed group bound"));
    }
    let mut groups = vec![0; count as usize];
    let actual = unsafe { libc::getgroups(count, groups.as_mut_ptr()) };
    if actual < 0 {
        return Err(io::Error::last_os_error());
    }
    if actual != count || before != ids() {
        return Err(refused("command search credentials changed during capture"));
    }
    groups.sort_unstable();
    time(end)?;
    Ok(Credentials {
        ids: before,
        groups,
    })
}

#[derive(Clone, PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    links: u64,
    bytes: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
impl Stamp {
    fn metadata(m: &Metadata) -> Self {
        Self {
            dev: m.dev(),
            ino: m.ino(),
            mode: m.mode(),
            uid: m.uid(),
            gid: m.gid(),
            links: m.nlink(),
            bytes: m.len(),
            modified: (m.mtime(), m.mtime_nsec()),
            changed: (m.ctime(), m.ctime_nsec()),
        }
    }
    #[allow(
        clippy::unnecessary_cast,
        reason = "native stat ABI widths differ across supported Unix hosts"
    )]
    fn native(m: &libc::stat) -> Self {
        Self {
            dev: m.st_dev as u64,
            ino: m.st_ino as u64,
            mode: m.st_mode as u32,
            uid: m.st_uid,
            gid: m.st_gid,
            links: m.st_nlink as u64,
            bytes: m.st_size as u64,
            modified: (m.st_mtime as i64, m.st_mtime_nsec as i64),
            changed: (m.st_ctime as i64, m.st_ctime_nsec as i64),
        }
    }
    fn directory(&self, other: &Self) -> bool {
        // Search tests read identity/kind/permissions, not directory listings.
        // Unrelated sibling writes do not alter these selected facts. As with
        // HeldProgram, this is no atomic namespace or same-UID writer exclusion.
        (self.dev, self.ino, self.mode, self.uid, self.gid)
            == (other.dev, other.ino, other.mode, other.uid, other.gid)
    }
}
struct Directory {
    file: File,
    stamp: Stamp,
    acl: Vec<u8>,
}
enum Entry {
    Absent,
    Link {
        stamp: Stamp,
        target: Vec<u8>,
    },
    File {
        original: File,
        stamp: Stamp,
        acl: Vec<u8>,
    },
}

/// Read-only physical custody only. Its source/operation issuer must supply
/// the original environment/cwd and keep it beside the actual selector and
/// native program owner. Freely fabricated PATH bytes cannot create that role.
pub struct HeldCommandSearchInputs {
    inner: Mutex<Search>,
    creator: u32,
}
struct Search {
    candidates: Vec<PathBuf>,
    retained_paths: Vec<PathBuf>,
    cwd: PathBuf,
    directories: BTreeMap<PathBuf, Directory>,
    entries: BTreeMap<PathBuf, Entry>,
    creator: u32,
    credentials: Credentials,
    end: Instant,
    revoked: bool,
}

fn status(parent: &File, name: &OsStr) -> io::Result<Option<Stamp>> {
    let name = CString::new(name.as_bytes()).map_err(|_| refused("search name contains NUL"))?;
    let mut raw = std::mem::MaybeUninit::uninit();
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            raw.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(error)
        };
    }
    Ok(Some(Stamp::native(&unsafe { raw.assume_init() })))
}

fn link(parent: &File, name: &OsStr) -> io::Result<Vec<u8>> {
    let name = CString::new(name.as_bytes()).map_err(|_| refused("search link contains NUL"))?;
    let mut raw = vec![0; MAX_PATH + 1];
    let len = unsafe {
        libc::readlinkat(
            parent.as_raw_fd(),
            name.as_ptr(),
            raw.as_mut_ptr().cast(),
            raw.len(),
        )
    };
    if len < 0 {
        return Err(io::Error::last_os_error());
    }
    if len == 0 || len as usize > MAX_PATH {
        return Err(refused("search symlink target exceeds bound"));
    }
    raw.truncate(len as usize);
    if raw.contains(&0) {
        return Err(refused("search symlink target contains NUL"));
    }
    Ok(raw)
}

impl HeldCommandSearchInputs {
    /// Capture is bounded and transactional: a failed open/dup, including real
    /// EMFILE, closes ONLY newly opened descriptions. No kernel FD reservation
    /// is inferred from this conservative admission/count demand.
    pub fn capture_descriptor_bound() -> usize {
        MAX_NODES + 2
    }

    /// A missing PATH needs the ordinary platform's separate default-search
    /// input contract; this API accepts an explicitly present original value.
    /// Empty PATH stays empty (the canonical selector decides absence).
    pub fn capture(cwd: &Path, path: &OsStr, name: &OsStr, end: Instant) -> io::Result<Self> {
        canonical(cwd)?;
        let raw = path.as_bytes();
        if raw.len() > 64 * 1024
            || raw.contains(&0)
            || name.is_empty()
            || name.as_bytes().len() > 255
            || name.as_bytes().contains(&0)
            || name.as_bytes().contains(&b'/')
            || matches!(name.to_str(), Some("." | ".."))
        {
            return Err(refused(
                "command search inputs exceed their fixed syntax bounds",
            ));
        }
        let mut candidates = Vec::new();
        if !raw.is_empty() {
            for root in raw.split(|b| *b == b':') {
                if candidates.len() == MAX_ROOTS {
                    return Err(refused("command search root population exceeds bound"));
                }
                let candidate = Path::new(OsStr::from_bytes(root)).join(name);
                if candidate.as_os_str().as_bytes().len() > MAX_PATH {
                    return Err(refused("command search candidate exceeds path bound"));
                }
                candidates.push(candidate);
            }
        }
        let cwd_original = directory(cwd, end)?;
        let cwd_stamp = Stamp::metadata(&owned(&cwd_original, true, false)?);
        let root = directory(Path::new("/"), end)?;
        let stamp = Stamp::metadata(&owned(&root, true, false)?);
        let acl = crate::private::acl_snapshot(&root)?;
        let mut search = Search {
            candidates,
            retained_paths: Vec::new(),
            cwd: cwd.into(),
            directories: BTreeMap::from([(
                PathBuf::from("/"),
                Directory {
                    file: root,
                    stamp,
                    acl,
                },
            )]),
            entries: BTreeMap::new(),
            creator: std::process::id(),
            credentials: credentials(end)?,
            end,
            revoked: false,
        };
        // Retain the actual cwd even when every PATH root is absolute.
        search.walk(cwd, true)?;
        if search
            .directories
            .get(cwd)
            .is_none_or(|dir| !cwd_stamp.directory(&dir.stamp))
        {
            return Err(refused("original command search cwd changed"));
        }
        for candidate in search.candidates.clone() {
            search.walk(&candidate, true)?;
        }
        let result = Self {
            inner: Mutex::new(search),
            creator: std::process::id(),
        };
        result.check_current()?;
        Ok(result)
    }

    /// Fixed original end and creator; failed drift/timeout stays sticky even
    /// if its pathname is later restored. No observation can revive this owner.
    pub fn check_current(&self) -> io::Result<()> {
        // A fork must refuse before touching an inherited mutex which another
        // vanished thread could have held. No copied PID/status revives it.
        if self.creator != std::process::id() {
            // Inline errno: the post-fork refusal must not allocate either.
            return Err(io::Error::from_raw_os_error(libc::EPERM));
        }
        let mut original = self
            .inner
            .lock()
            .map_err(|_| refused("command search input check panicked"))?;
        original.check_current()
    }

    /// Exact lexical membership DATA, never `which`, executability or absence
    /// authority. Even an absent candidate can belong to this held population;
    /// only the actual canonical selector may decide its meaning.
    pub fn check_candidate(&self, candidate: &Path) -> io::Result<()> {
        self.check_current()?;
        {
            let mut original = self
                .inner
                .lock()
                .map_err(|_| refused("command search input check panicked"))?;
            if !original
                .candidates
                .iter()
                .any(|path| path.as_os_str().as_bytes() == candidate.as_os_str().as_bytes())
            {
                original.revoked = true;
                return Err(refused(
                    "selected command is outside original search population",
                ));
            }
        }
        self.check_current()
    }

    /// Retain additional physical reads beside the SAME original search. This
    /// never adds a PATH candidate or selects a tool. It shares the existing
    /// total node/descriptor bound rather than charging another full census.
    /// Call before the maintained selection reads these paths. A failed or
    /// partial extension permanently quarantines all retained originals.
    pub fn retain_inputs(&self, paths: &[PathBuf]) -> io::Result<()> {
        if self.creator != std::process::id() {
            return Err(io::Error::from_raw_os_error(libc::EPERM));
        }
        let mut original = self
            .inner
            .lock()
            .map_err(|_| refused("command search input check panicked"))?;
        let result = (|| {
            original.check_current()?;
            if paths.len() > MAX_ROOTS {
                return Err(refused("additional search input population exceeds bound"));
            }
            for path in paths {
                let bytes = path.as_os_str().as_bytes();
                if !path.is_absolute() || bytes.len() > MAX_PATH || bytes.contains(&0) {
                    return Err(refused("additional search input path exceeds bound"));
                }
                if original
                    .candidates
                    .iter()
                    .chain(&original.retained_paths)
                    .any(|old| old.as_os_str().as_bytes() == bytes)
                {
                    continue;
                }
                if original.candidates.len() + original.retained_paths.len() == MAX_ROOTS {
                    return Err(refused("complete search input population exceeds bound"));
                }
                // Install the expected root before fallible traversal. Any
                // refusal retains the actual partial descriptions, quarantined.
                original.retained_paths.push(path.clone());
                original.walk(path, true)?;
            }
            original.check_current()
        })();
        if result.is_err() {
            original.revoked = true;
        }
        result
    }
}

impl Search {
    fn walk(&mut self, path: &Path, capture: bool) -> io::Result<()> {
        time(self.end)?;
        let mut pending = VecDeque::new();
        if !path.is_absolute() {
            pending.extend(self.cwd.components().map(|p| p.as_os_str().to_owned()));
        }
        pending.extend(path.components().map(|p| p.as_os_str().to_owned()));
        let mut at = PathBuf::from("/");
        let mut steps = 0;
        let mut links = 0;
        while let Some(name) = pending.pop_front() {
            time(self.end)?;
            steps += 1;
            if steps > MAX_STEPS {
                return Err(refused("command search traversal exceeds bound"));
            }
            if name == OsStr::new("/") {
                at = PathBuf::from("/");
                continue;
            }
            if name == OsStr::new(".") {
                continue;
            }
            if name == OsStr::new("..") {
                at.pop();
                if at.as_os_str().is_empty() {
                    at.push("/");
                }
                continue;
            }
            let parent = self
                .directories
                .get(&at)
                .ok_or_else(|| refused("original search parent unavailable"))?;
            if !parent
                .stamp
                .directory(&Stamp::metadata(&owned(&parent.file, true, false)?))
                || parent.acl != crate::private::acl_snapshot(&parent.file)?
            {
                return Err(refused("original command search directory changed"));
            }
            let selected = at.join(&name);
            let current = status(&parent.file, &name)?;
            if let Some(dir) = self.directories.get(&selected) {
                if current.as_ref().is_none_or(|now| !dir.stamp.directory(now))
                    || !dir
                        .stamp
                        .directory(&Stamp::metadata(&owned(&dir.file, true, false)?))
                    || dir.acl != crate::private::acl_snapshot(&dir.file)?
                {
                    return Err(refused("command search directory was substituted"));
                }
                at = selected;
                continue;
            }
            if let Some(entry) = self.entries.get(&selected) {
                match entry {
                    Entry::Absent if current.is_none() => return Ok(()),
                    Entry::Link { stamp, target } if current.as_ref() == Some(stamp) => {
                        if link(&parent.file, &name)? != *target {
                            return Err(refused("command search symlink changed"));
                        }
                        links += 1;
                        if links > MAX_LINKS {
                            return Err(refused("command search symlink population exceeds bound"));
                        }
                        let path = Path::new(OsStr::from_bytes(target));
                        let parts = path
                            .components()
                            .map(|p| p.as_os_str().to_owned())
                            .collect::<Vec<_>>();
                        for part in parts.into_iter().rev() {
                            pending.push_front(part);
                        }
                        continue;
                    }
                    Entry::File {
                        original,
                        stamp,
                        acl,
                    } if current.as_ref() == Some(stamp) => {
                        if Stamp::metadata(&owned(original, false, false)?) != *stamp
                            || *acl != crate::private::acl_snapshot(original)?
                        {
                            return Err(refused("original command search file changed"));
                        }
                        // A non-directory blocks any remaining suffix as well.
                        return Ok(());
                    }
                    _ => return Err(refused("original command search entry changed")),
                }
            }
            if !capture {
                return Err(refused("command search reached an uncaptured entry"));
            }
            if self.entries.len() + self.directories.len() >= MAX_NODES {
                return Err(refused("command search input population exceeds bound"));
            }
            let Some(before) = current else {
                self.entries.insert(selected, Entry::Absent);
                return Ok(());
            };
            let kind = before.mode & libc::S_IFMT as u32;
            if kind == libc::S_IFLNK as u32 {
                if before.uid != 0 && before.uid != unsafe { libc::geteuid() } {
                    return Err(refused("command search symlink belongs to another account"));
                }
                let target = link(&parent.file, &name)?;
                if status(&parent.file, &name)?.as_ref() != Some(&before) {
                    return Err(refused("command search symlink changed during capture"));
                }
                self.entries.insert(
                    selected,
                    Entry::Link {
                        stamp: before,
                        target,
                    },
                );
                pending.push_front(name); // Visit the SAME newly captured link.
            } else if kind == libc::S_IFDIR as u32 || kind == libc::S_IFREG as u32 {
                let is_dir = kind == libc::S_IFDIR as u32;
                let original = openat(parent.file.as_raw_fd(), &name, is_dir)?;
                let after = Stamp::metadata(&owned(&original, is_dir, false)?);
                let acl = crate::private::acl_snapshot(&original)?;
                if after != before || status(&parent.file, &name)?.as_ref() != Some(&before) {
                    return Err(refused("command search entry changed during capture"));
                }
                if is_dir {
                    self.directories.insert(
                        selected.clone(),
                        Directory {
                            file: original,
                            stamp: before,
                            acl,
                        },
                    );
                    at = selected;
                } else {
                    self.entries.insert(
                        selected,
                        Entry::File {
                            original,
                            stamp: before,
                            acl,
                        },
                    );
                    return Ok(());
                }
            } else {
                return Err(refused(
                    "command search candidate has an unsupported input kind",
                ));
            }
        }
        Ok(())
    }

    fn check_current(&mut self) -> io::Result<()> {
        if self.revoked {
            return Err(refused("command search inputs are quarantined"));
        }
        let result = (|| {
            if self.creator != std::process::id() {
                return Err(refused("command search inputs belong to another process"));
            }
            time(self.end)?;
            if self.credentials != credentials(self.end)? {
                return Err(refused("original command search credentials changed"));
            }
            let root = self
                .directories
                .get(Path::new("/"))
                .ok_or_else(|| refused("original search root unavailable"))?;
            if !root
                .stamp
                .directory(&Stamp::metadata(&owned(&root.file, true, false)?))
                || root.acl != crate::private::acl_snapshot(&root.file)?
                || !root.stamp.directory(&Stamp::metadata(&owned(
                    &directory(Path::new("/"), self.end)?,
                    true,
                    false,
                )?))
            {
                return Err(refused("original command search root changed"));
            }
            let cwd = self.cwd.clone();
            self.walk(&cwd, false)?;
            for candidate in self
                .candidates
                .iter()
                .chain(&self.retained_paths)
                .cloned()
                .collect::<Vec<_>>()
            {
                self.walk(&candidate, false)?;
            }
            if self.credentials != credentials(self.end)? {
                return Err(refused("original command search credentials changed"));
            }
            time(self.end)
        })();
        if result.is_err() {
            self.revoked = true;
        }
        result
    }
}

#[cfg(test)]
mod tests;
