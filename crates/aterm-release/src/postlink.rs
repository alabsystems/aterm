// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Identity is patched in a file-backed Mach-O section, then covered by a fresh signature.

use std::ops::Range;
use std::path::Path;

fn u32_at(bytes: &[u8], at: usize) -> Result<u32, String> {
    let word = bytes.get(at..at + 4).ok_or("truncated Mach-O word")?;
    Ok(u32::from_le_bytes(
        word.try_into().map_err(|_| "Mach-O word")?,
    ))
}

fn u64_at(bytes: &[u8], at: usize) -> Result<u64, String> {
    let word = bytes.get(at..at + 8).ok_or("truncated Mach-O word")?;
    Ok(u64::from_le_bytes(
        word.try_into().map_err(|_| "Mach-O word")?,
    ))
}

fn range(offset: u64, size: u64, limit: usize) -> Result<Range<usize>, String> {
    let end = offset.checked_add(size).ok_or("Mach-O range overflow")?;
    if end > limit as u64 {
        return Err("Mach-O range outside file".into());
    }
    Ok(offset as usize..end as usize)
}

fn named(bytes: &[u8], name: &[u8]) -> bool {
    bytes.get(..name.len()) == Some(name)
        && bytes
            .get(name.len()..)
            .is_some_and(|tail| tail.iter().all(|b| *b == 0))
}

struct Layout {
    stamp: Range<usize>,
    signature: Range<usize>,
    signature_command: Range<usize>,
    linkedit_sizes: [Range<usize>; 2],
}

fn layout(bytes: &[u8]) -> Result<Layout, String> {
    if u32_at(bytes, 0)? != 0xfeed_facf
        || !matches!(u32_at(bytes, 4)?, 0x0100_0007 | 0x0100_000c)
        || u32_at(bytes, 12)? != 2
    {
        return Err("stamp requires a thin 64-bit macOS executable".into());
    }
    let commands = range(32, u64::from(u32_at(bytes, 20)?), bytes.len())?;
    let count = u32_at(bytes, 16)?;
    if count as usize > commands.len() / 8 {
        return Err("Mach-O command count exceeds table".into());
    }
    let mut at = commands.start;
    let mut stamp = None;
    let mut signature = None;
    let mut linkedit_sizes = None;
    for _ in 0..count {
        if at + 8 > commands.end {
            return Err("truncated Mach-O command".into());
        }
        let kind = u32_at(bytes, at)?;
        let size = u32_at(bytes, at + 4)? as usize;
        if size < 8 || size > commands.end - at {
            return Err("invalid Mach-O command size".into());
        }
        if kind == 0x19 {
            if size < 72 {
                return Err("short Mach-O segment".into());
            }
            let segment = range(
                u64_at(bytes, at + 40)?,
                u64_at(bytes, at + 48)?,
                bytes.len(),
            )?;
            let sections = u32_at(bytes, at + 64)? as usize;
            if sections > (size - 72) / 80 {
                return Err("Mach-O sections exceed segment command".into());
            }
            if named(&bytes[at + 8..at + 24], b"__LINKEDIT")
                && linkedit_sizes
                    .replace([at + 32..at + 40, at + 48..at + 56])
                    .is_some()
            {
                return Err("duplicate __LINKEDIT".into());
            }
            for i in 0..sections {
                let section = at + 72 + i * 80;
                if !named(&bytes[section..section + 16], b"__aterm_stamp") {
                    continue;
                }
                if !named(&bytes[at + 8..at + 24], b"__DATA")
                    || !named(&bytes[section + 16..section + 32], b"__DATA")
                    || u64_at(bytes, section + 40)? != aterm_stamp::LEN as u64
                    || u32_at(bytes, section + 64)? & 0xff != 0
                {
                    return Err("stamp must be fixed-size __DATA S_REGULAR".into());
                }
                let record = range(
                    u64::from(u32_at(bytes, section + 48)?),
                    aterm_stamp::LEN as u64,
                    bytes.len(),
                )?;
                if record.start < commands.end
                    || record.start < segment.start
                    || record.end > segment.end
                {
                    return Err("stamp is outside file-backed segment data".into());
                }
                if stamp.replace(record).is_some() {
                    return Err("duplicate stamp section".into());
                }
            }
        } else if kind == 0x1d {
            if size != 16 {
                return Err("invalid code signature command".into());
            }
            let blob = range(
                u64::from(u32_at(bytes, at + 8)?),
                u64::from(u32_at(bytes, at + 12)?),
                bytes.len(),
            )?;
            if blob.is_empty() || blob.start < commands.end || blob.end != bytes.len() {
                return Err("signature must be a nonempty trailing blob".into());
            }
            if signature.replace((blob, at + 8..at + 16)).is_some() {
                return Err("duplicate code signature".into());
            }
        } else if kind == 1 {
            return Err("32-bit segment in 64-bit image".into());
        }
        at += size;
    }
    if at != commands.end {
        return Err("unclaimed Mach-O load commands".into());
    }
    let stamp = stamp.ok_or("missing __DATA,__aterm_stamp")?;
    let (signature, signature_command) = signature.ok_or("missing code signature")?;
    if stamp.end > signature.start {
        return Err("stamp overlaps code signature".into());
    }
    Ok(Layout {
        stamp,
        signature,
        signature_command,
        linkedit_sizes: linkedit_sizes.ok_or("missing __LINKEDIT")?,
    })
}

