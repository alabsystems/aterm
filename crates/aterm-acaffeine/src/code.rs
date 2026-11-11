// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The bundle copy the mug runs from.

use std::io;
use std::path::{Path, PathBuf};

/// The variable naming the executable the mug was started as, set when it re-executes from its copy.
pub const ORIGIN_ENV: &str = "ATERM_CAFFEINE_ORIGIN";

const PLIST_CAP: u64 = 1 << 20;

/// The file a mug running from its copy writes into the copy's directory first thing ([`mark_ran`]).
pub const RAN: &str = aterm_uds::deadline::RAN;

/// Say, in the directory of the copy under `code_dir` this process runs from (`<code dir>/<label>/<id>`), that it runs from it.
pub fn mark_ran(code_dir: &Path) {
    let (Some(exe), Ok(dir)) = (own_exe(), code_dir.canonicalize()) else {
        return;
    };
    let Ok(rest) = exe.strip_prefix(&dir) else {
        return;
    };
    let mut parts = rest.components();
    let (Some(label), Some(id)) = (parts.next(), parts.next()) else {
        return;
    };
    let home = dir.join(label).join(id);
    if let Err(e) = aterm_uds::deadline::try_mark_ran(&home) {
        eprintln!(
            "aterm acaffeine: cannot mark {} as run from: {e}",
            home.display()
        );
    }
}

/// The `.app` bundle `exe` is the main executable of (`X.app/Contents/MacOS/<exe>`), if it is one.
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

fn own_exe() -> Option<PathBuf> {
    std::env::current_exe().ok()?.canonicalize().ok()
}

/// The executable the mug was started as.
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

fn is_under(exe: &Path, dir: &Path) -> bool {
    exe.strip_prefix(dir)
        .is_ok_and(|rest| rest.components().next().is_some())
}

