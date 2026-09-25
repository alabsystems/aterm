// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Laying EXECUTABLES — the `bin/` shims, their `agents/` twins and `alab-` aliases, the
//! pending stubs, the reroute stubs, the tombstones: [`write_in_process`], the one lay path.
//!
//! A file a provenance-tracked process writes carries `com.apple.provenance`, and a tagged
//! `#!/bin/sh` shim tracks the tool it execs (law m21, [`crate::provenance`]). That is not
//! answered here: every store-changing door ends with [`crate::provenance::heal_store`],
//! which clears the tag in place. Until 2026-09-24 a tracked process laid these through an
//! untracked launchd job instead; that lane was deleted once the heal made it redundant.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

/// One executable to lay: where, and what bytes. Always mode `0755`, always temp+rename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Executable {
    /// The destination path.
    pub path: PathBuf,
    /// The file's bytes.
    pub body: Vec<u8>,
}

impl Executable {
    /// An executable at `path` with `body`.
    pub fn new(path: impl Into<PathBuf>, body: impl Into<Vec<u8>>) -> Self {
        Self {
            path: path.into(),
            body: body.into(),
        }
    }
}

/// Write ONE executable: a dotted sibling temp (`.<name>.lay-<pid>`, never swept by the
/// reroute dir's walk), the bytes, mode `0755` set explicitly (so a restrictive umask cannot
/// lay a shim only its owner can run in a system prefix), fsync, then `rename(2)` over the
/// destination — a shim on the user's PATH is never briefly absent or half-written. The
/// discipline the shim, stub and tombstone writers share.
pub fn write_in_process(file: &Executable) -> io::Result<()> {
    let name = match crate::call1(Path::file_name, file.path.as_path()) {
        Some(name) => crate::call1(OsStr::to_str, name),
        None => None,
    }
    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "executable has no file name"))?;
    let mut tmp_name = String::from(".");
    tmp_name.push_str(name);
    tmp_name.push_str(".lay-");
    tmp_name.push_str(&crate::dec_u64(u64::from(std::process::id())));
    let tmp = file.path.with_file_name(tmp_name);
    let _ = std::fs::remove_file(&tmp);
    let written = (|| -> io::Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o755);
        }
        let mut f = opts.open(&tmp)?;
        use std::io::Write as _;
        f.write_all(&file.body)?;
        f.sync_all()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        }
        Ok(())
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    // Windows `rename` does not replace an existing file (the same brief window the
    // Windows shim writer documents; a shim there is per-user and serialized).
    #[cfg(windows)]
    let _ = std::fs::remove_file(&file.path);
    if let Err(e) = std::fs::rename(&tmp, &file.path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(label: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("atpkg-lay-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// The in-process writer: mode 0755, the exact bytes, an existing file replaced, no
    /// temp left behind.
    #[cfg(unix)]
    #[test]
    fn the_in_process_writer_lays_0755_atomically() {
        use std::os::unix::fs::PermissionsExt as _;
        let d = tmp("write");
        let dest = d.join("tool");
        std::fs::write(&dest, "old\n").unwrap();
        let f = Executable::new(&dest, "#!/bin/sh\necho new\n");
        write_in_process(&f).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), f.body);
        assert_eq!(
            std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
            0o755
        );
        let leftovers: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with('.'))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
