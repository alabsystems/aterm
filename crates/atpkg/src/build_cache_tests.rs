// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::collections::HashMap;
use std::ffi::OsString;
#[cfg(unix)]
use std::time::{Duration, SystemTime};

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

fn rust(
    home: &Path,
    cwd: &Path,
    env: &HashMap<&str, String>,
    args: &[String],
    tool: Tool,
) -> Vec<String> {
    let m = Machine {
        root: None,
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

#[cfg(unix)]
#[derive(Debug, Clone, Copy)]
enum Want<'a> {
    Typed,
    Cache,
    Target(&'a str),
    Trust(&'a str),
}

#[cfg(unix)]
impl From<bool> for Want<'_> {
    fn from(cache: bool) -> Self {
        if cache { Self::Cache } else { Self::Typed }
    }
}

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

fn block(tool: Tool) -> String {
    let name = match tool {
        Tool::Tippy => "targo-tippy",
        Tool::Targo | Tool::Cargo => "targo",
    };
    shim_block(name).unwrap()
}

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

/// The shim this version lays names the cache root the Rust rule uses, so a build that follows
/// the shim on `$PATH` ([`shim_root`]) and one that follows the rule agree once it is laid.
#[test]
fn the_laid_shim_names_this_versions_cache_root() {
    let home = Path::new("/Users//u");
    for name in ["targo", "targo-tippy"] {
        let block = shim_block(name).expect("a shim");
        assert_eq!(
            root_named_by_shim(home, &block),
            Some(cache_root(home)),
            "{name}"
        );
    }
}

/// A TEST RUN'S SCRATCH GOES QUIET: under every shell the block hands `targo --unverified test`
/// (and `t`, `bench`) `$TMPDIR`'s `.ubd` folder once that folder is this user's, so what the
/// tests make there never reaches fseventsd's clients. NEGATIVE CONTROL: a build, an absent
/// folder, a link in its place, and a `$TMPDIR` already quiet keep `$TMPDIR` as it was.
#[cfg(unix)]
#[test]
fn a_test_run_gets_the_quiet_tmpdir_once_it_is_this_users() {
    struct Unlink(PathBuf);
    impl Drop for Unlink {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let f = Fixture::new("build-cache-tmpdir");
    let ws = f.dir("home/ws");
    // The fixture is itself in a quiet folder, which the block takes for quiet already: the
    // `$TMPDIR` it reads is a name outside one, a link to the fixture's folder.
    let temp = Unlink(Path::new("/tmp").join(format!("atb-tmpdir-{}", std::process::id())));
    let _ = std::fs::remove_file(&temp.0);
    std::os::unix::fs::symlink(f.dir("T"), &temp.0).unwrap();
    let tmpdir = format!("{}/", temp.0.display());
    let quiet = temp.0.join(".ubd");
    let run = |shell: &[&str], args: &[&str], tmpdir: &str| -> String {
        let out = aterm_uds::spawnfd::command(shell[0])
            .args(&shell[1..])
            .arg("-c")
            .arg(format!("{}printf '%s' \"$TMPDIR\"", block(Tool::Targo)))
            .arg("sh")
            .args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &f.home)
            .env("PWD", &ws)
            .env("TMPDIR", tmpdir)
            .current_dir(&ws)
            .output()
            .unwrap();
        assert!(out.status.success(), "{shell:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap()
    };
    let test = ["--unverified", "test", "-p", "x"];
    for shell in shells() {
        assert_eq!(
            run(&shell, &test, &tmpdir),
            tmpdir,
            "{shell:?}: no folder yet"
        );
    }
    std::os::unix::fs::symlink(&ws, &quiet).unwrap();
    for shell in shells() {
        assert_eq!(run(&shell, &test, &tmpdir), tmpdir, "{shell:?}: a link");
    }
    std::fs::remove_file(&quiet).unwrap();
    std::fs::create_dir(&quiet).unwrap();
    let moved = format!("{}/", quiet.display());
    for shell in shells() {
        for args in [
            &test[..],
            &["--unverified", "t"],
            &["--unverified", "bench"],
        ] {
            assert_eq!(run(&shell, args, &tmpdir), moved, "{shell:?} {args:?}");
        }
        assert_eq!(
            run(&shell, &["--unverified", "build"], &tmpdir),
            tmpdir,
            "{shell:?}"
        );
        assert_eq!(
            run(&shell, &test, &moved),
            moved,
            "{shell:?}: quiet already"
        );
    }
}

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

    for dir in [
        "home/brace{x}",
        "home/open{only",
        "home/close}only",
        "home/cc/{{name}}",
    ] {
        check(&f, &f.dir(dir), &[], &["--unverified", "build"], false);
    }

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

    let default = Want::Trust("{workspace-root}/target/build.noindex");
    check(&f, &ws, &[], trust, default);
    check(&f, &ws, &[], &["trust", "test", "-p", "x"], default);
    check(&f, &f.dir("elsewhere/trusted"), &[], trust, false);
    check(&f, &f.dir("home/lab.noindex/ws"), &[], trust, false);
    check(&f, &f.dir("home/lab.noindex/ws"), &[], build, true);

    f.file(
        "home/chosen/.cargo/config.toml",
        "[build]\ntarget-dir = \"out\"\n",
    );
    let chosen = f.dir("home/chosen");
    check(&f, &chosen, &[], build, false);
    check(&f, &chosen, &[], trust, false);
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

    check(&f, &f.dir("elsewhere/ws"), &[], build, false);
    check(&f, &f.home, &[], build, false);
    let cache = cache_root(&f.home).join("x");
    std::fs::create_dir_all(&cache).unwrap();
    check(&f, &cache, &[], build, false);
    std::fs::remove_dir_all(f.home.join("Library/Caches/aterm")).unwrap();

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

/// A tool that starts a nested `targo` at a checkout it names gets the shim's answer for a build
/// typed there, whatever `$PWD` it inherited; a checkout named through a link is left alone.
#[cfg(unix)]
#[test]
fn a_build_started_at_a_named_folder_gets_that_folders_answer() {
    let f = Fixture::new("build-cache-named");
    let ws = f.dir("home/ws");
    let typed_in = f.dir("home/elsewhere");
    let bench = strings(&["--unverified", "bench", "--no-run"]);
    let env = HashMap::from([("PWD", typed_in.display().to_string())]);
    let m = Machine {
        root: None,
        home: Some(&f.home),
        cwd: Some(&typed_in),
        env: &|name| env.get(name).map(OsString::from),
        read: &read_config,
        writable: &writable,
    };
    // CONTROL: read with the inherited `$PWD`, the checkout looks reached through a link.
    let read_pwd = Machine {
        cwd: Some(&ws),
        ..m
    };
    assert_eq!(with_build_dir(&bench, Tool::Targo, &read_pwd), None);
    let mut cached = vec!["--config".to_owned(), f.value()];
    cached.extend(bench.iter().cloned());
    assert_eq!(
        with_build_dir_at(&bench, Tool::Targo, &ws, &m),
        Some(cached)
    );
    let link = f.root.join("home/link");
    std::os::unix::fs::symlink(&ws, &link).unwrap();
    assert_eq!(with_build_dir_at(&bench, Tool::Targo, &link, &m), None);
}

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

#[cfg(unix)]
fn age_by(path: &Path, ago: Duration) {
    let t = SystemTime::now() - ago;
    std::fs::File::open(path).unwrap().set_modified(t).unwrap();
}

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
    let leftover = mirror(&f.root.join("home/left")).join(DOOMED);
    std::fs::create_dir_all(leftover.join("debug")).unwrap();
    let beside = mirror(&emptied).join(DOOMED);
    std::fs::create_dir_all(beside.join("debug")).unwrap();

    let outside = f.dir("outside");
    std::fs::write(outside.join("keep"), b"k").unwrap();
    let linked_leaf = build_dir(&root, &f.root.join("home/linked"));
    std::fs::create_dir_all(linked_leaf.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&outside, &linked_leaf).unwrap();
    let elsewhere = f.dir("elsewhere");
    let planted = lay(&elsewhere, Path::new("/x"), 40 * DAY);
    let via = root.join("via");
    std::os::unix::fs::symlink(&elsewhere, &via).unwrap();

    let mut removed = reclaim(&root, home, SystemTime::now());
    removed.sort();
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

    let old = lay(&root, &f.root.join("home/old"), 40 * DAY);
    let linked_root = f.root.join("linked-root");
    std::os::unix::fs::symlink(&root, &linked_root).unwrap();
    assert!(reclaim(&linked_root, home, SystemTime::now()).is_empty());
    assert!(old.is_dir());
}

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

    assert!(reclaim(&root, home, SystemTime::now()).is_empty());
    assert!(gone_leaf.join("debug/deps/x").is_file());

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

#[cfg(unix)]
#[test]
fn pressure_reclamation_records_an_orphaned_stage_before_removing_it() {
    let f = Fixture::new("build-cache-orphan-intent");
    let root = cache_root(&f.home);
    let home = Some(f.home.as_path());

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

/// `path`'s permission bits set to `bits`.
#[cfg(unix)]
pub(super) fn mode(path: &Path, bits: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits)).unwrap();
}

/// `path`'s own permission bits.
#[cfg(unix)]
pub(super) fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o7777
}

