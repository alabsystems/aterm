// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Package records are display metadata; these fixtures never execute their entrypoints.

use super::*;
use aterm_uds::process::Birth;

fn package(root: &Path, version: &str) -> PathBuf {
    std::fs::create_dir_all(root.join("bin")).unwrap();
    let exe = root.join("bin/codex");
    std::fs::write(&exe, b"plain fixture; never executed").unwrap();
    std::fs::write(
        root.join("codex-package.json"),
        format!(r#"{{"layoutVersion":1,"version":"{version}","entrypoint":"bin/codex"}}"#),
    )
    .unwrap();
    std::fs::write(root.join("alab-build.json"), b"{}").unwrap();
    exe
}

#[test]
fn a_retained_live_package_stays_distinct_after_the_development_link_advances() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let layout = atpkg::store::Layout {
        prefix: scratch.path().join("pkg"),
    };
    let a = scratch.path().join("artifacts/a");
    let b = scratch.path().join("artifacts/b");
    let old = "0.1.0-dev.1+g111111111";
    let new = "0.1.0-dev.2+g222222222";
    let running = package(&a, old);
    package(&b, new);
    let marker = layout.link_marker("codex");
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    let checkout = Value::from(b.to_str().unwrap()).to_string();
    std::fs::write(
        marker,
        format!("schema = 1\nprogram = \"codex\"\ncheckout = {checkout}\nbins = [\"bin/codex\"]\nlinked_at = \"\"\n"),
    )
    .unwrap();
    assert_eq!(atpkg::linked_checkout(&layout, "codex"), Some(b));
    let observed = |pid| {
        claimant_build_version_reading(
            &layout,
            "codex",
            pid,
            |_| Some(Birth(100)),
            |_| Some(running.clone()),
        )
    };
    assert_eq!(observed(42).as_deref(), Some(old));
    let installed = atpkg::harness::installed(&layout);
    assert_eq!(installed.len(), 1);
    assert_eq!(installed[0].version.as_deref(), Some(new));
    let claims = [LiveClaim {
        sid: "s-ab".into(),
        holder: "codex-alab@42".into(),
        proven: true,
        version: None,
        vendor: None,
    }];
    let mut output = Vec::new();
    write_versions(
        Some(&installed),
        Some(&claims),
        &observed,
        true,
        &mut output,
    );
    let rows: Value = aterm_json::from_str(std::str::from_utf8(&output).unwrap()).unwrap();
    let row = |kind: &str| {
        rows.as_array()
            .unwrap()
            .iter()
            .find(|row| row["kind"].as_str() == Some(kind))
            .unwrap()
    };
    assert_eq!(row("package")["version"].as_str(), Some(new));
    assert_eq!(row("claim")["version"].as_str(), Some(old));
    assert_ne!(row("built-in")["version"].as_str(), Some(old));
}

#[test]
fn changed_or_unreadable_birth_cannot_label_a_reused_pid_with_package_metadata() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let layout = atpkg::store::Layout {
        prefix: scratch.path().join("pkg"),
    };
    let exe = package(&scratch.path().join("a"), "0.1.0");
    for after in [None, Some(Birth(200)), Some(Birth(100))] {
        let mut reads = 0;
        let shown = claimant_build_version_reading(
            &layout,
            "codex",
            42,
            |_| {
                reads += 1;
                if reads == 1 { Some(Birth(100)) } else { after }
            },
            |_| Some(exe.clone()),
        );
        assert_eq!(reads, 2);
        assert_eq!(
            shown.as_deref(),
            (after == Some(Birth(100))).then_some("0.1.0")
        );
    }
    assert_eq!(
        claimant_build_version_reading(
            &layout,
            "codex",
            42,
            |_| None,
            |_| panic!("no birth proof")
        ),
        None
    );
    let other = exe.with_file_name("other");
    std::fs::write(&other, b"another plain file").unwrap();
    assert_eq!(
        claimant_build_version_reading(&layout, "codex", 42, |_| Some(Birth(100)), |_| Some(other)),
        None
    );
    std::fs::write(
        exe.parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("codex-package.json"),
        b"{",
    )
    .unwrap();
    assert_eq!(
        claimant_build_version_reading(&layout, "codex", 42, |_| Some(Birth(100)), |_| Some(exe)),
        None
    );
}

