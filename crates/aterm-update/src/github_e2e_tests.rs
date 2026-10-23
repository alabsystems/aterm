// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! End-to-end tests for the app-container background update check over real `/usr/bin/curl`.

use super::*;

const PROBE_DIR: &str = "ATERM_TEST_CHECK_E2E_DIR";
const PROBE: &str = "github::tests::e2e::probe_one_check_over_real_curl";

const RUNNING: u64 = 10;
const BUILD: u64 = 20;
const VERSION: &str = "0.20.0";
const TAG: &str = "v0.20.0";
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

const HOLD_AT: usize = 64 * 1024;

const HANG: std::time::Duration = std::time::Duration::from_secs(60);

const QUICK_STALL_SECS: &str = "8";

const FRESH_STALL_SECS: u32 = 2;

fn e2e_staging(dir: &std::path::Path) -> Staging {
    let root = dir.join("Updates");
    aterm_update_core::ensure_private_dir(&root.join("download")).expect("scratch updates root");
    Staging {
        apply_lock: root.join("apply.lock"),
        stage_lock: root.join("stage.lock"),
        download: root.join("download"),
        staged_app: root.join("staged").join("aterm.app"),
        ready: root.join("ready.toml"),
        status: root.join("status.toml"),
        root,
    }
}

#[test]
fn probe_one_check_over_real_curl() {
    let Some(dir) = std::env::var_os(PROBE_DIR) else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let installed_parent = dir.join("Applications");
    let master = pub_b64(&MASTER_SEED_FIXTURE);
    let masters = [master.as_str()];
    let mut head = aterm_update_core::head_no_redirect;
    let world = CheckWorld {
        installed_parent: Some(&installed_parent),
        staging: e2e_staging(&dir),
        head: &mut head,
        master_pubkeys: &masters,
        team: "",
    };
    let said = format!(
        "{:?}",
        check_and_stage_in(world, RUNNING, &test_source(), Asked::Schedule)
    );
    let pending = dir.join("said.pending");
    std::fs::write(&pending, said).unwrap();
    std::fs::rename(pending, dir.join("said")).unwrap();
    std::thread::sleep(HANG);
}

fn stand_in_app(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let app = dir.join("aterm.app");
    let exe = app.join("Contents/MacOS/aterm");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::create_dir_all(app.join("Contents/Resources")).unwrap();
    let mut x = 0x9E37_79B9_7F4A_7C15_u64;
    let noise: Vec<u8> = (0..1 << 20)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x.to_le_bytes()[0]
        })
        .collect();
    std::fs::write(app.join("Contents/Resources/payload.bin"), noise).unwrap();
    std::fs::write(
        &exe,
        format!("#!/bin/sh\necho 'aterm {VERSION} (build: {BUILD})'\n"),
    )
    .unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        app.join("Contents/Info.plist"),
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
             <key>CFBundleExecutable</key><string>aterm</string>\
             <key>CFBundleIdentifier</key><string>com.aterm.fixture</string>\
             <key>CFBundleShortVersionString</key><string>{VERSION}</string>\
             <key>CFBundleVersion</key><string>{BUILD}</string>\
             <key>ATermGitCommit</key><string>{COMMIT}</string></dict></plist>\n"
        ),
    )
    .unwrap();
    aterm_uds::deadline::require_trust_service();
    aterm_uds::deadline::sign_ad_hoc(&app);
    app
}

fn updater_zip(app: &std::path::Path, zip: &std::path::Path) -> Vec<u8> {
    let out = aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command("/usr/bin/ditto")
            .args([
                "-c",
                "-k",
                "--norsrc",
                "--noextattr",
                "--noacl",
                "--noqtn",
                "--keepParent",
            ])
            .arg(app)
            .arg(zip),
        std::time::Instant::now() + HANG,
    )
    .expect("ditto runs");
    assert!(out.status.success(), "ditto: {out:?}");
    std::fs::read(zip).unwrap()
}

