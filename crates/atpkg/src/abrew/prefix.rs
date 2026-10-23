// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `abrew` prefix layout, installed-keg census, receipts, pins, and single-writer lock.

use std::path::{Path, PathBuf};

/// Environment variable overriding the `abrew` prefix root.
pub const PREFIX_ENV: &str = "ABREW_PREFIX";
/// Marker filename under the prefix root that disables `abrew`.
pub const DISABLED_FILE: &str = "disabled";

/// Installed formula keg or cask payload record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub name: String,
    pub version: String,
    pub kind: Kind,
    /// Receipt-admitted installed versions.
    pub versions: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Formula,
    Cask,
}

impl Kind {
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Kind::Formula => "formula",
            Kind::Cask => "cask",
        }
    }
}

/// Returns a warning line when `ABREW_PREFIX` is set to an unusable value.
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

/// Returns whether `root` overlaps a real Homebrew prefix or has an uncertain physical path.
#[must_use]
pub fn overlaps_real_brew_prefix(root: &Path) -> bool {
    overlaps_brew_roots(
        root,
        &[
            Path::new("/opt/homebrew"),
            Path::new("/usr/local"),
            Path::new("/home/linuxbrew/.linuxbrew"),
        ],
    )
}

fn overlaps_brew_roots(root: &Path, real: &[&Path]) -> bool {
    fn overlaps(a: &Path, b: &Path) -> bool {
        a.starts_with(b) || b.starts_with(a)
    }
    if real.iter().any(|brew| overlaps(root, brew)) {
        return true;
    }
    let Some(root) = physical_destination(root) else {
        return true;
    };
    real.iter()
        .any(|brew| physical_destination(brew).is_none_or(|brew| overlaps(&root, &brew)))
}

/// Resolve only existing directories; never create or open a destination's contents.
fn physical_destination(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return None;
    }
    let mut existing = path;
    loop {
        match std::fs::symlink_metadata(existing) {
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                existing = existing.parent()?;
            }
            Err(_) => return None,
        }
    }
    let mut chain = Vec::new();
    for ancestor in existing.ancestors() {
        let identity = std::fs::symlink_metadata(ancestor).ok()?;
        if !identity.is_dir() && !identity.file_type().is_symlink() {
            return None;
        }
        chain.push((ancestor, identity));
    }
    let physical = std::fs::canonicalize(existing).ok()?;
    let identity = std::fs::symlink_metadata(&physical).ok()?;
    if !identity.is_dir() || crate::platform::is_reparse(&identity) {
        return None;
    }
    if chain
        .iter()
        .any(|(path, identity)| !super::install::same_path_identity(path, identity))
        || !super::install::same_path_identity(&physical, &identity)
        || std::fs::canonicalize(existing).ok().as_ref() != Some(&physical)
    {
        return None;
    }
    let suffix = path.strip_prefix(existing).ok()?;
    let mut missing = existing.to_path_buf();
    for part in suffix.components() {
        if !matches!(part, std::path::Component::Normal(_)) {
            return None;
        }
        missing.push(part.as_os_str());
        match std::fs::symlink_metadata(&missing) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            _ => return None,
        }
    }
    Some(physical.join(suffix))
}

/// The `abrew` prefix layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prefix {
    pub root: PathBuf,
}

impl Prefix {
    /// Resolves the prefix from `ABREW_PREFIX`, the store layout, or `~/.abrew`.
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

    /// Creates a prefix rooted at `root`.
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

