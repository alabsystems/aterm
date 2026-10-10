// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE REROUTE NAMES WHAT IT RUNS, end to end through the real `atpkg __reroute` a stub
//! execs.
//!
//! Replays a recorded bug: after `rustup default trust`, a bare `rustc -vV` in an unpinned
//! directory ran the Trust seal 7d5533b2 (its `-vV` prints `trust: 0.1.0`), yet the
//! signpost said "running upstream 'rustc'" — the words it printed over stock 1.97.1. And
//! inside a Trust source checkout a stock resolution must not run at all: there a stock
//! compiler would build part of Trust.
//!
//! Each case puts ONE fake `rustc` on PATH — a script that answers `-vV` like a Trust or a
//! stock rustc and prints its arguments otherwise, exiting 3 (not 0, not the reroute's 2 or
//! 127) — and runs `atpkg __reroute rustc main.rs` from a clean environment under a temp
//! `HOME`, so the store, the settings and the identity cache are the fixture's.
//!
//! The fakes are content-addressed beside the test binary and run once before `atpkg` is,
//! so a first exec's `syspolicyd` assessment of a freshly written file (seconds under load,
//! `atpkg::reroute`'s test notes) is paid here, never inside the resolution's probe bound.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// What each fake prints for anything but `-vV`, and its exit code.
const RAN_EXIT: i32 = 3;

const TRUST_RUSTC: &str = "#!/bin/sh\n\
if [ \"$1\" = -vV ]; then\n\
printf '%s\\n' 'rustc 1.99.0-dev (7d5533b2b 2026-09-23)' 'binary: rustc' \
'commit-hash: 7d5533b2bb9c9417bc09c27f1acd9d0b808f8f15' 'commit-date: 2026-09-23' \
'host: aarch64-apple-darwin' 'release: 1.99.0-dev' 'trust: 0.1.0' 'LLVM version: 22.1.2'\n\
exit 0\nfi\necho \"trust rustc ran: $*\"\nexit 3\n";

const STOCK_RUSTC: &str = "#!/bin/sh\n\
if [ \"$1\" = -vV ]; then\n\
printf '%s\\n' 'rustc 1.97.1 (8bab26f4f 2026-07-14)' 'binary: rustc' \
'commit-hash: 8bab26f4f68e0e26f0bb7960be334d5b520ea452' 'commit-date: 2026-07-14' \
'host: aarch64-apple-darwin' 'release: 1.97.1' 'LLVM version: 22.1.6'\n\
exit 0\nfi\necho \"stock rustc ran: $*\"\nexit 3\n";

/// A wrapper on PATH — the shape of one around rustup's proxy (a devcontainer's, an
/// asdf-style shim): it runs the `+<toolchain>` it is handed, stock for any channel but
/// `trust`, and Trust when handed none.
const WRAPPER_RUSTC: &str = "#!/bin/sh\n\
tc=trust\n\
case \"$1\" in +*) tc=${1#+}; shift;; esac\n\
if [ \"$1\" = -vV ]; then\n\
if [ \"$tc\" = trust ]; then\n\
printf '%s\\n' 'rustc 1.99.0-dev (7d5533b2b 2026-09-23)' 'binary: rustc' \
'commit-hash: 7d5533b2bb9c9417bc09c27f1acd9d0b808f8f15' 'commit-date: 2026-09-23' \
'host: aarch64-apple-darwin' 'release: 1.99.0-dev' 'trust: 0.1.0' 'LLVM version: 22.1.2'\n\
else\n\
printf '%s\\n' 'rustc 1.97.1 (8bab26f4f 2026-07-14)' 'binary: rustc' \
'commit-hash: 8bab26f4f68e0e26f0bb7960be334d5b520ea452' 'commit-date: 2026-07-14' \
'host: aarch64-apple-darwin' 'release: 1.97.1' 'LLVM version: 22.1.6'\n\
fi\n\
exit 0\nfi\necho \"$tc rustc ran: $*\"\nexit 3\n";

