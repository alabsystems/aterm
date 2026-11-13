// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact installer child custody, never command authority. A receipt names the claim of the
//! account's ask record its tab's program holds ([`aterm_types::clt_mark::Claim`]), so a launch
//! that adopts the live tab records that attempt's end exactly as the window that opened it
//! would; an unclaimed receipt ends no attempt. An orphan's attempt reads cut off in the record,
//! which its program held, so its receipt only goes. An earlier aterm's receipt (`v2`), which
//! names at most the build that claimed, still gives custody of its live tab: the adopting launch
//! carries that tab's attempt into the record ([`super::Mark::carry`]) and rewrites the receipt
//! as this build's, naming the claim it carried ([`Receipt::carry`]).

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

const MAX_BYTES: usize = 256;

/// The receipt's last line for a tab whose own claim added no line to the record.
const UNCLAIMED: &str = "unclaimed";

#[derive(Debug)]
pub(super) struct Receipt {
    directory: PathBuf,
    path: PathBuf,
    text: String,
    /// The claim of the account's record the tab's program holds, if its opening claimed one.
    claimed: Option<aterm_types::clt_mark::Claim>,
    /// Read from an earlier aterm's `v2` receipt, which names no claim of this build's record:
    /// the build its last line says claimed (`claimed <build>`), if it says one.
    earlier: Option<Earlier>,
    sid: String,
    pid: i32,
    birth: u64,
}

/// What an earlier aterm's `v2` receipt said of its tab's claim: its last line, `claimed <build>`
/// (the build whose `opened` line the tab's opening added to the account's record), or
/// `unclaimed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Earlier {
    /// The build its last line names, if it names one.
    pub(super) build: Option<String>,
}

/// Whether a line of the record could name `build` ([`aterm_types::clt_mark::BUILD_BYTES`]).
fn nameable(build: &str) -> bool {
    !build.is_empty()
        && build.len() <= aterm_types::clt_mark::BUILD_BYTES
        && !build.contains(char::is_whitespace)
}

/// A receipt's last line: `claimed <build> <pid> <birth> <at> <n>` for the claim the tab's program
/// holds, else [`UNCLAIMED`]; `None` for a build no line of the record could name.
fn claim_line(claimed: Option<&aterm_types::clt_mark::Claim>) -> Option<String> {
    match claimed {
        None => Some(UNCLAIMED.to_owned()),
        Some(claim) if !nameable(&claim.asker.build) => None,
        Some(claim) => Some(format!(
            "claimed {} {} {} {} {}",
            claim.asker.build, claim.asker.pid, claim.asker.birth, claim.at, claim.n
        )),
    }
}

/// A receipt's whole text: `v3`, the child's identity, and its claim line ([`claim_line`]).
fn receipt_text(sid: &str, pid: i32, birth: u64, claim: &str) -> String {
    format!("v3\n{sid}\n{pid}\n{birth}\n{claim}\n")
}

/// The claim a receipt's `claimed …` line names ([`claim_line`]); `None` for any other text.
fn parse_claim(line: &str) -> Option<aterm_types::clt_mark::Claim> {
    let mut words = line.strip_prefix("claimed ")?.split(' ');
    let build = words.next()?.to_owned();
    let pid = words.next()?.parse().ok()?;
    let birth = words.next()?.parse().ok()?;
    let at = words.next()?.parse().ok()?;
    let n = words.next()?.parse().ok()?;
    let claim = aterm_types::clt_mark::Claim {
        asker: aterm_types::clt_mark::Asker { build, pid, birth },
        at,
        n,
    };
    (words.next().is_none() && claim_line(Some(&claim)).as_deref() == Some(line)).then_some(claim)
}

impl Receipt {
    /// The receipt of the installer child `sid` (`pid`, born `birth`) in a folder beside the
    /// record at `mark`: `v3`, the child's identity, and the claim its program holds
    /// ([`claim_line`]), else `unclaimed`. `None` for an identity no receipt names (a pid of 1 or
    /// less, no birth, a session id that is none) and a build no line of the record could name.
    pub(super) fn for_child(
        mark: &Path,
        sid: &str,
        pid: i32,
        birth: Option<u64>,
        claimed: Option<&aterm_types::clt_mark::Claim>,
    ) -> Option<Self> {
        let birth = birth.filter(|birth| *birth > 0)?;
        if pid <= 1 || !crate::keeper_link::valid_sid(sid) {
            return None;
        }
        let claim = claim_line(claimed)?;
        let directory = folder(mark)?;
        let name = format!("{sid}-{pid}-{birth}");
        let text = receipt_text(sid, pid, birth, &claim);
        (name.len() <= 255 && text.len() <= MAX_BYTES).then(|| Self {
            path: directory.join(name),
            directory,
            text,
            claimed: claimed.cloned(),
            earlier: None,
            sid: sid.to_owned(),
            pid,
            birth,
        })
    }