    /// Creates the standing prefix directories idempotently.
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
            self.root.join("Library").join("Fonts"),
            self.root.join("Library").join("Keyboard Layouts"),
            self.root.join("Library").join("Screen Savers"),
            self.root.join("Library").join("PreferencePanes"),
            self.root.join("Library").join("Services"),
            self.root.join("Library").join("QuickLook"),
            self.root.join("Library").join("ColorPickers"),
            self.root.join("Library").join("Dictionaries"),
            self.root.join("Library").join("Input Methods"),
            self.root.join("Library").join("Internet Plug-Ins"),
            self.root.join("Library").join("Spotlight"),
            self.taps_dir(),
            self.applications(),
            self.root.join("Pinned"),
        ];
        ensure_directories(&directories)
    }

    /// Prepares the API metadata cache directory.
    pub(super) fn ensure_api_cache(&self) -> Result<(), String> {
        ensure_directories(&[self.root.clone(), self.cache(), self.cache().join("api")])
    }

    /// Returns all installed packages of `kind`, sorted by name.
    #[must_use]
    pub fn installed(&self, kind: Kind) -> Vec<Installed> {
        self.scan_installed(kind, false).unwrap_or_default()
    }

    /// Returns the checked installed census of `kind`, failing on unreadable or aliased entries.
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
            if is_staging_name(&name) {
                continue;
            }
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
                    if is_staging_name(&version) {
                        continue;
                    }
                    if !super::valid_cask_version(&version)
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

    /// Returns the installed record for `name`, if any version is admitted.
    #[must_use]
    pub fn installed_one(&self, kind: Kind, name: &str) -> Option<Installed> {
        self.installed(kind).into_iter().find(|i| i.name == name)
    }

    /// Returns the first name in `names` already installed as a formula or cask.
    pub(super) fn installed_conflict(&self, names: &[String]) -> Result<Option<String>, String> {
        if names.is_empty() {
            return Ok(None);
        }
        let formulae = self.installed_checked(Kind::Formula)?;
        let casks = self.installed_checked(Kind::Cask)?;
        Ok(names.iter().find_map(|name| {
            let short = super::formula::Formula::short_name(name);
            formulae
                .iter()
                .chain(&casks)
                .any(|row| row.name == short)
                .then(|| short.to_string())
        }))
    }

    /// Returns the owned version that `opt/<name>` links to, if valid.
    #[must_use]
    pub fn optlinked_version(&self, name: &str) -> Option<String> {
        if !super::valid_component(name) {
            return None;
        }
        for base in [
            self.root.clone(),
            self.cellar(),
            self.cellar().join(name),
            self.root.join("opt"),
        ] {
            real_base(&base).ok()?;
        }
        let target = std::fs::read_link(self.opt(name)).ok()?;
        let version = target.file_name()?.to_str()?.to_owned();
        let keg = self.keg(name, &version);
        (target == keg
            && std::fs::symlink_metadata(&keg).is_ok_and(|metadata| metadata.file_type().is_dir())
            && owned_receipt(&keg, Kind::Formula, name, &version))
        .then_some(version)
    }

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
        let keg = self.keg(name, &version);
        (target == keg
            && std::fs::symlink_metadata(&keg).is_ok_and(|metadata| metadata.file_type().is_dir())
            && owned_receipt(&keg, Kind::Formula, name, &version))
        .then_some(version)
    }

    #[must_use]
    /// Returns the tap-qualified name for `inst`.
    pub fn full_name(&self, inst: &Installed) -> String {
        let dir = match inst.kind {
            Kind::Formula => self.keg(&inst.name, &inst.version),
            Kind::Cask => self.cask_dir(&inst.name, &inst.version),
        };
        if let Some(full) = receipt_word(&dir, "full_name")
            && full.contains('/')
        {
            return full;
        }
        match inst.kind {
            Kind::Formula => format!("homebrew/core/{}", inst.name),
            Kind::Cask => format!("homebrew/cask/{}", inst.name),
        }
    }

    /// Returns the checked tap-qualified name for `name`@`version`.
    pub(super) fn full_name_checked(
        &self,
        kind: Kind,
        name: &str,
        version: &str,
    ) -> Result<String, String> {
        if !super::valid_component(name) || !super::valid_cask_version(version) {
            return Err("abrew: invalid installed identity".to_owned());
        }
        let base = match kind {
            Kind::Formula => self.cellar(),
            Kind::Cask => self.caskroom(),
        };
        let package = base.join(name);
        let dir = package.join(version);
        for path in [&self.root, &base, &package, &dir] {
            let metadata = std::fs::symlink_metadata(path)
                .map_err(|e| format!("abrew: cannot inspect {}: {e}", path.display()))?;
            if !metadata.file_type().is_dir() || crate::platform::is_reparse(&metadata) {
                return Err(format!("abrew: {} is not a real directory", path.display()));
            }
        }
        let receipt_path = dir.join("INSTALL_RECEIPT.json");
        let bytes = crate::metadata_io::read_bounded_regular(&receipt_path, 64 * 1024)
            .map_err(|e| format!("abrew: cannot read {}: {e}", receipt_path.display()))?;
        let receipt = aterm_json::from_slice::<aterm_json::Value>(&bytes)
            .map_err(|e| format!("abrew: cannot parse {}: {e}", receipt_path.display()))?;
        let word = |key| receipt.get(key).and_then(aterm_json::Value::as_str);
        if word("installed_by") != Some("abrew")
            || word(match kind {
                Kind::Formula => "name",
                Kind::Cask => "token",
            }) != Some(name)
            || word("version") != Some(version)
        {
            return Err(format!(
                "abrew: unowned install receipt: {}",
                receipt_path.display()
            ));
        }
        if let Some(value) = receipt.get("full_name") {
            let full = value
                .as_str()
                .ok_or_else(|| format!("abrew: invalid full name in {}", receipt_path.display()))?;
            if full != name {
                if !super::valid_name(full)
                    || !full.contains('/')
                    || full.rsplit('/').next() != Some(name)
                {
                    return Err(format!(
                        "abrew: invalid full name in {}",
                        receipt_path.display()
                    ));
                }
                return Ok(full.to_owned());
            }
        }
        Ok(match kind {
            Kind::Formula => format!("homebrew/core/{name}"),
            Kind::Cask => format!("homebrew/cask/{name}"),
        })
    }

    pub fn is_pinned(&self, name: &str) -> bool {
        self.pinned_version(name).is_some()
    }

    /// Returns whether `name` has a pin entry, failing on inspection errors other than `NotFound`.
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

    /// Returns whether `Pinned/<name>` is a dangling symlink into this cellar's `<name>` kegs.
    pub(super) fn pin_link_dangling_ours(&self, name: &str) -> bool {
        if !super::valid_component(name) {
            return false;
        }
        for base in [
            self.root.clone(),
            self.cellar(),
            self.cellar().join(name),
            self.root.join("Pinned"),
        ] {
            if real_base(&base).is_err() {
                return false;
            }
        }
        let Ok(target) = std::fs::read_link(self.root.join("Pinned").join(name)) else {
            return false;
        };
        let Some(version) = target.file_name().and_then(|v| v.to_str()) else {
            return false;
        };
        let keg = self.keg(name, version);
        super::valid_cask_version(version)
            && target == keg
            && matches!(std::fs::symlink_metadata(&keg), Err(error)
                if error.kind() == std::io::ErrorKind::NotFound)
    }

    /// Pins `name` to its current admitted keg.
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
            Ok(_) if self.is_pinned(name) || self.pin_link_dangling_ours(name) => {
                // not block re-pinning forever.
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
        let link = self.root.join("Pinned").join(name);
        if self.pinned_version(name).is_none() && !self.pin_link_dangling_ours(name) {
            // would strand it forever short of a manual rm.
            return Err(format!("abrew: {name} has no owned pin"));
        }
        std::fs::remove_file(&link).map_err(|e| format!("abrew: {e}"))
    }

    /// Writes `INSTALL_RECEIPT.json` into `dir`.
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
        if s.len() > 64 * 1024 {
            return Err("abrew: install receipt exceeds the 64KiB reader limit".to_string());
        }
        write_metadata(&dir.join("INSTALL_RECEIPT.json"), s.as_bytes())
            .map_err(|e| format!("abrew: cannot write receipt in {}: {e}", dir.display()))
    }

    /// Restores owned payloads stranded by an interrupted same-version reinstall.
    pub fn restore_stale_asides(&self) {
        for (base, kind) in [
            (self.cellar(), Kind::Formula),
            (self.caskroom(), Kind::Cask),
        ] {
            let Ok(entries) = std::fs::read_dir(&base) else {
                continue;
            };
            for entry in entries.flatten() {
                if !is_staging_name(&entry.file_name().to_string_lossy())
                    || !entry
                        .file_type()
                        .is_ok_and(|t| t.is_dir() && !t.is_symlink())
                {
                    continue;
                }
                self.restore_aside(kind, &entry.path());
            }
        }
    }

    /// Restore a stranded same-version-reinstall aside: the staging
    /// dir's `payload` is the live keg, set aside by a run that died
    /// before publishing its replacement.
    fn restore_aside(&self, kind: Kind, staging: &Path) {
        let payload = staging.join("payload");
        if !std::fs::symlink_metadata(&payload)
            .is_ok_and(|m| m.file_type().is_dir() && !m.file_type().is_symlink())
        {
            return;
        }
        let Some((name, version)) = receipt_identity(&payload, kind) else {
            return;
        };
        let dest = match kind {
            Kind::Formula => self.keg(&name, &version),
            Kind::Cask => self.cask_dir(&name, &version),
        };
        if std::fs::symlink_metadata(&dest).is_ok() {
            return;
        }
        let Some(parent) = dest.parent() else {
            return;
        };
        match std::fs::symlink_metadata(parent) {
            Ok(m) if m.file_type().is_dir() && !m.file_type().is_symlink() => {}
            Err(_) if std::fs::create_dir(parent).is_ok() => {}
            _ => return,
        }
        if publish_directory(&payload, &dest, false).is_ok() {
            eprintln!("abrew: restored {name} {version} from an interrupted reinstall");
        }
    }

    /// Rebuilds `state.json` from the cellar and caskroom.
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

