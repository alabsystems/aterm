// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::appkit_content::{Prepared, REDRAW_GROUP};
use crate::exec::Cmd;
use crate::{Ctx, EnvSnapshot, Scope};

fn kept(prefix: &str) -> PathBuf {
    aterm_tempfile::Builder::new()
        .prefix(prefix)
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .unwrap()
        .keep()
}

fn fixture() -> (
    PathBuf,
    Vec<u8>,
    PathBuf,
    Ctx,
    std::collections::BTreeMap<&'static str, String>,
) {
    let root = kept("appkit-image-source-");
    let source = root.join(crate::stages::REDRAW_CONFORMANCE_BIN);
    let bytes = std::fs::read(aterm_tempfile::bin_exe("aterm-verify-artifact-fixture")).unwrap();
    std::fs::write(&source, &bytes).unwrap();
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o700)).unwrap();
    let cache = kept("appkit-image-cache-");
    let ctx = Ctx::new(
        root.clone(),
        Scope::workspace(),
        EnvSnapshot::default(),
        root.join("scratch"),
    );
    std::fs::create_dir(&ctx.scratch).unwrap();
    let manifest = ctx.root.join("crates/aterm-gui/Cargo.toml");
    let receipt = aterm_json::json!({
        "reason":"compiler-artifact", "manifest_path":manifest,
        "target":{"name":crate::stages::REDRAW_CONFORMANCE_BIN,"kind":["bin"]},
        "executable":source, "trust_compile_mode":"build", "trust_compile_kind":"host",
        "trust_compile_target":format!("{}-apple-darwin",std::env::consts::ARCH),
        "trust_unit_identity_sha256":"a".repeat(64),
        "trust_executable_sha256":crate::test_index::digest(&source).unwrap(),
        "profile":{"test":false,"debug_assertions":true}
    });
    let messages = format!(
        "{}\n{{\"reason\":\"build-finished\",\"success\":true}}\n",
        aterm_json::to_string(&receipt).unwrap()
    );
    let expected = crate::appkit_content::producer_digests(
        &[(crate::stages::REDRAW_CONFORMANCE_BIN, source.clone())],
        &messages,
        &manifest,
    )
    .unwrap();
    (source, bytes, cache, ctx, expected)
}

fn command(ctx: &Ctx, image: &Path) -> Cmd {
    let args = [image.as_os_str().to_owned(), "__snapshot_identity".into()];
    crate::exec::finalize_exact_command(
        Cmd::replayed(&args, &ctx.scratch, Vec::new()).counting_waits(),
        ctx.exec_env(),
    )
    .unwrap()
}

