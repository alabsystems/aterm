// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn only_an_external_frontend_loses_the_config_words() {
    let words = |line: &str| {
        line.split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    for line in [
        "tippy",
        "clippy -p demo",
        "--unverified tippy",
        "+trust --config build.jobs=1 clippy",
        "--color always tippy",
    ] {
        assert!(loses_config(&words(line), Tool::Cargo), "{line}");
    }
    for line in [
        "",
        "--unverified",
        "--unverified build",
        "--config build.jobs=1 test -q",
        "+trust --target-dir tippy check",
        "-- tippy",
        "fix",
        "clean",
        "--unverified verify",
        "an-alias",
    ] {
        assert!(!loses_config(&words(line), Tool::Cargo), "{line}");
    }
    assert!(loses_config(&words("-p demo"), Tool::Tippy));
}

use super::*;
use std::collections::BTreeMap;

#[test]
fn global_and_command_configs_share_one_parser_without_moving_program_words() {
    let words = |s: &str| s.split_whitespace().map(str::to_owned).collect::<Vec<_>>();
    assert_eq!(
        configs_after_subcommand(&words(
            "--config a=1 --color always --unverified test --config b=2"
        )),
        words("--color always --unverified test --config a=1 --config b=2")
    );
    assert_eq!(
        configs_after_subcommand(&words("--config=a=1 --unverified test --config b=2")),
        words("--unverified test --config=a=1 --config b=2")
    );
    for line in [
        "--config a=1 --unverified test -p x",
        "--config a=1 --unverified --config b=2 test -Zfeature-unification",
        "--config a=1",
        "--config a=1 --unverified run -- --config b=2",
        "--config a=1 trust check --config b=2",
    ] {
        assert_eq!(configs_after_subcommand(&words(line)), words(line));
    }
}

