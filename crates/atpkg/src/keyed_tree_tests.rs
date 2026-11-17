// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

const TARGET: &str = "aarch64-apple-darwin";

fn key(fill: char) -> TreeKey {
    TreeKey::parse(&fill.to_string().repeat(64)).expect("a key")
}

fn scratch_layout() -> (aterm_tempfile::TempDir, Layout) {
    let home = aterm_tempfile::tempdir().expect("scratch");
    let layout = Layout {
        prefix: home.path().join("pkg"),
    };
    (home, layout)
}

fn at(layout: &Layout, program: &str, key: &TreeKey) -> PathBuf {
    dir(
        layout,
        super::program(program).expect("a keyed program"),
        TARGET,
        key,
    )
}

/// Lay a complete tree of `key` for `program`, last used `age` ago.
fn laid(layout: &Layout, program: &str, key: &TreeKey, age: Duration) -> PathBuf {
    let at = at(layout, program, key);
    std::fs::create_dir_all(at.join("lib")).expect("mkdir");
    std::fs::write(at.join("lib/libLLVM.dylib"), b"llvm").expect("write");
    crate::store::mark_build_ready(&at).expect("mark ready");
    mark_used(&at).expect("mark used");
    let used = std::fs::File::options()
        .write(true)
        .open(sidecar(&at, USED_SUFFIX).expect("sidecar"))
        .expect("open");
    used.set_modified(SystemTime::now() - age).expect("age it");
    at
}

#[test]
fn a_key_is_64_lowercase_hex() {
    assert!(TreeKey::parse(&"a".repeat(64)).is_some());
    for bad in [
        "",
        &"a".repeat(63),
        &"A".repeat(64),
        &"g".repeat(64),
        &"a".repeat(65),
    ] {
        assert!(TreeKey::parse(bad).is_none(), "{bad:?}");
    }
}

/// GC evicts a tree no hand-out used for [`KEEP`], keeps one used within it, and keeps each
/// program's most recently used tree per target however old.
#[test]
fn gc_evicts_idle_trees_but_each_programs_latest() {
    let (_home, layout) = scratch_layout();
    let day = Duration::from_secs(86_400);
    let stale = laid(&layout, "llvm-tree", &key('a'), KEEP + day * 2);
    let fresh = laid(&layout, "llvm-tree", &key('b'), day);
    let newest = laid(&layout, "llvm-tree", &key('c'), Duration::ZERO);
    let lone = laid(&layout, "trust-stage0", &key('d'), KEEP * 3);

    let out = sweep_idle(&layout, SystemTime::now(), KEEP, &|_| true);

    assert_eq!(
        out.evicted,
        vec![("llvm-tree".to_string(), vec![key('a').to_string()])]
    );
    assert!(out.interrupted.is_empty());
    assert!(!stale.exists(), "the idle tree is evicted");
    assert!(
        !holds(&layout, "llvm-tree", TARGET, &key('a')),
        "and reads as not installed"
    );
    assert!(fresh.is_dir() && newest.is_dir(), "used within the window");
    assert!(lone.is_dir(), "a program's one tree stays however old");
}

/// A tree whose hand-out holds its lock is not evicted under it.
#[test]
fn gc_leaves_a_tree_being_handed_out() {
    let (_home, layout) = scratch_layout();
    let day = Duration::from_secs(86_400);
    let held = laid(&layout, "llvm-tree", &key('a'), KEEP + day);
    laid(&layout, "llvm-tree", &key('b'), Duration::ZERO);
    let _hand_out = lock(&held, Duration::ZERO).expect("lock");

    assert_eq!(
        sweep_idle(&layout, SystemTime::now(), KEEP, &|_| true),
        Sweep::default()
    );
    assert!(held.is_dir());
}

/// What an interrupted install or fetch left — a tree with no ready marker, stage scratch, a
/// download — goes with gc, for a key no install asks for again; what an install holds the lock
/// of at that moment stays.
#[test]
fn gc_removes_what_interrupted_tree_installs_left() {
    let (_home, layout) = scratch_layout();
    let unmarked = at(&layout, "llvm-tree", &key('a'));
    std::fs::create_dir_all(unmarked.join("lib")).expect("mkdir");
    let staged =
        at(&layout, "llvm-tree", &key('b')).with_file_name(format!("{}.incoming-4242", key('b')));
    std::fs::create_dir_all(staged.join("lib")).expect("mkdir");
    let download = at(&layout, "trust-stage0", &key('c'))
        .with_file_name(format!("{}.download-4242", key('c')));
    std::fs::create_dir_all(download.parent().expect("parent")).expect("mkdir");
    std::fs::write(&download, b"half an archive").expect("write");
    let installing = at(&layout, "llvm-tree", &key('d'));
    std::fs::create_dir_all(&installing).expect("mkdir");
    let _install = lock(&installing, Duration::ZERO).expect("lock");

    let out = sweep_idle(&layout, SystemTime::now(), KEEP, &|_| true);

    assert_eq!(
        out.interrupted,
        vec![
            (
                "llvm-tree".to_string(),
                vec![key('a').to_string(), key('b').to_string()]
            ),
            ("trust-stage0".to_string(), vec![key('c').to_string()]),
        ]
    );
    assert!(out.evicted.is_empty());
    assert!(!unmarked.exists() && !staged.exists() && !download.exists());
    assert!(
        installing.is_dir(),
        "an install that holds its lock keeps its tree"
    );
}

