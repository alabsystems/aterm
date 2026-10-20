// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! The abrew prefix: layout, installed-keg scan, receipts, pins, lock.
//!
//! Design: `docs/DESIGN-abrew-2026-10-19.md` §2. The cellar is truth:
//! an installed formula is a `Cellar/<name>/<version>` directory with an
//! `INSTALL_RECEIPT.json`; `state.json` is a rebuilt-on-demand index,
//! never the source a decision is made from.

use std::path::{Path, PathBuf};

/// Environment override for the whole prefix (tests, private instances).
pub const PREFIX_ENV: &str = "ABREW_PREFIX";
/// The filename whose presence under the prefix means `abrew off`.
pub const DISABLED_FILE: &str = "disabled";

/// One installed keg (formula) or cask payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub name: String,
    pub version: String,
    pub kind: Kind,
    /// Every receipt-admitted version (old versions kept until cleanup).
    pub versions: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Formula,
    Cask,
}

impl Kind {
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Kind::Formula => "formula",
            Kind::Cask => "cask",
        }
    }
}

/// A set-but-unusable `ABREW_PREFIX`, spelled for one warning line:
/// silently falling through to the real prefix would aim a scratch
/// run's writes at the owner's actual cellar.
#[must_use]
pub fn prefix_env_warning() -> Option<String> {
    let p = std::env::var_os(PREFIX_ENV)?;
    let path = PathBuf::from(&p);
    if path.is_absolute() {
        None
    } else {
        Some(format!(
            "ABREW_PREFIX={} is not absolute; ignoring it (using the resolved prefix)",
            path.display()
        ))
    }
}

/// Whether `root` overlaps a real Homebrew prefix. abrew receipts and
/// deletion authority assume disjoint trees; pointing abrew at (or
/// above/below) `/opt/homebrew`, `/usr/local`, or linuxbrew breaks
/// that assumption, so the engine refuses.
#[must_use]
pub fn overlaps_real_brew_prefix(root: &Path) -> bool {
    const REAL: &[&str] = &["/opt/homebrew", "/usr/local", "/home/linuxbrew/.linuxbrew"];
    REAL.iter()
        .map(Path::new)
        .any(|real| root == real || root.starts_with(real) || real.starts_with(root))
}

/// The abrew prefix layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prefix {
    pub root: PathBuf,
}

impl Prefix {
    /// Resolve the prefix: `ABREW_PREFIX` (absolute, non-empty), else
    /// `<atpkg prefix>/abrew` from the resolved store layout, else
    /// `~/.abrew` so a bare `atpkg-abrew` still runs on a machine where
    /// aterm has laid nothing yet.
    #[must_use]
    pub fn resolve() -> Prefix {
        if let Some(p) = std::env::var_os(PREFIX_ENV) {
            let p = PathBuf::from(p);
            if p.is_absolute() {
                return Prefix { root: p };
            }
        }
        if let Some(layout) = crate::store::resolve_from(crate::config::cached()) {
            return Prefix {
                root: layout.prefix.join("abrew"),
            };
        }
        let home = aterm_types::dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Prefix {
            root: home.join(".abrew"),
        }
    }

    /// A prefix rooted at an explicit path (tests).
    #[must_use]
    pub fn at(root: &Path) -> Prefix {
        Prefix {
            root: root.to_path_buf(),
        }
    }

    #[must_use]
    pub fn cellar(&self) -> PathBuf {
        self.root.join("Cellar")
    }
    #[must_use]
    pub fn caskroom(&self) -> PathBuf {
        self.root.join("Caskroom")
    }
    #[must_use]
    pub fn keg(&self, name: &str, version: &str) -> PathBuf {
        self.cellar().join(name).join(version)
    }
    #[must_use]
    pub fn cask_dir(&self, token: &str, version: &str) -> PathBuf {
        self.caskroom().join(token).join(version)
    }
    #[must_use]
    pub fn opt(&self, name: &str) -> PathBuf {
        self.root.join("opt").join(name)
    }
    #[must_use]
    pub fn bin(&self) -> PathBuf {
        self.root.join("bin")
    }
    #[must_use]
    pub fn cache(&self) -> PathBuf {
        self.root.join("cache")
    }
    #[must_use]
    pub fn taps_dir(&self) -> PathBuf {
        self.root.join("Library").join("Taps")
    }
    #[must_use]
    pub fn applications(&self) -> PathBuf {
        self.root.join("Applications")
    }
    #[must_use]
    pub fn homebrew_bundle(&self) -> PathBuf {
        self.root.join("homebrew")
    }
    #[must_use]
    pub fn disabled_path(&self) -> PathBuf {
        self.root.join(DISABLED_FILE)
    }
    #[must_use]
    pub fn is_disabled(&self) -> bool {
        self.disabled_path().exists()
    }

