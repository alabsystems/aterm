// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The bundle copy the mug runs from (see the crate docs, "Where it runs"): `serve` started
//! from `X.app/Contents/MacOS/aterm` copies `X.app` whole into
//! [`crate::paths::Paths::code_dir`] and re-executes from the copy with [`ORIGIN_ENV`] naming
//! where it started, so LaunchServices never takes the mug for the app an open of `X.app`
//! names. The model is `aterm_keeper::code` (the keeper runs from its own copy for another
//! reason); this one is simpler, as one mug runs at a time.
//!
//! The copy is `<code dir>/<label>/X.app`, made as `<label>.partial` and renamed whole, so a
//! half-made copy is never run. Only the holder of the one-mug lock ([`crate::instance`])
//! makes one, so it removes everything else in the directory first: an earlier copy, a half
//! made one, another build's. Files are copied with `std::fs::copy` (an APFS clone, the mode
//! kept), symbolic links recreated as they read, and anything else refused, so the bundle's
//! signature still seals the copy.

use std::io;
use std::path::{Path, PathBuf};

/// The variable naming the executable the mug was started as, set when it re-executes from
/// its copy: the job, its label and the update watch are that executable's.
pub const ORIGIN_ENV: &str = "ATERM_CAFFEINE_ORIGIN";

/// The largest `Info.plist` read: aterm's is a few KiB.
const PLIST_CAP: u64 = 1 << 20;

/// The `.app` bundle `exe` is the main executable of (`X.app/Contents/MacOS/<exe>`), if it is
/// one.
#[must_use]
pub fn own_bundle(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    let is_bundle = macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension().is_some_and(|e| e == "app");
    is_bundle.then(|| bundle.to_path_buf())
}

/// This process's executable, canonical.
fn own_exe() -> Option<PathBuf> {
    std::env::current_exe().ok()?.canonicalize().ok()
}

/// The executable the mug was started as: [`ORIGIN_ENV`] when this process runs from a copy
/// under `code_dir`, else this process's own (canonical). `None` when it runs from a copy and
/// [`ORIGIN_ENV`] names no absolute path (nothing says whose job it is), or this process's
/// path cannot be read.
#[must_use]
pub fn origin_exe(code_dir: &Path) -> Option<PathBuf> {
    if runs_from_copy(code_dir) {
        std::env::var_os(ORIGIN_ENV)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    } else {
        own_exe()
    }
}

/// Whether this process runs from a copy under `code_dir`.
#[must_use]
pub fn runs_from_copy(code_dir: &Path) -> bool {
    match (own_exe(), code_dir.canonicalize()) {
        (Some(exe), Ok(dir)) => is_under(&exe, &dir),
        _ => false,
    }
}

/// Whether `exe` is inside `dir` (both canonical), not `dir` itself.
fn is_under(exe: &Path, dir: &Path) -> bool {
    exe.strip_prefix(dir)
        .is_ok_and(|rest| rest.components().next().is_some())
}

/// Copy the bundle `origin` belongs to into `code_dir` (replacing an earlier copy for
/// `label`) and return the copy's executable; `Ok(None)` when `origin` is not in a bundle
/// (a build tree's executable runs where it is).
///
/// Only the holder of the one-mug lock calls this: it removes everything else in `code_dir`
/// first, and makes `code_dir` (`0700`) when it is missing.
///
/// # Errors
///
/// The copy cannot be made: `label` is not a plain name, `code_dir` is not a directory of
/// this user's, an entry in it cannot be removed, or the bundle holds something other than
/// files, directories and links, or cannot be read.
pub fn prepare_copy(origin: &Path, code_dir: &Path, label: &str) -> io::Result<Option<PathBuf>> {
    let Some(bundle) = own_bundle(origin) else {
        return Ok(None);
    };
    #[cfg(unix)]
    {
        copy_bundle(origin, &bundle, code_dir, label).map(Some)
    }
    #[cfg(not(unix))]
    {
        let _ = (bundle, code_dir, label);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "a bundle copy is made on macOS only",
        ))
    }
}

/// Whether `label` can name an entry of the code directory: one plain path component.
#[cfg(unix)]
fn plain_name(label: &str) -> bool {
    !label.is_empty()
        && label != "."
        && label != ".."
        && !label.contains(['/', '\0'])
        && !label.ends_with(".partial")
}