#[test]
fn a_harness_store_build_and_retained_local_artifact_resolve_independently() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let layout = atpkg::store::Layout {
        prefix: scratch.path().join("pkg"),
    };
    let store_dir = layout.build_dir("grok-alab", 17);
    std::fs::create_dir_all(store_dir.join("bin")).unwrap();
    let store_exe = store_dir.join("bin/grok-alab");
    std::fs::write(&store_exe, b"plain fixture; never executed").unwrap();
    atpkg::store::mark_build_ready(&store_dir).unwrap();
    std::fs::write(store_dir.with_extension("version"), b"version v1\n0.3.0\n").unwrap();
    assert_eq!(
        claimant_build_version_reading(
            &layout,
            "grok-alab",
            42,
            |_| Some(Birth(100)),
            |_| Some(store_exe.clone()),
        )
        .as_deref(),
        Some("0.3.0")
    );

    let local_a = scratch.path().join("artifacts/grok-a");
    let local_b = scratch.path().join("artifacts/grok-b");
    for (dir, ver) in [
        (&local_a, "0.3.0+g111111111"),
        (&local_b, "0.3.0+g222222222"),
    ] {
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin/grok-alab"), b"plain fixture; never executed").unwrap();
        std::fs::write(
            dir.join(atpkg::harness::LOCAL_BUILD_JSON),
            format!(
                r#"{{"schema":2,"origin":"local-build","program":"grok-alab","command":"grok-alab","version":"{ver}"}}"#
            ),
        )
        .unwrap();
    }
    let marker = layout.link_marker("grok-alab");
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    let checkout = Value::from(local_b.to_str().unwrap()).to_string();
    std::fs::write(
        marker,
        format!(
            "schema = 1\nprogram = \"grok-alab\"\ncheckout = {checkout}\nbins = [\"bin/grok-alab\"]\nlinked_at = \"\"\n"
        ),
    )
    .unwrap();
    assert_eq!(
        claimant_build_version_reading(
            &layout,
            "grok-alab",
            42,
            |_| Some(Birth(100)),
            |_| Some(local_a.join("bin/grok-alab")),
        )
        .as_deref(),
        Some("0.3.0+g111111111")
    );
    let store_other = store_dir.join("bin/other");
    std::fs::write(&store_other, b"not the harness entrypoint").unwrap();
    assert_eq!(
        claimant_build_version_reading(
            &layout,
            "grok-alab",
            42,
            |_| Some(Birth(100)),
            |_| Some(store_other),
        ),
        None,
        "an unexposed file in a store build is refused"
    );
    let local_other = local_b.join("bin/other");
    std::fs::write(&local_other, b"not the harness entrypoint").unwrap();
    assert_eq!(
        claimant_build_version_reading(
            &layout,
            "grok-alab",
            42,
            |_| Some(Birth(100)),
            |_| Some(local_other),
        ),
        None,
        "an unexposed file in the linked local artifact is refused"
    );
    assert_eq!(
        claimant_build_version_reading(
            &layout,
            "grok",
            42,
            |_| Some(Birth(100)),
            |_| Some(store_exe),
        ),
        None,
        "a vendor program that is neither a harness nor an ALab agent is refused"
    );
    let installed = atpkg::harness::installed(&layout);
    assert_eq!(installed.len(), 1);
    assert_eq!(installed[0].harness, "grok-alab");
    assert_eq!(installed[0].version.as_deref(), Some("0.3.0+g222222222"));
}
