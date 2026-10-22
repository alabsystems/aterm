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

/// Refuse overlap with a real Homebrew tree, or an uncertain physical location.
/// Existing ancestors are resolved before comparing an uncreated suffix. Raw `..`
/// is refused: collapsing it lexically would disagree with symlink traversal.
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
/// Recheck the observed chain and missing suffix before returning. This is admission,
/// not an atomic filesystem transaction with independent writers after the check.
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
            // Staging dirs left by an interrupted run are scratch,
            // never installed rows: a killed pour must not wedge
            // every later command (found live). Readers do not see
            // them; anonymous leftovers are retained without a custody witness.
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

    /// The installed record for one name, if any version stands.
    #[must_use]
    pub fn installed_one(&self, kind: Kind, name: &str) -> Option<Installed> {
        self.installed(kind).into_iter().find(|i| i.name == name)
    }

    /// A conflict can be absent only after both installed censuses are complete.
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

    /// The admitted version named by this engine's pin marker. A foreign file,
    /// link outside this package, or dangling/unowned target grants no ownership.
    /// The version `opt/<name>` points at, proven the same way as
    /// a pin: exact own-shape target into an owned, receipted keg.
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
    /// The tap-qualified identity `list --full-name` prints: the
    /// receipt's `full_name` when it is qualified, else the official
    /// tap for the kind (homebrew/core API rows and cask tokens
    /// record the short name; receipts predating the field fall back
    /// the same way).
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

    /// A retained version's admitted tap identity, for destructive decisions.
    /// Unlike `full_name`, an unreadable or malformed receipt never becomes
    /// an official-tap fallback. Legacy receipts with no field, and the
    /// official API's exact short name, keep their established identity.
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

    /// Whether the Pinned/<name> entry is a dangling symlink of
    /// abrew's own shape (into this Cellar's <name> kegs) whose
    /// target keg is gone. Such a link holds no owner bytes and
    /// may be replaced/removed; a foreign file or an outward
    /// symlink at the pin path is never touched.
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
        // Only the missing keg itself qualifies: lexical descendants can escape
        // through `..`, and a live unowned keg or a dangling version symlink is
        // still foreign. Failed inspection is not evidence of absence.
        let keg = self.keg(name, version);
        super::valid_cask_version(version)
            && target == keg
            && matches!(std::fs::symlink_metadata(&keg), Err(error)
                if error.kind() == std::io::ErrorKind::NotFound)
    }

    /// Pin the current admitted keg. An occupied marker is replaced only when
    /// it already names an admitted keg or a missing keg of this package.
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
                // A live owned pin refreshes; a dangling pin of
                // ours (keg hand-deleted, then reinstalled) must
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
            // A dangling pin of ours is still removable — refusing
            // would strand it forever short of a manual rm.
            return Err(format!("abrew: {name} has no owned pin"));
        }
        std::fs::remove_file(&link).map_err(|e| format!("abrew: {e}"))
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
        // The ownership readers admit at most 64KiB; never publish a receipt
        // which those readers cannot recognize after a successful install.
        if s.len() > 64 * 1024 {
            return Err("abrew: install receipt exceeds the 64KiB reader limit".to_string());
        }
        write_metadata(&dir.join("INSTALL_RECEIPT.json"), s.as_bytes())
            .map_err(|e| format!("abrew: cannot write receipt in {}: {e}", dir.display()))
    }

    /// Restore owned payloads from an interrupted same-version reinstall.
    /// The writer lock excludes a cooperating live writer; the payload's
    /// receipt supplies its destination identity. Anonymous staging directories
    /// and any payload that cannot be restored are retained.
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
    /// before publishing its replacement. Restored only under the
    /// identity its own owned receipt claims, only when no live keg
    /// stands there now (a replacement that did publish wins), and
    /// never through a symlinked payload or parent. Keep the surrounding
    /// directory and every payload that cannot be restored.
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

/// Whether a directory name is abrew scratch rather than state:
/// `aterm_tempfile`'s default staging prefix, worn by every scratch
/// dir abrew makes inside the prefix (pour staging, download
/// scratch, reinstall asides). Exercised by
/// `staging_dirs_are_invisible_to_the_installed_census`, which stages through a
/// real `TempDir`: if the default prefix ever changes, that test
/// fails instead of this check silently going stale.
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

