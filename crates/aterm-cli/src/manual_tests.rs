// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// `aterm help <topic> <section>` opens a section of a page that has them (`sys`, `agent`, and
/// `instructions`, its other name), refuses a word that names none with the page's list, and
/// refuses any second word on a page that has no sections, or on an unknown topic as that topic.
#[test]
fn a_section_word_opens_that_section_or_is_refused() {
    let (page, code) = render(Some("sys"), Some("wait"), None);
    assert_eq!(code, 0);
    assert!(page.starts_with("aterm help sys wait — "), "{page}");
    let (msg, code) = render(Some("sys"), Some("bogus"), None);
    assert_eq!(code, 2);
    assert!(msg.contains("aterm help sys: no section 'bogus'"), "{msg}");
    let (page, code) = render(Some("instructions"), Some("restart"), None);
    assert_eq!(code, 0);
    assert!(page.starts_with("aterm help agent restart — "), "{page}");
    let (msg, code) = render(Some("agent"), Some("bogus"), None);
    assert_eq!(code, 2);
    assert!(
        msg.contains("aterm help agent: no section 'bogus'"),
        "{msg}"
    );
    let (msg, code) = render(Some("telemetry"), Some("rows"), None);
    assert_eq!(code, 2);
    assert!(msg.contains("no sections"), "{msg}");
    let (msg, code) = render(Some("bogus"), Some("x"), None);
    assert_eq!(code, 2);
    assert!(msg.contains("unknown topic 'bogus'"), "{msg}");
}

/// `aterm help themes` (and the colour words) is the theme page, and its
/// key list is the parser's table: every key and alias the parser reads is
/// on it, each with its type, and only the required keys say so.
#[test]
fn the_themes_page_lists_every_key_the_parser_reads() {
    use aterm_types::scheme::THEME_KEYS;
    let (page, code) = render(Some("theme"), None, None);
    assert_eq!(code, 0);
    for alias in ["themes", "colors", "colours", "color-scheme"] {
        assert_eq!(
            render(Some(alias), None, None),
            (page.clone(), 0),
            "{alias}"
        );
    }
    // The list is the run of key lines under THE FORMAT, grouped under a
    // `  <group>:` line wherever the table's group changes.
    let list: Vec<&str> = page
        .lines()
        .skip_while(|line| *line != "THE FORMAT")
        .skip_while(|line| !line.ends_with(':') || line.starts_with("    "))
        .skip(1)
        .take_while(|line| line.starts_with("    ") || line.ends_with(':'))
        .collect();
    let keys: Vec<&str> = list
        .iter()
        .copied()
        .filter(|line| line.starts_with("    "))
        .collect();
    assert_eq!(keys.len(), THEME_KEYS.len(), "{keys:#?}");
    let mut groups: Vec<&str> = THEME_KEYS.iter().map(|spec| spec.group.label()).collect();
    groups.dedup();
    let headers: Vec<&str> = page
        .lines()
        .skip_while(|line| *line != "THE FORMAT")
        .filter(|line| line.starts_with("  ") && !line.starts_with("    "))
        .filter_map(|line| line.trim().strip_suffix(':'))
        .filter(|word| groups.contains(word))
        .collect();
    assert_eq!(headers, groups, "one header per group, in table order");
    for (line, spec) in keys.iter().zip(&THEME_KEYS) {
        let suffix = if spec.indexed > 0 { "<N>" } else { "" };
        for name in std::iter::once(spec.name).chain(spec.aliases.iter().copied()) {
            assert!(line.contains(&format!("{name}{suffix}")), "{line}");
        }
        assert!(line.contains(spec.value.label()), "{line}");
        assert_eq!(line.ends_with(", required"), spec.required, "{line}");
    }
    assert!(
        page.contains(&format!(
            "{} KiB",
            aterm_types::scheme::MAX_USER_THEME_FILE_BYTES / 1024
        )),
        "the byte bound is the parser's"
    );
}

/// Every rostered verb resolves as a help topic.
#[test]
fn every_front_door_verb_resolves() {
    for v in crate::Verb::ALL {
        let (page, code) = render(Some(v.name()), None, None);
        assert_eq!(code, 0, "`aterm help {}` exits {code}", v.name());
        assert!(
            !page.contains("unknown topic"),
            "`aterm help {}` 404s",
            v.name()
        );
        assert!(page.len() > 200, "`aterm help {}` is a stub", v.name());
    }
    for (name, _) in crate::DIAG_COMMANDS {
        let (page, code) = render(Some(name), None, None);
        assert_eq!(code, 0, "`aterm help {name}` exits {code}");
        assert!(!page.contains("unknown topic"), "`aterm help {name}` 404s");
        assert!(page.contains(name), "`aterm help {name}` never says {name}");
    }
}

/// Every `aterm ctl` verb `page` teaches is one the server's table or the client serves.
fn teaches_only_served_verbs(name: &str, page: &str) {
    for (command, verb) in aterm_primer::taught_ctl_verbs(page) {
        assert!(
            aterm_types::control_verbs::spec(&verb).is_some()
                || aterm_ctl::CLIENT_HELP_VERBS.contains(&verb.as_str()),
            "`aterm help {name}` teaches `aterm ctl {command}` (`{verb}`), which \
             nothing serves"
        );
    }
}

