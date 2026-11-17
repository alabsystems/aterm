// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! KEYED TREES end to end (DC-16b): `atpkg-publish pack-tree` packs a build tree whole under
//! the key its signed inputs hash to, the index lists the key, the client fetches it by key,
//! verifies the signed pair under the roster and installs it under that key alone
//! (`atpkg::keyed_tree`), and the store hands it out as copy-on-write clones.

#![cfg(unix)]

use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use atpkg::keyed_tree::{self, TreeError, TreeKey};
use atpkg::store::Layout;
use atpkg_publish::api::{Baseline, Delta, IndexRequest, Signing};
use atpkg_publish::artifact::tarwrite::RawFrames;
use atpkg_publish::artifact::tree::{TreeOutcome, TreeRequest, pack_tree_with};
use atpkg_publish::ctx::Ctx;
use atpkg_publish::host::fake::FakeHost;
use atpkg_publish::testkit::Keys;
use atpkg_publish::triple::AARCH64_APPLE_DARWIN;

use super::fixture::{client_verifies, put, put_exe, scratch};

const TARGET: &str = "aarch64-apple-darwin";

/// What the fixture LLVM build recorded it was built from (its `.llvm-stamp`).
const STAMP: &str = "llvm 20.1.8 assertions=false host=aarch64-apple-darwin";

/// Key inputs naming the tree's stamp, varied by `variant`: each variant is another key.
fn inputs(variant: &str) -> String {
    format!("stamp = \"{STAMP}\"\nsdk = \"macosx27.0\"\nvariant = \"{variant}\"\n")
}

/// The key of [`inputs`]`(variant)`.
fn key(variant: &str) -> TreeKey {
    TreeKey::of_inputs(inputs(variant).as_bytes())
}

/// A signing context whose roster the channel head carries.
fn signing_ctx(home: &Path) -> Ctx {
    let keys = Keys::mint("m18", &[]);
    let host = Arc::new(FakeHost::new());
    let ctx = keys.ctx(home, host.clone());
    keys.publish_quad(
        &host,
        &ctx.index_slug(),
        41,
        &keys.index_toml(41, &[("ay", "ay")], &[("ay", 9)]),
    );
    ctx
}

/// An LLVM-shaped build tree: a tool, a large library and a versioned link to it, a header.
fn llvm_tree(root: &Path) -> PathBuf {
    let tree = root.join("build/aarch64-apple-darwin/llvm");
    put_exe(&tree.join("bin/llvm-config"), b"#!/bin/sh\necho 20.1.8\n");
    put(&tree.join("lib/libLLVM.dylib"), &vec![0x5a; 1 << 20]);
    std::os::unix::fs::symlink("libLLVM.dylib", tree.join("lib/libLLVM-20.dylib"))
        .expect("symlink");
    put(
        &tree.join("include/llvm/Config/llvm-config.h"),
        b"#define LLVM_VERSION_MAJOR 20\n",
    );
    put(&tree.join(".llvm-stamp"), format!("{STAMP}\n").as_bytes());
    tree
}

fn request(tree: &Path, variant: &str, out: &Path) -> TreeRequest {
    let key_inputs = out.with_extension("inputs");
    std::fs::create_dir_all(key_inputs.parent().expect("parent")).expect("mkdir");
    std::fs::write(&key_inputs, inputs(variant)).expect("write inputs");
    TreeRequest {
        program: "llvm-tree".into(),
        tree: tree.to_path_buf(),
        key: key(variant).to_string(),
        key_inputs,
        version: "llvm-20.1.8".into(),
        build: 3,
        triple: AARCH64_APPLE_DARWIN,
        out: out.to_path_buf(),
        signing_key: None,
        signing: Signing::MachineKey,
        upload: false,
    }
}

fn pack(ctx: &Ctx, tree: &Path, variant: &str, out: &Path) -> TreeOutcome {
    pack_tree_with(ctx, &request(tree, variant, out), &RawFrames::WHOLE).expect("the tree packs")
}