/// Normalize identity and signature storage, retaining every other product byte.
pub fn normalized(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if bytes.starts_with(&[0xca, 0xfe, 0xba, 0xbe]) {
        let be = |at: usize| -> Result<u32, String> {
            Ok(u32::from_be_bytes(
                bytes
                    .get(at..at + 4)
                    .ok_or("truncated fat header")?
                    .try_into()
                    .map_err(|_| "fat word")?,
            ))
        };
        let count = be(4)? as usize;
        if count == 0 || count > bytes.len().saturating_sub(8) / 20 {
            return Err("invalid fat architecture table".into());
        }
        let mut result = b"aterm-fat-content-v1\0".to_vec();
        let mut seen = std::collections::BTreeSet::new();
        for i in 0..count {
            let at = 8 + 20 * i;
            let cpu = be(at)?;
            if !seen.insert(cpu) {
                return Err("duplicate fat architecture".into());
            }
            let slice = range(u64::from(be(at + 8)?), u64::from(be(at + 12)?), bytes.len())?;
            if slice.start < 8 + 20 * count
                || u32_at(&bytes[slice.clone()], 0)? != 0xfeed_facf
                || u32_at(&bytes[slice.clone()], 4)? != cpu
            {
                return Err("fat slice disagrees with architecture table".into());
            }
            let normalized = normalized(&bytes[slice])?;
            result.extend_from_slice(&cpu.to_le_bytes());
            result.extend_from_slice(&be(at + 4)?.to_le_bytes());
            result.extend_from_slice(&(normalized.len() as u64).to_le_bytes());
            result.extend(normalized);
        }
        return Ok(result);
    }
    let layout = layout(bytes)?;
    let mut bytes = bytes.to_vec();
    bytes[layout.stamp].fill(0);
    bytes[layout.signature_command].fill(0);
    for size in layout.linkedit_sizes {
        bytes[size].fill(0);
    }
    bytes.truncate(layout.signature.start);
    Ok(bytes)
}

/// Patch exactly the dedicated section. Never search for a stamp-shaped string elsewhere.
pub fn stamp(path: &Path, record: &[u8; aterm_stamp::LEN]) -> Result<(), String> {
    let mut bytes = crate::durable::read_regular_bytes(path, u64::MAX)
        .map_err(|e| format!("read stamp slice {}: {e}", path.display()))?;
    let layout = layout(&bytes)?;
    if !bytes[layout.stamp.clone()].starts_with(aterm_stamp::HEADER.as_bytes()) {
        return Err("stamp section has no identity record".into());
    }
    bytes[layout.stamp].copy_from_slice(record);
    std::fs::write(path, bytes).map_err(|e| format!("stamp {}: {e}", path.display()))
}