/// `aterm help introspection` lists every verb of the protocol table by its summary, in
/// table order, and every line of it fits the entry wrap.
#[test]
fn the_introspection_page_lists_every_verb_within_the_wrap() {
    let page = introspection_page();
    let mut rest = page.as_str();
    for line in aterm_types::control_verbs::catalog_lines_wrapped() {
        let at = rest
            .find(&format!("\n{line}\n"))
            .unwrap_or_else(|| panic!("the page lost (or reordered) {line}"));
        rest = &rest[at + line.len() + 1..];
    }
    let wrap = aterm_types::control_verbs::ENTRY_WRAP_COLUMNS;
    for line in page.lines() {
        assert!(
            line.chars().count() <= wrap,
            "{} columns: {line}",
            line.chars().count()
        );
    }
}

/// Every topic renders, ends its last line and teaches only `aterm ctl` verbs something
/// serves, and the toolchain words an agent types land on the rust page: `render` reads only
/// the page a word opens, and the rust page measures the toolchain each time it is made, so
/// it is made once.
#[test]
fn every_topic_renders() {
    for name in topic_names() {
        let (page, code) = render(Some(name), None, None);
        assert_eq!(code, 0, "topic {name} should render 0");
        assert!(!page.trim().is_empty(), "topic {name} rendered empty");
        assert!(
            page.ends_with('\n'),
            "topic {name} ends {:?}",
            page.lines().last()
        );
        teaches_only_served_verbs(name, &page);
        if name == "rust" {
            assert!(page.contains("THE DEFAULT HERE IS THE TRUST TOOLCHAIN"));
        }
    }
    for alias in ["cargo", "targo", "rustc", "trustc", "toolchain"] {
        assert_eq!(
            page_of(alias),
            "rust",
            "`aterm help {alias}` must land on the rust page"
        );
    }
}

/// The `trust version` row reads Trust's own version off verbatim `-vV`
/// texts: the `trust:` line, never the `rustc` release.
#[test]
fn trust_version_row_says_trusts_own_version_and_what_the_rustc_line_means() {
    let trust = "rustc 1.99.0-dev (3a3e781fe 2026-09-14)\nbinary: rustc\n\
                 commit-hash: 3a3e781fe082b74d3c4de0ade65b1de3cbee7255\ncommit-date: 2026-09-14\n\
                 host: x86_64-unknown-linux-gnu\nrelease: 1.99.0-dev\ntrust: 0.1.0\nLLVM version: 22.1.2\n";
    let row = trust_version_row(Some(trust.to_string()));
    assert!(row.starts_with("0.1.0 "), "{row}");
    let stock = "rustc 1.96.0 (ac68faa20 2026-05-25) (Homebrew)\nbinary: rustc\nrelease: 1.96.0\n";
    let row = trust_version_row(Some(stock.to_string()));
    assert!(row.starts_with("NONE "), "{row}");
    assert!(!row.contains("0.1.0"), "{row}");
    // An empty value is not a version.
    let empty = "rustc 1.99.0-dev (x 2026-01-01)\nrelease: 1.99.0-dev\ntrust:\n";
    assert!(trust_version_row(Some(empty.to_string())).starts_with("NONE"));
}

