// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Windows release handoff: the portable zip `release-build` ([`crate::release`]) makes
//! of one claimed release build, and the receipt beside it, which the release cutter on the
//! Mac admits (`targo --unverified ship cut --windows-artifacts DIRECTORY`,
//! crates/aterm-release/src/windows_release.rs) as it admits a hand-carried Linux pair
//! (crates/aterm-release/src/linux_release.rs, `Handoff::import`).
//!
//! One definition serves both ends: this crate writes the receipt on Windows, and the cutter,
//! which links this crate, reads it on macOS, so the two agree by compilation.
//!
//! A handoff directory holds exactly one pair: [`TARGET`]'s zip for the version
//! (`aterm-<version>-windows-x86_64.zip`, `WindowsTarget::asset_name`) and its receipt
//! ([`receipt_name`]). [`admit`] holds the receipt to the cut (its version, build and commit,
//! the commit's tree), its compiler to the commit's pins ([`Pins`]), and the zip to the
//! receipt (a regular file of its size and SHA-256) and to what a Windows release is
//! (`WindowsTarget::validate_zip`). A name starting with `.` (a `.DS_Store` a Finder copy
//! leaves) is no part of a carry and is passed over; any other name is refused.
//!
//! Admission proves the pair agrees with itself and the cut, not who wrote it: the cutter also
//! holds the zip to the SHA-256 the person read off the Windows host's console
//! ([`Receipt::check_carried`]), which whoever can write where the pair travels cannot set.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use aterm_update_core::windows::{Subsystem, WINDOWS_ZIP_MAX_BYTES, WindowsTarget};
use serde::{Deserialize, Serialize};

/// The receipt's schema.
pub const SCHEMA: u32 = 1;

/// The one Windows target a release publishes.
pub const TARGET: WindowsTarget = WindowsTarget::X86_64;

/// The largest receipt [`read_receipt`] reads.
pub const RECEIPT_MAX_BYTES: u64 = 64 * 1024;

/// The zip member that carries get-aterm.ps1's signature checker, `aterm-bootstrap-verify`
/// built for [`TARGET`] (`apps\aterm-win\build.ps1` lays it beside the images). The cutter
/// publishes it as its own asset and signs its digest into the appcast; carried inside the
/// zip, it is bound by the zip's SHA-256 the person names (`--windows-sha256`), as the images
/// are, with no second digest to type.
pub const VERIFIER_MEMBER: &str = "aterm/aterm-bootstrap-verify.exe";

/// The largest verifier [`zip_verifier`] reads: `aterm_bootstrap_verify::pin::MAX_ASSET`,
/// the most get-aterm.ps1 downloads.
pub const VERIFIER_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// The verifier the portable zip `zip` carries ([`VERIFIER_MEMBER`]): its bytes, a console
/// image of [`TARGET`] of at most [`VERIFIER_MAX_BYTES`].
///
/// # Errors
/// A zip that carries none, or one that is not that.
pub fn zip_verifier<R: Read + Seek>(zip: R) -> Result<Vec<u8>, String> {
    let mut archive =
        aterm_codec::zip::Archive::open(zip).map_err(|e| format!("the zip is unreadable: {e}"))?;
    let entry = archive
        .find(VERIFIER_MEMBER.as_bytes())
        .map_err(|e| format!("the zip is unreadable: {e}"))?
        .ok_or_else(|| {
            format!(
                "the zip carries no {VERIFIER_MEMBER}, the signature checker get-aterm.ps1 \
                 runs: build it with this tree's apps\\aterm-win\\build.ps1 -Zip"
            )
        })?;
    let bytes = archive
        .read(&entry, VERIFIER_MAX_BYTES)
        .map_err(|e| format!("the zip's {VERIFIER_MEMBER}: {e}"))?;
    TARGET
        .validate_pe_header(&bytes, Subsystem::Console)
        .map_err(|why| format!("the zip's {VERIFIER_MEMBER}: {why}"))?;
    Ok(bytes)
}

/// The receipt's file name beside the zip `asset`.
#[must_use]
pub fn receipt_name(asset: &str) -> String {
    format!("{asset}.handoff.toml")
}

/// The toolchain lane a Windows release is built in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// STOCK EXCEPTION: the stock release the commit pins
    /// (`aterm_toolchain::rustup::STOCK_PIN`), which `apps\aterm-win\build.ps1 -Toolchain
    /// stock` builds with.
    Stock,
    /// The Trust build the commit's `trust.lock` pins.
    Trust,
}

