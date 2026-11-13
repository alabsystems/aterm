// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::testkit::row;
use super::*;

fn exposes() -> Vec<String> {
    vec!["claude".to_string()]
}

fn refused(a: &Artifact, exposes: &[String]) -> String {
    match check_row(a, exposes) {
        Err(FlowError::VendorRefused(m)) => m,
        other => panic!("expected VendorRefused, got {other:?}"),
    }
}

#[test]
fn a_github_release_row_is_admitted_on_its_asset_and_root_alone() {
    let mut a = row();
    a.protocol = "github-release".into();
    a.url = String::new();
    a.payload = String::new();
    a.entry = String::new();
    a.size = 0;
    a.sha256 = "d".into();
    check_row(&a, &[]).expect("the release lane has no vendor admission");
    a.url = "http://evil.example/x".into();
    check_row(&a, &[]).expect("release rows ignore url");

    a.tree_root = String::new();
    assert!(
        refused(&a, &[]).contains("tree_root is required for a release bundle"),
        "a bundle that signs no root is refused"
    );
    a.payload = "tar-zst".into();
    check_row(&a, &[]).expect("only the bundle lane must sign a root");
}

#[test]
fn elf_binary_rows_are_confined_to_the_published_gnu_targets() {
    let mut artifact = row();
    artifact.protocol = "github-release".into();
    artifact.kind = "elf-binary".into();
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        artifact.target = target.into();
        assert!(check_row(&artifact, &[]).is_ok());
    }
    for target in ["aarch64-apple-darwin", "x86_64-unknown-linux-musl", ""] {
        artifact.target = target.into();
        assert!(refused(&artifact, &[]).contains("GNU Linux target"));
    }
}

#[test]
fn a_github_release_row_with_a_path_asset_is_refused() {
    let mut a = row();
    a.protocol = "github-release".into();
    a.sha256 = "d".into();
    for bad in [
        "/Users//victim/.ssh/authorized_keys",
        "../../../x",
        "a/b",
        "a\\b",
        "..",
        ".",
        "x\ny",
    ] {
        a.asset = bad.into();
        assert!(
            refused(&a, &[]).contains("asset must be a bare local file name"),
            "{bad:?}"
        );
    }
    a.asset = "ty-2973.tar.zst".into();
    check_row(&a, &[]).expect("a bare release asset name is admitted");
}

#[test]
fn an_unknown_protocol_is_refused_by_name() {
    let mut a = row();
    a.protocol = "ftp".into();
    assert!(refused(&a, &exposes()).contains("protocol must be"));
    a.protocol = String::new();
    assert!(refused(&a, &exposes()).contains("protocol must be"));
    a.protocol = "HTTPS".into();
    assert!(refused(&a, &exposes()).contains("protocol must be"));
    for deleted in ["pkg", "system-pm", "softwareupdate"] {
        a.protocol = deleted.into();
        let m = refused(&a, &exposes());
        assert!(m.contains("protocol must be github-release | https"), "{m}");
        assert!(m.ends_with(deleted), "{m}");
    }
}

#[test]
fn a_well_formed_archive_row_with_links_is_admitted() {
    let mut a = row();
    a.url = "https://github.com/cli/cli/releases/download/v2.80.0/gh_2.80.0_macOS_arm64.zip".into();
    a.payload = "zip".into();
    a.entry = String::new();
    a.strip_components = 1;
    a.links.insert("gh".into(), "share/gh/bin/gh".into());
    check_row(&a, &["gh".to_string()]).expect("archive row admits");
}

#[test]
fn a_well_formed_dmg_row_is_refused_for_its_lane_alone() {
    let mut d = row();
    d.kind = "app-bundle".into();
    d.url = "https://github.com/example/app/releases/download/v1/App-1.0-universal.dmg".into();
    d.payload = "dmg".into();
    d.entry = String::new();
    d.links
        .insert("emacs".into(), "Emacs.app/Contents/MacOS/Emacs".into());
    d.links.insert(
        "emacsclient".into(),
        "Emacs.app/Contents/MacOS/bin/emacsclient".into(),
    );
    let m = refused(&d, &["emacs".to_string(), "emacsclient".to_string()]);
    assert!(m.starts_with("payload = dmg is refused"), "{m}");
    assert!(m.contains("removable-volume dialog"), "{m}");
    let mut r = row();
    r.protocol = "github-release".into();
    r.sha256 = "d".into();
    r.payload = "dmg".into();
    let m = refused(&r, &[]);
    assert!(m.starts_with("payload = dmg is refused"), "{m}");
    r.payload = String::new();
    check_row(&r, &[]).expect("the same release row on the bundle lane");
}

#[test]
fn https_kind_and_payload_must_agree() {
    let mut app_as_zip = row();
    app_as_zip.kind = "app-bundle".into();
    app_as_zip.payload = "zip".into();
    app_as_zip.entry = String::new();
    assert!(refused(&app_as_zip, &exposes()).contains("needs payload = dmg"));
    let mut bin_as_dmg = row();
    bin_as_dmg.payload = "dmg".into();
    bin_as_dmg.entry = String::new();
    assert!(refused(&bin_as_dmg, &exposes()).contains("app-bundle lane"));
    for other in [
        "sysroot-bundle",
        "cargo-src",
        "installer-pkg",
        "system-package",
        "",
        "vendor-fetch",
    ] {
        let mut a = row();
        a.kind = other.into();
        assert!(
            refused(&a, &exposes()).contains("kind must be binary | app-bundle"),
            "{other:?}"
        );
    }
    let mut old = row();
    old.kind = "app-bundle".into();
    old.payload = "dmg-app".into();
    old.entry = String::new();
    assert!(refused(&old, &exposes()).contains("payload must be"));
}