    /// Create the standing directories. Idempotent.
    pub fn ensure(&self) -> Result<(), String> {
        let directories = [
            self.root.clone(),
            self.cellar(),
            self.caskroom(),
            self.root.join("opt"),
            self.bin(),
            self.root.join("sbin"),
            self.root.join("lib"),
            self.root.join("include"),
            self.root.join("share"),
            self.root.join("etc"),
            self.root.join("var"),
            self.cache(),
            self.cache().join("api"),
            self.root.join("Library"),
            self.taps_dir(),
            self.applications(),
            self.root.join("Pinned"),
        ];
        ensure_directories(&directories)
    }

    /// Prepare metadata storage without initializing the installation layout.
    pub(super) fn ensure_api_cache(&self) -> Result<(), String> {
        ensure_directories(&[self.root.clone(), self.cache(), self.cache().join("api")])
    }

    /// Every installed keg of one kind, sorted by name. Read-only callers keep
    /// the historical best-effort scan; destructive callers require its checked form.
    #[must_use]
    pub fn installed(&self, kind: Kind) -> Vec<Installed> {
        self.scan_installed(kind, false).unwrap_or_default()
    }

    /// The complete physical-directory census used before deletion. An unreadable
    /// row or aliased directory is unknown, not evidence that no dependent exists.
    pub(super) fn installed_checked(&self, kind: Kind) -> Result<Vec<Installed>, String> {
        self.scan_installed(kind, true)
    }

