// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Toolchain-seed validation for the batteries-included bundle
//! (docs/TOOLCHAIN-PACKAGE-MANAGER.md §9.1): a cut may seal the flat signed
//! registry `tools/atpkg-*.sh` emit (`index.toml`(`.sig`), `pkg-*.toml`(`.sig`),
//! artifact tarballs) into `Contents/Resources/toolchain-seed`, where the
//! client's bundled-seed lane (crates/atpkg/src/bundled.rs) installs from it
//! offline through atpkg's ordinary signature gates.
//!
//! This module is the CUT-TIME quality gate, not the client trust anchor: it
//! refuses to seal a seed the shipped client would reject or ignore. The
//! checks mirror the client's cheapest-first order (§8) — signature over raw
//! bytes before any parse, freshness, then per-package delegation — plus two
//! producer-only rules the client cannot enforce: the seed must verify under
//! the SAME root key this build bakes (`ATERM_PKG_ROOTKEY`), and every file in
//! the directory must be accounted for by the signed manifests (nothing rides
//! the code-signature seal unaudited). Artifact bytes are NOT re-hashed here —
//! every client re-verifies sha256 + tree_root at install; the cut checks
//! presence + exact size so a truncated copy cannot ship.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use ring::signature::{ED25519, UnparsedPublicKey};

/// Sanity caps for the metadata reads (the registry's big files are the
/// artifact tarballs, which are never read here — only stat'd).
const MANIFEST_CAP: u64 = 4 << 20;
const SIG_LEN: u64 = 64;

/// What a validated seed holds — feeds the bundle step's log line and the
/// provenance record.
#[derive(Debug, Clone)]
pub struct SeedStat {
    /// The validated registry directory (the copy source).
    pub dir: PathBuf,
    /// Regular files in the registry (all accounted for).
    pub files: usize,
    /// Total payload bytes.
    pub bytes: u64,
    /// The signed index's monotonic build.
    pub index_build: u64,
    /// The signed index's freshness horizon (RFC3339, verbatim).
    pub valid_until: String,
    /// The channel-pinned `(program, build)` set the seed can install.
    pub programs: Vec<(String, u64)>,
}

/// Resolve the seed directory for this cut: `ATERM_SEED_DIR` (explicit
/// operator override) beats the conventional `dist/toolchain-seed`; absence of
/// both means this cut ships no seed — today's bundle, byte-identical.
pub fn resolve(dist: &Path) -> Option<PathBuf> {
    if let Ok(v) = std::env::var("ATERM_SEED_DIR") {
        let v = v.trim();
        if !v.is_empty() {
            return Some(PathBuf::from(v));
        }
    }
    let conventional = dist.join("toolchain-seed");
    conventional.is_dir().then_some(conventional)
}