/// Every entry under `dir`, sorted, never through a link.
#[cfg(target_os = "macos")]
fn entries(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        for entry in std::fs::read_dir(&at).unwrap().flatten() {
            if entry.file_type().unwrap().is_dir() {
                stack.push(entry.path());
            }
            found.push(entry.path());
        }
    }
    found.sort();
    found
}

/// The entry a test scratch's run left on its folder (`install_sh`'s ACL rows): it refuses
/// every remover, `rm -rf` and `std::fs::remove_dir_all` alike.
#[cfg(target_os = "macos")]
pub(super) const DENY_DELETE: &str = "group:everyone deny delete";

/// `chmod +a` each of `aces` on `path` itself (`-h`: a link's own ACL, never its target's).
#[cfg(target_os = "macos")]
pub(super) fn acl(path: &Path, aces: &[&str]) {
    for ace in aces {
        let status = aterm_uds::spawnfd::command("/bin/chmod")
            .args(["-h", "+a", ace])
            .arg(path)
            .status()
            .expect("chmod +a");
        assert!(
            status.success(),
            "chmod +a {ace} {}: {status}",
            path.display()
        );
    }
}

/// Whether `path`'s own ACL holds `ace` (`ls -led`).
#[cfg(target_os = "macos")]
pub(super) fn holds_ace(path: &Path, ace: &str) -> bool {
    let out = aterm_uds::spawnfd::command("/bin/ls")
        .arg("-led")
        .arg(path)
        .output()
        .expect("ls -led");
    String::from_utf8_lossy(&out.stdout).contains(ace)
}

/// A flag that binds even its owner (`uchg`), cleared off whichever of `at` holds it when
/// dropped, so a failed test leaves its scratch removable.
#[cfg(target_os = "macos")]
struct Unflag(Vec<PathBuf>);

#[cfg(target_os = "macos")]
impl Drop for Unflag {
    fn drop(&mut self) {
        for at in &self.0 {
            let _ = crate::platform::set_file_flags(at, 0);
        }
    }
}

/// HOW A TEST KEEPS ONE ENTRY OF A STAGE FROM GOING: read as another user's (the remover's seam:
/// it leaves such an entry as it is), on every unix; or flagged immutable (`uchg`: the remover
/// clears ACLs and owner bits, never a flag), on macOS.
#[cfg(unix)]
#[derive(Debug, Clone, Copy)]
enum Keep {
    Theirs,
    #[cfg(target_os = "macos")]
    Flagged,
}

/// A file a test keeps from going ([`Keep`]), let go when dropped, so a failed test leaves its
/// scratch removable: the flagged one by its path.
#[cfg(unix)]
enum Kept {
    Theirs,
    #[cfg(target_os = "macos")]
    Flagged(PathBuf),
}

#[cfg(unix)]
impl Kept {
    /// `path` written and kept as `keep` says. A flagged one refuses `std::fs::remove_file`
    /// itself.
    fn lay(path: &Path, keep: Keep) -> Self {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"held").unwrap();
        match keep {
            Keep::Theirs => {
                use std::os::unix::fs::MetadataExt as _;
                remove::seams::other_user(std::fs::symlink_metadata(path).unwrap().ino());
                Self::Theirs
            }
            #[cfg(target_os = "macos")]
            Keep::Flagged => {
                crate::platform::set_file_flags(path, libc::UF_IMMUTABLE).unwrap();
                assert!(
                    std::fs::remove_file(path).is_err(),
                    "the flag refuses its removal"
                );
                Self::Flagged(path.to_path_buf())
            }
        }
    }

    /// Let it go: it reads as this user's again, or its flag is cleared.
    fn release(&self) {
        match self {
            Self::Theirs => remove::seams::clear(),
            #[cfg(target_os = "macos")]
            Self::Flagged(path) => crate::platform::set_file_flags(path, 0).unwrap(),
        }
    }
}

#[cfg(unix)]
impl Drop for Kept {
    fn drop(&mut self) {
        remove::seams::clear();
        #[cfg(target_os = "macos")]
        if let Self::Flagged(path) = self {
            let _ = crate::platform::set_file_flags(path, 0);
        }
    }
}

