// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::items::apps::tests::{SPCTL_KYA_ON, codesign_key, dir, err, ok, sha_of};
use crate::port::Fake;

/// The head of the live v0.104.0 appcast (2026-10-16), verbatim up to its changelog.
const APPCAST: &str = r#"schema = 1
version = "0.104.0"
build_number = 1792088595
commit = "74147aac3d1f292fdae5a900e63fab9f9ffea994"
dmg = "aterm-0.104.0.dmg"
sha256 = "da54bb61b14170781a85a9d12e942b27b958c63a1b6afcd6bd65074a2722b84e"
url = "https://github.com/alabsystems/aterm/releases/download/v0.104.0/aterm-0.104.0.dmg"
zip = "aterm-0.104.0-mac.zip"
zip_sha256 = "457cee7e20d0d406fb6ce43275593d0f553eb0fccd46fcc812dfb8d02c0b427f"
min_os = "11.0"
team_id = "A66A9P66Z7"
pub_date = "2026-10-15T18:52:14Z"
min_build = 1786131079
machine_id = "m3"
roster_seq = 4
changelog = '''
### Added

- **Iosevka Term ships with aterm: `font_family = "Iosevka Term"` selects it on macOS, Linux
  and Windows.**
'''
"#;

const ZIP_SHA: &str = "457cee7e20d0d406fb6ce43275593d0f553eb0fccd46fcc812dfb8d02c0b427f";

fn paths() -> Paths {
    Paths {
        root: PathBuf::from("/m"),
    }
}

fn part(label: &str, sequence: u64) -> String {
    Fake::scratch_path(Some(Path::new("/m/cache")), label, sequence)
        .join("payload")
        .to_string_lossy()
        .into_owned()
}

fn plistbuddy(bundle: &str, key: &str) -> String {
    format!("/usr/libexec/PlistBuddy -c 'Print :{key}' {bundle}/Contents/Info.plist")
}

#[test]
fn every_pinned_artifact_is_needed_once() {
    let b = crate::baseline::parse(&format!(
        r#"schema = 1
[[app]]
name = "Caffeine"
kind = "dmg-app"
url = "https://github.com/IntelliScape/caffeine/releases/download/1.1.4/Caffeine.dmg"
sha256 = "1ad34c3299a0c866873c4feb432c3e11186375adb6f43c5f481ae9d46d06d723"
team_id = "YD6LEYT6WZ"
bundle = "Caffeine.app"
version = "1.1.4"

[[app]]
name = "Caffeine-again"
kind = "dmg-app"
url = "https://github.com/IntelliScape/caffeine/releases/download/1.1.4/Caffeine.dmg"
sha256 = "1AD34C3299A0C866873C4FEB432C3E11186375ADB6F43C5F481AE9D46D06D723"
team_id = "YD6LEYT6WZ"
bundle = "Caffeine.app"
version = "1.1.4"

[homebrew]
pkg = {{ url = "https://github.com/Homebrew/brew/releases/download/7.0.9/Homebrew.pkg", sha256 = "e021295947a4a9b7ca1a8eff310c9ea1db14d923c5acf7d79e325b60a81f1a15", team_id = "927JGANW46" }}

[aterm]
version = "0.104.0"
archive_sha256 = "{ZIP_SHA}"
install_sh_sha256 = "dd92e2224c1c5a8e2c69992c4bafdf79142cf27f36a5bacfbfaa8efe12949b98"
"#
    ))
    .unwrap();
    let n = needed(&b);
    let names: Vec<&str> = n.iter().map(|x| x.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "app.Caffeine",
            "homebrew.pkg",
            "aterm 0.104.0",
            "aterm 0.104.0 install.sh"
        ]
    );
    assert_eq!(
        n[2].url,
        "https://github.com/alabsystems/aterm/releases/download/v0.104.0/aterm-0.104.0-mac.zip"
    );
    assert_eq!(
        n[3].url,
        "https://raw.githubusercontent.com/alabsystems/aterm/v0.104.0/tools/install.sh"
    );
}

#[test]
fn a_download_that_is_not_https_is_refused_before_anything_runs() {
    let sys = Fake::root();
    let item = Needed {
        name: "app.X".into(),
        url: "http://example.com/x.dmg".into(),
        sha256: "0".repeat(64),
    };
    assert!(
        ensure(&sys, &paths(), &item)
            .unwrap_err()
            .contains("not https://")
    );
    assert!(sys.calls().is_empty());
}