impl Lane {
    /// The word the receipt carries, and `build.ps1 -Toolchain` takes.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Stock => "stock",
            Self::Trust => "trust",
        }
    }

    /// The lane a receipt's word names.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        [Self::Stock, Self::Trust]
            .into_iter()
            .find(|lane| lane.word() == word)
    }
}

/// The file, in a commit, that pins the STOCK EXCEPTION release.
pub const STOCK_PIN_FILE: &str = "crates/aterm-toolchain/src/stock_pin.rs";

/// The file, in a commit, that pins the Trust build.
pub const TRUST_LOCK_FILE: &str = "trust.lock";

/// The stock release [`STOCK_PIN_FILE`]'s text pins: its one
/// `pub const STOCK_PIN: &str = "…";` line, read as `build.ps1` (`Resolve-Stock`) reads it,
/// so the release the build used and the release the cutter admits are one reading of one
/// file at the commit.
#[must_use]
pub fn stock_pin(source: &str) -> Option<&str> {
    let mut pins = source.lines().filter_map(|line| {
        let rest = line
            .trim_start()
            .strip_prefix("pub const STOCK_PIN: &str = \"")?;
        let (pin, after) = rest.split_once('"')?;
        (after.starts_with(';') && !pin.is_empty()).then_some(pin)
    });
    let first = pins.next()?;
    pins.next().is_none().then_some(first)
}

/// The compilers a cut admits, as its commit pins them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pins<'a> {
    /// The stock release of the STOCK EXCEPTION lane ([`stock_pin`] of the commit's
    /// [`STOCK_PIN_FILE`]).
    pub stock: &'a str,
    /// The commit's [`TRUST_LOCK_FILE`]'s `compiler_commit`: the `commit-hash` its Trust build
    /// answers in `-vV`. `None` when the commit carries no lock.
    pub trust_commit: Option<&'a str>,
}

/// What a cut is: the identity a receipt must name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cut<'a> {
    /// The release version.
    pub version: &'a str,
    /// The claimed build number.
    pub build: u64,
    /// The release commit, forty lowercase hex digits.
    pub commit: &'a str,
    /// That commit's root tree, as `git rev-parse <commit>^{tree}` names it.
    pub source_tree: &'a str,
}

/// The receipt `release-build` writes beside the zip, shaped like a Linux handoff's
/// (crates/aterm-release/src/linux_release.rs `Artifact`).
///
/// A key it does not know is refused: a receipt of another shape is another tool's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    /// [`SCHEMA`].
    pub schema: u32,
    /// The release version the images carry.
    pub version: String,
    /// The claimed build number the images carry.
    pub build_number: u64,
    /// The commit the images were built from, forty lowercase hex digits.
    pub commit: String,
    /// [`TARGET`]'s triple.
    pub target: String,
    /// The zip's name, [`TARGET`]'s asset name for [`Receipt::version`].
    pub asset: String,
    /// The zip's SHA-256, lowercase hex.
    pub sha256: String,
    /// The zip's size in bytes.
    pub size: u64,
    /// The commit's root tree: what `build.ps1` compiled, in a checkout found clean at the
    /// commit before and after the build. (A Linux receipt's `source_fingerprint` is the
    /// cutter's own take of the commit, which only the Unix cutter computes; the tree is
    /// what git, on both ends, names for the same bytes.)
    pub source_tree: String,
    /// The toolchain lane, [`Lane::word`].
    pub lane: String,
    /// The compiler's whole `-vV` answer.
    pub compiler_version: String,
    /// SHA-256 of the compiler executable (`rustc.exe`, or Trust's `trustc.exe`).
    pub compiler_sha256: String,
    /// SHA-256 of the build driver (`cargo.exe`, or Trust's `targo.exe`).
    pub driver_sha256: String,
    /// SHA-256 of the compiler's library (`rustc_driver-*.dll`).
    pub compiler_library_sha256: String,
}

/// The value of the one `key: value` line of a compiler's `-vV` answer; `None` when no line
/// or more than one has that key.
#[must_use]
pub fn compiler_line<'a>(version: &'a str, key: &str) -> Option<&'a str> {
    let mut found = version.lines().filter_map(|line| {
        line.strip_prefix(key)
            .and_then(|rest| rest.strip_prefix(": "))
            .map(str::trim)
    });
    let first = found.next()?;
    found.next().is_none().then_some(first)
}