/// ONE ENTRY THAT WILL NOT GO STRANDS NOTHING ELSE: a gone workspace's build directory holding,
/// beside a tree of its own, a file kept as `keep` says (read as another user's, which the
/// remover leaves as it is; or flagged `uchg`: the remover clears ACLs and owner bits, never a
/// flag, and no aterm tool sets one in a build directory) is renamed aside, and that pass removes
/// everything else in it and returns the stage as `Stayed`, never the leaf as removed: the one
/// entry counted, the bytes that went counted, and it named as what stayed; `reclaim` returns
/// nothing. The next pass is asked for the stage again, and it stays staged as it was, nothing
/// more of it gone; once the file is let go, the pass after finishes it, returned as `Unfinished`
/// after its intent, and the emptied mirror folder goes. NEGATIVE CONTROLS: a flagged file
/// refuses `std::fs::remove_file` itself ([`Kept::lay`]), and once let go the same file goes.
#[cfg(unix)]
fn a_removal_cut_short(keep: Keep) {
    let f = Fixture::new("build-cache-cut-short");
    let root = cache_root(&f.home);
    let home = Some(f.home.as_path());
    let ago = Duration::from_secs(15 * 60);
    let leaf = lay(&root, &f.root.join("home/removed"), ago);
    let stage = leaf.with_file_name(DOOMED);
    for i in 0..16 {
        let out = leaf.join(format!("debug/build/unit-{i}/out"));
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("generated.rs"), vec![b'x'; 8192]).unwrap();
    }
    age_tree(&leaf, ago);
    #[cfg(target_os = "macos")]
    let _unflag = Unflag(vec![
        leaf.join("debug/deps/stuck"),
        stage.join("debug/deps/stuck"),
    ]);
    let _kept = Kept::lay(&leaf.join("debug/deps/stuck"), keep);
    // The new name wrote `debug/deps`: aged back, so the leaf still reads unwritten.
    age_by(&leaf.join("debug/deps"), ago);
    let pass = |asked: &mut Vec<(PathBuf, Unused)>| {
        reclaim_watched(
            &root,
            home,
            SystemTime::now(),
            RECLAIM_GONE_UNDER_PRESSURE,
            None,
            &mut |p, w| {
                asked.push((p.to_path_buf(), w));
                true
            },
        )
    };

    let mut asked = Vec::new();
    let first = pass(&mut asked);
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0].0, leaf);
    assert!(matches!(asked[0].1, Unused::Gone { .. }), "{asked:?}");
    assert_eq!(first.len(), 1, "{first:?}");
    assert_eq!(first[0].0, stage, "the stage, never the leaf as removed");
    let Unused::Stayed { stayed, freed } = first[0].1 else {
        panic!("returned as removed before it went: {first:?}");
    };
    assert_eq!(stayed, 1);
    assert!(
        freed >= 16 * 8192,
        "the bytes that went are counted: {freed}"
    );
    assert!(!leaf.exists());
    assert_eq!(
        remove::stayed(&stage, 10),
        [stage.join("debug/deps/stuck")],
        "every other entry went"
    );
    assert_eq!(
        remove::named(&stage, stayed).first,
        [PathBuf::from("debug/deps/stuck")]
    );
    assert!(
        reclaim(&root, home, SystemTime::now()).is_empty(),
        "reclaim returns only what went whole"
    );

    asked.clear();
    let again = pass(&mut asked);
    assert_eq!(asked, [(stage.clone(), Unused::Unfinished)], "asked again");
    assert_eq!(
        again,
        [(
            stage.clone(),
            Unused::Stayed {
                stayed: 1,
                freed: 0
            }
        )]
    );
    assert_eq!(remove::stayed(&stage, 10), [stage.join("debug/deps/stuck")]);
    assert_eq!(
        std::fs::read(stage.join("debug/deps/stuck")).unwrap(),
        b"held"
    );

    // Let go by its moved path: a flagged `Kept` holds the leaf's, which the rename left stale.
    match keep {
        Keep::Theirs => remove::seams::clear(),
        #[cfg(target_os = "macos")]
        Keep::Flagged => {
            crate::platform::set_file_flags(&stage.join("debug/deps/stuck"), 0).unwrap();
        }
    }
    asked.clear();
    let went = pass(&mut asked);
    assert_eq!(went, [(stage.clone(), Unused::Unfinished)]);
    assert_eq!(asked, went, "its intent asked before it went");
    assert!(!stage.exists() && !leaf.parent().unwrap().exists());
}

#[cfg(target_os = "macos")]
#[test]
fn a_removal_cut_short_is_returned_by_the_pass_that_finishes_it() {
    a_removal_cut_short(Keep::Flagged);
}

#[cfg(unix)]
#[test]
fn a_stage_keeping_an_entry_is_asked_again_and_counted_until_it_goes() {
    a_removal_cut_short(Keep::Theirs);
}

/// A STUCK STAGE COSTS ONLY ITSELF (DISK4-1): beside `.cargo-build.removing` (the one name an
/// older aterm staged under) keeping one entry as `keep` says, a gone workspace's build directory
/// is staged under the next free name (`.cargo-build.removing-1`) and goes whole in the same
/// pass, while the stuck stage is asked for and reported once a pass (`Stayed`, its other bytes
/// counted the first time), never twice. The plan lists each once, the leaf as going. The
/// workspace builds again and the next pass takes that build directory too; once the entry lets
/// go, the old fixed-name stage is finished, returned as `Unfinished`, and its mirror folder goes.
#[cfg(unix)]
fn a_stuck_stage_costs_only_itself(keep: Keep) {
    let f = Fixture::new("build-cache-stuck-stage");
    let root = cache_root(&f.home);
    let home = Some(f.home.as_path());
    let ago = Duration::from_secs(15 * 60);
    let workspace = f.root.join("home/removed");
    let leaf = lay(&root, &workspace, ago);
    let stage = leaf.with_file_name(DOOMED);
    let next = leaf.with_file_name(format!("{DOOMED}-1"));
    std::fs::create_dir_all(stage.join("debug/build")).unwrap();
    std::fs::write(stage.join("debug/build/spare"), vec![b'x'; 64 * 1024]).unwrap();
    let held = stage.join("debug/deps/held");
    let kept = Kept::lay(&held, keep);
    let gone = |why: &Unused| matches!(why, Unused::Gone { after, .. } if *after == RECLAIM_GONE_UNDER_PRESSURE);
    let planned = || {
        plan(
            &root,
            home,
            SystemTime::now(),
            RECLAIM_GONE_UNDER_PRESSURE,
            None,
        )
    };
    let pass = |asked: &mut Vec<(PathBuf, Unused)>| {
        let mut went = reclaim_watched(
            &root,
            home,
            SystemTime::now(),
            RECLAIM_GONE_UNDER_PRESSURE,
            None,
            &mut |p, w| {
                asked.push((p.to_path_buf(), w));
                true
            },
        );
        went.sort_by(|a, b| a.0.cmp(&b.0));
        asked.sort_by(|a, b| a.0.cmp(&b.0));
        went
    };

    let before = planned();
    assert_eq!(
        before.iter().map(|p| p.path.clone()).collect::<Vec<_>>(),
        [leaf.clone(), stage.clone()],
        "{before:?}"
    );
    assert!(gone(&before[0].why) && before[1].why == Unused::Unfinished);

    for round in 0..2 {
        if round > 0 {
            assert_eq!(
                lay(&root, &workspace, ago),
                leaf,
                "the workspace built again"
            );
        }
        let mut asked = Vec::new();
        let went = pass(&mut asked);
        assert_eq!(asked.len(), 2, "round {round}, each asked once: {asked:?}");
        assert!(asked[0].0 == leaf && gone(&asked[0].1), "{asked:?}");
        assert_eq!(asked[1], (stage.clone(), Unused::Unfinished));
        assert_eq!(went.len(), 2, "round {round}, each once: {went:?}");
        assert!(
            went[0].0 == leaf && gone(&went[0].1),
            "round {round}: the leaf went whole in the same pass: {went:?}"
        );
        let (path, Unused::Stayed { stayed, freed }) = &went[1] else {
            panic!("round {round}: {went:?}");
        };
        assert_eq!((path, *stayed), (&stage, 1));
        if round == 0 {
            assert!(*freed >= 64 * 1024, "the bytes that went: {freed}");
        } else {
            assert_eq!(*freed, 0);
        }
        assert!(!leaf.exists() && !next.exists(), "round {round}");
        assert_eq!(remove::stayed(&stage, 10), std::slice::from_ref(&held));
        let after = planned();
        assert_eq!(
            after.iter().map(|p| (&p.path, p.why)).collect::<Vec<_>>(),
            [(&stage, Unused::Unfinished)],
            "round {round}"
        );
    }

    kept.release();
    let mut asked = Vec::new();
    let went = pass(&mut asked);
    assert_eq!(
        went,
        [(stage.clone(), Unused::Unfinished)],
        "the old fixed-name stage is finished"
    );
    assert_eq!(asked, went, "its intent asked before it went");
    assert!(!stage.exists() && !leaf.parent().unwrap().exists());
}

