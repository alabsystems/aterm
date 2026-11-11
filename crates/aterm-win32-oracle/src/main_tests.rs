// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// A checkout whose marker files are symlinks to regular files is still the root, as
/// `Path::is_file` follows them; one missing a marker is not.
#[cfg(unix)]
#[test]
fn a_root_whose_markers_are_symlinks_is_found() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("checkout");
    let real = dir.path().join("real");
    std::fs::create_dir_all(&real).expect("real");
    for rel in ["win32-oracle/Cargo.toml", "crates/aterm-win32/Cargo.toml"] {
        let target = real.join(rel.replace('/', "-"));
        std::fs::write(&target, "").expect("write");
        let link = root.join(rel);
        std::fs::create_dir_all(link.parent().expect("parent")).expect("dirs");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
    }
    let start = root.join("crates/aterm-win32");
    assert_eq!(find_root(&start), Some(root.clone()));
    std::fs::remove_file(root.join("win32-oracle/Cargo.toml")).expect("remove");
    assert_eq!(find_root(&start), None);
}
#[test]
fn recorded_stream_is_strict_and_keeps_all_answer_kinds() {
    let r = parse_record("layout\tA\t8\t4\noffset\tA\tx\t0\nvalue\tB\t-1\ncstr\tC\t1.2\n").unwrap();
    assert_eq!(r["A"]["offsets"]["x"].as_u64(), Some(0));
    assert_eq!(r["B"]["value"].as_str(), Some("-1"));
    assert_eq!(r["C"]["cstr"].as_str(), Some("1.2"));
    for bad in ["garbage", "offset\tA\tx\t0", "value\tB\t1\nvalue\tB\t2"] {
        assert!(parse_record(bad).is_err(), "{bad}")
    }
}

/// A build directory the oracle makes carries cargo's cache-directory tag, which stock cargo's
/// `clean` requires, and one already there is kept. NEGATIVE CONTROL: `hold_target` creating
/// the directory alone (no `make_target`) fails "the tag cargo's clean requires".
#[test]
fn a_build_directory_the_oracle_makes_is_one_cargo_will_clean() {
    let dir = aterm_tempfile::tempdir().expect("scratch checkout");
    let target = dir.path().join("target.noindex");
    drop(
        hold_target(&target, "oracle", dir.path(), &mut |line| panic!("{line}"))
            .expect("a free directory"),
    );
    let tag = target.join("CACHEDIR.TAG");
    assert_eq!(
        std::fs::read_to_string(&tag).expect("the tag cargo's clean requires"),
        CACHEDIR_TAG
    );
    assert!(CACHEDIR_TAG.starts_with("Signature: 8a477f597d28d172789f06886806bc55"));
    std::fs::write(
        &tag,
        "Signature: 8a477f597d28d172789f06886806bc55\n# cargo's own\n",
    )
    .expect("rewrite");
    make_target(&target).expect("again");
    assert!(
        std::fs::read_to_string(&tag)
            .expect("tag")
            .ends_with("# cargo's own\n"),
        "a tag already there is kept"
    );
}

/// A second run on one build directory waits for the first, naming it, and has the directory
/// once the first lets go (WB2-2). Negative control: without the lock the second run returns
/// at once, naming nobody, and the channel closes before any line.
#[test]
fn a_second_run_on_one_build_directory_waits_for_the_first() {
    const HANG: std::time::Duration = std::time::Duration::from_secs(60);
    let dir = aterm_tempfile::tempdir().expect("scratch checkout");
    let target = dir.path().join("target.noindex");
    let first = hold_target(&target, "oracle", dir.path(), &mut |line| {
        panic!("a free directory is taken at once, not waited for: {line}")
    })
    .expect("a free directory");
    let (said, heard) = std::sync::mpsc::channel();
    let second = {
        let (target, root) = (target.clone(), dir.path().to_path_buf());
        std::thread::spawn(move || {
            hold_target(&target, "oracle-052", &root, &mut |line| {
                let _ = said.send(line.to_string());
            })
            .map(drop)
        })
    };
    let line = heard
        .recv_timeout(HANG)
        .expect("the second run names the first before it has the directory");
    assert!(
        line.contains(&format!("pid {}", std::process::id()))
            && line.contains("aterm-win32-oracle oracle")
            && line.contains(&target.display().to_string()),
        "{line}"
    );
    assert!(!second.is_finished(), "it waits while the first holds it");
    drop(first);
    second
        .join()
        .expect("the waiter")
        .expect("the second run has the directory once the first lets go");
}

#[test]
fn the_stock_toolchains_llvm_nm_is_its_llvm_tools_one() {
    let sysroot = aterm_tempfile::tempdir().expect("scratch sysroot");
    let rustc = sysroot.path().join("bin").join("rustc");
    std::fs::create_dir_all(rustc.parent().unwrap()).unwrap();
    std::fs::write(&rustc, b"").unwrap();
    let rustlib = sysroot.path().join("lib").join("rustlib");
    std::fs::create_dir_all(rustlib.join("x86_64-pc-windows-msvc").join("lib")).unwrap();
    assert_eq!(toolchain_llvm_nm(&rustc), None, "no llvm-tools component");
    let name = if cfg!(windows) {
        "llvm-nm.exe"
    } else {
        "llvm-nm"
    };
    let host_bin = rustlib.join("aarch64-unknown-linux-gnu").join("bin");
    std::fs::create_dir_all(&host_bin).unwrap();
    std::fs::write(host_bin.join(name), b"").unwrap();
    assert_eq!(toolchain_llvm_nm(&rustc), Some(host_bin.join(name)));
}