#[test]
fn a_download_is_cached_under_its_hash_only_when_it_hashes_to_the_pin() {
    let bytes = b"the dmg";
    let sha = sha_of(bytes);
    let url = "https://example.com/x.dmg";
    let item = Needed {
        name: "app.X".into(),
        url: url.into(),
        sha256: sha.clone(),
    };
    let sys = Fake::root();
    sys.on_download(url, Ok(bytes));
    let got = ensure(&sys, &paths(), &item).unwrap();
    assert_eq!(got, PathBuf::from(format!("/m/cache/{sha}")));
    assert!(
        sys.calls().contains(&format!(
            "download {url} -> {} (max {MAX_ARTIFACT_BYTES})",
            part("download", 0)
        )),
        "read into the cache's scratch through the port: {:?}",
        sys.calls()
    );
    assert_eq!(sys.written(&format!("/m/cache/{sha}")).unwrap(), bytes);
    let before = sys.calls().len();
    ensure(&sys, &paths(), &item).unwrap();
    assert_eq!(sys.calls().len(), before);
    let sys = Fake::root();
    sys.on_download(url, Ok(b"tampered"));
    let why = ensure(&sys, &paths(), &item).unwrap_err();
    assert!(why.contains("the baseline pins"), "{why}");
    assert!(sys.written(&format!("/m/cache/{sha}")).is_none());
    assert!(sys.written(&part("download", 0)).is_none());
}

#[test]
fn a_failed_download_says_why_and_caches_nothing() {
    let sha = "0".repeat(64);
    let url = "https://example.com/x.dmg";
    let sys = Fake::root();
    sys.on_download(url, Err("https://example.com/x.dmg: HTTP 404"));
    let why = ensure(
        &sys,
        &paths(),
        &Needed {
            name: "app.X".into(),
            url: url.into(),
            sha256: sha.clone(),
        },
    )
    .unwrap_err();
    assert_eq!(why, "app.X: https://example.com/x.dmg: HTTP 404");
    assert!(sys.written(&format!("/m/cache/{sha}")).is_none());
    assert!(sys.written(&part("download", 0)).is_none());
}

#[test]
fn the_appcast_is_admitted_the_way_install_sh_admits_it() {
    assert_eq!(
        parse_appcast(APPCAST, None).unwrap(),
        Release {
            version: "0.104.0".into(),
            zip_sha256: ZIP_SHA.into()
        }
    );
    assert!(parse_appcast(APPCAST, Some("0.104.0")).is_ok());
    assert!(
        parse_appcast(APPCAST, Some("0.103.0"))
            .unwrap_err()
            .contains("mismatch")
    );
    let no_team = APPCAST.replace("team_id = \"A66A9P66Z7\"\n", "");
    assert!(
        parse_appcast(&no_team, None)
            .unwrap_err()
            .contains("omits team_id")
    );
    let other = APPCAST.replace("A66A9P66Z7", "AAAAAAAAAA");
    assert!(
        parse_appcast(&other, None)
            .unwrap_err()
            .contains("names signing team")
    );
    let renamed = APPCAST.replace("zip = \"aterm-0.104.0-mac.zip\"", "zip = \"aterm.zip\"");
    assert!(
        parse_appcast(&renamed, None)
            .unwrap_err()
            .contains("not aterm-0.104.0-mac.zip")
    );
}

#[test]
fn a_filled_or_absent_aterm_table_is_left_alone_and_nothing_is_fetched() {
    let sys = Fake::root();
    let text = "schema = 1\n";
    assert_eq!(resolve_aterm(&sys, &paths(), text).unwrap(), text);
    let text = format!(
        "schema = 1\n[aterm]\nversion = \"0.104.0\"\narchive_sha256 = \"{ZIP_SHA}\"\ninstall_sh_sha256 = \"{ZIP_SHA}\"\n"
    );
    assert_eq!(resolve_aterm(&sys, &paths(), &text).unwrap(), text);
    assert!(sys.calls().is_empty());
}