/// A stock `rustc` whose `-vV` leaves a process behind holding its output, as a wrapper does
/// that backgrounds a usage logger or starts a server: with `$LEFT_BEHIND_PIDFILE` set it
/// starts a `/bin/sleep 600` (the fixture's PATH holds only the fake) that inherits its
/// stdout and stderr, and writes its pid there.
const LEFT_BEHIND_RUSTC: &str = "#!/bin/sh\n\
if [ \"$1\" = -vV ]; then\n\
if [ -n \"$LEFT_BEHIND_PIDFILE\" ]; then /bin/sleep 600 & echo $! > \"$LEFT_BEHIND_PIDFILE\"; fi\n\
printf '%s\\n' 'rustc 1.97.1 (8bab26f4f 2026-07-14)' 'binary: rustc' \
'commit-hash: 8bab26f4f68e0e26f0bb7960be334d5b520ea452' 'commit-date: 2026-07-14' \
'host: aarch64-apple-darwin' 'release: 1.97.1' 'LLVM version: 22.1.6'\n\
exit 0\nfi\necho \"stock rustc ran: $*\"\nexit 3\n";

/// A Trust `cargo` whose subcommand spawns `rustc +1.97.1 main.rs` BY NAME and exits with
/// its code — a cargo subcommand, `cargo run -p xtask`, a build script that runs `rustc
/// +<channel>`, Trust's own miri-script (`rustc +miri`), under a Trust cargo the reroute
/// allowed.
const TRUST_CARGO_SPAWNING_STOCK: &str = "#!/bin/sh\n\
if [ \"$1\" = -vV ]; then\n\
printf '%s\\n' 'cargo 1.99.0-dev (7d5533b2b 2026-09-23)' 'release: 1.99.0-dev' \
'commit-hash: 7d5533b2bb9c9417bc09c27f1acd9d0b808f8f15' 'commit-date: 2026-09-23' \
'host: aarch64-apple-darwin' 'trust: 0.1.0'\n\
exit 0\nfi\necho \"trust cargo ran: $*\"\nrustc +1.97.1 main.rs\n";

/// The program whose body is `body`, one file per distinct body, shared by every run — laid
/// and first run under one lock: every test thread lays through the same temporary name, and
/// without the lock two tests laying one body from a cold directory rename it from under
/// each other (`NotFound` at the `set_permissions` or the `rename`). `atpkg`'s unit tests
/// share the same discipline (`test_programs::assessed`).
fn program(name: &str, body: &str) -> PathBuf {
    use std::hash::{Hash as _, Hasher as _};
    static LAYING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _laying = LAYING.lock().unwrap_or_else(|e| e.into_inner());
    let exe = std::env::current_exe().unwrap();
    let dir = exe
        .parent()
        .and_then(Path::parent)
        .expect("a test binary lives in target/<profile>/deps")
        .join("atpkg-reroute-names-programs");
    std::fs::create_dir_all(&dir).unwrap();
    let mut hasher = std::hash::DefaultHasher::new();
    body.hash(&mut hasher);
    let path = dir.join(format!("{name}-{:016x}", hasher.finish()));
    if std::fs::read(&path).ok().as_deref() != Some(body.as_bytes()) {
        let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
        std::fs::write(&tmp, body).unwrap();
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::rename(&tmp, &path).unwrap();
    }
    // The first exec's assessment, paid here.
    let warmed = Command::new(&path).arg("-vV").output().unwrap();
    assert!(warmed.status.success(), "{name}: {warmed:?}");
    path
}

struct Fixture {
    root: PathBuf,
    home: PathBuf,
    config_home: PathBuf,
    bin: PathBuf,
    work: PathBuf,
}

impl Fixture {
    /// A fixture whose PATH holds one `rustc`: `body`.
    fn new(case: &str, name: &str, body: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("atpkg-reroute-names-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (home, config_home, bin, work) = (
            root.join("home"),
            root.join("config"),
            root.join("bin"),
            root.join("work"),
        );
        for dir in [&home, &config_home, &bin, &work] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::os::unix::fs::symlink(program(name, body), bin.join("rustc")).unwrap();
        Self {
            root,
            home,
            config_home,
            bin,
            work,
        }
    }

