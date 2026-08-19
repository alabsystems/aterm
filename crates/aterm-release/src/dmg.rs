// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bundle packaging (release spec §6 `dmg.rs`). Two containers carry the SAME
//! signed `aterm.app`:
//!
//! * the **DMG** — `hdiutil create` UDZO with the `/Applications` symlink (the
//!   pretty create-dmg layout was deliberately dropped — spec decision 20). This
//!   is the human download.
//! * the **zip** — `ditto -c -k --sequesterRsrc --keepParent`. This is what the
//!   in-app updater stages from, because `hdiutil attach` needs a live bootstrap
//!   context (DiskImages registers with the `com.apple.hdiejectd` XPC service)
//!   and the survivor of a seamless overlap update is an orphan whose launchd
//!   job has exited — every attach from there fails ENXIO. `ditto` speaks to no
//!   XPC service, so it works from any process context.
//!
//! Both digests are computed in-process via `sha2`, so the digest written into
//! the manifest is provably the digest of the file we just produced.
//!
//! Port of `apps/aterm-mac/make-dmg.sh`, hdiutil branch only. The signed .app
//! goes in AS-IS: run this AFTER `sign::sign_app` (both containers freeze the
//! app's bytes), and hand the DMG to `sign::sign_and_notarize_dmg` next.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A packaged artifact (DMG or zip): the release asset path, its exact byte
/// digest (→ manifest `sha256`/`zip_sha256`, publish self-check) and size
/// (transcript).
pub struct Packaged {
    pub path: PathBuf,
    pub sha256: String,
    pub size_bytes: u64,
}

