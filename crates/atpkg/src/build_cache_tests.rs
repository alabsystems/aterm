// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::collections::HashMap;
use std::time::{Duration, SystemTime};

/// A canonical scratch tree (`/var` is a link to `/private/var` on macOS, and `$PWD` is
/// the resolved path), with a home whose caches folder exists.
struct Fixture {
    _dir: crate::testdir::Dir,
    root: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let dir = crate::testdir::dir(label);
        let root = std::fs::canonicalize(&*dir).unwrap();
        let home = root.join("home");
        std::fs::create_dir_all(home.join("Library/Caches")).unwrap();
        Self {
            _dir: dir,
            root,
            home,
        }
    }

    fn dir(&self, rel: &str) -> PathBuf {
        let p = self.root.join(rel);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn file(&self, rel: &str, text: &str) {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn value(&self) -> String {
        config_value(&cache_root(&self.home)).unwrap()
    }
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_owned()).collect()
}

/// The Rust half on the fixture's real files.
fn rust(
    home: &Path,
    cwd: &Path,
    env: &HashMap<&str, String>,
    args: &[String],
    tool: Tool,
) -> Vec<String> {
    let m = Machine {
        home: Some(home),
        cwd: Some(cwd),
        env: &|name| env.get(name).map(OsString::from),
        read: &read_config,
        writable: &writable,
    };
    with_build_dir(args, tool, &m).unwrap_or_else(|| args.to_vec())
}

/// The shells the block must parse alike: macOS's `/bin/sh` (bash in POSIX mode), and
/// dash, ksh and zsh's sh emulation where installed (any of them can be `/bin/sh`).
fn shells() -> Vec<Vec<&'static str>> {
    let mut out = vec![vec!["/bin/sh"]];
    for (path, extra) in [
        ("/bin/dash", None),
        ("/bin/ksh", None),
        ("/bin/zsh", Some("--emulate")),
    ] {
        if Path::new(path).is_file() {
            let mut v = vec![path];
            if let Some(flag) = extra {
                v.extend([flag, "sh"]);
            }
            out.push(v);
        }
    }
    out
}

/// The shim block for `tool`.
fn block(tool: Tool) -> String {
    let name = match tool {
        Tool::Tippy => "targo-tippy",
        Tool::Targo | Tool::Cargo => "targo",
    };
    shim_block(name).unwrap()
}