/// `aterm help rust` names the tree's lock and its build, what took its place, and a `targo`
/// shim that runs another build than the run compiles with; with no lock it says none binds
/// the tree. NEGATIVE CONTROLS: a run whose locked build is refused, and a shim laid for the
/// build the run resolved, state no mismatch.
#[test]
#[cfg(unix)]
fn trust_lock_rows_name_the_lock_and_a_shim_that_runs_another_build() {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-cli-lock-rows-")
        .tempdir()
        .expect("scratch");
    let prefix = scratch.path().join("pkg");
    let runnable = |bin: &Path| {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::create_dir_all(bin).expect("bin");
        let targo = bin.join("targo");
        std::fs::write(&targo, "#!/bin/sh\n").expect("targo");
        std::fs::set_permissions(&targo, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    };
    let tools = |stage2_dir: PathBuf, lock_override: Option<&str>| atpkg::toolchain::Toolchain {
        targo: stage2_dir.join("targo"),
        trustdoc: stage2_dir.join("trustdoc"),
        stage2_dir,
        tippy: None,
        trustfmt: None,
        borrowed: Vec::new(),
        refused: None,
        store_bin: None,
        demoted: None,
        lock: None,
        lock_refusal: None,
        lock_override: lock_override.map(str::to_string),
    };
    let own = scratch.path().join("s/bin");
    runnable(&own);
    let none = trust_lock_rows(None, &tools(own.clone(), None), &prefix);
    assert!(none.contains("none above this directory"), "{none}");

    let found = atpkg::trust_lock::Found {
        path: PathBuf::from("/c/trust.lock"),
        lock: Ok(atpkg::trust_lock::TrustLock {
            build: 83,
            tree_root: "d".repeat(64),
            compiler_commit: "a".repeat(40),
            receipt: "SEED-MINT sha256:00".to_string(),
            stock_toolchain: "1.99.1".to_string(),
        }),
    };
    let laid = |build: u64| {
        std::fs::create_dir_all(prefix.join("bin")).expect("bin");
        let target = atpkg::trust_lock::build_dir(&prefix, build).join("bin/targo");
        std::fs::write(
            prefix.join("bin/targo"),
            format!(
                "#!/bin/sh\n# atpkg shim\nexec '{}' \"$@\"\n",
                target.display()
            ),
        )
        .expect("shim");
    };
    laid(16387);
    let rows = trust_lock_rows(
        Some(&found),
        &tools(
            own.clone(),
            Some("TRUST_STAGE2_BIN=/s/bin overrides /c/trust.lock"),
        ),
        &prefix,
    );
    assert!(
        rows.starts_with(
            "  trust.lock       /c/trust.lock: Trust build 83 (tree root dddddddddddd"
        ),
        "{rows}"
    );
    assert!(rows.contains("TRUST_STAGE2_BIN=/s/bin overrides"), "{rows}");
    assert!(rows.contains("plain `targo` resolves 16387"), "{rows}");

    // A run whose locked build is refused compiles with nothing: no shim differs from it.
    let resolved = atpkg::trust_lock::build_dir(&prefix, 83).join("bin");
    let refused = atpkg::toolchain::Toolchain {
        lock_refusal: Some(
            "/c/trust.lock names Trust build 83, which this Mac's store does not hold".to_string(),
        ),
        ..tools(resolved.clone(), None)
    };
    let rows = trust_lock_rows(Some(&found), &refused, &prefix);
    assert!(!rows.contains("plain `targo`"), "{rows}");
    assert_eq!(rows.lines().count(), 1, "{rows}");

    laid(83);
    runnable(&resolved);
    let rows = trust_lock_rows(Some(&found), &tools(resolved, None), &prefix);
    assert!(!rows.contains("plain `targo`"), "{rows}");
    assert_eq!(rows.lines().count(), 1, "{rows}");
}

/// The `rust-toolchain` row never says a Trust pin drives a stock `cargo` while
/// `RUSTUP_TOOLCHAIN` (which rustup ranks above the pin) names another toolchain,
/// and never says nothing selects stock Rust where no pin exists.
#[test]
fn rust_toolchain_row_says_when_rustup_toolchain_overrides_the_pin() {
    let pinned = rust_toolchain_row(Some("trust"), None);
    assert!(pinned.contains("this project pins Trust"), "{pinned}");
    // Unset, empty and the pin's own name are no override.
    for env in [Some(""), Some("trust")] {
        assert_eq!(rust_toolchain_row(Some("trust"), env), pinned, "{env:?}");
    }
    // Negative control: the environment outranks the pin, and the row says
    // so.
    let overridden = rust_toolchain_row(Some("trust"), Some("stable"));
    assert!(
        overridden.contains("RUSTUP_TOOLCHAIN=stable overrides it")
            && overridden.contains("runs stable"),
        "{overridden}"
    );
    assert!(!overridden.contains("drives"), "{overridden}");
    let unpinned = rust_toolchain_row(None, Some("stable"));
    assert!(
        unpinned.contains("no channel pinned") && unpinned.contains("what rustup or"),
        "{unpinned}"
    );
    assert!(
        !unpinned.contains("nothing selects stock Rust"),
        "{unpinned}"
    );
}

/// The `targo here` row names the trustc beside targo when nothing moves it,
/// the mover when one does, and says so when that targo has no trustc.
#[test]
fn targo_here_names_the_compiler_or_what_moved_it() {
    let hold = path_scratch("targo-here");
    let root = hold.path();
    std::fs::write(root.join("trustc"), "").expect("write");
    let row = targo_here_row(Some(root), &[]);
    assert!(
        row.starts_with(&format!("{}", root.join("trustc").display())),
        "{row}"
    );
    assert!(row.contains("the trustc beside targo"), "{row}");
    assert!(
        row.contains("a rust-toolchain.toml pin does not move it"),
        "{row}"
    );
    let moved = targo_here_row(
        Some(root),
        &[atpkg::toolchain::rustc_movers::Mover {
            what: "$RUSTC".to_string(),
            value: "/s/rustc".to_string(),
            base: root.to_path_buf(),
        }],
    );
    assert!(
        moved.starts_with("NOT trustc: $RUSTC = /s/rustc"),
        "{moved}"
    );
    let bare = root.join("bare");
    std::fs::create_dir_all(&bare).expect("mkdir");
    assert!(targo_here_row(Some(&bare), &[]).contains("holds no `trustc`"));
    assert!(targo_here_row(None, &[]).starts_with("unknown"));
}

/// The probe helper is bounded: `sleep 120` against a 200 ms limit returns
/// `None` well within the 60 s check.
#[test]
fn bounded_first_line_kills_a_wedged_probe() {
    let (wedge, wedge_args): (&str, &[&str]) = if cfg!(windows) {
        ("ping", &["-n", "120", "127.0.0.1"])
    } else {
        ("sleep", &["120"])
    };
    let (echo, echo_args): (&str, &[&str]) = if cfg!(windows) {
        ("cmd", &["/d", "/c", "echo", "hello"])
    } else {
        ("echo", &["hello"])
    };
    let t0 = std::time::Instant::now();
    let got = bounded_first_line(wedge, wedge_args, std::time::Duration::from_millis(200));
    assert!(
        got.is_none(),
        "a wedged probe must answer None, got {got:?}"
    );
    assert!(
        t0.elapsed() < std::time::Duration::from_secs(60),
        "the bound did not hold: {:?}",
        t0.elapsed()
    );
    assert_eq!(
        bounded_first_line(echo, echo_args, std::time::Duration::from_secs(60)).as_deref(),
        Some("hello")
    );
    #[cfg(unix)]
    assert_eq!(
        bounded_first_line(
            "/bin/sh",
            &[
                "-c",
                "printf 'first-line\\n'; dd if=/dev/zero bs=1024 count=256 2>/dev/null"
            ],
            std::time::Duration::from_secs(60),
        )
        .as_deref(),
        Some("first-line"),
        "probe output larger than the OS pipe buffer must drain concurrently without deadlocking"
    );
}

/// A portable scratch directory for the toolchain and PATH-resolution tests,
/// unique per test and process, removed on drop.
fn path_scratch(tag: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-cli-help-rust-{tag}-"))
        .tempdir()
        .expect("scratch dir")
}

/// An executable file that is only READ by the test (PATH resolution, the exec bit,
/// a shim's text): written and `chmod`ed, never run.
#[cfg(unix)]
fn put_exec(path: &Path, body: &[u8]) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::create_dir_all(path.parent().expect("has a parent")).expect("mkdir");
    std::fs::write(path, body).expect("write");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// An executable file the page under test will RUN (a version probe): [`put_exec`],
/// then its first exec paid here (see [`run_once`]).
#[cfg(unix)]
fn write_exec(path: &Path, body: &[u8]) {
    put_exec(path, body);
    run_once(path);
}

/// Run `path` once, unbounded, output discarded.
#[cfg(unix)]
fn run_once(path: &Path) {
    let _ = aterm_spec::wait::past_text_busy(|| {
        aterm_uds::spawnfd::command(path)
            .arg("--version")
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .status()
    });
}

/// The shim shape this page reads matches atpkg's: each shim (plain,
/// env-exporting, and with a single quote in the target) is rendered by
/// atpkg's writer and read back by [`sh_exec_target`].
#[test]
fn the_shim_shape_matches_atpkg() {
    #[cfg(unix)]
    {
        let env = atpkg::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()])
            .expect("a plain NAME=VALUE is admitted");
        for target in [
            "/prefix/store/trust/8595/bin/targo",
            "/Users//x/Library/Application Support/aterm/pkg/store/trust/8595/bin/trustc",
            "/odd/it's/bin/tippy",
        ] {
            for env in [&atpkg::shim_env::ShimEnv::NONE, &env] {
                let shim = atpkg::platform::shim_executable_to_env(
                    Path::new("/prefix/bin/tool"),
                    Path::new(target),
                    env,
                )
                .expect("renders");
                let body = String::from_utf8(shim.body).expect("a shim is UTF-8");
                assert_eq!(
                    sh_exec_target(&body).as_deref(),
                    Some(Path::new(target)),
                    "atpkg's shim was not read back to its target:\n{body}"
                );
            }
        }
    }
    #[cfg(windows)]
    {
        let shim = atpkg::platform::shim_executable_to_env(
            Path::new(r"C:\prefix\bin\targo.cmd"),
            Path::new(r"C:\prefix\store\trust\8595\bin\targo.exe"),
            &atpkg::shim_env::ShimEnv::NONE,
        )
        .expect("renders");
        let body = String::from_utf8(shim.body).expect("a shim is UTF-8");
        assert_eq!(sh_exec_target(&body), None, "{body}");
    }
    assert_eq!(sh_exec_target("#!/bin/sh\necho gone >&2\nexit 127\n"), None);
    assert_eq!(sh_exec_target("\u{7f}ELF"), None);
    let guard = "[ ! -h '/r/bin/t' ] && [ -f '/r/bin/t' ] && [ -x '/r/bin/t' ] && [ -f \
                 '/s/bin/t' ] && [ ! -h '/r/.atpkg-root' ] && [ -f '/r/.atpkg-root' ] && \
                 exec '/r/bin/t' \"$@\"";
    assert_eq!(
        sh_guard_line(guard),
        Some(ShimGuard {
            root: PathBuf::from("/r/bin/t"),
            store: PathBuf::from("/s/bin/t"),
            marker: PathBuf::from("/r/.atpkg-root"),
        })
    );
    for other in [
        guard.replacen("[ -f '/r/bin/t' ]", "[ -f '/x/bin/t' ]", 1),
        guard.replacen("[ -x '/r/bin/t' ]", "[ -x '/x/bin/t' ]", 1),
        guard.replacen("exec '/r/bin/t'", "exec '/x/bin/t'", 1),
        guard.replacen("[ -f '/r/.atpkg-root' ]", "[ -f '/x/.atpkg-root' ]", 1),
        guard.replacen(" \"$@\"", "", 1),
        guard.replacen("[ ! -h '/r/bin/t' ]", "[ -h '/r/bin/t' ]", 1),
        String::from(
            "[ ! -h '/r/bin/t' ] && [ '/r/bin/t' -ef '/s/bin/t' ] && exec '/r/bin/t' \"$@\"",
        ),
    ] {
        assert_eq!(sh_guard_line(&other), None, "{other}");
    }
    let late = format!("#!/bin/sh\nexec '/s/bin/t' \"$@\"\n{guard}\n");
    assert_eq!(
        sh_shim_script(&late),
        Some(ShimScript {
            guards: Vec::new(),
            target: PathBuf::from("/s/bin/t"),
        })
    );
    assert_eq!(
        sh_quoted_word("'it'\\''s a pkg' rest"),
        Some(("it's a pkg".to_string(), " rest"))
    );
    assert_eq!(sh_quoted_word("unquoted"), None);
    assert_eq!(sh_quoted_word("'unterminated"), None);
}

