// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::collections::HashMap;
use std::ffi::OsString;
#[cfg(unix)]
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

    #[cfg(unix)]
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

/// The Rust half on the fixture's real files, rendered as the shim's harness prints it:
/// `env=<dir>` first where it exports one, then the arguments it leaves.
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
    match decide(args, tool, &m) {
        Some(BuildDir::TrustEnv(dir)) => std::iter::once(format!("env={dir}"))
            .chain(args.iter().cloned())
            .collect(),
        _ => with_build_dir(args, tool, &m).unwrap_or_else(|| args.to_vec()),
    }
}

/// What a case must come out as.
#[cfg(unix)]
#[derive(Debug, Clone, Copy)]
enum Want<'a> {
    /// As typed.
    Typed,
    /// The shared cache's `--config` first.
    Cache,
    /// `--config build.build-dir='<dir>'` first: the target directory's own folder.
    Target(&'a str),
    /// `CARGO_BUILD_BUILD_DIR=<dir>` exported for `targo trust`, the line as typed.
    Trust(&'a str),
}

/// `true` is the shared cache, `false` the line as typed.
#[cfg(unix)]
impl From<bool> for Want<'_> {
    fn from(cache: bool) -> Self {
        if cache { Self::Cache } else { Self::Typed }
    }
}

/// The shells the block must parse alike: macOS's `/bin/sh` (bash in POSIX mode), and
/// dash, ksh and zsh's sh emulation where installed (any of them can be `/bin/sh`).
#[cfg(unix)]
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

/// The `sh` half, run for real: the block, then `env=<dir>` where it exports one and the
/// arguments it leaves, one per line.
#[cfg(unix)]
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
            "{}[ -z \"$__atb_D\" ] || [ -z \"$__atb_e\" ] || printf 'env=%s\\n' \"$CARGO_BUILD_BUILD_DIR\"\n[ $# -eq 0 ] || printf '%s\\n' \"$@\"",
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