/// Validate `dir` as a shippable seed under `root_key_b64` (the exact
/// `ATERM_PKG_ROOTKEY` this build bakes into the client). Any failure is a
/// hard error — a cut must never seal a seed its own client would refuse.
pub fn validate(dir: &Path, root_key_b64: &str) -> Result<SeedStat, String> {
    // ---- 0. flat, regular, no surprises ---------------------------------
    let mut names: BTreeSet<String> = BTreeSet::new();
    let mut bytes: u64 = 0;
    for entry in
        std::fs::read_dir(dir).map_err(|e| format!("read seed dir {}: {e}", dir.display()))?
    {
        let entry = entry.map_err(|e| format!("read seed dir {}: {e}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let meta = std::fs::symlink_metadata(entry.path())
            .map_err(|e| format!("stat {name}: {e}"))?;
        if !meta.is_file() {
            return Err(format!(
                "seed entry {name} is not a regular file (symlinks/subdirectories do not ship)"
            ));
        }
        bytes = bytes.saturating_add(meta.len());
        names.insert(name);
    }
    if names.is_empty() {
        return Err(format!("seed dir {} is empty", dir.display()));
    }

    // ---- 1. the root-signed index, raw bytes before any parse ------------
    let index_bytes = read_capped(&dir.join("index.toml"), MANIFEST_CAP)?;
    let index_sig = read_capped(&dir.join("index.toml.sig"), SIG_LEN)?;
    verify_detached(root_key_b64, &index_bytes, &index_sig)
        .map_err(|e| format!("index.toml does not verify under the build's ATERM_PKG_ROOTKEY ({e}) — the shipped client would refuse this seed"))?;
    let index: toml::Value = toml::from_str(
        std::str::from_utf8(&index_bytes).map_err(|_| "index.toml is not UTF-8".to_string())?,
    )
    .map_err(|e| format!("index.toml parse (after verify): {e}"))?;

    let index_build = index
        .get("index_build")
        .and_then(toml::Value::as_integer)
        .and_then(nonneg)
        .ok_or_else(|| "index.toml: no index_build (or it is negative — the client parses it as u64 and would refuse the whole index)".to_string())?;
    let valid_until = index
        .get("valid_until")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| "index.toml: no valid_until".to_string())?
        .to_string();
    // A seed whose freshness already lapsed is dead weight the client will
    // refuse for the tools; refuse to seal it.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let horizon = rfc3339_to_unix(&valid_until)
        .ok_or_else(|| format!("index.toml: unparseable valid_until {valid_until:?}"))?;
    if now >= horizon {
        return Err(format!(
            "index.toml freshness lapsed ({valid_until}) — repack the seed with a live horizon"
        ));
    }
    let release_key = index
        .get("keys")
        .and_then(|k| k.get("release_key_pubkey"))
        .and_then(toml::Value::as_str)
        .ok_or_else(|| "index.toml: no [keys].release_key_pubkey".to_string())?
        .to_string();

    // The union of every channel's pin set — the installable surface.
    let mut pins: Vec<(String, u64)> = Vec::new();
    if let Some(channels) = index.get("channels").and_then(toml::Value::as_array) {
        for ch in channels {
            if let Some(pin) = ch.get("pin").and_then(toml::Value::as_table) {
                for (program, build) in pin {
                    if let Some(b) = build.as_integer().and_then(nonneg)
                        && !pins.iter().any(|(p, pb)| p == program && *pb == b)
                    {
                        pins.push((program.clone(), b));
                    }
                }
            }
        }
    }
    if pins.is_empty() {
        return Err("index.toml pins no programs — an empty seed must not ship".to_string());
    }

    // ---- 2. every pinned program: signed pkg manifest + present artifact --
    let mut accounted: BTreeSet<String> =
        ["index.toml", "index.toml.sig"].map(str::to_string).into();
    for (program, build) in &pins {
        let pkg_name = format!("pkg-{program}-{build}.toml");
        let sig_name = format!("{pkg_name}.sig");
        let pkg_bytes = read_capped(&dir.join(&pkg_name), MANIFEST_CAP)
            .map_err(|e| format!("pinned {program}@{build}: {e}"))?;
        let pkg_sig = read_capped(&dir.join(&sig_name), SIG_LEN)
            .map_err(|e| format!("pinned {program}@{build}: {e}"))?;
        verify_detached(&release_key, &pkg_bytes, &pkg_sig).map_err(|e| {
            format!("{pkg_name} does not verify under the index's delegated release key ({e})")
        })?;
        accounted.insert(pkg_name.clone());
        accounted.insert(sig_name);

        let pkg: toml::Value = toml::from_str(
            std::str::from_utf8(&pkg_bytes)
                .map_err(|_| format!("{pkg_name} is not UTF-8"))?,
        )
        .map_err(|e| format!("{pkg_name} parse (after verify): {e}"))?;
        // ANTI-REPLAY BIND, mirroring the client's own check (atpkg
        // flow.rs: `!pkg.is_for(program) || pkg.build_number != pinned`
        // ⇒ Mismatch): a release-key signature proves the OWNER made these
        // bytes, never that they are the bytes THIS pin names. Without this,
        // a stale or mis-named pack artifact copied into the registry
        // verifies here and is refused on every client.
        let inner_program = pkg.get("program").and_then(toml::Value::as_str);
        if inner_program != Some(program.as_str()) {
            return Err(format!(
                "{pkg_name} names program {inner_program:?}, not {program:?} — the client \
                 would reject this pin (anti-replay bind)"
            ));
        }
        let inner_build = pkg.get("build_number").and_then(toml::Value::as_integer);
        if inner_build != Some(i64::try_from(*build).unwrap_or(i64::MAX)) {
            return Err(format!(
                "{pkg_name} carries build_number {inner_build:?}, not the pinned {build} — \
                 the client would reject this pin (anti-replay bind)"
            ));
        }
        let artifacts = pkg
            .get("artifact")
            .and_then(toml::Value::as_array)
            .ok_or_else(|| format!("{pkg_name}: no [[artifact]] rows"))?;
        let mut present = 0usize;
        for row in artifacts {
            let Some(asset) = row.get("asset").and_then(toml::Value::as_str) else {
                return Err(format!("{pkg_name}: artifact row without asset"));
            };
            let path = dir.join(asset);
            if !names.contains(asset) {
                continue; // other-triple artifact not carried by this seed — fine
            }
            let size = row
                .get("size")
                .and_then(toml::Value::as_integer)
                .and_then(nonneg)
                .ok_or_else(|| format!("{pkg_name}: artifact {asset} has no (non-negative) size"))?;
            let actual = std::fs::metadata(&path)
                .map_err(|e| format!("stat {asset}: {e}"))?
                .len();
            if actual != size {
                return Err(format!(
                    "artifact {asset} is {actual} bytes but the signed manifest says {size} — truncated or stale copy"
                ));
            }
            accounted.insert(asset.to_string());
            present += 1;
        }
        if present == 0 {
            return Err(format!(
                "pinned {program}@{build} has no artifact present in the seed — it would be offered and then fail offline"
            ));
        }
    }

    // ---- 3. nothing unaccounted rides the seal ---------------------------
    let extras: Vec<&String> = names.difference(&accounted).collect();
    if !extras.is_empty() {
        return Err(format!(
            "unaccounted file(s) in the seed: {} — every sealed byte must be named by a signed manifest",
            extras
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    Ok(SeedStat {
        dir: dir.to_path_buf(),
        files: names.len(),
        bytes,
        index_build,
        valid_until,
        programs: pins,
    })
}

/// A TOML integer the shipped client can actually parse as `u64`. TOML allows
/// negatives; the client's serde types are all `u64`, so a negative anywhere in
/// the signed bytes makes `parse_index`/`parse_pkg` fail and the whole index is
/// discarded. `unsigned_abs` would have silently sealed `-4` as `4` (adversarial
/// review 2026-07-30) — refuse instead.
fn nonneg(v: i64) -> Option<u64> {
    u64::try_from(v).ok()
}

fn read_capped(path: &Path, cap: u64) -> Result<Vec<u8>, String> {
    let meta =
        std::fs::symlink_metadata(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    if meta.len() > cap {
        return Err(format!(
            "{} is {} bytes (cap {cap}) — not a plausible manifest/signature",
            path.display(),
            meta.len()
        ));
    }
    std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))
}

/// The same Ed25519 primitive the client pins (atpkg sig.rs / aterm-update
/// sig.rs): detached 64-byte signature over the exact raw bytes.
fn verify_detached(pubkey_b64: &str, msg: &[u8], sig: &[u8]) -> Result<(), String> {
    let pk = base64::engine::general_purpose::STANDARD
        .decode(pubkey_b64.trim())
        .map_err(|_| "public key is not base64".to_string())?;
    if pk.len() != 32 {
        return Err(format!("public key is {} bytes, not 32", pk.len()));
    }
    if sig.len() != 64 {
        return Err(format!("signature is {} bytes, not 64", sig.len()));
    }
    UnparsedPublicKey::new(&ED25519, pk)
        .verify(msg, sig)
        .map_err(|_| "signature does not verify".to_string())
}

/// Minimal strict RFC3339 `YYYY-MM-DDTHH:MM:SSZ` → Unix seconds (UTC only —
/// exactly the shape `tools/atpkg-index.sh` writes). Inverse of
/// [`crate::bundle::epoch_to_rfc3339`]'s civil math. `None` on any deviation.
pub fn rfc3339_to_unix(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() != 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' {
        return None;
    }
    if b[13] != b':' || b[16] != b':' || b[19] != b'Z' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<u64> { s.get(r)?.parse().ok() };
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hh, mm, ss) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1970..=9999).contains(&y) || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    if hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    // days_from_civil (Howard Hinnant), day 0 = 1970-01-01.
    let y_adj = if m <= 2 { y - 1 } else { y };
    let era = y_adj / 400;
    let yoe = y_adj % 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe;
    days.checked_sub(719_468)
        .map(|days| days * 86_400 + hh * 3600 + mm * 60 + ss)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_round_trips_the_bundle_formatter() {
        for epoch in [0u64, 86_399, 86_400, 1_753_920_000, 4_102_444_800] {
            let s = crate::bundle::epoch_to_rfc3339(epoch);
            assert_eq!(rfc3339_to_unix(&s), Some(epoch), "epoch {epoch} via {s}");
        }
        assert_eq!(rfc3339_to_unix("2026-07-30"), None);
        assert_eq!(rfc3339_to_unix("2026-07-30T99:00:00Z"), None);
        assert_eq!(rfc3339_to_unix("2026-07-30T00:00:00+01:00"), None);
    }

    #[test]
    fn validate_refuses_junk_and_accounts_every_file() {
        let d = std::env::temp_dir().join(format!("seedpack-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        // No index at all → refused.
        std::fs::write(d.join("stray.bin"), b"x").unwrap();
        let err = validate(&d, "AAAA").unwrap_err();
        assert!(err.contains("index.toml"), "{err}");
        let _ = std::fs::remove_dir_all(&d);
    }

    fn keypair(seed: &[u8; 32]) -> ring::signature::Ed25519KeyPair {
        ring::signature::Ed25519KeyPair::from_seed_unchecked(seed).unwrap()
    }

    fn b64_pub(kp: &ring::signature::Ed25519KeyPair) -> String {
        use ring::signature::KeyPair as _;
        base64::engine::general_purpose::STANDARD.encode(kp.public_key().as_ref())
    }

    /// A minimal REAL registry: root-signed index delegating a release key,
    /// one pinned program with one present artifact. The positive path plus
    /// the three producer-rule refusals (tamper, stray file, size lie).
    #[test]
    fn validate_accepts_a_signed_registry_and_refuses_drift() {
        let root = keypair(&[7u8; 32]);
        let release = keypair(&[9u8; 32]);
        let d = std::env::temp_dir().join(format!("seedpack-pos-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();

        std::fs::write(d.join("ay-42-host.tar.zst"), b"tarball-bytes").unwrap();
        let pkg = "schema = 1\nprogram = \"ay\"\nversion = \"0.1.0\"\nbuild_number = 42\n\
                   exposes = [\"ay\"]\n\n[[artifact]]\ntarget = \"aarch64-apple-darwin\"\n\
                   kind = \"binary\"\nasset = \"ay-42-host.tar.zst\"\nsha256 = \"aa\"\n\
                   tree_root = \"bb\"\nsize = 13\n";
        std::fs::write(d.join("pkg-ay-42.toml"), pkg).unwrap();
        std::fs::write(d.join("pkg-ay-42.toml.sig"), release.sign(pkg.as_bytes())).unwrap();

        let index = format!(
            "schema = 1\nindex_build = 4\ngenerated_at = \"2026-07-30T00:00:00Z\"\n\
             valid_until = \"2999-01-01T00:00:00Z\"\n\n[keys]\nrelease_key_id = \"rk-test\"\n\
             release_key_pubkey = \"{}\"\n\n[programs.ay]\nrepo = \"ay\"\npolicy = \"prebuilt-only\"\n\n\
             [[channels]]\nname = \"stable\"\nchannel_build = 1\n\npin = {{ ay = 42 }}\n",
            b64_pub(&release)
        );
        std::fs::write(d.join("index.toml"), &index).unwrap();
        std::fs::write(d.join("index.toml.sig"), root.sign(index.as_bytes())).unwrap();

        let stat = validate(&d, &b64_pub(&root)).expect("a coherent signed registry validates");
        assert_eq!(stat.index_build, 4);
        assert_eq!(stat.programs, vec![("ay".to_string(), 42)]);
        assert_eq!(stat.files, 5);

        // Wrong root key → the index refusal names the client consequence.
        let err = validate(&d, &b64_pub(&release)).unwrap_err();
        assert!(err.contains("ATERM_PKG_ROOTKEY"), "{err}");

        // A stray file rides nothing: refused by the accounting rule.
        std::fs::write(d.join("notes.txt"), b"scratch").unwrap();
        let err = validate(&d, &b64_pub(&root)).unwrap_err();
        assert!(err.contains("unaccounted"), "{err}");
        std::fs::remove_file(d.join("notes.txt")).unwrap();

        // A truncated artifact contradicts the signed size: refused.
        std::fs::write(d.join("ay-42-host.tar.zst"), b"short").unwrap();
        let err = validate(&d, &b64_pub(&root)).unwrap_err();
        assert!(err.contains("truncated or stale"), "{err}");

        let _ = std::fs::remove_dir_all(&d);
    }
}
