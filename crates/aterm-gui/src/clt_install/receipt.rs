// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact installer child custody, never command authority. The claimed owner remains unchanged
//! across updates. Only a proven preprompt end may release that owner's ask; an unknown orphan
//! retains the durable hold. An unclaimed receipt cannot release another opening's claim.

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
    /// The build whose line the tab's own claim (its landing's, or a later adoption's) added to
    /// the account's record, if one added a line.
    claimed: Option<String>,
    sid: String,
    pid: i32,
    birth: u64,
}

/// A receipt's last line: `claimed <build>` for the build whose line the tab's own claim added,
/// else [`UNCLAIMED`]; `None` for a build no line of the record could name.
fn claim_line(claimed: Option<&str>) -> Option<String> {
    match claimed {
        None => Some(UNCLAIMED.to_owned()),
        Some(build)
            if build.is_empty()
                || build.len() > aterm_types::clt_mark::BUILD_BYTES
                || build.contains(char::is_whitespace) =>
        {
            None
        }
        Some(build) => Some(format!("claimed {build}")),
    }
}

impl Receipt {
    /// The receipt of the installer child `sid` (`pid`, born `birth`) in a folder beside the
    /// record at `mark`: `v2`, the child's identity, and `claimed <build>` where that tab's own
    /// claim added `claimed`'s line to the record, else `unclaimed`. `None` for an identity no
    /// receipt names (a pid of 1 or less, no birth, a session id that is none) and a build no line
    /// of the record could name.
    pub(super) fn for_child(
        mark: &Path,
        sid: &str,
        pid: i32,
        birth: Option<u64>,
        claimed: Option<&str>,
    ) -> Option<Self> {
        let birth = birth.filter(|birth| *birth > 0)?;
        if pid <= 1 || !crate::keeper_link::valid_sid(sid) {
            return None;
        }
        let claim = claim_line(claimed)?;
        let directory = folder(mark)?;
        let name = format!("{sid}-{pid}-{birth}");
        let text = format!("v2\n{sid}\n{pid}\n{birth}\n{claim}\n");
        (name.len() <= 255 && text.len() <= MAX_BYTES).then(|| Self {
            path: directory.join(name),
            directory,
            text,
            claimed: claimed.map(str::to_owned),
            sid: sid.to_owned(),
            pid,
            birth,
        })
    }