#[test]
fn refuses_non_https_and_hostile_urls() {
    for bad in [
        "http://downloads.claude.ai/x",
        "ftp://downloads.claude.ai/x",
        "file:///etc/passwd",
        "downloads.claude.ai/x",
        "",
        "https://",
        "https:///x",
        "https://downloads.claude.ai:443/x",
        "https://user@downloads.claude.ai/x",
        "https://user:pw@downloads.claude.ai/x",
        "https://[::1]/x",
        "https://downloads.claude.ai/x y",
        "https://downloads.claude.ai/x\n",
        "https://downloads.claude.ai/x\u{7f}",
    ] {
        let mut a = row();
        a.url = bad.into();
        let m = refused(&a, &exposes());
        assert!(m.contains("url must be https"), "{bad:?}: {m}");
    }
}

#[test]
fn refuses_a_host_outside_the_allow_list_exactly() {
    for bad in [
        "https://evil.example/x",
        "https://downloads.claude.ai.evil.example/x",
        "https://evil.downloads.claude.ai/x",
        "https://DOWNLOADS.CLAUDE.AI/x",
        "https://api.github.com/x",
        "https://raw.githubusercontent.com/x",
    ] {
        let mut a = row();
        a.url = bad.into();
        let m = refused(&a, &exposes());
        assert!(
            m.contains("not an allow-listed vendor host"),
            "{bad:?}: {m}"
        );
    }
    for host in VENDOR_HOSTS {
        for tail in ["", "/", "/a/b/c?x=1#f"] {
            let mut a = row();
            a.url = format!("https://{host}{tail}");
            check_row(&a, &exposes()).unwrap_or_else(|e| panic!("{}: {e}", a.url));
        }
    }
}

#[test]
fn asset_must_be_a_bare_local_file_name() {
    for bad in [
        "",
        ".",
        "..",
        "../claude",
        "dl/claude",
        "/tmp/claude",
        "dl\\claude",
        "claude\0",
        "a\nb",
        "claude\r",
        "claude\t",
        "claude\u{7f}",
    ] {
        let mut a = row();
        a.asset = bad.into();
        let m = refused(&a, &exposes());
        assert!(
            m.contains("asset must be a bare local file name"),
            "{bad:?}: {m}"
        );
    }
    let mut dotted = row();
    dotted.asset = "gh_2.80.0_macOS_arm64.zip".into();
    check_row(&dotted, &exposes()).expect("dots inside a name are fine");
}

#[test]
fn refuses_an_unknown_payload() {
    for bad in ["", "tar", "tar.gz", "raw", "TAR-GZ", "pkg", "dmg-app"] {
        let mut a = row();
        a.payload = bad.into();
        a.entry = String::new();
        let m = refused(&a, &exposes());
        assert!(m.contains("payload must be"), "{bad:?}: {m}");
    }
}

#[test]
fn raw_binary_needs_a_shimmable_exposed_entry() {
    let mut empty = row();
    empty.entry = String::new();
    assert!(refused(&empty, &exposes()).contains("non-empty entry"));
    let mut sensitive = row();
    sensitive.entry = "sudo".into();
    assert!(refused(&sensitive, &["sudo".to_string()]).contains("not an admissible tool name"));
    let mut sep = row();
    sep.entry = "bin/claude".into();
    assert!(refused(&sep, &exposes()).contains("not an admissible tool name"));
    let mut unexposed = row();
    unexposed.entry = "claude".into();
    assert!(refused(&unexposed, &["codex".to_string()]).contains("not in exposes"));
}

#[test]
fn a_tar_gz_binary_row_is_one_exposed_entry_and_nothing_else() {
    let exposes = ["agy".to_string(), "agy-alias".to_string()];
    let agy = || {
        let mut a = row();
        a.payload = "tar-gz-binary".into();
        a.entry = "agy".into();
        a
    };
    check_row(&agy(), &exposes).expect("one exposed entry admits");
    let mut a = agy();
    a.entry = String::new();
    assert!(refused(&a, &exposes).contains("non-empty entry is required by payload"));
    let mut a = agy();
    a.entry = "codex".into();
    assert!(refused(&a, &exposes).contains("entry is not in exposes"));
    let mut a = agy();
    a.kind = "app-bundle".into();
    assert!(refused(&a, &exposes).contains("needs payload = dmg"));
    let mut a = agy();
    a.strip_components = 1;
    assert!(refused(&a, &exposes).contains("strip_components applies to archive"));
    let mut a = agy();
    a.links.insert("agy-alias".into(), "bin/agy".into());
    assert!(refused(&a, &exposes).contains("links do not apply"));
    a.payload = "raw-binary".into();
    check_row(&a, &exposes).expect("a raw-binary may carry the same link");
}

#[test]
fn entry_is_refused_on_non_raw_payloads() {
    let mut a = row();
    a.payload = "tar-gz".into();
    a.entry = "claude".into();
    assert!(refused(&a, &exposes()).contains("only meaningful for raw-binary"));
}

#[test]
fn strip_components_only_on_archives() {
    let mut raw = row();
    raw.strip_components = 1;
    assert!(refused(&raw, &exposes()).contains("strip_components applies to archive"));
    let mut dmg = row();
    dmg.kind = "app-bundle".into();
    dmg.payload = "dmg".into();
    dmg.entry = String::new();
    dmg.strip_components = 2;
    assert!(refused(&dmg, &exposes()).contains("strip_components applies to archive"));
    for ok in ["tar-gz", "tar-zst", "zip"] {
        let mut a = row();
        a.payload = ok.into();
        a.entry = String::new();
        a.strip_components = 3;
        check_row(&a, &exposes()).unwrap_or_else(|e| panic!("{ok}: {e}"));
    }
}

