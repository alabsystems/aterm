// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::path::{Path, PathBuf};

use atpkg::extract::{Container, ExtractOptions, SymlinkPolicy};

use super::{Host, Spawn, Stamp};

/// The prefix of every row's scratch under a cache root.
pub const SCRATCH_PREFIX: &str = aterm_uds::host::TOOLING_SCRATCH;

/// The most an unpacked tree may hold: twice the largest any row has measured (pwsh's
/// 193 MB), with room for upstream to grow.
const UNPACKED_CAP: u64 = 2 << 30;

/// A row's scratch, removed on drop unless an unproven copied program retains it.
pub(super) struct Scratch {
    dir: aterm_tempfile::TempDir,
}

impl Scratch {
    /// A new `.provision-<row>-*` directory under `root`, made first.
    pub(super) fn new(root: &Path, row: &str) -> Result<Self, String> {
        std::fs::create_dir_all(root)
            .map_err(|e| format!("could not create {}: {e}", root.display()))?;
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("{SCRATCH_PREFIX}{row}-"))
            .tempdir_in(root)
            .map_err(|e| format!("could not create a scratch under {}: {e}", root.display()))?;
        Ok(Self { dir })
    }

    pub(super) fn path(&self) -> &Path {
        self.dir.path()
    }

    /// A new, empty directory `name` in the scratch.
    pub(super) fn fresh(&self, name: &str) -> Result<PathBuf, String> {
        let dir = self.path().join(name);
        std::fs::create_dir(&dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        Ok(dir)
    }

    /// Run a program copied into this scratch. A nonempty stdout response proves it ran
    /// from the copy; only then may cleanup or placement rename its image. A spawn,
    /// read or wait error cannot establish that proof, and an unwind must retain it too.
    pub(super) fn run_copied(
        self,
        host: &dyn Host,
        spawn: &Spawn,
    ) -> Result<(Self, String), String> {
        let path = self.path().to_path_buf();
        let mut retained = RetainedScratch(Some(self));
        let stdout = host.run(spawn).map_err(|e| {
            format!(
                "{e}; keeping the unproven program's scratch at {}",
                path.display()
            )
        })?;
        if stdout.trim().is_empty() {
            return Err(format!(
                "{} produced no stdout proving it ran; keeping its scratch at {}",
                spawn.program.display(),
                path.display()
            ));
        }
        let scratch = retained.0.take().expect("the executing scratch is owned");
        Ok((scratch, stdout))
    }
}

/// Retention is armed before execution. Only an observed stdout response takes the
/// scratch back; every other return or unwind leaves the actual copied image in place.
struct RetainedScratch(Option<Scratch>);

impl Drop for RetainedScratch {
    fn drop(&mut self) {
        if let Some(scratch) = self.0.take() {
            let _ = scratch.dir.keep();
        }
    }
}

/// The last path segment of `url`, the name its download takes: `archive` where it has
/// none a file may carry.
fn file_name(url: &str) -> String {
    let last = url.rsplit('/').next().unwrap_or_default();
    if last.is_empty()
        || last.starts_with('.')
        || !last
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
    {
        "archive".to_string()
    } else {
        last.to_string()
    }
}

/// Download `url` into a new empty directory of the scratch and hold it to its pin: the
/// size, then the sha256. The path of the checked file.
pub(super) fn fetch_checked(
    host: &dyn Host,
    scratch: &Scratch,
    url: &str,
    sha256: &str,
    bytes: u64,
    say: &mut dyn FnMut(&str),
) -> Result<PathBuf, String> {
    let dir = scratch.fresh("download")?;
    let file = dir.join(file_name(url));
    say(&format!(
        "downloading {url} ({:.1} MB)",
        bytes as f64 / 1_000_000.0
    ));
    host.fetch(url, &file, bytes)?;
    let got = std::fs::metadata(&file)
        .map_err(|e| format!("{url} left no file: {e}"))?
        .len();
    if got != bytes {
        return Err(format!(
            "{url} is {got} bytes, the pin is {bytes}: the archive changed under its version; \
             nothing was unpacked"
        ));
    }
    let hash = crate::perf::arena::sha256_file(&file)?;
    if hash != sha256 {
        return Err(format!(
            "{url} hashes to {hash}, the pin is {sha256}: the archive changed under its version; \
             nothing was unpacked"
        ));
    }
    Ok(file)
}

/// The container `url` names by its suffix.
pub(super) fn container_of(url: &str) -> Result<Container, String> {
    if url.ends_with(".tar.gz") || url.ends_with(".tgz") {
        Ok(Container::TarGz)
    } else if url.ends_with(".tar") {
        Ok(Container::Tar)
    } else if url.ends_with(".zip") {
        Ok(Container::Zip)
    } else {
        Err(format!("{url} names no container atpkg's extractor reads"))
    }
}

/// Lay the checked `archive` into a new empty directory `name` of the scratch through
/// atpkg's vetted extractor: each member's path held beneath it, links only within it,
/// modes by `atpkg::extract::safe_mode` (0755 for a directory or anything executable,
/// else 0644).
pub(super) fn unpack(
    scratch: &Scratch,
    archive: &Path,
    container: Container,
    strip: u32,
    name: &str,
) -> Result<PathBuf, String> {
    let tree = scratch.fresh(name)?;
    let opts = ExtractOptions {
        strip_components: strip,
        symlinks: SymlinkPolicy::InRoot,
        allow_global_pax: false,
    };
    atpkg::extract::extract_tree(archive, &tree, container, UNPACKED_CAP, opts)
        .map_err(|e| format!("{} did not unpack: {e}", archive.display()))?;
    Ok(tree)
}