    /// Another program on the fixture's PATH: `bin/<as_name>` runs `body`.
    fn add(&self, as_name: &str, name: &str, body: &str) {
        std::os::unix::fs::symlink(program(name, body), self.bin.join(as_name)).unwrap();
    }

    /// The `cargo` and `rustc` stubs a session puts first on PATH, as `atpkg::reroute::lay`
    /// leaves them (the marker file beside them), naming this test's `atpkg`; the PATH
    /// `stubs:bin`.
    fn stubs(&self) -> String {
        let stubs = self.root.join("stubs");
        std::fs::create_dir_all(&stubs).unwrap();
        std::fs::write(
            stubs.join(atpkg::reroute::DIR_MARKER_FILE),
            format!("{}\n", atpkg::reroute::STUB_MARKER),
        )
        .unwrap();
        for name in ["cargo", "rustc"] {
            let stub = stubs.join(name);
            std::fs::write(
                &stub,
                atpkg::reroute::stub_body_sh(
                    name,
                    Path::new(env!("CARGO_BIN_EXE_atpkg")),
                    &stubs,
                    false,
                ),
            )
            .unwrap();
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        format!("{}:{}", stubs.display(), self.bin.display())
    }

    /// `x.py` and a `src/stage0` naming `channel` under `work/checkout`; the directory to
    /// run in, two levels below its root.
    fn checkout(&self, channel: &str) -> PathBuf {
        let root = self.work.join("checkout");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("x.py"), "#!/usr/bin/env python3\n").unwrap();
        std::fs::write(
            root.join("src").join("stage0"),
            format!(
                "dist_server=https://example.invalid\ncompiler_dist_channel={channel}\ncompiler_version=1.99.0-{channel}\n"
            ),
        )
        .unwrap();
        let sub = root.join("compiler").join("rustc_driver");
        std::fs::create_dir_all(&sub).unwrap();
        sub
    }

    /// `atpkg __reroute rustc main.rs` in `cwd`, from a clean environment.
    fn rustc(&self, cwd: &Path) -> Output {
        self.rustc_with(cwd, &["main.rs"])
    }

    /// `atpkg __reroute rustc <args…>` in `cwd`, from a clean environment.
    fn rustc_with(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_atpkg"))
            .args(["__reroute", "rustc"])
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.config_home)
            .env("PATH", &self.bin)
            .output()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// A bare `rustc` that IS Trust is announced as Trust — by its commit — never as "upstream"
/// with "no proof claim"; the fake then runs with the caller's arguments and its exit code.
#[test]
fn a_rustc_that_resolves_to_trust_is_named_trust() {
    let fx = Fixture::new("trust", "trust-rustc", TRUST_RUSTC);
    let out = fx.rustc(&fx.work);
    let stderr = text(&out.stderr);
    let first = stderr.lines().next().unwrap_or_default();
    assert!(
        first.starts_with("aterm: running 'rustc' -> Trust 7d5533b2"),
        "the first line names Trust by its commit — stderr={stderr:?}"
    );
    assert!(
        !stderr.contains("running upstream") && !stderr.contains("no proof claim);"),
        "Trust is not called upstream, nor said to carry no proof claim — stderr={stderr:?}"
    );
    assert!(
        stderr.contains(&format!(
            "('rustc' here is {}",
            fx.bin.join("rustc").canonicalize().unwrap().display()
        )),
        "the file that runs is named — stderr={stderr:?}"
    );
    assert!(stderr.contains("trustc main.rs"), "{stderr}");
    assert_eq!(text(&out.stdout), "trust rustc ran: main.rs\n", "{stderr}");
    assert_eq!(out.status.code(), Some(RAN_EXIT), "{stderr}");
}

/// A bare `rustc` that is STOCK says so, with its version, and that it is not Trust — and
/// still runs (outside a Trust source checkout a SIGNPOST announces; it does not prevent).
#[test]
fn a_rustc_that_resolves_to_stock_is_named_stock_and_runs() {
    let fx = Fixture::new("stock", "stock-rustc", STOCK_RUSTC);
    let out = fx.rustc(&fx.work);
    let stderr = text(&out.stderr);
    let first = stderr.lines().next().unwrap_or_default();
    assert!(
        first.starts_with("aterm: running 'rustc' -> STOCK rustc 1.97.1 (8bab26f4f); not Trust"),
        "the first line names STOCK and its version — stderr={stderr:?}"
    );
    assert!(
        first.ends_with("on Trust the tool is 'trustc':"),
        "{stderr}"
    );
    assert_eq!(text(&out.stdout), "stock rustc ran: main.rs\n", "{stderr}");
    assert_eq!(out.status.code(), Some(RAN_EXIT), "{stderr}");
}

/// Inside a Trust source checkout a stock `rustc` does not run: exit 2, stdout clean, and
/// the line names what it resolved to and the checkout. Trust there runs as anywhere, and an
/// upstream Rust checkout (channel `nightly`) is not Trust's — the negative controls.
#[test]
fn inside_a_trust_checkout_a_stock_rustc_is_refused_and_trust_runs() {
    let fx = Fixture::new("checkout-stock", "stock-rustc", STOCK_RUSTC);
    let inside = fx.checkout("trust");
    let out = fx.rustc(&inside);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "refused — stderr={stderr:?}");
    assert_eq!(
        text(&out.stdout),
        "",
        "the stock rustc never ran — stderr={stderr:?}"
    );
    assert!(
        stderr.starts_with(
            "aterm: 'rustc' did not run -> STOCK rustc 1.97.1 (8bab26f4f); not Trust,"
        ),
        "{stderr}"
    );
    assert!(
        stderr.contains(
            "is a Trust source checkout (its src/stage0 says compiler_dist_channel=trust)"
        ),
        "{stderr}"
    );
    assert!(
        stderr.contains("`aterm --no-reroute`"),
        "the escape is named — {stderr}"
    );