#[test]
fn links_keys_must_be_exposed_shimmable_names() {
    let mut a = row();
    a.kind = "app-bundle".into();
    a.payload = "dmg".into();
    a.entry = String::new();
    a.links
        .insert("emacs".into(), "Emacs.app/Contents/MacOS/Emacs".into());
    assert!(refused(&a, &["claude".to_string()]).contains("links key is not in exposes"));
    let mut s = row();
    s.kind = "app-bundle".into();
    s.payload = "dmg".into();
    s.entry = String::new();
    s.links.insert("ssh".into(), "Emacs.app/ssh".into());
    assert!(refused(&s, &["ssh".to_string()]).contains("links key is not an admissible tool name"));
}

#[test]
fn links_targets_must_be_relative_and_dotdot_free() {
    for bad in [
        "/Applications/Emacs.app/Contents/MacOS/Emacs",
        "../outside",
        "Emacs.app/../../etc/passwd",
        "Emacs.app/./Contents",
        "",
        "Emacs.app/Contents/",
        "Emacs.app\\Contents\\MacOS\\Emacs",
        "a\0b",
    ] {
        let mut a = row();
        a.kind = "app-bundle".into();
        a.payload = "dmg".into();
        a.entry = String::new();
        a.links.insert("claude".into(), bad.into());
        let m = refused(&a, &exposes());
        assert!(m.contains("links target must be"), "{bad:?}: {m}");
    }
}

#[test]
fn signed_digests_must_be_present_and_well_formed() {
    let mut zero = row();
    zero.size = 0;
    assert!(refused(&zero, &exposes()).contains("size must be > 0"));
    for bad in [
        "",
        "deadbeef",
        &"z".repeat(64),
        &"a".repeat(63),
        &"a".repeat(65),
    ] {
        let mut a = row();
        a.sha256 = bad.to_string();
        assert!(
            refused(&a, &exposes()).contains("sha256 must be 64 hex"),
            "{bad:?}"
        );
    }
    let mut upper = row();
    upper.sha256 = "7B09F01C".repeat(8);
    check_row(&upper, &exposes()).expect("hex case is not a refusal");
    let mut root = row();
    root.tree_root = String::new();
    assert!(refused(&root, &exposes()).contains("tree_root is required"));
}

#[test]
fn windows_lookup_names_follow_pathext() {
    assert_eq!(
        windows_lookup_names("gh", ".COM;.EXE;.BAT;.CMD"),
        vec!["gh.COM", "gh.EXE", "gh.BAT", "gh.CMD"]
    );
    assert_eq!(
        windows_lookup_names("gh", ""),
        vec!["gh.COM", "gh.EXE", "gh.BAT", "gh.CMD"],
        "empty PATHEXT is the default"
    );
    assert_eq!(windows_lookup_names("gh.exe", ".COM;.EXE"), vec!["gh.exe"]);
    assert_eq!(
        windows_lookup_names("scoop.CMD", ".EXE;.cmd"),
        vec!["scoop.CMD"]
    );
    assert_eq!(
        windows_lookup_names("gh.tar", ".COM;.EXE"),
        vec!["gh.tar.COM", "gh.tar.EXE"],
        "an extension PATHEXT does not list is part of the name"
    );
    assert_eq!(
        windows_lookup_names("gh", ".EXE;;junk;.;.CMD"),
        vec!["gh.EXE", "gh.CMD"],
        "entries without a leading dot, and a bare dot, are ignored"
    );
    assert_eq!(
        windows_lookup_names("GNU.Emacs", ".EXE"),
        vec!["GNU.Emacs.EXE"]
    );
}