/// A directory alone is not an installed package. Only the receipt this engine
/// writes, naming this exact package/version, admits it to mutation and cleanup.
/// Whether the owned receipt for `name`@`version` says it was installed
/// on request (vs pulled in as a dependency).
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
/// identity a stranded reinstall aside is restored under. Anything
/// else (foreign, malformed, unsafe spelling) restores nothing.
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

/// One string field of the receipt in `dir`, if the receipt reads
/// and parses. Ownership checks stay with [`owned_receipt`]; this
/// only reads.
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

    /// The lock only if free right now: diagnostics must never wait
    /// behind (or fail because of) a live writer they exist to diagnose.
    pub fn try_acquire(prefix: &Prefix) -> Option<Lock> {
        Self::within(prefix, std::time::Duration::ZERO).ok()
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
        // Lexical parents are refused before resolving any possible alias.
        assert!(overlaps_brew_roots(&own.join("../brew"), &roots));
        let alias = tmp.path().join("alias");
        symlink_dir(&brew, &alias).unwrap();
        assert!(overlaps_brew_roots(&alias.join("new/abrew"), &roots));
        // The Homebrew root itself may be reached through an alias.
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
        // The keg vanishes (hand-deleted): the pin dangles, is
        // inert, and unpin must still be able to remove it.
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(!p.is_pinned("tool"));
        p.unpin("tool").unwrap();
        assert!(std::fs::symlink_metadata(p.root.join("Pinned/tool")).is_err());
        // A foreign regular file at the pin path is not ours.
        std::fs::write(p.root.join("Pinned/foreign"), b"keep").unwrap();
        assert!(p.unpin("foreign").is_err());
        assert_eq!(
            std::fs::read(p.root.join("Pinned/foreign")).unwrap(),
            b"keep"
        );
        // Nor is a symlink pointing outside this Cellar.
        symlink_dir(std::path::Path::new("/tmp"), &p.root.join("Pinned/outward")).unwrap();
        assert!(p.unpin("outward").is_err());
        assert!(std::fs::symlink_metadata(p.root.join("Pinned/outward")).is_ok());

        // A same-Cellar spelling alone is no ownership: a live unowned keg,
        // an escaping target and an existing version symlink must all survive
        // both mutations, even with an admitted version available for pin.
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
                // A receipt-looking foreign target still cannot turn a version
                // symlink into an admitted physical keg through `/` or `/.`.
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
        // Removing the whole package directory is recoverable too: its missing
        // parent is accepted, while linked or unreadable parents still refuse.
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
        // A tap formula: the receipt's qualified identity wins.
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
        // A core formula records the short name; the official tap
        // is supplied for display.
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
        // A receipt predating the field falls back the same way.
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
        // A killed pour's staging, made the way the pour makes it:
        // a real TempDir inside the cellar holding a receipt-less
        // keg-shaped subtree. `.keep()` survives the "kill".
        let staged = aterm_tempfile::TempDir::new_in(p.cellar()).unwrap().keep();
        std::fs::create_dir_all(staged.join("go/1.27.3")).unwrap();
        // The checked census must not wedge on it, in either kind.
        let checked = p.installed_checked(Kind::Formula).unwrap();
        assert_eq!(checked.len(), 1);
        assert_eq!(checked[0].name, "hello");
        assert!(p.installed_checked(Kind::Cask).unwrap().is_empty());
        // A staging-shaped version row inside a real keg is skipped,
        // not an "incomplete row" error.
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
        // The aside a killed same-version reinstall leaves: the live
        // keg (receipt and all) at <staging>/payload, nothing at the
        // live path.
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
        // When a replacement did publish, it wins; the older payload
        // is retained and the live keg is untouched.
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
        // A foreign receipt restores nothing.
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
            // A dangling pin of abrew's own shape (into this
            // Cellar, keg gone) holds no owner bytes: pin replaces
            // it and unpin removes it, instead of stranding the
            // name forever. Foreign entries above still refuse.
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