/// The signed manifest of `packed`, as the client verifies it under the channel's roster.
fn verified(ctx: &Ctx, packed: &TreeOutcome) -> atpkg::PkgManifest {
    let sig = packed.signature.as_ref().expect("signed");
    client_verifies(ctx, &packed.manifest, sig).expect("the client verifies the pair")
}

/// Every regular file under `root`, relative, sorted.
fn files(root: &Path) -> Vec<PathBuf> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read dir").flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                out.push(path.strip_prefix(root).expect("under root").to_path_buf());
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

/// The pack is the whole tree and its one row is a keyed tree signed for its key and the inputs
/// it hashes from; the client installs it at `trees.noindex/llvm-tree/<target>/<key>` (out of
/// Spotlight's index), every file of the tree there with its bytes, a link to a file laid as a
/// hard link to it (one body in the archive, one inode in the store), and holds it under that
/// key.
#[test]
fn a_keyed_tree_packs_whole_and_installs_under_its_key() {
    let tmp = scratch("keyed-tree-pack");
    let ctx = signing_ctx(&tmp.path().join("home"));
    let tree = llvm_tree(tmp.path());
    let k = key("a");
    let packed = pack(&ctx, &tree, "a", &tmp.path().join("out"));
    let pkg = verified(&ctx, &packed);
    assert!(pkg.exposes.is_empty(), "a tree shims no tool");
    let row = pkg.artifact_for(TARGET).expect("one row");
    assert_eq!(
        (row.kind.as_str(), row.key.as_str(), row.key_inputs.as_str()),
        (keyed_tree::KIND, k.as_str(), inputs("a").as_str())
    );

    let layout = Layout {
        prefix: tmp.path().join("pkg"),
    };
    let at = keyed_tree::install(&layout, &pkg, &k, TARGET, &packed.asset).expect("installs");
    assert_eq!(
        at,
        layout
            .prefix
            .join("trees.noindex/llvm-tree")
            .join(TARGET)
            .join(k.as_str())
    );
    assert!(keyed_tree::holds(&layout, "llvm-tree", TARGET, &k));
    let (lib, alias) = (
        std::fs::metadata(at.join("lib/libLLVM.dylib")).expect("lib"),
        std::fs::metadata(at.join("lib/libLLVM-20.dylib")).expect("alias"),
    );
    assert_eq!(
        (lib.ino(), lib.nlink()),
        (alias.ino(), 2),
        "the link is one inode in the store, not a second copy"
    );
    let want = files(&tree);
    assert_eq!(files(&at), want, "the whole tree, and nothing else");
    for rel in &want {
        assert_eq!(
            std::fs::read(at.join(rel)).expect("installed file"),
            std::fs::read(tree.join(rel)).expect("tree file"),
            "{}",
            rel.display()
        );
    }
    let exe = std::fs::metadata(at.join("bin/llvm-config")).expect("tool");
    assert_eq!(exe.permissions().mode() & 0o111, 0o111, "its mode with it");
}