/// A `.tar.xz` rewritten as a plain tar for atpkg's tar reader, by bsdtar (KEEP-OS, with
/// liblzma): `tar -c -f <out> @<archive>` copies the archive's members into a new archive
/// without writing one of them to disk, so nothing of the tree exists until atpkg lays
/// it. There is no xz decoder in house.
pub(super) fn plain_tar(
    host: &dyn Host,
    scratch: &Scratch,
    archive: &Path,
) -> Result<PathBuf, String> {
    let out = scratch.fresh("plain")?.join("source.tar");
    let mut at = std::ffi::OsString::from("@");
    at.push(archive);
    host.run(&Spawn {
        program: PathBuf::from(TAR),
        args: vec!["-c".into(), "-f".into(), out.clone().into_os_string(), at],
        cwd: scratch.path().to_path_buf(),
        set: Vec::new(),
        remove: Vec::new(),
    })
    .map_err(|e| format!("bsdtar could not rewrite {}: {e}", archive.display()))?;
    Ok(out)
}

/// The OS's tar (bsdtar on macOS), by absolute path.
pub(super) const TAR: &str = "/usr/bin/tar";

/// Whether `path` is a regular file with an execute bit.
pub(super) fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::symlink_metadata(path)
        .is_ok_and(|m| m.file_type().is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Refuse `tree` if any link in it points outside it, to nothing, or round a loop, each
/// resolved the way the kernel will follow it (`ditto` and `unzip` lay whatever links a
/// zip holds). Nothing in the tree is read but its names and links.
pub(super) fn links_within(tree: &Path) -> Result<(), String> {
    let root = std::fs::canonicalize(tree)
        .map_err(|e| format!("could not resolve {}: {e}", tree.display()))?;
    let mut dirs = vec![tree.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let entries = std::fs::read_dir(&dir)
            .map_err(|e| format!("could not list {}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("could not list {}: {e}", dir.display()))?;
            let path = entry.path();
            let kind = entry
                .file_type()
                .map_err(|e| format!("could not read {}: {e}", path.display()))?;
            if kind.is_dir() {
                dirs.push(path);
            } else if kind.is_symlink() {
                let shown = path
                    .strip_prefix(tree)
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                match std::fs::canonicalize(&path) {
                    Ok(real) if real.starts_with(&root) => {}
                    Ok(real) => {
                        return Err(format!(
                            "the archive's link {shown} leads outside its tree, to {}: refused",
                            real.display()
                        ));
                    }
                    Err(e) => {
                        return Err(format!(
                            "the archive's link {shown} leads to nothing it holds ({e}): refused"
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Write `stamp` into `tree` as a new `stamp.json`: whatever an archive laid under that
/// name (a file, or a link that would carry the write elsewhere) is removed first, and the
/// stamp is created, never written through.
pub(super) fn write_stamp(tree: &Path, stamp: &Stamp) -> Result<(), String> {
    use std::io::Write as _;
    let json = aterm_json::to_string(stamp).map_err(|e| format!("stamp: {e}"))?;
    let path = tree.join(super::STAMP);
    if std::fs::symlink_metadata(&path).is_ok() {
        std::fs::remove_file(&path)
            .map_err(|e| format!("could not replace {}: {e}", path.display()))?;
    }
    std::fs::File::options()
        .write(true)
        .create_new(true)
        .open(&path)
        .and_then(|mut f| f.write_all(format!("{json}\n").as_bytes()))
        .map_err(|e| format!("could not write {}: {e}", path.display()))
}

/// The stamp in `dir`, where one reads.
pub(super) fn read_stamp(dir: &Path) -> Option<Stamp> {
    let text = std::fs::read_to_string(dir.join(super::STAMP)).ok()?;
    aterm_json::from_str(&text).ok()
}

/// Mark `dir`'s stamp used now, so the reclaim's idle rule counts a run that found the
/// row in place as use. Only a stamp that is a file: a link is never followed.
pub(super) fn touch_stamp(dir: &Path) {
    let path = dir.join(super::STAMP);
    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file())
        && let Ok(file) = std::fs::File::options().append(true).open(&path)
    {
        let _ = file.set_modified(std::time::SystemTime::now());
    }
}

/// Put the finished `tree` at `dest` with one rename. Whatever stands at `dest` (a copy
/// with no stamp, or a stamp of another pin) is renamed aside into the scratch first and
/// goes with it. A rename that fails because another run placed the same pin meanwhile is
/// no failure when `complete()` (of `dest`) holds.
pub(super) fn place(
    scratch: &Scratch,
    tree: &Path,
    dest: &Path,
    complete: &dyn Fn() -> bool,
) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    if std::fs::symlink_metadata(dest).is_ok() {
        let aside = scratch.path().join("replaced");
        if let Err(e) = std::fs::rename(dest, &aside)
            && std::fs::symlink_metadata(dest).is_ok()
        {
            return Err(format!("could not move {} aside: {e}", dest.display()));
        }
    }
    match std::fs::rename(tree, dest) {
        Ok(()) => Ok(()),
        Err(_) if complete() => Ok(()),
        Err(e) => Err(format!("could not place {}: {e}", dest.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_download_is_named_by_its_urls_last_segment_or_archive() {
        assert_eq!(
            file_name("https://x/releases/download/4.9.3/fish-4.9.3.tar.xz"),
            "fish-4.9.3.tar.xz"
        );
        for odd in [
            "https://x/",
            "https://x/..",
            "https://x/a b",
            "https://x/.hidden",
        ] {
            assert_eq!(file_name(odd), "archive", "{odd}");
        }
        assert_eq!(container_of("a.tar.gz"), Ok(Container::TarGz));
        assert_eq!(container_of("a.zip"), Ok(Container::Zip));
        assert!(
            container_of("a.tar.xz").is_err(),
            "xz goes through plain_tar"
        );
    }
}
