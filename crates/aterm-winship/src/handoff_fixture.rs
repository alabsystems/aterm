// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test fixtures of a Windows handoff: a portable zip the release protocol admits (its two
//! images' PE headers, stored), a receipt of it for a cut, and the pair written into a
//! directory. The admission tests here and the release cutter's (aterm-release
//! windows_release_tests.rs) build their carries from these. A test seam: compiled only into
//! tests and debug builds, called by no code a release build compiles.

use std::path::Path;

use super::{Cut, Lane, Receipt, SCHEMA, TARGET, receipt_name};

/// A stock compiler's `-vV` answer: release `1.97.1`, native x86_64 MSVC.
pub const STOCK_VV: &str = "rustc 1.97.1 (8bab26f4f 2026-08-07)\n\
binary: rustc\n\
commit-hash: 8bab26f4f68e0e26f0bb7960be334d5b520ea452\n\
commit-date: 2026-08-07\n\
host: x86_64-pc-windows-msvc\n\
release: 1.97.1\n\
LLVM version: 20.1.8";

/// The stock release [`STOCK_VV`] is.
pub const STOCK: &str = "1.97.1";

/// A Trust compiler's `-vV` answer whose `commit-hash` is `commit` (40 lowercase hex digits).
#[must_use]
pub fn trust_vv(commit: &str) -> String {
    format!(
        "rustc 1.101.0-dev\nbinary: rustc\ncommit-hash: {commit}\ncommit-date: 2026-11-08\n\
         host: x86_64-pc-windows-msvc\nrelease: 1.101.0-dev\nLLVM version: 21.1.0\n\
         trust: 0.13.0+g{}",
        commit.get(..9).unwrap_or(commit)
    )
}

/// The start of an x86_64 PE32+ image declaring `subsystem` (2 windowed, 3 console), laid
/// out as a linker lays it out (the shape aterm-update-core's windows tests build).
#[must_use]
pub fn image(subsystem: u16) -> Vec<u8> {
    const E_LFANEW: usize = 0x80;
    let mut bytes = vec![0; 1024];
    bytes[..2].copy_from_slice(b"MZ");
    bytes[60..64].copy_from_slice(&(E_LFANEW as u32).to_le_bytes());
    let pe = E_LFANEW;
    bytes[pe..pe + 4].copy_from_slice(b"PE\0\0");
    bytes[pe + 4..pe + 6].copy_from_slice(&TARGET.machine().to_le_bytes());
    bytes[pe + 6..pe + 8].copy_from_slice(&3_u16.to_le_bytes());
    bytes[pe + 20..pe + 22].copy_from_slice(&240_u16.to_le_bytes());
    bytes[pe + 22..pe + 24].copy_from_slice(&0x0022_u16.to_le_bytes());
    let optional = pe + 24;
    bytes[optional..optional + 2].copy_from_slice(&0x20b_u16.to_le_bytes());
    bytes[optional + 68..optional + 70].copy_from_slice(&subsystem.to_le_bytes());
    bytes
}

/// A stored zip of `members` (no ZIP64, no comment, DOS host records, CRC left zero).
#[must_use]
pub fn zip(members: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    let len = |n: usize| u32::try_from(n).expect("a fixture member fits a zip");
    for (name, data) in members {
        let offset = len(out.len());
        let fixed = |into: &mut Vec<u8>| {
            into.extend_from_slice(&20_u16.to_le_bytes());
            into.extend_from_slice(&0_u16.to_le_bytes());
            into.extend_from_slice(&0_u16.to_le_bytes());
            into.extend_from_slice(&[0; 8]);
            into.extend_from_slice(&len(data.len()).to_le_bytes());
            into.extend_from_slice(&len(data.len()).to_le_bytes());
            into.extend_from_slice(&(len(name.len()) as u16).to_le_bytes());
            into.extend_from_slice(&0_u16.to_le_bytes());
        };
        out.extend_from_slice(&0x0403_4b50_u32.to_le_bytes());
        fixed(&mut out);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        central.extend_from_slice(&0x0201_4b50_u32.to_le_bytes());
        central.extend_from_slice(&20_u16.to_le_bytes());
        fixed(&mut central);
        central.extend_from_slice(&[0; 10]);
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let cd_offset = len(out.len());
    let count = (members.len() as u16).to_le_bytes();
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50_u32.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&count);
    out.extend_from_slice(&count);
    out.extend_from_slice(&len(central.len()).to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes());
    out
}

/// The portable folder as `build.ps1 -Zip` lays it out, its images' headers only; its
/// verifier ([`super::VERIFIER_MEMBER`]) is [`verifier`].
#[must_use]
pub fn portable() -> Vec<u8> {
    zip(&[
        ("aterm/BUILD.json", b"{}".to_vec()),
        ("aterm/OpenConsole.exe", image(3)),
        (super::VERIFIER_MEMBER, verifier()),
        ("aterm/aterm-gui.exe", image(2)),
        ("aterm/aterm.exe", image(3)),
    ])
}

/// The verifier [`portable`] carries: a console image, told apart from the others by its
/// last byte, so a digest of it names it alone.
#[must_use]
pub fn verifier() -> Vec<u8> {
    let mut bytes = image(3);
    if let Some(last) = bytes.last_mut() {
        *last = 0x5a;
    }
    bytes
}

/// The lowercase SHA-256 of `bytes`.
#[must_use]
pub fn sha256(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(bytes))
}

/// The receipt `release-build` writes for `zip`, built for `cut` in `lane`: a stock lane's
/// compiler is [`STOCK_VV`], a Trust lane's [`trust_vv`] of `trust_commit`.
#[must_use]
pub fn receipt(zip: &[u8], cut: &Cut<'_>, lane: Lane, trust_commit: &str) -> Receipt {
    Receipt {
        schema: SCHEMA,
        version: cut.version.to_string(),
        build_number: cut.build,
        commit: cut.commit.to_string(),
        target: TARGET.triple().to_string(),
        asset: TARGET.asset_name(cut.version),
        sha256: sha256(zip),
        size: zip.len() as u64,
        source_tree: cut.source_tree.to_string(),
        lane: lane.word().to_string(),
        compiler_version: match lane {
            Lane::Stock => STOCK_VV.to_string(),
            Lane::Trust => trust_vv(trust_commit),
        },
        compiler_sha256: "1".repeat(64),
        driver_sha256: "2".repeat(64),
        compiler_library_sha256: "3".repeat(64),
    }
}

/// Writes `zip` and `receipt` into `directory` under their canonical names.
///
/// # Errors
/// A write that fails, or a receipt the serializer refuses.
pub fn carry(directory: &Path, zip: &[u8], receipt: &Receipt) -> Result<(), String> {
    let write = |name: &str, bytes: &[u8]| {
        std::fs::write(directory.join(name), bytes).map_err(|e| format!("{name}: {e}"))
    };
    write(&receipt.asset, zip)?;
    write(&receipt_name(&receipt.asset), receipt.to_toml()?.as_bytes())
}