    // Negative control: the same stock rustc in an UPSTREAM Rust checkout runs.
    let upstream = Fixture::new("checkout-upstream", "stock-rustc", STOCK_RUSTC);
    let out = upstream.rustc(&upstream.checkout("nightly"));
    assert_eq!(out.status.code(), Some(RAN_EXIT), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "stock rustc ran: main.rs\n");

    // Trust inside the Trust checkout runs.
    let trust = Fixture::new("checkout-trust", "trust-rustc", TRUST_RUSTC);
    let out = trust.rustc(&trust.checkout("trust"));
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(RAN_EXIT), "{stderr}");
    assert_eq!(text(&out.stdout), "trust rustc ran: main.rs\n", "{stderr}");
    assert!(
        stderr.starts_with("aterm: running 'rustc' -> Trust 7d5533b2"),
        "{stderr}"
    );
}

/// A `+<toolchain>` is part of what runs, so it is part of what is asked: a wrapper on PATH
/// that is not rustup's own proxy hands `+1.97.1` on and runs stock, and is named STOCK for
/// it — and inside a Trust source checkout it does not run (a question that dropped it
/// said `-> Trust 7d5533b2` over a stock run). The same wrapper handed nothing is Trust,
/// and runs.
#[test]
fn a_toolchain_handed_to_a_wrapper_is_part_of_the_question() {
    let fx = Fixture::new("plus", "rustc-wrapper", WRAPPER_RUSTC);
    let out = fx.rustc_with(&fx.work, &["+1.97.1", "main.rs"]);
    let stderr = text(&out.stderr);
    assert!(
        stderr.starts_with(
            "aterm: running 'rustc +1.97.1' -> STOCK rustc 1.97.1 (8bab26f4f); not Trust"
        ),
        "{stderr}"
    );
    assert_eq!(text(&out.stdout), "1.97.1 rustc ran: main.rs\n", "{stderr}");
    assert_eq!(out.status.code(), Some(RAN_EXIT), "{stderr}");

    let inside = fx.checkout("trust");
    let out = fx.rustc_with(&inside, &["+1.97.1", "main.rs"]);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "refused — stderr={stderr:?}");
    assert_eq!(text(&out.stdout), "", "nothing ran — stderr={stderr:?}");
    assert!(
        stderr.starts_with(
            "aterm: 'rustc +1.97.1' did not run -> STOCK rustc 1.97.1 (8bab26f4f); not Trust,"
        ),
        "{stderr}"
    );

    let out = fx.rustc(&inside);
    let stderr = text(&out.stderr);
    assert!(
        stderr.starts_with("aterm: running 'rustc' -> Trust 7d5533b2"),
        "{stderr}"
    );
    assert_eq!(text(&out.stdout), "trust rustc ran: main.rs\n", "{stderr}");
    assert_eq!(out.status.code(), Some(RAN_EXIT), "{stderr}");
}