    /// The receipt an earlier aterm wrote for the live child `sid` (`pid`, born `birth`) beside the
    /// record at `mark`, read back whole ([`Self::for_child`]'s text exactly, whatever build it
    /// says claimed): `None` where there is none of exactly that child, its folder is not this
    /// account's alone, or it is partial, malformed or no regular file.
    pub(super) fn adopt(mark: &Path, sid: &str, pid: i32, birth: Option<u64>) -> Option<Self> {
        let unclaimed = Self::for_child(mark, sid, pid, birth, None)?;
        unclaimed.directory_owned().ok()?;
        let text = crate::config_marker::read_marker(&unclaimed.path, MAX_BYTES)?;
        let head = unclaimed.text.strip_suffix(&format!("{UNCLAIMED}\n"))?;
        let claim = text.strip_prefix(head)?.strip_suffix('\n')?;
        let claimed = match claim {
            UNCLAIMED => None,
            claim => Some(claim.strip_prefix("claimed ")?),
        };
        Self::for_child(mark, sid, pid, birth, claimed).filter(|receipt| receipt.text == text)
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

    /// The build whose line the tab's own claim added to the account's record, if one added a line.
    pub(super) fn claimed(&self) -> Option<&str> {
        self.claimed.as_deref()
    }

    /// An adoption's claim added `build`'s line to the account's record: the receipt on disk, still
    /// exactly this one ([`Self::matches`]), is replaced whole by one whose last line is `claimed
    /// <build>`, and this receipt becomes that one, so [`Self::remove_if_matching`] still finds it.
    /// The new text goes to a file of this process's own in the receipt's owned folder (created
    /// exclusively, never through a link, `0600`), flushed and renamed over the receipt, and the
    /// folder flushed where it can be. On an error the receipt on disk and this one stay as they
    /// were, and no staged file is left.
    ///
    /// # Errors
    /// The receipt on disk is no longer exactly this one, or its folder is not this account's
    /// alone ([`io::ErrorKind::InvalidData`]); `build` is no build a line of the record could
    /// name ([`io::ErrorKind::InvalidInput`]); or the staged file's creation, write, flush or
    /// rename failed.
    pub(super) fn claim(&mut self, build: &str) -> io::Result<()> {
        let invalid = |why: &str| io::Error::new(io::ErrorKind::InvalidInput, why.to_owned());
        let head = claim_line(self.claimed.as_deref())
            .and_then(|line| self.text.strip_suffix(&format!("{line}\n")))
            .ok_or_else(|| invalid("the receipt's own text has no claim line"))?;
        let line = claim_line(Some(build)).ok_or_else(|| invalid("no build a line could name"))?;
        let text = format!("{head}{line}\n");
        if text.len() > MAX_BYTES {
            return Err(invalid(
                "the claimed receipt is larger than a receipt may hold",
            ));
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
            .join(format!(".{name}.claim-{}", std::process::id()));
        // One an earlier process of this pid left as it died mid-claim; a link goes, not where
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
        self.claimed = Some(build.to_owned());
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
mod tests {
    use super::*;

    const SID: &str = "s-0123456789abcdef0123";

    #[test]
    fn immutable_child_receipts_are_exact_bounded_and_removed_only_when_matching() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mark = root.path().join("command-line-tools-tab");
        let receipt = Receipt::for_child(&mark, SID, 23, Some(45), None).unwrap();
        receipt.write_exclusive().unwrap();
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&receipt.path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert!(receipt.matches());
        assert_eq!(
            receipt.write_exclusive().unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        for (sid, pid, birth) in [("s-abcdef", 23, 45), (SID, 24, 45), (SID, 23, 46)] {
            assert!(
                !Receipt::for_child(&mark, sid, pid, Some(birth), None)
                    .unwrap()
                    .matches()
            );
        }
        std::fs::write(&receipt.path, b"partial\n").unwrap();
        assert!(!receipt.matches());
        receipt.remove_if_matching();
        assert_eq!(std::fs::read(&receipt.path).unwrap(), b"partial\n");
        std::fs::write(&receipt.path, &receipt.text).unwrap();
        receipt.remove_if_matching();
        assert!(!receipt.path.exists());
        for (sid, pid, birth) in [
            ("../escaped", 23, Some(45)),
            (SID, 1, Some(45)),
            (SID, 23, Some(0)),
            (SID, 23, None),
        ] {
            assert!(Receipt::for_child(&mark, sid, pid, birth, None).is_none());
        }
    }

    /// A receipt says whether that tab's landing claim added a build's line to the record, and an
    /// adopting launch reads it back with the receipt: `claimed <build>` or `unclaimed`, the rest
    /// of the receipt exactly that child's. A claim line that is neither, and a build no line of
    /// the record could name, give no receipt.
    /// NEGATIVE CONTROL: the receipt of another child, or none at all, adopts nothing.
    #[test]
    fn a_receipt_says_whose_line_its_landing_claimed() {
        const BUILD: &str = "0.9.0+gabc1234";
        let root = aterm_tempfile::tempdir().unwrap();
        let mark = root.path().join("command-line-tools-tab");
        assert!(
            Receipt::adopt(&mark, SID, 23, Some(45)).is_none(),
            "none written"
        );
        for claimed in [Some(BUILD), None] {
            let receipt = Receipt::for_child(&mark, SID, 23, Some(45), claimed).unwrap();
            receipt.write_exclusive().unwrap();
            let adopted = Receipt::adopt(&mark, SID, 23, Some(45)).expect("its own receipt");
            assert_eq!(adopted.claimed(), claimed);
            assert!(adopted.matches(), "{claimed:?}");
            assert!(
                Receipt::adopt(&mark, SID, 24, Some(45)).is_none(),
                "another child's"
            );
            std::fs::remove_file(&receipt.path).unwrap();
        }
        let head = format!("v2\n{SID}\n23\n45\n");
        for claim in [
            "claimed\n",
            "claimed two words\n",
            "spent\n",
            "",
            "unclaimed",
        ] {
            std::fs::write(
                Receipt::for_child(&mark, SID, 23, Some(45), None)
                    .unwrap()
                    .path,
                format!("{head}{claim}"),
            )
            .unwrap();
            assert!(
                Receipt::adopt(&mark, SID, 23, Some(45)).is_none(),
                "{claim:?}"
            );
        }
        for unnameable in [
            "",
            "two words",
            &"x".repeat(aterm_types::clt_mark::BUILD_BYTES + 1),
        ] {
            assert!(
                Receipt::for_child(&mark, SID, 23, Some(45), Some(unnameable)).is_none(),
                "{unnameable:?}"
            );
        }
    }

    /// An adoption's claim replaces the receipt whole: the one on disk says the adopting build,
    /// still `0600` and exactly that child's, the receipt in memory becomes it (so its removal
    /// still finds it), and no staged file is left. A receipt on disk that is no longer exactly
    /// the adopted one, a build no line could name, and a link at the receipt's place are
    /// refused, leaving what is there as it was.
    /// NEGATIVE CONTROL: the mutant whose claim skips its `matches` look replaces a receipt that
    /// is no longer the adopted child's (the `partial` case).
    #[test]
    fn an_adoptions_claim_replaces_the_receipt_whole() {
        use std::os::unix::fs::PermissionsExt as _;
        const EARLIER: &str = "0.1.0+gdeadbeef";
        const BUILD: &str = "0.9.0+gabc1234";
        let root = aterm_tempfile::tempdir().unwrap();
        let mark = root.path().join("command-line-tools-tab");
        Receipt::for_child(&mark, SID, 23, Some(45), Some(EARLIER))
            .unwrap()
            .write_exclusive()
            .unwrap();
        let mut adopted = Receipt::adopt(&mark, SID, 23, Some(45)).unwrap();
        let before = Receipt::adopt(&mark, SID, 23, Some(45)).unwrap();
        adopted.claim(BUILD).unwrap();
        assert_eq!(adopted.claimed(), Some(BUILD));
        assert!(
            adopted.matches(),
            "the receipt in memory is the one on disk"
        );
        assert!(!before.matches(), "the earlier text is gone");
        let read = Receipt::adopt(&mark, SID, 23, Some(45)).expect("still that child's");
        assert_eq!(read.claimed(), Some(BUILD));
        assert_eq!(read.text, adopted.text);
        assert_eq!(
            std::fs::metadata(&adopted.path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let directory = adopted.directory.clone();
        let names = || -> Vec<_> {
            std::fs::read_dir(&directory)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect()
        };
        assert_eq!(
            names(),
            [adopted.path.file_name().unwrap()],
            "no staged file"
        );
        before.remove_if_matching();
        assert!(
            adopted.path.exists(),
            "an earlier text's removal finds nothing"
        );
        adopted.claim(EARLIER).unwrap();
        assert_eq!(
            Receipt::adopt(&mark, SID, 23, Some(45)).unwrap().claimed(),
            Some(EARLIER)
        );

        let held = std::fs::read(&adopted.path).unwrap();
        for unnameable in ["", "two words"] {
            assert_eq!(
                adopted.claim(unnameable).unwrap_err().kind(),
                io::ErrorKind::InvalidInput,
                "{unnameable:?}"
            );
            assert_eq!(
                std::fs::read(&adopted.path).unwrap(),
                held,
                "{unnameable:?}"
            );
        }
        std::fs::write(&adopted.path, b"partial\n").unwrap();
        assert_eq!(
            adopted.claim(BUILD).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(std::fs::read(&adopted.path).unwrap(), b"partial\n");
        let owner = root.path().join("owner");
        std::fs::write(&owner, &adopted.text).unwrap();
        std::fs::remove_file(&adopted.path).unwrap();
        std::os::unix::fs::symlink(&owner, &adopted.path).unwrap();
        assert!(
            adopted.claim(BUILD).is_err(),
            "a link at the receipt's place"
        );
        assert_eq!(std::fs::read_to_string(&owner).unwrap(), adopted.text);
        assert!(
            std::fs::symlink_metadata(&adopted.path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(names().len(), 1, "no staged file");
        adopted.remove_if_matching();
        assert!(std::fs::symlink_metadata(&adopted.path).is_ok());
    }

    #[test]
    fn malformed_and_nonregular_receipts_neither_match_nor_get_replaced() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mark = root.path().join("command-line-tools-tab");
        let receipt = Receipt::for_child(&mark, SID, 23, Some(45), None).unwrap();
        std::fs::create_dir(&receipt.directory).unwrap();
        let owner = root.path().join("owner");
        std::fs::write(&owner, &receipt.text).unwrap();
        for kind in ["partial", "oversized", "link", "directory"] {
            match kind {
                "partial" => std::fs::write(&receipt.path, b"v1\n").unwrap(),
                "oversized" => std::fs::write(&receipt.path, vec![b'x'; MAX_BYTES + 1]).unwrap(),
                "link" => std::os::unix::fs::symlink(&owner, &receipt.path).unwrap(),
                _ => std::fs::create_dir(&receipt.path).unwrap(),
            }
            assert!(!receipt.matches(), "{kind}");
            assert!(receipt.write_exclusive().is_err(), "{kind}");
            receipt.remove_if_matching();
            assert!(std::fs::symlink_metadata(&receipt.path).is_ok(), "{kind}");
            if kind == "directory" {
                std::fs::remove_dir(&receipt.path).unwrap();
            } else {
                std::fs::remove_file(&receipt.path).unwrap();
            }
        }
        assert_eq!(std::fs::read_to_string(&owner).unwrap(), receipt.text);
    }

    #[test]
    fn other_writers_or_a_link_at_the_recovery_directory_give_no_custody() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = aterm_tempfile::tempdir().unwrap();
        let mark = root.path().join("command-line-tools-tab");
        let receipt = Receipt::for_child(&mark, SID, 23, Some(45), None).unwrap();
        receipt.write_exclusive().unwrap();
        for mode in [0o770, 0o777] {
            std::fs::set_permissions(&receipt.directory, std::fs::Permissions::from_mode(mode))
                .unwrap();
            assert!(!receipt.matches());
            assert_eq!(
                receipt.write_exclusive().unwrap_err().kind(),
                io::ErrorKind::PermissionDenied
            );
            receipt.remove_if_matching();
            assert_eq!(
                std::fs::read_to_string(&receipt.path).unwrap(),
                receipt.text
            );
        }
        std::fs::set_permissions(&receipt.directory, std::fs::Permissions::from_mode(0o755))
            .unwrap();
        assert!(
            receipt.matches(),
            "a non-writable owned directory is sufficient"
        );
        receipt.remove_if_matching();
        std::fs::remove_dir(&receipt.directory).unwrap();
        let elsewhere = root.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &receipt.directory).unwrap();
        assert!(receipt.write_exclusive().is_err());
        assert!(!receipt.matches());
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
    }

    /// A launch reads back, whole, each receipt whose child no longer runs, and none whose child
    /// does; a staged claim file, a stranger's name and a partial receipt are no receipt. This
    /// process runs at the birth [`alive`] reads, and at no other.
    /// NEGATIVE CONTROL: the mutant whose look skips `alive` reads the running child's receipt as
    /// gone, which its adoption still needs.
    #[test]
    fn a_receipt_whose_child_is_gone_is_an_orphan() {
        const BUILD: &str = "0.9.0+gabc1234";
        const OTHER: &str = "s-fedcba9876543210fedc";
        let root = aterm_tempfile::tempdir().unwrap();
        let mark = root.path().join("command-line-tools-tab");
        assert!(
            Receipt::orphans(&mark, |_, _| false).is_empty(),
            "no folder"
        );
        let gone = Receipt::for_child(&mark, SID, 23, Some(45), Some(BUILD)).unwrap();
        gone.write_exclusive().unwrap();
        let running = Receipt::for_child(&mark, OTHER, 24, Some(46), None).unwrap();
        running.write_exclusive().unwrap();
        let directory = gone.directory.clone();
        std::fs::write(directory.join(format!(".{SID}-23-45.claim-7")), &gone.text).unwrap();
        std::fs::write(directory.join("notes"), b"v2\n").unwrap();
        std::fs::write(directory.join(format!("{OTHER}-25-47")), b"partial\n").unwrap();

        let orphans = Receipt::orphans(&mark, |pid, birth| (pid, birth) == (24, 46));
        assert_eq!(orphans.len(), 1, "{orphans:?}");
        assert_eq!(orphans[0].text, gone.text);
        assert_eq!(orphans[0].claimed(), Some(BUILD));
        assert!(orphans[0].matches());
        assert_eq!(Receipt::orphans(&mark, |_, _| true).len(), 0, "all running");

        let own = i32::try_from(std::process::id()).unwrap();
        let birth = aterm_pty::process_birth(own).expect("this process's birth");
        assert!(alive(own, birth));
        assert!(!alive(own, birth + 1), "another process at this pid");
    }
}