#[cfg(unix)]
fn copy_bundle(origin: &Path, bundle: &Path, code_dir: &Path, label: &str) -> io::Result<PathBuf> {
    if !plain_name(label) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{label:?} cannot name a copy"),
        ));
    }
    let inner = bundle
        .parent()
        .and_then(|p| origin.strip_prefix(p).ok())
        .ok_or_else(|| io::Error::other("the executable is not inside its bundle"))?
        .to_path_buf();
    let top = bundle
        .file_name()
        .ok_or_else(|| io::Error::other("the bundle has no name"))?;
    private_dir(code_dir)?;
    sweep(code_dir)?;
    let home = code_dir.join(label);
    let partial = code_dir.join(format!("{label}.partial"));
    let made = std::fs::create_dir(&partial)
        .and_then(|()| copy_tree(bundle, &partial.join(top)))
        .and_then(|()| std::fs::rename(&partial, &home));
    if let Err(e) = made {
        let _ = std::fs::remove_dir_all(&partial);
        return Err(io::Error::new(
            e.kind(),
            format!(
                "copying {} into {}: {e}",
                bundle.display(),
                code_dir.display()
            ),
        ));
    }
    Ok(home.join(inner))
}

/// Make `dir` (and its parents) when missing, and hold it to `0700`; refuse a link, a
/// non-directory or another user's directory at its name, before anything in it is swept.
#[cfg(unix)]
fn private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a directory", dir.display()),
        ));
    }
    if meta.uid() != aterm_uds::peer::our_uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not this user's directory", dir.display()),
        ));
    }
    if meta.permissions().mode() & 0o777 != 0o700 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(target_os = "macos")]
    exclude_from_backup(dir);
    Ok(())
}

/// The attribute Time Machine (and every backup tool that keeps the convention) reads to
/// leave an item out.
#[cfg(target_os = "macos")]
const BACKUP_EXCLUDE: &std::ffi::CStr = c"com.apple.metadata:com_apple_backup_excludeItem";

/// Its value: `true` as a binary plist (header, the `true` marker, the offset table and the
/// trailer), atpkg's bytes for its store.
#[cfg(target_os = "macos")]
const TRUE_BPLIST: &[u8] = &[
    0x62, 0x70, 0x6c, 0x69, 0x73, 0x74, 0x30, 0x30, // "bplist00"
    0x09, // kCFBinaryPlistMarkerTrue
    0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x09,
];

/// Whether `dir` carries [`BACKUP_EXCLUDE`] set to [`TRUE_BPLIST`].
#[cfg(target_os = "macos")]
// Skip: bottoms out at the `getxattr` FFI call.
#[cfg_attr(trust_verify, trust::skip)]
fn backup_excluded(dir: &std::ffi::CStr) -> bool {
    let mut have = [0u8; 64];
    // SAFETY: `dir` and the name are NUL-terminated strings live for the call, and `have` is
    // a writable buffer of the length passed; `getxattr` writes at most that many bytes and
    // answers how many, or -1.
    let n = unsafe {
        libc::getxattr(
            dir.as_ptr(),
            BACKUP_EXCLUDE.as_ptr(),
            have.as_mut_ptr().cast(),
            have.len(),
            0,
            0,
        )
    };
    usize::try_from(n).is_ok_and(|n| have.get(..n) == Some(TRUE_BPLIST))
}

/// Keep the code directory out of backups: every serve from a bundle makes its copy afresh,
/// new files that a backup would copy again each time, of code rebuilt on demand (`.noindex`
/// keeps Spotlight out, not Time Machine). Read before written, as atpkg does: writing the
/// same value again still moves the directory's ctime. Best effort: a volume without
/// extended attributes costs backup space and nothing else.
#[cfg(target_os = "macos")]
// Skip: bottoms out at the `setxattr` FFI call.
#[cfg_attr(trust_verify, trust::skip)]
fn exclude_from_backup(dir: &Path) {
    use std::os::unix::ffi::OsStrExt as _;
    let Ok(name) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
        return;
    };
    if backup_excluded(&name) {
        return;
    }
    // SAFETY: both strings are NUL-terminated and live for the call, and the value is a
    // `'static` slice of the length passed; the answer is ignored (best effort, above).
    unsafe {
        libc::setxattr(
            name.as_ptr(),
            BACKUP_EXCLUDE.as_ptr(),
            TRUE_BPLIST.as_ptr().cast(),
            TRUE_BPLIST.len(),
            0,
            0,
        );
    }
}

/// Remove every entry of `dir`: a link or a file as itself, a directory with all it holds.
#[cfg(unix)]
fn sweep(dir: &Path) -> io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let removed = if entry.file_type()?.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        removed.map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))?;
    }
    Ok(())
}