/// The store never answers for one key with the tree of another: a lookup and a hand-out of
/// another key find nothing, the pack is refused under any key but the one it is signed for
/// (in both directions), and the key cannot be rewritten without the signature refusing.
#[test]
fn a_keyed_tree_of_another_key_is_not_served() {
    let tmp = scratch("keyed-tree-key");
    let ctx = signing_ctx(&tmp.path().join("home"));
    let tree = llvm_tree(tmp.path());
    let (a, b) = (key("a"), key("b"));
    let packed_a = pack(&ctx, &tree, "a", &tmp.path().join("out-a"));
    let pkg_a = verified(&ctx, &packed_a);
    let layout = Layout {
        prefix: tmp.path().join("pkg"),
    };
    keyed_tree::install(&layout, &pkg_a, &a, TARGET, &packed_a.asset).expect("installs");

    assert!(!keyed_tree::holds(&layout, "llvm-tree", TARGET, &b));
    let dest = tmp.path().join("worktree-b/llvm");
    std::fs::create_dir_all(dest.parent().expect("parent")).expect("mkdir");
    assert!(matches!(
        keyed_tree::hand_out(&layout, "llvm-tree", TARGET, &b, &dest),
        Err(TreeError::Absent { .. })
    ));
    assert!(!dest.exists(), "nothing is laid for a key the store lacks");

    assert!(matches!(
        keyed_tree::install(&layout, &pkg_a, &b, TARGET, &packed_a.asset),
        Err(TreeError::OtherKey { .. })
    ));
    assert!(!keyed_tree::holds(&layout, "llvm-tree", TARGET, &b));
    assert!(
        !keyed_tree::dir(
            &layout,
            keyed_tree::program("llvm-tree").expect("row"),
            TARGET,
            &b
        )
        .exists()
    );

    let packed_b = pack(&ctx, &tree, "b", &tmp.path().join("out-b"));
    let pkg_b = verified(&ctx, &packed_b);
    let fresh = Layout {
        prefix: tmp.path().join("pkg-fresh"),
    };
    assert!(matches!(
        keyed_tree::install(&fresh, &pkg_b, &a, TARGET, &packed_b.asset),
        Err(TreeError::OtherKey { .. })
    ));
    assert!(!keyed_tree::holds(&fresh, "llvm-tree", TARGET, &a));

    let text = std::fs::read_to_string(&packed_a.manifest).expect("manifest");
    let forged = tmp.path().join("forged.toml");
    std::fs::write(&forged, text.replace(a.as_str(), b.as_str())).expect("write");
    let sig = packed_a.signature.as_ref().expect("signed");
    assert!(
        client_verifies(&ctx, &forged, sig).is_err(),
        "the key is inside the signed bytes"
    );
}

/// Where on its device the byte at offset 0 of `path` lies (`fcntl(F_LOG2PHYS_EXT)`): two files
/// that share it share their first block, as only a clone does.
#[cfg(target_os = "macos")]
fn device_offset(path: &Path) -> i64 {
    use std::os::fd::AsRawFd as _;

    /// `<sys/fcntl.h>`: map a file offset to a device offset.
    const F_LOG2PHYS_EXT: i32 = 65;

    /// `<sys/fcntl.h>`'s `struct log2phys`, under its `#pragma pack(4)`.
    #[repr(C, packed(4))]
    struct Log2Phys {
        flags: u32,
        contigbytes: i64,
        devoffset: i64,
    }

    unsafe extern "C" {
        /// `<fcntl.h>`: `int fcntl(int fildes, int cmd, ...)`.
        fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    }

    let file = std::fs::File::open(path).expect("open");
    aterm_durable::durable(&file).expect("flush, so the file has blocks");
    let mut l2p = Log2Phys {
        flags: 0,
        contigbytes: 4096,
        devoffset: 0,
    };
    // SAFETY: the descriptor is open for the call, and `l2p` is the struct the command
    // reads and writes, alive and exclusively borrowed across it.
    let rc = unsafe { fcntl(file.as_raw_fd(), F_LOG2PHYS_EXT, &raw mut l2p) };
    assert_eq!(rc, 0, "F_LOG2PHYS_EXT: {}", std::io::Error::last_os_error());
    l2p.devoffset
}