/// A routed shim runs from where its guard sends the shell.
#[cfg(unix)]
#[test]
fn routed_shims_resolve_to_where_the_shell_execs() {
    let hold = path_scratch("routed");
    let scratch = std::fs::canonicalize(hold.path()).expect("scratch");
    let prefix = scratch.join("it's a pkg");
    let layout = atpkg::Layout {
        prefix: prefix.clone(),
    };
    let build = layout.build_dir("trust", 8595);
    let bin = build.join("bin");
    write_exec(&bin.join("targo"), b"#!/bin/sh\nprintf '%s\\n' \"$0\"\n");
    write_exec(
        &bin.join("trustc"),
        b"frontend 8595 / code signature: trustc",
    );
    write_exec(
        &bin.join("rustc"),
        b"frontend 8595 / code signature: rustc_",
    );
    write_exec(&bin.join("tippy"), b"tippy 8595");
    std::fs::create_dir_all(build.join("lib/rustlib/aarch64-apple-darwin/lib")).expect("lib");
    std::fs::write(build.join("lib/librustc_driver-5cfd.dylib"), b"driver").expect("dylib");
    assert!(
        atpkg::compat::needs_root(&build)
            .expect("the fixture build reads")
            .is_some(),
        "the fixture is not shaped like 8595"
    );
    assert_eq!(
        atpkg::compat::ensure_root(&layout, &build, atpkg::seam::Depth::Deep).expect("laid"),
        atpkg::compat::Ensured::Built
    );
    let root_bin = atpkg::compat::root_dir(&layout, 8595).join("bin");
    let (store_targo, root_targo) = (bin.join("targo"), root_bin.join("targo"));
    let marker = atpkg::compat::root_marker(&atpkg::compat::root_dir(&layout, 8595));
    run_once(&root_targo);

    let shims = prefix.join("bin");
    let shim = shims.join("targo");
    let body = atpkg::platform::shim_executable_to_env(
        &shim,
        &store_targo,
        &atpkg::shim_env::ShimEnv::NONE,
    )
    .expect("renders")
    .body;
    write_exec(&shim, &body);
    let body = String::from_utf8(body).expect("a shim is UTF-8");
    assert_eq!(
        sh_shim_script(&body),
        Some(ShimScript {
            guards: vec![ShimGuard {
                root: root_targo.clone(),
                store: store_targo.clone(),
                marker: marker.clone(),
            }],
            target: store_targo.clone(),
        }),
        "atpkg's routed shim was not read back:\n{body}"
    );
    let path_env = std::env::join_paths([&shims]).expect("join");
    let shell_execs = || {
        let run = aterm_spec::wait::past_text_busy(|| {
            aterm_uds::spawnfd::command(&shim).env_clear().output()
        })
        .expect("run the shim");
        assert!(run.status.success(), "{run:?}");
        PathBuf::from(String::from_utf8_lossy(&run.stdout).trim_end())
    };

    let got = resolve_on_path("targo", &path_env).expect("targo is on PATH");
    assert_eq!(
        got.shims,
        vec![ShimHop {
            execs: root_targo.clone(),
            route: Some(Route::Taken {
                store: store_targo.clone()
            }),
        }]
    );
    assert_eq!(shell_execs(), root_targo, "the shell disagrees");
    assert_eq!(got.real_dir(), Some(root_bin.clone()));
    assert!(is_toolchain_bin(&root_bin) && is_toolchain_bin(&bin));

    let marker_body = std::fs::read(&marker).expect("the marker reads");
    let root_body = std::fs::read(&root_targo).expect("the root file reads");
    let restore = || {
        let _ = std::fs::remove_file(&root_targo);
        write_exec(&root_targo, &root_body);
        let _ = std::fs::remove_file(&marker);
        std::fs::write(&marker, &marker_body).expect("marker");
    };
    let refusals: [(&str, &dyn Fn(), Refusal); 4] = [
        (
            "unmarked",
            &|| std::fs::remove_file(&marker).expect("unlink"),
            Refusal::Unmarked,
        ),
        (
            "not executable",
            &|| {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&root_targo, std::fs::Permissions::from_mode(0o644))
                    .expect("chmod");
            },
            Refusal::NotExecutable,
        ),
        (
            "symlink",
            &|| {
                std::fs::remove_file(&root_targo).expect("unlink");
                std::os::unix::fs::symlink(&store_targo, &root_targo).expect("symlink");
            },
            Refusal::Symlink,
        ),
        (
            "missing",
            &|| std::fs::remove_file(&root_targo).expect("unlink"),
            Refusal::Missing,
        ),
    ];
    for (tag, plant, why) in refusals {
        restore();
        plant();
        let got = resolve_on_path("targo", &path_env).expect("targo is on PATH");
        assert_eq!(
            got.shims,
            vec![ShimHop {
                execs: store_targo.clone(),
                route: Some(Route::Refused {
                    root: root_targo.clone(),
                    why
                }),
            }],
            "{tag}"
        );
        assert_eq!(shell_execs(), store_targo, "{tag}: the shell disagrees");
        assert_eq!(got.real_dir(), Some(bin.clone()), "{tag}");
    }
    drop(hold);
}

