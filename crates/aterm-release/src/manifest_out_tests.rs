// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

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