/// Copy the bundle `origin` belongs to into `code_dir` (replacing an earlier copy for `label`) and return the copy's executable, or, when a mug already ran from the copy for `label` and the bundle is unchanged since it was made ([`ran_copy`]), that copy's: a start again (launchd's retry, a window's kickstart) copies no bundle.
///
/// # Errors
/// The copy cannot be made: `label` is not a plain name, `code_dir` is not a directory of
/// this user's, an entry in it cannot be removed, or the bundle holds something other than
/// files, directories and links, or cannot be read.
pub fn prepare_copy(origin: &Path, code_dir: &Path, label: &str) -> io::Result<Option<PathBuf>> {
    let Some(bundle) = own_bundle(origin) else {
        #[cfg(unix)]
        match discard_copies(code_dir) {
            Ok(kept) => kept.say(),
            Err(e) => eprintln!("aterm acaffeine: {}: {e}", code_dir.display()),
        }
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

pub(crate) fn plain_name(label: &str) -> bool {
    !label.is_empty()
        && label != "."
        && label != ".."
        && !label.contains(['/', '\\', '\0'])
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
    let print = bundle_print(origin, bundle).ok();
    let reused = print
        .as_deref()
        .and_then(|print| ran_copy(code_dir, label, print, &inner));
    sweep(code_dir, std::time::SystemTime::now(), reused.as_deref())?.say();
    if let Some(home) = reused {
        return Ok(home.join(inner));
    }
    let slot = code_dir.join(label);
    match std::fs::create_dir(&slot) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
        _ => {}
    }
    if !std::fs::symlink_metadata(&slot)?.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a directory", slot.display()),
        ));
    }
    let id = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    );
    let home = slot.join(&id);
    let partial = slot.join(format!("{id}.partial"));
    let made = std::fs::create_dir(&partial)
        .and_then(|()| copy_tree(bundle, &partial.join(top)))
        .and_then(|()| {
            print.as_ref().map_or(Ok(()), |print| {
                use std::io::Write as _;
                let mut opts = std::fs::OpenOptions::new();
                opts.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt as _;
                    opts.custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS);
                }
                let mut f = opts.open(partial.join(SOURCE))?;
                f.write_all(print.as_bytes())
            })
        })
        .and_then(|()| std::fs::rename(&partial, &home));
    if let Err(e) = made {
        let _ = std::fs::remove_dir_all(&partial);
        let _ = std::fs::remove_dir(&slot);
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

/// The file in a copy's directory, beside [`RAN`], naming the bundle it was copied from
/// ([`bundle_print`]).
#[cfg(unix)]
const SOURCE: &str = "source";

/// What names `bundle`'s code, for a reuse of a copy of it ([`ran_copy`]): `origin`, and every
/// entry under the bundle by its path, kind, size, permissions and modification time (a link by
/// its target too), hashed. Only metadata is read, so a start that reuses a copy walks the
/// bundle and copies none of its bytes; an update, which puts a new bundle in place, and a
/// rebuild, which writes new files, both change it.
#[cfg(unix)]
fn bundle_print(origin: &Path, bundle: &Path) -> io::Result<String> {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::MetadataExt as _;
    fn walk(at: &Path, rel: &Path, hash: &mut u64, entries: &mut u64) -> io::Result<()> {
        let meta = std::fs::symlink_metadata(at)?;
        let kind = meta.file_type();
        *entries += 1;
        let mut row = rel.as_os_str().as_bytes().to_vec();
        row.push(0);
        row.extend_from_slice(
            format!(
                "{}:{}:{:o}:{}.{:09}\n",
                if kind.is_dir() {
                    'd'
                } else if kind.is_symlink() {
                    'l'
                } else {
                    'f'
                },
                meta.len(),
                meta.mode(),
                meta.mtime(),
                meta.mtime_nsec()
            )
            .as_bytes(),
        );
        if kind.is_symlink() {
            row.extend_from_slice(std::fs::read_link(at)?.as_os_str().as_bytes());
            row.push(0);
        }
        *hash = aterm_hash::fnv1a_64_extend(*hash, &row);
        if kind.is_dir() {
            let mut names = std::fs::read_dir(at)?
                .map(|entry| entry.map(|entry| entry.file_name()))
                .collect::<io::Result<Vec<_>>>()?;
            names.sort();
            for name in names {
                walk(&at.join(&name), &rel.join(&name), hash, entries)?;
            }
        }
        Ok(())
    }
    let (mut hash, mut entries) = (aterm_hash::fnv1a_64(b""), 0);
    walk(bundle, Path::new(""), &mut hash, &mut entries)?;
    Ok(format!(
        "origin={}\nentries={entries}\ntree={hash:016x}\n",
        origin.display()
    ))
}

#[cfg(unix)]
fn is_regular_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_file())
}

/// The copy for `label` under `code_dir` that a mug already ran from (its [`RAN`] marker) and
/// whose [`SOURCE`] is `print`, with its executable at `inner`: macOS assessed its first exec
/// when that mug ran, and the bundle has not changed since it was made.
#[cfg(unix)]
fn ran_copy(code_dir: &Path, label: &str, print: &str, inner: &Path) -> Option<PathBuf> {
    let slot = code_dir.join(label);
    if !std::fs::symlink_metadata(&slot).ok()?.file_type().is_dir() {
        return None;
    }
    std::fs::read_dir(&slot)
        .ok()?
        .flatten()
        .filter(|entry| {
            entry.file_type().is_ok_and(|ft| ft.is_dir())
                && Path::new(&entry.file_name())
                    .extension()
                    .is_none_or(|e| e != "partial")
        })
        .map(|entry| entry.path())
        .find(|home| {
            is_regular_file(&home.join(RAN))
                && read_regular_capped(&home.join(SOURCE))
                    .is_some_and(|text| text == print.as_bytes())
                && home.join(inner).is_file()
        })
}