    fn scan_installed(&self, kind: Kind, complete: bool) -> Result<Vec<Installed>, String> {
        let base = match kind {
            Kind::Formula => self.cellar(),
            Kind::Cask => self.caskroom(),
        };
        real_base(&self.root)?;
        real_base(&base)?;
        let names = match std::fs::read_dir(&base) {
            Ok(names) => names,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) if complete => return Err(format!("abrew: {e}")),
            Err(_) => return Ok(Vec::new()),
        };
        let mut out = Vec::new();
        for entry in names {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) if complete => return Err(format!("abrew: {e}")),
                Err(_) => continue,
            };
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(e) if complete => return Err(format!("abrew: {e}")),
                Err(_) => continue,
            };
            if complete && ft.is_symlink() {
                return Err(format!(
                    "abrew: aliased installed row: {}",
                    entry.path().display()
                ));
            }
            if !ft.is_dir() {
                continue;
            }
            let raw_name = entry.file_name();
            let name = if complete {
                raw_name
                    .into_string()
                    .map_err(|_| "abrew: installed name is not UTF-8".to_string())?
            } else {
                raw_name.to_string_lossy().into_owned()
            };
            if !super::valid_component(&name) {
                if complete {
                    return Err("abrew: invalid installed name".to_string());
                }
                continue;
            }
            let version_rows = match std::fs::read_dir(entry.path()) {
                Ok(rows) => rows,
                Err(e) if complete => return Err(format!("abrew: {e}")),
                Err(_) => continue,
            };
            let mut versions = Vec::new();
            for row in version_rows {
                let row = match row {
                    Ok(row) => row,
                    Err(e) if complete => return Err(format!("abrew: {e}")),
                    Err(_) => continue,
                };
                let ft = match row.file_type() {
                    Ok(ft) => ft,
                    Err(e) if complete => return Err(format!("abrew: {e}")),
                    Err(_) => continue,
                };
                if complete && ft.is_symlink() {
                    return Err(format!(
                        "abrew: aliased installed version: {}",
                        row.path().display()
                    ));
                }
                if ft.is_dir() {
                    let raw_version = row.file_name();
                    let version = if complete {
                        raw_version
                            .into_string()
                            .map_err(|_| "abrew: installed version is not UTF-8".to_string())?
                    } else {
                        raw_version.to_string_lossy().into_owned()
                    };
                    if !super::valid_component(&version)
                        || !owned_receipt(&row.path(), kind, &name, &version)
                    {
                        if complete {
                            return Err(format!(
                                "abrew: incomplete or unowned installed row: {}",
                                row.path().display()
                            ));
                        }
                        continue;
                    }
                    versions.push(version);
                }
            }
            if versions.is_empty() {
                continue;
            }
            versions.sort_by(|a, b| super::formula::version_cmp(a, b));
            out.push(Installed {
                name,
                version: versions.last().cloned().unwrap_or_default(),
                kind,
                versions,
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// The installed record for one name, if any version stands.
    #[must_use]
    pub fn installed_one(&self, kind: Kind, name: &str) -> Option<Installed> {
        self.installed(kind).into_iter().find(|i| i.name == name)
    }

    /// The admitted version named by this engine's pin marker. A foreign file,
    /// link outside this package, or dangling/unowned target grants no ownership.
    #[must_use]
    pub fn pinned_version(&self, name: &str) -> Option<String> {
        if !super::valid_component(name) {
            return None;
        }
        for base in [
            self.root.clone(),
            self.cellar(),
            self.cellar().join(name),
            self.root.join("Pinned"),
        ] {
            real_base(&base).ok()?;
        }
        let target = std::fs::read_link(self.root.join("Pinned").join(name)).ok()?;
        let version = target.file_name()?.to_str()?.to_owned();
        (target == self.keg(name, &version)
            && std::fs::symlink_metadata(&target)
                .is_ok_and(|metadata| metadata.file_type().is_dir())
            && owned_receipt(&target, Kind::Formula, name, &version))
        .then_some(version)
    }

    #[must_use]
    pub fn is_pinned(&self, name: &str) -> bool {
        self.pinned_version(name).is_some()
    }

    /// Any standing marker protects a formula from automatic deletion. Only
    /// NotFound proves absence; a failed lookup must not become "not pinned".
    pub(super) fn is_pinned_checked(&self, name: &str) -> Result<bool, String> {
        if !super::valid_component(name) {
            return Err("abrew: invalid formula name".to_string());
        }
        real_base(&self.root)?;
        let parent = self.root.join("Pinned");
        match std::fs::symlink_metadata(&parent) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => return Err("abrew: pin parent is not a physical directory".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(format!("abrew: cannot inspect pin parent: {e}")),
        }
        match std::fs::symlink_metadata(parent.join(name)) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(format!("abrew: cannot read pin for {name}: {e}")),
        }
    }

    /// Pin the current admitted keg. An occupied marker is replaced only when
    /// it already names an admitted keg of this package.
    pub fn pin(&self, name: &str) -> Result<(), String> {
        if !super::valid_component(name) {
            return Err(format!("abrew: invalid formula name {name}"));
        }
        self.ensure()?;
        let Some(inst) = self.installed_one(Kind::Formula, name) else {
            return Err(format!("abrew: {name} is not installed"));
        };
        let dir = self.root.join("Pinned");
        std::fs::create_dir_all(&dir).map_err(|e| format!("abrew: {e}"))?;
        let link = dir.join(name);
        match std::fs::symlink_metadata(&link) {
            Ok(_) if self.is_pinned(name) => {
                std::fs::remove_file(&link).map_err(|e| format!("abrew: {e}"))?;
            }
            Ok(_) => {
                return Err(format!(
                    "abrew: pin {} is not owned by abrew",
                    link.display()
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("abrew: {e}")),
        }
        symlink_dir(&self.keg(name, &inst.version), &link)
    }

    pub fn unpin(&self, name: &str) -> Result<(), String> {
        if !super::valid_component(name) {
            return Err(format!("abrew: invalid formula name {name}"));
        }
        self.ensure()?;
        if self.pinned_version(name).is_none() {
            return Err(format!("abrew: {name} has no owned pin"));
        }
        std::fs::remove_file(self.root.join("Pinned").join(name)).map_err(|e| format!("abrew: {e}"))
    }

    /// Write an install receipt into a keg/cask dir. Receipts are the
    /// durable record `doctor` rebuilds state from.
    pub fn write_receipt(&self, dir: &Path, fields: &[(&str, &str)]) -> Result<(), String> {
        self.ensure()?;
        let mut s = String::from("{\n");
        for (i, (k, v)) in fields.iter().enumerate() {
            if i > 0 {
                s.push_str(",\n");
            }
            s.push_str(&format!("  \"{}\": \"{}\"", json_escape(k), json_escape(v)));
        }
        s.push_str("\n}\n");
        write_metadata(&dir.join("INSTALL_RECEIPT.json"), s.as_bytes())
            .map_err(|e| format!("abrew: cannot write receipt in {}: {e}", dir.display()))
    }

    /// Rebuild `state.json` (a cache) from the cellar and caskroom.
    pub fn rebuild_state(&self) -> Result<(), String> {
        self.ensure()?;
        let mut rows = Vec::new();
        for kind in [Kind::Formula, Kind::Cask] {
            for i in self.installed(kind) {
                rows.push(format!(
                    "  {{\"name\":\"{}\",\"kind\":\"{}\",\"version\":\"{}\",\"pinned\":{}}}",
                    json_escape(&i.name),
                    kind.word(),
                    json_escape(&i.version),
                    kind == Kind::Formula && self.is_pinned(&i.name)
                ));
            }
        }
        let body = format!("{{\n\"installed\": [\n{}\n]\n}}\n", rows.join(",\n"));
        write_metadata(&self.root.join("state.json"), body.as_bytes())
            .map_err(|e| format!("abrew: cannot write state.json: {e}"))
    }
}

/// Exchange an admitted complete directory, or publish a first one without
/// replacing any entry that appeared after preflight. Unsupported filesystems
/// leave both names in place.
pub(super) fn publish_directory(staged: &Path, dest: &Path, replace: bool) -> std::io::Result<()> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        use std::os::unix::ffi::OsStrExt as _;
        let staged = std::ffi::CString::new(staged.as_os_str().as_bytes())?;
        let dest = std::ffi::CString::new(dest.as_os_str().as_bytes())?;
        // SAFETY: both paths are live NUL-terminated strings. These calls
        // exchange directory entries without dereferencing Rust memory.
        #[cfg(target_os = "macos")]
        let status = unsafe {
            libc::renamex_np(
                staged.as_ptr(),
                dest.as_ptr(),
                if replace {
                    libc::RENAME_SWAP
                } else {
                    libc::RENAME_EXCL
                },
            )
        };
        #[cfg(target_os = "linux")]
        let status = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                staged.as_ptr(),
                libc::AT_FDCWD,
                dest.as_ptr(),
                if replace {
                    libc::RENAME_EXCHANGE
                } else {
                    libc::RENAME_NOREPLACE
                },
            )
        };
        if status == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (staged, dest, replace);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "atomic directory publication requires macOS or Linux",
        ))
    }
}

