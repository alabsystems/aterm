// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn scratch(label: &str) -> (aterm_tempfile::TempDir, PathBuf) {
    let dir = crate::test_scratch_dir(label);
    let canon = std::fs::canonicalize(dir.path()).expect("canonical scratch dist");
    (dir, canon)
}

fn bundle_in(dist: &Path, name: &str) -> PathBuf {
    let app = dist.join(name).join("aterm.app/Contents/MacOS");
    std::fs::create_dir_all(&app).expect("create staging bundle");
    dist.join(name)
}

/// NEGATIVE CONTROL: `set_plist_string` must ignore commented-out `<key>`/`</dict>` tags,
/// replace empty `<string/>` tags cleanly, and refuse unescaped `<` inside `<string>` or
/// non-`<string>` values without clobbering later keys.
#[test]
fn set_plist_string_rejects_non_string_values_without_overwriting_later_strings() {
    let plist = "<plist><dict>\n\
        \t<!-- <key>Name</key><string>commented</string> -->\n\
        \t<key>Flag</key>\n\t<false/>\n\
        \t<key>Items</key>\n\t<array><string>keep-me</string></array>\n\
        \t<key>Empty</key>\n\t<!-- gap --><string/>\n\
        \t<key>Name</key>\n\t<string>old</string>\n\
        </dict><!-- </dict> --></plist>";
    let err = set_plist_string(plist, "Flag", "new").unwrap_err();
    assert!(err.contains("Flag is not a <string> value"), "{err}");
    let err = set_plist_string(plist, "Items", "new").unwrap_err();
    assert!(err.contains("Items is not a <string> value"), "{err}");
    let updated = set_plist_string(plist, "Name", "new").unwrap();
    assert!(updated.contains("<!-- <key>Name</key><string>commented</string> -->"));
    assert!(updated.contains("<string>keep-me</string>"));
    assert!(updated.contains("<key>Name</key>\n\t<string>new</string>"));
    let filled = set_plist_string(&updated, "Empty", "filled").unwrap();
    assert!(filled.contains("<key>Empty</key>\n\t<!-- gap --><string>filled</string>"));
    let inserted = set_plist_string(&filled, "Added", "val").unwrap();
    assert!(
        inserted.contains("\t<key>Added</key>\n\t<string>val</string>\n</dict><!-- </dict> -->")
    );

    let unclosed = "<plist><dict>\n\
        \t<key>Broken</key>\n\t<string>missing-close\n\
        \t<key>Later</key>\n\t<string>ok</string>\n\
        </dict></plist>";
    let err = set_plist_string(unclosed, "Broken", "new").unwrap_err();
    assert!(err.contains("Broken"), "{err}");
    let err = set_plist_string("<!-- unclosed <dict></dict>", "Added", "x").unwrap_err();
    assert!(err.contains("Added"), "{err}");
}

#[test]
fn the_staging_name_is_per_claim_and_recognised() {
    assert_eq!(
        staged_app_path(Path::new("/r/dist"), 1_790_000_000),
        PathBuf::from("/r/dist/cut-1790000000.noindex/aterm.app")
    );
    assert_ne!(
        staged_app_path(Path::new("/r/dist"), 1),
        staged_app_path(Path::new("/r/dist"), 2),
        "two claims never share a staging bundle"
    );
    for name in ["cut-0.noindex", "cut-1.noindex", "cut-1790000000.noindex"] {
        assert!(is_staging_dir_name(name), "{name}");
    }
    for name in [
        "cut-app",
        "cut-state.toml",
        "cut-.noindex",
        "cut-007.noindex",
        "cut-+7.noindex",
        "cut-999999999999999999999.noindex",
        "cut-12x.noindex",
        "cut-app-old",
        "aterm.app",
        "cut-1.noindex.bak",
    ] {
        assert!(!is_staging_dir_name(name), "{name}");
    }
}

/// A staging the self-check started a copy in: [`aterm_uds::deadline::stamp_exec`], and
/// [`aterm_uds::deadline::RAN`] when `ran`.
fn started_in(dist: &Path, build: u64, ran: bool) -> PathBuf {
    let dir = bundle_in(dist, &staging_dir_name(build));
    aterm_uds::deadline::stamp_exec(&dir);
    if ran {
        std::fs::write(dir.join(aterm_uds::deadline::RAN), "").unwrap();
    }
    dir
}

#[test]
fn pruning_removes_only_what_is_provably_unused() {
    let (_guard, dist) = scratch("prune");
    let keep = bundle_in(&dist, &staging_dir_name(3));
    let legacy = bundle_in(&dist, "cut-app");
    let old = bundle_in(&dist, &staging_dir_name(1));
    let live = bundle_in(&dist, &staging_dir_name(2));
    let unproven = started_in(&dist, 4, false);
    let proven = started_in(&dist, 5, true);
    std::fs::write(dist.join("cut-state.toml"), "journal").unwrap();
    let dev = dist.join("aterm.app");
    std::fs::create_dir_all(&dev).unwrap();

    prune_staging_dirs(&dist, &keep, None);
    assert!(legacy.is_dir() && old.is_dir() && live.is_dir());

    let processes = vec![(
        "85619".to_string(),
        format!("{}/aterm.app/Contents/MacOS/aterm", live.display()),
    )];
    let lines = prune_staging_dirs(&dist, &keep, Some(&processes));
    assert!(
        legacy.is_dir(),
        "the retired fixed directory is not this cutter's to delete: {lines:?}"
    );
    assert!(!old.exists(), "an unused older claim is pruned: {lines:?}");
    assert!(live.is_dir(), "a live one is KEPT: {lines:?}");
    assert!(
        unproven.is_dir() && lines.iter().any(|l| l.contains("never showed it ran")),
        "one whose copy never showed it ran is KEPT: {lines:?}"
    );
    assert!(!proven.exists(), "one whose copy ran is pruned: {lines:?}");
    assert!(keep.is_dir(), "this claim's own directory is never pruned");
    assert!(dist.join("cut-state.toml").is_file() && dev.is_dir());
}

/// A finished cut removes its own staging directory, but never from under a live
/// process, on a process list it could not read, or while its copy never showed it ran.
#[test]
fn a_finished_cut_discards_its_staging_bundle_only_when_provably_unused() {
    let (_guard, dist) = scratch("discard");
    let own = bundle_in(&dist, &staging_dir_name(7));
    let other = bundle_in(&dist, &staging_dir_name(6));
    let exe = format!("{}/aterm.app/Contents/MacOS/aterm", own.display());

    let unknown = discard_staged_dir(&dist, 7, None);
    assert!(unknown.is_err() && own.is_dir(), "{unknown:?}");

    let live = vec![("4242".to_string(), exe)];
    let refused = discard_staged_dir(&dist, 7, Some(&live));
    assert!(refused.is_err() && own.is_dir(), "{refused:?}");

    aterm_uds::deadline::stamp_exec(&own);
    let held = discard_staged_dir(&dist, 7, Some(&[]));
    assert!(
        held.as_ref()
            .is_err_and(|why| why.contains("never showed it ran"))
            && own.is_dir(),
        "{held:?}"
    );
    std::fs::write(own.join(aterm_uds::deadline::RAN), "").unwrap();
    assert_eq!(discard_staged_dir(&dist, 7, Some(&[])), Ok(()));
    assert!(!own.exists(), "the finished claim's bundle is gone");
    assert!(
        other.is_dir(),
        "another claim's directory is the prune's, not this"
    );
    assert_eq!(
        discard_staged_dir(&dist, 7, Some(&[])),
        Ok(()),
        "nothing left to discard is not a failure"
    );
}