/// Atomically exchanges or publishes `staged` at `dest`.
pub(super) fn publish_directory(staged: &Path, dest: &Path, replace: bool) -> std::io::Result<()> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        use std::os::unix::ffi::OsStrExt as _;
        let staged = std::ffi::CString::new(staged.as_os_str().as_bytes())?;
        let dest = std::ffi::CString::new(dest.as_os_str().as_bytes())?;
        // SAFETY: `staged` and `dest` are valid NUL-terminated C strings.
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

/// Atomically writes `bytes` to `path` via an exclusive staging file.
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
    for directory in directories {
        real_base(directory)?;
    }
    for directory in directories {
        std::fs::create_dir_all(directory)
            .map_err(|e| format!("abrew: cannot create {}: {e}", directory.display()))?;
    }
    Ok(())
}

/// Whether a directory name is abrew scratch rather than state:
/// `aterm_tempfile`'s default staging prefix, worn by every scratch
/// dir abrew makes inside the prefix (pour staging, download
/// scratch, reinstall asides).
fn is_staging_name(name: &str) -> bool {
    name.starts_with(".tmp")
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

/// Returns whether the receipt in `dir` marks the package as installed on request.
#[must_use]
pub(super) fn receipt_on_request(dir: &Path) -> bool {
    let Ok(bytes) =
        crate::metadata_io::read_bounded_regular(&dir.join("INSTALL_RECEIPT.json"), 64 * 1024)
    else {
        return false;
    };
    let Ok(receipt) = aterm_json::from_slice::<aterm_json::Value>(&bytes) else {
        return false;
    };
    receipt
        .get("installed_on_request")
        .and_then(aterm_json::Value::as_str)
        == Some("true")
}

/// The (name, version) an owned receipt in `dir` claims, when the
/// receipt is abrew's and both are single safe components — the
/// identity a stranded reinstall aside is restored under.
fn receipt_identity(dir: &Path, kind: Kind) -> Option<(String, String)> {
    let bytes =
        crate::metadata_io::read_bounded_regular(&dir.join("INSTALL_RECEIPT.json"), 64 * 1024)
            .ok()?;
    let receipt = aterm_json::from_slice::<aterm_json::Value>(&bytes).ok()?;
    let word = |key| receipt.get(key).and_then(aterm_json::Value::as_str);
    if word("installed_by") != Some("abrew") {
        return None;
    }
    let name = word(match kind {
        Kind::Formula => "name",
        Kind::Cask => "token",
    })?;
    let version = word("version")?;
    if !super::valid_component(name) || !super::valid_cask_version(version) {
        return None;
    }
    Some((name.to_string(), version.to_string()))
}

/// Reads string field `key` from `INSTALL_RECEIPT.json` in `dir`.
pub(super) fn receipt_word(dir: &Path, key: &str) -> Option<String> {
    let bytes =
        crate::metadata_io::read_bounded_regular(&dir.join("INSTALL_RECEIPT.json"), 64 * 1024)
            .ok()?;
    let receipt = aterm_json::from_slice::<aterm_json::Value>(&bytes).ok()?;
    receipt
        .get(key)
        .and_then(aterm_json::Value::as_str)
        .map(str::to_string)
}

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

/// Creates a directory symlink `link -> target`.
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

/// Creates a file symlink `link -> target`.
pub fn symlink_file(target: &Path, link: &Path) -> Result<(), String> {
    symlink_dir(target, link)
}

/// Escapes `s` for inclusion in a JSON string literal.
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

/// Single-writer file lock for an `abrew` prefix.
pub struct Lock {
    _file: crate::lock::Flock,
}

impl Lock {
    pub fn acquire(prefix: &Prefix) -> Result<Lock, String> {
        Self::within(prefix, std::time::Duration::from_secs(60))
    }

    /// Attempts to acquire the prefix lock without blocking.
    pub fn try_acquire(prefix: &Prefix) -> Option<Lock> {
        Self::within(prefix, std::time::Duration::ZERO).ok()
    }

    fn within(prefix: &Prefix, limit: std::time::Duration) -> Result<Lock, String> {
        prefix.ensure()?;
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
    #[cfg(unix)]
    fn real_brew_prefix_overlap_is_detected() {
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        let brew = tmp.path().join("brew");
        let own = tmp.path().join("own");
        std::fs::create_dir(&brew).unwrap();
        std::fs::create_dir(&own).unwrap();
        let roots = [brew.as_path()];
        assert!(overlaps_brew_roots(&brew, &roots));
        assert!(overlaps_brew_roots(&brew.join("new/abrew"), &roots));
        assert!(overlaps_brew_roots(tmp.path(), &roots));
        assert!(!overlaps_brew_roots(&own.join("new/abrew"), &roots));
        assert!(overlaps_brew_roots(&own.join("../brew"), &roots));
        let alias = tmp.path().join("alias");
        symlink_dir(&brew, &alias).unwrap();
        assert!(overlaps_brew_roots(&alias.join("new/abrew"), &roots));
        assert!(overlaps_brew_roots(&brew.join("new"), &[alias.as_path()]));
        let dangling = tmp.path().join("dangling");
        symlink_dir(&tmp.path().join("absent"), &dangling).unwrap();
        assert!(overlaps_brew_roots(&dangling.join("new"), &roots));
        let file = tmp.path().join("file");
        std::fs::write(&file, b"owner file").unwrap();
        assert!(overlaps_brew_roots(&file.join("new"), &roots));
        assert!(overlaps_brew_roots(&own, &[dangling.as_path()]));
        assert_eq!(std::fs::read(&file).unwrap(), b"owner file");
        assert_eq!(std::fs::read_dir(&brew).unwrap().count(), 0);
    }

    #[test]
    #[cfg(unix)]
    fn unpin_removes_our_dangling_pin_but_not_foreign_entries() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let p = Prefix::at(tmp.path());
        p.ensure().unwrap();
        let dir = p.keg("tool", "1.0");
        std::fs::create_dir_all(&dir).unwrap();
        p.write_receipt(
            &dir,
            &[
                ("installed_by", "abrew"),
                ("name", "tool"),
                ("version", "1.0"),
            ],
        )
        .unwrap();
        p.pin("tool").unwrap();
        assert!(p.is_pinned("tool"));
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(!p.is_pinned("tool"));
        p.unpin("tool").unwrap();
        assert!(std::fs::symlink_metadata(p.root.join("Pinned/tool")).is_err());
        std::fs::write(p.root.join("Pinned/foreign"), b"keep").unwrap();
        assert!(p.unpin("foreign").is_err());
        assert_eq!(
            std::fs::read(p.root.join("Pinned/foreign")).unwrap(),
            b"keep"
        );
        symlink_dir(std::path::Path::new("/tmp"), &p.root.join("Pinned/outward")).unwrap();
        assert!(p.unpin("outward").is_err());
        assert!(std::fs::symlink_metadata(p.root.join("Pinned/outward")).is_ok());

        std::fs::create_dir_all(&dir).unwrap();
        p.write_receipt(
            &dir,
            &[
                ("installed_by", "abrew"),
                ("name", "tool"),
                ("version", "1.0"),
            ],
        )
        .unwrap();
        let live = p.keg("tool", "unowned");
        std::fs::create_dir_all(&live).unwrap();
        std::fs::write(live.join("owner-note"), b"keep").unwrap();
        let version_link = p.keg("tool", "linked");
        let absent = tmp.path().join("foreign-missing");
        symlink_dir(&absent, &version_link).unwrap();
        let marker = p.root.join("Pinned/tool");
        let mut version_slash = version_link.as_os_str().to_os_string();
        version_slash.push("/");
        let targets = [
            live.clone(),
            p.cellar().join("tool/../../foreign-missing"),
            p.keg("tool", "missing").join("nested"),
            version_link.clone(),
            PathBuf::from(version_slash),
            version_link.join("."),
        ];
        for live_alias in [false, true] {
            if live_alias {
                std::fs::create_dir_all(&absent).unwrap();
                std::fs::write(absent.join("owner-note"), b"keep").unwrap();
                std::fs::write(
                    absent.join("INSTALL_RECEIPT.json"),
                    r#"{"installed_by":"abrew","name":"tool","version":"linked"}"#,
                )
                .unwrap();
            }
            for target in &targets {
                symlink_dir(target, &marker).unwrap();
                assert!(p.pinned_version("tool").is_none(), "{target:?}");
                assert!(!p.pin_link_dangling_ours("tool"), "{target:?}");
                assert!(p.pin("tool").is_err(), "{target:?}");
                assert!(p.unpin("tool").is_err(), "{target:?}");
                assert_eq!(std::fs::read_link(&marker).unwrap(), *target);
                std::fs::remove_file(&marker).unwrap();
            }
        }
        assert_eq!(std::fs::read(live.join("owner-note")).unwrap(), b"keep");
        assert_eq!(std::fs::read(absent.join("owner-note")).unwrap(), b"keep");
        assert_eq!(std::fs::read_link(&version_link).unwrap(), absent);
        std::fs::remove_file(&version_link).unwrap();
        std::fs::remove_dir_all(p.cellar().join("tool")).unwrap();
        for version in ["1.0", "1,2"] {
            symlink_dir(&p.keg("tool", version), &marker).unwrap();
            assert!(p.pin_link_dangling_ours("tool"));
            p.unpin("tool").unwrap();
            assert!(std::fs::symlink_metadata(&marker).is_err());
        }
    }

    #[test]
    fn full_name_uses_receipt_then_official_tap() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let p = Prefix::at(tmp.path());
        p.ensure().unwrap();
        let dir = p.keg("tool", "1.0");
        std::fs::create_dir_all(&dir).unwrap();
        p.write_receipt(
            &dir,
            &[
                ("installed_by", "abrew"),
                ("name", "tool"),
                ("version", "1.0"),
                ("full_name", "someone/tap/tool"),
            ],
        )
        .unwrap();
        let inst = p.installed_one(Kind::Formula, "tool").unwrap();
        assert_eq!(p.full_name(&inst), "someone/tap/tool");
        let dir = p.keg("wget", "1.25");
        std::fs::create_dir_all(&dir).unwrap();
        p.write_receipt(
            &dir,
            &[
                ("installed_by", "abrew"),
                ("name", "wget"),
                ("version", "1.25"),
                ("full_name", "wget"),
            ],
        )
        .unwrap();
        let inst = p.installed_one(Kind::Formula, "wget").unwrap();
        assert_eq!(p.full_name(&inst), "homebrew/core/wget");
        let dir = p.cask_dir("firefox", "128.0");
        std::fs::create_dir_all(&dir).unwrap();
        p.write_receipt(
            &dir,
            &[
                ("installed_by", "abrew"),
                ("token", "firefox"),
                ("version", "128.0"),
            ],
        )
        .unwrap();
        let inst = p.installed_one(Kind::Cask, "firefox").unwrap();
        assert_eq!(p.full_name(&inst), "homebrew/cask/firefox");
    }

    #[test]
    fn staging_dirs_are_invisible_to_the_installed_census() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let p = Prefix::at(tmp.path());
        p.ensure().unwrap();
        let dir = p.keg("hello", "2.12.3");
        std::fs::create_dir_all(&dir).unwrap();
        p.write_receipt(
            &dir,
            &[
                ("installed_by", "abrew"),
                ("name", "hello"),
                ("version", "2.12.3"),
            ],
        )
        .unwrap();
        let staged = aterm_tempfile::TempDir::new_in(p.cellar()).unwrap().keep();
        std::fs::create_dir_all(staged.join("go/1.27.3")).unwrap();
        let checked = p.installed_checked(Kind::Formula).unwrap();
        assert_eq!(checked.len(), 1);
        assert_eq!(checked[0].name, "hello");
        assert!(p.installed_checked(Kind::Cask).unwrap().is_empty());
        let stray = p.cellar().join("hello").join(".tmp1234_deadbeef_0");
        std::fs::create_dir_all(&stray).unwrap();
        let checked = p.installed_checked(Kind::Formula).unwrap();
        assert_eq!(checked[0].versions, vec!["2.12.3".to_string()]);
    }

    #[test]
    fn stranded_reinstall_aside_is_restored_without_discarding_other_payloads() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let p = Prefix::at(tmp.path());
        p.ensure().unwrap();
        let aside = |receipt: &[(&str, &str)], marker: &str| {
            let staged = aterm_tempfile::TempDir::new_in(p.cellar()).unwrap().keep();
            let payload = staged.join("payload");
            std::fs::create_dir_all(&payload).unwrap();
            p.write_receipt(&payload, receipt).unwrap();
            std::fs::write(payload.join("MARKER"), marker).unwrap();
            staged
        };
        let mut foreign = Vec::new();
        for base in [p.cellar(), p.caskroom(), p.cache(), p.cache().join("api")] {
            let dir = base.join(".tmp-owner-work");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("KEEP"), b"owner bytes").unwrap();
            foreign.push(dir);
        }
        let staged = aside(
            &[
                ("installed_by", "abrew"),
                ("name", "hello"),
                ("version", "2.12.3"),
            ],
            "old-keg",
        );
        p.restore_stale_asides();
        assert!(staged.exists());
        assert!(!staged.join("payload").exists());
        for dir in foreign {
            assert_eq!(std::fs::read(dir.join("KEEP")).unwrap(), b"owner bytes");
        }
        let live = p.keg("hello", "2.12.3");
        assert_eq!(
            std::fs::read_to_string(live.join("MARKER")).unwrap(),
            "old-keg"
        );
        let checked = p.installed_checked(Kind::Formula).unwrap();
        assert_eq!(checked.len(), 1);
        assert_eq!(checked[0].name, "hello");
        std::fs::write(live.join("MARKER"), "new-keg").unwrap();
        let staged = aside(
            &[
                ("installed_by", "abrew"),
                ("name", "hello"),
                ("version", "2.12.3"),
            ],
            "old-keg",
        );
        p.restore_stale_asides();
        assert!(staged.exists());
        assert_eq!(
            std::fs::read(staged.join("payload/MARKER")).unwrap(),
            b"old-keg"
        );
        assert_eq!(
            std::fs::read_to_string(live.join("MARKER")).unwrap(),
            "new-keg"
        );
        let staged = aside(
            &[
                ("installed_by", "another"),
                ("name", "evil"),
                ("version", "9.9"),
            ],
            "foreign",
        );
        p.restore_stale_asides();
        assert!(staged.exists());
        assert_eq!(
            std::fs::read(staged.join("payload/MARKER")).unwrap(),
            b"foreign"
        );
        assert!(!p.keg("evil", "9.9").exists());
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
            p.pin("wget").unwrap();
            assert_eq!(p.pinned_version("wget").as_deref(), Some("1.25.0"));
            p.unpin("wget").unwrap();
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