/// `targo on PATH` resolves as the shell does: the first executable entry
/// wins, a reroute directory is skipped, and an atpkg shim is followed to
/// the canonical file it execs.
#[cfg(unix)]
#[test]
fn resolve_on_path_takes_the_first_executable_and_follows_the_shim() {
    let hold = path_scratch("resolve");
    let root = hold.path();
    let reroute = root.join("reroute");
    let plain = root.join("plain");
    let shims = root.join("shims");
    let store = root.join("store/trust/8595/bin");
    put_exec(&reroute.join("targo"), b"#!/bin/sh\nexit 9\n");
    std::fs::write(reroute.join(REROUTE_DIR_MARKER), "marker\n").expect("marker");
    std::fs::create_dir_all(&plain).expect("mkdir");
    std::fs::write(plain.join("targo"), "not executable").expect("write");
    let real = store.join("targo");
    put_exec(&real, b"\x7fELF not a script");
    put_exec(
        &shims.join("targo"),
        atpkg::platform::shim_executable_to_env(
            &shims.join("targo"),
            &real,
            &atpkg::shim_env::ShimEnv::NONE,
        )
        .expect("renders")
        .body
        .as_slice(),
    );
    let path_env = std::env::join_paths([&reroute, &plain, &shims]).expect("join");
    let got = resolve_on_path("targo", &path_env).expect("targo is on PATH");
    assert_eq!(got.found, shims.join("targo"));
    assert_eq!(
        got.shims,
        vec![ShimHop {
            execs: real.clone(),
            route: None
        }]
    );
    let canonical_store = std::fs::canonicalize(&store).expect("store exists");
    assert_eq!(got.real, Some(canonical_store.join("targo")));
    assert_eq!(got.real_dir(), Some(canonical_store));
    let described = describe_on_path(&got);
    assert!(described.contains("a shim that execs"), "{described}");

    let plain_only = std::env::join_paths([&store]).expect("join");
    let got = resolve_on_path("targo", &plain_only).expect("the store file itself");
    assert!(got.shims.is_empty());
    assert!(resolve_on_path("trustc", &plain_only).is_none());

    put_exec(
        &root.join("dangling/targo"),
        b"#!/bin/sh\nexec '/nonexistent/aterm-cli-test/targo' \"$@\"\n",
    );
    let dangling = std::env::join_paths([root.join("dangling")]).expect("join");
    let got = resolve_on_path("targo", &dangling).expect("the shim is on PATH");
    assert_eq!(got.real, None);
    assert_eq!(got.real_dir(), None);
    assert!(describe_on_path(&got).contains("does NOT resolve"));
}

