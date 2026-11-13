// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Physical custody requires the platform's actual private-file and machine-key
//! implementation. Unsupported acquisition is an error, never absent custody.

use aterm_secrets::SecretBytes;
use aterm_secrets::age;
use std::path::Path;

#[cfg(unix)]
pub(super) use super::pending_native_store::{PhysicalIdentity, Store};
#[cfg(unix)]
pub(super) use aterm_uds::private_file::{
    check_private_dir, open_private, open_private_rw, read_private, read_private_with_metadata,
    write_private,
};

#[cfg(unix)]
pub(super) fn read_single_link_private(
    directory: &Path,
    name: &str,
    maximum: usize,
) -> std::io::Result<SecretBytes> {
    use std::os::unix::fs::MetadataExt as _;
    let (bytes, metadata) =
        read_private_with_metadata(directory, name, maximum).map_err(std::io::Error::other)?;
    let bytes = SecretBytes::new(bytes);
    if metadata.nlink() != 1 {
        return Err(std::io::Error::other("native auth document has aliases"));
    }
    Ok(bytes)
}

pub(super) fn require_available() -> Result<(), &'static str> {
    if cfg!(unix) {
        Ok(())
    } else {
        Err("native credential custody is unsupported on this platform")
    }
}

pub(super) fn machine_identity(home: &Path) -> Result<Option<age::Identity>, &'static str> {
    #[cfg(unix)]
    {
        use aterm_secrets::client;
        client::read_machine_identity(&client::Paths::locate_for_home(home))
            .map_err(|_| "native custody identity unavailable")
    }
    #[cfg(not(unix))]
    {
        let _ = home;
        Err("native credential custody is unsupported on this platform")
    }
}

#[cfg(unix)]
pub(super) use aterm_secrets::fsx::ensure_private_dir;

#[cfg(unix)]
pub(super) fn registry_lock(
    root: &Path,
) -> Result<aterm_uds::private_file::PrivateLock, &'static str> {
    aterm_uds::private_file::PrivateLock::take(
        root,
        "registry.lock",
        std::time::Duration::ZERO,
        "grant registry busy",
    )
    .map_err(|_| "grant registry is busy")
}

#[cfg(not(unix))]
pub(super) use unsupported::*;

#[cfg(not(unix))]
mod unsupported {
    use super::*;

    // No constructor can yield either type. In particular, an unsupported
    // platform cannot manufacture a claim that would satisfy custody consumers.
    pub(in crate::harness) enum Store {}
    pub(in crate::harness) enum RegistryLock {}
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(in crate::harness) enum PhysicalIdentity {}

    impl PhysicalIdentity {
        pub(in crate::harness) fn current(&self) -> bool {
            match *self {}
        }
        pub(in crate::harness) fn directory(&self) -> &Path {
            match *self {}
        }
    }

    pub(in crate::harness) fn registry_lock(_: &Path) -> Result<RegistryLock, &'static str> {
        Err("native credential custody is unsupported on this platform")
    }
    pub(in crate::harness) fn check_private_dir(_: &Path) -> std::io::Result<()> {
        Err(std::io::ErrorKind::Unsupported.into())
    }
    pub(in crate::harness) fn read_single_link_private(
        _: &Path,
        _: &str,
        _: usize,
    ) -> std::io::Result<SecretBytes> {
        Err(std::io::ErrorKind::Unsupported.into())
    }

    pub(in crate::harness) fn read_private(
        _: &Path,
        _: &str,
        _: usize,
    ) -> std::io::Result<Vec<u8>> {
        Err(std::io::ErrorKind::Unsupported.into())
    }
    pub(in crate::harness) fn write_private(_: &Path, _: &str, _: &[u8]) -> std::io::Result<()> {
        Err(std::io::ErrorKind::Unsupported.into())
    }
    pub(in crate::harness) fn open_private(
        _: &Path,
        _: &str,
    ) -> std::io::Result<(std::fs::File, std::fs::Metadata)> {
        Err(std::io::ErrorKind::Unsupported.into())
    }

    pub(in crate::harness) fn ensure_private_dir(_: &Path) -> std::io::Result<()> {
        Err(std::io::ErrorKind::Unsupported.into())
    }
    pub(in crate::harness) fn open_private_rw(_: &Path, _: &str) -> std::io::Result<std::fs::File> {
        Err(std::io::ErrorKind::Unsupported.into())
    }

    impl Store {
        pub(in crate::harness) fn physical_identity(
            &self,
        ) -> Result<PhysicalIdentity, &'static str> {
            match *self {}
        }
        pub(in crate::harness) fn open_owned(
            _: &Path,
            _: &Path,
            _: usize,
        ) -> Result<(Self, Option<SecretBytes>), &'static str> {
            Err("native credential custody is unsupported on this platform")
        }
        pub(in crate::harness) fn open_owned_existing(
            _: &Path,
            _: &Path,
            _: usize,
        ) -> Result<Option<(Self, Option<SecretBytes>)>, &'static str> {
            Err("native credential custody is unsupported on this platform")
        }
        pub(in crate::harness) fn recipient(&self) -> age::Recipient {
            match *self {}
        }
        pub(in crate::harness) fn sealed_digest(&self) -> Option<[u8; 32]> {
            match *self {}
        }
        pub(in crate::harness) fn unchanged(&self) -> Result<(), &'static str> {
            match *self {}
        }
        pub(in crate::harness) fn save(&mut self, _: &[u8]) -> Result<(), &'static str> {
            match *self {}
        }
        pub(in crate::harness) fn recover_exact_write(
            &mut self,
            _: &age::Identity,
            _: &[u8],
        ) -> Result<bool, &'static str> {
            match *self {}
        }
    }
}

#[cfg(all(test, not(unix)))]
#[path = "native_custody_platform_tests.rs"]
mod tests;
