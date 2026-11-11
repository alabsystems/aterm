// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// `build.rustc` is read the way cargo's config names it: under `[build]`, or dotted at the
/// top level; never a wrapper, never another table's key.
#[test]
fn build_rustc_is_read_from_the_build_table_only() {
    assert_eq!(
        build_rustc_in("[build]\nrustc = \"/x/rustc\"\n").as_deref(),
        Some("/x/rustc")
    );
    assert_eq!(
        build_rustc_in("build.rustc = '/y/rustc' # stock\n").as_deref(),
        Some("/y/rustc")
    );
    assert_eq!(
        build_rustc_in("[build]\nrustc = \"/a/b#c\" # a comment\n").as_deref(),
        Some("/a/b#c"),
        "a # inside the quotes is the value's"
    );
    for none in [
        "[build]\nrustc-wrapper = \"sccache\"\n",
        "[target.aarch64-apple-darwin]\nrustc = \"/z\"\n",
        "# [build]\n# rustc = \"/z\"\n",
        "[build]\nrustflags = [\"-Ztrust-verify=off\"]\n",
        "[build]\nrustc = \"\"\n",
        "",
    ] {
        assert_eq!(build_rustc_in(none), None, "{none:?}");
    }
}

/// What can move targo off trustc here, in cargo's precedence: `$RUSTC`, then
/// `$CARGO_BUILD_RUSTC`, then the nearest config file's `build.rustc`, then `$CARGO_HOME`'s,
/// each file read once however it is reached, `config` over `config.toml` in one directory.
#[test]
fn what_moves_targo_off_trustc_is_read_in_cargos_order() {
    let hold = aterm_tempfile::tempdir().expect("scratch");
    let root = hold.path().canonicalize().expect("canonical");
    let repo = root.join("repo");
    let deep = repo.join("crates").join("x");
    let home = root.join("cargo-home");
    for dir in [&deep, &repo.join(".cargo"), &home] {
        std::fs::create_dir_all(dir).expect("mkdir");
    }
    let none = |_: &str| None;
    assert!(
        rustc_movers(Some(&deep), &none, Some(&home)).is_empty(),
        "nothing set, nothing moves it"
    );
    std::fs::write(
        home.join("config.toml"),
        "[build]\nrustc = \"/home/rustc\"\n",
    )
    .expect("write");
    std::fs::write(
        repo.join(".cargo").join("config.toml"),
        "[build]\nrustc = \"bin/rustc\"\n",
    )
    .expect("write");
    let env = |k: &str| (k == "RUSTC").then(|| "/env/rustc".to_string());
    let got = rustc_movers(Some(&deep), &env, Some(&home));
    let said: Vec<String> = got
        .iter()
        .map(|m| format!("{}={}", m.what, m.value))
        .collect();
    assert_eq!(
        said,
        [
            "$RUSTC=/env/rustc".to_string(),
            format!(
                "build.rustc in {}=bin/rustc",
                repo.join(".cargo/config.toml").display()
            ),
            format!(
                "build.rustc in {}=/home/rustc",
                home.join("config.toml").display()
            ),
        ]
    );
    assert_eq!(got[0].base, deep, "a variable is relative to the run");
    assert_eq!(
        got[1].base, repo,
        "a file to the directory above its `.cargo`"
    );
    assert_eq!(got[0].fix(), "unset RUSTC");
    assert_eq!(
        got[1].fix(),
        format!(
            "remove build.rustc from {}",
            repo.join(".cargo/config.toml").display()
        )
    );
    // An empty variable is unset.
    let empty = |k: &str| (k == "RUSTC").then(String::new);
    assert_eq!(rustc_movers(Some(&deep), &empty, Some(&home)).len(), 2);
    // `config` is the file cargo reads where both are.
    std::fs::write(
        repo.join(".cargo").join("config"),
        "[build]\nrustc = \"/plain/rustc\"\n",
    )
    .expect("write");
    assert_eq!(
        rustc_movers(Some(&deep), &none, Some(&home))[0].value,
        "/plain/rustc"
    );
    // $CARGO_HOME reached twice, as an ancestor's `.cargo` and as itself, is one row.
    let nested_home = repo.join(".cargo");
    assert_eq!(
        rustc_movers(Some(&deep), &none, Some(&nested_home)).len(),
        1
    );
}

/// A mover resolves as cargo resolves it in the child: a bare name on the child's `PATH`, a
/// path with a separator against its base; and only a first mover that answers the lock's
/// commit leaves the run on the lock's compiler.
#[cfg(unix)]
#[test]
fn the_first_mover_is_held_to_the_lock() {
    let hold = aterm_tempfile::tempdir().expect("scratch");
    let root = hold.path().canonicalize().expect("canonical");
    let rustc = |dir: &Path, commit: &str| {
        std::fs::create_dir_all(dir).expect("mkdir");
        let path = dir.join("rustc");
        crate::test_script::script(
            &path,
            &format!("#!/bin/sh\nprintf 'commit-hash: {commit}\\n'\n"),
        );
        path
    };
    let locked = "a".repeat(40);
    let other = "b".repeat(40);
    let on_path = root.join("locked-bin");
    rustc(&on_path, &locked);
    rustc(&root.join("repo/bin"), &other);
    let lock = TrustLock {
        build: 16387,
        tree_root: "4".repeat(64),
        compiler_commit: locked.clone(),
        receipt: "SEED-MINT sha256:00".to_string(),
        stock_toolchain: "1.99.1".to_string(),
    };
    let lock_path = root.join("repo/trust.lock");
    let path_env = on_path.as_os_str();
    let mover = |what: &str, value: &str| Mover {
        what: what.to_string(),
        value: value.to_string(),
        base: root.join("repo"),
    };

    assert_eq!(lock_refusal(&[], path_env, &lock_path, &lock), None);
    assert_eq!(
        lock_refusal(&[mover("$RUSTC", "rustc")], path_env, &lock_path, &lock),
        None,
        "a bare name is the child's PATH's: the locked build's own"
    );
    let why = lock_refusal(&[mover("$RUSTC", "bin/rustc")], path_env, &lock_path, &lock)
        .expect("a relative path is the base's: another compiler");
    assert!(
        why.contains(&format!("answers commit {other}")) && why.contains("Fix: unset RUSTC"),
        "{why}"
    );
    let why = lock_refusal(
        &[
            mover("build.rustc in /x/.cargo/config.toml", "/nowhere/rustc"),
            mover("$RUSTC", "rustc"),
        ],
        path_env,
        &lock_path,
        &lock,
    )
    .expect("the first decides");
    assert!(
        why.contains("names no executable")
            && why.contains("Fix: remove build.rustc from /x/.cargo/config.toml"),
        "{why}"
    );
}