/// Package `<app>` into `<out_dir>/aterm-<short>.dmg`.
///
/// The artifact name MUST stay `aterm-{short}.dmg` — it is the exact asset
/// name written into the manifest's `dmg`/`url` fields, and every installed
/// v0.25 client resolves its download by that name (a mismatch 404s the whole
/// fleet's update).
pub fn create(app: &Path, out_dir: &Path, short_version: &str) -> Result<Packaged, String> {
    if !app.is_dir() {
        return Err(format!(
            "{} not found — assemble the bundle first",
            app.display()
        ));
    }
    let dmg = out_dir.join(format!("aterm-{short_version}.dmg"));
    // Volume name matches make-dmg.sh ("aterm 0.2.0") — what Finder shows when
    // the image is mounted.
    let volname = format!("aterm {short_version}");
    let _ = std::fs::remove_file(&dmg);

    // Stage the .app + an /Applications symlink in a scratch dir, so the
    // mounted image offers the standard drag-to-install gesture even without
    // create-dmg's decorated window. The stage lives under dist/ (same
    // filesystem, already Spotlight-excluded via .metadata_never_index) and is
    // removed on every exit path.
    let stage = out_dir.join(format!(".dmg-stage-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage).map_err(|e| format!("create {}: {e}", stage.display()))?;
    let result = build_in_stage(app, &stage, &volname, &dmg);
    let _ = std::fs::remove_dir_all(&stage); // cleanup on success AND failure
    result?;

    let size_bytes = std::fs::metadata(&dmg)
        .map_err(|e| format!("stat {}: {e}", dmg.display()))?
        .len();
    // In-process digest of the final bytes (spec §6): this exact string feeds
    // the manifest and the post-publish byte-identity check.
    let sha256 = sha256_file(&dmg)?;
    println!(
        "==> done: {} ({:.1} MB)",
        dmg.display(),
        size_bytes as f64 / 1_000_000.0
    );
    println!("    sha256: {sha256}");
    Ok(Packaged {
        path: dmg,
        sha256,
        size_bytes,
    })
}

/// Package `<app>` into `<out_dir>/aterm-<short>-mac.zip` — the container the
/// in-app updater downloads and extracts.
///
/// `ditto -c -k --sequesterRsrc --keepParent` is the ONLY supported way to
/// archive a signed bundle: it preserves extended attributes and the
/// `_CodeSignature` layout (so the extracted app still verifies), `--keepParent`
/// puts `aterm.app` at the archive root (the client extracts and expects exactly
/// that name), and `--sequesterRsrc` keeps resource forks in the standard
/// `__MACOSX` sidecar rather than mangling them. `zip(1)` would silently drop
/// the metadata the signature seals.
///
/// The artifact name MUST stay `aterm-{short}-mac.zip`: it is the exact asset
/// name written into the manifest's `zip` field, and the client re-derives that
/// same string from the release tag and refuses anything else.
pub fn create_zip(
    app: &Path,
    out_dir: &Path,
    short_version: &str,
    notarized: bool,
) -> Result<Packaged, String> {
    if !app.is_dir() {
        return Err(format!(
            "{} not found — assemble the bundle first",
            app.display()
        ));
    }
    let zip = out_dir.join(format!("aterm-{short_version}-mac.zip"));
    // `ditto -c -k` would overwrite an existing archive, but a stale one from an
    // earlier attempt must not survive a FAILED run and get hashed as this cut's
    // artifact — remove it first, exactly as `create` does for the DMG.
    let _ = std::fs::remove_file(&zip);

    // THE LEAN UPDATER CONTAINER (§9.1 / docs/GOLDEN-INSTALL-PATH.md §4).
    //
    // The DMG ships the batteries-included bundle; this zip must NOT. It is what
    // every self-updating install downloads, and the toolchain seed inside the
    // bundle is a BOOTSTRAP-only payload — an already-provisioned machine can
    // never use it again (`cmd_seed` returns immediately once anything is
    // installed). Archiving it here would make every app update re-transfer
    // ~600-800 MB of bytes the receiving machine is guaranteed to ignore: ~15x
    // the update, forever, for nothing.
    //
    // Stripping it is safe because of WHERE it lives, not by luck. The payload
    // sits in a `.lproj` directory, which codesign's built-in v2 rules seal with
    // `optional = true` — measured on macOS 26.5.2: the bundle verifies with the
    // payload, and verifies with it ENTIRELY ABSENT. So the lean zip carries the
    // same signature and the same notarization staple as the fat DMG, from ONE
    // signing and ONE notarization. (`ditto` has no `--exclude`; a staged
    // copy-then-delete is the simple mechanism, and `cp -R` preserves the seal
    // and xattrs exactly as `build_in_stage` relies on.)
    let staged = strip_seed_into_stage(app, out_dir)?;
    let source = staged.as_ref().map_or(app, |s| s.app.as_path());
    // PROVE the two things this artifact's whole design rests on, before it is
    // hashed and handed to the fleet. Everything downstream — one notarization,
    // a 51 MB update instead of 850 MB, and `reclaim_bundled_seed` deleting a
    // sealed resource from an installed app — is downstream of "the stripped
    // bundle still verifies". Until this gate existed, that was an assumption
    // measured once on a scratch bundle and never checked on the artifact
    // actually shipped: a silent no-op in the strip would ship an 850 MB
    // "updater zip" to every client forever, and a wrong seal assumption would
    // ship a zip that fails verification on every client. Both are fleet-wide
    // and neither is recoverable by a resume.
    // `#[cfg(unix)]` like the function it calls: `verify_stripped_bundle` shells out
    // to codesign/spctl, which exist only on macOS, and calling it unconditionally
    // broke the non-unix build outright. (`archive_app` already refuses there, so
    // nothing is lost — a non-unix cut cannot produce this container at all.)
    #[cfg(unix)]
    if staged.is_some() {
        verify_stripped_bundle(source, notarized)?;
    }
    #[cfg(not(unix))]
    let _ = notarized;

    println!("==> ditto -c -k {} (updater container)", zip.display());
    archive_app(source, &zip)?;
    drop(staged); // remove the stage before hashing, so a failure cannot leave it behind

    let size_bytes = std::fs::metadata(&zip)
        .map_err(|e| format!("stat {}: {e}", zip.display()))?
        .len();
    let sha256 = sha256_file(&zip)?;
    println!(
        "==> done: {} ({:.1} MB)",
        zip.display(),
        size_bytes as f64 / 1_000_000.0
    );
    println!("    sha256: {sha256}");
    Ok(Packaged {
        path: zip,
        sha256,
        size_bytes,
    })
}

/// A scratch copy of the signed bundle with the toolchain seed removed, deleted
/// on drop. `None` when the bundle carries no seed (an `ATERM_SEEDLESS=1` cut),
/// in which case the caller archives the original and copies nothing.
///
/// Declared on every platform (only ever CONSTRUCTED on unix) so `create_zip`'s
/// body stays one shape — a `cfg`-divergent return type here would leave the
/// non-unix build failing to compile on a line nobody looks at.
struct SeedlessStage {
    #[cfg_attr(not(unix), allow(dead_code))]
    dir: PathBuf,
    #[cfg_attr(not(unix), allow(dead_code))]
    app: PathBuf,
}

impl Drop for SeedlessStage {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Copy `app` to a scratch stage and delete its `Contents/Resources/*.lproj`
/// toolchain seed, returning the stage (or `None` when there is no seed to strip).
///
/// The ORIGINAL bundle is never touched: it is the notarized, stapled artifact the
/// DMG was built from, and mutating it after signing is the one thing this whole
/// area of the pipeline forbids.
#[cfg(unix)]
fn strip_seed_into_stage(app: &Path, out_dir: &Path) -> Result<Option<SeedlessStage>, String> {
    let seed_rel = Path::new("Contents/Resources").join(atpkg::SEED_DIR_NAME);
    if !app.join(&seed_rel).is_dir() {
        return Ok(None); // seedless cut — the zip is already lean
    }
    let dir = out_dir.join("zip-stage");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let stage = SeedlessStage {
        app: dir.join(app.file_name().ok_or("bundle path has no file name")?),
        dir,
    };
    // `cp -Rc` — clone, not copy. `-c` uses APFS copy-on-write, so staging a
    // ~640 MB bundle costs metadata instead of writing every byte a second time
    // (and a third, since only the stripped remainder is then read back out).
    // Falls back to a plain `-R` on any filesystem that cannot clone, which is
    // the pre-existing behaviour rather than a new failure mode. Either way it
    // is `cp`, not a hand-rolled walk: symlinks, extended attributes and the
    // codesign seal must survive verbatim (the same reason `build_in_stage`
    // uses it).
    let cloned = run_quiet(
        Command::new("cp").args(["-Rc"]).arg(app).arg(&stage.dir),
        "cp -Rc app into zip stage",
    );
    if cloned.is_err() {
        let _ = std::fs::remove_dir_all(&stage.app);
        run_quiet(
            Command::new("cp").arg("-R").arg(app).arg(&stage.dir),
            "cp -R app into zip stage (clone unsupported here)",
        )?;
    }
    let seed = stage.app.join(&seed_rel);
    let bytes = dir_size(&seed);
    std::fs::remove_dir_all(&seed).map_err(|e| format!("strip {}: {e}", seed.display()))?;
    println!(
        "    stripped {} from the updater zip ({:.1} MB — the DMG keeps it)",
        atpkg::SEED_DIR_NAME,
        bytes as f64 / 1_000_000.0
    );
    Ok(Some(stage))
}

/// Prove the stripped bundle is (a) actually stripped and (b) still valid.
///
/// This is the gate that turns the `.lproj` optional-seal property from a
/// measured claim about a scratch bundle into a checked property of THIS cut's
/// artifact. It runs on the real signed — and, on the active Apple tier,
/// notarized and stapled — bundle, which is the only place the notarization leg
/// can be observed at all: `codesign --verify --deep --strict` covers the seal,
/// and `spctl --assess` covers Gatekeeper's separate assessment path, which is
/// NOT the same code path and is the one a stapled ticket participates in.
///
/// Fail-closed on both. A cut that cannot prove its updater container verifies
/// must not publish one.
#[cfg(unix)]
fn verify_stripped_bundle(app: &Path, notarized: bool) -> Result<(), String> {
    let seed = app.join("Contents/Resources").join(atpkg::SEED_DIR_NAME);
    if seed.exists() {
        return Err(format!(
            "the updater zip's staged bundle STILL carries {} — the strip silently did \
             nothing, and every client would download the seeded bundle on every update",
            seed.display()
        ));
    }
    run_quiet(
        Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict", "--verbose=2"])
            .arg(app),
        "codesign --verify the stripped updater bundle",
    )
    .map_err(|e| {
        format!(
            "{e}\n    The stripped bundle does NOT verify. This is the assumption the whole \
             lean-update design rests on: `Contents/Resources/*.lproj` is supposed to be \
             sealed `optional = true`, so removing it leaves the signature valid. If this \
             fires, that is false for this bundle — do NOT ship; the updater zip would fail \
             verification on every client."
        )
    })?;
    // Gatekeeper's assessment is a DIFFERENT evaluation from codesign's, and it is
    // the one that consults the stapled notarization ticket — the single leg of this
    // design that cannot be observed any other way.
    //
    // `notarized` is what makes the result MEAN something. On the inactive (ad-hoc)
    // tier there is no signing identity, so spctl rejects every bundle and a failure
    // proves nothing — reporting it is all that is honest. On the ACTIVE tier the
    // bundle really was Developer-ID signed, notarized and stapled, so a rejection
    // says the ticket does not survive the strip, and shipping anyway would hand a
    // zip that fails Gatekeeper to the entire fleet. Leaving that as a println on the
    // one tier where it is decisive meant the whole lean-zip design could ship
    // unproven and fail everywhere at once.
    match Command::new("/usr/sbin/spctl")
        .args(["--assess", "--type", "exec", "-vv"])
        .arg(app)
        .output()
    {
        Ok(out) if out.status.success() => {
            println!("    spctl accepts the stripped bundle (notarization leg proven)");
            Ok(())
        }
        Ok(out) => {
            let why = String::from_utf8_lossy(&out.stderr).trim().replace('\n', "; ");
            if notarized {
                Err(format!(
                    "the STRIPPED updater bundle is rejected by Gatekeeper: {why}. This cut is \
                     Developer-ID signed and notarized, so this is decisive: the stapled ticket \
                     does not survive removing the `.lproj` payload, and the lean updater zip \
                     would fail verification on every client. Do not publish — re-examine the \
                     lean-zip design (docs/GOLDEN-INSTALL-PATH.md §4)."
                ))
            } else {
                println!(
                    "    NOTE: spctl did not accept the stripped bundle — {why}. Expected on an \
                     ad-hoc/unsigned cut (no signing identity exists, so spctl rejects \
                     everything); this check becomes FATAL on a notarized cut."
                );
                Ok(())
            }
        }
        Err(e) => {
            if notarized {
                Err(format!(
                    "could not run spctl ({e}) — a notarized cut must not publish an updater \
                     zip whose Gatekeeper acceptance is unknown"
                ))
            } else {
                println!("    NOTE: could not run spctl ({e}) — notarization leg unchecked");
                Ok(())
            }
        }
    }
}

/// Recursive byte count, best-effort — this only feeds a log line, so an
/// unreadable entry contributes zero rather than failing the cut.
#[cfg(unix)]
fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            _ => e.metadata().map(|m| m.len()).unwrap_or(0),
        })
        .sum()
}