const VENDOR_DIRECT_URLS: &[(&str, &str)] = &[
    (
        "claude",
        "https://downloads.claude.ai/claude-code-releases/latest",
    ),
    (
        "claude",
        "https://downloads.claude.ai/claude-code-releases/2.1.280/manifest.json",
    ),
    (
        "claude",
        "https://downloads.claude.ai/claude-code-releases/2.1.280/manifest.json.sig",
    ),
    (
        "claude",
        "https://downloads.claude.ai/claude-code-releases/2.1.280/darwin-arm64/claude",
    ),
    (
        "claude",
        "https://downloads.claude.ai/claude-code-releases/2.1.280/win32-x64/claude.exe",
    ),
    ("codex", "https://releases.openai.com/codex/channels/latest"),
    (
        "codex",
        "https://releases.openai.com/codex/releases/0.156.0/argument-comment-lint",
    ),
    (
        "codex",
        "https://github.com/openai/codex/releases/download/rust-v0.156.0/codex-package_SHA256SUMS",
    ),
    (
        "codex",
        "https://github.com/openai/codex/releases/download/rust-v0.156.0/\
         codex-package-aarch64-apple-darwin.tar.gz",
    ),
    (
        "agy",
        "https://antigravity-cli-auto-updater-974169037036.us-central1.run.app/manifests/\
         darwin_arm64.json",
    ),
    (
        "agy",
        "https://storage.googleapis.com/antigravity-public/antigravity-cli/\
         1.3.6-5068262057312256/darwin-arm/manifest.json",
    ),
    (
        "agy",
        "https://storage.googleapis.com/antigravity-public/antigravity-cli/\
         1.3.6-5068262057312256/darwin-arm/cli_mac_arm64.tar.gz",
    ),
    ("muse", "https://api.meta.ai/muse-code/channels/muse-stable"),
    (
        "muse",
        "https://lookaside.facebook.com/lookaside/muse/download/?channel=muse\
         &version=1.4.9-R6297.1&file=manifest.json",
    ),
    (
        "muse",
        "https://lookaside.facebook.com/lookaside/muse/download/?channel=muse\
         &version=1.4.9-R6297.1&file=muse-aarch64-macos",
    ),
    ("kimi", "https://cdn.kimi.com/kimi-code/latest"),
    (
        "kimi",
        "https://cdn.kimi.com/kimi-code/binaries/2.3.1/manifest.json",
    ),
    (
        "kimi",
        "https://cdn.kimi.com/kimi-code/binaries/2.3.1/kimi-code-darwin-arm64",
    ),
    (
        "kimi",
        "https://cdn.kimi.com/kimi-code/binaries/2.3.1/kimi-code-win32-x64.exe",
    ),
    (
        "kimi",
        "https://github.com/MoonshotAI/kimi-code/releases/download/\
         @moonshot-ai/kimi-code@2.3.1/manifest.json",
    ),
    (
        "grok",
        "https://storage.googleapis.com/grok-build-public-artifacts/cli/stable",
    ),
    (
        "grok",
        "https://storage.googleapis.com/grok-build-public-artifacts/cli/\
         grok-1.0.70-linux-x86_64",
    ),
    (
        "grok",
        "https://storage.googleapis.com/grok-build-public-artifacts/cli/\
         grok-1.0.70-linux-x86_64.zst",
    ),
    (
        "grok",
        "https://registry.npmjs.org/@xai-official/grok-linux-x64/1.0.70",
    ),
    (
        "grok",
        "https://registry.npmjs.org/@xai-official/grok-linux-x64/-/grok-linux-x64-1.0.70.tgz",
    ),
    (
        "grok",
        "https://registry.npmjs.org/@xai-official/grok-darwin-arm64/-/\
         grok-darwin-arm64-1.0.70.tgz",
    ),
    (
        "grok",
        "https://registry.npmjs.org/@xai-official/grok-darwin-x64/1.0.70",
    ),
    (
        "grok",
        "https://registry.npmjs.org/@xai-official/grok-linux-arm64/1.0.70",
    ),
];

#[test]
fn the_vendor_direct_pins_admit_the_real_urls() {
    for (program, url) in VENDOR_DIRECT_URLS {
        assert!(vendor_direct_url_allowed(program, url), "{program} {url}");
    }
}

#[test]
fn a_vendor_direct_url_is_admitted_only_for_its_own_vendors_lane() {
    use crate::vendor_direct::VENDORS;
    for (program, url) in VENDOR_DIRECT_URLS {
        let own = crate::vendor_direct::spec(program).unwrap();
        for other in VENDORS.iter().filter(|s| s.program != *program) {
            assert_eq!(
                vendor_direct_url_allowed(other.program, url),
                other.head_url == own.head_url,
                "{url} for {}",
                other.program
            );
        }
        for nobody in ["ay", "", "CLAUDE", "CODEX-OLD", "codex-older"] {
            assert!(
                !vendor_direct_url_allowed(nobody, url),
                "{url} admitted for {nobody:?}"
            );
        }
    }
    for a in VENDORS {
        for b in VENDORS.iter().filter(|b| b.head_url == a.head_url) {
            let pins = |s: &crate::vendor_direct::VendorSpec| {
                (
                    s.vendor,
                    s.anchor,
                    s.apple_team,
                    s.url_prefixes,
                    s.url_query,
                    s.digest_docs,
                )
            };
            assert_eq!(pins(a), pins(b), "{} and {}", a.program, b.program);
        }
    }
    let mut shared = 0;
    for (program, url) in VENDOR_DIRECT_URLS {
        let admitted = vendor_direct_url_allowed("codex-old", url);
        assert_eq!(admitted, *program == "codex", "{program} {url}");
        shared += usize::from(admitted);
    }
    assert!(
        shared > 0,
        "PRECONDITION: codex-old reads codex's documents"
    );
}

