// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn fixture() -> (aterm_tempfile::TempDir, Engine, PathBuf, PathBuf, PathBuf) {
    let tmp = aterm_tempfile::TempDir::new().expect("scratch");
    let home = canonical(tmp.path())
        .expect("absolute scratch")
        .join("home");
    let root = home.join("work");
    let cache = root.join("incremental");
    let other = home.join("other");
    std::fs::create_dir_all(&cache).expect("cache");
    std::fs::create_dir_all(&other).expect("other root");
    std::fs::write(cache.join("data"), [7; 8192]).expect("cache data");
    std::fs::write(other.join("data"), [5; 4096]).expect("other data");
    let policy = Policy::new(
        Deny::for_home(&home, &[], &[]),
        aterm_sysprobe::volumes::table(),
    );
    let (_send, inbox) = std::sync::mpsc::channel();
    let mut engine = Engine::new(inbox, Arc::default(), None);
    engine.policy = Some(policy.clone());
    let now = Instant::now();
    for path in [&root, &other] {
        let mut walk = aterm_diskwalk::Walk::begin(&RootSpec::new(path), &policy).expect("walk");
        while matches!(walk.diskwalk_slice(&mut Instant::now), Step::More { .. }) {}
        let dev = walk.root_dev();
        let tree = Arc::new(walk.into_tree());
        let complete = complete_of(&tree, DirId::ROOT, now, Duration::from_secs(1000), None);
        engine.records.insert(
            path.clone(),
            Arc::new(RootRecord {
                state: RootState::Complete,
                id: Some((dev, tree.root().ino)),
                complete: Some(complete),
                ..RootRecord::queued(path.clone())
            }),
        );
        engine.kept.trees.insert(path.clone(), tree);
        engine
            .kept
            .history
            .insert(path.clone(), VecDeque::from([(now, 8192)]));
    }
    for path in [&root, &cache, &other] {
        engine.life.insert(
            path.clone(),
            Life {
                streak: 2,
                seen_in: 2,
                seen_at: now,
                promoted: true,
            },
        );
    }
    engine.rederive_nested();
    engine.dirty = false;
    assert!(
        engine.records[&root]
            .complete
            .as_ref()
            .expect("complete")
            .biggest
            .iter()
            .any(|row| row.rel == Path::new("incremental"))
    );
    (tmp, engine, root, cache, other)
}

#[test]
fn a_removed_visible_cache_invalidates_its_root_and_derived_rows_before_rewalk_is_due() {
    let (_tmp, mut engine, root, cache, other) = fixture();
    let unrelated = Arc::clone(&engine.records[&other]);
    assert_eq!(
        pick_root(
            &engine.plan(),
            &engine.records,
            &BTreeSet::new(),
            &BTreeSet::new(),
            Instant::now()
        ),
        None,
        "the expensive walk is not due"
    );
    std::fs::rename(&cache, root.join("retired")).expect("external cache reclamation");
    engine.invalidate_changed_snapshots();
    assert!(!engine.records.contains_key(&root));
    assert!(!engine.records.contains_key(&cache));
    assert!(!engine.kept.trees.contains_key(&root));
    assert!(!engine.kept.history.contains_key(&root));
    assert!(Arc::ptr_eq(&engine.records[&other], &unrelated));
    assert_eq!(
        pick_root(
            &engine.plan(),
            &engine.records,
            &BTreeSet::new(),
            &BTreeSet::new(),
            Instant::now()
        ),
        Some(root)
    );
}

#[test]
fn a_replacement_at_the_same_path_does_not_inherit_old_sizes() {
    let (_tmp, mut engine, root, cache, _other) = fixture();
    let before = directory_identity(&cache, engine.policy.as_ref().expect("policy")).expect("id");
    std::fs::rename(&cache, root.join("retired")).expect("keep old inode alive");
    std::fs::create_dir(&cache).expect("replacement");
    let after = directory_identity(&cache, engine.policy.as_ref().expect("policy")).expect("id");
    assert_ne!(before, after);
    engine.invalidate_changed_snapshots();
    assert!(!engine.records.contains_key(&root));
    assert!(!engine.records.contains_key(&cache));
}

#[test]
fn an_unchanged_snapshot_keeps_its_rewalk_backoff() {
    let (_tmp, mut engine, root, cache, _other) = fixture();
    let before = Arc::clone(&engine.records[&root]);
    engine.invalidate_changed_snapshots();
    assert!(Arc::ptr_eq(&engine.records[&root], &before));
    assert!(engine.records.contains_key(&cache));
    assert!(!engine.dirty);
}
