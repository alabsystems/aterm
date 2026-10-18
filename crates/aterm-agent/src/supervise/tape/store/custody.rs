// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Hosted-store operations over retained directory handles. Pathnames below
//! are labels, never authority to reopen an admitted parent. Existing roots
//! and session directories must be owned by this uid and private (0700).
//! Unsupported platforms refuse this optional development store.
//!
//! Retirement first transfers a name, without replacement, to an unpredictable
//! name under the held private root. It checks the transferred inode against
//! the still-open tape before unlinking. A different entry is restored without
//! replacement, or preserved at the transfer name if restoration is occupied.
//! This is not unlink-by-fd: a hostile same-uid writer that discovers and
//! replaces the fresh private transfer name can still race check-to-unlink.
//! Nor is a directory scan a snapshot of concurrent writers. No such stronger
//! POSIX guarantee is claimed. Ancestor replacement never redirects an operation.

use std::fs::File;
use std::io;
use std::path::Path;

use super::Stored;

#[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
mod platform {
    use super::*;
    use std::ffi::{OsStr, OsString};
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::MetadataExt as _;
    use std::path::{Component, PathBuf};
    use std::sync::Arc;

    use aterm_dirfd::{self as fd, AtFlags, Mode, OFlags, RenameFlags};

    struct Directory {
        file: File,
        path: PathBuf,
    }

    pub struct Store {
        root: Arc<Directory>,
    }

    pub struct Tape {
        pub stored: Stored,
        pub file: File,
        parent: Arc<Directory>,
        name: OsString,
    }

    fn refused(why: &str) -> io::Error {
        io::Error::new(io::ErrorKind::PermissionDenied, why)
    }

    fn component(name: &OsStr) -> io::Result<()> {
        let bytes = name.as_bytes();
        if bytes.is_empty()
            || bytes == b"."
            || bytes == b".."
            || bytes.contains(&b'/')
            || bytes.contains(&0)
        {
            return Err(refused("not one store component"));
        }
        Ok(())
    }

    fn flags() -> OFlags {
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC
    }

    fn open(parent: &File, name: &OsStr, directory: bool) -> io::Result<File> {
        component(name)?;
        let flags = if directory {
            flags() | OFlags::DIRECTORY
        } else {
            flags()
        };
        Ok(File::from(fd::openat(parent, name, flags, Mode::empty())?))
    }

    fn private_directory(file: &File) -> io::Result<()> {
        let meta = file.metadata()?;
        if !meta.is_dir()
            || meta.uid() != aterm_uds::peer::our_uid()
            || meta.mode() & 0o777 != 0o700
        {
            return Err(refused("store directory is not owned and private (0700)"));
        }
        Ok(())
    }

    fn private_tape(file: &File) -> io::Result<std::fs::Metadata> {
        let meta = file.metadata()?;
        if !meta.is_file()
            || meta.uid() != aterm_uds::peer::our_uid()
            || meta.mode() & 0o777 != 0o600
            || meta.nlink() != 1
        {
            return Err(refused(
                "store tape is not a private owned single-link regular file",
            ));
        }
        Ok(meta)
    }

    fn child(parent: &Arc<Directory>, name: &OsStr, create: bool) -> io::Result<Arc<Directory>> {
        let file = match open(&parent.file, name, true) {
            Ok(file) => file,
            Err(e) if create && e.kind() == io::ErrorKind::NotFound => {
                // No recursive adoption and no chmod of an existing entry.
                fd::mkdirat(&parent.file, name, Mode::RWXU)?;
                open(&parent.file, name, true)?
            }
            Err(e) => return Err(e),
        };
        private_directory(&file)?;
        Ok(Arc::new(Directory {
            file,
            path: parent.path.join(name),
        }))
    }

    fn names(dir: &Directory) -> io::Result<Vec<OsString>> {
        let mut names = Vec::new();
        for entry in fd::Dir::read_from(&dir.file)? {
            let entry = entry?;
            let name = OsStr::from_bytes(entry.file_name().to_bytes());
            if name == "." || name == ".." {
                continue;
            }
            component(name)?;
            names.push(name.to_owned());
        }
        Ok(names)
    }