/// The `sh` half, run for real: the block, then the arguments it leaves, one per line.
fn sh(
    shell: &[&str],
    tool: Tool,
    home: &Path,
    cwd: &Path,
    env: &HashMap<&str, String>,
    args: &[String],
) -> Vec<String> {
    let mut cmd = aterm_uds::spawnfd::command(shell[0]);
    cmd.args(&shell[1..])
        .arg("-c")
        .arg(format!(
            "{}[ $# -eq 0 ] || printf '%s\\n' \"$@\"",
            block(tool)
        ))
        .arg("sh")
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("PWD", cwd)
        .current_dir(cwd);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{shell:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stderr.is_empty(),
        "{shell:?} said: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// One case through both halves for `targo`: `applied` says whether `--config <value>`
/// must come first, and every shell and the Rust rule must agree.
fn check(f: &Fixture, cwd: &Path, env: &[(&'static str, &str)], args: &[&str], applied: bool) {
    check_tool(f, Tool::Targo, cwd, env, args, applied);
}

/// [`check`] for `tool`'s shim.
fn check_tool(
    f: &Fixture,
    tool: Tool,
    cwd: &Path,
    env: &[(&'static str, &str)],
    args: &[&str],
    applied: bool,
) {
    let env: HashMap<&str, String> = env.iter().map(|(k, v)| (*k, (*v).to_owned())).collect();
    let args = strings(args);
    let want = if applied {
        let mut w = vec!["--config".to_owned(), f.value()];
        w.extend(args.iter().cloned());
        w
    } else {
        args.clone()
    };
    assert_eq!(
        rust(&f.home, cwd, &env, &args, tool),
        want,
        "rust {tool:?}: {args:?} in {}",
        cwd.display()
    );
    for shell in shells() {
        assert_eq!(
            sh(&shell, tool, &f.home, cwd, &env, &args),
            want,
            "{shell:?} {tool:?}: {args:?} in {}",
            cwd.display()
        );
    }
}

/// EVERY RULE, in both halves. A building verb with `--unverified` (or `clean`, which
/// needs no lane) in a folder under `$HOME` gets the cache; each choice of the user's —
/// an environment variable, a flag, a config line anywhere up the tree or in
/// `$CARGO_HOME`, a folder outside the home or inside the cache, a cache that cannot be
/// written — keeps the command exactly as typed, and so do the verified lane and the
/// verbs that do not build. The `sh` block and [`with_build_dir`] answer alike in every
/// shell found.
#[test]
fn the_shim_and_the_rust_rule_decide_every_case_alike() {
    let f = Fixture::new("build-cache-rules");
    let ws = f.dir("home/ws");
    let home = f.home.display().to_string();

    for args in [
        &["--unverified", "build"][..],
        &["--unverified", "test", "-p", "x"],
        &["build", "--unverified"],
        &["clean"],
        &["clean", "-p", "x"],
        &["--unverified", "b"],
        &["--unverified", "t", "--lib"],
        &["--color", "always", "--unverified", "check"],
        &["-Z", "unstable-options", "--unverified", "doc"],
        &["--unverified", "run", "--", "--target-dir", "x"],
        &["--unverified", "build", "--config", "build.jobs=2"],
        &["--unverified", "build", "--config=build.jobs=2"],
        &["--unverified", "build", "--manifest-path", "sub/Cargo.toml"],
        &["--unverified", "tippy", "--all-targets"],
    ] {
        check(&f, &ws, &[], args, true);
    }
    let in_home = format!("{home}/ws/Cargo.toml");
    check(
        &f,
        &ws,
        &[],
        &["--unverified", "build", "--manifest-path", &in_home],
        true,
    );

    for args in [
        &[][..],
        &["--version"],
        &["build"],
        &["test"],
        &["trust", "check", "--lib"],
        &["--unverified", "metadata"],
        &["--unverified", "install", "x"],
        &["new", "x"],
        &["--unverified", "verify"],
        &["--unverified", "build", "--target-dir", "x"],
        &["--unverified", "build", "--target-dir=x"],
        &["--config", "build.target-dir='x'", "--unverified", "build"],
        &["--unverified", "build", "--config=build.build-dir='y'"],
        &["--config", "my.toml", "--unverified", "build"],
        &["--unverified", "build", "--config"],
        &["-C", "elsewhere", "--unverified", "build"],
        &[
            "--unverified",
            "build",
            "--manifest-path",
            "/elsewhere/Cargo.toml",
        ],
        &[
            "--unverified",
            "build",
            "--manifest-path",
            "../x/Cargo.toml",
        ],
        &[
            "--unverified",
            "build",
            "--manifest-path",
            "{{project_name}}/Cargo.toml",
        ],
        &["--unverified", "build", "--manifest-path=a}b/Cargo.toml"],
    ] {
        check(&f, &ws, &[], args, false);
    }
    // A manifest inside the cache (a build script building under its `OUT_DIR`).
    let cached = format!("{}/x/Cargo.toml", cache_root(&f.home).display());
    check(
        &f,
        &ws,
        &[],
        &["--unverified", "build", "--manifest-path", &cached],
        false,
    );
    check(
        &f,
        &ws,
        &[],
        &[
            "--unverified",
            "build",
            &format!("--manifest-path={cached}"),
        ],
        false,
    );

    // A working directory Cargo would read as a template once it is the workspace root.
    for dir in [
        "home/brace{x}",
        "home/open{only",
        "home/close}only",
        "home/cc/{{name}}",
    ] {
        check(&f, &f.dir(dir), &[], &["--unverified", "build"], false);
    }

    // `targo-tippy` is the lint: no subcommand, no lane, every argument its own.
    for args in [
        &[][..],
        &[
            "--workspace",
            "--all-targets",
            "--keep-going",
            "--",
            "-D",
            "warnings",
        ],
        &["-p", "x", "--lib"],
    ] {
        check_tool(&f, Tool::Tippy, &ws, &[], args, true);
    }
    check_tool(&f, Tool::Tippy, &ws, &[], &["--target-dir", "t"], false);
    check_tool(
        &f,
        Tool::Tippy,
        &ws,
        &[("CARGO_TARGET_DIR", "/t")],
        &[],
        false,
    );
    check_tool(&f, Tool::Tippy, &f.dir("elsewhere/lint"), &[], &[], false);

    let build = &["--unverified", "build"][..];
    for var in [
        "CARGO_TARGET_DIR",
        "CARGO_BUILD_TARGET_DIR",
        "CARGO_BUILD_BUILD_DIR",
    ] {
        check(&f, &ws, &[(var, "/x")], build, false);
        check(&f, &ws, &[(var, "")], build, false);
    }

    // Config files up the tree: a line naming either key chooses unless it is a comment
    // (a mention after a value counts: the safe side), the legacy name counts, and an
    // ancestor's file counts for every folder below it.
    f.file(
        "home/chosen/.cargo/config.toml",
        "[build]\ntarget-dir = \"out\"\n",
    );
    check(&f, &f.dir("home/chosen"), &[], build, false);
    f.file(
        "home/commented/.cargo/config.toml",
        "# target-dir = \"out\"\n[build]\n  \t# build-dir = \"b\"\njobs = 2\n",
    );
    check(&f, &f.dir("home/commented"), &[], build, true);
    f.file(
        "home/trailing/.cargo/config.toml",
        "[build]\njobs = 2 # not a build-dir\n",
    );
    check(&f, &f.dir("home/trailing"), &[], build, false);
    f.file(
        "home/inline/.cargo/config.toml",
        "build = { rustflags = [\"#\"], target-dir = \"x\" }\n",
    );
    check(&f, &f.dir("home/inline"), &[], build, false);
    f.file("home/legacy/.cargo/config", "build.build-dir = 'b'");
    check(&f, &f.dir("home/legacy"), &[], build, false);
    f.file("home/up/.cargo/config.toml", "[build]\nbuild-dir = \"b\"\n");
    check(&f, &f.dir("home/up/a/b"), &[], build, false);
    f.file("cargohome/config.toml", "[build]\ntarget-dir = \"t\"\n");
    let cargo_home = f.root.join("cargohome").display().to_string();
    check(&f, &ws, &[("CARGO_HOME", &cargo_home)], build, false);
    f.file("home/.cargo/config.toml", "[net]\noffline = true\n");
    check(&f, &ws, &[], build, true);
    f.file("home/.cargo/config.toml", "build.target-dir = \"t\"\n");
    check(&f, &ws, &[], build, false);
    std::fs::remove_file(f.root.join("home/.cargo/config.toml")).unwrap();

    // Where the command runs: outside the home, the home itself, the cache.
    check(&f, &f.dir("elsewhere/ws"), &[], build, false);
    check(&f, &f.home, &[], build, false);
    let cache = cache_root(&f.home).join("x");
    std::fs::create_dir_all(&cache).unwrap();
    check(&f, &cache, &[], build, false);
    std::fs::remove_dir_all(f.home.join("Library/Caches/aterm")).unwrap();

    // A cache this process cannot write (a containment tab's): as before. A missing
    // caches folder is the same answer.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let caches = f.home.join("Library/Caches");
        std::fs::set_permissions(&caches, std::fs::Permissions::from_mode(0o555)).unwrap();
        check(&f, &ws, &[], build, false);
        std::fs::set_permissions(&caches, std::fs::Permissions::from_mode(0o755)).unwrap();
        check(&f, &ws, &[], build, true);
        std::fs::remove_dir(&caches).unwrap();
        check(&f, &ws, &[], build, false);
    }
}

/// A HOME CARGO CANNOT TAKE gets nothing, in both halves: one a TOML literal string
/// cannot hold (a `'`), one Cargo would read as a template (a `{` or `}`), and one ending
/// in `/`.
#[test]
fn a_home_cargo_cannot_take_is_left_alone() {
    let f = Fixture::new("build-cache-home");
    let args = strings(&["--unverified", "build"]);
    for (dir, home) in [
        ("o'home", "o'home"),
        ("h{b}", "h{b}"),
        ("open{", "open{"),
        ("slash", "slash/"),
    ] {
        let real = f.dir(dir);
        std::fs::create_dir_all(real.join("Library/Caches")).unwrap();
        let ws = f.dir(&format!("{dir}/ws"));
        let home = PathBuf::from(format!("{}/{home}", f.root.display()));
        assert_eq!(
            rust(&home, &ws, &HashMap::new(), &args, Tool::Targo),
            args,
            "{home:?}"
        );
        for shell in shells() {
            assert_eq!(
                sh(&shell, Tool::Targo, &home, &ws, &HashMap::new(), &args),
                args,
                "{shell:?} {home:?}"
            );
        }
    }
    assert_eq!(config_value(Path::new("/o'h")), None);
    assert_eq!(config_value(Path::new("/h{b}")), None);
}

/// A WORKING DIRECTORY REACHED THROUGH A LINK is left alone by both halves: Cargo names
/// the workspace by its real path, which may hold a brace or lie outside the home. (The
/// shim sees the link in `$PWD`; the Rust half sees `$PWD` differ from the real folder.)
#[cfg(unix)]
#[test]
fn a_working_directory_reached_through_a_link_is_left_alone() {
    let f = Fixture::new("build-cache-link");
    let build = strings(&["--unverified", "build"]);
    for (link, real) in [
        ("home/in", "home/plain"),
        ("home/braced", "home/real{x}"),
        ("home/out", "elsewhere/ws"),
    ] {
        let real = f.dir(real);
        let link = f.root.join(link);
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let env = HashMap::from([("PWD", link.display().to_string())]);
        assert_eq!(
            rust(&f.home, &real, &env, &build, Tool::Targo),
            build,
            "{link:?}"
        );
        for shell in shells() {
            assert_eq!(
                sh(&shell, Tool::Targo, &f.home, &real, &env, &build),
                build,
                "{shell:?} {link:?}"
            );
        }
    }
    check(
        &f,
        &f.root.join("home/plain"),
        &[],
        &["--unverified", "build"],
        true,
    );
}

/// AN UPSTREAM CARGO: no lane flag to look for, and a leading `+<toolchain>` (rustup's)
/// stays first, the `--config` right after it.
#[test]
fn an_upstream_cargo_keeps_its_toolchain_first() {
    let f = Fixture::new("build-cache-cargo");
    let ws = f.dir("home/ws");
    let env = HashMap::new();
    let v = f.value();
    let run = |args: &[&str]| rust(&f.home, &ws, &env, &strings(args), Tool::Cargo);
    assert_eq!(run(&["build"]), strings(&["--config", &v, "build"]));
    assert_eq!(
        run(&["+1.97.1", "test"]),
        strings(&["+1.97.1", "--config", &v, "test"])
    );
    assert_eq!(
        run(&["+nightly", "metadata"]),
        strings(&["+nightly", "metadata"])
    );
    assert_eq!(run(&["install", "x"]), strings(&["install", "x"]));
    assert_eq!(
        run(&["build", "--target-dir", "t"]),
        strings(&["build", "--target-dir", "t"])
    );
}

/// Which cargo reads `build.build-dir` and its templates: stable 1.91 on, any
/// pre-release from 1.92 on (Trust's `1.99.0-dev`), never an unreadable release.
#[test]
fn the_release_gate_reads_the_version_line() {
    for yes in [
        "1.91.0",
        "1.91.1",
        "1.92.0-nightly",
        "1.97.1",
        "1.99.0-dev",
        "2.0.0",
    ] {
        assert!(release_reads_build_dir(yes), "{yes}");
    }
    for no in [
        "1.90.0",
        "1.91.0-nightly",
        "1.85.1",
        "0.99.0",
        "",
        "x.y",
        "1",
    ] {
        assert!(!release_reads_build_dir(no), "{no}");
    }
}

/// THE PATH NAMES THE WORKSPACE, and two workspaces never share a folder: the value Cargo
/// expands, and the directory it lands on, for nested and sibling workspaces.
#[test]
fn every_workspace_gets_its_own_folder_under_the_cache() {
    let root = Path::new("/h/Library/Caches/aterm/cargo-build.noindex");
    assert_eq!(
        config_value(root).unwrap(),
        "build.build-dir='/h/Library/Caches/aterm/cargo-build.noindex{workspace-root}/.cargo-build'"
    );
    let dirs: Vec<PathBuf> = [
        "/h/p",
        "/h/p/sub",
        "/h/p.cargo-build",
        "/h/p/.cargo-build",
        "/h/q",
    ]
    .iter()
    .map(|w| build_dir(root, Path::new(w)))
    .collect();
    assert_eq!(
        dirs[0],
        Path::new("/h/Library/Caches/aterm/cargo-build.noindex/h/p/.cargo-build")
    );
    for (i, a) in dirs.iter().enumerate() {
        for b in &dirs[i + 1..] {
            assert_ne!(a, b);
        }
    }
}

/// THE RENDERED SHIM: a `targo` shim carries the block (on macOS) ahead of its unchanged
/// `exec`, so every reader of a shim's target and environment answers as before; any
/// other tool's shim is the plain one.
#[test]
fn the_targo_shim_carries_the_block_and_reads_as_before() {
    let targo = Path::new("/p/store/trust/9192/bin/targo");
    let env = crate::shim_env::ShimEnv::admit(&["A=1".to_owned()]).unwrap();
    let body = crate::platform::sh_shim_content_routed(targo, &env, None);
    assert_eq!(
        body.contains(&block(Tool::Targo)),
        cfg!(target_os = "macos"),
        "{body}"
    );
    let lint = crate::platform::sh_shim_content_routed(
        Path::new("/p/store/trust/9192/bin/targo-tippy"),
        &env,
        None,
    );
    assert_eq!(
        lint.contains(&block(Tool::Tippy)),
        cfg!(target_os = "macos"),
        "{lint}"
    );
    assert_ne!(block(Tool::Tippy), block(Tool::Targo));
    assert_eq!(
        crate::platform::parse_sh_shim_target(&body).as_deref(),
        Some(targo)
    );
    assert_eq!(crate::platform::parse_sh_shim_env(&body), env);
    assert!(
        body.ends_with("exec '/p/store/trust/9192/bin/targo' \"$@\"\n"),
        "{body}"
    );
    for line in SH_BLOCK.lines() {
        let line = line.trim();
        assert!(
            !line.starts_with("exec") && !line.starts_with("export "),
            "{line}"
        );
    }
    let tippy = crate::platform::sh_shim_content_routed(
        Path::new("/p/store/trust/9192/bin/tippy"),
        &env,
        None,
    );
    assert!(!tippy.contains("__atb"), "{tippy}");
}

/// THE SHIM, RUN WHOLE (macOS): its `exec` hands the real tool `--config …` first and the
/// caller's arguments after it. The tool here is `/bin/echo` under the name `targo` (a
/// link: a freshly written script would wait on the system's first-run assessment).
#[cfg(target_os = "macos")]
#[test]
fn the_rendered_shim_hands_targo_the_config() {
    let f = Fixture::new("build-cache-shim");
    let ws = f.dir("home/ws");
    let bin = f.dir("store/bin");
    std::os::unix::fs::symlink("/bin/echo", bin.join("targo")).unwrap();
    let shim = f.root.join("shim");
    let body = crate::platform::sh_shim_content_routed(
        &bin.join("targo"),
        &crate::shim_env::ShimEnv::NONE,
        None,
    );
    std::fs::write(&shim, body).unwrap();
    let run = |args: &[&str]| {
        let out = aterm_uds::spawnfd::command("/bin/sh")
            .arg(&shim)
            .args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &f.home)
            .env("PWD", &ws)
            .current_dir(&ws)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    assert_eq!(
        run(&["--unverified", "test", "-q"]),
        format!("--config {} --unverified test -q\n", f.value())
    );
    assert_eq!(run(&["trust", "check"]), "trust check\n");
}

/// Sets `path`'s modification time `ago` before now.
#[cfg(unix)]
fn age_by(path: &Path, ago: Duration) {
    let t = SystemTime::now() - ago;
    std::fs::File::open(path).unwrap().set_modified(t).unwrap();
}

/// Lays `<root>W/.cargo-build/debug/deps/x` and ages every level by `ago`.
#[cfg(unix)]
fn lay(root: &Path, workspace: &Path, ago: Duration) -> PathBuf {
    let leaf = build_dir(root, workspace);
    let deps = leaf.join("debug/deps");
    std::fs::create_dir_all(&deps).unwrap();
    std::fs::write(deps.join("x"), b"0123456789").unwrap();
    for p in [
        deps.join("x"),
        deps.clone(),
        leaf.join("debug"),
        leaf.clone(),
    ] {
        age_by(&p, ago);
    }
    leaf
}

#[cfg(unix)]
const DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// RECLAIM: a build directory unwritten for a day whose workspace is seen gone goes (its
/// folder there without a manifest, or the folder above it there without it); a live one
/// (`Cargo.toml` or Trust's `Targo.toml`), one gone with its parent folder, one in a
/// folder aterm never looks into, and any written within the day stay — until a month
/// unwritten, when any goes. A `.cargo-build` that is a link stays (and what it names), a
/// mirror folder that is a link is never entered, a removal an earlier pass left is
/// finished, and the mirror folders left empty go.
#[cfg(unix)]
#[test]
fn reclaim_removes_only_what_nobody_builds_into() {
    let f = Fixture::new("build-cache-reclaim");
    let root = cache_root(&f.home);
    std::fs::create_dir_all(&root).unwrap();
    let home = Some(f.home.as_path());

    let live = f.dir("home/live");
    std::fs::write(live.join("Cargo.toml"), "[workspace]\n").unwrap();
    let live_leaf = lay(&root, &live, 2 * DAY);
    let native = f.dir("home/native");
    std::fs::write(native.join("Targo.toml"), "[workspace]\n").unwrap();
    let native_leaf = lay(&root, &native, 2 * DAY);
    let gone = f.root.join("home/gone"); // removed, its parent `home` stands
    let gone_leaf = lay(&root, &gone, 2 * DAY);
    let fresh = f.root.join("home/fresh");
    let fresh_leaf = lay(&root, &fresh, Duration::from_secs(60 * 60));
    let emptied = f.dir("home/emptied"); // the folder stands, its manifest is gone
    let emptied_leaf = lay(&root, &emptied, 3 * DAY);
    let away = f.root.join("home/away/ws"); // gone with its parent folder
    let away_leaf = lay(&root, &away, 2 * DAY);
    let private = f.root.join("home/Documents/ws"); // never looked into
    let private_leaf = lay(&root, &private, 2 * DAY);
    let volume = Path::new("/Volumes/aterm-test-no-such-volume/ws");
    let volume_leaf = lay(&root, volume, 2 * DAY);
    let mirror = |ws: &Path| root.join(ws.strip_prefix("/").unwrap());
    // A pass that died between the rename and the removal, alone and beside a build
    // directory that goes now.
    let leftover = mirror(&f.root.join("home/left")).join(DOOMED);
    std::fs::create_dir_all(leftover.join("debug")).unwrap();
    let beside = mirror(&emptied).join(DOOMED);
    std::fs::create_dir_all(beside.join("debug")).unwrap();

    // A `.cargo-build` link to a folder outside the cache.
    let outside = f.dir("outside");
    std::fs::write(outside.join("keep"), b"k").unwrap();
    let linked_leaf = build_dir(&root, &f.root.join("home/linked"));
    std::fs::create_dir_all(linked_leaf.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&outside, &linked_leaf).unwrap();
    // A mirror folder that is a link to a tree holding a `.cargo-build`.
    let elsewhere = f.dir("elsewhere");
    let planted = lay(&elsewhere, Path::new("/x"), 40 * DAY);
    let via = root.join("via");
    std::os::unix::fs::symlink(&elsewhere, &via).unwrap();

    let mut removed = reclaim(&root, home, SystemTime::now());
    removed.sort();
    let mut want = vec![emptied_leaf.clone(), gone_leaf.clone()];
    want.sort();
    assert_eq!(removed, want);
    assert!(!mirror(&gone).exists() && !mirror(&emptied).exists());
    assert!(!leftover.exists() && !mirror(&f.root.join("home/left")).exists());
    assert!(!beside.exists());
    for kept in [
        &live_leaf,
        &native_leaf,
        &fresh_leaf,
        &away_leaf,
        &private_leaf,
        &volume_leaf,
        &planted,
    ] {
        assert!(kept.is_dir(), "{} kept", kept.display());
    }
    assert!(
        std::fs::symlink_metadata(&linked_leaf)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(outside.join("keep").is_file());
    assert!(
        std::fs::symlink_metadata(&via)
            .unwrap()
            .file_type()
            .is_symlink()
    );

    // Past a month unwritten, every one goes, the live ones too (and the gone one that
    // was fresh, a day on).
    let mut removed = reclaim(&root, home, SystemTime::now() + 29 * DAY);
    removed.sort();
    let mut want = vec![
        live_leaf,
        native_leaf,
        fresh_leaf,
        away_leaf,
        private_leaf,
        volume_leaf,
    ];
    want.sort();
    assert_eq!(removed, want);
    assert!(planted.is_dir());

    // A cache root that is a link is not walked at all.
    let old = lay(&root, &f.root.join("home/old"), 40 * DAY);
    let linked_root = f.root.join("linked-root");
    std::os::unix::fs::symlink(&root, &linked_root).unwrap();
    assert!(reclaim(&linked_root, home, SystemTime::now()).is_empty());
    assert!(old.is_dir());
}

/// Ages every entry of the tree at `dir` (`dir` too) by `ago`, never following a link.
#[cfg(unix)]
fn age_tree(dir: &Path, ago: Duration) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let t = entry.file_type().unwrap();
        if t.is_dir() {
            age_tree(&entry.path(), ago);
        } else if t.is_file() {
            age_by(&entry.path(), ago);
        }
    }
    age_by(dir, ago);
}

/// A BUILD WITH A TARGET, AND A BUILD RUNNING. A build that names a target triple writes
/// `<triple>/<profile>/deps`, a level deeper: written today, its build directory stays
/// however old the rest. One whose cargo lock (`.cargo-build-lock`, in a profile or a
/// triple's profile) a build holds stays, and so does one whose lock is a link; released,
/// a month unwritten, it goes. NEGATIVE CONTROL: the same tree unwritten for a month goes.
#[cfg(unix)]
#[test]
fn reclaim_reads_a_build_with_a_target_and_keeps_one_running() {
    let f = Fixture::new("build-cache-reclaim-running");
    let root = cache_root(&f.home);
    let home = Some(f.home.as_path());
    let triple = "aarch64-apple-darwin";
    let lay_triple = |name: &str| {
        let leaf = build_dir(&root, &f.root.join("home").join(name));
        for dir in ["debug/build", &format!("{triple}/debug/deps")] {
            std::fs::create_dir_all(leaf.join(dir)).unwrap();
        }
        std::fs::write(leaf.join(format!("{triple}/debug/deps/x")), b"x").unwrap();
        for lock in [
            "debug/.cargo-build-lock",
            &format!("{triple}/debug/.cargo-build-lock"),
        ] {
            std::fs::write(leaf.join(lock), b"").unwrap();
        }
        age_tree(&leaf, 40 * DAY);
        leaf
    };
    let cross = lay_triple("cross");
    age_by(&cross.join(format!("{triple}/debug/deps")), Duration::ZERO);
    let idle = lay_triple("idle");
    let running = lay_triple("running");
    let running_cross = lay_triple("running-cross");
    let linked = lay_triple("linked");
    let lock = linked.join("debug/.cargo-build-lock");
    std::fs::remove_file(&lock).unwrap();
    std::os::unix::fs::symlink(f.dir("elsewhere").join("lock"), &lock).unwrap();
    age_tree(&linked, 40 * DAY);
    let hold = |p: PathBuf| crate::lock::Flock::try_lock(std::fs::File::open(p).unwrap()).unwrap();
    let held = [
        hold(running.join("debug/.cargo-build-lock")),
        hold(running_cross.join(format!("{triple}/debug/.cargo-build-lock"))),
    ];

    assert_eq!(reclaim(&root, home, SystemTime::now()), vec![idle]);
    for kept in [&cross, &running, &running_cross, &linked] {
        assert!(kept.is_dir(), "{} kept", kept.display());
    }
    drop(held);
    let mut removed = reclaim(&root, home, SystemTime::now());
    removed.sort();
    assert_eq!(removed, vec![running, running_cross]);
    assert!(cross.is_dir() && linked.is_dir());
}

/// A WORKSPACE REACHED THROUGH A LINK BELOW `$HOME` IS NEVER LOOKED THROUGH: Cargo names a
/// workspace by the path it was given, so `~/links/doc` (a link into `~/Documents`) or
/// `~/via/ws` (`via` a link to `~/Documents`) can name a build directory, and the gone
/// rule looks at each folder on the way before anything under it. NEGATIVE CONTROL: a real
/// folder without a manifest is looked into, and seen gone.
#[cfg(unix)]
#[test]
fn a_workspace_reached_through_a_link_is_never_looked_through() {
    let f = Fixture::new("build-cache-link-gone");
    let target = f.dir("home/Documents/viaLink");
    f.dir("home/links");
    std::os::unix::fs::symlink(&target, f.home.join("links/doc")).unwrap();
    std::os::unix::fs::symlink(f.home.join("Documents"), f.home.join("via")).unwrap();
    for (checkout, link) in [
        (f.home.join("links/doc"), f.home.join("links/doc")),
        (f.home.join("via/viaLink"), f.home.join("via")),
    ] {
        let mut looked = Vec::new();
        let verdict = checkout::gone_by(&checkout, Some(&f.home), &mut |p| {
            looked.push(p.to_path_buf());
            std::fs::symlink_metadata(p).map(|m| m.is_dir())
        });
        assert!(
            verdict
                .as_ref()
                .is_err_and(|why| why.contains("a link is not followed")),
            "{}: {verdict:?}",
            checkout.display()
        );
        assert_eq!(looked.last(), Some(&link), "{looked:?}");
        assert!(
            !looked.iter().any(|p| p.starts_with(&link) && *p != link),
            "{} looked through the link: {looked:?}",
            checkout.display()
        );
    }
    let real = f.dir("home/real/ws");
    let mut looked = Vec::new();
    let verdict = checkout::gone_by(&real, Some(&f.home), &mut |p| {
        looked.push(p.to_path_buf());
        std::fs::symlink_metadata(p).map(|m| m.is_dir())
    });
    assert!(verdict.is_ok(), "{verdict:?}");
    assert!(looked.contains(&real.join("Cargo.toml")), "{looked:?}");
}
