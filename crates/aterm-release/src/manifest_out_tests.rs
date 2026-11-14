// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_update_core::windows::{WindowsArtifact, WindowsTarget};

fn inputs() -> ManifestInputs<'static> {
    ManifestInputs {
        version: "0.200.0",
        build_number: 2_000,
        commit: "0123456789abcdef0123456789abcdef01234567",
        dmg_name: "aterm-0.200.0.dmg",
        dmg_sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        zip_name: "aterm-0.200.0-mac.zip",
        zip_sha256: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        repo_slug: "alabsystems/aterm",
        min_os: "14.0",
        team_id: "",
        pub_date: "2026-11-12T00:00:00Z",
        min_build: None,
        changelog: "- Windows joins the appcast.\n",
    }
}

/// The writer emits a Windows zip's fields only once one is stamped: an appcast without one
/// carries no Windows key, a stamped one adds exactly its three keys and reads back through
/// the clients' reader with the zip selected, and an entry that reader refuses reaches the
/// signed bytes neither through the stamp nor set by hand.
#[test]
fn a_stamped_windows_zip_is_emitted_and_reads_back() {
    let built = build(&inputs());
    let plain = emit(&built).unwrap();
    assert!(!plain.contains("windows"), "{plain}");

    let digest = "cd".repeat(32);
    let zip = WindowsArtifact {
        target: WindowsTarget::X86_64,
        name: "aterm-0.200.0-windows-x86_64.zip",
        sha256: &digest,
        size: 95_000_000,
    };
    let mut stamped = built.clone();
    stamped.set_windows_artifact(&zip).unwrap();
    let text = emit(&stamped).unwrap();
    let added: Vec<&str> = text
        .lines()
        .filter(|line| !plain.lines().any(|old| old == *line))
        .collect();
    assert_eq!(
        added,
        [
            "windows_x86_64 = \"aterm-0.200.0-windows-x86_64.zip\"".to_string(),
            format!("windows_x86_64_sha256 = \"{digest}\""),
            "windows_x86_64_size = 95000000".to_string(),
        ],
        "{text}"
    );
    assert_eq!(text.lines().count(), plain.lines().count() + 3);
    let back = Manifest::parse(&text).unwrap();
    assert_eq!(back, stamped);
    assert_eq!(
        back.windows_artifact(WindowsTarget::X86_64).unwrap(),
        Some(zip)
    );

    for wrong in [
        WindowsArtifact {
            name: "aterm-0.199.0-windows-x86_64.zip",
            ..zip
        },
        WindowsArtifact {
            name: "aterm-0.200.0-x86_64-portable.zip",
            ..zip
        },
        WindowsArtifact { size: 0, ..zip },
    ] {
        let mut refused = built.clone();
        assert!(refused.set_windows_artifact(&wrong).is_err(), "{wrong:?}");
        assert_eq!(refused, built);
        let mut forced = built.clone();
        forced.windows_x86_64 = Some(wrong.name.to_string());
        forced.windows_x86_64_sha256 = Some(wrong.sha256.to_string());
        forced.windows_x86_64_size = Some(wrong.size);
        assert!(emit(&forced).is_err(), "{wrong:?}");
    }
}

#[test]
fn plist_string_requires_adjacent_string_tag_after_key() {
    let plist = "<plist><dict>\n\
        \t<key>Flag</key>\n\t<false/>\n\
        \t<key>Items</key>\n\t<array><string>item-zero</string></array>\n\
        \t<key>Name</key>\n\t<string>a &amp;lt; b</string>\n\
        </dict></plist>";
    assert_eq!(plist_string(plist, "Name").as_deref(), Some("a &lt; b"));
    assert_eq!(plist_string(plist, "Flag"), None);
    assert_eq!(plist_string(plist, "Items"), None);
}

/// NEGATIVE CONTROL: `plist_string` must ignore XML comments, decode `<string/>` and
/// `&quot;`/`&apos;`, and reject unescaped `<` or unknown XML entities; and `write` must
/// refuse a symlink destination instead of overwriting its target.
#[test]
fn plist_string_handles_comments_empty_tags_and_entities_and_write_refuses_symlinks() {
    let plist = "<plist><dict>\n\
        <!-- <key>Team</key><string>shadowed</string> -->\n\
        <key>Team</key>\n\t<string/>\n\
        <key>Quoted</key>\n\t<string>&quot;hi&apos;</string>\n\
        <key>BrokenTag</key>\n\t<string>a < b</string>\n\
        <key>BrokenEnt</key>\n\t<string>a &bogus; b</string>\n\
        </dict></plist>";
    assert_eq!(plist_string(plist, "Team").as_deref(), Some(""));
    assert_eq!(plist_string(plist, "Quoted").as_deref(), Some("\"hi'"));
    assert_eq!(plist_string(plist, "BrokenTag"), None);
    assert_eq!(plist_string(plist, "BrokenEnt"), None);
    assert_eq!(
        plist_string(
            "<!-- unterminated <key>Team</key><string>x</string>",
            "Team"
        ),
        None
    );

    #[cfg(unix)]
    {
        let dir = aterm_tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim.toml");
        std::fs::write(&victim, "keep = true\n").unwrap();
        let link_out = dir.path().join("out");
        std::fs::create_dir_all(&link_out).unwrap();
        std::os::unix::fs::symlink(&victim, link_out.join(MANIFEST_ASSET)).unwrap();
        let m = build(&ManifestInputs {
            version: "0.2.0",
            build_number: 7,
            commit: "0123456789abcdef0123456789abcdef01234567",
            dmg_name: "aterm-0.2.0.dmg",
            dmg_sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            zip_name: "aterm-0.2.0-mac.zip",
            zip_sha256: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            repo_slug: "alabsystems/aterm",
            min_os: "14.0",
            team_id: "",
            pub_date: "2026-11-03T00:00:00Z",
            min_build: None,
            changelog: "- Fixed manifest write.\n",
        });
        assert!(write(&link_out, &m).is_err());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep = true\n");

        let clean_out = dir.path().join("clean");
        std::fs::create_dir_all(&clean_out).unwrap();
        let written = write(&clean_out, &m).unwrap();
        let parsed = Manifest::parse(&std::fs::read_to_string(&written).unwrap()).unwrap();
        assert_eq!(parsed, m);
    }
}