#[test]
fn the_vendor_direct_pins_refuse_everything_else() {
    for bad in [
        "https://downloads.claude.ai.evil.com/claude-code-releases/latest",
        "https://evil.downloads.claude.ai/claude-code-releases/latest",
        "https://downloads.claude.ai.evil.com/",
        "https://releases.openai.com.evil.com/codex/channels/latest",
        "https://objects.githubusercontent.com/openai/codex/releases/download/x",
        "https://api.github.com/repos/openai/codex/releases/latest",
        "http://downloads.claude.ai/claude-code-releases/latest",
        "HTTPS://downloads.claude.ai/claude-code-releases/latest",
        "ftp://downloads.claude.ai/claude-code-releases/latest",
        "https://user@downloads.claude.ai/claude-code-releases/latest",
        "https://downloads.claude.ai@evil.com/claude-code-releases/latest",
        "https://downloads.claude.ai:443/claude-code-releases/latest",
        "https://DOWNLOADS.CLAUDE.AI/claude-code-releases/latest",
        "https://Downloads.claude.ai/claude-code-releases/latest",
        "https://downloads.claude.ai./claude-code-releases/latest",
        "https://github.com./openai/codex/releases/download/rust-v0.156.0/x",
        "https://downloads.claude.ai/other/latest",
        "https://downloads.claude.ai/claude-code-releases",
        "https://downloads.claude.ai/claude-code-releases/",
        "https://github.com/openai/codex-evil/releases/download/x/y",
        "https://github.com/evil/codex/releases/download/rust-v0.156.0/x",
        "https://github.com/openai/codex/releases/latest/download",
        "https://releases.openai.com/codexx/channels/latest",
        "https://releases.openai.com/other/codex/x",
        "https://downloads.claude.ai/claude-code-releases/../../evil",
        "https://downloads.claude.ai/claude-code-releases/%2E%2E/evil",
        "https://downloads.claude.ai/claude-code-releases/.%2e/evil",
        "https://downloads.claude.ai/claude-code-releases/%2e/latest",
        "https://downloads.claude.ai/./claude-code-releases/latest",
        "https://github.com/openai/codex/releases/download/../../x/y/releases/download/z",
        "https://github.com/openai/codex/releases/download/rust-v0.156.0/%2e%2e/x",
        "https://releases.openai.com/codex/../codex/channels/latest",
        "https://downloads.claude.ai/claude-code-releases/2.1.280/../../../x",
        "https://downloads.claude.ai/claude-code-releases/./latest",
        "https://downloads.claude.ai/claude-code-releases/%2e%2e/evil",
        "https://downloads.claude.ai/claude-code-releases/a%2Fb",
        "https://downloads.claude.ai/claude-code-releases/a\\..\\b",
        "https://downloads.claude.ai/claude-code-releases//latest",
        "https://downloads.claude.ai/claude-code-releases/latest/",
        "https://downloads.claude.ai/claude-code-releases/latest?x=1",
        "https://downloads.claude.ai/claude-code-releases/latest#f",
        "https://releases.openai.com/codex/channels/latest?redirect=https://evil",
        "https://downloads.claude.ai/claude-code-releases/lat est",
        "https://downloads.claude.ai/claude-code-releases/latest\n",
        "https://github.com/MoonshotAI/kimi-code/releases/download/\
         %40moonshot-ai/kimi-code%402.3.1/manifest.json",
        "https://cdn.kimi.com@evil.com/kimi-code/latest",
        "https://user@cdn.kimi.com/kimi-code/latest",
        "https://evil.com/@cdn.kimi.com/kimi-code/latest",
        "https://cdn.kimi.com/kimi-code/latest?v=2.3.1@x",
        "https://cdn.kimi.com/kimi-code/binaries/2.3.1/@/../x",
        "https://code.kimi.com/kimi-code/latest",
        "https://cdn.kimi.com/kimi-cli/latest",
        "https://github.com/MoonshotAI/kimi-cli/releases/download/v1.0/x",
        "https://github.com/moonshotai/kimi-code/releases/download/x/y",
        "https://x.ai/cli/stable",
        "https://storage.googleapis.com/grok-build-public-artifacts/other/stable",
        "https://storage.googleapis.com/grok-build-public-artifacts/cli",
        "https://storage.googleapis.com/grok-build-public-artifacts/cli/a%2Fb",
        "https://storage.googleapis.com/xai-org/grok-build-public-artifacts/cli/stable",
        "https://registry.npmjs.org/@xai-official/grok/1.0.70",
        "https://registry.npmjs.org/@xai-official/grok/-/grok-1.0.70.tgz",
        "https://registry.npmjs.org/@xai-official/grok-linux-x64",
        "https://registry.npmjs.org/@xai-official/grok-linux-x64/",
        "https://registry.npmjs.org/@xai-official%2Fgrok-linux-x64/1.0.70",
        "https://registry.npmjs.org/@xai-official/grok-linux-x64/-%2Fgrok-linux-x64-1.0.70.tgz",
        "https://registry.npmjs.org/@xai-official/grok-linux-x64/1.0.70?write=true",
        "https://registry.npmjs.org/@xai-official/grok-linux-x64/../grok/1.0.70",
        "https://registry.npmjs.org/@xai-official/grok-linux-x64/%2e%2e/grok/1.0.70",
        "https://registry.npmjs.org/@xai-officia1/grok-linux-x64/1.0.70",
        "https://registry.npmjs.org/@evil/grok-linux-x64/1.0.70",
        "https://registry.npmjs.org/grok-linux-x64/1.0.70",
        "https://registry.npmjs.org/-/npm/v1/keys",
        "https://registry.npmjs.org.evil.com/@xai-official/grok-linux-x64/1.0.70",
        "https://registry.npmjs.com/@xai-official/grok-linux-x64/1.0.70",
        "https://registry.yarnpkg.com/@xai-official/grok-linux-x64/1.0.70",
        "https://registry.npmjs.org/@xai-official/grok-win32-x64/1.0.70",
        "",
    ] {
        for program in crate::vendor_direct::VENDORS.iter().map(|s| s.program) {
            assert!(!vendor_direct_url_allowed(program, bad), "{bad:?}");
        }
    }
}