#[cfg(unix)]
fn private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{
        DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
    };
    if let Some(parent) = dir.parent()
        && !parent.as_os_str().is_empty()
    {
        let _ = crate::paths::existing_real_dir(parent)?;
    }
    let _ = crate::paths::existing_real_dir(dir)?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let link_meta = std::fs::symlink_metadata(dir)?;
    if !link_meta.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a directory", dir.display()),
        ));
    }
    let dir_fd = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(dir)
        .map_err(|err| match err.raw_os_error() {
            Some(code) if code == libc::ELOOP || code == libc::ENOTDIR => io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a directory", dir.display()),
            ),
            _ => err,
        })?;
    let meta = dir_fd.metadata()?;
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
        dir_fd.set_permissions(std::fs::Permissions::from_mode(0o700))?;
    }
    // Best effort, as it was: a copy with no backup mark still runs.
    let _ = aterm_uds::backup::Backups::SYSTEM.exclude(&dir_fd);
    Ok(())
}

/// Remove `code_dir` and every copy in it that may go ([`sweep`]), and return the copies kept.
///
/// # Errors
/// `code_dir` is not this user's directory, or an entry in it cannot be removed.
#[cfg(unix)]
pub fn discard_copies(code_dir: &Path) -> io::Result<Kept> {
    use std::os::unix::fs::MetadataExt as _;
    if let Some(parent) = code_dir.parent()
        && !parent.as_os_str().is_empty()
        && !crate::paths::existing_real_dir(parent)?
    {
        return Ok(Kept::default());
    }
    let meta = match std::fs::symlink_metadata(code_dir) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Kept::default()),
        Err(e) => return Err(e),
    };
    if !meta.file_type().is_dir() || meta.uid() != aterm_uds::peer::our_uid() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not this user's directory", code_dir.display()),
        ));
    }
    let kept = sweep(code_dir, std::time::SystemTime::now(), None)?;
    if std::fs::read_dir(code_dir)?.next().is_none() {
        std::fs::remove_dir(code_dir)?;
    }
    Ok(kept)
}

/// The copies a sweep kept because macOS may still be assessing their first exec, which the
/// caller says: [`Kept::say`] in the mug's own `code_dir`.
#[cfg(unix)]
#[must_use = "each copy kept is said"]
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Kept(pub Vec<PathBuf>);

#[cfg(unix)]
impl Kept {
    /// Say each copy on stderr ([`kept_line`]): a verb's, or the mug job's log.
    pub fn say(&self) {
        for copy in &self.0 {
            eprintln!("{}", kept_line(copy));
        }
    }
}

/// What a mug or a verb says of a copy in its own `code_dir` that a sweep kept.
#[cfg(unix)]
#[must_use]
pub fn kept_line(copy: &Path) -> String {
    format!(
        "aterm acaffeine: kept {}: no mug said it ran from it, so macOS may still be assessing \
         its first exec; `aterm acaffeine quit` or the next start removes it once the Mac has \
         been awake {} minutes since",
        copy.display(),
        aterm_uds::deadline::FIRST_EXEC_HANG.as_secs() / 60
    )
}

#[cfg(unix)]
const PROBE_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Whether the executable `origin` still has the `acaffeine` verb ([`answers_help`]).
#[cfg(unix)]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn serves_acaffeine(origin: &Path) -> Option<bool> {
    answers_help(origin, "acaffeine")
}

/// Whether the executable `origin` answers `help <topic>` with exit status 0, as an aterm does for
/// a verb it has. `None` when it gave no answer: it could not be run, did not exit within ten
/// seconds, or was ended by a signal.
#[cfg(unix)]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn answers_help(origin: &Path, topic: &str) -> Option<bool> {
    answers(origin, ["help", topic])
}

/// Whether the executable `origin`, run with `args`, exits with status 0. `None` when it gave no
/// answer, as for [`answers_help`].
#[cfg(unix)]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn answers(origin: &Path, args: [&str; 2]) -> Option<bool> {
    answers_within(origin, args, PROBE_WAIT)
}