/// Publish complete metadata without following an existing final link. The
/// stage is exclusively created, and its guard removes only our own scratch.
pub(super) fn write_metadata(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "metadata needs a parent")
    })?;
    let stage = aterm_tempfile::Builder::new()
        .prefix(".abrew-metadata-")
        .tempdir_in(parent)?;
    let staged = stage.path().join("metadata");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&staged)?;
    file.write_all(bytes)?;
    aterm_durable::durable(&file)?;
    drop(file);
    std::fs::rename(staged, path)
}

/// Existing canonical layout bases must be real directories. Missing bases
/// can be created after all existing ones pass, without following a final link.
fn ensure_directories(directories: &[PathBuf]) -> Result<(), String> {
    // Vet every existing canonical base before creating any. Parent aliases
    // such as /var -> /private/var are unaffected.
    for directory in directories {
        real_base(directory)?;
    }
    for directory in directories {
        std::fs::create_dir_all(directory)
            .map_err(|e| format!("abrew: cannot create {}: {e}", directory.display()))?;
    }
    Ok(())
}

fn real_base(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata)
            if metadata.file_type().is_dir() && !crate::platform::is_reparse(&metadata) =>
        {
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(format!("abrew: {} is not a real directory", path.display())),
        Err(error) => Err(format!("abrew: cannot inspect {}: {error}", path.display())),
    }
}