/// A process the copy leaves behind never holds the command: the copy is asked what it is,
/// and the command runs, while that process still lives: a probe that reads the copy's
/// pipes to their end waits as long as the leftover holds them open, forever for one that
/// never exits. A minute is the hang detector, not a budget: the command needs no wait.
#[test]
fn a_process_the_copy_leaves_behind_never_holds_the_command() {
    let fx = Fixture::new("left-behind", "left-behind-rustc", LEFT_BEHIND_RUSTC);
    let pidfile = fx.root.join("left-behind.pid");
    let (stdout, stderr) = (fx.root.join("stdout"), fx.root.join("stderr"));
    let mut atpkg = Command::new(env!("CARGO_BIN_EXE_atpkg"))
        .args(["__reroute", "rustc", "main.rs"])
        .current_dir(&fx.work)
        .env_clear()
        .env("HOME", &fx.home)
        .env("XDG_CONFIG_HOME", &fx.config_home)
        .env("PATH", &fx.bin)
        .env("LEFT_BEHIND_PIDFILE", &pidfile)
        .stdout(std::fs::File::create(&stdout).unwrap())
        .stderr(std::fs::File::create(&stderr).unwrap())
        .spawn()
        .unwrap();
    let left_behind = || {
        std::fs::read_to_string(&pidfile)
            .ok()
            .map(|pid| pid.trim().to_string())
    };
    let alive = |pid: &str| {
        Command::new("kill")
            .args(["-0", pid])
            .output()
            .is_ok_and(|out| out.status.success())
    };
    // The left-behind process dies by SIGKILL once judged, on every path.
    let reap = |pid: Option<String>| {
        if let Some(pid) = pid {
            let _ = Command::new("kill").args(["-KILL", &pid]).output();
        }
    };
    let detector = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let status = loop {
        if let Some(status) = atpkg.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= detector {
            reap(left_behind());
            let _ = atpkg.kill();
            let _ = atpkg.wait();
            panic!(
                "the command waited a minute on a process its copy left behind — stderr={:?}",
                std::fs::read_to_string(&stderr).unwrap_or_default()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let pid = left_behind().expect("the copy was asked `-vV` and left its process behind");
    let still_alive = alive(&pid);
    reap(Some(pid));
    let stderr = std::fs::read_to_string(&stderr).unwrap();
    assert!(
        still_alive,
        "the command finished only after the left-behind process did — stderr={stderr:?}"
    );
    assert!(
        stderr.starts_with("aterm: running 'rustc' -> STOCK rustc 1.97.1 (8bab26f4f); not Trust"),
        "the copy's answer was read, not lost — stderr={stderr:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&stdout).unwrap(),
        "stock rustc ran: main.rs\n",
        "{stderr}"
    );
    assert_eq!(status.code(), Some(RAN_EXIT), "{stderr}");
}

/// A STOCK COMPILER SPAWNED BY NAME BENEATH AN ALLOWED TRUST CARGO IS JUDGED. Replays a
/// recorded bug: inside a Trust checkout the `rustc +1.97.1` an allowed Trust cargo's
/// subcommand spawned by name ran stock with no line, because the reroute handed the
/// allowed cargo the ESCAPE marker; typed alone, it was refused. End to end through the
/// real stubs and the real `atpkg`: the child is refused there (and cargo fails with its
/// exit), runs unannounced anywhere else, and the escape still runs everything.
#[test]
fn a_stock_rustc_a_trust_cargo_spawns_by_name_is_judged_in_a_trust_checkout() {
    let fx = Fixture::new("child", "rustc-wrapper", WRAPPER_RUSTC);
    fx.add(
        "cargo",
        "trust-cargo-spawning-stock",
        TRUST_CARGO_SPAWNING_STOCK,
    );
    let path = fx.stubs();
    let cargo_demo = |cwd: &Path, env: &[(&str, &str)]| {
        let mut cmd = Command::new("/bin/sh");
        cmd.arg(fx.root.join("stubs").join("cargo"))
            .arg("demo")
            .current_dir(cwd)
            .env_clear()
            .env("HOME", &fx.home)
            .env("XDG_CONFIG_HOME", &fx.config_home)
            .env("PATH", &path);
        for (key, value) in env {
            cmd.env(key, value);
        }
        let out = cmd.output().unwrap();
        (out.status.code(), text(&out.stdout), text(&out.stderr))
    };

    let inside = fx.checkout("trust");
    let (code, stdout, stderr) = cargo_demo(&inside, &[]);
    assert!(
        stderr.starts_with("aterm: running 'cargo demo' -> Trust 7d5533b2"),
        "the Trust cargo runs, named — stderr={stderr:?}"
    );
    assert_eq!(
        stdout, "trust cargo ran: demo\n",
        "the stock rustc beneath it never ran — stderr={stderr:?}"
    );
    assert!(
        stderr.contains(
            "aterm: 'rustc +1.97.1' did not run -> STOCK rustc 1.97.1 (8bab26f4f); not Trust,"
        ),
        "the child is refused as the same name typed alone — stderr={stderr:?}"
    );
    assert_eq!(
        code,
        Some(2),
        "cargo fails with the refusal's exit — {stderr}"
    );

    // Outside a checkout the child runs, and is not announced a second time.
    let (code, stdout, stderr) = cargo_demo(&fx.work, &[]);
    assert_eq!(
        stdout, "trust cargo ran: demo\n1.97.1 rustc ran: main.rs\n",
        "{stderr}"
    );
    assert!(
        stderr.starts_with("aterm: running 'cargo demo' -> Trust 7d5533b2"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("'rustc +1.97.1'"),
        "a child is said nothing — stderr={stderr:?}"
    );
    assert_eq!(code, Some(RAN_EXIT), "{stderr}");

    // The escape (`aterm --no-reroute`'s marker) is still the escape: nothing judged or said.
    let (code, stdout, stderr) = cargo_demo(&inside, &[(atpkg::reroute::PASSTHROUGH_ENV, "1")]);
    assert_eq!(
        stdout, "trust cargo ran: demo\n1.97.1 rustc ran: main.rs\n",
        "{stderr}"
    );
    assert_eq!(stderr, "", "the escape says nothing");
    assert_eq!(code, Some(RAN_EXIT));
}

/// NO STORE IS NO WAY ROUND THE CHECKOUT (a recorded bug): with `HOME` unset
/// — an `env -i` wrapper — no package store resolves, and a stock `rustc` two levels inside
/// a Trust checkout ran under "running upstream 'rustc'". It is resolved and judged as with
/// a store: refused there, named STOCK elsewhere, and Trust runs in the checkout.
#[test]
fn with_no_store_a_stock_rustc_in_a_trust_checkout_is_refused() {
    let storeless = |fx: &Fixture, cwd: &Path| {
        let out = Command::new(env!("CARGO_BIN_EXE_atpkg"))
            .args(["__reroute", "rustc", "main.rs"])
            .current_dir(cwd)
            .env_clear()
            .env("XDG_CONFIG_HOME", &fx.config_home)
            .env("PATH", &fx.bin)
            .output()
            .unwrap();
        (out.status.code(), text(&out.stdout), text(&out.stderr))
    };
    let fx = Fixture::new("storeless", "stock-rustc", STOCK_RUSTC);
    let (code, stdout, stderr) = storeless(&fx, &fx.checkout("trust"));
    assert!(
        stderr.contains("no package store") || stderr.contains("HOME"),
        "no store resolved, as the case needs — stderr={stderr:?}"
    );
    assert_eq!(stdout, "", "the stock rustc never ran — stderr={stderr:?}");
    assert!(
        stderr.contains("aterm: 'rustc' did not run -> STOCK rustc 1.97.1 (8bab26f4f); not Trust,"),
        "{stderr}"
    );
    assert_eq!(code, Some(2), "{stderr}");

    // Elsewhere it runs, named STOCK — never "upstream".
    let (code, stdout, stderr) = storeless(&fx, &fx.work);
    assert_eq!(stdout, "stock rustc ran: main.rs\n", "{stderr}");
    assert!(
        stderr.contains(
            "aterm: running 'rustc' -> STOCK rustc 1.97.1 (8bab26f4f); not Trust — no package store resolves here"
        ),
        "{stderr}"
    );
    assert_eq!(code, Some(RAN_EXIT), "{stderr}");

    // Trust in the checkout runs, named Trust.
    let trust = Fixture::new("storeless-trust", "trust-rustc", TRUST_RUSTC);
    let (code, stdout, stderr) = storeless(&trust, &trust.checkout("trust"));
    assert_eq!(stdout, "trust rustc ran: main.rs\n", "{stderr}");
    assert!(
        stderr.contains("aterm: running 'rustc' -> Trust 7d5533b2 — no package store"),
        "{stderr}"
    );
    assert_eq!(code, Some(RAN_EXIT), "{stderr}");
}

/// A STOCK CARGO'S OWN COMPILE IS JUDGED IN A TRUST CHECKOUT. cargo marks every compile
/// it spawns for a crate with `CARGO_CRATE_NAME`, and the `rustc` stub takes that mark as
/// it takes a child of a command the reroute ran: said nothing, run as the cargo chose.
/// Sent down the escape's walk before atpkg is asked, a stock cargo started by its path,
/// whose `rustc -vV` cargo had cached, would compile stock inside a Trust checkout with
/// nothing judged. End to end through the real stub and `atpkg`: refused there, with the
/// refusal its only line; run with no line anywhere else; Trust runs there, quietly.
#[test]
fn a_stock_cargos_compile_is_judged_in_a_trust_checkout() {
    let compile = |fx: &Fixture, path: &str, cwd: &Path| {
        let out = Command::new("/bin/sh")
            .arg(fx.root.join("stubs").join("rustc"))
            .args(["--crate-name", "dep1", "main.rs"])
            .current_dir(cwd)
            .env_clear()
            .env("HOME", &fx.home)
            .env("XDG_CONFIG_HOME", &fx.config_home)
            .env("PATH", path)
            .env(atpkg::reroute::CARGO_CRATE_ENV, "dep1")
            .output()
            .unwrap();
        (out.status.code(), text(&out.stdout), text(&out.stderr))
    };
    let fx = Fixture::new("compile", "stock-rustc", STOCK_RUSTC);
    let path = fx.stubs();
    let (code, stdout, stderr) = compile(&fx, &path, &fx.checkout("trust"));
    assert_eq!(
        stdout, "",
        "the stock compile never ran — stderr={stderr:?}"
    );
    assert!(
        stderr.starts_with(
            "aterm: 'rustc' did not run -> STOCK rustc 1.97.1 (8bab26f4f); not Trust,"
        ),
        "refused, and nothing announced before it — stderr={stderr:?}"
    );
    assert_eq!(code, Some(2), "{stderr}");

    // Outside a checkout the compile runs, and says nothing.
    let (code, stdout, stderr) = compile(&fx, &path, &fx.work);
    assert_eq!(
        stdout, "stock rustc ran: --crate-name dep1 main.rs\n",
        "{stderr}"
    );
    assert_eq!(stderr, "", "a compile is said nothing");
    assert_eq!(code, Some(RAN_EXIT));

    // Trust's compile in the Trust checkout runs, and says nothing either.
    let trust = Fixture::new("compile-trust", "trust-rustc", TRUST_RUSTC);
    let path = trust.stubs();
    let (code, stdout, stderr) = compile(&trust, &path, &trust.checkout("trust"));
    assert_eq!(
        stdout, "trust rustc ran: --crate-name dep1 main.rs\n",
        "{stderr}"
    );
    assert_eq!(stderr, "", "a compile is said nothing");
    assert_eq!(code, Some(RAN_EXIT));
}