/// Uses the actual admitted-image preparation, finalized runtime, and production per-child
/// guard. Only the never-executed owned source copy is replaced. Any failed/uncertain attempt
/// keeps both namespaces, independent of this test binary's own root or TMPDIR guard.
#[test]
fn appkit_retained_images_survive_original_overwrite_and_refuse_corrupt_copies() {
    let (source, bytes, cache, ctx, expected) = fixture();
    let paths = [(crate::stages::REDRAW_CONFORMANCE_BIN, source.clone())];
    let images = Images::prepare_in(&cache, &paths, &expected).unwrap();
    let prepared = Prepared::from_images(images).unwrap();
    let selected = prepared.paths()[0].1.clone();
    assert_eq!(selected.file_name(), source.file_name());
    assert!(!selected.starts_with(&ctx.root));
    assert!(!selected.starts_with(&ctx.scratch));
    assert_eq!(std::fs::read(&selected).unwrap(), bytes);
    let run = command(&ctx, &selected);
    let before = prepared
        .proof(REDRAW_GROUP, &"a".repeat(64), std::slice::from_ref(&run))
        .unwrap();

    std::fs::write(&source, b"replacement is not the admitted executable").unwrap();
    let output =
        crate::stages::run_appkit_row(&ctx, "image custody", "original copy", &prepared, &run);
    assert!(output.ok, "{:?} {}", output.spawn_error, output.output);
    assert!(
        output
            .output
            .lines()
            .any(|line| line == selected.to_string_lossy()),
        "the copied child itself must identify its actual executable: {}",
        output.output
    );
    let after = prepared
        .proof(REDRAW_GROUP, &"a".repeat(64), std::slice::from_ref(&run))
        .unwrap();
    assert_eq!(
        before, after,
        "original-output replacement cannot change carry identity"
    );
    prepared.validate().unwrap();
    drop(prepared);
    assert!(
        selected.is_file(),
        "a completed execution does not retire descendant image custody"
    );

    // Damage a separate, never-attempted test-owned image. The executed positive's image is
    // left untouched, preserving the production cache's monotonic custody contract.
    let (source, _bytes, cache, ctx, expected) = fixture();
    let paths = [(crate::stages::REDRAW_CONFORMANCE_BIN, source)];
    let images = Images::prepare_in(&cache, &paths, &expected).unwrap();
    let prepared = Prepared::from_images(images).unwrap();
    let selected = prepared.paths()[0].1.clone();
    let run = command(&ctx, &selected);
    std::fs::set_permissions(&selected, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(&selected, b"corrupted private image").unwrap();
    let denied =
        crate::stages::run_appkit_row(&ctx, "image custody", "corrupt copy", &prepared, &run);
    assert!(
        denied
            .spawn_error
            .as_deref()
            .is_some_and(|s| s.contains("prepared artifact changed")),
        "{denied:?}"
    );
    assert!(denied.output.is_empty());
    assert!(denied.usage.is_none(), "refusal starts no child");
    assert!(
        prepared.validate().is_err(),
        "a prior success cannot admit the corrupt image"
    );
    assert!(
        prepared
            .proof(REDRAW_GROUP, &"a".repeat(64), std::slice::from_ref(&run))
            .is_err(),
        "a corrupt copy cannot produce a carry proof"
    );

    // Reusing a completed content key must refuse the corrupted cache; it must not replace it.
    assert!(Images::prepare_in(&cache, &paths, &expected).is_err());
    assert_eq!(
        std::fs::read(&selected).unwrap(),
        b"corrupted private image"
    );
    drop(prepared);
    assert!(
        selected.is_file(),
        "even refused images retain their published namespace"
    );
    failed_spawn_retains_its_unproved_namespace();
    let (source, _bytes, cache, _ctx, expected) = fixture();
    std::fs::remove_file(&source).unwrap();
    aterm_tempfile::copy_to_new_file(
        aterm_tempfile::bin_exe("aterm-verify-candidate-helper"),
        &source,
    )
    .unwrap();
    let wrong_origin = Images::prepare_in(
        &cache,
        &[(crate::stages::REDRAW_CONFORMANCE_BIN, source)],
        &expected,
    );
    assert!(
        wrong_origin
            .unwrap_err()
            .contains("compiler's original executable digest"),
        "a different valid Mach-O at the same Cargo name cannot impersonate the producer"
    );
    assert!(
        std::fs::read_dir(&cache).unwrap().next().is_none(),
        "a producer mismatch publishes no image or execution/carry authority"
    );
    unfinished_candidates_do_not_poison_the_content_key();
}

fn failed_spawn_retains_its_unproved_namespace() {
    let (source, _bytes, cache, ctx, expected) = fixture();
    let paths = [(crate::stages::REDRAW_CONFORMANCE_BIN, source)];
    let images = Images::prepare_in(&cache, &paths, &expected).unwrap();
    let prepared = Prepared::from_images(images).unwrap();
    let selected = prepared.paths()[0].1.clone();
    let mut run = command(&ctx, &selected);
    run.cwd = Some(ctx.root.join("missing-working-directory"));
    let failed =
        crate::stages::run_appkit_row(&ctx, "image custody", "failed spawn", &prepared, &run);
    assert!(!failed.ok && failed.spawn_error.is_some(), "{failed:?}");
    drop(prepared);
    assert!(
        selected.is_file(),
        "a failed spawn is not proof that assessment retired"
    );
}

fn unfinished_candidates_do_not_poison_the_content_key() {
    let (source, _bytes, cache, _ctx, expected) = fixture();
    let paths = [(crate::stages::REDRAW_CONFORMANCE_BIN, source)];
    let images = Images::prepare_in(&cache, &paths, &expected).unwrap();
    let selected = images.paths()[0].1.clone();
    drop(images);
    // Only the completion RECORD is removed in this fixture. Never move or remove the image.
    // Its namespace is left as an interrupted producer would leave a completed candidate.
    let content = selected.parent().unwrap().parent().unwrap();
    std::fs::remove_file(content.join("complete")).unwrap();
    let mut incomplete_entries = Vec::new();
    for n in 0..64 {
        let incomplete = content.join(format!("image-{n}"));
        match std::fs::create_dir(&incomplete) {
            Ok(()) => {
                std::fs::set_permissions(&incomplete, std::fs::Permissions::from_mode(0o700))
                    .unwrap();
                let payload = incomplete.join("not-admitted");
                std::fs::write(&payload, b"unfinished").unwrap();
                incomplete_entries.push(payload);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => panic!("legacy incomplete candidate: {e}"),
        }
    }
    let next = Images::prepare_in(&cache, &paths, &expected).unwrap();
    assert_ne!(next.paths()[0].1, selected);
    assert!(selected.is_file());
    for payload in incomplete_entries {
        assert_eq!(std::fs::read(payload).unwrap(), b"unfinished");
    }
    let reused = Images::prepare_in(&cache, &paths, &expected).unwrap();
    assert_eq!(
        reused.paths(),
        next.paths(),
        "the completed path is stable across preparations"
    );
}

#[test]
fn appkit_image_replay_retries_a_failed_completion_directory_flush() {
    use aterm_durable::seam::{EIO, FailingDirs};
    use std::os::unix::fs::MetadataExt as _;

    let (source, bytes, cache, _ctx, expected) = fixture();
    let paths = [(crate::stages::REDRAW_CONFORMANCE_BIN, source)];
    let failed = {
        // The advisory lock-holder record and copied image directory are flushed first.
        // Fail only the content-directory flush after the completion record is published.
        let _failure = FailingDirs::after(2, EIO);
        Images::prepare_in(&cache, &paths, &expected)
    };
    assert!(failed.is_err());
    let content = std::fs::read_dir(&cache)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.join("complete").is_file())
        .unwrap_or_else(|| panic!("the failed final flush left no completion record: {failed:?}"))
        .canonicalize()
        .unwrap();
    let marker = content.join("complete");
    let record = std::fs::read(&marker).unwrap();
    let name = std::str::from_utf8(&record)
        .unwrap()
        .strip_prefix("v1 ")
        .unwrap()
        .strip_suffix('\n')
        .unwrap();
    let selected = content.join(name).join(paths[0].0);
    let identity = |path: &Path| {
        let metadata = std::fs::symlink_metadata(path).unwrap();
        (metadata.dev(), metadata.ino(), metadata.mode())
    };
    let marker_identity = identity(&marker);
    let image_identity = identity(&selected);
    let membership = || {
        let mut names = std::fs::read_dir(&content)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        names.sort();
        names
    };
    let original_membership = membership();
    let untouched = || {
        assert_eq!(std::fs::read(&marker).unwrap(), record);
        assert_eq!(std::fs::read(&selected).unwrap(), bytes);
        assert_eq!(identity(&marker), marker_identity);
        assert_eq!(identity(&selected), image_identity);
        assert_eq!(membership(), original_membership);
    };
    untouched();

    let denied = {
        let _failure = FailingDirs::with(EIO);
        Images::prepare_in(&cache, &paths, &expected)
    };
    untouched();
    assert!(
        denied.is_err(),
        "persistent completion-directory EIO was accepted during image replay"
    );

    let recovered = Images::prepare_in(&cache, &paths, &expected).unwrap();
    assert_eq!(recovered.paths(), vec![(paths[0].0, selected.clone())]);
    recovered.validate().unwrap();
    untouched();
}