/// A directory alone is not an installed package. Only the receipt this engine
/// writes, naming this exact package/version, admits it to mutation and cleanup.
pub(super) fn owned_receipt(dir: &Path, kind: Kind, name: &str, version: &str) -> bool {
    let Ok(bytes) =
        crate::metadata_io::read_bounded_regular(&dir.join("INSTALL_RECEIPT.json"), 64 * 1024)
    else {
        return false;
    };
    let Ok(receipt) = aterm_json::from_slice::<aterm_json::Value>(&bytes) else {
        return false;
    };
    let word = |key| receipt.get(key).and_then(aterm_json::Value::as_str);
    word("installed_by") == Some("abrew")
        && word(match kind {
            Kind::Formula => "name",
            Kind::Cask => "token",
        }) == Some(name)
        && word("version") == Some(version)
}

/// Symlink `link` to the directory `target` (Unix). On other platforms a
/// plain copy is never right for a keg link, so it is an error.
pub fn symlink_dir(target: &Path, link: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
            .map_err(|e| format!("abrew: cannot link {}: {e}", link.display()))
    }
    #[cfg(not(unix))]
    {
        let _ = (target, link);
        Err("abrew: symlinks are not supported on this platform".to_string())
    }
}

/// Symlink `link` to the file `target` (Unix).
pub fn symlink_file(target: &Path, link: &Path) -> Result<(), String> {
    symlink_dir(target, link)
}

/// Escape a string for a JSON string literal (the small writer above).
#[must_use]
pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// The abrew single-writer lock. The kernel releases it after a crash; the
/// rendezvous file stays in place so another holder can never be unlinked.
pub struct Lock {
    _file: crate::lock::Flock,
}

impl Lock {
    pub fn acquire(prefix: &Prefix) -> Result<Lock, String> {
        Self::within(prefix, std::time::Duration::from_secs(60))
    }