    /// The receipt an earlier aterm wrote for the live child `sid` (`pid`, born `birth`) beside the
    /// record at `mark`, read back whole ([`Self::for_child`]'s text exactly, whatever build it
    /// says claimed), or an earlier aterm's `v2` one of exactly that child, whose last line is
    /// `claimed <build>` or `unclaimed` ([`Self::earlier`]): `None` where there is none of exactly
    /// that child, its folder is not this account's alone, or it is partial, malformed or no
    /// regular file.
    pub(super) fn adopt(mark: &Path, sid: &str, pid: i32, birth: Option<u64>) -> Option<Self> {
        let unclaimed = Self::for_child(mark, sid, pid, birth, None)?;
        unclaimed.directory_owned().ok()?;
        let text = crate::config_marker::read_marker(&unclaimed.path, MAX_BYTES)?;
        let head = unclaimed.text.strip_suffix(&format!("{UNCLAIMED}\n"))?;
        if let Some(claim) = text
            .strip_prefix(head)
            .and_then(|rest| rest.strip_suffix('\n'))
        {
            let claimed = match claim {
                UNCLAIMED => None,
                claim => Some(parse_claim(claim)?),
            };
            return Self::for_child(mark, sid, pid, birth, claimed.as_ref())
                .filter(|receipt| receipt.text == text);
        }
        let earlier_head = format!("v2\n{sid}\n{pid}\n{}\n", unclaimed.birth);
        let build = match text.strip_prefix(&earlier_head)?.strip_suffix('\n')? {
            UNCLAIMED => None,
            line => Some(line.strip_prefix("claimed ")?.to_owned()),
        };
        if build.as_deref().is_some_and(|build| !nameable(build)) {
            return None;
        }
        Some(Self {
            text,
            earlier: Some(Earlier { build }),
            ..unclaimed
        })
    }

    /// The receipts beside the record at `mark` whose child no longer runs (`alive` says whether
    /// the process `pid` born `birth` runs: [`alive`]), each read back whole as [`Self::adopt`]
    /// reads one. No launch adopts such a tab: the PTY keeper closes a record whose shell died
    /// while no window held it, and a tab only its window held (one that landed before the keeper
    /// answered) went with that window. So no window read how it ended. A name no receipt has,
    /// and a receipt that is partial, malformed, or in a folder that is not this account's alone,
    /// give none.
    #[cfg(any(not(test), unix))]
    pub(super) fn orphans(mark: &Path, alive: impl Fn(i32, u64) -> bool) -> Vec<Self> {
        let Some(entries) = folder(mark).and_then(|directory| std::fs::read_dir(directory).ok())
        else {
            return Vec::new();
        };
        entries
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter_map(|name| {
                // `<sid>-<pid>-<birth>`: a session id holds a dash of its own.
                let mut parts = name.rsplitn(3, '-');
                let birth = parts.next()?.parse().ok()?;
                let pid = parts.next()?.parse().ok()?;
                let sid = parts.next()?;
                if alive(pid, birth) {
                    return None;
                }
                Self::adopt(mark, sid, pid, Some(birth))
            })
            .collect()
    }

    /// The claim of the account's record the tab's program holds, if its opening claimed one.
    pub(super) fn claimed(&self) -> Option<&aterm_types::clt_mark::Claim> {
        self.claimed.as_ref()
    }

    /// What an earlier aterm's `v2` receipt, this one as read, said of its tab's claim; `None`
    /// for one of this build's.
    pub(super) const fn earlier(&self) -> Option<&Earlier> {
        self.earlier.as_ref()
    }