/// Whether a compiler that answered `version` to `-vV` may build a Windows release in `lane`
/// under `pins`: a native [`TARGET`] compiler, and for [`Lane::Stock`] exactly the pinned
/// stock release with no Trust line, for [`Lane::Trust`] exactly the Trust build the lock
/// pins (its `commit-hash`).
///
/// # Errors
/// Which of those it is not.
pub fn check_compiler(lane: Lane, version: &str, pins: &Pins<'_>) -> Result<(), String> {
    let host = compiler_line(version, "host");
    if host != Some(TARGET.triple()) {
        return Err(format!(
            "the compiler is not a native {} compiler (its -vV says host {host:?})",
            TARGET.triple()
        ));
    }
    let trust = compiler_line(version, "trust");
    match lane {
        Lane::Stock => {
            if let Some(trust) = trust {
                return Err(format!(
                    "a Trust compiler ({trust}) is no STOCK EXCEPTION build: the stock lane is \
                     upstream release {}",
                    pins.stock
                ));
            }
            let release = compiler_line(version, "release");
            if release != Some(pins.stock) {
                return Err(format!(
                    "the STOCK EXCEPTION lane builds with stock {}, the release the commit pins \
                     (crates/aterm-toolchain/src/stock_pin.rs); this compiler is release \
                     {release:?}",
                    pins.stock
                ));
            }
        }
        Lane::Trust => {
            if trust.is_none() {
                return Err("the Trust lane's compiler prints no trust: line in -vV".into());
            }
            let Some(pinned) = pins.trust_commit else {
                return Err(
                    "the commit carries no trust.lock, so it pins no Trust build to admit".into(),
                );
            };
            let commit = compiler_line(version, "commit-hash");
            if commit != Some(pinned) {
                return Err(format!(
                    "this Trust compiler (commit-hash {commit:?}) is not the build the commit's \
                     trust.lock pins (compiler_commit {pinned}); a Trust the commit does not pin \
                     is admitted in neither lane"
                ));
            }
        }
    }
    Ok(())
}

/// `text`, the `--windows-sha256` a person typed, as a lowercase SHA-256: 64 hex digits in
/// either case (`Get-FileHash` prints upper case, `release-build` lower case), nothing else.
///
/// # Errors
/// Text that is not 64 hex digits.
pub fn carried_sha256(text: &str) -> Result<String, String> {
    let lower = text.to_ascii_lowercase();
    if aterm_codec::hex::is_lower(&lower, 64) {
        Ok(lower)
    } else {
        Err(format!(
            "--windows-sha256 {text:?} is not a SHA-256 (64 hex digits): name the zip's digest \
             release-build printed on the Windows host"
        ))
    }
}

fn is_hex(text: &str, lens: &[usize]) -> bool {
    lens.iter()
        .any(|&len| aterm_codec::hex::is_lower(text, len))
}

impl Receipt {
    /// The receipt's lane.
    ///
    /// # Errors
    /// A word that names no lane.
    pub fn lane(&self) -> Result<Lane, String> {
        Lane::parse(&self.lane).ok_or_else(|| {
            format!(
                "the Windows receipt names the toolchain lane {:?}, neither stock nor trust",
                self.lane
            )
        })
    }

    /// The receipt is of release `version`, build `build`, commit `commit`, for [`TARGET`]:
    /// its schema, identity, target and canonical asset name, a size in 64 bytes to
    /// `WINDOWS_ZIP_MAX_BYTES`, every digest 64 lowercase hex digits, a lane, and a compiler
    /// that names [`TARGET`] as its host.
    ///
    /// # Errors
    /// The first of those it is not.
    pub fn check_identity(&self, version: &str, build: u64, commit: &str) -> Result<(), String> {
        if build == 0 || !aterm_codec::hex::is_lower(commit, 40) {
            return Err(
                "a Windows handoff is of a positive build number and a full lowercase commit"
                    .into(),
            );
        }
        if self.schema != SCHEMA {
            return Err(format!(
                "the Windows receipt is schema {}, and this reads schema {SCHEMA}",
                self.schema
            ));
        }
        if self.version != version || self.build_number != build || self.commit != commit {
            return Err(format!(
                "the Windows receipt is of v{} build {} at {}, not this cut's v{version} build \
                 {build} at {commit}",
                self.version, self.build_number, self.commit
            ));
        }
        let asset = TARGET.asset_name(version);
        if self.target != TARGET.triple() || self.asset != asset {
            return Err(format!(
                "the Windows receipt names {} for {}, not {asset} for {}",
                self.asset,
                self.target,
                TARGET.triple()
            ));
        }
        if !(64..=WINDOWS_ZIP_MAX_BYTES).contains(&self.size) {
            return Err(format!(
                "the Windows receipt's zip is {} bytes, outside 64..={WINDOWS_ZIP_MAX_BYTES}",
                self.size
            ));
        }
        for (what, digest) in [
            ("zip", &self.sha256),
            ("compiler", &self.compiler_sha256),
            ("driver", &self.driver_sha256),
            ("compiler library", &self.compiler_library_sha256),
        ] {
            if !aterm_codec::hex::is_lower(digest, 64) {
                return Err(format!(
                    "the Windows receipt's {what} SHA-256 {digest:?} is not 64 lowercase hex \
                     digits"
                ));
            }
        }
        if !is_hex(&self.source_tree, &[40, 64]) {
            return Err(format!(
                "the Windows receipt's source tree {:?} is no git object id",
                self.source_tree
            ));
        }
        self.lane()?;
        let host = compiler_line(&self.compiler_version, "host");
        if host != Some(TARGET.triple()) {
            return Err(format!(
                "the Windows receipt's compiler is not a native {} compiler (host {host:?})",
                TARGET.triple()
            ));
        }
        Ok(())
    }