    impl Tape {
        /// Observe this inventory's retained tape, not a path another writer
        /// can replace. Inventory opens a fresh description; it owns no lock.
        pub fn owner(&self) -> aterm_uds::ownerlock::Owner {
            use aterm_uds::ownerlock::Owner;
            #[cfg(any(target_vendor = "apple", target_os = "linux"))]
            {
                let file: &std::fs::File = &self.file;
                #[expect(
                    clippy::disallowed_methods,
                    reason = "the inventory probe releases every taken lock by LOCK_UN before close"
                )]
                let taken = file.try_lock();
                match taken {
                    Ok(()) => match file.unlock() {
                        Ok(()) => Owner::Dead,
                        Err(_) => Owner::Unknown,
                    },
                    Err(std::fs::TryLockError::WouldBlock) => Owner::Live,
                    Err(std::fs::TryLockError::Error(_)) => Owner::Unknown,
                }
            }
            #[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
            {
                Owner::Unknown
            }
        }
    }

    impl Store {
        pub fn open(path: &Path, create: bool) -> io::Result<Self> {
            if !path.is_absolute() {
                return Err(refused("store path is not absolute"));
            }
            let leaf = path
                .file_name()
                .ok_or_else(|| refused("store has no leaf"))?;
            let parent = path
                .parent()
                .ok_or_else(|| refused("store has no parent"))?;
            let mut file = File::from(fd::openat(
                fd::CWD,
                "/",
                flags() | OFlags::DIRECTORY,
                Mode::empty(),
            )?);
            for part in parent.components() {
                match part {
                    Component::RootDir => {}
                    Component::Normal(name) => file = open(&file, name, true)?,
                    _ => return Err(refused("store path contains a non-normal component")),
                }
            }
            let parent = Arc::new(Directory {
                file,
                path: parent.to_path_buf(),
            });
            Ok(Self {
                root: child(&parent, leaf, create)?,
            })
        }

        pub fn tapes(&self, sid: Option<&str>) -> io::Result<Vec<Tape>> {
            private_directory(&self.root.file)?;
            let requested =
                sid.map(|sid| super::super::session_dir(Path::new(""), Some(sid)).into_os_string());
            let mut dirs = Vec::new();
            for name in names(&self.root)? {
                if requested
                    .as_ref()
                    .is_some_and(|r| &name != r && name != super::super::KEPT)
                {
                    continue;
                }
                let file = open(&self.root.file, &name, false)?;
                if !file.metadata()?.is_dir() {
                    continue;
                }
                private_directory(&file)?;
                dirs.push((
                    Arc::new(Directory {
                        file,
                        path: self.root.path.join(&name),
                    }),
                    name == super::super::KEPT,
                ));
            }
            let mut tapes = Vec::new();
            for (dir, kept) in dirs {
                for name in names(&dir)? {
                    if !Path::new(&name)
                        .extension()
                        .is_some_and(|x| x == super::super::EXT)
                    {
                        continue;
                    }
                    let file = open(&dir.file, &name, false)?;
                    let meta = private_tape(&file)?;
                    let stored = Stored {
                        path: dir.path.join(&name),
                        bytes: meta.len(),
                        modified: meta.modified()?,
                        kept,
                    };
                    tapes.push(Tape {
                        stored,
                        file,
                        parent: dir.clone(),
                        name,
                    });
                }
            }
            tapes.sort_by(|a, b| {
                b.stored
                    .modified
                    .cmp(&a.stored.modified)
                    .then_with(|| b.stored.path.cmp(&a.stored.path))
            });
            Ok(tapes)
        }

        pub fn create(&self, sid: Option<&str>, name: &str) -> io::Result<Tape> {
            private_directory(&self.root.file)?;
            let session = super::super::session_dir(Path::new(""), sid);
            let dir = child(&self.root, session.as_os_str(), true)?;
            component(OsStr::new(name))?;
            let flags = OFlags::WRONLY
                | OFlags::CREATE
                | OFlags::EXCL
                | OFlags::NOFOLLOW
                | OFlags::CLOEXEC
                | OFlags::NONBLOCK;
            let file = File::from(fd::openat(&dir.file, name, flags, Mode::RUSR | Mode::WUSR)?);
            let meta = private_tape(&file)?;
            Ok(Tape {
                stored: Stored {
                    path: dir.path.join(name),
                    bytes: meta.len(),
                    modified: meta.modified()?,
                    kept: false,
                },
                file,
                parent: dir,
                name: name.into(),
            })
        }

        pub fn retire(&self, tape: &Tape) -> io::Result<()> {
            // Randomness supplies an unadvertised transfer name; NOREPLACE supplies
            // uniqueness. A collision is a refusal, never a pre-delete/retry.
            let name = format!(".retiring-{}", aterm_uds::rand::hex_token::<16>()?);
            private_directory(&self.root.file)?;
            private_directory(&tape.parent.file)?;
            let expected = private_tape(&tape.file)?;
            fd::renameat_with(
                &tape.parent.file,
                &tape.name,
                &self.root.file,
                &name,
                RenameFlags::NOREPLACE,
            )?;
            let transferred =
                open(&self.root.file, OsStr::new(&name), false).and_then(|f| private_tape(&f));
            let same = transferred
                .as_ref()
                .is_ok_and(|m| m.dev() == expected.dev() && m.ino() == expected.ino());
            if !same {
                // Restore without clobbering any newly occupied original name.
                // If that refuses, preserve the transferred bytes at `name`.
                let restored = fd::renameat_with(
                    &self.root.file,
                    &name,
                    &tape.parent.file,
                    &tape.name,
                    RenameFlags::NOREPLACE,
                );
                let where_left = match restored {
                    Ok(()) => format!("restored at {}", tape.stored.path.display()),
                    Err(e) => format!(
                        "restoration refused ({e}); transfer preserved at {}",
                        self.root.path.join(name).display()
                    ),
                };
                return Err(io::Error::other(format!(
                    "store entry changed; {where_left}"
                )));
            }
            fd::unlinkat(&self.root.file, &name, AtFlags::empty()).map_err(|e| {
                io::Error::other(format!(
                    "retirement refused at {}: {e}",
                    self.root.path.join(name).display()
                ))
            })?;
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::io::Write as _;
        use std::os::unix::fs::{DirBuilderExt as _, symlink};

        fn dir(path: &Path) {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(path)
                .expect("owned directory");
        }

        #[test]
        fn held_store_handles_do_not_delete_an_ancestor_or_entry_replacement() {
            let guard = aterm_tempfile::tempdir().expect("exclusive fixture");
            let base = guard
                .path()
                .canonicalize()
                .expect("owned canonical fixture");
            let root = base.join("tapes");
            dir(&root);
            let held = Store::open(&root, false).expect("private store");
            let made = held
                .create(Some("@s-1"), "one.atape")
                .expect("exclusive tape");
            assert!(
                held.create(Some("@s-1"), "one.atape").is_err(),
                "occupied name is preserved"
            );
            let moved = base.join("moved");
            std::fs::rename(&root, &moved).expect("move owned root");
            let other = base.join("other");
            dir(&other);
            dir(&other.join("s-1"));
            std::fs::write(
                other.join("s-1/one.atape"),
                b"unrelated ancestor replacement",
            )
            .expect("own bytes");
            symlink(&other, &root).expect("owned replacement link");
            held.retire(&made).expect("retire under original held root");
            assert!(!moved.join("s-1/one.atape").exists());
            assert_eq!(
                std::fs::read(other.join("s-1/one.atape")).expect("replacement retained"),
                b"unrelated ancestor replacement"
            );

            let mut made = held.create(Some("@s-1"), "two.atape").expect("second tape");
            made.file.write_all(b"original").expect("original bytes");
            let path = moved.join("s-1/two.atape");
            std::fs::rename(&path, moved.join("s-1/original-held.atape"))
                .expect("move original entry");
            let mut replacement = super::super::super::create(&path).expect("new replacement");
            replacement
                .write_all(b"unrelated entry replacement")
                .expect("replacement bytes");
            drop(replacement);
            assert!(
                held.retire(&made).is_err(),
                "changed entry refuses retirement"
            );
            assert_eq!(
                std::fs::read(&path).expect("replacement restored"),
                b"unrelated entry replacement"
            );
            assert_eq!(
                std::fs::read(moved.join("s-1/original-held.atape")).expect("original retained"),
                b"original"
            );
        }
    }
}

#[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
mod platform {
    use super::*;
    pub struct Store;
    pub struct Tape {
        pub stored: Stored,
        pub file: File,
    }
    impl Tape {
        pub fn owner(&self) -> aterm_uds::ownerlock::Owner {
            aterm_uds::ownerlock::Owner::Unknown
        }
    }
    fn unsupported() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "hosted tape store needs nofollow directory handles, complete directory reads, and no-replace rename",
        )
    }
    impl Store {
        pub fn open(_: &Path, _: bool) -> io::Result<Self> {
            Err(unsupported())
        }
        pub fn tapes(&self, _: Option<&str>) -> io::Result<Vec<Tape>> {
            Err(unsupported())
        }
        pub fn create(&self, _: Option<&str>, _: &str) -> io::Result<Tape> {
            Err(unsupported())
        }
        pub fn retire(&self, _: &Tape) -> io::Result<()> {
            Err(unsupported())
        }
    }
}

pub(super) use platform::{Store, Tape};