#[cfg(unix)]
#[test]
fn a_stage_keeping_another_users_entry_costs_only_itself() {
    a_stuck_stage_costs_only_itself(Keep::Theirs);
}

#[cfg(target_os = "macos")]
#[test]
fn a_stage_keeping_a_flagged_entry_costs_only_itself() {
    a_stuck_stage_costs_only_itself(Keep::Flagged);
}

/// A STUCK STAGE KEEPS NO PRUNE: beside a stage that keeps one entry ([`Keep::Theirs`]), a live
/// workspace's stale incremental folder moves into a new stage of its own
/// (`.cargo-build.removing-1`) and goes in the same pass, returned as `Stale`, while the stuck
/// stage is asked for and reported once; the build directory stays.
#[cfg(unix)]
#[test]
fn a_stuck_stage_beside_a_live_build_directory_keeps_no_prune() {
    let f = Fixture::new("build-cache-stuck-prune");
    let root = cache_root(&f.home);
    let live = f.dir("home/live");
    std::fs::write(live.join("Cargo.toml"), "[workspace]\n").unwrap();
    let leaf = build_dir(&root, &live);
    let profile = leaf.join("debug");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join(".cargo-build-lock"), b"").unwrap();
    let stale_crate = profile.join("incremental/ws-1k116trr0a0xf");
    std::fs::create_dir_all(stale_crate.join("s-hneouhc0uf-1tn8o9j-2awpw")).unwrap();
    age_tree(&stale_crate, 4 * DAY);
    let stage = leaf.with_file_name(DOOMED);
    let held = stage.join("tmp/held");
    let _kept = Kept::lay(&held, Keep::Theirs);

    let mut asked = Vec::new();
    let mut went = reclaim_watched(
        &root,
        Some(f.home.as_path()),
        SystemTime::now(),
        RECLAIM_GONE,
        None,
        &mut |p, w| {
            asked.push((p.to_path_buf(), w));
            true
        },
    );
    went.sort_by(|a, b| a.0.cmp(&b.0));
    asked.sort_by(|a, b| a.0.cmp(&b.0));
    let pruned = leaf.with_file_name(format!("{DOOMED}-1"));
    let stale = |why: &Unused| matches!(why, Unused::Stale { incremental: 1, .. });
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert_eq!(asked[0], (stage.clone(), Unused::Unfinished));
    assert!(asked[1].0 == pruned && stale(&asked[1].1), "{asked:?}");
    assert_eq!(went.len(), 2, "{went:?}");
    assert_eq!(
        went[0],
        (
            stage.clone(),
            Unused::Stayed {
                stayed: 1,
                freed: 0
            }
        )
    );
    assert!(went[1].0 == pruned && stale(&went[1].1), "{went:?}");
    assert!(!stale_crate.exists() && !pruned.exists());
    assert!(
        profile.join(".cargo-build-lock").is_file(),
        "the leaf stays"
    );
    assert_eq!(std::fs::read(&held).unwrap(), b"held");
}

/// A STUCK ASIDE KEEPS NO TARGET, AND IS SAID: beside two asides of earlier calls that each keep
/// one entry ([`Keep::Theirs`]), `<dir>.removing` (the one name an older aterm used) and the
/// numbered `<dir>.removing-2`, a target directory nothing builds in is renamed aside under the
/// lowest free name (`-1`) and goes whole, each earlier aside tried and kept as it was, and the
/// call says what stayed of each (`stayed_words`, joined), though the target went; with the target
/// gone, a call says the same; once their entries let go, a call finishes both (`Ok(false)`: no
/// target), and nothing is left beside the target.
#[cfg(unix)]
#[test]
fn a_stuck_aside_keeps_no_target_from_going() {
    let f = Fixture::new("build-cache-stuck-aside");
    let target = f.dir("target/verify/cells.noindex");
    std::fs::create_dir_all(target.join("debug/deps")).unwrap();
    std::fs::write(target.join("debug/deps/x.rmeta"), b"x").unwrap();
    let bare = target.with_file_name("cells.noindex.removing");
    let numbered = target.with_file_name("cells.noindex.removing-2");
    let held = [bare.join("debug/deps/held"), numbered.join("held")];
    let kept = held.each_ref().map(|h| Kept::lay(h, Keep::Theirs));
    let words = || {
        format!(
            "{}; {}",
            remove::stayed_words(&bare, 1),
            remove::stayed_words(&numbered, 1)
        )
    };

    let said = remove_unbuilt(&target).unwrap_err().to_string();
    assert_eq!(said, words(), "each stuck aside is said, the target gone");
    assert!(!target.exists());
    assert!(!target.with_file_name("cells.noindex.removing-1").exists());
    for h in &held {
        assert_eq!(std::fs::read(h).unwrap(), b"held");
    }
    let said = remove_unbuilt(&target).unwrap_err().to_string();
    assert_eq!(said, words(), "no target");

    kept[0].release();
    assert!(!remove_unbuilt(&target).unwrap(), "no target");
    let listed: Vec<_> = std::fs::read_dir(target.parent().unwrap())
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert!(listed.is_empty(), "nothing beside it: {listed:?}");
}

/// A LEAF THAT WILL NOT MOVE IS NEITHER PLANNED NOR ASKED FOR: a gone workspace's build directory
/// its owner flagged `uchg`, or whose own ACL denies its deletion, in a folder that takes new
/// names, cannot be renamed aside (the kernel refuses its rename as it would its removal), so the
/// plan does not list it, its intent is never asked, and it stays whole. NEGATIVE CONTROLS: each
/// refuses `std::fs::rename` itself; with the flag off and the ACL cleared, the plan lists both
/// and a pass takes both.
#[cfg(target_os = "macos")]
#[test]
fn a_leaf_that_will_not_move_is_neither_planned_nor_asked_for() {
    /// Clears the flag and the ACL off each leaf when dropped, so a failed test leaves its
    /// scratch removable.
    struct Let(Vec<PathBuf>);
    impl Drop for Let {
        fn drop(&mut self) {
            for at in &self.0 {
                let _ = crate::platform::set_file_flags(at, 0);
                let _ = aterm_uds::spawnfd::command("/bin/chmod")
                    .arg("-N")
                    .arg(at)
                    .stderr(aterm_uds::spawnfd::Stdio::null())
                    .status();
            }
        }
    }
    let f = Fixture::new("build-cache-unmovable");
    let root = cache_root(&f.home);
    let home = Some(f.home.as_path());
    let flagged = lay(&root, &f.root.join("home/flagged"), 2 * DAY);
    let denied = lay(&root, &f.root.join("home/denied"), 2 * DAY);
    let both = Let(vec![denied.clone(), flagged.clone()]);
    crate::platform::set_file_flags(&flagged, libc::UF_IMMUTABLE).unwrap();
    acl(&denied, &[DENY_DELETE]);
    for leaf in &both.0 {
        let folder = leaf.parent().unwrap();
        assert!(crate::platform::dir_writable_by_caller(folder));
        assert!(
            std::fs::rename(leaf, folder.join("moved")).is_err(),
            "{} refuses its rename",
            leaf.display()
        );
    }
    assert!(plan(&root, home, SystemTime::now(), RECLAIM_GONE, None).is_empty());
    let mut asked = Vec::new();
    let went = reclaim_watched(
        &root,
        home,
        SystemTime::now(),
        RECLAIM_GONE,
        None,
        &mut |p, w| {
            asked.push((p.to_path_buf(), w));
            true
        },
    );
    assert!(went.is_empty() && asked.is_empty(), "{went:?} {asked:?}");
    for leaf in &both.0 {
        assert!(leaf.join("debug/deps/x").is_file());
    }

    crate::platform::set_file_flags(&flagged, 0).unwrap();
    let cleared = aterm_uds::spawnfd::command("/bin/chmod")
        .arg("-N")
        .arg(&denied)
        .status()
        .unwrap();
    assert!(cleared.success());
    let mut planned: Vec<PathBuf> = plan(&root, home, SystemTime::now(), RECLAIM_GONE, None)
        .into_iter()
        .map(|p| p.path)
        .collect();
    planned.sort();
    assert_eq!(planned, both.0);
    let mut went = reclaim(&root, home, SystemTime::now());
    went.sort();
    assert_eq!(went, both.0);
}