/// Copy `src` to `dst`: directories recreated (their mode set once filled), symbolic links
/// recreated as they read, files copied (a clone on APFS) with their mode; anything else is
/// refused.
#[cfg(unix)]
fn copy_tree(src: &Path, dst: &Path) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    let kind = meta.file_type();
    if kind.is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(src)?, dst)
    } else if kind.is_dir() {
        std::fs::create_dir(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_tree(&entry.path(), &dst.join(entry.file_name()))?;
        }
        std::fs::set_permissions(dst, meta.permissions())
    } else if kind.is_file() {
        std::fs::copy(src, dst).map(drop)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a file, a directory or a link", src.display()),
        ))
    }
}

/// The bundle identifier `bundle`'s `Contents/Info.plist` declares (`CFBundleIdentifier`, a
/// `<string>`, its XML entities read); `None` when the file is missing, not an XML plist, or
/// declares none.
#[must_use]
pub fn bundle_identifier(bundle: &Path) -> Option<String> {
    let bytes = read_capped(&bundle.join("Contents").join("Info.plist"))?;
    plist_string(std::str::from_utf8(&bytes).ok()?, "CFBundleIdentifier")
}

/// The `<string>` value of `key` in a plist's text: the first `<key>` of that name, then (past
/// whitespace) a `<string>` element. `None` for another kind of value or an empty one.
fn plist_string(text: &str, key: &str) -> Option<String> {
    let tag = format!("<key>{key}</key>");
    let at = text.find(&tag)? + tag.len();
    let rest = text[at..].trim_start().strip_prefix("<string>")?;
    let value = &rest[..rest.find("</string>")?];
    if value.contains('<') {
        return None;
    }
    let value = xml_unescape(value);
    (!value.is_empty()).then_some(value)
}

/// The `<string>` values of the `<array>` that is `key`'s value in a plist's text: the first
/// `<key>` of that name, then (past whitespace) an array of strings only. `None` for another
/// kind of value, or an array holding anything but strings.
#[must_use]
pub(crate) fn plist_strings(text: &str, key: &str) -> Option<Vec<String>> {
    let tag = format!("<key>{key}</key>");
    let at = text.find(&tag)? + tag.len();
    let mut rest = text[at..].trim_start().strip_prefix("<array>")?;
    let mut values = Vec::new();
    loop {
        rest = rest.trim_start();
        if rest.starts_with("</array>") {
            return Some(values);
        }
        rest = rest.strip_prefix("<string>")?;
        let end = rest.find("</string>")?;
        let value = &rest[..end];
        if value.contains('<') {
            return None;
        }
        values.push(xml_unescape(value));
        rest = &rest[end + "</string>".len()..];
    }
}

/// The text of the plist at `path`, when it is UTF-8 and at most [`PLIST_CAP`] bytes.
#[must_use]
pub(crate) fn read_plist(path: &Path) -> Option<String> {
    String::from_utf8(read_capped(path)?).ok()
}

/// The five predefined XML entities, read; `&amp;` last, so `&amp;lt;` reads as `&lt;`.
fn xml_unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The whole file at `path`, when it is at most [`PLIST_CAP`] bytes.
fn read_capped(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(PLIST_CAP + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= PLIST_CAP).then_some(bytes)
}

/// What the update watch compares to tell that the code the mug was started from changed: the
/// origin bundle's `Contents/Info.plist` bytes (an update replaces the bundle, and its version
/// with it), or for a bare executable its length and modification time in nanoseconds.
/// `None` when it cannot be read (in the middle of an update's swap, say, or something other
/// than a file is at a bare executable's path): no reading, no verdict.
#[must_use]
pub fn code_fingerprint(origin_exe: &Path) -> Option<Vec<u8>> {
    if let Some(bundle) = own_bundle(origin_exe) {
        return read_capped(&bundle.join("Contents").join("Info.plist"));
    }
    let meta = std::fs::metadata(origin_exe)
        .ok()
        .filter(std::fs::Metadata::is_file)?;
    let modified = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    let mut print = meta.len().to_le_bytes().to_vec();
    print.extend_from_slice(&modified.to_le_bytes());
    Some(print)
}

/// The command that becomes `copy` with this process's arguments (its `argv[0]` too) and
/// [`ORIGIN_ENV`] naming `origin`.
#[cfg(unix)]
fn reexec_command(copy: &Path, origin: &Path) -> aterm_uds::spawnfd::Command {
    let mut args = std::env::args_os();
    let mut cmd = aterm_uds::spawnfd::command(copy);
    if let Some(arg0) = args.next() {
        cmd.arg0(arg0);
    }
    cmd.args(args).env(ORIGIN_ENV, origin);
    cmd
}

