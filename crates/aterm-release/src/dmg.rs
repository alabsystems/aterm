// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bundle packaging (release spec §6 `dmg.rs`). Two containers carry the SAME
//! signed, lean `aterm.app`:
//!
//! * the **DMG** — `hdiutil create` UDZO with the `/Applications` symlink (the
//!   pretty create-dmg layout was deliberately dropped — spec decision 20). This
//!   is the human download, under the fleet-pinned bare `aterm-<v>.dmg` name.
//! * the **zip** — `ditto -c -k --sequesterRsrc --keepParent`. This is what the
//!   in-app updater stages from, because `hdiutil attach` needs a live bootstrap
//!   context (DiskImages registers with the `com.apple.hdiejectd` XPC service)
//!   and the survivor of a seamless overlap update is an orphan whose launchd
//!   job has exited — every attach from there fails ENXIO. `ditto` speaks to no
//!   XPC service, so it works from any process context.
//!
//! Both digests are computed in-process via `aterm-digest`, so the digest written into
//! the manifest is provably the digest of the file we just produced.
//!
//! Port of `apps/aterm-mac/make-dmg.sh`, hdiutil branch only. The signed .app
//! goes in AS-IS: run this AFTER `sign::sign_app` (both containers freeze the
//! app's bytes), and hand the DMG to `sign::sign_and_notarize_dmg` next.
//!
//! RETIRED 2026-08-26 (owner direction: ONE lean self-provisioning macOS
//! download): the batteries-included seed and with it the whole restage
//! family — the seed-stripping zip stage, the per-arch `aterm-<v>-x86_64.dmg`
//! filter (`create_arch_filtered`), the `aterm-<v>-lite.dmg` twin
//! (`create_lite`) and the post-restage codesign/Gatekeeper re-proof. The
//! bundle `bundle::assemble` hands over carries no seed, so both containers
//! image it exactly as signed, with nothing subtracted.

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
/// stands.
///
/// The artifact name MUST stay `aterm-{short}.dmg` — it is the exact asset
/// name written into the manifest's `dmg`/`url` fields, and every installed
/// client resolves its download by that name (a mismatch 404s the whole
/// fleet's update). The BYTES under the name are the lean app: nothing in the
/// deployed fleet ever required them to carry a toolchain seed, and since
/// 2026-08-26 nothing does.
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
///
/// Archives the bundle AS SIGNED (and, on the active Apple tier, as stapled):
/// the seed-stripping restage that used to run here is retired with the seed
/// itself, so the bytes `ditto` reads are exactly the bytes the DMG carries.
pub fn create_zip(app: &Path, out_dir: &Path, short_version: &str) -> Result<Packaged, String> {
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

    println!("==> ditto -c -k {} (updater container)", zip.display());
    archive_app(app, &zip)?;

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

/// The mounted window's geometry, in Finder's coordinates: broad enough to hold
/// the app and the `/Applications` alias side by side, with the drag going
/// left → right. Numbers, not prose, so the layout is reviewable.
///
/// (This used to read "a window / wide enough to hold …", and OB-17's reserved
/// vocabulary matched the accidental adjacency of those two words across the
/// line break. It was never a scope claim — the window is broad, it does not
/// scope anything — so the sentence is reworded rather than waived: a waiver
/// spent on a false positive teaches the next reader that the channel is for
/// noise.)
const WIN_LEFT: u32 = 200;
const WIN_TOP: u32 = 160;
const WIN_WIDTH: u32 = 620;
const WIN_HEIGHT: u32 = 400;
const ICON_SIZE: u32 = 128;
/// Icon centres inside the window's content area.
const APP_ICON_X: u32 = 160;
const APP_ICON_Y: u32 = 205;
const DROP_ICON_X: u32 = 460;
const DROP_ICON_Y: u32 = 205;

// The geometry must describe a window a user can actually drag across: the
// app on the left, the /Applications alias on the right, both fully visible.
const _: () = {
    assert!(APP_ICON_X < DROP_ICON_X, "the drag must go left to right");
    assert!(
        DROP_ICON_X + ICON_SIZE / 2 <= WIN_WIDTH,
        "the /Applications icon falls outside the window"
    );
    assert!(
        APP_ICON_X > ICON_SIZE / 2,
        "the app icon is clipped by the left edge"
    );
    assert!(
        APP_ICON_Y + ICON_SIZE / 2 <= WIN_HEIGHT && DROP_ICON_Y + ICON_SIZE / 2 <= WIN_HEIGHT,
        "an icon falls below the window"
    );
};

/// Set to anything non-empty to skip the decorated window and cut the plain
/// image directly — the escape hatch for a machine where driving Finder is not
/// wanted (and the switch the tests use).
const NO_WINDOW_ENV: &str = "ATERM_DMG_NO_WINDOW";

/// The Finder script that lays the window out. Written against the MOUNTED
/// volume by name; every value comes from the constants above.
fn window_applescript(volname: &str) -> String {
    format!(
        r#"tell application "Finder"
  tell disk "{volname}"
    open
    set current view of container window to icon view
    set toolbar visible of container window to false
    set statusbar visible of container window to false
    set the bounds of container window to {{{left}, {top}, {right}, {bottom}}}
    set opts to the icon view options of container window
    set arrangement of opts to not arranged
    set icon size of opts to {icon}
    set position of item "aterm.app" of container window to {{{ax}, {ay}}}
    set position of item "Applications" of container window to {{{dx}, {dy}}}
    close
    open
    update without registering applications
    delay 1
  end tell
end tell
"#,
        volname = volname,
        left = WIN_LEFT,
        top = WIN_TOP,
        right = WIN_LEFT + WIN_WIDTH,
        bottom = WIN_TOP + WIN_HEIGHT,
        icon = ICON_SIZE,
        ax = APP_ICON_X,
        ay = APP_ICON_Y,
        dx = DROP_ICON_X,
        dy = DROP_ICON_Y,
    )
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

    // THE DECORATED WINDOW (reinstating what spec decision 20 dropped; owner
    // 2026-08-30). The drag to /Applications is not decoration — it is the
    // gesture that clears App Translocation, and a copy launched from anywhere
    // else can neither self-update nor put `aterm` on PATH. A window that opens
    // with the app on the left and the Applications alias on the right is the
    // only thing in the download that says so.
    //
    // FAIL-SOFT, DELIBERATELY. Laying the window out needs Finder, and Finder
    // automation can be refused by TCC on a headless or freshly-provisioned
    // cutting machine. A refused prompt must never cost a release, so every
    // step below falls back to the plain image the cut has always produced.
    // The DMG's bytes are not signed at this point (the container signature and
    // the notarization staple are applied downstream), so the fallback is a
    // straight substitution, not a half-built artifact.
    if std::env::var_os(NO_WINDOW_ENV).is_none_or(|v| v.is_empty()) {
        match build_decorated(stage, volname, dmg) {
            Ok(()) => return Ok(()),
            Err(why) => {
                println!("==> DMG window layout skipped ({why}); cutting the plain image");
                let _ = std::fs::remove_file(dmg);
            }
        }
    }

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

/// Build the image the long way so Finder can lay the window out: a read-write
/// image, mounted; the script above; then a compressed copy. Any failure is
/// returned so the caller can fall back — and the mount is detached on EVERY
/// path, because a leaked mount outlives the cut and wedges the next one.
#[cfg(unix)]
fn build_decorated(stage: &Path, volname: &str, dmg: &Path) -> Result<(), String> {
    let rw = dmg.with_extension("rw.dmg");
    let _ = std::fs::remove_file(&rw);
    println!("==> hdiutil create {} (UDRW, for layout)", rw.display());
    run_quiet(
        Command::new("hdiutil")
            .arg("create")
            .args(["-volname", volname])
            .arg("-srcfolder")
            .arg(stage)
            .args(["-fs", "HFS+", "-format", "UDRW", "-ov"])
            .arg(&rw),
        "hdiutil create (UDRW)",
    )?;

    // -nobrowse keeps the volume out of the Finder sidebar; it is still
    // scriptable by name, which is what the AppleScript addresses.
    let mount = dmg.with_extension("mnt");
    let _ = std::fs::remove_dir_all(&mount);
    std::fs::create_dir_all(&mount).map_err(|e| format!("create {}: {e}", mount.display()))?;
    run_quiet(
        Command::new("hdiutil")
            .arg("attach")
            .arg(&rw)
            .args(["-nobrowse", "-mountpoint"])
            .arg(&mount),
        "hdiutil attach (layout)",
    )
    .inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&mount);
        let _ = std::fs::remove_file(&rw);
    })?;

    let laid_out = run_quiet(
        Command::new("osascript")
            .arg("-e")
            .arg(window_applescript(volname)),
        "osascript (DMG window layout)",
    );
    // Detach BEFORE deciding what the layout result means: the mount must go
    // whether Finder cooperated or not.
    let detached = run_quiet(
        Command::new("hdiutil")
            .arg("detach")
            .arg(&mount)
            .arg("-force"),
        "hdiutil detach (layout)",
    );
    let _ = std::fs::remove_dir_all(&mount);
    if let Err(e) = laid_out {
        let _ = std::fs::remove_file(&rw);
        return Err(e);
    }
    detached.inspect_err(|_| {
        let _ = std::fs::remove_file(&rw);
    })?;

    println!("==> hdiutil convert -> {} (UDZO)", dmg.display());
    let converted = run_quiet(
        Command::new("hdiutil")
            .arg("convert")
            .arg(&rw)
            .args(["-format", "UDZO", "-ov", "-o"])
            .arg(dmg),
        "hdiutil convert (UDZO)",
    );
    let _ = std::fs::remove_file(&rw);
    converted
}