    /// The receipt's sources are the cut's: the commit's tree is `source_tree`, and its
    /// compiler is one its lane admits under `pins` ([`check_compiler`]).
    ///
    /// # Errors
    /// Another tree, or a compiler its lane does not admit.
    pub fn check_sources(&self, source_tree: &str, pins: &Pins<'_>) -> Result<(), String> {
        if self.source_tree != source_tree {
            return Err(format!(
                "the Windows receipt's source tree {} is not the commit's tree {source_tree}",
                self.source_tree
            ));
        }
        check_compiler(self.lane()?, &self.compiler_version, pins)
            .map_err(|why| format!("the Windows receipt's compiler is refused: {why}"))
    }

    /// The receipt's zip is the one the person who built it named: `carried`, the SHA-256
    /// `release-build` printed on the Windows host, typed on the Mac (`--windows-sha256`).
    ///
    /// The pair travels by hand (a stick, a share, a synced folder), and every field of a
    /// receipt is public or the writer's own word, so anyone who can write where it travels
    /// can put a pair there that agrees with itself and is admitted. The digest read off the
    /// Windows host's console, and not off the carried files, is the one thing such a writer
    /// cannot set.
    ///
    /// # Errors
    /// No digest named, or another than the receipt's zip has.
    pub fn check_carried(&self, carried: Option<&str>) -> Result<(), String> {
        let Some(carried) = carried else {
            return Err(format!(
                "the Windows pair came by hand, which proves nothing about who wrote it: name \
                 its zip's SHA-256 as release-build printed it on the Windows host (read there, \
                 not off the carried files) with --windows-sha256; this receipt says {}",
                self.sha256
            ));
        };
        if carried != self.sha256 {
            return Err(format!(
                "the carried Windows zip has SHA-256 {}, not {carried}, the one --windows-sha256 \
                 names: it is not the zip release-build printed on the Windows host (or the \
                 digest was mistyped), and nothing was admitted",
                self.sha256
            ));
        }
        Ok(())
    }

    /// The file at `path` is the receipt's zip: a regular file (not a link) of its size and
    /// SHA-256, a portable zip of [`TARGET`] (`WindowsTarget::validate_zip`), and one that
    /// carries the verifier ([`zip_verifier`]).
    ///
    /// # Errors
    /// Any of those it is not, or a file it cannot read.
    pub fn verify_zip(&self, path: &Path) -> Result<(), String> {
        let (sha256, size, mut file) = hash_regular(path, WINDOWS_ZIP_MAX_BYTES)?;
        if size != self.size {
            return Err(format!(
                "{} is {size} bytes, not the receipt's {}",
                path.display(),
                self.size
            ));
        }
        if sha256 != self.sha256 {
            return Err(format!(
                "{} has SHA-256 {sha256}, not the receipt's {}",
                path.display(),
                self.sha256
            ));
        }
        file.seek(SeekFrom::Start(0))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        TARGET
            .validate_zip(&mut file)
            .map_err(|why| format!("{}: {why}", path.display()))?;
        file.seek(SeekFrom::Start(0))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        zip_verifier(&mut file)
            .map(drop)
            .map_err(|why| format!("{}: {why}", path.display()))
    }

    /// The receipt as the TOML [`read_receipt`] reads.
    ///
    /// # Errors
    /// One the serializer refuses.
    pub fn to_toml(&self) -> Result<String, String> {
        aterm_toml::to_string(self).map_err(|e| format!("Windows receipt: {e}"))
    }
}