    /// An earlier aterm's receipt ([`Self::earlier`]) becomes this build's, naming `claimed`, the
    /// claim of the account's record its tab's program now holds ([`super::Mark::carry`]), if
    /// any: the receipt on disk, still exactly this one ([`Self::matches`]), is replaced whole by
    /// [`Self::for_child`]'s, and this receipt becomes that one, so [`Self::remove_if_matching`]
    /// still finds it and a later launch adopts the tab with its claim. The new text goes to a
    /// file of this process's own in the receipt's owned folder (created exclusively, never
    /// through a link, `0600`), flushed and renamed over the receipt, and the folder flushed
    /// where it can be. On an error the receipt on disk and this one stay as they were, and no
    /// staged file is left.
    ///
    /// # Errors
    /// The receipt on disk is no longer exactly this one, or its folder is not this account's
    /// alone ([`io::ErrorKind::InvalidData`]); `claimed` names a build no line of the record
    /// could name ([`io::ErrorKind::InvalidInput`]); or the staged file's creation, write, flush
    /// or rename failed.
    pub(super) fn carry(
        &mut self,
        claimed: Option<&aterm_types::clt_mark::Claim>,
    ) -> io::Result<()> {
        let invalid = |why: &str| io::Error::new(io::ErrorKind::InvalidInput, why.to_owned());
        let claim = claim_line(claimed).ok_or_else(|| invalid("no build a line could name"))?;
        let text = receipt_text(&self.sid, self.pid, self.birth, &claim);
        if text.len() > MAX_BYTES {
            return Err(invalid("the receipt is larger than a receipt may hold"));
        }
        if !self.matches() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the installer's recovery receipt is no longer this child's own",
            ));
        }
        let name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| invalid("the receipt has no name"))?;
        // A leading dot: no receipt's name (a session id's `s-` first) is ever this.
        let staged = self
            .directory
            .join(format!(".{name}.carry-{}", std::process::id()));
        // One an earlier process of this pid left as it died mid-carry; a link goes, not where
        // it points.
        let _ = std::fs::remove_file(&staged);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
        }
        let mut file = options.open(&staged)?;
        let written = file.write_all(text.as_bytes()).and_then(|()| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            }
            aterm_durable::ordered_or_fsync(&file)
        });
        drop(file);
        let written = written.and_then(|()| std::fs::rename(&staged, &self.path));
        if written.is_err() {
            let _ = std::fs::remove_file(&staged);
        }
        written?;
        // The receipt is in place; a folder that cannot be flushed here (a network home that
        // takes no full flush) keeps it, as the record beside it is kept.
        let _ = aterm_durable::durable_dir(&self.directory);
        self.text = text;
        self.claimed = claimed.cloned();
        self.earlier = None;
        Ok(())
    }

    /// Where the receipt is kept (a test's look).
    #[cfg(all(test, unix))]
    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    fn directory_owned(&self) -> io::Result<()> {
        let metadata = std::fs::symlink_metadata(&self.directory)?;
        if !metadata.file_type().is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "installer recovery directory is not a regular directory",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            if metadata.uid() != aterm_uds::peer::our_uid() || metadata.mode() & 0o022 != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "installer recovery directory has another owner or permits other writers",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn write_exclusive(&self) -> io::Result<()> {
        #[cfg(unix)]
        let created = {
            use std::os::unix::fs::DirBuilderExt as _;
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&self.directory)
        };
        #[cfg(not(unix))]
        let created = std::fs::create_dir(&self.directory);
        match created {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        self.directory_owned()?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
        }
        let mut file = options.open(&self.path)?;
        file.write_all(self.text.as_bytes())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        // An error leaves the exclusive allocation in place. No installer bytes have been
        // submitted yet; a partial or unreadable receipt can never recover custody.
        aterm_durable::ordered_or_fsync(&file)
    }

    pub(super) fn matches(&self) -> bool {
        self.directory_owned().is_ok()
            && crate::config_marker::read_marker(&self.path, MAX_BYTES).as_deref()
                == Some(self.text.as_str())
    }

    /// Whether this unchanged custody record belongs to the exact child still in the pool.
    pub(super) fn owns(&self, sid: &str, pid: i32, birth: Option<u64>) -> bool {
        self.sid == sid && self.pid == pid && Some(self.birth) == birth && self.matches()
    }

    /// The successor may have claimed the receipt before the outgoing process commits.
    /// Only that build claim may change; custody must still name the exact same child.
    pub(super) fn transferable(
        &self,
        mark: &Path,
        sid: &str,
        pid: i32,
        birth: Option<u64>,
    ) -> bool {
        self.sid == sid
            && self.pid == pid
            && Some(self.birth) == birth
            && Self::adopt(mark, sid, pid, birth).is_some_and(|current| current.path == self.path)
    }

    pub(super) fn remove_if_matching(&self) {
        if self.matches()
            && let Err(error) = std::fs::remove_file(&self.path)
            && error.kind() != io::ErrorKind::NotFound
        {
            aterm_log::warn!(
                "System State: the installer's recovery receipt was not removed: {error}"
            );
        }
    }
}

/// The folder of receipts beside the record at `mark`: the record's name with `.children`.
fn folder(mark: &Path) -> Option<PathBuf> {
    let mut directory = mark.file_name()?.to_os_string();
    directory.push(".children");
    Some(mark.with_file_name(directory))
}

/// Whether the process `pid` born `birth` runs now, and is no zombie: a receipt names the birth
/// [`aterm_pty::process_birth`] read as its child spawned ([`aterm_pty::record_spawned_shell`]).
#[cfg(any(not(test), unix))]
pub(super) fn alive(pid: i32, birth: u64) -> bool {
    aterm_pty::process_birth(pid) == Some(birth)
}

#[cfg(all(test, unix))]
#[path = "receipt_tests.rs"]
mod tests;