/// Non-unix: `archive_app` already refuses here, so there is nothing to stage.
#[cfg(not(unix))]
fn strip_seed_into_stage(_app: &Path, _out_dir: &Path) -> Result<Option<SeedlessStage>, String> {
    Ok(None)
}

#[cfg(unix)]
fn archive_app(app: &Path, zip: &Path) -> Result<(), String> {
    run_quiet(
        Command::new("/usr/bin/ditto")
            .args(["-c", "-k", "--sequesterRsrc", "--keepParent"])
            .arg(app)
            .arg(zip),
        "ditto -c -k app into updater zip",
    )
}

/// Archiving a signed bundle without losing its seal is a macOS operation
/// (`ditto`, extended attributes); refuse plainly elsewhere.
#[cfg(not(unix))]
fn archive_app(_app: &Path, _zip: &Path) -> Result<(), String> {
    Err("bundle zip creation requires macOS (ditto); build releases on a Mac".into())
}

#[cfg(unix)]
fn build_in_stage(app: &Path, stage: &Path, volname: &str, dmg: &Path) -> Result<(), String> {
    // `cp -R` (not a hand-rolled walk): preserves symlinks, extended
    // attributes and the codesign seal exactly — the .app inside the DMG must
    // be byte-identical to the one we just signed.
    run_quiet(
        Command::new("cp").arg("-R").arg(app).arg(stage),
        "cp -R app into DMG stage",
    )?;
    std::os::unix::fs::symlink("/Applications", stage.join("Applications"))
        .map_err(|e| format!("symlink /Applications: {e}"))?;

    println!("==> hdiutil create {} (UDZO)", dmg.display());
    run_quiet(
        Command::new("hdiutil")
            .arg("create")
            .args(["-volname", volname])
            .arg("-srcfolder")
            .arg(stage)
            .args(["-fs", "HFS+", "-format", "UDZO", "-ov"])
            .arg(dmg),
        "hdiutil create",
    )
}

/// DMG assembly is a macOS operation end-to-end (`hdiutil`, HFS+, the
/// /Applications symlink); refuse plainly elsewhere.
#[cfg(not(unix))]
fn build_in_stage(_app: &Path, _stage: &Path, _volname: &str, _dmg: &Path) -> Result<(), String> {
    Err("DMG creation requires macOS (hdiutil); build releases on a Mac".into())
}

/// Streaming in-process SHA-256 → lowercase hex. Public: the publish
/// self-check re-hashes assets through the same code path.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn run_quiet(cmd: &mut Command, what: &str) -> Result<(), String> {
    let out = cmd.output().map_err(|e| format!("spawn {what}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{what} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}