/// The decorated path is macOS-only for the same reason the plain one is.
#[cfg(not(unix))]
fn build_decorated(_stage: &Path, _volname: &str, _dmg: &Path) -> Result<(), String> {
    Err("DMG window layout requires macOS".into())
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
    use aterm_digest::Sha256;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The script addresses the volume BY NAME — the name `create` derives from
    /// the version — and carries the geometry above. A script that names the
    /// wrong disk would lay out whatever else happens to be mounted.
    #[test]
    fn applescript_addresses_the_volume_and_carries_the_geometry() {
        let script = window_applescript("aterm 9.9.0");
        assert!(script.contains(r#"tell disk "aterm 9.9.0""#), "{script}");
        assert!(
            script.contains(&format!(
                "set the bounds of container window to {{{}, {}, {}, {}}}",
                WIN_LEFT,
                WIN_TOP,
                WIN_LEFT + WIN_WIDTH,
                WIN_TOP + WIN_HEIGHT
            )),
            "{script}"
        );
        assert!(
            script.contains(&format!(
                r#"set position of item "aterm.app" of container window to {{{APP_ICON_X}, {APP_ICON_Y}}}"#
            )),
            "{script}"
        );
        assert!(
            script.contains(&format!(
                r#"set position of item "Applications" of container window to {{{DROP_ICON_X}, {DROP_ICON_Y}}}"#
            )),
            "{script}"
        );
        assert!(
            script.contains(&format!("set icon size of opts to {ICON_SIZE}")),
            "{script}"
        );
    }

    /// A volume name with a quote in it would end the AppleScript string early
    /// and run whatever followed. The names this cut produces are
    /// `aterm <version>`, so this is a guard against a future caller, not a
    /// live bug — but the guard belongs in the test, not in a comment.
    #[test]
    fn volume_names_this_cut_produces_carry_no_quotes() {
        for v in ["0.67.0", "1.0.0", "0.100.0"] {
            let volname = format!("aterm {v}");
            assert!(!volname.contains('"'), "{volname}");
            let script = window_applescript(&volname);
            assert_eq!(
                script.matches('"').count() % 2,
                0,
                "unbalanced quotes in the script for {volname}"
            );
        }
    }

    /// The decorated path must fail CLEANLY when it cannot build the image —
    /// returning an error for the caller to fall back on, and leaving neither
    /// the read-write image nor the mount directory behind. Finder is never
    /// reached here: `hdiutil create` refuses the missing stage first.
    #[cfg(unix)]
    #[test]
    fn decorated_build_fails_clean_and_leaves_nothing() {
        let tmp = std::env::temp_dir().join(format!("aterm-dmg-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).expect("temp dir");
        let dmg = tmp.join("aterm-9.9.0.dmg");
        let missing_stage = tmp.join("no-such-stage");

        let err = build_decorated(&missing_stage, "aterm 9.9.0", &dmg)
            .expect_err("a missing stage must not produce an image");
        assert!(!err.is_empty());

        assert!(
            !dmg.with_extension("rw.dmg").exists(),
            "left the UDRW image behind"
        );
        assert!(
            !dmg.with_extension("mnt").exists(),
            "left the mount dir behind"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