/// A hand-out is every file of the store's tree as a clone: its bytes, mode and time on an inode
/// of its own (no hard link), sharing the store's blocks on macOS (no byte copy), and a write to
/// it leaves the store's tree as it was. A destination that exists is refused.
#[test]
fn a_keyed_tree_is_handed_out_as_clones() {
    let tmp = scratch("keyed-tree-clone");
    let ctx = signing_ctx(&tmp.path().join("home"));
    let tree = llvm_tree(tmp.path());
    let k = key("c");
    let packed = pack(&ctx, &tree, "c", &tmp.path().join("out"));
    let pkg = verified(&ctx, &packed);
    let layout = Layout {
        prefix: tmp.path().join("pkg"),
    };
    let store = keyed_tree::install(&layout, &pkg, &k, TARGET, &packed.asset).expect("installs");

    let dest = tmp.path().join("worktree/build/aarch64-apple-darwin/llvm");
    std::fs::create_dir_all(dest.parent().expect("parent")).expect("mkdir");
    let laid = keyed_tree::hand_out(&layout, "llvm-tree", TARGET, &k, &dest).expect("handed out");
    let want = files(&store);
    assert_eq!(files(&dest), want);
    assert_eq!(laid.files, u64::try_from(want.len()).expect("count"));
    for rel in &want {
        let (s, d) = (store.join(rel), dest.join(rel));
        let (sm, dm) = (
            std::fs::symlink_metadata(&s).expect("store"),
            std::fs::symlink_metadata(&d).expect("clone"),
        );
        assert!(dm.is_file(), "{}", rel.display());
        assert_ne!(sm.ino(), dm.ino(), "{} is its own inode", rel.display());
        assert_eq!(sm.permissions(), dm.permissions(), "{}", rel.display());
        assert_eq!(sm.modified().ok(), dm.modified().ok(), "{}", rel.display());
        assert_eq!(std::fs::read(&s).ok(), std::fs::read(&d).ok());
    }
    #[cfg(target_os = "macos")]
    assert_eq!(
        device_offset(&dest.join("lib/libLLVM.dylib")),
        device_offset(&store.join("lib/libLLVM.dylib")),
        "the hand-out shares the store's blocks: a clone, not a copy"
    );

    let lib = dest.join("lib/libLLVM.dylib");
    let mut perms = std::fs::metadata(&lib).expect("lib").permissions();
    perms.set_mode(0o644);
    std::fs::set_permissions(&lib, perms).expect("chmod");
    std::fs::write(&lib, b"rebuilt").expect("a worktree writes its clone");
    assert_eq!(
        std::fs::read(store.join("lib/libLLVM.dylib")).expect("store"),
        vec![0x5a; 1 << 20],
        "the store's tree is untouched"
    );

    assert!(matches!(
        keyed_tree::hand_out(&layout, "llvm-tree", TARGET, &k, &dest),
        Err(TreeError::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists
    ));
}

/// A key is bound to what it names before anything is signed: inputs that do not hash to the
/// key, inputs that do not name the tree's own `.llvm-stamp`, a tree with no stamp (a build that
/// never finished), and an `--out` inside the tree are each refused, and no manifest is written.
#[test]
fn pack_tree_refuses_a_key_its_inputs_and_stamp_do_not_make() {
    let tmp = scratch("keyed-tree-inputs");
    let ctx = signing_ctx(&tmp.path().join("home"));
    let tree = llvm_tree(tmp.path());
    let refused = |request: &TreeRequest| {
        pack_tree_with(&ctx, request, &RawFrames::WHOLE)
            .expect_err("refused")
            .to_string()
    };

    let mut other_key = request(&tree, "a", &tmp.path().join("out-key"));
    other_key.key = key("b").to_string();
    assert!(
        refused(&other_key).contains("hashes to"),
        "inputs of another key"
    );

    let unnamed = request(&tree, "a", &tmp.path().join("out-stamp"));
    std::fs::write(&unnamed.key_inputs, "variant = \"a\"\n").expect("write");
    let mut unnamed = unnamed;
    unnamed.key = TreeKey::of_inputs(b"variant = \"a\"\n").to_string();
    assert!(
        refused(&unnamed).contains(".llvm-stamp"),
        "inputs that skip the stamp"
    );

    let unfinished = tmp.path().join("unfinished");
    put(&unfinished.join("lib/libLLVM.dylib"), b"half");
    let no_stamp = request(&unfinished, "a", &tmp.path().join("out-unfinished"));
    assert!(refused(&no_stamp).contains("only a build that finished"));

    let inside = request(&tree, "a", &tree.join("out"));
    assert!(refused(&inside).contains("would pack itself"));

    for out in ["out-key", "out-stamp", "out-unfinished"] {
        assert!(
            !tmp.path().join(out).join("llvm-tree").exists(),
            "{out}: nothing staged"
        );
    }
}

/// An install interrupted mid-stage leaves its scratch beside the tree's name; the next install
/// of that key, under the tree's lock, removes it and lays the tree.
#[test]
fn tree_install_sweeps_an_interrupted_installs_scratch() {
    let tmp = scratch("keyed-tree-scratch");
    let ctx = signing_ctx(&tmp.path().join("home"));
    let tree = llvm_tree(tmp.path());
    let k = key("a");
    let packed = pack(&ctx, &tree, "a", &tmp.path().join("out"));
    let pkg = verified(&ctx, &packed);
    let layout = Layout {
        prefix: tmp.path().join("pkg"),
    };
    let at = keyed_tree::dir(
        &layout,
        keyed_tree::program("llvm-tree").expect("row"),
        TARGET,
        &k,
    );
    let left = at.with_file_name(format!("{k}.incoming-4242"));
    let other = at.with_file_name(format!("{}.incoming-4242", key("b")));
    put(&left.join("lib/half"), b"half");
    std::fs::create_dir_all(&other).expect("mkdir");

    keyed_tree::install(&layout, &pkg, &k, TARGET, &packed.asset).expect("installs");

    assert!(!left.exists(), "this key's scratch goes");
    assert!(
        other.exists(),
        "another key's stays: its own install owns it"
    );
    assert!(keyed_tree::holds(&layout, "llvm-tree", TARGET, &k));
}

/// The signed index lists a packed key (`index --tree`), and a client with no tree of it fetches
/// it by key alone: the build the index names, its manifest verified under the index's roster and
/// its archive fetched, then installed under the key. A key the index does not list is refused
/// with nothing laid.
#[test]
fn tree_fetch_takes_the_pack_the_signed_index_lists_for_its_key() {
    let tmp = scratch("keyed-tree-fetch");
    let ctx = signing_ctx(&tmp.path().join("home"));
    let tree = llvm_tree(tmp.path());
    let k = key("a");
    let packed = pack(&ctx, &tree, "a", &tmp.path().join("out"));

    let quad_dir = tmp.path().join("quad");
    let mut delta = Delta::default();
    delta.trees.insert(
        "llvm-tree".into(),
        std::collections::BTreeMap::from([(k.to_string(), 3)]),
    );
    let request = IndexRequest {
        baseline: Baseline::ChannelHead,
        delta,
        out: quad_dir.clone(),
        upload: false,
        pack_dir: None,
    };
    let mut log = atpkg_publish::channel::Log::capture("test");
    atpkg_publish::channel::index::publish_with(&ctx, &request, &mut log)
        .unwrap_or_else(|e| panic!("{e}\n{}", log.text()));
    let quad = atpkg_publish::quad::verify_dir(&ctx, &quad_dir).expect("the quad verifies");
    assert_eq!(quad.index.tree_build("llvm-tree", k.as_str()), Some(3));

    let served = tmp.path().join("served");
    std::fs::create_dir_all(&served).expect("mkdir");
    for file in [
        &packed.manifest,
        packed.signature.as_ref().expect("signed"),
        &packed.asset,
    ] {
        std::fs::copy(file, served.join(file.file_name().expect("name"))).expect("serve");
    }
    let fetcher = atpkg::DirFetcher::new(served);
    let layout = Layout {
        prefix: tmp.path().join("pkg"),
    };
    assert!(matches!(
        keyed_tree::fetch(
            &layout,
            &fetcher,
            &quad.index,
            "llvm-tree",
            TARGET,
            &key("b")
        ),
        Err(TreeError::NotPublished { .. })
    ));
    let at = keyed_tree::fetch(&layout, &fetcher, &quad.index, "llvm-tree", TARGET, &k)
        .expect("fetched by key");
    assert!(keyed_tree::holds(&layout, "llvm-tree", TARGET, &k));
    assert_eq!(files(&at), files(&tree));
    assert!(
        !keyed_tree::holds(&layout, "llvm-tree", TARGET, &key("b")),
        "nothing laid for the unlisted key"
    );
}
