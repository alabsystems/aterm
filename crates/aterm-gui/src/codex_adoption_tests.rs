// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn codex_adoption_follows_the_mapped_executable_across_a_real_selection_change() {
    use aterm_uds::deadline::{KeptUntilRan, ad_hoc_copy, cat_echoes};
    use aterm_uds::spawnfd::{Stdio, command};
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::Path;

    let mut scratch = KeptUntilRan::new(aterm_tempfile::tempdir().unwrap().keep());
    let layout = atpkg::Layout {
        prefix: scratch.path().canonicalize().unwrap(),
    };
    let old = 2026111601;
    let new = 2026111701;
    let install = |build| {
        let root = layout.build_dir("codex", build);
        fs::create_dir_all(root.join("bin")).unwrap();
        let executable = root.join("bin/codex");
        ad_hoc_copy(Path::new("/bin/cat"), &executable);
        // Two different installed builds can report the very same product and client version.
        fs::write(
            root.join("codex-package.json"),
            r#"{"layoutVersion":1,"entrypoint":"bin/codex","version":"0.11.0"}"#,
        )
        .unwrap();
        fs::write(
            root.join("alab-build.json"),
            r#"{"version":"0.11.0","client_version":"0.170.0"}"#,
        )
        .unwrap();
        atpkg::store::mark_build_ready(&root).unwrap();
        executable
    };
    let first = install(old);
    let second = install(new);
    let shim = layout.bin_dir().join("codex");
    fs::create_dir_all(layout.bin_dir()).unwrap();
    symlink(&first, &shim).unwrap();
    let start = || {
        let mut child = command(&shim)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let input = cat_echoes(&mut child);
        (child, input)
    };
    let (mut original, original_input) = start();
    let observe = |pid| sample(Some(&layout), || loaded(pid)).fields();
    assert_eq!(
        observe(original.id()),
        "codex_adoption=matched codex_running_build=2026111601 codex_selected_build=2026111601"
    );
    let next = layout.bin_dir().join("next");
    symlink(&second, &next).unwrap();
    fs::rename(&next, &shim).unwrap();
    assert_eq!(
        observe(original.id()),
        "codex_adoption=differs codex_running_build=2026111601 codex_selected_build=2026111701"
    );
    assert_eq!(
        sample(Some(&layout), || None).fields(),
        "codex_adoption=unknown codex_running_build=- codex_selected_build=2026111701"
    );
    let (mut successor, successor_input) = start();
    scratch.ran();
    assert_eq!(
        observe(successor.id()),
        "codex_adoption=matched codex_running_build=2026111701 codex_selected_build=2026111701"
    );
    let reads = std::cell::Cell::new(0);
    assert!(
        sample(Some(&layout), || {
            let n = reads.replace(reads.get() + 1);
            loaded(if n == 0 {
                original.id()
            } else {
                successor.id()
            })
        })
        .running
        .is_none()
    );
    let reads = std::cell::Cell::new(0);
    let changed = sample(Some(&layout), || {
        if reads.replace(reads.get() + 1) == 1 {
            symlink(&first, &next).unwrap();
            fs::rename(&next, &shim).unwrap();
        }
        loaded(original.id())
    });
    assert_eq!(
        changed.fields(),
        "codex_adoption=unknown codex_running_build=2026111601 codex_selected_build=-"
    );
    fs::remove_file(shim).unwrap();
    assert_eq!(
        observe(original.id()),
        "codex_adoption=unknown codex_running_build=2026111601 codex_selected_build=-"
    );
    let alias_build = layout.build_dir("codex", new + 1);
    fs::create_dir_all(alias_build.join("bin")).unwrap();
    fs::hard_link(&first, alias_build.join("bin/codex")).unwrap();
    for manifest in ["codex-package.json", "alab-build.json"] {
        fs::copy(
            layout.build_dir("codex", old).join(manifest),
            alias_build.join(manifest),
        )
        .unwrap();
    }
    atpkg::store::mark_build_ready(&alias_build).unwrap();
    assert!(
        sample(Some(&layout), || loaded(original.id()))
            .running
            .is_none()
    );
    drop(original_input);
    drop(successor_input);
    assert!(original.wait().unwrap().success());
    assert!(successor.wait().unwrap().success());
}

#[test]
fn codex_adoption_rechecks_the_claim_revision_before_publishing_queued_work() {
    use crate::session_timeline::{claim_supervisor_as, release_supervisor};
    let handle = crate::session_store::test_handle_on(923572, -1);
    let ctx = &handle.ctx;
    let holder = "codex-alab@4242";
    let source = Proven {
        pid: 4242,
        start: "known-start".into(),
    };
    let take = || {
        claim_supervisor_as(
            ctx,
            holder,
            Some(27),
            None,
            Some(source.clone()),
            crate::metrics::now_us(),
            aterm_slog::Actor::Ctl,
        )
        .unwrap()
    };
    take();
    ctx.timeline.lock().unwrap().note_foreground_group(4242);
    let now = Instant::now();
    let mut scheduled = None;
    let job = Job::due(&mut scheduled, ctx, now).unwrap();
    assert!(Job::due(&mut scheduled, ctx, now).is_none());
    assert!(
        Job::due(
            &mut scheduled,
            ctx,
            now + crate::codex_rollout_tap::REST_FAR
        )
        .is_some()
    );
    assert!(release_supervisor(
        ctx,
        Some(27),
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl
    ));
    take();
    let fresh = Job::due(&mut scheduled, ctx, now).expect("a new claim is observed immediately");
    job.run_with(|_| true, || 4242, || None, |_| None);
    assert!(
        !ctx.timeline
            .lock()
            .unwrap()
            .view_newest_first()
            .any(|(row, _)| row == "codex-adoption")
    );
    fresh.run_with(|_| true, || 4242, || None, |_| None);
    assert_eq!(
        ctx.timeline
            .lock()
            .unwrap()
            .view_newest_first()
            .filter(|(row, _)| *row == "codex-adoption")
            .count(),
        1
    );
    assert!(release_supervisor(
        ctx,
        Some(27),
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl
    ));
    assert!(Job::due(&mut scheduled, ctx, now).is_none());
    assert!(scheduled.is_none());
}
