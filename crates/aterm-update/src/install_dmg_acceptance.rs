// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Explicit mounted-volume acceptance of the two installers that attach a release's disk image.

use super::*;

const BUILD: u64 = 777;
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const VERSION: &str = "0.0.777";
const HANG: std::time::Duration = std::time::Duration::from_secs(60);

fn manifest() -> Manifest {
    Manifest {
        #[cfg(test)]
        schema: 1,
        version: VERSION.into(),
        build_number: BUILD,
        commit: Some(COMMIT.into()),
        sha256: "ab".repeat(32),
        dmg: format!("aterm-{VERSION}.dmg"),
        url: None,
        zip: None,
        zip_sha256: None,
        min_build: None,
        changelog: None,
        machine_id: None,
        roster_seq: None,
    }
}

fn staging(dir: &Path) -> std::io::Result<Staging> {
    let root = dir.join("Updates");
    ensure_private_dir(&root)?;
    std::fs::create_dir_all(root.join("download"))?;
    Ok(Staging {
        apply_lock: root.join("apply.lock"),
        stage_lock: root.join("stage.lock"),
        download: root.join("download"),
        staged_app: root.join("staged").join("aterm.app"),
        ready: root.join("ready.toml"),
        status: root.join("status.toml"),
        root,
    })
}

fn release_tree(src: &Path, app: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::create_dir_all(src)?;
    std::os::unix::fs::symlink("/Applications", src.join("Applications"))?;
    if !app {
        return std::fs::write(src.join("README.txt"), b"no app here");
    }
    let bundle = src.join("aterm.app");
    let exe = bundle.join("Contents/MacOS/aterm");
    std::fs::create_dir_all(exe.parent().expect("the executable's folder"))?;
    std::fs::write(
        &exe,
        format!("#!/bin/sh\necho 'aterm {VERSION} (build: {BUILD})'\n"),
    )?;
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755))?;
    std::fs::write(
        bundle.join("Contents/Info.plist"),
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
             <key>CFBundleExecutable</key><string>aterm</string>\
             <key>CFBundleIdentifier</key><string>com.aterm.fixture</string>\
             <key>CFBundleShortVersionString</key><string>{VERSION}</string>\
             <key>CFBundleVersion</key><string>{BUILD}</string>\
             <key>ATermGitCommit</key><string>{COMMIT}</string></dict></plist>\n"
        ),
    )?;
    aterm_uds::deadline::require_trust_service();
    aterm_uds::deadline::sign_ad_hoc(&bundle);
    Ok(())
}

fn create_image(src: &Path, image: &Path) -> std::io::Result<()> {
    let out = aterm_uds::spawnfd::command("/usr/bin/hdiutil")
        .arg("create")
        .arg("-srcfolder")
        .arg(src)
        .args(["-volname", &format!("aterm {VERSION}"), "-format", "UDZO"])
        .arg(image)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()?;
    aterm_uds::deadline::require_disk_image_service(&out);
    if out.status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!("hdiutil create: {out:?}")))
    }
}

fn attached_under(dir: &Path) -> std::io::Result<Vec<(PathBuf, String)>> {
    let out = aterm_uds::spawnfd::command("/usr/bin/hdiutil")
        .arg("info")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(format!("hdiutil info: {out:?}")));
    }
    let prefixes = [
        dir.to_path_buf(),
        dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf()),
    ];
    let mut found = Vec::new();
    let mut image: Option<PathBuf> = None;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some(rest) = line.strip_prefix("image-path") {
            let path = PathBuf::from(rest.trim_start().trim_start_matches(':').trim());
            image = prefixes.iter().any(|p| path.starts_with(p)).then_some(path);
        } else if let (Some(path), Some(dev)) = (&image, line.split_whitespace().next())
            && dev.starts_with("/dev/disk")
        {
            found.push((path.clone(), dev.to_owned()));
            image = None;
        }
    }
    Ok(found)
}

struct DetachAll(PathBuf);

impl Drop for DetachAll {
    fn drop(&mut self) {
        for (_, dev) in attached_under(&self.0).unwrap_or_default() {
            let _ = aterm_uds::spawnfd::command("/usr/bin/hdiutil")
                .args(["detach", "-force", "-quiet"])
                .arg(&dev)
                .status();
        }
    }
}

fn mount_points(staging: &Staging) -> Vec<String> {
    std::fs::read_dir(&staging.root)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|name| name.starts_with("mnt-"))
                .collect()
        })
        .unwrap_or_default()
}

fn prerequisites() -> std::io::Result<()> {
    for tool in ["/usr/bin/hdiutil", "/usr/bin/ditto", "/usr/bin/codesign"] {
        std::fs::metadata(tool)?;
    }
    Ok(())
}

