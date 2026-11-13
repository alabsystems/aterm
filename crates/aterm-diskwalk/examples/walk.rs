// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A live walk of one folder on this host, for holding the walker to the
//! system's own tools: the root's sums, then each direct child folder's, with
//! the native lister (`getattrlistbulk` on macOS, `getdents64` + `statx` on
//! Linux) and the portable one (`readdir` + `fstatat`), each walked whole on
//! the real clock. Dev only; nothing ships it. On Linux this is the live check
//! of the `getdents64` arm.
//!
//! ```sh
//! targo --unverified run -q -p aterm-diskwalk --example walk -- /path/to/folder
//! ```
//!
//! Every row is `alloc=<bytes> logical=<bytes> files=<n> dirs=<n> <path>`:
//! `alloc` is the allocated size with each hard link's share (`du -s -B1`'s
//! figure when every link stays inside the folder; `du -sk` times 1024 on
//! macOS), `logical` the sum of every non-directory's size, `files` the
//! non-directories and `dirs` the folders below the row's own; the root's
//! `alloc_first_seen` counts each multiply linked file once, where its first
//! link is met, as `du` counts it. The walk
//! follows no symlink and refuses what the walker refuses (`/`, a home, a
//! protected folder): the refusal is printed and the example exits 2.

use std::path::{Path, PathBuf};
use std::time::Instant;

use aterm_diskwalk::{Deny, DirId, ListerChoice, Policy, RootSpec, Step, Walk};

fn main() {
    let Some(root) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("walk: usage: walk <folder>");
        std::process::exit(2);
    };
    let home =
        std::env::var_os("HOME").map_or_else(|| PathBuf::from("/nonexistent"), PathBuf::from);
    for lister in [ListerChoice::Native, ListerChoice::Portable] {
        let mut policy = Policy::new(
            Deny::for_home(&home, &[], &[]),
            aterm_sysprobe::volumes::table(),
        );
        policy.lister = lister;
        policy.links_out = true;
        if let Err(code) = walk(&root, &policy, lister) {
            std::process::exit(code);
        }
    }
}

fn now() -> Instant {
    #[expect(
        clippy::disallowed_methods,
        reason = "a live walk's slices are timed on the real clock, as the host's walker thread times them"
    )]
    Instant::now()
}

fn walk(root: &Path, policy: &Policy, lister: ListerChoice) -> Result<(), i32> {
    let started = now();
    let mut walk = match Walk::begin(&RootSpec::new(root), policy) {
        Ok(w) => w,
        Err(refusal) => {
            eprintln!("walk: {} refused: {refusal:?}", root.display());
            return Err(2);
        }
    };
    let mut clock = now;
    let mut slices = 0u64;
    loop {
        slices += 1;
        match walk.diskwalk_slice(&mut clock) {
            Step::More { .. } => {}
            Step::Done => break,
            other => {
                eprintln!("walk: {} stopped: {other:?}", root.display());
                return Err(1);
            }
        }
    }
    let ms = (now() - started).as_secs_f64() * 1000.0;
    println!(
        "{lister:?}: {} entries in {ms:.1} ms, {slices} slices, {} calls",
        walk.walked(),
        walk.calls()
    );
    let tree = walk.tree();
    row(&tree.root().subtree, root);
    let mut kids: Vec<DirId> = tree.children(DirId::ROOT);
    kids.sort_by_key(|id| tree.name(*id).to_vec());
    for id in kids {
        if let Some(rec) = tree.get(id) {
            row(&rec.subtree, &tree.path(id));
        }
    }
    if let Some(l) = walk.links() {
        println!(
            "  links: alloc_first_seen={} shared_out={} linked_inodes={}",
            l.alloc_first_seen, l.shared_out, l.linked_inodes
        );
    }
    let r = &tree.root().subtree;
    if r.unreadable + r.gone > 0 {
        println!("  unreadable={} gone={}", r.unreadable, r.gone);
    }
    Ok(())
}

fn row(s: &aterm_diskwalk::Sums, path: &Path) {
    println!(
        "  alloc={} logical={} files={} dirs={} {}",
        s.alloc_fair,
        s.logical,
        s.files,
        s.dirs,
        path.display()
    );
}
