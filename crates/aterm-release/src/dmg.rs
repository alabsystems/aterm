// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bundle packaging (release spec §6 `dmg.rs`). Up to four containers carry
//! the SAME signed `aterm.app`:
//!
//! * the **DMG** — `hdiutil create` UDZO with the `/Applications` symlink (the
//!   pretty create-dmg layout was deliberately dropped — spec decision 20). This
//!   is the human download. On a cut whose seed covers both darwin triples it
//!   becomes a PER-ARCH PAIR ([`create_arch_filtered`]): the fleet-pinned bare
//!   `aterm-<v>.dmg` with the arm64 seed slice, plus the additive
//!   `aterm-<v>-x86_64.dmg` with the Intel slice — one signing, one
//!   notarization, two restages of the `optional = true` `.lproj` seal.
//! * the **lite DMG** ([`create_lite`]) — the SEED-STRIPPED (lean) app in the
//!   same drag-install image: the ~28 MB browser download for a machine that
//!   installs its toolchain from the network on first launch. Same restage
//!   proof chain as the zip, same hdiutil lane and container hook as the DMG.
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

use std::collections::BTreeSet;
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

/// Package `<app>` into `<out_dir>/aterm-<short>.dmg` — the app exactly as it
/// stands, seed and all (the single-DMG lane: a seedless or acknowledged
/// arm64-only cut).
///
/// The artifact name MUST stay `aterm-{short}.dmg` — it is the exact asset
/// name written into the manifest's `dmg`/`url` fields, and every installed
/// v0.25 client resolves its download by that name (a mismatch 404s the whole
/// fleet's update). On a per-arch-DMG cut ([`create_arch_filtered`]) the bare
/// name stays pinned to the CANONICAL (arm64-seeded) image for the same
/// reason, and the Intel variant is strictly additive (`-x86_64` suffix): a
/// symmetric `-arm64`/`-x86_64` rename would 404 or refuse every deployed
/// client, which binds this exact spelling.
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
    seeded: bool,
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
    // A SEEDED CUT WITH NO SEAL IS NOT A SEEDLESS CUT. `strip_seed_into_stage`
    // answers `None` for both "this cut ships no toolchain" and "the seal that was
    // here is gone", and on 2026-08-19 the second one happened: the live updater
    // adopted the half-built bundle and its successor reclaimed the payload out of
    // it. The strip became a no-op, so the restage verification below never ran —
    // the container shipped without the codesign + Gatekeeper proof that is the
    // entire reason the lean zip is allowed to exist. Whoever knows this cut is
    // seeded has to say so, because from in here the two states look identical.
    if seeded && staged.is_none() {
        return Err(format!(
            "this cut seals a toolchain, but {} carries no {} — something removed it \
             between bundle and package. Refusing to archive an updater container whose \
             strip was a silent no-op",
            app.display(),
            atpkg::SEED_DIR_NAME
        ));
    }
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
    // `#[cfg(unix)]` like the function it calls: `verify_restaged_bundle` shells out
    // to codesign/spctl, which exist only on macOS, and calling it unconditionally
    // broke the non-unix build outright. (`archive_app` already refuses there, so
    // nothing is lost — a non-unix cut cannot produce this container at all.)
    #[cfg(unix)]
    if staged.is_some() {
        verify_restaged_bundle(source, notarized, SeedExpectation::Absent)?;
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

/// Package a PER-ARCH batteries-included DMG from the one signed (and, on the
/// active tier, notarized + stapled) universal app: restage the bundle with its
/// sealed seed filtered to `triple`'s artifact tarballs plus ALL signed
/// manifests, re-prove the filtered seed through the client's own chain
/// (`seedpack::validate_scoped`, `ArchScope::Only`) and the codesign/Gatekeeper
/// seal (`verify_restaged_bundle`), then image it through the SAME
/// volname/hdiutil lane as [`create`].
///
/// WHY: the dual-arch fat DMG measured 2,090,384,004 bytes on v0.46.0 — 97.3%
/// of the 2 GiB `RELEASE_ASSET_DOWNLOAD_BOUND` — and every download carried
/// ~0.9–1.1 GB of seed tarballs the receiving CPU can never execute (the
/// client installs strictly by its own triple). Nothing is re-signed and
/// nothing signed is altered: the filter subtracts whole artifact FILES from
/// the `optional = true` `.lproj`, the index/roster/pkg manifests and their
/// signatures ride intact, and every client re-verifies sha256 + tree_root at
/// install exactly as today.
///
/// Names: `aarch64-apple-darwin` keeps the fleet-pinned bare `aterm-<v>.dmg`
/// (bare-name-is-arm64 is the only fleet-safe spelling — see [`create`]);
/// `x86_64-apple-darwin` is the additive `aterm-<v>-x86_64.dmg`. Any other
/// triple has no DMG lane and is refused.
#[cfg(unix)]
pub fn create_arch_filtered(
    app: &Path,
    out_dir: &Path,
    short_version: &str,
    triple: &str,
    notarized: bool,
) -> Result<Packaged, String> {
    if !app.is_dir() {
        return Err(format!(
            "{} not found — assemble the bundle first",
            app.display()
        ));
    }
    let dmg = match triple {
        "aarch64-apple-darwin" => out_dir.join(format!("aterm-{short_version}.dmg")),
        "x86_64-apple-darwin" => out_dir.join(format!("aterm-{short_version}-x86_64.dmg")),
        other => {
            return Err(format!(
                "no per-arch DMG lane exists for triple {other:?} (aarch64-apple-darwin \
                 takes the canonical bare name, x86_64-apple-darwin the -x86_64 suffix)"
            ));
        }
    };
    let seed_dir = app
        .join("Contents/Resources")
        .join(atpkg::SEED_DIR_NAME);
    // The keep-set comes from the SEALED seed's own signed [[artifact]] rows,
    // read back through the full client chain — never from filename suffixes,
    // so every byte-selection decision stays anchored in attested data.
    let keep = crate::seedpack::assets_by_triple(&seed_dir)
        .map_err(|e| format!("per-arch DMG: the sealed seed does not re-validate: {e}"))?
        .remove(triple)
        .filter(|set| !set.is_empty())
        .ok_or_else(|| {
            format!(
                "the sealed seed carries no {triple} artifacts — a {triple} DMG variant \
                 is not producible from this seal (publish.rs should not have asked)"
            )
        })?;
    let staged = restage_with_seed_filter(app, out_dir, SeedFilter::KeepAssets(&keep))?
        .ok_or_else(|| {
            format!(
                "per-arch DMG requested but {} carries no sealed seed — a seedless cut \
                 has exactly one DMG",
                app.display()
            )
        })?;
    // Re-prove the FILTERED registry through the shipped client's own chain,
    // scoped to exactly this triple: a dropped manifest, a leaked wrong-arch
    // tarball, or a gutted seed each refuses the cut here, before hdiutil.
    let staged_seed = staged
        .app
        .join("Contents/Resources")
        .join(atpkg::SEED_DIR_NAME);
    crate::seedpack::validate_scoped(&staged_seed, crate::seedpack::ArchScope::Only(triple))
        .map_err(|e| format!("per-arch DMG ({triple}): filtered seed refused: {e}"))?;
    verify_restaged_bundle(&staged.app, notarized, SeedExpectation::FilteredPresent)?;

    let volname = format!("aterm {short_version}");
    let _ = std::fs::remove_file(&dmg);
    let stage = out_dir.join(format!(".dmg-stage-{triple}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage).map_err(|e| format!("create {}: {e}", stage.display()))?;
    let result = build_in_stage(&staged.app, &stage, &volname, &dmg);
    let _ = std::fs::remove_dir_all(&stage); // cleanup on success AND failure
    drop(staged); // remove the filtered app stage before hashing
    result?;

    let size_bytes = std::fs::metadata(&dmg)
        .map_err(|e| format!("stat {}: {e}", dmg.display()))?
        .len();
    let sha256 = sha256_file(&dmg)?;
    println!(
        "==> done: {} ({:.1} MB, {triple} seed)",
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

/// Per-arch DMG assembly is macOS end-to-end (restage + hdiutil); refuse
/// plainly elsewhere, exactly like [`build_in_stage`].
#[cfg(not(unix))]
pub fn create_arch_filtered(
    _app: &Path,
    _out_dir: &Path,
    _short_version: &str,
    _triple: &str,
    _notarized: bool,
) -> Result<Packaged, String> {
    Err("per-arch DMG creation requires macOS (hdiutil); build releases on a Mac".into())
}

/// Package the LEAN (seed-stripped) app into `<out_dir>/aterm-<short>-lite.dmg`
/// — the drag-install browser download for a machine that installs its
/// toolchain from the network on first launch: exactly the bundle the updater
/// zip carries, offered in the DMG gesture the seeded image carries.
///
/// One lane, two proven halves. The strip half is [`create_zip`]'s, verbatim:
/// restage, delete the `optional = true` `.lproj` seal, refuse a seeded cut
/// whose strip was a silent no-op, and re-prove the stripped bundle through
/// codesign + Gatekeeper ([`verify_restaged_bundle`], `SeedExpectation::Absent`)
/// — nothing signed is altered, so the ONE app signing and ONE app notarization
/// cover this container too. The image half is [`create`]'s, verbatim: the same
/// versioned volume name, the same `/Applications` symlink stage, the same
/// `hdiutil create` UDZO lane. The caller then hands this DMG to the SAME
/// container hook the seeded DMG gets (`sign::sign_and_notarize_dmg` — Dev-ID
/// signature, notarization, staple, identical identity pin) and re-hashes it,
/// exactly as `notarize_and_package` sequences for every DMG.
///
/// The `-lite` name is ADDITIVE and elected by NO deployed client: install.sh's
/// asset allowlist and the in-app updater bind only the manifest-named
/// containers, and the signed manifest deliberately does not name this one
/// (see `mirror::dmg_lite_asset_name` for the naming contract). The unversioned
/// `aterm.dmg` download alias is a byte copy of THIS artifact.
#[cfg(unix)]
pub fn create_lite(
    app: &Path,
    out_dir: &Path,
    short_version: &str,
    notarized: bool,
    seeded: bool,
) -> Result<Packaged, String> {
    if !app.is_dir() {
        return Err(format!(
            "{} not found — assemble the bundle first",
            app.display()
        ));
    }
    let dmg = out_dir.join(format!("aterm-{short_version}-lite.dmg"));
    let staged = restage_with_seed_filter(
        app,
        out_dir,
        SeedFilter::Remove {
            stage: "lite-dmg-stage",
        },
    )?;
    // Same refusal, same reason as create_zip: a seeded cut whose strip
    // silently did nothing would ship a ~1 GB image under the name whose whole
    // point is that it is small — and from in here "no seal" and "the seal was
    // removed under us" look identical, so whoever knows must say.
    if seeded && staged.is_none() {
        return Err(format!(
            "this cut seals a toolchain, but {} carries no {} — something removed it \
             between bundle and package. Refusing to image a lite DMG whose strip was \
             a silent no-op",
            app.display(),
            atpkg::SEED_DIR_NAME
        ));
    }
    let source = staged.as_ref().map_or(app, |s| s.app.as_path());
    // The proof the design rests on, per artifact as always: the stripped
    // bundle still verifies (codesign) and still assesses (Gatekeeper, which is
    // where the stapled ticket participates) — decisive on a notarized cut,
    // advisory on an ad-hoc one, exactly as for the zip.
    if staged.is_some() {
        verify_restaged_bundle(source, notarized, SeedExpectation::Absent)?;
    }

    let volname = format!("aterm {short_version}");
    let _ = std::fs::remove_file(&dmg);
    let stage = out_dir.join(format!(".dmg-stage-lite-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage).map_err(|e| format!("create {}: {e}", stage.display()))?;
    let result = build_in_stage(source, &stage, &volname, &dmg);
    let _ = std::fs::remove_dir_all(&stage); // cleanup on success AND failure
    drop(staged); // remove the lean restage before hashing
    result?;

    let size_bytes = std::fs::metadata(&dmg)
        .map_err(|e| format!("stat {}: {e}", dmg.display()))?
        .len();
    let sha256 = sha256_file(&dmg)?;
    println!(
        "==> done: {} ({:.1} MB, lean — seed stripped)",
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

/// Lite-DMG assembly is macOS end-to-end (restage + hdiutil); refuse plainly
/// elsewhere, exactly like [`create_arch_filtered`].
#[cfg(not(unix))]
pub fn create_lite(
    _app: &Path,
    _out_dir: &Path,
    _short_version: &str,
    _notarized: bool,
    _seeded: bool,
) -> Result<Packaged, String> {
    Err("lite DMG creation requires macOS (hdiutil); build releases on a Mac".into())
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

/// How a restage treats the sealed toolchain seed.
#[cfg(unix)]
enum SeedFilter<'a> {
    /// Delete the whole `.lproj` — the LEAN restage, shared by the updater zip
    /// and the lite DMG (neither ever carries the seed). `stage` keeps the two
    /// lanes' scratch dirs distinct, under the same rule as the per-filter dirs
    /// below: two restages in one dist/ must never be able to package each
    /// other's half-built stage.
    Remove { stage: &'static str },
    /// Keep every signed manifest (`*.toml` + `*.toml.sig` — the registry's
    /// index/roster/pkg quads travel INTACT, their signatures untouched) and
    /// only the named artifact tarballs. The per-arch DMG lane: the keep-set
    /// comes from `seedpack::assets_by_triple`, i.e. from signed `[[artifact]]`
    /// target rows, never from filename conventions.
    KeepAssets(&'a BTreeSet<String>),
}

/// What [`verify_restaged_bundle`] must find where the seed used to be.
#[cfg(unix)]
#[derive(Clone, Copy)]
enum SeedExpectation {
    /// The lean zip: the `.lproj` must be GONE (a silent no-op strip once
    /// shipped an 850 MB "updater zip"; see create_zip).
    Absent,
    /// A per-arch DMG: the `.lproj` must still exist and still carry the
    /// signed registry head (`index.toml`) — a filter that deleted the
    /// manifests would ship batteries the client cannot verify, which it
    /// treats as no batteries at all.
    FilteredPresent,
}

/// Copy `app` to a scratch stage and delete its `Contents/Resources/*.lproj`
/// toolchain seed, returning the stage (or `None` when there is no seed to strip).
/// The lean-zip specialization of [`restage_with_seed_filter`].
#[cfg(unix)]
fn strip_seed_into_stage(app: &Path, out_dir: &Path) -> Result<Option<SeedlessStage>, String> {
    restage_with_seed_filter(app, out_dir, SeedFilter::Remove { stage: "zip-stage" })
}

/// Copy `app` to a scratch stage and apply `filter` to its sealed toolchain
/// seed, returning the stage (or `None` when there is no seed to filter).
///
/// The ORIGINAL bundle is never touched: it is the notarized, stapled artifact the
/// DMG was built from, and mutating it after signing is the one thing this whole
/// area of the pipeline forbids. Filtering the STAGE is legal for the same
/// reason stripping it is: the payload lives in a `.lproj` codesign seals
/// `optional = true`, so subtracting whole files from it leaves the signature
/// and the stapled ticket valid — an assumption `verify_restaged_bundle` turns
/// into a per-cut proof rather than leaving measured-once.
#[cfg(unix)]
fn restage_with_seed_filter(
    app: &Path,
    out_dir: &Path,
    filter: SeedFilter<'_>,
) -> Result<Option<SeedlessStage>, String> {
    let seed_rel = Path::new("Contents/Resources").join(atpkg::SEED_DIR_NAME);
    if !app.join(&seed_rel).is_dir() {
        return Ok(None); // seedless cut — nothing to filter
    }
    let dir = out_dir.join(match filter {
        SeedFilter::Remove { stage } => stage.to_string(),
        // Distinct per-filter stage dirs: the zip restage, the lite-DMG restage
        // and two per-arch DMG restages run in the same dist/ during one cut,
        // and sharing a path would let one lane archive another lane's
        // half-built stage.
        SeedFilter::KeepAssets(_) => format!("dmg-arch-stage-{}", std::process::id()),
    });
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
    match filter {
        SeedFilter::Remove { .. } => {
            let bytes = dir_size(&seed);
            std::fs::remove_dir_all(&seed)
                .map_err(|e| format!("strip {}: {e}", seed.display()))?;
            println!(
                "    stripped {} from the lean restage ({:.1} MB — the seeded DMG keeps it)",
                atpkg::SEED_DIR_NAME,
                bytes as f64 / 1_000_000.0
            );
        }
        SeedFilter::KeepAssets(keep) => {
            // Subtract whole artifact files; NEVER touch a signed byte. Every
            // `*.toml`/`*.toml.sig` rides (index, roster, and ALL pkg manifests
            // + signatures — the per-asset sha256/tree_root the client
            // re-verifies at install live inside them), and only tarballs the
            // signed [[artifact]] rows name for this arch stay. Anything else
            // would already have failed `seedpack::validate`'s
            // nothing-unaccounted gate at cut time, so an unrecognized file
            // here is a state that gate has never seen — fail, don't guess.
            let mut kept = 0usize;
            let mut dropped = 0usize;
            let mut dropped_bytes = 0u64;
            for entry in std::fs::read_dir(&seed)
                .map_err(|e| format!("read staged seed {}: {e}", seed.display()))?
            {
                let entry = entry.map_err(|e| format!("read staged seed: {e}"))?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.ends_with(".toml") || name.ends_with(".toml.sig") {
                    kept += 1;
                    continue; // every signed manifest travels intact
                }
                if keep.contains(&name) {
                    kept += 1;
                    continue;
                }
                let path = entry.path();
                dropped_bytes = dropped_bytes.saturating_add(
                    std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
                );
                std::fs::remove_file(&path)
                    .map_err(|e| format!("filter {}: {e}", path.display()))?;
                dropped += 1;
            }
            println!(
                "    filtered {}: kept {kept} file(s), dropped {dropped} other-arch \
                 artifact(s) ({:.1} MB)",
                atpkg::SEED_DIR_NAME,
                dropped_bytes as f64 / 1_000_000.0
            );
        }
    }
    Ok(Some(stage))
}

/// Prove a restaged bundle is (a) actually restaged as intended and (b) still
/// valid — it runs on EVERY restage, lean zip and per-arch DMG alike, so what
/// each container ships is what was proven.
///
/// This is the gate that turns the `.lproj` optional-seal property from a
/// measured claim about a scratch bundle into a checked property of THIS cut's
/// artifact. It runs on the real signed — and, on the active Apple tier,
/// notarized and stapled — bundle, which is the only place the notarization leg
/// can be observed at all: `codesign --verify --deep --strict` covers the seal,
/// and `spctl --assess` covers Gatekeeper's separate assessment path, which is
/// NOT the same code path and is the one a stapled ticket participates in.
/// Should a future macOS tighten the `.lproj` optional-seal rule, this gate
/// stops the CUT — never the fleet.
///
/// Fail-closed on both. A cut that cannot prove its restaged container verifies
/// must not publish one.
#[cfg(unix)]
fn verify_restaged_bundle(
    app: &Path,
    notarized: bool,
    expect: SeedExpectation,
) -> Result<(), String> {
    let seed = app.join("Contents/Resources").join(atpkg::SEED_DIR_NAME);
    match expect {
        SeedExpectation::Absent => {
            if seed.exists() {
                return Err(format!(
                    "the updater zip's staged bundle STILL carries {} — the strip silently did \
                     nothing, and every client would download the seeded bundle on every update",
                    seed.display()
                ));
            }
        }
        SeedExpectation::FilteredPresent => {
            if !seed.join("index.toml").is_file() {
                return Err(format!(
                    "the per-arch DMG's staged bundle lost its signed registry head \
                     ({}/index.toml) — the filter must subtract artifact tarballs only, \
                     never a signed manifest",
                    seed.display()
                ));
            }
        }
    }
    run_quiet(
        Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict", "--verbose=2"])
            .arg(app),
        "codesign --verify the restaged bundle",
    )
    .map_err(|e| {
        format!(
            "{e}\n    The restaged bundle does NOT verify. This is the assumption the whole \
             lean-zip AND per-arch-DMG design rests on: `Contents/Resources/*.lproj` is \
             supposed to be sealed `optional = true`, so subtracting from it leaves the \
             signature valid. If this fires, that is false for this bundle — do NOT ship; \
             the restaged container would fail verification on every client."
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
            println!("    spctl accepts the restaged bundle (notarization leg proven)");
            Ok(())
        }
        Ok(out) => {
            let why = String::from_utf8_lossy(&out.stderr).trim().replace('\n', "; ");
            if notarized {
                Err(format!(
                    "the RESTAGED bundle is rejected by Gatekeeper: {why}. This cut is \
                     Developer-ID signed and notarized, so this is decisive: the stapled ticket \
                     does not survive subtracting from the `.lproj` payload, and this restaged \
                     container would fail verification on every client. Do not publish — \
                     re-examine the lean-zip / per-arch-DMG design \
                     (docs/GOLDEN-INSTALL-PATH.md §4)."
                ))
            } else {
                println!(
                    "    NOTE: spctl did not accept the restaged bundle — {why}. Expected on an \
                     ad-hoc/unsigned cut (no signing identity exists, so spctl rejects \
                     everything); this check becomes FATAL on a notarized cut."
                );
                Ok(())
            }
        }
        Err(e) => {
            if notarized {
                Err(format!(
                    "could not run spctl ({e}) — a notarized cut must not publish a restaged \
                     container whose Gatekeeper acceptance is unknown"
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