/// The build a version line names stops at the first `)`; a line with no
/// parenthesis is compared whole.
#[test]
fn build_of_stops_at_the_commit_parenthesis() {
    assert_eq!(
        build_of("targo 1.99.0-dev (43f8b339f 2026-09-12) (targo 0.1.0)"),
        "targo 1.99.0-dev (43f8b339f 2026-09-12)"
    );
    assert_eq!(
        build_of("targo 1.99.0-dev (43f8b339f 2026-09-12)"),
        "targo 1.99.0-dev (43f8b339f 2026-09-12)"
    );
    assert_eq!(build_of(" targo 0.1.0 "), "targo 0.1.0");
}

/// [`compare_toolchains`] over five shapes: one directory; one build in two toolchain
/// directories; a matching line from a non-toolchain directory; two builds; and a probe that
/// did not answer.
#[test]
fn compare_toolchains_classifies_the_five_shapes() {
    fn side<'a>(dir: &'a str, ver: Option<&'a str>, toolchain_bin: bool) -> Side<'a> {
        Side {
            dir: Path::new(dir),
            ver,
            toolchain_bin,
        }
    }
    let store = "/p/store/trust/8595/bin";
    let view = "/p/rustup/trust/bin";
    let tree = "/h/trust/build/aarch64-apple-darwin/stage2/bin";
    let script = "/h/fake/script-old";
    let new = "targo 1.99.0-dev (43f8b339f 2026-09-12) (targo 0.1.0)";
    let old = "targo 1.99.0-dev (07ec014cb 2026-08-10)";
    let cmp = |g: Side<'_>, p: Side<'_>| compare_toolchains(&g, &p);
    assert_eq!(
        cmp(side(store, Some(new), true), side(store, Some(new), true)),
        Agreement::Same
    );
    assert_eq!(
        cmp(side(store, None, true), side(store, Some(new), true)),
        Agreement::Same
    );
    assert_eq!(
        cmp(
            side(view, Some(new), true),
            side(store, Some("targo 1.99.0-dev (43f8b339f 2026-09-12)"), true)
        ),
        Agreement::SameBuild
    );
    assert_eq!(
        cmp(side(tree, Some(old), true), side(script, Some(old), false)),
        Agreement::SameLine
    );
    assert_eq!(
        cmp(side(script, Some(old), false), side(tree, Some(old), true)),
        Agreement::SameLine
    );
    assert_eq!(
        cmp(side(tree, Some(old), true), side(store, Some(new), true)),
        Agreement::OtherBuild
    );
    assert_eq!(
        cmp(side(store, Some(old), true), side(store, Some(new), true)),
        Agreement::OtherBuild
    );
    assert_eq!(
        cmp(side(tree, None, true), side(store, Some(new), true)),
        Agreement::OtherDir
    );
    assert_eq!(
        cmp(side(tree, Some(old), true), side(store, None, true)),
        Agreement::OtherDir
    );
}

/// `is_toolchain_bin` separates ONE BUILD from a matching line: a `trustc` with a
/// `lib/rustlib` sysroot above it, and nothing less.
#[cfg(unix)]
#[test]
fn a_toolchain_bin_needs_trustc_and_a_sysroot() {
    let hold = path_scratch("toolchain-bin");
    let root = hold.path();
    let bin = root.join("tc/bin");
    put_exec(&bin.join("targo"), b"#!/bin/sh\n");
    assert!(!is_toolchain_bin(&bin), "no trustc");
    put_exec(&bin.join("trustc"), b"#!/bin/sh\n");
    assert!(!is_toolchain_bin(&bin), "no lib/rustlib");
    std::fs::create_dir_all(root.join("tc/lib/rustlib")).expect("sysroot");
    assert!(is_toolchain_bin(&bin));
}