/// Every architecture must carry exactly the reservation's fields, in the stamp format's
/// order. U alone deliberately says nothing about identity.
pub fn prove_stamp(bytes: &[u8], expected: &[(&str, &str)]) -> Result<(), String> {
    if bytes.starts_with(&[0xca, 0xfe, 0xba, 0xbe]) {
        let be = |at: usize| -> Result<u32, String> {
            Ok(u32::from_be_bytes(
                bytes
                    .get(at..at + 4)
                    .ok_or("truncated fat header")?
                    .try_into()
                    .map_err(|_| "fat word")?,
            ))
        };
        let count = be(4)? as usize;
        if count == 0 || count > bytes.len().saturating_sub(8) / 20 {
            return Err("invalid fat table".into());
        }
        for i in 0..count {
            let at = 8 + 20 * i;
            let slice = range(u64::from(be(at + 8)?), u64::from(be(at + 12)?), bytes.len())?;
            if slice.start < 8 + 20 * count
                || u32_at(&bytes[slice.clone()], 0)? != 0xfeed_facf
                || u32_at(&bytes[slice.clone()], 4)? != be(at)?
            {
                return Err("invalid fat architecture slice".into());
            }
            prove_stamp(&bytes[slice], expected)?;
        }
        return Ok(());
    }
    let record = &bytes[layout(bytes)?.stamp];
    let end = record.iter().position(|b| *b == 0).unwrap_or(record.len());
    if record[end..].iter().any(|b| *b != 0) {
        return Err("nonzero stamp padding".into());
    }
    let text = std::str::from_utf8(&record[..end]).map_err(|e| e.to_string())?;
    let fields: Option<Vec<_>> = text
        .strip_prefix(aterm_stamp::HEADER)
        .ok_or("invalid stamp header")?
        .lines()
        .map(|line| line.split_once('='))
        .collect();
    if fields.as_deref() != Some(expected) {
        return Err("stamp differs from reservation identity".into());
    }
    Ok(())
}

/// Immutable linked slices are addressed by U before any reservation is stamped into them.
/// These files are never executed; the build stamps a private copy.
pub fn keep_unstamped(path: &Path, store: &Path) -> Result<std::path::PathBuf, String> {
    crate::snapshot::private_dir(store, "slice artifact store")?;
    let bytes = crate::durable::read_regular_bytes(path, u64::MAX).map_err(|e| e.to_string())?;
    if bytes[layout(&bytes)?.stamp] != aterm_stamp::UNSTAMPED_RECORD {
        return Err("linked slice did not compile the constant postlink placeholder".into());
    }
    let content = normalized(&bytes)?;
    let mut h = aterm_digest::Sha256::new();
    h.update(&content);
    let key = aterm_codec::hex::encode_lower(&h.finalize());
    let dir = store.join("unstamped");
    crate::snapshot::private_dir(&dir, "unstamped slice store")?;
    let kept = dir.join(key);
    match crate::durable::read_regular_bytes(&kept, u64::MAX) {
        Ok(bytes) => {
            verify(&kept)?;
            if bytes[layout(&bytes)?.stamp] != aterm_stamp::UNSTAMPED_RECORD
                || normalized(&bytes)? != content
            {
                return Err("unstamped content store entry changed".into());
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            crate::durable::copy_regular_fresh(path, &kept).map_err(|e| e.to_string())?;
        }
        Err(e) => return Err(format!("read unstamped slice: {e}")),
    }
    Ok(kept)
}

/// Only the current two slices need raw storage: cuts retain their own completed bundles.
pub fn prune_unstamped(store: &Path, keep: &[std::path::PathBuf]) -> Result<(), String> {
    for entry in std::fs::read_dir(store.join("unstamped")).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if !keep.contains(&path)
            && entry
                .file_name()
                .to_str()
                .is_some_and(|n| aterm_codec::hex::is_lower(n, 64))
        {
            std::fs::remove_file(&path).map_err(|e| format!("prune unstamped slice: {e}"))?;
        }
    }
    Ok(())
}