/// The SHA-256 and size of the regular file at `path`, which must not be a link and must
/// hold at most `max` bytes, and the file itself, open.
///
/// # Errors
/// A path that is not a regular file, a file over `max`, or one that cannot be read.
pub fn hash_regular(path: &Path, max: u64) -> Result<(String, u64, File), String> {
    let shown = || path.display().to_string();
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", shown()))?;
    if !meta.file_type().is_file() {
        return Err(format!("{} is not a regular file", shown()));
    }
    let mut file = File::open(path).map_err(|e| format!("{}: {e}", shown()))?;
    let opened = file.metadata().map_err(|e| format!("{}: {e}", shown()))?;
    if !opened.is_file() || opened.len() > max {
        return Err(format!(
            "{} is not a regular file of at most {max} bytes",
            shown()
        ));
    }
    let mut digest = aterm_digest::Sha256::new();
    let mut size = 0_u64;
    let mut buf = vec![0_u8; 1 << 20];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("{}: {e}", shown()))?;
        if n == 0 {
            break;
        }
        size += n as u64;
        if size > max {
            return Err(format!("{} grew past {max} bytes as it was read", shown()));
        }
        digest.update(&buf[..n]);
    }
    Ok((
        aterm_codec::hex::encode_lower(&digest.finalize()),
        size,
        file,
    ))
}

/// The receipt at `path`: a regular file of at most [`RECEIPT_MAX_BYTES`], UTF-8, TOML of
/// [`Receipt`]'s shape.
///
/// # Errors
/// Any of those it is not.
pub fn read_receipt(path: &Path) -> Result<Receipt, String> {
    let (_, _, mut file) = hash_regular(path, RECEIPT_MAX_BYTES)?;
    file.seek(SeekFrom::Start(0))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    aterm_toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// The names of the pair for `version` that `directory` lacks: the zip, then the receipt.
///
/// # Errors
/// A directory that cannot be read for them.
pub fn missing(directory: &Path, version: &str) -> Result<Vec<String>, String> {
    let asset = TARGET.asset_name(version);
    let mut lacking = Vec::new();
    for name in [asset.clone(), receipt_name(&asset)] {
        match std::fs::symlink_metadata(directory.join(&name)) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => lacking.push(name),
            Err(e) => return Err(format!("{}: {e}", directory.join(&name).display())),
        }
    }
    Ok(lacking)
}

/// Admit `directory` as `cut`'s Windows handoff under `pins`: it holds [`TARGET`]'s zip for
/// the version and its receipt and no other name (one starting with `.` is passed over); the
/// receipt is the cut's ([`Receipt::check_identity`]), of the commit's tree and an admitted
/// compiler ([`Receipt::check_sources`]); the zip is the receipt's ([`Receipt::verify_zip`]).
/// The receipt it admitted.
///
/// # Errors
/// The first of those that does not hold.
pub fn admit(directory: &Path, cut: &Cut<'_>, pins: &Pins<'_>) -> Result<Receipt, String> {
    let asset = TARGET.asset_name(cut.version);
    let receipt = receipt_name(&asset);
    let entries =
        std::fs::read_dir(directory).map_err(|e| format!("{}: {e}", directory.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", directory.display()))?;
        let name = entry.file_name().into_string().map_err(|name| {
            format!(
                "the Windows handoff directory {} holds a name that is not UTF-8 ({name:?})",
                directory.display()
            )
        })?;
        if name.starts_with('.') || name == asset || name == receipt {
            continue;
        }
        return Err(format!(
            "the Windows handoff directory {} holds {name:?}, which is no part of v{}'s pair \
             ({asset} and {receipt}): a carry holds that pair and nothing else; remove it",
            directory.display(),
            cut.version
        ));
    }
    let lacking = missing(directory, cut.version)?;
    if !lacking.is_empty() {
        return Err(format!(
            "the Windows handoff directory {} lacks {}",
            directory.display(),
            lacking.join(" and ")
        ));
    }
    let admitted = read_receipt(&directory.join(&receipt))?;
    admitted.check_identity(cut.version, cut.build, cut.commit)?;
    admitted.check_sources(cut.source_tree, pins)?;
    admitted.verify_zip(&directory.join(&asset))?;
    Ok(admitted)
}

#[cfg(any(test, debug_assertions))]
#[path = "handoff_fixture.rs"]
pub mod fixture;

#[cfg(test)]
#[path = "handoff_tests.rs"]
pub(crate) mod tests;