/// The manifest parser admits a key and its inputs only on a keyed tree's row, and a keyed tree
/// only with a key its signed inputs hash to.
#[test]
fn a_key_is_signed_only_on_a_keyed_tree_row_with_its_inputs() {
    let inputs = "llvm = \"20.1.8\"\nconfig = \"[llvm] assertions = false\"\n";
    let good = TreeKey::of_inputs(inputs.as_bytes());
    let row = |kind: &str, key: &str, inputs: &str| {
        format!(
            "schema = 1\nprogram = \"llvm-tree\"\nbuild_number = 1\nexposes = []\n\n\
             [[artifact]]\ntarget = \"aarch64-apple-darwin\"\nkind = \"{kind}\"\n\
             asset = \"llvm-tree-1.tar.zst\"\nsha256 = \"{}\"\nsize = 1\nkey = \"{key}\"\n\
             key_inputs = {inputs:?}\n",
            "0".repeat(64)
        )
    };
    let parses = |text: String| {
        crate::manifest::parse_pkg(&crate::sig::testkit::machine_signed(text.into_bytes())).is_ok()
    };
    assert!(parses(row(KIND, good.as_str(), inputs)));
    assert!(!parses(row(KIND, "", inputs)), "a keyed tree with no key");
    assert!(
        !parses(row(KIND, good.as_str(), "")),
        "a key with no inputs"
    );
    assert!(
        !parses(row(KIND, &"a".repeat(64), inputs)),
        "a key its inputs do not hash to"
    );
    assert!(
        !parses(row(KIND, &"A".repeat(64), inputs)),
        "a key that is not one"
    );
    assert!(
        !parses(row("binary", good.as_str(), inputs)),
        "a key on another kind"
    );
    assert!(!parses(row("binary", "", inputs)), "inputs on another kind");
}

/// The tree of one target is never the tree of another: the target is part of its name.
#[test]
fn a_tree_of_one_target_is_not_held_for_another() {
    let (_home, layout) = scratch_layout();
    laid(&layout, "trust-stage0", &key('a'), Duration::ZERO);
    assert!(holds(&layout, "trust-stage0", TARGET, &key('a')));
    assert!(!holds(
        &layout,
        "trust-stage0",
        "x86_64-apple-darwin",
        &key('a')
    ));
}

/// `serve-units --off` reads every tree's kept unit, and `uninstall --all` removes every tree
/// but one a hand-out holds.
#[test]
fn uninstall_all_removes_every_tree_but_one_held() {
    let (_home, layout) = scratch_layout();
    let a = laid(&layout, "llvm-tree", &key('a'), Duration::ZERO);
    let unit = sidecar(&a, crate::peers::UNIT_SUFFIX).expect("sidecar");
    std::fs::write(&unit, b"archive").expect("write");
    assert_eq!(kept_units(&layout), vec![unit]);
    let b = laid(&layout, "trust-stage0", &key('b'), Duration::ZERO);
    {
        let _held = lock(&b, Duration::ZERO).expect("lock");
        let held = remove_all(&layout).expect("removes");
        assert_eq!(held, vec![format!("trust-stage0 {}", key('b'))]);
        assert!(!a.exists() && b.is_dir());
    }
    assert!(remove_all(&layout).expect("removes").is_empty());
    assert!(!root(&layout).exists(), "the whole store of trees goes");
}

/// The disk report's reading names exactly what gc would evict, with the tree that stays.
#[test]
fn evictable_names_what_gc_would_evict() {
    let (_home, layout) = scratch_layout();
    let day = Duration::from_secs(86_400);
    laid(&layout, "llvm-tree", &key('a'), KEEP + day);
    laid(&layout, "llvm-tree", &key('b'), Duration::ZERO);
    laid(&layout, "trust-stage0", &key('c'), KEEP * 3);
    let read = evictable(&layout, SystemTime::now());
    assert_eq!(
        read.iter()
            .map(|e| (e.tree.program.name, e.tree.key.clone(), e.newest.clone()))
            .collect::<Vec<_>>(),
        vec![("llvm-tree", key('a'), key('b'))]
    );
    assert_eq!(
        sweep_idle(&layout, SystemTime::now(), KEEP, &|_| true).evicted,
        vec![("llvm-tree".to_string(), vec![key('a').to_string()])]
    );
}