/// Before `trust` lands, PATH's `targo` is atpkg's stub, which the page names and never runs.
#[cfg(unix)]
#[test]
fn page_never_runs_a_pending_targo() {
    const CHILD: &str = "ATERM_TEST_HELP_RUST_PENDING_CHILD";
    if let Some(out) = std::env::var_os(CHILD) {
        let (page, code) = render(Some("rust"), None, None);
        assert_eq!(code, 0);
        std::fs::write(out, page).expect("write the page");
        return;
    }
    let hold = path_scratch("pending");
    let root = std::fs::canonicalize(hold.path()).expect("scratch");
    let home = root.join("home");
    let stubs = root.join("stubs");
    std::fs::create_dir_all(&home).expect("home");
    let ran = root.join("targo-ran");
    put_exec(
        &stubs.join("targo"),
        format!(
            "#!/bin/sh\n# atpkg pending-program stub v1\n: > '{}'\nexit 127\n",
            ran.display()
        )
        .as_bytes(),
    );
    let name = format!(
        "{}::page_never_runs_a_pending_targo",
        module_path!().split_once("::").map_or("", |(_, rest)| rest)
    );
    let out = root.join("page.txt");
    let run = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"))
        .args(["--exact", &name, "--nocapture", "--test-threads=1"])
        .env_clear()
        .env(CHILD, &out)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", root.join("xdg"))
        .env("PATH", format!("{}:/usr/bin:/bin", stubs.display()))
        .current_dir(&root)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .expect("re-exec the test binary");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(run.status.success(), "the re-exec failed:\n{text}");
    assert!(
        text.contains("1 passed"),
        "the re-exec ran no test:\n{text}"
    );
    let page = std::fs::read_to_string(&out).expect("the child wrote the page");
    assert!(!ran.exists(), "the pending stub was run:\n{page}");
    for row in [
        "  targo            ",
        "  targo fmt        ",
        "  targo tippy      ",
    ] {
        assert!(
            page.lines()
                .any(|l| l.starts_with(row) && l.contains("pending stub")),
            "{row:?} names the stub:\n{page}"
        );
    }
    drop(hold);
}

/// `aterm help privacy` is the permissions page, which opens with a table of at most ten
/// lines (a header, and per permission what it reads now and its one fix) before DETAILS.
/// A test binary reads no state; the table shows the states it is given (the control).
#[test]
fn help_privacy_is_the_permissions_page_and_it_opens_with_the_live_table() {
    let (page, code) = render(Some("privacy"), None, None);
    assert_eq!(code, 0, "{page}");
    assert_eq!(page, render(Some("permissions"), None, None).0);
    assert!(page.starts_with("permissions — "), "{page}");
    let head = &page[..page.find("\nDETAILS\n").expect("a DETAILS heading")];
    let table: Vec<&str> = head
        .lines()
        .skip_while(|l| !l.starts_with("  PERMISSION"))
        .take_while(|l| l.starts_with("  ") && !l.starts_with("  (`"))
        .collect();
    assert!(table.len() <= 10, "{table:#?}");
    let rows = |name: &str| {
        table
            .iter()
            .filter(|l| l.starts_with(&format!("  {name}")))
            .count()
    };
    for name in ["Full Disk Access", "Developer Tools", "Debugger access"] {
        assert_eq!(rows(name), 1, "{name}: {table:#?}");
    }
    assert_eq!(
        table
            .iter()
            .filter(|l| l.trim_start().starts_with("fix: "))
            .count(),
        3
    );
    assert_eq!(
        table.iter().filter(|l| l.contains("not read")).count(),
        3,
        "{table:#?}"
    );
    assert!(page.contains("WHAT TO DO, IN ORDER"), "the details stay");

    let live = permissions_table(["granted", "off", "asks"], None);
    let line = |name: &str| {
        live.lines()
            .find(|l| l.starts_with(&format!("  {name}")))
            .unwrap()
    };
    assert!(line("Full Disk Access").contains(" granted "), "{live}");
    assert!(line("Developer Tools").contains(" off "), "{live}");
    assert!(line("Debugger access").contains(" asks "), "{live}");
    assert!(live.lines().all(|l| l.chars().count() <= 100), "{live}");
    assert!(!live.contains("This shell runs under"), "{live}");
    let terminal = permissions_table(
        ["not aterm's", "on", "asks"],
        Some(&elsewhere_note("Terminal")),
    );
    assert!(
        terminal.contains("macOS grants Full Disk Access here to Terminal, not aterm"),
        "{terminal}"
    );
    assert!(terminal.lines().count() <= 10, "{terminal}");
    let unbundled = permissions_table(["not read"; 3], Some(NO_BUNDLE_NOTE));
    assert!(
        unbundled
            .lines()
            .any(|l| l == format!("  {NO_BUNDLE_NOTE}")),
        "{unbundled}"
    );
    for table in [terminal, unbundled] {
        assert!(table.lines().all(|l| l.chars().count() <= 100), "{table}");
    }
}

/// The app macOS holds responsible for this process is another app's unless its bundle id
/// reads as aterm's own: one that cannot be read, or an aterm whose own id cannot be, is
/// still another app's.
#[test]
fn the_responsible_app_is_another_apps_unless_its_bundle_id_reads_as_aterms_own() {
    let aterm = Some("com.aterm.aterm");
    assert!(!runs_elsewhere(aterm, aterm));
    assert!(runs_elsewhere(aterm, Some("com.apple.Terminal")));
    assert!(runs_elsewhere(aterm, None));
    assert!(runs_elsewhere(None, Some("com.apple.Terminal")));
    assert!(runs_elsewhere(None, None));
}

