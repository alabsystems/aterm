// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! get-aterm.ps1's Windows bootstrap verifier in a release: how a cut signs the verifier it
//! publishes into the appcast ([`stamp`]), how a signed appcast is read back
//! ([`signed_windows_verifiers`]), and how get-aterm.ps1's pin is written from what a public
//! release signs ([`write_windows_pin`], which
//! `buildplan::linux::from_release::windows_pin_from_release` feeds).
//!
//! get-aterm.ps1 runs whatever file hashes to its pin, so the pin is written only from a
//! release's signed appcast, under the paper master, never from an unsigned record: unlike
//! install.sh's, which a cut's `dist/` record may also write, the Windows verifier is built on
//! a Windows host and carried to the cutter, one more hand its digest would pass through
//! unsigned.

use std::io::Read as _;
use std::path::Path;

use aterm_bootstrap_verify::pin::{self, WindowsPin, WindowsRecord};
use aterm_update_core::Manifest;

/// The Windows verifiers a signed `manifest` names, in `pin::WINDOWS_ARCHES` order, each a
/// [`pin::Receipt`] of a canonical asset name for the manifest's version, and the interface
/// they speak; `None` when it names none.
///
/// # Errors
/// As [`pin::signed_windows_receipts`]: a partial, noncanonical or oversized entry, or
/// receipts and interface apart.
pub fn signed_windows_verifiers(
    manifest: &Manifest,
) -> Result<Option<(Vec<pin::Receipt>, u32)>, String> {
    pin::signed_windows_receipts(
        &manifest.version,
        [(
            manifest.windows_verifier_x86_64.as_deref(),
            manifest.windows_verifier_x86_64_sha256.as_deref(),
            manifest.windows_verifier_x86_64_size,
        )],
        manifest.windows_verifier_interface,
    )
}

/// Sign `receipt`, the Windows verifier a cut publishes beside its Windows zip, into
/// `manifest`: its architecture's three `windows_verifier_*` fields and the interface this
/// tree's get-aterm.ps1 calls (`pin::WINDOWS_INTERFACE`). These are the bytes
/// `ship pin-verifier --windows --from-release` binds get-aterm.ps1's pin to; no client reads
/// them.
///
/// # Errors
/// A receipt [`signed_windows_verifiers`] would not read back as itself (another
/// architecture, version or name, a digest that is not lowercase SHA-256, a size outside
/// `64..=pin::MAX_ASSET`): `manifest` is left as it was.
pub fn stamp(manifest: &mut Manifest, receipt: &pin::Receipt) -> Result<(), String> {
    let mut stamped = manifest.clone();
    let fields = (
        Some(receipt.asset.clone()),
        Some(receipt.sha256.clone()),
        Some(receipt.size),
    );
    match receipt.arch.as_str() {
        "x86_64" => {
            (
                stamped.windows_verifier_x86_64,
                stamped.windows_verifier_x86_64_sha256,
                stamped.windows_verifier_x86_64_size,
            ) = fields;
        }
        other => return Err(format!("no Windows verifier is built for {other:?}")),
    }
    stamped.windows_verifier_interface = Some(pin::WINDOWS_INTERFACE);
    let signed = signed_windows_verifiers(&stamped)?;
    if !signed
        .as_ref()
        .is_some_and(|(receipts, _)| receipts.contains(receipt))
    {
        return Err(format!(
            "the Windows verifier {} does not read back from the appcast it was signed into",
            receipt.asset
        ));
    }
    *manifest = stamped;
    Ok(())
}

/// The file at `path` is the signed Windows verifier: a regular file of `size` bytes, an x86-64
/// console executable (`pin::windows_verifier_image`), of SHA-256 `sha256`.
///
/// # Errors
/// Any of those that it is not, or a file it cannot inspect or read.
pub fn windows_verifier_file(path: &Path, size: u64, sha256: &str) -> Result<(), String> {
    let (file, metadata) = crate::durable::open_regular_file_with_meta(path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    if metadata.len() != size {
        return Err(format!(
            "the Windows bootstrap verifier is not a regular file of the signed size: {}",
            path.display()
        ));
    }
    let mut bytes = Vec::new();
    file.take(pin::MAX_ASSET + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("read {}: {e}", path.display()))?;
    if u64::try_from(bytes.len()).ok() != Some(size) {
        return Err(format!(
            "the Windows bootstrap verifier changed size while it was read: {}",
            path.display()
        ));
    }
    pin::windows_verifier_image(&bytes).map_err(|why| format!("{why}: {}", path.display()))?;
    if crate::publish::sha256_bytes(&bytes) != sha256 {
        return Err(format!(
            "the Windows bootstrap verifier is not of the signed SHA-256: {}",
            path.display()
        ));
    }
    Ok(())
}

/// Write `record` into the checkout at `root` as get-aterm.ps1's pin
/// (`pin::WINDOWS_SCRIPT`) and the committed record beside it (`pin::WINDOWS_RECORD`), once
/// both pass `pin::check_windows`. The pin it wrote.
///
/// # Errors
/// A record this checkout's RELEASES.ledger does not claim, one of another interface, or a
/// get-aterm.ps1 whose pin it cannot place: nothing is written.
pub fn write_windows_pin(root: &Path, record: &WindowsRecord) -> Result<WindowsPin, String> {
    let read = |path: &Path| {
        crate::durable::read_text(path).map_err(|e| format!("{}: {e}", path.display()))
    };
    let ledger = read(&root.join("RELEASES.ledger"))?;
    let script_at = root.join(pin::WINDOWS_SCRIPT);
    let (script, body) =
        pin::windows_pin_files(record, &read(&script_at)?, &ledger).map_err(|why| {
            format!(
                "{why}. Pin from a checkout of main that holds the release's claim (pull it \
                 into {} first)",
                root.display()
            )
        })?;
    crate::durable::write_atomic_durable(&script_at, script.as_bytes(), 0o644)
        .map_err(|e| format!("{}: {e}", script_at.display()))?;
    let at = root.join(pin::WINDOWS_RECORD);
    crate::durable::write_atomic_durable(&at, body.as_bytes(), 0o644)
        .map_err(|e| format!("{}: {e}", at.display()))?;
    Ok(record.pin())
}

#[cfg(test)]
#[path = "windows_pin_tests.rs"]
mod tests;