fn release_assets(zip: &[u8]) -> Vec<(String, Vec<u8>)> {
    use ring::signature::Ed25519KeyPair;
    let master = Ed25519KeyPair::from_seed_unchecked(&MASTER_SEED_FIXTURE).unwrap();
    let m3 = Ed25519KeyPair::from_seed_unchecked(&M3_SEED_FIXTURE).unwrap();
    let roster = aterm_update_core::roster::Roster {
        schema: 1,
        roster_seq: 4,
        valid_until: "9999-12-31T00:00:00Z".into(),
        machines: vec![aterm_update_core::roster::Machine {
            id: "m3".into(),
            pubkey: pub_b64(&M3_SEED_FIXTURE),
            added_at: "2026-08-04T00:00:00Z".into(),
            not_after: None,
        }],
        revoked: vec![],
    }
    .to_toml()
    .unwrap()
    .into_bytes();
    let dmg = format!("aterm-{VERSION}.dmg");
    let zip_name = format!("aterm-{VERSION}-mac.zip");
    let appcast = format!(
        "schema = 1\nversion = \"{VERSION}\"\nbuild_number = {BUILD}\ncommit = \"{COMMIT}\"\n\
         sha256 = \"{}\"\ndmg = \"{dmg}\"\nurl = \"{}\"\nzip = \"{zip_name}\"\n\
         zip_sha256 = \"{}\"\nmachine_id = \"m3\"\nroster_seq = 4\n",
        "ab".repeat(32),
        tag_url(TAG, &dmg),
        aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(zip)),
    )
    .into_bytes();
    vec![
        (
            APPCAST_SIG_ASSET.to_string(),
            m3.sign(&appcast).as_ref().to_vec(),
        ),
        (APPCAST_ASSET.to_string(), appcast),
        (
            aterm_update_core::roster::ROSTER_SIG_ASSET.to_string(),
            master.sign(&roster).as_ref().to_vec(),
        ),
        (aterm_update_core::roster::ROSTER_ASSET.to_string(), roster),
        (zip_name, zip.to_vec()),
    ]
}

type Seen = std::sync::Arc<std::sync::Mutex<Vec<(String, String, String)>>>;

fn serve(listener: std::net::TcpListener, assets: Vec<(String, Vec<u8>)>, seen: Seen) {
    use std::io::{Read as _, Write as _};
    let assets = std::sync::Arc::new(assets);
    let held = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { return };
            let (assets, seen, held) = (assets.clone(), seen.clone(), held.clone());
            std::thread::spawn(move || {
                let mut head = Vec::new();
                let mut buf = [0u8; 4096];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match conn.read(&mut buf) {
                        Ok(0) | Err(_) => return,
                        Ok(k) => head.extend_from_slice(&buf[..k]),
                    }
                }
                let head = String::from_utf8_lossy(&head).into_owned();
                let mut words = head.split_whitespace();
                let method = words.next().unwrap_or_default().to_string();
                let path = words.next().unwrap_or_default().to_string();
                let range = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("range: ")
                            .map(str::to_string)
                    })
                    .unwrap_or_else(|| "none".into());
                seen.lock()
                    .unwrap()
                    .push((method.clone(), path.clone(), range.clone()));
                let evergreen = "/alabsystems/aterm/releases/latest/download/aterm-appcast.toml";
                if path == evergreen {
                    let to = tag_url(TAG, APPCAST_ASSET);
                    let _ = conn.write_all(
                        format!(
                            "HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\n\
                             Connection: close\r\n\r\n"
                        )
                        .as_bytes(),
                    );
                    return;
                }
                let prefix = format!("/alabsystems/aterm/releases/download/{TAG}/");
                let Some(body) = path
                    .strip_prefix(&prefix)
                    .and_then(|name| assets.iter().find(|(n, _)| n == name))
                    .map(|(_, body)| body)
                else {
                    let _ = conn.write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                    return;
                };
                let size = body.len();
                let from = range
                    .strip_prefix("bytes=")
                    .and_then(|r| r.trim_end_matches('-').parse::<usize>().ok())
                    .unwrap_or(0)
                    .min(size);
                let reply = if from == 0 {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {size}\r\nConnection: close\r\n\r\n"
                    )
                } else {
                    format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\n\
                         Content-Range: bytes {from}-{}/{size}\r\nConnection: close\r\n\r\n",
                        size - from,
                        size - 1
                    )
                };
                let _ = conn.write_all(reply.as_bytes());
                if method != "GET" {
                    return;
                }
                let container = path.ends_with("-mac.zip");
                if container && from == 0 && !held.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    let _ = conn.write_all(&body[..HOLD_AT.min(size)]);
                    while matches!(conn.read(&mut buf), Ok(k) if k > 0) {}
                    return;
                }
                let _ = conn.write_all(&body[from..]);
            });
        }
    });
}