    fn within(prefix: &Prefix, limit: std::time::Duration) -> Result<Lock, String> {
        prefix.ensure()?;
        // Claim the legacy rendezvous first: checking it only after the
        // flock lets an old binary claim it in between and write beside us.
        let legacy = prefix.root.join(".abrew-lock.d");
        match std::fs::symlink_metadata(&legacy) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(format!(
                    "abrew: legacy lock {} is present; verify its writer has stopped before removing it",
                    legacy.display()
                ));
            }
            Err(error) => {
                return Err(format!(
                    "abrew: cannot inspect legacy lock {}: {error}",
                    legacy.display()
                ));
            }
        }
        let path = prefix.root.join("abrew.lock");
        let file = crate::index_probe::open_private(&path, true).ok_or_else(|| {
            format!(
                "abrew: cannot open lock {} as a regular non-link file",
                path.display()
            )
        })?;
        let file = crate::lock::Flock::lock_within(file, limit, &path)
            .map_err(|error| format!("abrew: cannot take lock {}: {error}", path.display()))?;
        // An earlier binary may still own the old directory rendezvous. Age
        // cannot prove its writer dead; preserve the claim and refuse migration.
        let legacy = prefix.root.join(".abrew-lock.d");
        match std::fs::symlink_metadata(&legacy) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Lock { _file: file }),
            Ok(_) => Err(format!(
                "abrew: legacy lock {} is present; verify its writer has stopped before removing it",
                legacy.display()
            )),
            Err(error) => Err(format!(
                "abrew: cannot inspect legacy lock {}: {error}",
                legacy.display()
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_brew_prefix_overlap_is_detected() {
        assert!(overlaps_real_brew_prefix(Path::new("/opt/homebrew")));
        assert!(overlaps_real_brew_prefix(Path::new("/opt/homebrew/abrew")));
        assert!(overlaps_real_brew_prefix(Path::new("/opt")));
        assert!(overlaps_real_brew_prefix(Path::new("/usr/local")));
        assert!(!overlaps_real_brew_prefix(Path::new("/Users//x/.abrew")));
        assert!(!overlaps_real_brew_prefix(Path::new("/opt/other")));
    }

    #[test]
    fn installed_scan_and_pin_round_trip() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let p = Prefix::at(tmp.path());
        p.ensure().unwrap();
        for version in ["1.24.5", "1.25.0"] {
            let dir = p.keg("wget", version);
            std::fs::create_dir_all(&dir).unwrap();
            p.write_receipt(
                &dir,
                &[
                    ("installed_by", "abrew"),
                    ("name", "wget"),
                    ("version", version),
                ],
            )
            .unwrap();
        }
        for (version, receipt) in [
            ("3.0", None),
            (
                "4.0",
                Some(r#"{"installed_by":"another","name":"wget","version":"4.0"}"#),
            ),
            (
                "5.0",
                Some(r#"{"installed_by":"abrew","name":"different","version":"5.0"}"#),
            ),
            (
                "6.0",
                Some(r#"{"installed_by":"abrew","name":"wget","version":"7.0"}"#),
            ),
            ("8.0", Some("not json")),
        ] {
            let dir = p.keg("wget", version);
            std::fs::create_dir_all(&dir).unwrap();
            if let Some(receipt) = receipt {
                std::fs::write(dir.join("INSTALL_RECEIPT.json"), receipt).unwrap();
            }
        }
        let cask = p.cask_dir("demo", "1.0");
        std::fs::create_dir_all(&cask).unwrap();
        p.write_receipt(
            &cask,
            &[
                ("installed_by", "abrew"),
                ("token", "demo"),
                ("version", "1.0"),
            ],
        )
        .unwrap();
        assert_eq!(p.installed(Kind::Cask)[0].name, "demo");
        #[cfg(unix)]
        {
            let linked = p.keg("wget", "9.0");
            std::fs::create_dir_all(&linked).unwrap();
            let outside = tmp.path().join("foreign-receipt");
            std::fs::write(
                &outside,
                r#"{"installed_by":"abrew","name":"wget","version":"9.0"}"#,
            )
            .unwrap();
            std::os::unix::fs::symlink(&outside, linked.join("INSTALL_RECEIPT.json")).unwrap();
        }
        let got = p.installed(Kind::Formula);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].version, "1.25.0");
        assert_eq!(got[0].versions.len(), 2);
        #[cfg(unix)]
        {
            p.pin("wget").unwrap();
            assert_eq!(p.pinned_version("wget").as_deref(), Some("1.25.0"));
            assert!(p.is_pinned("wget"));
            p.unpin("wget").unwrap();
            assert!(!p.is_pinned("wget"));
            let marker = p.root.join("Pinned/wget");
            std::fs::write(&marker, b"owner bytes").unwrap();
            assert!(p.pin("wget").is_err());
            assert!(p.unpin("wget").is_err());
            assert_eq!(std::fs::read(&marker).unwrap(), b"owner bytes");
            std::fs::remove_file(&marker).unwrap();
            std::os::unix::fs::symlink(p.keg("wget", "missing"), &marker).unwrap();
            assert!(!p.is_pinned("wget"));
            assert!(p.pin("wget").is_err());
            assert!(p.unpin("wget").is_err());
            assert_eq!(
                std::fs::read_link(&marker).unwrap(),
                p.keg("wget", "missing")
            );
            std::fs::remove_file(&marker).unwrap();
            let foreign = tmp.path().join("owner-note");
            std::fs::write(&foreign, b"keep").unwrap();
            std::os::unix::fs::symlink(&foreign, p.root.join("state.json")).unwrap();
            p.rebuild_state().unwrap();
            assert!(!p.root.join("state.json").is_symlink());
            assert_eq!(std::fs::read(&foreign).unwrap(), b"keep");
            let receipt = p.keg("wget", "1.25.0").join("INSTALL_RECEIPT.json");
            std::fs::remove_file(&receipt).unwrap();
            std::os::unix::fs::symlink(&foreign, &receipt).unwrap();
            p.write_receipt(
                &p.keg("wget", "1.25.0"),
                &[
                    ("installed_by", "abrew"),
                    ("name", "wget"),
                    ("version", "1.25.0"),
                ],
            )
            .unwrap();
            assert!(!receipt.is_symlink());
            assert_eq!(std::fs::read(&foreign).unwrap(), b"keep");
            for bad in ["../owner-note", "..", ".", "", foreign.to_str().unwrap()] {
                assert!(p.pin(bad).is_err(), "{bad:?}");
                assert!(p.unpin(bad).is_err(), "{bad:?}");
                assert!(p.pinned_version(bad).is_none());
            }
            assert_eq!(std::fs::read(&foreign).unwrap(), b"keep");
            let original = tmp.path().join("original-package");
            std::fs::rename(p.cellar().join("wget"), &original).unwrap();
            std::os::unix::fs::symlink(&original, p.cellar().join("wget")).unwrap();
            std::os::unix::fs::symlink(p.keg("wget", "1.25.0"), &marker).unwrap();
            assert!(p.installed(Kind::Formula).is_empty());
            assert!(p.pinned_version("wget").is_none());
            assert!(p.pin("wget").is_err());
            assert!(p.unpin("wget").is_err());
            assert!(marker.is_symlink());
            assert!(original.join("1.25.0/INSTALL_RECEIPT.json").is_file());
        }
    }

    #[cfg(unix)]
    #[test]
    fn linked_layout_bases_never_admit_or_mutate_a_foreign_tree() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(outside.join("wget/1.0")).unwrap();
        std::fs::write(outside.join("owner-note"), b"keep").unwrap();
        std::fs::write(
            outside.join("wget/1.0/INSTALL_RECEIPT.json"),
            r#"{"installed_by":"abrew","name":"wget","version":"1.0"}"#,
        )
        .unwrap();
        for relative in ["", "Cellar", "Pinned", "Library", "cache/api"] {
            let root = tmp
                .path()
                .join(format!("prefix-{}", relative.replace('/', "-")));
            let prefix = Prefix::at(&root);
            if relative.is_empty() {
                std::os::unix::fs::symlink(&outside, &root).unwrap();
            } else {
                std::fs::create_dir_all(root.join(relative).parent().unwrap()).unwrap();
                std::os::unix::fs::symlink(&outside, root.join(relative)).unwrap();
            }
            assert!(prefix.ensure().is_err(), "{relative}");
            assert!(prefix.pin("wget").is_err(), "{relative}");
            assert!(prefix.unpin("wget").is_err(), "{relative}");
            assert!(Lock::within(&prefix, std::time::Duration::ZERO).is_err());
            assert!(!root.join("state.json").exists());
            assert!(!root.join("abrew.lock").exists());
            if relative.is_empty() || relative == "Cellar" {
                assert!(prefix.installed(Kind::Formula).is_empty());
            }
            assert_eq!(std::fs::read(outside.join("owner-note")).unwrap(), b"keep");
            assert!(outside.join("wget/1.0/INSTALL_RECEIPT.json").is_file());
        }
    }

    #[test]
    fn the_writer_lock_never_reclaims_a_live_holder_or_unlinks_its_rendezvous() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(tmp.path());
        let first = Lock::acquire(&prefix).unwrap();
        first
            ._file
            .set_times(std::fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
            .unwrap();
        assert!(Lock::within(&prefix, std::time::Duration::ZERO).is_err());
        let path = prefix.root.join("abrew.lock");
        assert!(path.is_file());
        drop(first);
        let successor = Lock::acquire(&prefix).unwrap();
        assert!(Lock::within(&prefix, std::time::Duration::ZERO).is_err());
        drop(successor);
        assert!(
            path.is_file(),
            "the lock inode remains the next writer's rendezvous"
        );
        drop(Lock::acquire(&prefix).unwrap());
        let legacy = prefix.root.join(".abrew-lock.d");
        std::fs::create_dir(&legacy).unwrap();
        std::fs::write(legacy.join("owner-note"), b"keep").unwrap();
        assert!(Lock::within(&prefix, std::time::Duration::ZERO).is_err());
        assert_eq!(std::fs::read(legacy.join("owner-note")).unwrap(), b"keep");
        std::fs::remove_file(legacy.join("owner-note")).unwrap();
        std::fs::remove_dir(&legacy).unwrap();
        drop(Lock::acquire(&prefix).unwrap());
        #[cfg(unix)]
        {
            std::fs::remove_file(&path).unwrap();
            let foreign = tmp.path().join("foreign");
            std::fs::write(&foreign, b"owner bytes").unwrap();
            std::os::unix::fs::symlink(&foreign, &path).unwrap();
            assert!(Lock::within(&prefix, std::time::Duration::ZERO).is_err());
            assert_eq!(std::fs::read(&foreign).unwrap(), b"owner bytes");
            assert!(path.is_symlink());
        }
    }
}