/// Script the newest release (v0.104.0) on `sys`: its appcast, zip, the zip's checks and
/// install.sh.
fn scripted_release(sys: &Fake) -> (String, String, String) {
    let zip = b"the release zip";
    let zip_sha = sha_of(zip);
    let appcast = APPCAST.replace(ZIP_SHA, &zip_sha);
    sys.on_download(&appcast_url(None), Ok(appcast.as_bytes()));
    let zip_url = release_url("0.104.0", "aterm-0.104.0-mac.zip");
    sys.on_download(&zip_url, Ok(zip));
    let verify = Fake::scratch_path(Some(Path::new("/m/cache")), "verify", 1)
        .to_string_lossy()
        .into_owned();
    sys.on(
        &format!("/usr/bin/ditto -x -k --noacl --noqtn /m/cache/{zip_sha} {verify}"),
        ok(""),
    );
    let app = format!("{verify}/aterm.app");
    let unpack = format!("/usr/bin/ditto -x -k --noacl --noqtn /m/cache/{zip_sha} {verify}");
    sys.on_write(&unpack, Path::new(&app), b"");
    sys.on_stat(&unpack, Path::new(&app), dir());
    sys.on_write(
        &unpack,
        Path::new(&format!("{app}/Contents/Info.plist")),
        b"",
    );
    sys.on(&codesign_key(&app, TEAM_ID), ok(""));
    sys.on(
        &format!("/usr/sbin/spctl -a -vv -t exec {app}"),
        err(0, SPCTL_KYA_ON),
    );
    sys.on(
        &plistbuddy(&app, "CFBundleIdentifier"),
        ok("com.aterm.aterm\n"),
    );
    sys.on(
        &plistbuddy(&app, "CFBundleShortVersionString"),
        ok("0.104.0\n"),
    );
    let script = b"#!/usr/bin/env bash\nOFFICIAL_TEAM_ID=\"A66A9P66Z7\"\nmain \"$@\"\n";
    let script_sha = sha_of(script);
    sys.on_download(&install_sh_url("0.104.0"), Ok(script));
    (zip_sha, script_sha, verify)
}

#[test]
fn resolving_the_newest_release_checks_its_zip_and_pins_both_hashes_keeping_comments() {
    let sys = Fake::root();
    let (zip_sha, script_sha, verify) = scripted_release(&sys);
    let text = "schema = 1\n\n# The lab runs the newest release.\n[aterm]\nowner = \"admin\" # owns /Applications/aterm.app\ntoolset = true\n";
    let out = resolve_aterm(&sys, &paths(), text).unwrap();
    let calls = sys.calls();
    for read in [
        format!("download {} (max {MAX_APPCAST_BYTES})", appcast_url(None)),
        format!(
            "download {} -> {} (max {MAX_ARTIFACT_BYTES})",
            release_url("0.104.0", "aterm-0.104.0-mac.zip"),
            part("download", 0)
        ),
        format!(
            "download {} -> {} (max {MAX_SCRIPT_BYTES})",
            install_sh_url("0.104.0"),
            part("install-script", 2)
        ),
    ] {
        assert!(calls.contains(&read), "{read} in {calls:?}");
    }
    assert!(out.starts_with("schema = 1\n\n# The lab runs the newest release.\n[aterm]\nowner = \"admin\" # owns /Applications/aterm.app\ntoolset = true\n"), "{out}");
    let b = crate::baseline::parse(&out).unwrap().aterm.unwrap();
    assert_eq!(b.version.as_deref(), Some("0.104.0"));
    assert_eq!(b.archive_sha256.as_deref(), Some(zip_sha.as_str()));
    assert_eq!(b.install_sh_sha256.as_deref(), Some(script_sha.as_str()));
    assert_eq!(
        sys.written(&format!("/m/cache/{script_sha}")).unwrap(),
        b"#!/usr/bin/env bash\nOFFICIAL_TEAM_ID=\"A66A9P66Z7\"\nmain \"$@\"\n"
    );
    assert!(sys.stat(Path::new(&verify)).is_err());
    assert!(
        sys.written(&format!("{verify}/aterm.app/Contents/Info.plist"))
            .is_none()
    );
}