/// Ad hoc signing is required after changing even one byte of an arm64 code page.
pub fn resign(path: &Path) -> Result<(), String> {
    for args in [vec!["--force", "--sign", "-"], vec!["--verify", "--strict"]] {
        let output = aterm_uds::spawnfd::command(aterm_uds::os_path!("codesign"))
            .args(args)
            .arg(path)
            .output()
            .map_err(|e| format!("codesign: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "codesign {}: {}",
                path.display(),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }
    Ok(())
}

/// A normalized digest never makes an invalid or missing signature usable evidence.
pub fn verify(path: &Path) -> Result<(), String> {
    let output = aterm_uds::spawnfd::command(aterm_uds::os_path!("codesign"))
        // The pre-cut bundle has no resource envelope yet. U hashes every resource itself;
        // here codesign judges the executable's pages, not a later bundle resource seal.
        .args([
            "--verify",
            "--strict",
            "--all-architectures",
            "--ignore-resources",
        ])
        .arg(path)
        .output()
        .map_err(|e| format!("verify product signature: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "invalid product signature: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

/// The running image must report the newly stamped identity even when its measure is reused.
pub fn identity_report(stamp: &aterm_stamp::Stamp, report: &str) -> Result<(), String> {
    let fields: Vec<_> = report
        .lines()
        .filter_map(|line| line.strip_prefix("version:"))
        .map(str::trim)
        .collect();
    let expected = format!(
        "{} ({}, built {})",
        stamp.app_version, stamp.git_commit, stamp.build_time
    );
    if fields.as_slice() == [expected.as_str()] {
        Ok(())
    } else {
        Err(format!(
            "running image identity {fields:?}, expected {expected:?}"
        ))
    }
}

#[cfg(test)]
mod identity_tests {
    /// A previous hop's stamp cannot answer for the current reservation.
    #[test]
    fn a_runtime_identity_names_this_stamp_once() {
        let stamp = aterm_stamp::Stamp {
            app_version: "1.2.0",
            git_commit: "123456789abc",
            build_time: "2026-11-07T00:00:00Z",
            ..aterm_stamp::Stamp::UNSTAMPED
        };
        let report = "version: 1.2.0 (123456789abc, built 2026-11-07T00:00:00Z)\n";
        super::identity_report(&stamp, report).unwrap();
        assert!(
            super::identity_report(&stamp, &report.replace("123456789abc", "aaaaaaaaaaaa"))
                .is_err()
        );
        assert!(
            super::identity_report(&stamp, &report.replace("2026-11-07", "2026-11-06")).is_err()
        );
        assert!(super::identity_report(&stamp, &report.repeat(2)).is_err());
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    /// The same linked product reports each new identity after signing. A product-byte
    /// mutation changes U; stamping without signing leaves a signature that cannot verify.
    #[test]
    fn two_stamps_execute_with_equal_content_but_product_changes_do_not() {
        use aterm_uds::deadline::{FIRST_EXEC_HANG, KeptUntilRan, prints};
        use aterm_uds::spawnfd::{Stdio, command};
        let dir = aterm_tempfile::tempdir().unwrap();
        let project = dir.path().join("fixture");
        std::fs::create_dir_all(project.join("src")).unwrap();
        let root = aterm_tempfile::manifest_dir()
            .parent()
            .unwrap()
            .to_path_buf();
        std::fs::write(project.join("Cargo.toml"), format!(
            "[package]\nname = \"stamp-fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[dependencies]\naterm-stamp = {{ path = {:?} }}\n[workspace]\n", root.join("aterm-stamp"))).unwrap();
        std::fs::write(
            project.join("src/main.rs"),
            r#"
#[used]
#[unsafe(link_section = "__DATA,__aterm_stamp")]
static STAMP: [u8; aterm_stamp::LEN] = aterm_stamp::UNSTAMPED_RECORD;
fn main() {
    aterm_stamp::install(&STAMP, aterm_stamp::Stamp::UNSTAMPED);
    println!("identity {}", aterm_stamp::current().build_id());
}
"#,
        )
        .unwrap();
        let target = project.join("target.noindex");
        let output = command(crate::gates::trust_stage2_bin().unwrap().join("targo"))
            .args(["--unverified", "build", "--offline", "--target-dir"])
            .arg(&target)
            .current_dir(&project)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let original = std::fs::read(target.join("debug/stamp-fixture")).unwrap();
        let expected = normalized(&original).unwrap();
        let linked = layout(&original).unwrap();
        for end in [0, 3, 31, 32, linked.stamp.end - 1, linked.signature.start] {
            assert!(normalized(&original[..end]).is_err());
        }
        let mut bad_command = original.clone();
        bad_command[36..40].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(normalized(&bad_command).is_err());
        let fat = |slice: &[u8]| {
            let mut fat = vec![0; 32];
            fat[..4].copy_from_slice(&[0xca, 0xfe, 0xba, 0xbe]);
            for (at, value) in [
                (4, 1),
                (8, u32_at(slice, 4).unwrap()),
                (12, u32_at(slice, 8).unwrap()),
                (16, 32),
                (20, slice.len() as u32),
            ] {
                fat[at..at + 4].copy_from_slice(&value.to_be_bytes());
            }
            fat.extend_from_slice(slice);
            fat
        };
        let fat_content = normalized(&fat(&original)).unwrap();
        let mut bad_fat = fat(&original);
        bad_fat[16..20].fill(0);
        assert!(normalized(&bad_fat).is_err());
        let store = dir.path().join("store");
        let kept = keep_unstamped(&target.join("debug/stamp-fixture"), &store).unwrap();
        assert_eq!(std::fs::read(&kept).unwrap(), original);
        assert_eq!(
            keep_unstamped(&target.join("debug/stamp-fixture"), &store).unwrap(),
            kept
        );
        let mut bundle_content = None;
        for (version, commit, build) in [("1.2.0", "111111111111", 7), ("1.3.0", "222222222222", 8)]
        {
            let copy = dir.path().join(version);
            let app = copy.join("aterm.app");
            std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
            std::fs::create_dir_all(app.join("Contents/Resources")).unwrap();
            let bin = app.join("Contents/MacOS/aterm");
            let thin = copy.join("fixture");
            std::fs::copy(target.join("debug/stamp-fixture"), &thin).unwrap();
            std::fs::write(app.join("Contents/Info.plist"), format!("<plist><dict><key>CFBundleShortVersionString</key><string>{version}</string><key>CFBundleVersion</key><string>{build}</string><key>ATermGitCommit</key><string>{commit}</string></dict></plist>")).unwrap();
            let resource = app.join("Contents/Resources/Help.html");
            std::fs::write(&resource, "product help").unwrap();
            let identity = aterm_stamp::Stamp {
                app_version: version,
                git_commit: commit,
                git_commit_full: format!("{commit}{commit}{commit}0000").leak(),
                build_number: build.to_string().leak(),
                build_time: crate::bundle::epoch_to_rfc3339(build).leak(),
                release_build: true,
                ..aterm_stamp::Stamp::UNSTAMPED
            };
            stamp(&thin, &aterm_stamp::encode(&identity).unwrap()).unwrap();
            std::fs::copy(&thin, &bin).unwrap();
            let unsigned = command(aterm_uds::os_path!("codesign"))
                .args(["--verify", "--strict"])
                .arg(&bin)
                .output()
                .unwrap();
            assert!(
                !unsigned.status.success(),
                "patched page must invalidate the old signature"
            );
            assert!(
                crate::artifact::content_digest(&app).is_err(),
                "U evidence refuses an invalid signature"
            );
            resign(&thin).unwrap();
            assert!(
                keep_unstamped(&thin, &store).is_err(),
                "a stamped slice cannot enter the raw store"
            );
            std::fs::copy(&thin, &bin).unwrap();
            assert_eq!(normalized(&std::fs::read(&bin).unwrap()).unwrap(), expected);
            assert_eq!(
                normalized(&fat(&std::fs::read(&bin).unwrap())).unwrap(),
                fat_content
            );
            let content = crate::artifact::content_digest(&app).unwrap();
            if let Some(expected) = &bundle_content {
                assert_eq!(&content, expected);
            }
            bundle_content = Some(content.clone());
            let source = crate::artifact::Identity {
                version: version.into(),
                build,
                commit: format!("{commit}{commit}{commit}0000"),
            };
            let compilers = crate::artifact::Compilers {
                trustc: "trust".into(),
                rustc: "stable".into(),
                inputs: "seal".into(),
            };
            let lookup = crate::artifact::key(&source, &compilers);
            let address = crate::artifact::stamped_key(&content, &lookup, None);
            assert_ne!(
                address,
                crate::artifact::stamped_key(&"ff".repeat(32), &lookup, None)
            );
            assert_ne!(
                address,
                crate::artifact::stamped_key(&content, &lookup, Some("1..2")),
                "the stamp address includes the index hint"
            );
            let complete = crate::artifact::Complete {
                schema: crate::artifact::SCHEMA.into(),
                key: address.clone(),
                version: source.version,
                build: source.build,
                commit: source.commit,
                trustc: compilers.trustc,
                rustc: compilers.rustc,
                inputs: compilers.inputs,
                snapshot: "snapshot".into(),
                archs: "arm64".into(),
                compiler_line: "Trust".into(),
                macho_sha256: crate::artifact::macho_sha256(&app).unwrap(),
                bundle_digest: crate::artifact::bundle_digest(&app).unwrap(),
                content: Some(content.clone()),
                slices: Vec::new(),
                index_hint: None,
            };
            let entry = store.join(&address);
            std::fs::create_dir_all(&entry).unwrap();
            crate::artifact::copy_bundle(&complete, &app, &complete.app(&store)).unwrap();
            std::fs::write(
                entry.join(crate::artifact::COMPLETE_FILE),
                aterm_toml::to_string(&complete).unwrap(),
            )
            .unwrap();
            std::fs::create_dir_all(store.join("identity")).unwrap();
            std::fs::write(store.join("identity").join(&lookup), &address).unwrap();
            assert_eq!(
                crate::artifact::read(&store, &lookup).unwrap(),
                Some(complete.clone())
            );
            assert_eq!(
                crate::artifact::read(&store, &address).unwrap(),
                Some(complete.clone())
            );
            let plist_path = app.join("Contents/Info.plist");
            let plist = std::fs::read_to_string(&plist_path).unwrap();
            std::fs::write(
                &plist_path,
                crate::bundle::set_plist_string(&plist, "CFBundleVersion", "999").unwrap(),
            )
            .unwrap();
            let mut wrong_plist = complete.clone();
            wrong_plist.bundle_digest = crate::artifact::bundle_digest(&app).unwrap();
            assert_eq!(crate::artifact::content_digest(&app).unwrap(), content);
            assert!(
                crate::artifact::prove(&wrong_plist, &app).is_err(),
                "equal U and matching hashes cannot hide the wrong plist identity"
            );
            std::fs::write(&plist_path, plist).unwrap();
            let mut wrong_stamp = complete;
            wrong_stamp.commit = "ff".repeat(20);
            assert!(
                crate::artifact::prove(&wrong_stamp, &app).is_err(),
                "equal U does not prove the reservation's stamp"
            );
            let wrong = "ab".repeat(32);
            std::fs::write(store.join("identity").join(&wrong), &address).unwrap();
            assert!(
                crate::artifact::read(&store, &wrong).is_err(),
                "the index cannot substitute another reservation"
            );
            std::fs::write(&resource, "changed product help").unwrap();
            assert_ne!(crate::artifact::content_digest(&app).unwrap(), content);
            let mut custody = KeptUntilRan::new(copy);
            let mut child = command(&bin).stdout(Stdio::piped()).spawn().unwrap();
            prints(
                &mut child,
                &format!("identity {version}+g{commit}"),
                FIRST_EXEC_HANG,
            );
            custody.ran();
            assert!(child.wait().unwrap().success());
        }
        let mut changed = original.clone();
        let section = layout(&changed).unwrap().stamp;
        // A byte immediately before the dedicated identity section remains product content.
        changed[section.start - 1] ^= 1;
        assert_ne!(normalized(&changed).unwrap(), expected);
        std::fs::write(&kept, &changed).unwrap();
        resign(&kept).unwrap();
        assert!(
            keep_unstamped(&target.join("debug/stamp-fixture"), &store).is_err(),
            "the content address detects changed signed product bytes"
        );
        std::fs::write(&kept, &original).unwrap();
        let unused = store.join("unstamped").join("aa".repeat(32));
        std::fs::write(&unused, "obsolete raw slice").unwrap();
        prune_unstamped(&store, std::slice::from_ref(&kept)).unwrap();
        assert!(kept.exists() && !unused.exists());
        assert!(stamp(&project.join("absent"), &aterm_stamp::UNSTAMPED_RECORD).is_err());
        // A source hop in the product, rather than its tests, changes the linked content too.
        let source = project.join("src/main.rs");
        let text = std::fs::read_to_string(&source)
            .unwrap()
            .replace("identity {}", "changed identity {}");
        std::fs::write(source, text).unwrap();
        let output = command(crate::gates::trust_stage2_bin().unwrap().join("targo"))
            .args(["--unverified", "build", "--offline", "--target-dir"])
            .arg(&target)
            .current_dir(&project)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_ne!(
            normalized(&std::fs::read(target.join("debug/stamp-fixture")).unwrap()).unwrap(),
            expected
        );
    }
}