/// Become `copy` with this process's arguments and [`ORIGIN_ENV`] naming `origin`. Returns
/// only the error of a failed `exec`, which leaves this process as it was.
///
/// The new image keeps the pid and the stdio (launchd's log) and nothing else: no lock goes
/// with it, so it takes the one-mug lock afresh. [`reexec_holding`] hands the lock over.
#[must_use]
pub fn reexec(copy: &Path, origin: &Path) -> io::Error {
    #[cfg(unix)]
    {
        reexec_command(copy, origin).exec()
    }
    #[cfg(not(unix))]
    {
        let _ = (copy, origin);
        io::Error::new(io::ErrorKind::Unsupported, "a re-exec is Unix only")
    }
}

/// [`reexec`], handing `instance`'s lock to the new image on its stdin
/// ([`crate::instance::Instance::handover`]) so no moment passes without it held; the copy
/// takes it back with [`crate::instance::Instance::adopt`]. Returns only the error of a failed
/// `exec` (this process then still holds the lock as it did).
#[must_use]
pub fn reexec_holding(
    copy: &Path,
    origin: &Path,
    instance: &crate::instance::Instance,
) -> io::Error {
    #[cfg(unix)]
    {
        let lock = match instance.handover() {
            Ok(lock) => lock,
            Err(e) => return e,
        };
        let mut cmd = reexec_command(copy, origin);
        cmd.stdin(lock).env(
            crate::instance::LOCK_FD_ENV,
            crate::instance::HANDOVER_FD.to_string(),
        );
        cmd.exec()
    }
    #[cfg(not(unix))]
    {
        let _ = (copy, origin, instance);
        io::Error::new(io::ErrorKind::Unsupported, "a re-exec is Unix only")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundle_is_read_off_the_executable_path() {
        assert_eq!(
            own_bundle(Path::new("/Applications/aterm.app/Contents/MacOS/aterm")),
            Some(PathBuf::from("/Applications/aterm.app"))
        );
        assert_eq!(
            own_bundle(Path::new("/x/aterm (dev).app/Contents/MacOS/aterm")),
            Some(PathBuf::from("/x/aterm (dev).app"))
        );
        for bare in [
            "/Users//x/target/debug/aterm",
            "/x/Contents/MacOS/aterm",
            "/x/a.app/Contents/aterm",
            "/x/a.app/MacOS/aterm",
            "/x/a.bundle/Contents/MacOS/aterm",
            "/x/a.app/Contents/MacOS/sub/aterm",
            "aterm",
            "/",
        ] {
            assert_eq!(own_bundle(Path::new(bare)), None, "{bare}");
        }
    }

    #[test]
    fn only_a_path_inside_the_directory_is_under_it() {
        let dir = Path::new("/s/caffeine-code.noindex");
        assert!(is_under(
            Path::new("/s/caffeine-code.noindex/l/a.app/Contents/MacOS/aterm"),
            dir
        ));
        assert!(!is_under(dir, dir));
        assert!(!is_under(
            Path::new("/s/caffeine-code.noindex2/l/aterm"),
            dir
        ));
        assert!(!is_under(Path::new("/Applications/aterm.app"), dir));
    }

    #[cfg(unix)]
    #[test]
    fn a_label_names_a_copy_only_as_one_plain_component() {
        assert!(plain_name("com.aterm.aterm.caffeine"));
        assert!(plain_name("com.aterm.aterm.caffeine.dev.0123abcd"));
        for bad in ["", ".", "..", "a/b", "/a", "a\0b", "x.partial"] {
            assert!(!plain_name(bad), "{bad:?}");
        }
    }

    /// A stand-in bundle: nested directories, an executable (0755), a read-only file (0444),
    /// a relative symbolic link and an absolute one, and an `Info.plist`.
    #[cfg(unix)]
    fn stand_in_bundle(root: &Path, id: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let app = root.join("Fake (dev).app");
        let macos = app.join("Contents").join("MacOS");
        let res = app
            .join("Contents")
            .join("Resources")
            .join("deep")
            .join("er");
        std::fs::create_dir_all(&macos).expect("macos");
        std::fs::create_dir_all(&res).expect("resources");
        std::fs::write(
            app.join("Contents").join("Info.plist"),
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n\
                 \t<key>CFBundleIdentifier</key>\n\t<string>{id}</string>\n</dict>\n</plist>\n"
            ),
        )
        .expect("plist");
        let exe = macos.join("aterm");
        std::fs::write(&exe, "#!/bin/sh\nexit 0\n").expect("exe");
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let ro = res.join("ro.txt");
        std::fs::write(&ro, "read only").expect("ro");
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o444)).expect("chmod");
        std::os::unix::fs::symlink("aterm", macos.join("aterm-alias")).expect("alias");
        std::os::unix::fs::symlink("../../MacOS/aterm", res.join("up")).expect("up");
        std::os::unix::fs::symlink("/nonexistent/abs", res.join("abs")).expect("abs");
        app
    }

    /// `a` and `b` hold the same tree: the same names, kinds, file bytes and modes, and link
    /// targets.
    #[cfg(unix)]
    fn assert_same_tree(a: &Path, b: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        let ma = std::fs::symlink_metadata(a).expect("a");
        let mb = std::fs::symlink_metadata(b).expect("b");
        assert_eq!(ma.file_type(), mb.file_type(), "{}", b.display());
        if ma.file_type().is_symlink() {
            assert_eq!(
                std::fs::read_link(a).expect("a"),
                std::fs::read_link(b).expect("b")
            );
            return;
        }
        assert_eq!(
            ma.permissions().mode() & 0o7777,
            mb.permissions().mode() & 0o7777,
            "{}",
            b.display()
        );
        if ma.is_file() {
            assert_eq!(std::fs::read(a).expect("a"), std::fs::read(b).expect("b"));
            return;
        }
        let names = |dir: &Path| {
            let mut names: Vec<_> = std::fs::read_dir(dir)
                .expect("list")
                .map(|e| e.expect("entry").file_name())
                .collect();
            names.sort();
            names
        };
        let (na, nb) = (names(a), names(b));
        assert_eq!(na, nb, "{}", b.display());
        for name in na {
            assert_same_tree(&a.join(&name), &b.join(&name));
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_bundle_is_copied_exactly_and_replaces_every_other_entry() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let app = stand_in_bundle(root.path(), "com.example.fake");
        let origin = app.join("Contents").join("MacOS").join("aterm");
        let code = root.path().join("state").join("caffeine-code.noindex");
        // A directory that exists with a looser mode is held to 0700.
        std::fs::create_dir_all(&code).expect("code");
        std::fs::set_permissions(&code, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        // Leftovers: an earlier copy of this label, a half-made one, another build's copy, a
        // stray file and a link (removed as itself, its target kept).
        let target = root.path().join("kept");
        std::fs::create_dir_all(&target).expect("target");
        for dir in ["lbl", "lbl.partial", "com.other"] {
            std::fs::create_dir_all(code.join(dir).join("Old.app")).expect("old");
        }
        std::fs::write(code.join(".DS_Store"), "x").expect("stray");
        std::os::unix::fs::symlink(&target, code.join("link")).expect("link");

        let copy = prepare_copy(&origin, &code, "lbl")
            .expect("copy")
            .expect("a bundle");
        assert_eq!(
            copy,
            code.join("lbl")
                .join("Fake (dev).app")
                .join("Contents")
                .join("MacOS")
                .join("aterm")
        );
        assert_same_tree(&app, &code.join("lbl").join("Fake (dev).app"));
        let entries: Vec<_> = std::fs::read_dir(&code)
            .expect("list")
            .map(|e| e.expect("entry").file_name())
            .collect();
        assert_eq!(entries, [std::ffi::OsString::from("lbl")]);
        assert!(target.is_dir(), "a link's target is not swept");
        let mode = std::fs::metadata(&code).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        // Kept out of backups: a copy made afresh at every serve is no backup's business.
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::ffi::OsStrExt as _;
            let name = std::ffi::CString::new(code.as_os_str().as_bytes()).expect("no NUL");
            assert!(backup_excluded(&name), "{}", code.display());
        }

        // A second prepare replaces the first copy: a file added to it is gone.
        std::fs::write(code.join("lbl").join("marker"), "m").expect("marker");
        std::fs::write(app.join("Contents").join("new.txt"), "new").expect("new");
        let again = prepare_copy(&origin, &code, "lbl")
            .expect("copy")
            .expect("a bundle");
        assert_eq!(again, copy);
        assert!(!code.join("lbl").join("marker").exists());
        assert_same_tree(&app, &code.join("lbl").join("Fake (dev).app"));
        assert!(!code.join("lbl.partial").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_bundle_with_a_fifo_is_refused_and_leaves_no_partial_copy() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let app = stand_in_bundle(root.path(), "com.example.fake");
        let fifo = app.join("Contents").join("pipe");
        let made = aterm_uds::spawnfd::command("/usr/bin/mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo");
        assert!(made.success());
        let code = root.path().join("code");
        let origin = app.join("Contents").join("MacOS").join("aterm");
        let err = prepare_copy(&origin, &code, "lbl").expect_err("a fifo");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{err}");
        let entries: Vec<_> = std::fs::read_dir(&code).expect("list").collect();
        assert!(entries.is_empty(), "{entries:?}");
    }

    /// A link at the code directory's name is refused before the sweep: what it points at
    /// keeps every entry, and nothing is copied through it.
    #[cfg(unix)]
    #[test]
    fn a_link_at_the_code_directory_is_refused_before_anything_is_swept() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let app = stand_in_bundle(root.path(), "com.example.fake");
        let origin = app.join("Contents").join("MacOS").join("aterm");
        let elsewhere = root.path().join("elsewhere");
        std::fs::create_dir_all(elsewhere.join("keep")).expect("elsewhere");
        std::fs::write(elsewhere.join("file"), "kept").expect("file");
        let code = root.path().join("code");
        std::os::unix::fs::symlink(&elsewhere, &code).expect("link");
        let err = prepare_copy(&origin, &code, "lbl").expect_err("a link");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{err}");
        let mut names: Vec<_> = std::fs::read_dir(&elsewhere)
            .expect("list")
            .map(|e| e.expect("entry").file_name())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [std::ffi::OsString::from("file"), "keep".into()],
            "nothing swept"
        );
        // A plain file at the name is refused the same way.
        let file = root.path().join("code-file");
        std::fs::write(&file, "x").expect("file");
        assert!(prepare_copy(&origin, &file, "lbl").is_err());
        assert_eq!(std::fs::read_to_string(&file).expect("read"), "x");
    }

    #[test]
    fn a_bare_executable_runs_where_it_is() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let exe = root.path().join("target").join("debug").join("aterm");
        let code = root.path().join("code");
        assert!(matches!(prepare_copy(&exe, &code, "lbl"), Ok(None)));
        assert!(!code.exists(), "nothing is made for a bare executable");
    }

    #[cfg(unix)]
    #[test]
    fn a_label_that_is_not_a_plain_name_is_refused() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let app = stand_in_bundle(root.path(), "com.example.fake");
        let origin = app.join("Contents").join("MacOS").join("aterm");
        for bad in ["", "..", "a/b"] {
            assert!(
                prepare_copy(&origin, &root.path().join("code"), bad).is_err(),
                "{bad:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_bundle_identifier_is_read_off_its_info_plist() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let app = stand_in_bundle(root.path(), "com.example.a&amp;b");
        assert_eq!(bundle_identifier(&app).as_deref(), Some("com.example.a&b"));
        assert_eq!(bundle_identifier(&root.path().join("Missing.app")), None);
    }

    #[test]
    fn a_plist_string_is_the_keys_string_value_only() {
        let plist = |body: &str| {
            format!(
                "<?xml version=\"1.0\"?>\n<plist version=\"1.0\">\n<dict>\n{body}</dict>\n</plist>\n"
            )
        };
        let id = |body: &str| plist_string(&plist(body), "CFBundleIdentifier");
        assert_eq!(
            id("\t<key>CFBundleIdentifier</key>\n\t<string>com.aterm.aterm</string>\n").as_deref(),
            Some("com.aterm.aterm")
        );
        assert_eq!(
            id("<key>CFBundleIdentifier</key><string>a&lt;b&gt;&quot;c&apos;&amp;lt;</string>")
                .as_deref(),
            Some("a<b>\"c'&lt;")
        );
        // The first key of the name wins; another key's string is not it.
        assert_eq!(
            id("<key>CFBundleName</key><string>aterm</string>\
                <key>CFBundleIdentifier</key><string>x.y</string>")
            .as_deref(),
            Some("x.y")
        );
        for bad in [
            "",
            "<key>CFBundleName</key><string>aterm</string>",
            "<key>CFBundleIdentifier</key><integer>7</integer>",
            "<key>CFBundleIdentifier</key><string></string>",
            "<key>CFBundleIdentifier</key><string>unterminated",
            "<key>CFBundleIdentifier</key><array><string>x</string></array>",
            "<key>CFBundleIdentifier</key><string>a<b/>c</string>",
        ] {
            assert_eq!(id(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_plist_array_is_its_strings_only() {
        let job = crate::launchd::plist_text(
            "l",
            Path::new("/A&B <x>/aterm"),
            Path::new("/l.log"),
            Some("com.aterm.aterm"),
        );
        assert_eq!(
            plist_strings(&job, "ProgramArguments"),
            Some(vec![
                "/A&B <x>/aterm".to_string(),
                "caffeine".to_string(),
                "serve".to_string()
            ])
        );
        assert_eq!(
            plist_strings("<key>A</key> <array>\n</array>", "A"),
            Some(Vec::new())
        );
        for bad in [
            "<key>A</key><string>x</string>",
            "<key>A</key><array><string>x</string><integer>1</integer></array>",
            "<key>A</key><array><string>x</string>",
            "<key>A</key><array><string>x<y/></string></array>",
            "<key>B</key><array><string>x</string></array>",
        ] {
            assert_eq!(plist_strings(bad, "A"), None, "{bad:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_fingerprint_follows_the_info_plist_or_the_bare_executable() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let app = stand_in_bundle(root.path(), "com.example.fake");
        let origin = app.join("Contents").join("MacOS").join("aterm");
        let before = code_fingerprint(&origin).expect("print");
        assert_eq!(code_fingerprint(&origin), Some(before.clone()), "stable");
        // A change elsewhere in the bundle, its executable's included, is not an update.
        std::fs::write(app.join("Contents").join("other"), "x").expect("other");
        std::fs::write(&origin, "#!/bin/sh\nexit 1\n").expect("exe");
        assert_eq!(code_fingerprint(&origin), Some(before.clone()));
        let plist = app.join("Contents").join("Info.plist");
        let mut text = std::fs::read_to_string(&plist).expect("plist");
        text.push_str("<!-- 1.1 -->\n");
        std::fs::write(&plist, text).expect("plist");
        let after = code_fingerprint(&origin).expect("print");
        assert_ne!(after, before);
        // Too long to be an Info.plist.
        std::fs::write(
            &plist,
            vec![b'x'; usize::try_from(PLIST_CAP).expect("cap") + 1],
        )
        .expect("big");
        assert_eq!(code_fingerprint(&origin), None, "over the cap");
        std::fs::remove_file(&plist).expect("rm");
        assert_eq!(code_fingerprint(&origin), None, "mid-swap: no reading");

        let bare = root.path().join("aterm");
        assert_eq!(code_fingerprint(&bare), None);
        std::fs::write(&bare, "one").expect("bare");
        let one = code_fingerprint(&bare).expect("print");
        assert_eq!(one.len(), 8 + 16);
        std::fs::write(&bare, "three").expect("bare");
        let three = code_fingerprint(&bare).expect("print");
        assert_ne!(three, one, "the length changed");
        // The same length written at another time is a rebuild too.
        let later = std::fs::metadata(&bare)
            .and_then(|m| m.modified())
            .expect("mtime")
            + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&bare)
            .and_then(|f| f.set_modified(later))
            .expect("set mtime");
        assert_ne!(
            code_fingerprint(&bare).expect("print"),
            three,
            "the time changed"
        );
        // A directory where the executable was is not one.
        std::fs::remove_file(&bare).expect("rm");
        std::fs::create_dir(&bare).expect("mkdir");
        assert_eq!(code_fingerprint(&bare), None);
    }

    #[test]
    fn a_process_outside_the_code_directory_does_not_run_from_a_copy() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        assert!(!runs_from_copy(root.path()));
        assert!(!runs_from_copy(&root.path().join("missing")));
        assert_eq!(origin_exe(root.path()), own_exe());
    }

    /// The one-mug lock crosses the re-exec into the bundle copy: a process started from a
    /// stand-in bundle (this test binary as its executable) takes the lock, copies the bundle,
    /// and becomes the copy with the lock handed over on its stdin; the copy adopts it. The
    /// same pid on both sides, the lock held by it after the exec, the variable consumed and
    /// stdin back on `/dev/null`. No AppKit, no launchd.
    #[cfg(target_os = "macos")]
    mod handover {
        use std::path::{Path, PathBuf};
        use std::time::{Duration, Instant};

        use crate::code;
        use crate::instance::{self, Instance};

        /// Set only in the child this module starts: the scratch directory it works in.
        const DIR_ENV: &str = "ATERM_CAFFEINE_TEST_HANDOVER_DIR";

        /// The child's two halves, one test: inert in a normal run (no [`DIR_ENV`]). Started
        /// from the stand-in bundle it takes the lock and re-executes from its copy with the
        /// same arguments, so the copy runs this test again and adopts the lock.
        #[test]
        fn handover_child() {
            let Some(dir) = std::env::var_os(DIR_ENV).map(PathBuf::from) else {
                return;
            };
            let lock = dir.join("caffeine.lock");
            let code_dir = dir.join("code");
            if code::runs_from_copy(&code_dir) {
                let origin = code::origin_exe(&code_dir);
                let adopted = Instance::adopt(&lock);
                let stdin_null = {
                    use std::os::fd::AsFd as _;
                    use std::os::unix::fs::MetadataExt as _;
                    let stdin = std::io::stdin();
                    let held =
                        std::fs::File::from(stdin.as_fd().try_clone_to_owned().expect("stdin"));
                    let null = std::fs::metadata("/dev/null").expect("null");
                    held.metadata().expect("stdin").rdev() == null.rdev()
                };
                let report = format!(
                    "pid={}\nadopted={}\norigin={}\nvariable={}\nstdin_null={stdin_null}\nholder={}\n",
                    std::process::id(),
                    adopted.is_some(),
                    origin.map_or_else(|| "-".to_string(), |p| p.display().to_string()),
                    std::env::var_os(instance::LOCK_FD_ENV).is_some(),
                    instance::running_pid(&lock).map_or_else(|| "-".to_string(), |p| p.to_string()),
                );
                std::fs::write(dir.join("report"), report).expect("report");
                drop(adopted);
            } else {
                let me = std::env::current_exe()
                    .and_then(std::fs::canonicalize)
                    .expect("exe");
                let held = Instance::try_hold(&lock).expect("lock").expect("free");
                std::fs::write(dir.join("first"), std::process::id().to_string()).expect("first");
                let copy = code::prepare_copy(&me, &code_dir, "lbl")
                    .expect("copy")
                    .expect("a bundle");
                // The negative control re-executes without handing the lock over.
                let plain = std::fs::read_to_string(dir.join("mode")).is_ok_and(|m| m == "plain");
                let failed = if plain {
                    code::reexec(&copy, &me)
                } else {
                    code::reexec_holding(&copy, &me, &held)
                };
                panic!("the re-exec into {} failed: {failed}", copy.display());
            }
        }

        /// A stand-in bundle whose executable is this test binary.
        fn stand_in(root: &Path) -> PathBuf {
            let macos = root.join("T.app").join("Contents").join("MacOS");
            std::fs::create_dir_all(&macos).expect("bundle");
            std::fs::write(
                root.join("T.app").join("Contents").join("Info.plist"),
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
                 <key>CFBundleIdentifier</key><string>com.aterm.caffeine.handovertest</string>\
                 </dict></plist>\n",
            )
            .expect("plist");
            let exe = macos.join("handover");
            std::fs::copy(std::env::current_exe().expect("exe"), &exe).expect("copy exe");
            exe
        }

        /// Run the child from a fresh stand-in bundle, in `mode`: its pid before the exec, the
        /// copy's report, the stand-in's executable (canonical) and the scratch directory.
        fn run_child(mode: &str) -> (String, String, PathBuf, aterm_tempfile::TempDir) {
            let root = aterm_tempfile::tempdir().expect("tempdir");
            let exe = stand_in(root.path());
            let work = root.path().join("work");
            std::fs::create_dir_all(&work).expect("work");
            std::fs::write(work.join("mode"), mode).expect("mode");
            // A minute is a hang detector.
            let out = aterm_uds::deadline::output_until(
                aterm_uds::spawnfd::command(&exe)
                    .args([
                        "code::tests::handover::handover_child",
                        "--exact",
                        "--test-threads=1",
                    ])
                    .env(DIR_ENV, &work),
                Instant::now() + Duration::from_secs(60),
            )
            .expect("child");
            assert!(
                out.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            let first = std::fs::read_to_string(work.join("first")).expect("first");
            let report = std::fs::read_to_string(work.join("report")).expect("the copy ran");
            (
                first,
                report,
                std::fs::canonicalize(&exe).expect("origin"),
                root,
            )
        }

        #[test]
        fn the_lock_is_handed_over_across_the_reexec_into_the_copy() {
            let (first, report, origin, root) = run_child("handover");
            assert_eq!(
                report,
                format!(
                    "pid={first}\nadopted=true\norigin={}\nvariable=false\nstdin_null=true\nholder={first}\n",
                    origin.display()
                )
            );
            let work = root.path().join("work");
            // The copy is where prepare_copy put it, and the lock went with the process.
            assert!(
                work.join("code")
                    .join("lbl")
                    .join("T.app")
                    .join("Contents")
                    .join("MacOS")
                    .join("handover")
                    .is_file()
            );
            assert_eq!(instance::running_pid(&work.join("caffeine.lock")), None);
            drop(root);
        }

        /// Negative control: a re-exec that does not hand the lock over leaves the copy nothing to
        /// adopt, and the lock free (the old image's descriptor closed at the exec).
        #[test]
        fn a_plain_reexec_carries_no_lock() {
            let (first, report, origin, root) = run_child("plain");
            assert_eq!(
                report,
                format!(
                    "pid={first}\nadopted=false\norigin={}\nvariable=false\nstdin_null=true\nholder=-\n",
                    origin.display()
                )
            );
            drop(root);
        }
    }
}