#[test]
fn one_command_resolves_aterm_once_and_every_host_gets_the_same_release() {
    let sys = Fake::root();
    let (zip_sha, script_sha, _) = scripted_release(&sys);
    let mut memo = AtermMemo::default();
    let lab_07 = "schema = 1\nusers = [\"lab07\"]\n[aterm]\nowner = \"admin\"\n";
    let lab_08 = "schema = 1\nusers = [\"lab08\"]\n[aterm]\nowner = \"admin\"\n";
    resolve_aterm_once(&sys, &paths(), lab_07, &mut memo).unwrap();
    let fetched = sys.calls().len();
    let second = resolve_aterm_once(&sys, &paths(), lab_08, &mut memo).unwrap();
    assert_eq!(
        sys.calls().len(),
        fetched,
        "nothing fetched or checked again"
    );
    let b = crate::baseline::parse(&second).unwrap();
    assert_eq!(b.users, ["lab08"]);
    let at = b.aterm.unwrap();
    assert_eq!(at.version.as_deref(), Some("0.104.0"));
    assert_eq!(at.archive_sha256.as_deref(), Some(zip_sha.as_str()));
    assert_eq!(at.install_sh_sha256.as_deref(), Some(script_sha.as_str()));
}

#[test]
fn an_appcast_that_is_not_utf8_is_refused() {
    let url = appcast_url(Some("0.104.0"));
    let sys = Fake::root();
    let mut body = APPCAST.as_bytes().to_vec();
    body.extend_from_slice(b"# \xff\n");
    sys.on_download(&url, Ok(&body));
    let why = fetch_text(&sys, &url, MAX_APPCAST_BYTES).unwrap_err();
    assert!(why.contains("is not UTF-8 text"), "{why}");
    // Negative control: the same appcast without the stray byte is read whole.
    let sys = Fake::root();
    sys.on_download(&url, Ok(APPCAST.as_bytes()));
    assert_eq!(fetch_text(&sys, &url, MAX_APPCAST_BYTES).unwrap(), APPCAST);
}

#[test]
fn an_archive_pin_the_release_contradicts_is_refused() {
    let sys = Fake::root();
    sys.on_download(&appcast_url(Some("0.104.0")), Ok(APPCAST.as_bytes()));
    let text = format!(
        "schema = 1\n[aterm]\nversion = \"0.104.0\"\narchive_sha256 = \"{}\"\n",
        "a".repeat(64)
    );
    let why = resolve_aterm(&sys, &paths(), &text).unwrap_err();
    assert!(why.contains("refusing"), "{why}");
}

#[test]
fn an_install_sh_that_is_cut_off_or_unpinned_is_not_cached() {
    let sys = Fake::root();
    sys.on_download(
        &install_sh_url("0.104.0"),
        Ok(b"#!/usr/bin/env bash\nOFFICIAL_TEAM_ID=\"A66A9P66Z7\"\nmain() {\n"),
    );
    let why = fetch_install_sh(&sys, &paths(), "0.104.0").unwrap_err();
    assert!(why.contains("main \"$@\""), "{why}");
    assert!(sys.written(&part("install-script", 0)).is_none());
}

/// Fetch `script` as release 0.104.0's install.sh on a fresh host.
fn fetched(script: &[u8]) -> Result<String, String> {
    let sys = Fake::root();
    sys.on_download(&install_sh_url("0.104.0"), Ok(script));
    fetch_install_sh(&sys, &paths(), "0.104.0")
}

#[test]
fn an_install_sh_ending_in_its_layout_s_guard_line_is_cached_and_one_cut_inside_it_is_not() {
    let head = "#!/usr/bin/env bash\nOFFICIAL_TEAM_ID=\"A66A9P66Z7\"\n";
    let grouped = format!("{head}{{\nmain() {{ :; }}\nmain \"$@\"; }}\n");
    let bare = format!("{head}main() {{ :; }}\nmain \"$@\"\n");
    for script in [&grouped, &bare] {
        assert_eq!(
            fetched(script.as_bytes()),
            Ok(sha_of(script.as_bytes())),
            "{script}"
        );
    }
    // Cut four bytes short, the grouped file ends in the bare line; cut one
    // short, either ends in its guard line without the newline.
    for (script, guard, cut) in [
        (&grouped, "main \"$@\"; }", 4),
        (&grouped, "main \"$@\"; }", 3),
        (&grouped, "main \"$@\"; }", 1),
        (&bare, "main \"$@\"", 1),
    ] {
        let short = &script[..script.len() - cut];
        let why = fetched(short.as_bytes()).unwrap_err();
        assert!(why.contains(&format!("`{guard}` line")), "{short}: {why}");
    }
}