/// A LEAF THAT CANNOT BE STAGED IS NEITHER PLANNED NOR ASKED FOR: in a mirror folder this user
/// may add no name to (0o555), a gone workspace's build directory is not listed by the plan, its
/// intent is never asked, and it stays whole. NEGATIVE CONTROL: the folder's write bit back, the
/// plan lists it and a pass takes it.
#[cfg(unix)]
#[test]
fn a_leaf_that_cannot_be_staged_is_neither_planned_nor_asked_for() {
    struct Writable(PathBuf);
    impl Drop for Writable {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
    }
    let f = Fixture::new("build-cache-unstageable");
    let root = cache_root(&f.home);
    let home = Some(f.home.as_path());
    let leaf = lay(&root, &f.root.join("home/removed"), 2 * DAY);
    let folder = Writable(leaf.parent().unwrap().to_path_buf());
    mode(&folder.0, 0o555);
    assert!(plan(&root, home, SystemTime::now(), RECLAIM_GONE, None).is_empty());
    let mut asked = Vec::new();
    let went = reclaim_watched(
        &root,
        home,
        SystemTime::now(),
        RECLAIM_GONE,
        None,
        &mut |p, w| {
            asked.push((p.to_path_buf(), w));
            true
        },
    );
    assert!(went.is_empty() && asked.is_empty(), "{went:?} {asked:?}");
    assert!(leaf.join("debug/deps/x").is_file());

    mode(&folder.0, 0o755);
    let planned: Vec<PathBuf> = plan(&root, home, SystemTime::now(), RECLAIM_GONE, None)
        .into_iter()
        .map(|p| p.path)
        .collect();
    assert_eq!(planned, std::slice::from_ref(&leaf));
    assert_eq!(
        reclaim(&root, home, SystemTime::now()),
        std::slice::from_ref(&leaf)
    );
    assert!(!leaf.exists());
}

/// EVERY STAGE NAME IS READ AS A STAGE, AND NOTHING ELSE IS: `.cargo-build.removing` and
/// `.cargo-build.removing-<n>` (n from 1, no leading zero) are stages, planned and finished once
/// each and never looked into for a workspace, while `-0`, `-01`, `-x`, a bare `-` and other
/// suffixes are mirror folders, walked as any other.
#[cfg(unix)]
#[test]
fn every_stage_name_is_read_as_a_stage_and_nothing_else_is() {
    let base = OsStr::new(DOOMED);
    for (name, stage) in [
        (DOOMED.to_owned(), true),
        (format!("{DOOMED}-1"), true),
        (format!("{DOOMED}-12"), true),
        (format!("{DOOMED}-0"), false),
        (format!("{DOOMED}-01"), false),
        (format!("{DOOMED}-x"), false),
        (format!("{DOOMED}-1x"), false),
        (format!("{DOOMED}-"), false),
        (format!("{DOOMED}x"), false),
        (LEAF.to_owned(), false),
    ] {
        assert_eq!(is_stage(OsStr::new(&name), base), stage, "{name}");
    }
    assert_eq!(stage_name(base, 0), DOOMED);
    assert_eq!(stage_name(base, 7), format!("{DOOMED}-7").as_str());

    let f = Fixture::new("build-cache-stage-names");
    let root = cache_root(&f.home);
    let ws = f.root.join("home/ws");
    let mirror = root.join(ws.strip_prefix("/").unwrap());
    let numbered = mirror.join(format!("{DOOMED}-12"));
    std::fs::create_dir_all(numbered.join(LEAF).join("debug")).unwrap();
    let walked = mirror.join(format!("{DOOMED}-01"));
    std::fs::create_dir_all(walked.join(LEAF)).unwrap();
    assert_eq!(workspaces(&root), [ws.join(format!("{DOOMED}-01"))]);
    let planned: Vec<(PathBuf, Unused)> = plan(
        &root,
        Some(f.home.as_path()),
        SystemTime::now(),
        RECLAIM_GONE,
        None,
    )
    .into_iter()
    .map(|p| (p.path, p.why))
    .collect();
    assert_eq!(planned, [(numbered.clone(), Unused::Unfinished)]);
    assert_eq!(
        reclaim(&root, Some(f.home.as_path()), SystemTime::now()),
        std::slice::from_ref(&numbered)
    );
    assert!(!numbered.exists() && walked.join(LEAF).is_dir());
}

/// THE STRANDED STAGE GOES IN ONE PASS (found live 2026-10-27, 252 GB stranded): under the root
/// the cache moved from, a stage whose FIRST entry in readdir order holds, as its `tmp/` did, a
/// test scratch's folder carrying `group:everyone deny delete` and the allow entry that run
/// added, beside a large tree. One pass clears the ACL aterm's own test put there, removes every
/// entry, returns the stage whole as `Unfinished` after its intent, and the emptied mirror
/// folders and the root go. NEGATIVE CONTROL: the same stage laid outside the cache,
/// `std::fs::remove_dir_all` — every pass's removal before — fails at that folder and leaves
/// every entry.
#[cfg(target_os = "macos")]
#[test]
fn a_stage_led_by_a_folder_denying_its_removal_goes_whole_in_one_pass() {
    let f = Fixture::new("build-cache-acl-stage");
    let old = moved_from(&f.home);
    let stage = build_dir(&old, &f.root.join("home/p2")).with_file_name(DOOMED);
    let control = f.root.join("control").join(DOOMED);
    let lay_stage = |stage: &Path| {
        for top in ["tmp", "debug", "release", "build"] {
            std::fs::create_dir_all(stage.join(top)).unwrap();
        }
        let first = || {
            std::fs::read_dir(stage)
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path()
        };
        let lead = first();
        let locked = lead.join("install-sh.20116_7cc62c51a515c111_119/tmp/d");
        std::fs::create_dir_all(&locked).unwrap();
        for sibling in ["tmp", "debug", "release", "build"]
            .map(|top| stage.join(top))
            .into_iter()
            .filter(|top| *top != lead)
        {
            for i in 0..8 {
                let deps = sibling.join(format!("deps-{i}"));
                std::fs::create_dir_all(&deps).unwrap();
                for j in 0..64 {
                    std::fs::write(deps.join(format!("lib{j}.rlib")), b"rlib").unwrap();
                }
            }
        }
        acl(
            &locked,
            &[DENY_DELETE, "group:everyone allow add_subdirectory"],
        );
        assert_eq!(
            first(),
            lead,
            "the locked folder's entry leads the readdir order"
        );
        locked
    };

    let locked = lay_stage(&control);
    let before = entries(&control);
    assert!(before.len() > 3 * 8 * 64, "{}", before.len());
    assert!(
        std::fs::remove_dir_all(&control).is_err(),
        "the folder denies its removal"
    );
    assert_eq!(
        entries(&control),
        before,
        "remove_dir_all stopped at the first entry and left every entry"
    );
    assert!(holds_ace(&locked, DENY_DELETE));

    lay_stage(&stage);
    let mut asked = Vec::new();
    let went = reclaim_moved(&old, SystemTime::now(), &mut |p, w| {
        asked.push((p.to_path_buf(), w));
        true
    });
    assert_eq!(went, [(stage.clone(), Unused::Unfinished)]);
    assert_eq!(asked, went, "its intent asked before it went");
    assert!(
        !stage.exists() && !old.exists(),
        "the stage, its mirror folders and the emptied root went"
    );
}