#[test]
fn a_declared_query_is_admitted_exactly() {
    const DOWNLOAD: &str = "https://lookaside.facebook.com/lookaside/muse/download/";
    let row = crate::vendor_direct::VendorSpec {
        url_prefixes: &[DOWNLOAD, "https://api.meta.ai/muse-code/channels/"],
        url_query: &["channel", "version", "file"],
        ..*crate::vendor_direct::spec("claude").unwrap()
    };
    let q = |query: &str| format!("{DOWNLOAD}?{query}");
    let real = [
        q("channel=muse&version=1.4.9-R6297.1&file=manifest.json"),
        q("channel=muse&version=1.4.9-R6297.1&file=muse-aarch64-macos"),
        q("channel=muse&version=1.4.9-R6297&file=muse-x86-macos"),
    ];
    for url in &real {
        assert!(pinned_by(&row, url), "{url}");
    }
    assert!(pinned_by(
        &row,
        "https://api.meta.ai/muse-code/channels/muse-stable"
    ));
    for bad in [
        q("channel=muse&version=1.4.9-R6297.1&file=manifest.json&x=1"),
        q("channel=muse&version=1.4.9-R6297.1"),
        q("version=1.4.9-R6297.1&channel=muse&file=manifest.json"),
        q("channel=muse&channel=muse&version=1.4.9-R6297.1&file=manifest.json"),
        q("channel=muse&version=1.4.9-R6297.1&version=1.4.9-R6297.1&file=manifest.json"),
        q("Channel=muse&version=1.4.9-R6297.1&file=manifest.json"),
        q("channel=&version=1.4.9-R6297.1&file=manifest.json"),
        q("channel&version=1.4.9-R6297.1&file=manifest.json"),
        q("channel=muse&version=1.4.9-R6297.1&file=.."),
        q("channel=muse&version=1.4.9-R6297.1&file=%2e%2e"),
        q("channel=muse&version=1.4.9-R6297.1&file=manifest%2ejson"),
        q("channel=muse&version=1.4.9-R6297.1&file=../manifest.json"),
        q("channel=muse&version=1.4.9-R6297.1&file=a=b"),
        q("channel=muse&version=1.4.9-R6297.1&file=manifest json"),
        q("channel=muse&&version=1.4.9-R6297.1&file=manifest.json"),
        q("channel=muse&version=1.4.9-R6297.1&file=manifest.json&"),
        q("channel=muse&version=1.4.9-R6297.1&file=manifest.json#frag"),
        q(""),
        format!("{DOWNLOAD}x?channel=muse&version=1.4.9-R6297.1&file=manifest.json"),
        "https://lookaside.facebook.com/lookaside/muse/download?channel=muse\
         &version=1.4.9-R6297.1&file=manifest.json"
            .to_string(),
        "https://lookaside.facebook.com/lookaside/other/download/?channel=muse\
         &version=1.4.9-R6297.1&file=manifest.json"
            .to_string(),
        "https://api.meta.ai/muse-code/channels/muse-stable?channel=muse".to_string(),
    ] {
        assert!(!pinned_by(&row, &bad), "{bad}");
    }
    let no_query = crate::vendor_direct::VendorSpec {
        url_query: &[],
        ..row
    };
    for url in &real {
        assert!(!pinned_by(&no_query, url), "{url}");
    }
}

#[test]
fn a_digest_document_lands_only_where_its_host_allows() {
    const CLAUDE: &str = "https://downloads.claude.ai/claude-code-releases/2.1.280/manifest.json";
    const OPENAI: &str = "https://releases.openai.com/codex/channels/latest";
    const SUMS: &str =
        "https://github.com/openai/codex/releases/download/rust-v0.156.0/codex-package_SHA256SUMS";
    const STORAGE: &str = "https://release-assets.githubusercontent.com/\
                           github-production-release-asset/965415649/ce906d71?sp=r&sig=x%3D";
    assert!(digest_effective_url_ok("claude", CLAUDE, CLAUDE));
    assert!(digest_effective_url_ok("codex", OPENAI, OPENAI));
    assert!(digest_effective_url_ok("codex", SUMS, SUMS));
    assert!(digest_effective_url_ok("codex", SUMS, STORAGE));
    assert!(digest_effective_url_ok(
        "codex",
        SUMS,
        "https://objects.githubusercontent.com/github-production-release-asset/1/2"
    ));

    assert!(!digest_effective_url_ok("claude", OPENAI, OPENAI));
    assert!(!digest_effective_url_ok("codex", CLAUDE, CLAUDE));
    assert!(!digest_effective_url_ok("claude", SUMS, STORAGE));

    for (requested, effective) in [
        (
            CLAUDE,
            "https://downloads.claude.ai/claude-code-releases/2.1.280/other.json",
        ),
        (
            OPENAI,
            "https://releases.openai.com/codex/releases/0.156.0/release.json",
        ),
        (OPENAI, "https://releases.openai.com/codex/channels/latest/"),
        (
            OPENAI,
            "https://github.com/openai/codex/releases/download/x/y",
        ),
        (OPENAI, STORAGE),
        (CLAUDE, STORAGE),
        (CLAUDE, "https://storage.googleapis.com/claude-code-dist/x"),
        (
            SUMS,
            "https://raw.githubusercontent.com/openai/codex/main/x",
        ),
        (SUMS, "https://evil.example/x"),
        (
            SUMS,
            "https://release-assets.githubusercontent.com.evil.com/x",
        ),
        (OPENAI, "http://releases.openai.com/codex/channels/latest"),
        (
            OPENAI,
            "https://releases.openai.com:8443/codex/channels/latest",
        ),
        (
            OPENAI,
            "https://x@releases.openai.com/codex/channels/latest",
        ),
        (OPENAI, "https://RELEASES.OPENAI.COM/codex/channels/latest"),
        (OPENAI, "https://releases.openai.com./codex/channels/latest"),
        (SUMS, ""),
        (
            "https://releases.openai.com/other/x",
            "https://releases.openai.com/other/x",
        ),
        (
            "https://github.com/evil/repo/releases/download/t/a",
            STORAGE,
        ),
    ] {
        for program in ["claude", "codex"] {
            assert!(
                !digest_effective_url_ok(program, requested, effective),
                "{program}: {requested} -> {effective}"
            );
        }
    }
}