/// Each state a probe reads has its own word in the NOW column, and Full Disk Access under
/// another app reads `not aterm's`.
#[test]
fn each_permission_state_reads_as_its_own_word() {
    use aterm_containment::consent::{DebuggerMode, DevToolListing, FdaState};
    assert_eq!(
        permission_words(
            Some(FdaState::Granted),
            DevToolListing::On,
            DebuggerMode::Enabled
        ),
        ["granted", "on", "allowed"]
    );
    assert_eq!(
        permission_words(None, DevToolListing::Absent, DebuggerMode::Disabled),
        ["not aterm's", "not listed", "asks"]
    );
    assert_eq!(
        permission_words(
            Some(FdaState::Denied),
            DevToolListing::Off,
            DebuggerMode::Unrecognized
        ),
        ["off", "off", "unknown"]
    );
    assert_eq!(
        permission_words(
            Some(FdaState::Unknown),
            DevToolListing::Unknown,
            DebuggerMode::Unknown
        ),
        ["unknown"; 3]
    );
}

/// The topic index lists every verb and every page that is no verb's exactly once, each
/// row naming a page `aterm help` opens and fitting 100 columns; it is the front page outside
/// a session and `aterm help topics` (or `index`).
#[test]
fn the_index_lists_every_verb_and_topic_once_within_100_columns() {
    let (index, code) = render(Some("topics"), None, None);
    assert_eq!(code, 0);
    assert_eq!(render(None, None, None), (index.clone(), 0));
    assert_eq!(render(Some("index"), None, None), (index.clone(), 0));
    for line in index.lines() {
        assert!(
            line.chars().count() <= 100,
            "{} columns: {line}",
            line.chars().count()
        );
    }
    let rows: Vec<&str> = index
        .lines()
        .filter(|l| l.starts_with("  ") && !l.starts_with("   "))
        .filter_map(|l| l.split_whitespace().next())
        .collect();
    let mut listed: Vec<&str> = crate::Verb::ALL.iter().map(|v| v.name()).collect();
    for t in TOPICS {
        if !listed.contains(&t.name) {
            listed.push(t.name);
        }
    }
    for name in listed.into_iter().chain(std::iter::once("help")) {
        let times = rows.iter().filter(|row| **row == name).count();
        assert_eq!(times, 1, "`{name}` is listed {times} times: {index}");
    }
    for name in rows.iter().filter(|name| **name != "help") {
        let (page, code) = render(Some(name), None, None);
        assert_eq!(code, 0, "`aterm help {name}` exits {code}");
        assert!(
            page.ends_with('\n'),
            "`aterm help {name}` ends {:?}",
            page.lines().last()
        );
    }
}

/// Where Trust's row says the index publishes no build for this target, every act the
/// page names is the local link, never the install that answers "unavailable".
#[test]
fn an_unserved_trust_is_sent_to_the_local_link_not_the_install() {
    for line in [
        rustup_unlinked_row(false, true),
        trust_install_act(true).to_string(),
    ] {
        assert!(
            line.contains("`aterm pkg link trust <sysroot>")
                && line.contains("docs/LINUX-LOCAL-TOOLCHAIN.md")
                && !line.contains("pkg install"),
            "{line}"
        );
    }
    assert_eq!(trust_install_act(false), "`aterm pkg install trust`");
}
#[test]
fn unknown_topic_is_a_usage_error_listing_topics() {
    let (msg, code) = render(Some("nonesuch"), None, None);
    assert_eq!(code, 2);
    assert!(msg.contains("unknown topic") && msg.contains("trust"));
    assert!(msg.ends_with('\n'), "{msg:?}");
}

/// `aterm help fabric` answers to the words an agent types when it has mail.
#[test]
fn fabric_topic_answers_to_its_aliases() {
    let (page, code) = render(Some("fabric"), None, None);
    assert_eq!(code, 0);
    for name in [
        "inbox",
        "post",
        "mail",
        "topic",
        "say",
        "broadcast",
        "messaging",
    ] {
        assert_eq!(
            render(Some(name), None, None),
            (page.clone(), 0),
            "`aterm help {name}`"
        );
    }
}

/// `aterm help secrets` answers to the words someone types for the
/// credentials vault.
#[test]
fn the_secrets_page_answers_to_its_words() {
    let (page, code) = render(Some("secrets"), None, None);
    assert_eq!(code, 0);
    for name in ["secret", "vault", "credentials"] {
        assert_eq!(
            render(Some(name), None, None),
            (page.clone(), 0),
            "`aterm help {name}`"
        );
    }
}

/// `aterm help workers`, where the brief's WORKERS line leads, answers to
/// the words a model types for it.
#[test]
fn the_workers_page_answers_to_its_words() {
    let (page, code) = render(Some("workers"), None, None);
    assert_eq!(code, 0);
    for name in ["worker", "survey", "result", "subagent", "subagents"] {
        assert_eq!(
            render(Some(name), None, None),
            (page.clone(), 0),
            "`aterm help {name}`"
        );
    }
}

/// `aterm help needs`, where the ❯ item points, answers to the words someone
/// types for a need, stepping in and desktop notices.
#[test]
fn the_needs_page_answers_to_its_words() {
    let (page, code) = render(Some("needs"), None, None);
    assert_eq!(code, 0);
    for name in ["need", "take-over", "takeover", "hand-back", "notify"] {
        assert_eq!(
            render(Some(name), None, None),
            (page.clone(), 0),
            "`aterm help {name}`"
        );
    }
}