#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
fn answers_within(origin: &Path, args: [&str; 2], wait: std::time::Duration) -> Option<bool> {
    let now = std::time::Instant::now();
    let deadline = now
        .checked_add(wait)
        .unwrap_or_else(|| now + std::time::Duration::from_secs(86_400 * 365));
    let out =
        aterm_uds::deadline::output_until(aterm_uds::spawnfd::command(origin).args(args), deadline)
            .ok()?;
    out.status.code().map(|code| code == 0)
}

/// Remove each copy in `dir` that may go at `now` ([`may_go`]) but `keep`, the copy a start
/// reuses ([`ran_copy`]), and return the ones kept because they may not go yet.
#[cfg(unix)]
fn sweep(dir: &Path, now: std::time::SystemTime, keep: Option<&Path>) -> io::Result<Kept> {
    let mut kept_copies = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_dir() {
            remove_entry(&path, false)?;
            continue;
        }
        if path.extension().is_some_and(|e| e == "partial") {
            remove_entry(&path, true)?;
            continue;
        }
        let mut kept = false;
        for copy in std::fs::read_dir(&path)? {
            let copy = copy?;
            let at = copy.path();
            let dir = copy.file_type()?.is_dir();
            if dir && keep == Some(at.as_path()) {
                kept = true;
                continue;
            }
            if dir && !may_go(&at, now) {
                kept_copies.push(at);
                kept = true;
                continue;
            }
            remove_entry(&at, dir)?;
        }
        if !kept {
            remove_entry(&path, true)?;
        }
    }
    Ok(Kept(kept_copies))
}

#[cfg(unix)]
fn may_go(home: &Path, now: std::time::SystemTime) -> bool {
    home.extension().is_some_and(|e| e == "partial") || aterm_uds::deadline::copy_may_go(home, now)
}

#[cfg(unix)]
fn remove_entry(path: &Path, dir: bool) -> io::Result<()> {
    let removed = if dir {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    removed.map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))
}

#[cfg(unix)]
fn copy_tree(src: &Path, dst: &Path) -> io::Result<()> {
    copy_tree_kind(src, dst, std::fs::symlink_metadata(src)?.file_type())
}

#[cfg(unix)]
fn copy_tree_kind(src: &Path, dst: &Path, kind: std::fs::FileType) -> io::Result<()> {
    if kind.is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(src)?, dst)
    } else if kind.is_dir() {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let meta = std::fs::symlink_metadata(src)?;
        std::fs::create_dir(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let ft = entry.file_type()?;
            copy_tree_kind(&entry.path(), &dst.join(entry.file_name()), ft)?;
        }
        let dir_fd = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(dst)?;
        let perms = meta.permissions();
        if dir_fd
            .metadata()
            .is_ok_and(|m| m.permissions().mode() & 0o7777 == perms.mode() & 0o7777)
        {
            return Ok(());
        }
        dir_fd.set_permissions(perms)
    } else if kind.is_file() {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let (mut source, src_meta) = aterm_uds::regular_file::open(src)?;
        let perms = src_meta.permissions();
        let mut sink = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(perms.mode())
            .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
            .open(dst)?;
        std::io::copy(&mut source, &mut sink)?;
        if sink
            .metadata()
            .is_ok_and(|m| m.permissions().mode() & 0o7777 == perms.mode() & 0o7777)
        {
            return Ok(());
        }
        sink.set_permissions(perms)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a file, a directory or a link", src.display()),
        ))
    }
}

/// The bundle identifier `bundle`'s `Contents/Info.plist` declares (`CFBundleIdentifier`, a `<string>`, its XML entities read).
#[must_use]
pub fn bundle_identifier(bundle: &Path) -> Option<String> {
    let bytes = read_capped(&bundle.join("Contents").join("Info.plist"))?;
    plist_string(std::str::from_utf8(&bytes).ok()?, "CFBundleIdentifier")
}