#[test]
fn a_check_over_real_curl_stages_a_release_whose_container_transfer_goes_silent() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let release = dir.path().join("release");
    std::fs::create_dir_all(&release).unwrap();
    let app = stand_in_app(&release);
    let zip = updater_zip(&app, &release.join("updater.zip"));
    assert!(zip.len() > HOLD_AT, "the hold cuts the container short");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen: Seen = std::sync::Arc::default();
    serve(listener, release_assets(&zip), seen.clone());

    let world = dir.path().join("world");
    std::fs::create_dir_all(world.join("Applications")).unwrap();
    let quick = QUICK_STALL_SECS;
    let wrapper = dir.path().join("curl");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nunset http_proxy HTTP_PROXY https_proxy HTTPS_PROXY all_proxy ALL_PROXY\n\
             fresh=1\nfor a do [ \"$a\" = --continue-at ] && fresh=0; done\n\
             n=$#; i=0; prev=\nfor a do\n  i=$((i+1))\n  [ $i -eq 1 ] && set --\n  \
             if [ $i -eq $n ]; then set -- \"$@\" \"$(printf '%s' \"$a\" | \
             sed 's#^https://github.com/#http://127.0.0.1:{port}/#')\"; \
             elif [ $fresh = 1 ] && [ \"$prev\" = --speed-time ] && [ \"$a\" = {quick} ]; \
             then set -- \"$@\" {FRESH_STALL_SECS}; \
             else set -- \"$@\" \"$a\"; fi\n  prev=$a\ndone\nexec /usr/bin/curl \"$@\"\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap())
        .args(["--exact", "--nocapture", PROBE])
        .process_group(0)
        .env(PROBE_DIR, &world)
        .env(aterm_update_core::CURL_SEAM, &wrapper)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .unwrap();
    let start = std::time::Instant::now();
    let said = loop {
        if let Ok(said) = std::fs::read_to_string(world.join("said")) {
            break said;
        }
        if start.elapsed() > HANG || !matches!(child.try_wait(), Ok(None)) {
            break String::new();
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let pid = i32::try_from(child.id()).unwrap();
    // SAFETY: process_group(0) made this unreaped child its group's leader.
    unsafe { libc::kill(-pid, libc::SIGKILL) };
    let _ = child.wait();
    let seen = seen.lock().unwrap().clone();
    assert_eq!(
        said,
        format!("Ok(Some({VERSION:?}))"),
        "the check stages the release within the hang detector; the host saw {seen:?}"
    );

    let staging = e2e_staging(&world);
    let ready = Ready::read_publishable(&staging).expect("the stage published its ready marker");
    assert_eq!(ready.build_number, BUILD);
    let plist = std::fs::read_to_string(staging.staged_app.join("Contents/Info.plist")).unwrap();
    assert!(
        plist.contains(&format!("<string>{BUILD}</string>")),
        "the staged bundle is the release's: {plist}"
    );
    let status = std::fs::read_to_string(&staging.status).unwrap();
    assert!(
        status.contains(&format!("staged {VERSION} (build {BUILD})")),
        "the ledger says what was staged: {status}"
    );

    let evergreen = "/alabsystems/aterm/releases/latest/download/aterm-appcast.toml";
    assert_eq!(seen[0].0, "HEAD", "{seen:?}");
    assert_eq!(seen[0].1, evergreen, "{seen:?}");
    assert_eq!(
        seen.iter().filter(|(_, path, _)| path == evergreen).count(),
        1,
        "{seen:?}"
    );
    let prefix = format!("/alabsystems/aterm/releases/download/{TAG}/");
    assert!(
        seen[1..]
            .iter()
            .all(|(_, path, _)| path.starts_with(&prefix)),
        "{seen:?}"
    );
    let container: Vec<&str> = seen
        .iter()
        .filter(|(method, path, _)| method == "GET" && path.ends_with("-mac.zip"))
        .map(|(_, _, range)| range.as_str())
        .collect();
    assert_eq!(
        container,
        ["none".to_string(), format!("bytes={HOLD_AT}-")],
        "{seen:?}"
    );
}