#[cfg(unix)]
fn scratch(label: &str) -> crate::testdir::Dir {
    crate::testdir::dir(&format!("atpkg-vendor-{label}-"))
}

#[cfg(unix)]
fn lay_exe(dir: &Path, name: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::create_dir_all(dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

#[cfg(unix)]
#[test]
fn a_system_binary_on_path_satisfies_and_the_managed_copy_never_does() {
    let root = scratch("probe");
    let prefix = root.join("prefix");
    let sys = root.join("usr-local-bin");
    let exe = lay_exe(&sys, "gh");
    lay_exe(&prefix.join("bin"), "gh");
    lay_exe(&prefix.join("store/gh/1/bin"), "gh");
    let path = std::env::join_paths([
        prefix.join("bin"),
        prefix.join("store/gh/1/bin"),
        sys.clone(),
    ])
    .unwrap();
    assert_eq!(
        system_binary_on_path(&prefix, "gh", Some(&path)),
        Some(exe.clone())
    );
    let managed_only =
        std::env::join_paths([prefix.join("bin"), prefix.join("store/gh/1/bin")]).unwrap();
    assert_eq!(
        system_binary_on_path(&prefix, "gh", Some(&managed_only)),
        None
    );
    let link_dir = root.join("home-bin");
    std::fs::create_dir_all(&link_dir).unwrap();
    std::os::unix::fs::symlink(prefix.join("store/gh/1/bin/gh"), link_dir.join("gh")).unwrap();
    let via_link = std::env::join_paths([link_dir.clone()]).unwrap();
    assert_eq!(system_binary_on_path(&prefix, "gh", Some(&via_link)), None);
    let brew = root.join("brew-bin");
    std::fs::create_dir_all(&brew).unwrap();
    std::os::unix::fs::symlink(&exe, brew.join("gh")).unwrap();
    let via_brew = std::env::join_paths([brew.clone()]).unwrap();
    assert_eq!(
        system_binary_on_path(&prefix, "gh", Some(&via_brew)),
        Some(brew.join("gh"))
    );
    let cwd = std::env::current_dir().unwrap();
    let ups: PathBuf =
        std::iter::repeat_n("..", cwd.components().count().saturating_sub(1)).collect();
    let relative = ups.join(sys.strip_prefix("/").unwrap());
    assert!(!relative.is_absolute());
    assert!(
        relative.join("gh").exists(),
        "PRECONDITION: the relative spelling really reaches the system copy ({})",
        relative.display()
    );
    let via_relative = std::env::join_paths([relative.clone()]).unwrap();
    assert_eq!(
        system_binary_on_path(&prefix, "gh", Some(&via_relative)),
        None,
        "a relative PATH entry is the cwd, not a system"
    );
    let dot = std::env::join_paths([PathBuf::from("."), PathBuf::from("bin")]).unwrap();
    assert_eq!(system_binary_on_path(&prefix, "gh", Some(&dot)), None);
    let mixed = std::env::join_paths([relative, sys.clone()]).unwrap();
    assert_eq!(
        system_binary_on_path(&prefix, "gh", Some(&mixed)),
        Some(exe.clone())
    );
    assert_eq!(system_binary_on_path(&prefix, "emacs", Some(&path)), None);
    std::fs::write(sys.join("plain"), b"x").unwrap();
    assert_eq!(system_binary_on_path(&prefix, "plain", Some(&path)), None);
    assert_eq!(system_binary_on_path(&prefix, "gh", None), None);
    assert_eq!(system_binary_on_path(&prefix, "../gh", Some(&path)), None);
    assert_eq!(system_binary_on_path(&prefix, "", Some(&path)), None);
    lay_exe(&sys, "cargo");
    assert_eq!(system_binary_on_path(&prefix, "cargo", Some(&path)), None);
    let unmanaged = Program {
        repo: "gh".into(),
        policy: String::new(),
        coherence_group: None,
        system: None,
        unavailable_hint: None,
        requires: vec![],
    };
    assert_eq!(system_satisfied(&prefix, &unmanaged), None);
    let _ = std::fs::remove_dir_all(&root);
}

/// A hit before the managed `bin/` (or with it absent) is a shadow.
#[cfg(unix)]
#[test]
fn the_shadow_probe_reports_only_what_precedes_the_managed_bin() {
    let root = scratch("shadow");
    let prefix = root.join("prefix");
    let local = root.join("local-bin");
    let exe = lay_exe(&local, "trust");
    lay_exe(&prefix.join("bin"), "trust");
    lay_exe(&prefix.join("store/trust/6808/bin"), "trust");
    let before = std::env::join_paths([local.clone(), prefix.join("bin")]).unwrap();
    assert_eq!(
        shadowing_binary_on_path(&prefix, "trust", Some(&before)),
        Some(exe.clone())
    );
    let after = std::env::join_paths([prefix.join("bin"), local.clone()]).unwrap();
    assert_eq!(
        shadowing_binary_on_path(&prefix, "trust", Some(&after)),
        None
    );
    let absent = std::env::join_paths([local.clone()]).unwrap();
    assert_eq!(
        shadowing_binary_on_path(&prefix, "trust", Some(&absent)),
        Some(exe.clone())
    );
    let store_first =
        std::env::join_paths([prefix.join("store/trust/6808/bin"), local.clone()]).unwrap();
    assert_eq!(
        shadowing_binary_on_path(&prefix, "trust", Some(&store_first)),
        None
    );
    let reroute = prefix.join("reroute");
    lay_exe(&reroute, "cargo");
    let reroute_first =
        std::env::join_paths([reroute.clone(), local.clone(), prefix.join("bin")]).unwrap();
    assert_eq!(
        shadowing_binary_on_path(&prefix, "trust", Some(&reroute_first)),
        Some(exe.clone()),
        "the reroute dir is transparent for a name it does not carry"
    );
    let agents = prefix.join("agents");
    lay_exe(&agents, "claude");
    lay_exe(&prefix.join("store/claude/2026091001/bin"), "claude");
    let foreign_claude = lay_exe(&local, "claude");
    let agents_first =
        std::env::join_paths([agents.clone(), local.clone(), prefix.join("bin")]).unwrap();
    assert_eq!(
        shadowing_binary_on_path(&prefix, "claude", Some(&agents_first)),
        None,
        "the agents twin at PATH[0] is the managed copy running"
    );
    assert_eq!(
        shadowing_binary_on_path(&prefix, "trust", Some(&agents_first)),
        Some(exe.clone()),
        "and transparent for every other name"
    );
    let other = root.join("other-bin");
    let other_claude = lay_exe(&other, "claude");
    let dup = root.join("dup-bin");
    std::fs::create_dir_all(&dup).unwrap();
    std::os::unix::fs::symlink(&other_claude, dup.join("claude")).unwrap();
    let wide = std::env::join_paths([
        agents.clone(),
        local.clone(),
        other.clone(),
        dup.clone(),
        prefix.join("bin"),
    ])
    .unwrap();
    assert_eq!(
        foreign_copies_on_path(&prefix, "claude", Some(&wide)),
        vec![foreign_claude.clone(), other_claude.clone()]
    );
    assert!(foreign_copies_on_path(&prefix, "trust-mc", Some(&wide)).is_empty());
    assert!(foreign_copies_on_path(&prefix, "claude", None).is_empty());
    assert!(foreign_copies_on_path(&prefix, "../claude", Some(&wide)).is_empty());
    let link_dir = root.join("home-bin");
    std::fs::create_dir_all(&link_dir).unwrap();
    std::os::unix::fs::symlink(
        prefix.join("store/trust/6808/bin/trust"),
        link_dir.join("trust"),
    )
    .unwrap();
    let via_link = std::env::join_paths([link_dir.clone(), prefix.join("bin")]).unwrap();
    assert_eq!(
        shadowing_binary_on_path(&prefix, "trust", Some(&via_link)),
        None
    );
    let alias = root.join("alias-bin");
    std::os::unix::fs::symlink(prefix.join("bin"), &alias).unwrap();
    let via_alias = std::env::join_paths([alias.clone(), local.clone()]).unwrap();
    assert_eq!(
        shadowing_binary_on_path(&prefix, "trust", Some(&via_alias)),
        None,
        "a symlinked managed bin/ ahead stops the walk"
    );
    let link_then_local = std::env::join_paths([link_dir.clone(), local.clone()]).unwrap();
    assert_eq!(
        shadowing_binary_on_path(&prefix, "trust", Some(&link_then_local)),
        None,
        "a link into the store ahead is the managed copy running"
    );
    assert_eq!(
        system_binary_on_path(&prefix, "trust", Some(&via_alias)),
        Some(exe.clone())
    );
    assert_eq!(
        system_binary_on_path(&prefix, "trust", Some(&link_then_local)),
        Some(exe.clone())
    );
    let dir_named = root.join("dir-named");
    std::fs::create_dir_all(dir_named.join("trust")).unwrap();
    let dir_first = std::env::join_paths([dir_named.clone(), prefix.join("bin")]).unwrap();
    assert_eq!(
        shadowing_binary_on_path(&prefix, "trust", Some(&dir_first)),
        None
    );
    assert_eq!(
        system_binary_on_path(&prefix, "trust", Some(&dir_first)),
        None
    );
    let dot = std::env::join_paths([PathBuf::from("."), prefix.join("bin")]).unwrap();
    assert_eq!(shadowing_binary_on_path(&prefix, "trust", Some(&dot)), None);
    assert_eq!(
        shadowing_binary_on_path(&prefix, "trust-mc", Some(&before)),
        None
    );
    assert_eq!(shadowing_binary_on_path(&prefix, "trust", None), None);
    assert_eq!(
        shadowing_binary_on_path(&prefix, "../trust", Some(&before)),
        None
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn agents_dir_on_path_sees_the_dir_as_spelled_or_resolved_and_absolute_only() {
    let prefix = crate::testdir::dir("atpkg-vendor-agents-");
    std::fs::create_dir_all(prefix.join("agents")).unwrap();
    let agents = prefix.join("agents");
    assert!(!agents_dir_on_path(&prefix, None));
    let without = std::env::join_paths([prefix.join("bin"), PathBuf::from("/usr/bin")]).unwrap();
    assert!(!agents_dir_on_path(&prefix, Some(&without)));
    let with = std::env::join_paths([PathBuf::from("/usr/bin"), agents.clone()]).unwrap();
    assert!(
        agents_dir_on_path(&prefix, Some(&with)),
        "anywhere on PATH counts"
    );
    let relative = std::ffi::OsString::from("agents:/usr/bin");
    assert!(
        !agents_dir_on_path(&prefix, Some(&relative)),
        "a relative entry never counts"
    );
    #[cfg(unix)]
    {
        let link = prefix.join("agents-link");
        std::os::unix::fs::symlink(&agents, &link).unwrap();
        let via_link = std::env::join_paths([link]).unwrap();
        assert!(
            agents_dir_on_path(&prefix, Some(&via_link)),
            "as it resolves"
        );
    }
    let _ = std::fs::remove_dir_all(&prefix);
}