/// A FOLDER WITHOUT ITS OWNER'S BITS STRANDS NO STAGE: a stage holding `debug/deps` at 0o500 and
/// in it a folder at 0o000, each holding a file, goes whole in one pass, each folder given its
/// owner's bits back before it is emptied. NEGATIVE CONTROL: the same stage laid outside the
/// cache, `std::fs::remove_dir_all` fails and its file stays.
#[cfg(unix)]
#[test]
fn a_stage_holding_a_folder_without_its_owners_bits_goes_whole() {
    let f = Fixture::new("build-cache-mode-stage");
    let root = cache_root(&f.home);
    let stage = build_dir(&root, &f.root.join("home/gone")).with_file_name(DOOMED);
    let control = f.root.join("control").join(DOOMED);
    let lay_stage = |stage: &Path| {
        let deps = stage.join("debug/deps");
        std::fs::create_dir_all(deps.join("sealed")).unwrap();
        std::fs::write(deps.join("x"), b"x").unwrap();
        std::fs::write(deps.join("sealed/y"), b"y").unwrap();
        mode(&deps.join("sealed"), 0o000);
        mode(&deps, 0o500);
    };
    lay_stage(&control);
    assert!(std::fs::remove_dir_all(&control).is_err());
    assert!(
        control.join("debug/deps/x").is_file(),
        "remove_dir_all left it"
    );

    lay_stage(&stage);
    let mut asked = Vec::new();
    let went = reclaim_watched(
        &root,
        Some(f.home.as_path()),
        SystemTime::now(),
        RECLAIM_GONE,
        None,
        &mut |p, w| {
            asked.push((p.to_path_buf(), w));
            true
        },
    );
    assert_eq!(went, [(stage.clone(), Unused::Unfinished)]);
    assert_eq!(asked, went);
    assert!(!stage.exists());
}

/// A LINK IN A STAGE GOES AS ITSELF: a stage holding, in a folder at 0o500, links to a folder
/// and a file outside the cache — the folder at 0o500 holding a file and, on macOS, the link
/// and the folder each carrying `deny delete` — goes whole in one pass; the links go, and
/// nothing they point to changes: the folder keeps its mode, its file and its ACL, the file
/// its bytes. NEGATIVE CONTROL (macOS): the link's own ACL refuses `std::fs::remove_file` before
/// the pass, so the pass cleared that link's ACL, and the folder's stayed.
#[cfg(unix)]
#[test]
fn a_link_in_a_stage_goes_and_what_it_points_to_stays_as_it_was() {
    let f = Fixture::new("build-cache-link-stage");
    let root = cache_root(&f.home);
    let stage = build_dir(&root, &f.root.join("home/gone")).with_file_name(DOOMED);
    let outside = f.dir("outside");
    let target = outside.join("folder");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("keep"), b"keep").unwrap();
    std::fs::write(outside.join("file"), b"bytes").unwrap();
    mode(&target, 0o500);
    let links = stage.join("tmp");
    std::fs::create_dir_all(&links).unwrap();
    std::os::unix::fs::symlink(&target, links.join("folder")).unwrap();
    std::os::unix::fs::symlink(outside.join("file"), links.join("file")).unwrap();
    #[cfg(target_os = "macos")]
    {
        acl(&target, &[DENY_DELETE]);
        acl(&links.join("folder"), &[DENY_DELETE]);
        assert!(
            std::fs::remove_file(links.join("folder")).is_err(),
            "the link's own ACL refuses its removal"
        );
    }
    mode(&links, 0o500);

    let went = reclaim_watched(
        &root,
        Some(f.home.as_path()),
        SystemTime::now(),
        RECLAIM_GONE,
        None,
        &mut |_, _| true,
    );
    assert_eq!(went, [(stage.clone(), Unused::Unfinished)]);
    assert!(!stage.exists());
    assert_eq!(mode_of(&target), 0o500, "the link's target was chmodded");
    assert_eq!(std::fs::read(target.join("keep")).unwrap(), b"keep");
    assert_eq!(std::fs::read(outside.join("file")).unwrap(), b"bytes");
    #[cfg(target_os = "macos")]
    assert!(
        holds_ace(&target, DENY_DELETE),
        "the link's target lost its ACL"
    );
}