/// THE NO-ZIP LANE STAGES A REAL IMAGE AND LEAVES NOTHING ATTACHED.
/// # Errors
/// A setup failure: no `hdiutil`, or an image that cannot be made or attached.
/// # Panics
/// When the stage fails, leaves a mount point, or leaves an image attached.
pub fn a_real_image_without_a_zip_stages_its_app(dir: &Path) -> std::io::Result<()> {
    prerequisites()?;
    let _detach = DetachAll(dir.to_path_buf());
    let staging = staging(dir)?;
    let src = dir.join("release");
    release_tree(&src, true)?;
    let dmg = dir.join(format!("aterm-{VERSION}.dmg"));
    create_image(&src, &dmg)?;
    let leftover = dir.join("leftover");
    std::fs::create_dir_all(&leftover)?;
    std::fs::write(leftover.join("x"), b"x")?;
    let leftover_dmg = dir.join("leftover.dmg");
    create_image(&leftover, &leftover_dmg)?;
    let stale = staging.root.join("mnt-1");
    std::fs::create_dir_all(&stale)?;
    let out = aterm_uds::spawnfd::command("/usr/bin/hdiutil")
        .args([
            "attach",
            "-nobrowse",
            "-readonly",
            "-noautoopen",
            "-mountpoint",
        ])
        .arg(&stale)
        .arg(&leftover_dmg)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()?;
    aterm_uds::deadline::require_disk_image_service(&out);
    if !out.status.success() {
        return Err(std::io::Error::other(format!("hdiutil attach: {out:?}")));
    }
    assert_eq!(
        attached_under(dir)?.len(),
        1,
        "the killed stage's image is attached"
    );

    stage_from_dmg(&staging, &dmg, &manifest(), "", &now_rfc3339())
        .unwrap_or_else(|why| panic!("the no-zip lane stages the image's app: {why}"));
    let ready = Ready::read(&staging.ready).expect("the stage published its ready marker");
    assert_eq!(ready.build_number, BUILD);
    assert!(
        staging.staged_app.join("Contents/MacOS/aterm").is_file(),
        "the staged bundle is the image's app"
    );
    assert_eq!(
        mount_points(&staging),
        Vec::<String>::new(),
        "no mount point left"
    );
    let attached = attached_under(dir)?;
    assert!(attached.is_empty(), "every image detached: {attached:?}");
    Ok(())
}

/// THE NO-ZIP LANE REFUSES AN IMAGE WITH NO `aterm.app` AND DETACHES IT.
/// # Errors
/// A setup failure: no `hdiutil`, or an image that cannot be made.
/// # Panics
/// When the stage passes, or leaves anything behind.
pub fn a_real_image_without_aterm_app_is_refused_and_detached(dir: &Path) -> std::io::Result<()> {
    prerequisites()?;
    let _detach = DetachAll(dir.to_path_buf());
    let staging = staging(dir)?;
    let src = dir.join("release");
    release_tree(&src, false)?;
    let dmg = dir.join(format!("aterm-{VERSION}.dmg"));
    create_image(&src, &dmg)?;

    let why = stage_from_dmg(&staging, &dmg, &manifest(), "", &now_rfc3339())
        .expect_err("an image with no aterm.app stages nothing");
    assert!(why.contains("not found on mounted DMG"), "{why}");
    assert!(!staging.ready.exists(), "no ready marker");
    assert!(!staging.staged_app.exists(), "no staged bundle");
    assert_eq!(
        mount_points(&staging),
        Vec::<String>::new(),
        "no mount point left"
    );
    let attached = attached_under(dir)?;
    assert!(
        attached.is_empty(),
        "detached on the error path: {attached:?}"
    );
    Ok(())
}

const RELEASE_HOST: &str = r#"#!/bin/sh
head=0 follow=0 fmt= url=
while [ $# -gt 0 ]; do
	case "$1" in
	-I) head=1 ;;
	-fsSIL) head=1; follow=1 ;;
	-fsSL) follow=1 ;;
	-w) fmt="$2"; shift ;;
	--proto | --proto-redir | --connect-timeout | --max-time | --max-filesize | --retry | -o) shift ;;
	-*) ;;
	*) url="$1" ;;
	esac
	shift
