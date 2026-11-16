// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Offline compile-facts admission bound to a held executable and its complete content.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

#[path = "artifact_snapshot.rs"]
mod snapshot;
pub(crate) use snapshot::Snapshot;

/// The requested assertion contract, checked against the artifact's actual compiled facts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Profile {
    Debug,
    #[default]
    Release,
}

impl Profile {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "debug" => Some(Self::Debug),
            "release" => Some(Self::Release),
            _ => None,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Release => "release",
        }
    }

    const fn debug(self) -> bool {
        matches!(self, Self::Debug)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(windows)]
    inode: u128,
    size: u64,
    modified: std::time::SystemTime,
}

fn identity(file: &File) -> Result<Identity, String> {
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    #[cfg(any(unix, windows))]
    {
        let stat = aterm_dirfd::fstat(file).map_err(|e| e.to_string())?;
        Ok(Identity {
            device: stat.st_dev,
            inode: stat.st_ino,
            size: metadata.len(),
            modified: metadata.modified().map_err(|e| e.to_string())?,
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = metadata;
        Err("this platform cannot retain executable file identity".into())
    }
}

const MAX_EXECUTABLE_BYTES: u64 = 1024 * 1024 * 1024;

fn digest(file: &mut File) -> Result<[u8; 32], String> {
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut hash = aterm_digest::Sha256::new();
    let mut buffer = [0; 65536];
    let mut total = 0_u64;
    loop {
        let count = match file.read(&mut buffer) {
            Ok(count) => count,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.to_string()),
        };
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > MAX_EXECUTABLE_BYTES {
            return Err("bundle executable exceeds the 1 GiB admission bound".into());
        }
        hash.update(&buffer[..count]);
    }
    Ok(hash.finalize())
}

/// Custody of the file whose named compile-facts sections established the requested profile.
#[derive(Debug)]
pub(crate) struct Executable {
    held: Arc<File>,
    identity: Identity,
    digest: [u8; 32],
    profile: Profile,
    cargo_profile: String,
    #[cfg_attr(
        not(unix),
        expect(
            dead_code,
            reason = "only the unix private copy compares the held facts"
        )
    )]
    facts: [u8; aterm_stamp::facts::LEN],
}

impl PartialEq for Executable {
    fn eq(&self, other: &Self) -> bool {
        self.identity == other.identity
            && self.digest == other.digest
            && self.profile == other.profile
    }
}
impl Eq for Executable {}

impl Executable {
    pub(crate) fn open(path: &Path, expected: Profile) -> Result<Self, String> {
        let mut file = crate::open_regular(path).map_err(|e| e.to_string())?;
        let before = identity(&file)?;
        if before.size > MAX_EXECUTABLE_BYTES {
            return Err("bundle executable exceeds the 1 GiB admission bound".into());
        }
        let digest_before = digest(&mut file)?;
        let records = aterm_stamp::facts::macho_records(&mut file)?;
        let mut active = None;
        let mut cargo_profile = None;
        for record in &records {
            let facts = aterm_stamp::facts::decode(record)?;
            if facts.debug_assertions != expected.debug() {
                return Err(format!(
                    "bundle compiled debug_assertions={} contradicts --app-profile {}",
                    facts.debug_assertions,
                    expected.name()
                ));
            }
            if cargo_profile
                .replace(facts.profile)
                .is_some_and(|prior| prior != facts.profile)
            {
                return Err("universal bundle slices disagree on their Cargo profile".into());
            }
            if facts.target_arch == std::env::consts::ARCH {
                active = Some((facts, *record));
            }
        }
        let (facts, active_record) =
            active.ok_or("bundle has no compile-facts slice for this verifier's architecture")?;
        let profile = facts.profile.to_owned();
        if identity(&file)? != before || digest(&mut file)? != digest_before {
            return Err("bundle executable changed while its compile facts were read".into());
        }
        Ok(Self {
            held: Arc::new(file),
            identity: before,
            digest: digest_before,
            profile: expected,
            cargo_profile: profile,
            facts: active_record,
        })
    }

    /// Re-read the exact named file before a consumer can start anything. A same-content
    /// replacement is still a different admitted executable; holding the old inode prevents reuse.
    pub(crate) fn validate(&self, path: &Path) -> Result<(), String> {
        let mut now = crate::open_regular(path).map_err(|e| e.to_string())?;
        if identity(&self.held)? != self.identity
            || identity(&now)? != self.identity
            || digest(&mut now)? != self.digest
            || identity(&now)? != self.identity
        {
            return Err("bundle executable no longer matches its admitted file and content".into());
        }
        Ok(())
    }

    pub(crate) fn matches_alias(&self, path: &Path) -> Result<(), String> {
        let now = crate::open_regular(path).map_err(|e| e.to_string())?;
        if identity(&now)? != self.identity {
            return Err(format!(
                "{} is not the admitted bundle executable",
                path.display()
            ));
        }
        Ok(())
    }

    pub(crate) fn describe(&self) -> String {
        format!(
            "{} assertions (Cargo profile {}; executable sha256={}){}",
            self.profile.name(),
            self.cargo_profile,
            aterm_codec::hex::encode_lower(&self.digest),
            if self.profile == Profile::Debug {
                "; debug validation does not qualify a release"
            } else {
                ""
            }
        )
    }

    pub(crate) const fn profile(&self) -> Profile {
        self.profile
    }

    /// Copy the held image rather than looking its pathname up again. Positional reads do not
    /// share a seek cursor between simultaneous snapshots of this admitted executable.
    #[cfg(unix)]
    fn copy_held(&self, to: &Path) -> Result<(), String> {
        use std::io::Write as _;
        use std::os::unix::fs::{FileExt as _, OpenOptionsExt as _, PermissionsExt as _};

        if identity(&self.held)? != self.identity {
            return Err("admitted executable changed before its private copy".into());
        }
        let mut sink = std::fs::OpenOptions::new()
            .write(true)
            .read(true)
            .create_new(true)
            .mode(0o500)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(to)
            .map_err(|e| e.to_string())?;
        let mut buffer = [0; 65536];
        let mut offset = 0_u64;
        let mut hash = aterm_digest::Sha256::new();
        loop {
            let count = match self.held.read_at(&mut buffer, offset) {
                Ok(count) => count,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.to_string()),
            };
            if count == 0 {
                break;
            }
            offset += count as u64;
            if offset > MAX_EXECUTABLE_BYTES {
                return Err("private executable exceeds the admission bound".into());
            }
            hash.update(&buffer[..count]);
            sink.write_all(&buffer[..count])
                .map_err(|e| e.to_string())?;
        }
        if offset != self.identity.size
            || hash.finalize() != self.digest
            || identity(&self.held)? != self.identity
        {
            return Err("held executable changed while its private copy was made".into());
        }
        sink.set_permissions(std::fs::Permissions::from_mode(0o500))
            .map_err(|e| e.to_string())?;
        let copied = Self::open(to, self.profile)?;
        if copied.digest != self.digest || copied.facts != self.facts {
            return Err("private executable differs from its admitted image".into());
        }
        Ok(())
    }
}

// Every test and fixture here reads or lays out a macOS bundle through unix file APIs.
#[cfg(all(test, unix))]
#[path = "artifact_tests.rs"]
pub(crate) mod tests;