/// EVERY REMOVAL A PASS MAKES IS THE ONE REMOVER: a gone workspace's build directory taken
/// whole, a stale incremental folder pruned from a live one, a stage an earlier pass left, a
/// tooling entry, and a target nothing builds in, each holding a folder at 0o500 and in it one
/// that (on macOS) denies its removal by ACL, all go whole. NEGATIVE CONTROL: such a tree
/// refuses `std::fs::remove_dir_all`, each of these removals before.
#[cfg(unix)]
#[test]
fn every_removal_a_pass_makes_takes_what_remove_dir_all_cannot() {
    let f = Fixture::new("build-cache-one-remover");
    let root = cache_root(&f.home);
    let home = Some(f.home.as_path());
    let seal = |dir: &Path| {
        let sealed = dir.join("sealed");
        std::fs::create_dir_all(sealed.join("held")).unwrap();
        std::fs::write(sealed.join("held/y"), b"y").unwrap();
        #[cfg(target_os = "macos")]
        acl(&sealed.join("held"), &[DENY_DELETE]);
        mode(&sealed, 0o500);
    };
    let control = f.dir("control");
    seal(&control);
    assert!(std::fs::remove_dir_all(&control).is_err());
    assert!(
        control.join("sealed/held").is_dir(),
        "remove_dir_all left it"
    );

    let gone_leaf = lay(&root, &f.root.join("home/gone"), 2 * DAY);
    seal(&gone_leaf.join("debug"));
    age_tree(&gone_leaf, 2 * DAY);

    let live = f.dir("home/live");
    std::fs::write(live.join("Cargo.toml"), "[workspace]\n").unwrap();
    let live_leaf = build_dir(&root, &live);
    let profile = live_leaf.join("debug");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join(".cargo-build-lock"), b"").unwrap();
    let stale_crate = profile.join("incremental/ws-1k116trr0a0xf");
    std::fs::create_dir_all(stale_crate.join("s-hneouhc0uf-1tn8o9j-2awpw")).unwrap();
    seal(&stale_crate);
    age_tree(&stale_crate, 4 * DAY);

    let mirror = |ws: &Path| root.join(ws.strip_prefix("/").unwrap());
    let left = mirror(&f.root.join("home/left")).join(DOOMED);
    std::fs::create_dir_all(&left).unwrap();
    seal(&left);

    let mut went = reclaim_watched(
        &root,
        home,
        SystemTime::now(),
        RECLAIM_GONE,
        None,
        &mut |_, _| true,
    );
    went.sort_by(|a, b| a.0.cmp(&b.0));
    let pruned = live_leaf.with_file_name(DOOMED);
    let mut want = vec![gone_leaf.clone(), pruned.clone(), left.clone()];
    want.sort();
    assert_eq!(
        went.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(),
        want,
        "{went:?}"
    );
    for (path, why) in &went {
        let right = if *path == gone_leaf {
            matches!(why, Unused::Gone { .. })
        } else if *path == pruned {
            matches!(why, Unused::Stale { incremental: 1, .. })
        } else {
            *why == Unused::Unfinished
        };
        assert!(right, "{}: {why:?}", path.display());
    }
    assert!(!gone_leaf.exists() && !stale_crate.exists() && !pruned.exists() && !left.exists());
    assert!(
        profile.join(".cargo-build-lock").is_file(),
        "the live leaf stays"
    );

    let tooling = tooling::root(&f.home);
    let entry = tooling.join(".provision-reclaim-tool-1");
    std::fs::create_dir_all(&entry).unwrap();
    seal(&entry);
    assert_eq!(
        tooling::reclaim(&tooling, SystemTime::now(), &mut |_, _| true),
        [(entry.clone(), Unused::Unfinished)]
    );
    assert!(!entry.exists());

    let target = f.dir("target/verify/cells.noindex");
    seal(&target);
    assert!(remove_unbuilt(&target).unwrap());
    assert!(!target.exists());
}

/// AN UNRECORDED STAGE KEEPS ITS BYTES AND ONLY THEM: a stage whose intent is refused stays as
/// it was, its bytes untouched, while the gone workspace's build directory beside it, its own
/// intent on record, is staged under a name of its own (`.cargo-build.removing-1`, never over
/// the old stage) and goes. Each intent is asked once.
#[cfg(unix)]
#[test]
fn pressure_reclamation_keeps_an_unrecorded_stage_and_takes_the_leaf_beside_it() {
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
        std::fs::read(&payload).ok(),
        Some(b"the previous generation".to_vec()),
        "denying the old stage must not delete its bytes"
    );
    assert_eq!(
        removed,
        std::slice::from_ref(&leaf),
        "the leaf, its intent on record"
    );
    assert!(!leaf.exists() && !leaf.with_file_name(format!("{DOOMED}-1")).exists());
    intents.sort();
    assert_eq!(intents, [leaf, stage]);
}

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

/// A holder appearing during intent recording keeps the original directory. A later probe
/// can leave only an empty registered stage: the next pass recovers that intent, then retires
/// the still-identical cache normally. A refused stale intent retains a newly held stage.
#[cfg(unix)]
#[test]
fn a_process_arriving_at_intent_keeps_the_cache_before_staging() {
    let f = Fixture::new("build-cache-late-process");
    let root = cache_root(&f.home);
    let owner = f.dir("home/owner");
    let leaf = lay(&root, &owner, RECLAIM_IDLE + DAY);
    let busy = std::cell::Cell::new(false);
    let processes = |dirs: &[PathBuf], _: &[u32]| {
        let users = busy
            .get()
            .then(|| {
                assert!(dirs.iter().any(|dir| leaf.starts_with(dir)));
                aterm_uds::holders::PathUse {
                    pid: 31,
                    how: "exe",
                    path: leaf.join("debug/deps/x"),
                }
            })
            .into_iter()
            .collect();
        Ok(aterm_uds::holders::PathScan {
            users,
            ..aterm_uds::holders::PathScan::default()
        })
    };
    let removed = reclaim_watched_on(
        &root,
        Some(&f.home),
        SystemTime::now(),
        RECLAIM_GONE,
        None,
        &mut |path, _| {
            assert_eq!(path, leaf);
            busy.set(true);
            true
        },
        &fence::Fence::new(&processes),
    );
    assert!(
        busy.get(),
        "the late holder was introduced by the actual intent callback"
    );
    assert!(removed.is_empty());
    assert!(leaf.join("debug/deps/x").is_file());
    assert!(!leaf.parent().unwrap().join(DOOMED).exists());

    let owner = f.dir("home/registered-owner");
    std::fs::write(owner.join("Cargo.toml"), "[workspace]\n").unwrap();
    let target = f.home.join("explicit.noindex");
    assert!(registered::create_checked(&f.home, &target, &owner).unwrap());
    std::fs::create_dir_all(target.join("debug/deps")).unwrap();
    std::fs::write(target.join("debug/deps/keep"), b"registered output").unwrap();
    std::fs::write(target.join("debug/.cargo-lock"), b"").unwrap();
    age_tree(&target, 2 * DAY);
    std::fs::remove_dir_all(&owner).unwrap();
    let pending_observed = std::cell::Cell::new(false);
    let late = |_: &[PathBuf], _: &[u32]| {
        let pending = registered::roots(&f.home)
            .iter()
            .any(|root| root.root == target && !root.is_current(&f.home));
        pending_observed.set(pending_observed.get() || pending);
        Ok(aterm_uds::holders::PathScan {
            users: pending
                .then(|| aterm_uds::holders::PathUse {
                    pid: 32,
                    how: "exe",
                    path: target.join("debug/deps/keep"),
                })
                .into_iter()
                .collect(),
            ..aterm_uds::holders::PathScan::default()
        })
    };
    let pass = |processes: fence::Processes<'_>| {
        registered::reclaim_on(
            &f.home,
            SystemTime::now(),
            RECLAIM_GONE,
            None,
            &mut |_, _| true,
            &fence::Fence::new(processes),
        )
    };
    assert!(pass(&late).is_empty());
    assert!(
        pending_observed.get(),
        "the holder arrived after the receipt had durable Pending state"
    );
    let stages = || {
        std::fs::read_dir(&f.home)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("atpkg-cache-retiring-")
            })
            .collect::<Vec<_>>()
    };
    let stage = stages()
        .into_iter()
        .next()
        .expect("the private empty stage");
    assert_eq!(std::fs::read_dir(&stage).unwrap().count(), 0);
    let clear = |_: &[PathBuf], _: &[u32]| Ok(aterm_uds::holders::PathScan::default());
    assert!(
        pass(&clear).is_empty(),
        "recovery does not remove the original"
    );
    assert!(stage.symlink_metadata().is_err());
    assert!(target.join("debug/deps/keep").is_file());
    assert!(
        registered::roots(&f.home)
            .iter()
            .any(|root| { root.root == target && root.is_current(&f.home) })
    );
    assert!(matches!(pass(&clear).as_slice(), [(path, Unused::Gone { .. })] if path == &target));
    assert!(!target.exists() && stages().is_empty());

    let owner = f.dir("home/stale-owner");
    std::fs::write(owner.join("Cargo.toml"), "[workspace]\n").unwrap();
    let leaf = build_dir(&root, &owner);
    let profile = leaf.join("debug");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join(".cargo-build-lock"), b"").unwrap();
    let incremental = profile.join("incremental/ws-1k116trr0a0xf");
    std::fs::create_dir_all(incremental.join("s-hneouhc0uf-1tn8o9j-2awpw")).unwrap();
    age_tree(&incremental, 4 * DAY);
    let staged = std::cell::RefCell::new(None::<PathBuf>);
    let newly_held = |dirs: &[PathBuf], _: &[u32]| {
        let users = staged
            .borrow()
            .as_ref()
            .map(|stage| {
                assert!(
                    dirs.contains(stage),
                    "the rollback probes the actual staged spelling"
                );
                aterm_uds::holders::PathUse {
                    pid: 33,
                    how: "cwd",
                    path: stage.join("0"),
                }
            })
            .into_iter()
            .collect();
        Ok(aterm_uds::holders::PathScan {
            users,
            ..Default::default()
        })
    };
    let mut removed = Vec::new();
    let mut intent = |stage: &Path, why| {
        assert!(matches!(why, Unused::Stale { incremental: 1, .. }));
        *staged.borrow_mut() = Some(stage.to_path_buf());
        false
    };
    let mut sweep = Sweep {
        root: &root,
        home: Some(&f.home),
        now: SystemTime::now(),
        gone_after: RECLAIM_GONE,
        placing: None,
        moved: false,
        compiler_store: None,
        record: &mut intent,
        fence: &fence::Fence::new(&newly_held),
        removed: &mut removed,
    };
    assert!(!sweep.prune(leaf.parent().unwrap(), &leaf));
    assert!(removed.is_empty());
    let stage = staged
        .borrow_mut()
        .take()
        .expect("the refused stale intent");
    assert!(
        stage.join("0").is_dir() && !incremental.exists(),
        "the held stage was not rolled back"
    );
    let went = reclaim_watched_on(
        &root,
        Some(&f.home),
        SystemTime::now(),
        RECLAIM_GONE,
        None,
        &mut |_, _| true,
        &fence::Fence::new(&clear),
    );
    assert!(
        went.iter()
            .any(|(path, why)| path == &stage && *why == Unused::Unfinished)
    );
    assert!(!stage.exists() && profile.join(".cargo-build-lock").is_file());
}