done
base="https://github.com/example/aterm/releases/download/v${STUB_VERSION:?}"
case "$url" in
"$base"/*) file="${STUB_DIR:?}/${url#"$base"/}" ;;
*) echo "curl stub: unexpected request: $url" >&2; exit 64 ;;
esac
case "$head$follow" in
10) if [ -f "$file" ]; then code=302; else code=404; fi
	printf '%s' "$fmt" | sed "s#%{http_code}#$code#" ;;
11) [ -f "$file" ] || exit 22
	printf 'HTTP/2 302\r\ncontent-length: 0\r\n\r\nHTTP/2 200\r\ncontent-length: %s\r\n\r\n' "$(wc -c <"$file" | tr -d ' ')" ;;
01) [ -f "$file" ] || exit 22
	cat "$file" ;;
*) echo "curl stub: unexpected request shape $head$follow: $url" >&2; exit 64 ;;
esac
"#;

fn install_sh(dir: &Path, script: &str, image_has_app: bool) -> std::io::Result<(i32, String)> {
    use std::os::unix::fs::PermissionsExt as _;
    let host = dir.join("host");
    let src = dir.join("release");
    release_tree(&src, image_has_app)?;
    std::fs::create_dir_all(&host)?;
    let dmg = host.join(format!("aterm-{VERSION}.dmg"));
    create_image(&src, &dmg)?;
    let sha = aterm_update_core::sha256_file(&dmg).map_err(std::io::Error::other)?;
    std::fs::write(
        host.join("aterm-appcast.toml"),
        format!("version = \"{VERSION}\"\ndmg = \"aterm-{VERSION}.dmg\"\nsha256 = \"{sha}\"\n"),
    )?;
    let web = dir.join("web");
    std::fs::create_dir_all(&web)?;
    let curl = web.join("curl");
    std::fs::write(&curl, RELEASE_HOST)?;
    std::fs::set_permissions(&curl, std::fs::Permissions::from_mode(0o755))?;
    let installer = dir.join("install.sh");
    std::fs::write(&installer, script)?;
    for sub in ["home", "tmp", "Applications"] {
        std::fs::create_dir_all(dir.join(sub))?;
    }
    let out = aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command("/bin/bash")
            .arg(&installer)
            .args([
                "--version",
                &format!("v{VERSION}"),
                "--no-cli",
                "--no-toolchain",
                "--no-path",
            ])
            .env_clear()
            .env("HOME", dir.join("home"))
            .env("TMPDIR", dir.join("tmp"))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", web.display()),
            )
            .env("ATERM_REPO_SLUG", "example/aterm")
            .env("ATERM_INSTALL_DIR", dir.join("Applications"))
            .env("STUB_DIR", &host)
            .env("STUB_VERSION", VERSION),
        std::time::Instant::now() + HANG,
    )
    .map_err(|e| std::io::Error::other(format!("install.sh: {e}")))?;
    Ok((
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    ))
}

fn left_in(tmp: &Path) -> Vec<String> {
    std::fs::read_dir(tmp)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// INSTALL.SH INSTALLS A RELEASE THAT SHIPS NO ZIP FROM ITS REAL IMAGE.
/// # Errors
/// A setup failure: no `hdiutil`, an image that cannot be made, or a run that cannot
/// start or did not end within the hang detector.
/// # Panics
/// When the install fails, or leaves its image attached or its scratch behind.
pub fn install_sh_installs_from_a_real_image(dir: &Path, script: &str) -> std::io::Result<()> {
    prerequisites()?;
    let _detach = DetachAll(dir.to_path_buf());
    let (code, said) = install_sh(dir, script, true)?;
    assert_eq!(code, 0, "{said}");
    assert!(
        said.contains(&format!("downloading aterm-{VERSION}.dmg")),
        "the image path ran: {said}"
    );
    assert!(
        said.contains(&format!("installed aterm {VERSION}")),
        "{said}"
    );
    assert!(
        dir.join("Applications/aterm.app/Contents/MacOS/aterm")
            .is_file(),
        "the image's app is placed: {said}"
    );
    let attached = attached_under(dir)?;
    assert!(attached.is_empty(), "the image detached: {attached:?}");
    let left = left_in(&dir.join("tmp"));
    assert!(left.is_empty(), "its scratch removed: {left:?}");
    Ok(())
}

/// INSTALL.SH REFUSES AN IMAGE WITH NO `aterm.app` AND DETACHES IT.
/// # Errors
/// As [`install_sh_installs_from_a_real_image`].
/// # Panics
/// When the run passes, places a bundle, or leaves its image attached or its scratch.
pub fn install_sh_refuses_an_image_with_no_app_and_detaches(
    dir: &Path,
    script: &str,
) -> std::io::Result<()> {
    prerequisites()?;
    let _detach = DetachAll(dir.to_path_buf());
    let (code, said) = install_sh(dir, script, false)?;
    assert_eq!(code, 1, "{said}");
    assert!(
        said.contains(&format!("no aterm.app inside aterm-{VERSION}.dmg")),
        "{said}"
    );
    assert!(
        !dir.join("Applications/aterm.app").exists(),
        "nothing placed"
    );
    let attached = attached_under(dir)?;
    assert!(
        attached.is_empty(),
        "the exit trap detached it: {attached:?}"
    );
    let left = left_in(&dir.join("tmp"));
    assert!(left.is_empty(), "its scratch removed: {left:?}");
    Ok(())
}