/// The test graph's `--config` goes right before the subcommand and its flag right after it,
/// past every flag and flag value before it, once. NEGATIVE CONTROL: a line naming no
/// subcommand before `--`, or one that chose its unification already, is left as it is.
#[test]
fn the_test_graph_wraps_the_subcommand() {
    let words = |line: &str| {
        line.split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let c = TEST_GRAPH_CONFIG.join(" ");
    let f = TEST_GRAPH_FLAG;
    for (line, want) in [
        (
            "--unverified test -p x",
            format!("--unverified {c} test {f} -p x"),
        ),
        (
            "--config build.jobs=2 --unverified build -p x",
            format!("--config build.jobs=2 --unverified {c} build {f} -p x"),
        ),
        (
            "-Z unstable-options --color always run -q -- -x",
            format!("-Z unstable-options --color always {c} run {f} -q -- -x"),
        ),
        (
            "+trust --manifest-path m/Cargo.toml metadata --no-deps",
            format!("+trust --manifest-path m/Cargo.toml {c} metadata {f} --no-deps"),
        ),
        (
            "test -- -Zfeature-unification",
            format!("{c} test {f} -- -Zfeature-unification"),
        ),
    ] {
        assert_eq!(with_test_graph(&words(line)), words(&want), "{line}");
    }
    for line in [
        "",
        "--unverified",
        "--config build.jobs=2",
        "-- test",
        "--unverified test -Zfeature-unification -p x",
    ] {
        assert_eq!(with_test_graph(&words(line)), words(line), "{line}");
    }
}

/// The shared cache sits in a quiet folder fseventsd drops, still named `*.noindex`, and so
/// does every build directory under it. NEGATIVE CONTROL: the root it moved from, the same
/// name without the folder, is no quiet place.
#[test]
fn the_cache_root_is_quiet_and_noindex() {
    let home = Path::new("/Users//u");
    let root = cache_root(home);
    assert!(aterm_tempfile::is_quiet(&root), "{root:?}");
    assert!(in_noindex(&root), "{root:?}");
    assert_eq!(
        root.parent().and_then(Path::file_name),
        Some(std::ffi::OsStr::new(aterm_tempfile::QUIET))
    );
    let leaf = build_dir(&root, Path::new("/Users//u/Developer/aterm"));
    assert!(
        leaf.to_str().is_some_and(|p| p.contains("/.ubd/")),
        "{leaf:?}"
    );
    let old = moved_from(home);
    assert!(!aterm_tempfile::is_quiet(&old), "{old:?}");
    assert_eq!(old.file_name(), root.file_name());
    assert_ne!(old, root);
}

/// A shim's `__atb_c` line names the root it builds in: this version's quiet one, or the
/// one before the move. NEGATIVE CONTROL: any other root, no such line, and no shim on the
/// path leave the quiet root.
#[test]
fn the_root_plain_targo_builds_in_is_the_one_its_shim_names() {
    let home = Path::new("/Users//u");
    let shim =
        |under: &str| format!("#!/bin/sh\n# atpkg shim\n__atb_t=\n__atb_c=${{HOME-}}/{under}\n");
    assert_eq!(
        root_named_by_shim(home, &shim(MOVED_FROM_UNDER_HOME)),
        Some(moved_from(home))
    );
    assert_eq!(
        root_named_by_shim(home, &shim(CACHE_UNDER_HOME)),
        Some(cache_root(home))
    );
    assert_eq!(root_named_by_shim(home, &shim("Library/elsewhere")), None);
    assert_eq!(
        root_named_by_shim(home, "#!/bin/sh\nexec targo \"$@\"\n"),
        None
    );
    assert_eq!(shim_root(home, std::ffi::OsStr::new("")), cache_root(home));
}

#[test]
fn only_what_cargo_run_says_about_its_child_is_dropped() {
    for name in [
        "CARGO_MANIFEST_DIR",
        "CARGO_PKG_NAME",
        "CARGO_PKG_VERSION_MAJOR",
        "CARGO_BIN_NAME",
        "OUT_DIR",
    ] {
        assert!(said_by_cargo_run(name), "{name}");
    }
    // The caller's own layout and toolchain choices, and a near miss, stay.
    for name in [
        "CARGO",
        "CARGO_HOME",
        "CARGO_TARGET_DIR",
        BUILD_DIR_ENV,
        "CARGO_PKG",
        "XCARGO_PKG_NAME",
        "OUT_DIRS",
    ] {
        assert!(!said_by_cargo_run(name), "{name}");
    }
}

/// `unix`, a fixture's absolute path, as this platform spells one (`C:\w\repo` on
/// Windows, where `/w/repo` is not absolute).
fn native(unix: &str) -> String {
    if cfg!(windows) && unix.starts_with('/') {
        format!("C:{}", unix.replace('/', "\\"))
    } else {
        unix.to_owned()
    }
}

/// A machine whose environment is `env` and whose config files are `files`, every
/// absolute path spelled as this platform spells it ([`native`]).
fn decide(
    target: &str,
    cwd: Option<&str>,
    env: &[(&str, &str)],
    files: &[(&str, &str)],
) -> Option<String> {
    let env: BTreeMap<String, OsString> = env
        .iter()
        .map(|(k, v)| ((*k).to_owned(), OsString::from(native(v))))
        .collect();
    let files: BTreeMap<PathBuf, Result<String, ()>> = files
        .iter()
        .map(|(p, text)| {
            let text = if *text == "<unreadable>" {
                Err(())
            } else {
                Ok((*text).to_owned())
            };
            (PathBuf::from(native(p)), text)
        })
        .collect();
    let lookup = |k: &str| env.get(k).cloned();
    let read = |p: &Path| files.get(p).cloned().transpose();
    let writable = |_: &Path| Some(true);
    let cwd = cwd.map(native);
    let home = native("/h");
    let m = Machine {
        home: Some(Path::new(&home)),
        root: None,
        cwd: cwd.as_deref().map(Path::new),
        env: &lookup,
        read: &read,
        writable: &writable,
    };
    target_build_dir(Path::new(&native(target)), &m)
}

/// A build pinned to a target directory keeps its intermediates in
/// `<target>/build.noindex`; a caller's own build directory, an unknown working
/// directory and a path the literal cannot hold leave the layout alone.
#[test]
fn a_pinned_target_keeps_its_intermediates_out_of_spotlight() {
    let w = Some("/w/repo");
    assert_eq!(
        decide("/w/repo/target", w, &[], &[]),
        Some(format!(
            "build.build-dir='{}'",
            native("/w/repo/target/build.noindex")
        ))
    );
    assert!(
        decide(
            "/t",
            w,
            &[],
            &[("/w/repo/.cargo/config.toml", "[build]\ntarget-dir = 'x'\n")]
        )
        .is_some()
    );
    assert!(
        decide(
            "/t",
            w,
            &[],
            &[("/w/.cargo/config.toml", "# build-dir = 'x'\n")]
        )
        .is_some()
    );
    for (why, got) in [
        ("relative", decide("target", w, &[], &[])),
        ("no cwd", decide("/t", None, &[], &[])),
        (
            "env",
            decide("/t", w, &[("CARGO_BUILD_BUILD_DIR", "")], &[]),
        ),
        (
            "ancestor config",
            decide(
                "/t",
                w,
                &[],
                &[("/w/.cargo/config.toml", "[build]\nbuild-dir = 'x'\n")],
            ),
        ),
        (
            "home config",
            decide("/t", w, &[], &[("/h/.cargo/config", "build.build-dir='x'")]),
        ),
        (
            "CARGO_HOME config",
            decide(
                "/t",
                w,
                &[("CARGO_HOME", "/ch")],
                &[("/ch/config.toml", "  build-dir = 'x'")],
            ),
        ),
        (
            "unreadable config",
            decide("/t", w, &[], &[("/w/repo/.cargo/config", "<unreadable>")]),
        ),
        ("quote", decide("/t'q", w, &[], &[])),
        ("brace", decide("/t{workspace-root}", w, &[], &[])),
        ("control", decide("/t\nx", w, &[], &[])),
    ] {
        assert_eq!(got, None, "{why}");
    }
    assert!(
        decide(
            "/t",
            w,
            &[("CARGO_HOME", "/ch")],
            &[("/h/.cargo/config.toml", "build-dir = 'x'")]
        )
        .is_some()
    );

    let dir = aterm_tempfile::Builder::new()
        .prefix("atc-read-config-")
        .tempdir()
        .expect("scratch dir");
    let cfg = dir.path().join("config.toml");
    std::fs::write(&cfg, "target-dir = 'x'\n").unwrap();
    assert_eq!(read_config(&cfg), Ok(Some("target-dir = 'x'\n".to_owned())));
    std::fs::write(&cfg, vec![b'x'; 1024 * 1024 + 1]).unwrap();
    assert_eq!(
        read_config(&cfg),
        Err(()),
        "oversized config must fail closed"
    );
    #[cfg(unix)]
    {
        let fifo = dir.path().join("fifo.toml");
        let c_fifo = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: `c_fifo` is a valid NUL-terminated path in our scratch directory.
        assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0);
        assert_eq!(
            read_config(&fifo),
            Ok(None),
            "a FIFO config must not hang or count as a regular file"
        );
    }
}