/// One case through both halves for `targo`: what it must come out as ([`Want`]), and
/// every shell and the Rust rule must agree.
#[cfg(unix)]
fn check<'a>(
    f: &Fixture,
    cwd: &Path,
    env: &[(&'static str, &str)],
    args: &[&str],
    want: impl Into<Want<'a>>,
) {
    check_tool(f, Tool::Targo, cwd, env, args, want);
}

/// [`check`] for `tool`'s shim.
#[cfg(unix)]
fn check_tool<'a>(
    f: &Fixture,
    tool: Tool,
    cwd: &Path,
    env: &[(&'static str, &str)],
    args: &[&str],
    want: impl Into<Want<'a>>,
) {
    let env: HashMap<&str, String> = env.iter().map(|(k, v)| (*k, (*v).to_owned())).collect();
    let args = strings(args);
    let first = match want.into() {
        Want::Typed => vec![],
        Want::Cache => vec!["--config".to_owned(), f.value()],
        Want::Target(dir) => vec!["--config".to_owned(), format!("build.build-dir='{dir}'")],
        Want::Trust(dir) => vec![format!("env={dir}")],
    };
    let want: Vec<String> = first.into_iter().chain(args.iter().cloned()).collect();
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

/// Every rule, in both halves and every shell found: a building verb with `--unverified`
/// (or `clean`) under `$HOME` gets the cache; one into a target directory it chose, or
/// with no cache to write, gets `<target>/build.noindex`, and `targo trust` gets that
/// folder in its environment; a build directory of the user's (a variable, a flag, a
/// config line), a folder outside the home or inside the cache, a target already in a
/// `*.noindex` folder, and the verbs that do not build keep it as typed.
#[cfg(unix)]
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
        &["--unverified", "metadata"],
        &["--unverified", "install", "x"],
        &["new", "x"],
        &["--unverified", "verify"],
        &["--unverified", "build", "--target-dir"],
        &["--unverified", "build", "--target-dir="],
        &["--unverified", "build", "--target-dir", "x.noindex"],
        &[
            "--unverified",
            "build",
            "--target-dir",
            "/t/lane.noindex/sub",
        ],
        &["--unverified", "build", "--target-dir", "o'x"],
        &["--unverified", "build", "--target-dir", "x{y}"],
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
    let ws_t = format!("{}/t/build.noindex", ws.display());
    check_tool(
        &f,
        Tool::Tippy,
        &ws,
        &[],
        &["--target-dir", "t"],
        Want::Target(&ws_t),
    );
    check_tool(
        &f,
        Tool::Tippy,
        &ws,
        &[("CARGO_TARGET_DIR", "/t")],
        &[],
        Want::Target("/t/build.noindex"),
    );
    check_tool(&f, Tool::Tippy, &f.dir("elsewhere/lint"), &[], &[], false);

    let build = &["--unverified", "build"][..];
    let trust = &["trust", "check", "--lib"][..];
    let x = Want::Target("/x/build.noindex");
    for var in ["CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR"] {
        check(&f, &ws, &[(var, "/x")], build, x);
        check(&f, &ws, &[(var, "/x")], &["clean"], x);
        check(
            &f,
            &ws,
            &[(var, "/x")],
            trust,
            Want::Trust("/x/build.noindex"),
        );
        check(&f, &ws, &[(var, "")], build, false);
        check(&f, &ws, &[(var, "/x/lane.noindex")], build, false);
    }
    check(&f, &ws, &[("CARGO_BUILD_BUILD_DIR", "/x")], build, false);
    check(&f, &ws, &[("CARGO_BUILD_BUILD_DIR", "")], build, false);
    check(&f, &ws, &[("CARGO_BUILD_BUILD_DIR", "/x")], trust, false);

    // A chosen target directory: the flag outranks CARGO_TARGET_DIR, which outranks
    // CARGO_BUILD_TARGET_DIR, and a relative one is the working directory's.
    let lane = format!("{}/target/lane/build.noindex", ws.display());
    check(
        &f,
        &ws,
        &[("CARGO_TARGET_DIR", "target/lane")],
        build,
        Want::Target(&lane),
    );
    check(
        &f,
        &ws,
        &[],
        &["--unverified", "build", "--target-dir=target/lane"],
        Want::Target(&lane),
    );
    check(
        &f,
        &ws,
        &[("CARGO_TARGET_DIR", "/b")],
        &["--unverified", "test", "--target-dir", "target/lane"],
        Want::Target(&lane),
    );
    check(
        &f,
        &ws,
        &[("CARGO_TARGET_DIR", "/x"), ("CARGO_BUILD_TARGET_DIR", "/b")],
        build,
        x,
    );
    check(
        &f,
        &ws,
        &[("CARGO_BUILD_TARGET_DIR", "/x")],
        &["--unverified", "build", "--", "--target-dir", "y"],
        x,
    );
    check(&f, &ws, &[("CARGO_TARGET_DIR", "o'q")], build, false);

    // Trust's verified lane builds into Cargo's default target: its own folder there,
    // through the environment.
    let default = Want::Trust("{workspace-root}/target/build.noindex");
    check(&f, &ws, &[], trust, default);
    check(&f, &ws, &[], &["trust", "test", "-p", "x"], default);
    check(&f, &f.dir("elsewhere/trusted"), &[], trust, false);
    check(&f, &f.dir("home/lab.noindex/ws"), &[], trust, false);
    check(&f, &f.dir("home/lab.noindex/ws"), &[], build, true);

    // Config files up the tree: a line naming either key chooses unless it is a comment
    // (a mention after a value counts: the safe side), the legacy name counts, and an
    // ancestor's file counts for every folder below it.
    f.file(
        "home/chosen/.cargo/config.toml",
        "[build]\ntarget-dir = \"out\"\n",
    );
    let chosen = f.dir("home/chosen");
    check(&f, &chosen, &[], build, false);
    check(&f, &chosen, &[], trust, false);
    // A target-dir line is outranked by a chosen target; a build-dir line never is.
    check(&f, &chosen, &[("CARGO_TARGET_DIR", "/x")], build, x);
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
    let up = f.dir("home/up/a/b");
    check(&f, &up, &[], build, false);
    check(&f, &up, &[("CARGO_TARGET_DIR", "/x")], build, false);
    check(&f, &up, &[("CARGO_TARGET_DIR", "/x")], trust, false);
    f.file("cargohome/config.toml", "[build]\ntarget-dir = \"t\"\n");
    let cargo_home = f.root.join("cargohome").display().to_string();
    check(&f, &ws, &[("CARGO_HOME", &cargo_home)], build, false);
    f.file("home/.cargo/config.toml", "[net]\noffline = true\n");
    check(&f, &ws, &[], build, true);
    f.file("home/.cargo/config.toml", "build.target-dir = \"t\"\n");
    check(&f, &ws, &[], build, false);
    std::fs::remove_file(f.root.join("home/.cargo/config.toml")).unwrap();

    // An alias builds as what its first word builds, defined under `[alias]` or as
    // `alias.<name>` anywhere, in an ancestor's config or CARGO_HOME's.
    f.file(
        "home/aliased/.cargo/config.toml",
        "alias.ship = \"run --profile gate -p aterm-release --\"\n[build]\njobs = 2\n\
         [alias]\n# lint = \"tippy\"\nverify = \"run -q -p aterm-verify --\"\n\
         lab = [\"run\", \"-q\", \"-p\", \"aterm-lab\", \"--\"]\ntree2 = \"tree -d\"\n\
         [env]\nfake = \"build\"\n",
    );
    let aliased = f.dir("home/aliased/sub");
    for verb in ["verify", "lab", "ship"] {
        check(&f, &aliased, &[], &["--unverified", verb, "x"], true);
    }
    for args in [
        &["--unverified", "tree2"][..],
        &["--unverified", "fake"],
        &["--unverified", "lint"],
        &["--unverified", "verif"],
        &["--unverified", "verifyx"],
        &["--unverified", "nope"],
        &["verify"],
        &["--unverified"],
    ] {
        check(&f, &aliased, &[], args, false);
    }
    check(
        &f,
        &aliased,
        &[("CARGO_TARGET_DIR", "/x")],
        &["--unverified", "verify"],
        x,
    );
    f.file(
        "cargohome2/config.toml",
        "[alias]\nxt = \"run -p xtask --\"\n",
    );
    let cargo_home2 = f.root.join("cargohome2").display().to_string();
    check(
        &f,
        &ws,
        &[("CARGO_HOME", &cargo_home2)],
        &["--unverified", "xt"],
        true,
    );
    check(&f, &ws, &[], &["--unverified", "xt"], false);

    // Where the command runs: outside the home, the home itself, the cache.
    check(&f, &f.dir("elsewhere/ws"), &[], build, false);
    check(&f, &f.home, &[], build, false);
    let cache = cache_root(&f.home).join("x");
    std::fs::create_dir_all(&cache).unwrap();
    check(&f, &cache, &[], build, false);
    std::fs::remove_dir_all(f.home.join("Library/Caches/aterm")).unwrap();

    // A cache this process cannot write (a containment tab's), or no caches folder at all:
    // the build keeps its intermediates in its own target directory's folder.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let own = Want::Target("{workspace-root}/target/build.noindex");
        let caches = f.home.join("Library/Caches");
        std::fs::set_permissions(&caches, std::fs::Permissions::from_mode(0o555)).unwrap();
        check(&f, &ws, &[], build, own);
        std::fs::set_permissions(&caches, std::fs::Permissions::from_mode(0o755)).unwrap();
        check(&f, &ws, &[], build, true);
        std::fs::remove_dir(&caches).unwrap();
        check(&f, &ws, &[], build, own);
        check(&f, &f.dir("home/lab.noindex/ws"), &[], build, false);
    }
}

/// A home Cargo cannot take gets nothing, in both halves: one a TOML literal string cannot
/// hold (a `'`), one Cargo would read as a template (a `{` or `}`), and one ending in `/`.
#[cfg(unix)]
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

/// A working directory reached through a link is left alone by both halves: Cargo names the
/// workspace by its real path, which may hold a brace or lie outside the home. (The shim
/// sees the link in `$PWD`; the Rust half sees `$PWD` differ from the real folder.)
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

/// An upstream cargo has no lane flag, and a leading `+<toolchain>` (rustup's) stays first,
/// the `--config` right after it.
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
    assert_eq!(run(&["trust", "check"]), strings(&["trust", "check"]));
    let t = format!("build.build-dir='{}/t/build.noindex'", ws.display());
    assert_eq!(
        run(&["+1.97.1", "build", "--target-dir", "t"]),
        strings(&["+1.97.1", "--config", &t, "build", "--target-dir", "t"])
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

/// The path names the workspace, and two workspaces never share a folder: the value Cargo
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

/// A `targo` shim carries the block (on macOS) ahead of its unchanged `exec`, so every
/// reader of a shim's target and environment answers the same; any other tool's shim is the
/// plain one.
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
    // It never execs, and exports only `targo trust`'s build directory.
    for line in SH_BLOCK.lines() {
        let line = line.trim();
        assert!(!line.starts_with("exec"), "{line}");
        assert!(
            !line.starts_with("export ") || line == "export CARGO_BUILD_BUILD_DIR",
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

/// The shim, run whole (macOS): its `exec` hands the real tool `--config …` first and the
/// caller's arguments after. The tool is `/bin/echo` linked as `targo` (a freshly written
/// script would wait on the system's first-run assessment).
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
    assert_eq!(
        run(&["--unverified", "build", "--target-dir", "t"]),
        format!(
            "--config build.build-dir='{}/t/build.noindex' --unverified build --target-dir t\n",
            ws.display()
        )
    );
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

/// A day-unwritten build directory whose workspace is seen gone goes; a live one, one gone
/// with its parent, one aterm never looks into and a fresh one stay until a month unwritten.
/// Links are never followed, an earlier pass's removal is finished, emptied mirrors go.
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
    // The two stages an earlier pass left are finished, and said so with what went.
    let mut want = vec![
        emptied_leaf.clone(),
        gone_leaf.clone(),
        leftover.clone(),
        beside.clone(),
    ];
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

/// The watch's pass below its floor: [`reclaim_watched`] with
/// [`RECLAIM_GONE_UNDER_PRESSURE`] and no mount table, now, `record` asked of each path
/// about to go; the paths that went.
#[cfg(unix)]
fn pressure(
    root: &Path,
    home: Option<&Path>,
    record: &mut dyn FnMut(&Path) -> bool,
) -> Vec<PathBuf> {
    reclaim_watched(
        root,
        home,
        SystemTime::now(),
        RECLAIM_GONE_UNDER_PRESSURE,
        None,
        &mut |p, _| record(p),
    )
    .into_iter()
    .map(|(p, _)| p)
    .collect()
}

/// WHY EACH WENT ([`Unused`]: what the disk ledger, the watch's log and its resume say of
/// it), asked before it goes and returned with it: a gone workspace's build directory
/// past the pass's window is `Gone`, with its own idle time and that window; a live
/// workspace's past a month is `Idle` — its workspace still stands, and nothing is said
/// of it; a stage an earlier pass left is `Unfinished`. NEGATIVE CONTROL: the live
/// workspace's directory two days idle stays, asked nothing.
#[cfg(unix)]
#[test]
fn reclaim_watched_says_why_each_directory_went() {
    let f = Fixture::new("build-cache-why");
    let root = cache_root(&f.home);
    std::fs::create_dir_all(&root).unwrap();
    let home = Some(f.home.as_path());
    let quarter_hour = Duration::from_secs(15 * 60);
    let gone_leaf = lay(&root, &f.root.join("home/wt-removed"), quarter_hour);
    let live = f.dir("home/live");
    std::fs::write(live.join("Cargo.toml"), "[workspace]\n").unwrap();
    let live_leaf = lay(&root, &live, 40 * DAY);
    let recent = f.dir("home/recent");
    std::fs::write(recent.join("Cargo.toml"), "[workspace]\n").unwrap();
    let recent_leaf = lay(&root, &recent, 2 * DAY);
    let stage = build_dir(&root, &f.root.join("home/orphan")).with_file_name(DOOMED);
    std::fs::create_dir_all(&stage).unwrap();

    let mut asked = Vec::new();
    let mut went = reclaim_watched(
        &root,
        home,
        SystemTime::now(),
        RECLAIM_GONE_UNDER_PRESSURE,
        None,
        &mut |p, why| {
            asked.push((p.to_path_buf(), why));
            true
        },
    );
    went.sort_by(|a, b| a.0.cmp(&b.0));
    let mut want = vec![gone_leaf.clone(), live_leaf.clone(), stage.clone()];
    want.sort();
    assert_eq!(
        went.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(),
        want
    );
    for (path, why) in &went {
        match why {
            Unused::Gone { idle, after } if *path == gone_leaf => {
                assert_eq!(*after, RECLAIM_GONE_UNDER_PRESSURE);
                assert!(*idle >= quarter_hour && *idle < DAY, "{idle:?}");
            }
            Unused::Idle { idle } if *path == live_leaf => {
                assert!(*idle >= 40 * DAY && *idle < 41 * DAY, "{idle:?}");
            }
            // The stage an earlier pass left: its intent asked, and its removal said.
            Unused::Unfinished if *path == stage => {}
            other => panic!("{}: {other:?}", path.display()),
        }
        assert!(asked.contains(&(path.clone(), *why)), "asked first");
    }
    assert!(!stage.exists());
    assert!(recent_leaf.is_dir());
    assert!(!asked.iter().any(|(p, _)| *p == recent_leaf));
    assert!(live.join("Cargo.toml").is_file(), "its workspace stands");
}

/// UNDER PRESSURE a gone workspace's build directory goes after ten minutes unwritten, not
/// a day: the removed worktree's cache that filled the disk. NEGATIVE CONTROLS, each kept
/// byte for byte: the same gone workspace written a minute ago, a live workspace's idle one,
/// one gone with its parent (away, not removed), a `.cargo-build` link out of the cache, and
/// the pressure-free sweep (a day's window) at the same instant.
#[cfg(unix)]
#[test]
fn under_pressure_a_removed_workspaces_cache_goes_after_ten_minutes() {
    let f = Fixture::new("build-cache-pressure");
    let root = cache_root(&f.home);
    std::fs::create_dir_all(&root).unwrap();
    let home = Some(f.home.as_path());
    let quarter_hour = Duration::from_secs(15 * 60);

    let gone = f.root.join("home/wt-removed");
    let gone_leaf = lay(&root, &gone, quarter_hour);
    let just_built = f.root.join("home/wt-just-built");
    let just_leaf = lay(&root, &just_built, Duration::from_secs(60));
    let live = f.dir("home/live");
    std::fs::write(live.join("Cargo.toml"), "[workspace]\n").unwrap();
    let live_leaf = lay(&root, &live, 2 * DAY);
    let away = f.root.join("home/away/ws");
    let away_leaf = lay(&root, &away, quarter_hour);
    let outside = f.dir("outside");
    std::fs::write(outside.join("keep"), b"k").unwrap();
    let linked_leaf = build_dir(&root, &f.root.join("home/linked"));
    std::fs::create_dir_all(linked_leaf.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&outside, &linked_leaf).unwrap();

    // Control: without pressure, the day's window keeps every one.
    assert!(reclaim(&root, home, SystemTime::now()).is_empty());
    assert!(gone_leaf.join("debug/deps/x").is_file());

    // WRITE-AHEAD: a directory whose intent cannot be put on record is kept.
    let mut asked = Vec::new();
    assert!(
        pressure(&root, home, &mut |p| {
            asked.push(p.to_path_buf());
            false
        })
        .is_empty()
    );
    assert_eq!(asked, vec![gone_leaf.clone()]);
    assert!(gone_leaf.join("debug/deps/x").is_file(), "unrecorded, kept");

    // Recorded, it goes — the record asked while it still stood.
    let mut stood = Vec::new();
    assert_eq!(
        pressure(&root, home, &mut |p| {
            stood.push(p.join("debug/deps/x").is_file());
            true
        }),
        vec![gone_leaf.clone()]
    );
    assert_eq!(stood, vec![true]);
    assert!(!gone_leaf.exists());
    for kept in [&just_leaf, &live_leaf, &away_leaf] {
        assert_eq!(
            std::fs::read(kept.join("debug/deps/x")).unwrap(),
            b"0123456789",
            "{} kept",
            kept.display()
        );
    }
    assert!(
        std::fs::symlink_metadata(&linked_leaf)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read(outside.join("keep")).unwrap(), b"k");
}

/// A crash can leave only the renamed stage. It still needs a fresh intent before
/// pressure reclamation removes its bytes; a denied intent leaves those bytes intact.
#[cfg(unix)]
#[test]
fn pressure_reclamation_records_an_orphaned_stage_before_removing_it() {
    let f = Fixture::new("build-cache-orphan-intent");
    let root = cache_root(&f.home);
    let home = Some(f.home.as_path());

    // Control: an ordinary, recorded leaf is sized while it stands and then removed.
    let leaf = lay(
        &root,
        &f.root.join("home/removed"),
        Duration::from_secs(15 * 60),
    );
    let mut recorded = Vec::new();
    assert_eq!(
        pressure(&root, home, &mut |p| {
            recorded.push((
                p.to_path_buf(),
                std::fs::read(p.join("debug/deps/x")).unwrap(),
            ));
            true
        }),
        vec![leaf.clone()]
    );
    assert_eq!(recorded, vec![(leaf.clone(), b"0123456789".to_vec())]);
    assert!(!leaf.exists());

    let stage = build_dir(&root, &f.root.join("home/orphan")).with_file_name(DOOMED);
    std::fs::create_dir_all(&stage).unwrap();
    let payload = stage.join("retained");
    std::fs::write(&payload, b"staged bytes need an intent").unwrap();
    let mut denied = Vec::new();
    assert!(
        pressure(&root, home, &mut |p| {
            denied.push(p.to_path_buf());
            false
        })
        .is_empty()
    );
    assert_eq!(
        std::fs::read(&payload).ok().as_deref(),
        Some(b"staged bytes need an intent".as_slice()),
        "a denied orphan-stage intent must preserve its bytes"
    );
    assert_eq!(denied, vec![stage.clone()]);

    // The same stage can go once recorded, and the callback sees its original bytes.
    let mut allowed = Vec::new();
    let _ = pressure(&root, home, &mut |p| {
        allowed.push((p.to_path_buf(), std::fs::read(p.join("retained")).unwrap()));
        true
    });
    assert_eq!(
        allowed,
        vec![(stage.clone(), b"staged bytes need an intent".to_vec())]
    );
    assert!(!stage.exists());
}

/// WHAT GOES IS SAID ONCE, WHEN IT IS GONE: a build directory whose removal is cut short
/// (a folder in it nobody may write) is not returned by the pass that moved it aside, and
/// its rest, a stage, is returned as [`Unused::Unfinished`] by the pass that finishes it —
/// the bytes it frees counted there, never twice. NEGATIVE CONTROL: while the removal
/// still cannot finish, the stage's intent is asked at each pass and nothing is returned.
#[cfg(unix)]
#[test]
fn a_removal_cut_short_is_returned_by_the_pass_that_finishes_it() {
    use std::os::unix::fs::PermissionsExt as _;
    let f = Fixture::new("build-cache-cut-short");
    let root = cache_root(&f.home);
    let home = Some(f.home.as_path());
    let leaf = lay(
        &root,
        &f.root.join("home/removed"),
        Duration::from_secs(15 * 60),
    );
    let stage = leaf.with_file_name(DOOMED);
    let deps = leaf.join("debug/deps");
    let mode = |dir: &Path, bits: u32| {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(bits)).unwrap();
    };
    mode(&deps, 0o500);
    // The test runs as no superuser: a folder nobody may write keeps its file.
    let mut asked = Vec::new();
    let first = pressure(&root, home, &mut |p| {
        asked.push(p.to_path_buf());
        true
    });
    assert!(first.is_empty(), "returned before it went: {first:?}");
    assert_eq!(asked, vec![leaf.clone()]);
    assert!(!leaf.exists() && stage.join("debug/deps/x").is_file());

    // NEGATIVE CONTROL: still cut short, the stage is asked for again and kept.
    asked.clear();
    let again = pressure(&root, home, &mut |p| {
        asked.push(p.to_path_buf());
        true
    });
    assert!(again.is_empty(), "{again:?}");
    assert_eq!(asked, vec![stage.clone()]);

    mode(&stage.join("debug/deps"), 0o700);
    let mut why = Vec::new();
    let went = reclaim_watched(
        &root,
        home,
        SystemTime::now(),
        RECLAIM_GONE_UNDER_PRESSURE,
        None,
        &mut |p, w| {
            why.push((p.to_path_buf(), w));
            true
        },
    );
    assert_eq!(went, vec![(stage.clone(), Unused::Unfinished)]);
    assert_eq!(why, went, "its intent asked before it went");
    assert!(!stage.exists());
}

/// An earlier stage must not be silently deleted to make room for a newly eligible
/// leaf. Refusing that stage's intent keeps both generations available.
#[cfg(unix)]
#[test]
fn pressure_reclamation_keeps_a_leaf_beside_an_unrecorded_stage() {
    let f = Fixture::new("build-cache-beside-intent");
    let root = cache_root(&f.home);
    let home = Some(f.home.as_path());
    let leaf = lay(
        &root,
        &f.root.join("home/removed"),
        Duration::from_secs(15 * 60),
    );
    let stage = leaf.with_file_name(DOOMED);
    std::fs::create_dir_all(&stage).unwrap();
    let payload = stage.join("retained");
    std::fs::write(&payload, b"the previous generation").unwrap();
    let mut intents = Vec::new();
    let removed = pressure(&root, home, &mut |p| {
        intents.push(p.to_path_buf());
        p != stage
    });
    assert_eq!(
        (
            std::fs::read(&payload).ok(),
            std::fs::read(leaf.join("debug/deps/x")).ok()
        ),
        (
            Some(b"the previous generation".to_vec()),
            Some(b"0123456789".to_vec())
        ),
        "denying the old stage must not delete either generation"
    );
    assert!(removed.is_empty());
    assert!(intents.contains(&stage));
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

/// A build naming a target triple writes a level deeper (`<triple>/<profile>/deps`), and a
/// build directory whose cargo lock is held, or is a link, stays until released. Negative
/// control: the same tree, idle and unlocked, goes.
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

/// A workspace reached through a link below `$HOME` is never looked through: Cargo names a
/// workspace by the path it was given, so `~/links/doc` (a link into `~/Documents`) or
/// `~/via/ws` can name a build directory, and the gone rule checks each folder on the way
/// first. Negative control: a real folder without a manifest is looked into, and seen gone.
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

/// WHAT NO BUILD READS AGAIN GOES FROM A BUILD DIRECTORY THAT STAYS ([`stale`]), on the
/// real filesystem: a live workspace's incremental crate folder four days unwritten, and its
/// unit the store's previous compiler built, go together through one recorded stage; the
/// dry run ([`plan`]) names exactly them first and moves nothing. NEGATIVE CONTROLS: while
/// a build holds the profile's lock nothing is asked or moved (and the dry run says so); a
/// refused intent moves every entry back; the fresh crate folder, the current compiler's
/// unit, the lock and `.rustc_info.json` stay throughout.
#[cfg(unix)]
#[test]
fn a_live_build_directory_loses_only_what_no_build_reads_again() {
    let f = Fixture::new("build-cache-stale");
    let root = cache_root(&f.home);
    let home = Some(f.home.as_path());
    let live = f.dir("home/live");
    std::fs::write(live.join("Cargo.toml"), "[workspace]\n").unwrap();
    let leaf = build_dir(&root, &live);
    let profile = leaf.join("debug");
    std::fs::create_dir_all(profile.join("deps")).unwrap();
    // The store's build 2 is current, its link made now; cargo last built here with it.
    let program = crate::store::default_prefix(&f.home).join("store/trust");
    let sysroot = program.join("2");
    std::fs::create_dir_all(&sysroot).unwrap();
    // The store is admitted only through folders nobody else may write
    // (`compiler_store`): each made 0755 whatever this process's umask (a
    // Linux login's 002 left them group-writable, and the store unknown).
    for dir in sysroot.ancestors().take_while(|d| d.starts_with(&f.home)) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::os::unix::fs::symlink("2", program.join("current")).unwrap();
    let info = format!(
        r#"{{"rustc_fingerprint":1,"outputs":{{"9":{{"success":true,"status":"","code":0,"stdout":"___\nlib___.rlib\n{}\noff\n","stderr":""}}}},"successes":{{}}}}"#,
        sysroot.display()
    );
    std::fs::write(leaf.join(".rustc_info.json"), info).unwrap();
    let lock = profile.join(".cargo-build-lock");
    std::fs::write(&lock, b"").unwrap();
    let unit = |hash: &str, rustc: u64| {
        let fp = profile.join(format!(".fingerprint/ws-{hash}"));
        std::fs::create_dir_all(&fp).unwrap();
        let json = format!(r#"{{"rustc":{rustc},"features":"[]"}}"#);
        std::fs::write(fp.join("lib-ws.json"), json).unwrap();
        let rlib = profile.join(format!("deps/libws-{hash}.rlib"));
        std::fs::write(&rlib, b"rlib").unwrap();
        (fp, rlib)
    };
    // Built after the link was made: compiler 2 is the current one.
    let (new_fp, new_rlib) = unit("2222222222222222", 2);
    let (old_fp, old_rlib) = unit("1111111111111111", 1);
    for p in [old_fp.join("lib-ws.json"), old_fp.clone(), old_rlib.clone()] {
        age_by(&p, 4 * DAY);
    }
    let stale_crate = profile.join("incremental/ws-1k116trr0a0xf");
    let fresh_crate = profile.join("incremental/ws-3g63pv341xugh");
    for folder in [&stale_crate, &fresh_crate] {
        std::fs::create_dir_all(folder.join("s-hneouhc0uf-1tn8o9j-2awpw")).unwrap();
        std::fs::write(folder.join("s-hneouhc0uf-1tn8o9j.lock"), b"").unwrap();
    }
    age_tree(&stale_crate, 4 * DAY);
    let stage = leaf.with_file_name(DOOMED);
    let doomed = {
        let mut d = vec![stale_crate.clone(), old_fp.clone(), old_rlib.clone()];
        d.sort();
        d
    };
    let why = Unused::Stale {
        incremental: 1,
        units: 1,
        after: RECLAIM_STALE,
    };
    let dry = |held: bool| {
        let mut planned = plan(&root, home, SystemTime::now(), RECLAIM_GONE, None);
        for p in &mut planned {
            p.paths.sort();
        }
        assert_eq!(
            planned,
            vec![Planned {
                path: leaf.clone(),
                why,
                paths: doomed.clone(),
                held,
            }]
        );
    };
    let kept = || {
        for p in [
            &new_fp,
            &new_rlib,
            &fresh_crate,
            &lock,
            &leaf.join(".rustc_info.json"),
        ] {
            assert!(p.exists(), "{} kept", p.display());
        }
    };
    let pass = |answer: bool, asked: &mut Vec<(PathBuf, Unused, usize)>| {
        reclaim_watched(
            &root,
            home,
            SystemTime::now(),
            RECLAIM_GONE,
            None,
            &mut |p, w| {
                let staged = std::fs::read_dir(p).map_or(0, |d| d.count());
                asked.push((p.to_path_buf(), w, staged));
                answer
            },
        )
    };

    dry(false);
    assert!(
        doomed.iter().all(|p| p.exists()),
        "the dry run moves nothing"
    );

    let held = crate::lock::Flock::try_lock(std::fs::File::open(&lock).unwrap()).unwrap();
    dry(true);
    let mut asked = Vec::new();
    assert!(pass(true, &mut asked).is_empty());
    assert!(
        asked.is_empty(),
        "nothing asked while a build holds the lock"
    );
    assert!(doomed.iter().all(|p| p.exists()));
    drop(held);

    assert!(pass(false, &mut asked).is_empty());
    assert_eq!(
        asked,
        vec![(stage.clone(), why, 3)],
        "the stage holds what goes"
    );
    assert!(
        doomed.iter().all(|p| p.exists()),
        "refused, every entry moved back"
    );
    assert_eq!(std::fs::read(&old_rlib).unwrap(), b"rlib");
    assert!(!stage.exists());
    kept();

    asked.clear();
    assert_eq!(pass(true, &mut asked), vec![(stage.clone(), why)]);
    assert_eq!(asked, vec![(stage.clone(), why, 3)]);
    assert!(doomed.iter().all(|p| !p.exists()) && !stage.exists());
    kept();
    assert!(plan(&root, home, SystemTime::now(), RECLAIM_GONE, None).is_empty());
}