/// THE ROOT THE CACHE MOVED FROM EMPTIES ITSELF: under it a build directory an hour quiet goes
/// whole whatever its workspace's state (a live one's too), one a build holds or wrote within the
/// hour stays, and the root goes with the last of them; `plan_moved` names what a pass would
/// take, a held one marked held. NEGATIVE CONTROL: the same live workspace's two-hour-quiet build
/// directory under the cache root itself stays (only a gone or a month-old one goes there).
#[cfg(unix)]
#[test]
fn the_root_the_cache_moved_from_goes_once_nothing_builds_there() {
    let f = Fixture::new("build-cache-moved");
    let old = moved_from(&f.home);
    let hour = Duration::from_secs(60 * 60);
    let live = f.dir("home/live");
    std::fs::write(live.join("Cargo.toml"), "[workspace]\n").unwrap();
    let quiet = lay(&old, &live, 2 * hour);
    let fresh = lay(&old, &f.root.join("home/fresh"), Duration::from_secs(60));
    let busy = lay(&old, &f.root.join("home/busy"), 2 * hour);
    let lock = busy.join("debug/.cargo-lock");
    std::fs::write(&lock, b"").unwrap();
    age_tree(&busy, 2 * hour);
    let held = crate::lock::Flock::try_lock(std::fs::File::open(&lock).unwrap()).unwrap();
    let now = SystemTime::now();

    let planned: Vec<(PathBuf, bool)> = plan_moved(&old, now)
        .into_iter()
        .map(|p| {
            assert!(matches!(p.why, Unused::Moved { after, .. } if after == MOVED_AFTER));
            (p.path, p.held)
        })
        .collect();
    let mut want = vec![(quiet.clone(), false), (busy.clone(), true)];
    want.sort();
    assert_eq!(planned, want);

    let went = reclaim_moved(&old, now, &mut |_, _| true);
    assert_eq!(went.len(), 1, "{went:?}");
    assert_eq!(went[0].0, quiet);
    assert!(
        matches!(went[0].1, Unused::Moved { idle, after } if idle >= 2 * hour && after == MOVED_AFTER)
    );
    assert!(!quiet.exists() && fresh.is_dir() && busy.is_dir() && old.is_dir());

    drop(held);
    let mut went: Vec<PathBuf> = reclaim_moved(&old, now + 2 * hour, &mut |_, _| true)
        .into_iter()
        .map(|(p, _)| p)
        .collect();
    went.sort();
    let mut want = vec![busy, fresh];
    want.sort();
    assert_eq!(went, want);
    assert!(!old.exists(), "the emptied root goes too");

    let root = cache_root(&f.home);
    let kept = lay(&root, &live, 2 * hour);
    assert!(reclaim(&root, Some(f.home.as_path()), now).is_empty());
    assert!(kept.is_dir());
}

/// A TARGET DIRECTORY NOTHING BUILDS IN GOES WHOLE: verify's old side lane goes once no build
/// holds a lock in it, a cells lane's deep in its cell and triple included, and nothing is left
/// beside it. NEGATIVE CONTROL: while a build holds that deep lock it stays, whole.
#[cfg(unix)]
#[test]
fn a_target_nothing_builds_in_goes_whole_and_a_held_one_stays() {
    let f = Fixture::new("build-cache-unbuilt");
    let old = f.dir("target/verify/cells.noindex");
    let deep = old.join("win/x86_64-pc-windows-msvc/debug");
    std::fs::create_dir_all(deep.join("deps")).unwrap();
    std::fs::write(deep.join(".cargo-lock"), b"").unwrap();
    std::fs::write(deep.join("deps/x.rmeta"), b"x").unwrap();
    let held = crate::lock::Flock::try_lock(std::fs::File::open(deep.join(".cargo-lock")).unwrap())
        .unwrap();
    assert!(!remove_unbuilt(&old).unwrap(), "a build holds it");
    assert!(deep.join("deps/x.rmeta").is_file(), "kept whole");
    drop(held);
    let obstruction = deep.join(".cargo-artifact-lock");
    std::fs::create_dir(&obstruction).unwrap();
    assert!(!remove_unbuilt(&old).unwrap());
    assert_eq!(std::fs::read(deep.join("deps/x.rmeta")).unwrap(), b"x");
    assert!(obstruction.is_dir());
    std::fs::remove_dir(&obstruction).unwrap();
    assert!(remove_unbuilt(&old).unwrap());
    assert!(!old.exists());
    let listed: Vec<_> = std::fs::read_dir(old.parent().unwrap())
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert!(listed.is_empty(), "nothing beside it: {listed:?}");
    assert!(!remove_unbuilt(&old).unwrap(), "gone already");
}

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
    let program = crate::store::default_prefix(&f.home).join("store/trust");
    let sysroot = program.join("2");
    std::fs::create_dir_all(&sysroot).unwrap();
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