fn strip_xml_comments(text: &str) -> Option<std::borrow::Cow<'_, str>> {
    if !text.contains("<!--") {
        return Some(std::borrow::Cow::Borrowed(text));
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut in_tag = false;
    while let Some(pos) = rest.find('<') {
        for b in rest[..pos].bytes() {
            if b == b'>' {
                in_tag = false;
            }
        }
        out.push_str(&rest[..pos]);
        if in_tag {
            return None;
        }
        let after_lt = &rest[pos..];
        if let Some(comment) = after_lt.strip_prefix("<!--") {
            let end = comment.find("-->")?;
            if comment[..end].contains("--") {
                return None;
            }
            rest = &comment[end + "-->".len()..];
        } else {
            in_tag = true;
            out.push('<');
            rest = &after_lt[1..];
        }
    }
    for b in rest.bytes() {
        if b == b'>' {
            in_tag = false;
        }
    }
    if in_tag {
        return None;
    }
    out.push_str(rest);
    Some(std::borrow::Cow::Owned(out))
}

fn plist_string(text: &str, key: &str) -> Option<String> {
    let text = strip_xml_comments(text)?;
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

/// The `<string>` values of the `<array>` that is `key`'s value in a plist's text.
#[must_use]
pub(crate) fn plist_strings(text: &str, key: &str) -> Option<Vec<String>> {
    let text = strip_xml_comments(text)?;
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

/// The text of the plist at `path`, when it is a regular UTF-8 file of at most [`PLIST_CAP`] bytes.
#[must_use]
pub(crate) fn read_plist(path: &Path) -> Option<String> {
    String::from_utf8(read_regular_capped(path)?).ok()
}

fn xml_unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn read_capped(path: &Path) -> Option<Vec<u8>> {
    read_capped_inner(path, false)
}

fn read_regular_capped(path: &Path) -> Option<Vec<u8>> {
    read_capped_inner(path, true)
}

fn read_capped_inner(path: &Path, nofollow: bool) -> Option<Vec<u8>> {
    use std::io::Read as _;
    #[cfg(not(unix))]
    if nofollow && !std::fs::symlink_metadata(path).ok()?.file_type().is_file() {
        return None;
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut flags = libc::O_NONBLOCK | libc::O_CLOEXEC;
        if nofollow {
            flags |= libc::O_NOFOLLOW;
        }
        options.custom_flags(flags);
    }
    let file = options.open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > PLIST_CAP {
        return None;
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len().saturating_add(1)).unwrap_or(0));
    file.take(PLIST_CAP + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= PLIST_CAP).then_some(bytes)
}

/// What the update watch compares to tell that the code the mug was started from changed. For a
/// bundle it is the `Info.plist` bytes, which a copy shares with its origin (an executable's
/// times need not survive the copy): a release changes its version there, and a dev bundle the
/// hash of its executable (aterm-release's `DEV_BINARY_KEY`).
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

#[cfg(unix)]
fn reexec_command(copy: &Path, origin: &Path) -> aterm_uds::spawnfd::Command {
    if let Some(home) = own_bundle(copy).as_deref().and_then(Path::parent) {
        aterm_uds::deadline::stamp_exec(home);
    }
    let mut args = std::env::args_os();
    let mut cmd = aterm_uds::spawnfd::command(copy);
    if let Some(arg0) = args.next() {
        cmd.arg0(arg0);
    }
    cmd.args(args).env(ORIGIN_ENV, origin);
    cmd
}

/// Become `copy` with this process's arguments and [`ORIGIN_ENV`] naming `origin`.
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

/// [`reexec`], handing `instance`'s lock to the new image on its stdin ([`crate::instance::Instance::handover`]) so no moment passes without it held.
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
#[path = "code_tests.rs"]
mod tests;
