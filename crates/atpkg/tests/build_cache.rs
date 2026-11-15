// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for `atpkg::build_cache` under an isolated temp `HOME`.

#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A wait for something that must happen, the first exec of the build script cargo just
/// linked among it: the first-exec hang detector, not a budget.
const HANG: Duration = aterm_uds::deadline::FIRST_EXEC_HANG;

const BUILD_RS: &str = r#"
fn main() {
    let out = std::env::var("OUT_DIR").unwrap();
    let shim = std::env::var("BC_SHIM").unwrap();
    let cargo = std::env::var("CARGO").unwrap();
    if let Ok(ready) = std::env::var("BC_HOP_READY") {
        std::fs::write(&ready, std::process::id().to_string()).unwrap();
        let release = std::env::var("BC_HOP_RELEASE").unwrap();
        let from = std::time::Instant::now();
        while !std::path::Path::new(&release).exists() {
            assert!(from.elapsed() < std::time::Duration::from_secs(60), "hang detector: lock witness");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    for (name, program, args) in [
        ("cargo", cargo.as_str(), vec!["--unverified", "build", "-p", "inner"]),
        ("shim", "/bin/sh", vec![shim.as_str(), "--unverified", "build", "-p", "inner"]),
    ] {
        let target = format!("{out}/nested-{name}");
        let status = std::process::Command::new(program)
            .args(&args)
            .env("CARGO_TARGET_DIR", &target)
            .env("ATERM_ADMIT", "wait")
            .status()
            .unwrap();
        assert!(status.success(), "nested {name}: {status}");
        assert!(std::path::Path::new(&target).join("debug/libinner.rlib").is_file(), "nested {name} published its compiled library in the requested target dir");
    }
}
"#;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn a_build_script_building_its_own_workspace_finishes_under_the_shim() {
    nested_build(false, "build");
}

/// The same through an alias cargo expands to `build`: the shim places it by its `--config`
/// alone, which reaches no build script's own cargo.
#[test]
fn a_build_script_building_its_own_workspace_through_an_alias_finishes_under_the_shim() {
    nested_build(false, "xb");
}

/// A real Cargo, identified by this suite's compile-time compiler path, has its active build
/// lock while its build.rs is stopped at an explicit barrier. The admission hop's covering
/// claim remains held, and nested builds explicitly told to wait still finish under it.
#[test]
fn a_real_cargo_build_script_keeps_its_lock_and_covered_nested_builds_live() {
    nested_build(true, "build");
}

/// Test observation, never admission authority: the actual compiler path baked into this
/// suite, a stable kernel process identity, and a held file that Cargo has open at the barrier.
fn real_cargo_lock(pid: u32, cargo: &Path) -> bool {
    let Ok(raw) = i32::try_from(pid) else {
        return false;
    };
    let Some(identity) = aterm_uds::process::read_identity(raw) else {
        return false;
    };
    let Ok(cargo) = cargo.canonicalize() else {
        return false;
    };
    if aterm_uds::process::read_process_path(raw).and_then(|p| p.canonicalize().ok()) != Some(cargo)
    {
        return false;
    }
    let locked = atpkg::build_admit::possible_build_lock(
        &|| vec![pid],
        &|pid| aterm_uds::holders::open_files(pid).unwrap_or_default(),
        &|path| aterm_uds::ownerlock::probe_any(path) == aterm_uds::ownerlock::Owner::Live,
    );
    locked.is_some() && aterm_uds::process::read_identity(raw) == Some(identity)
}

/// `verb` is `build`, or `xb`, which the workspace's config makes an alias of it.
fn nested_build(hop: bool, verb: &str) {
    let targo = Path::new(env!("CARGO"));
    if targo.file_name().is_none_or(|n| n != "targo") {
        aterm_spec::host::could_not_run(format!(
            "needs Trust's targo, and this suite runs under {}",
            targo.display()
        ));
    }
    // Cargo links and runs outer's build script under it: kept unless that script exited on
    // its own.
    let mut tmp = aterm_uds::deadline::KeptUntilRan::new(
        aterm_tempfile::Builder::new()
            .prefix("atpkg-build-cache-nested-")
            .tempdir()
            .unwrap()
            .keep(),
    );
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(home.join("Library/Caches")).unwrap();
    let ws = home.join("ws");
    write(
        &ws.join("Cargo.toml"),
        "[workspace]\nmembers = [\"outer\", \"inner\"]\nresolver = \"2\"\n",
    );
    write(
        &ws.join("inner/Cargo.toml"),
        "[package]\nname = \"inner\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(&ws.join("inner/src/lib.rs"), "pub fn f() -> u32 { 7 }\n");
    write(
        &ws.join("outer/Cargo.toml"),
        "[package]\nname = \"outer\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(&ws.join("outer/src/main.rs"), "fn main() {}\n");
    write(&ws.join("outer/build.rs"), BUILD_RS);
    write(&ws.join(".cargo/config.toml"), "[alias]\nxb = \"build\"\n");
    let shim = root.join("targo-shim");
    let block = atpkg::build_cache::shim_block("targo").unwrap();
    let forwarding = if hop {
        let quoted = format!("'{}'", targo.to_string_lossy().replace('\'', "'\\''"));
        atpkg::build_cache::registered::shim_hop(
            &aterm_tempfile::bin_exe("atpkg"),
            "targo",
            &quoted,
            None,
        )
    } else {
        format!("exec '{}' \"$@\"\n", targo.display())
    };
    write(&shim, &format!("#!/bin/sh\n{block}{forwarding}"));

    let log = root.join("build.log");
    let messages = root.join("build-messages.jsonl");
    let ledger = root.join("ledger");
    let ready = root.join("build-script-ready");
    let release = root.join("build-script-release");
    let mut command = aterm_uds::spawnfd::command("/bin/sh");
    command
        .arg(&shim)
        .args(["--unverified", verb, "-q", "--message-format=json"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &home)
        .env("PWD", &ws)
        .env("BC_SHIM", &shim)
        .current_dir(&ws)
        .stdout(std::fs::File::create(&messages).unwrap())
        .stderr(std::fs::File::create(&log).unwrap());
    if hop {
        command
            .env(atpkg::build_admit::LEDGER_ENV, &ledger)
            .env("ATERM_STATE_HOME", root.join("state"))
            .env("ATERM_ADMIT", "now")
            .env("BC_HOP_READY", &ready)
            .env("BC_HOP_RELEASE", &release);
    }
    let mut child = command.spawn().unwrap();
    let witness = if hop {
        let script_pid = aterm_spec::wait::until_within(HANG, "real Cargo's build script", || {
            std::fs::read_to_string(&ready).ok()?.parse::<i32>().ok()
        });
        let parent = aterm_uds::process::read_process_parent(script_pid);
        let locked = parent.is_some_and(|pid| real_cargo_lock(pid, targo));
        let claims = aterm_capacity::Ledger::at(&ledger)
            .list()
            .expect("scratch ledger");
        let covered = parent.is_some_and(|pid| {
            claims
                .iter()
                .any(|claim| claim.pid == pid && claim.held.is_some() && claim.need.covers)
        });
        let ordinary = real_cargo_lock(std::process::id(), targo);
        std::fs::write(&release, "go").expect("release the build script");
        Some((locked, covered, ordinary))
    } else {
        None
    };
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > HANG {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the build did not finish in {HANG:?}: the nested cargo waits on a lock");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(
        status.success(),
        "{status}: {}",
        std::fs::read_to_string(&log).unwrap_or_default()
    );
    // Cargo succeeds only once the build script exited 0 on its own.
    tmp.ran();
    if let Some((locked, covered, ordinary)) = witness {
        assert!(
            locked,
            "the real Cargo's active locked file and image were not witnessed"
        );
        assert!(covered, "the real Cargo had no covering claim");
        assert!(
            !ordinary,
            "an ordinary ancestor cannot become Cargo by its file names"
        );
        aterm_spec::wait::until_within(HANG, "the outer claim let go", || {
            aterm_capacity::Ledger::at(&ledger)
                .list()
                .ok()?
                .is_empty()
                .then_some(())
        });
    }

    let cache: PathBuf = atpkg::build_cache::build_dir(&atpkg::build_cache::cache_root(&home), &ws);
    let script_outputs: Vec<PathBuf> = std::fs::read_to_string(&messages)
        .unwrap()
        .lines()
        .map(|line| aterm_json::from_str::<aterm_json::Value>(line).expect("Cargo JSON message"))
        .filter(|message| {
            message.get("reason").and_then(aterm_json::Value::as_str)
                == Some("build-script-executed")
        })
        .map(|message| {
            PathBuf::from(
                message
                    .get("out_dir")
                    .and_then(aterm_json::Value::as_str)
                    .expect("actual build-script output directory"),
            )
        })
        .collect();
    assert!(
        !script_outputs.is_empty(),
        "Cargo reported the actual build script's output"
    );
    let cache = cache.canonicalize().unwrap();
    for output in script_outputs {
        assert!(
            output.is_dir() && output.canonicalize().unwrap().starts_with(&cache),
            "actual intermediates in the cache: {}",
            output.display()
        );
    }
    assert!(
        ws.join("target/debug/outer").is_file(),
        "the binary in target/"
    );
    assert!(
        !ws.join("target/debug/deps").exists(),
        "no intermediates in target/"
    );
    assert!(
        !ws.join("target/debug/build").exists(),
        "no per-unit intermediates in target/"
    );
}

/// A one-file package `p` in a scratch home with no `Library`, a stand-in `targo` that writes
/// the words it was handed ([`FallbackRig::run`]), and the real block and hop naming the real
/// front, as an installed shim is laid.
struct FallbackRig {
    _tmp: aterm_tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    package: PathBuf,
    standin: PathBuf,
    shim: PathBuf,
}

/// The words a build typed `targo --unverified build` in [`FallbackRig`]'s package.
const TYPED: [&str; 2] = ["--unverified", "build"];

impl FallbackRig {
    fn new() -> Self {
        let tmp = aterm_tempfile::Builder::new()
            .prefix("atpkg-build-cache-fallback-")
            .tempdir()
            .unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let home = root.join("home");
        let package = home.join("p");
        write(
            &package.join("Cargo.toml"),
            "[package]\nname = \"p\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        write(&package.join("src/lib.rs"), "");
        let standin = root.join("targo");
        write(&standin, "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$BC_RAN\"\n");
        std::fs::set_permissions(
            &standin,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        let shim = root.join("targo-shim");
        let block = atpkg::build_cache::shim_block("targo").unwrap();
        let hop = atpkg::build_cache::registered::shim_hop(
            &aterm_tempfile::bin_exe("atpkg"),
            "targo",
            &format!("'{}'", standin.display()),
            None,
        );
        write(&shim, &format!("#!/bin/sh\n{block}{hop}"));
        Self {
            _tmp: tmp,
            root,
            home,
            package,
            standin,
            shim,
        }
    }

    /// `program args…` from the package, in the rig's home: its status, what it said, and the
    /// words the stand-in was handed where the build started.
    fn run(
        &self,
        program: &Path,
        args: &[&str],
    ) -> (std::process::ExitStatus, String, Option<String>) {
        let ran = self.root.join("ran");
        let _ = std::fs::remove_file(&ran);
        let mut command = aterm_uds::spawnfd::command(program);
        command
            .args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.home)
            .env("PWD", &self.package)
            .env("BC_RAN", &ran)
            .env(atpkg::build_admit::LEDGER_ENV, self.root.join("ledger"))
            .env("ATERM_STATE_HOME", self.root.join("state"))
            .env("ATERM_ADMIT", "now")
            .current_dir(&self.package);
        let output = aterm_uds::deadline::output_within(&mut command, HANG).unwrap();
        (
            output.status,
            String::from_utf8_lossy(&output.stderr).into_owned(),
            std::fs::read_to_string(&ran).ok(),
        )
    }

    /// [`TYPED`] through the shim.
    fn build(&self) -> (std::process::ExitStatus, String, Option<String>) {
        let shim = self.shim.to_str().unwrap();
        self.run(Path::new("/bin/sh"), &[&[shim], &TYPED[..]].concat())
    }

    /// The front handed the block's fallback claim on [`TYPED`], as a shim that selected the
    /// fallback hands it.
    fn claim_fallback(&self) -> (std::process::ExitStatus, String, Option<String>) {
        let fallback = Self::fallback();
        let claim = [
            &["__build_admit", "--shim", "targo", "config"][..],
            &[self.standin.to_str().unwrap(), "--config", &fallback],
            &TYPED,
        ]
        .concat();
        self.run(&aterm_tempfile::bin_exe("atpkg"), &claim)
    }

    /// The `--config` value of the block's in-tree fallback.
    fn fallback() -> String {
        format!(
            "build.build-dir='{}'",
            atpkg::build_cache::target_build_path(Path::new("{workspace-root}/target")).display()
        )
    }

    /// The package's quiet build directory under its own target.
    fn quiet(&self) -> PathBuf {
        atpkg::build_cache::target_build_path(&self.package.join("target"))
    }
}

/// The two words that place a build's intermediates at `value`, as the stand-in writes them.
fn config_pair(value: &str) -> String {
    format!("--config\n{value}\n")
}

/// A BUILD IN A HOME WITH NO CACHES FOLDER RUNS IN THE SHARED CACHE: the real block, hop and
/// front, in a home with no `Library`. The home may be written, so the block selects the
/// shared cache; the front admits that claim, lays the folders the cache is made in and becomes
/// the build, which is handed the shared cache's `--config` pair, the first time and again
/// once the folders stand. 0.199 selected the in-tree fallback there and refused every such
/// build (exit 75, `the selected quiet build target could not be resolved safely`).
/// NEGATIVE CONTROL: the in-tree fallback claim handed to the front in that home is no
/// placement the block selects there, and is refused (exit 75) with no build started.
#[test]
fn a_build_in_a_home_with_no_caches_folder_runs_in_the_shared_cache() {
    let rig = FallbackRig::new();
    assert!(!rig.home.join("Library").exists());
    let cache = atpkg::build_cache::cache_root(&rig.home);
    let shared = atpkg::build_cache::config_value(&cache).unwrap();
    for _ in 0..2 {
        let (status, said, words) = rig.build();
        assert!(status.success(), "{status}: {said}");
        let words = words.expect("the build started");
        assert!(words.contains(&config_pair(&shared)), "{words}");
        assert!(
            cache.parent().unwrap().is_dir(),
            "admission laid the folder the shared cache is made in"
        );
    }

    let (status, said, words) = rig.claim_fallback();
    assert_eq!(status.code(), Some(75), "{said}");
    assert!(
        said.contains("does not match current placement"),
        "the control's refusal: {said}"
    );
    assert_eq!(words, None, "no build started");
    assert!(!rig.quiet().exists(), "a refused claim prepares nothing");
}

/// A BUILD IN A HOME WHOSE CACHES FOLDER CANNOT BE WRITTEN RUNS ON THE IN-TREE FALLBACK, OWNED BY
/// NO RECEIPT: `Library/Caches` stands read-only, so the block selects the in-tree fallback and
/// the front can lay no index there; it prepares the package's quiet build directory by the
/// private-parent checks alone and becomes the build, writing nothing under the caches folder.
/// Before, the front refused every build in such a home (exit 75, `the target's quiet build
/// directory could not be owned`).
/// NEGATIVE CONTROL: where the caches folder reaches aterm's own through a link, nothing says
/// the home can record no owner, and the same build is refused in those words, unstarted.
#[test]
fn a_build_in_a_home_whose_caches_folder_cannot_be_written_runs_on_the_in_tree_fallback() {
    use std::os::unix::fs::PermissionsExt as _;
    for linked in [false, true] {
        let rig = FallbackRig::new();
        let caches = rig.home.join("Library/Caches");
        std::fs::create_dir_all(&caches).unwrap();
        if linked {
            let elsewhere = rig.root.join("elsewhere");
            std::fs::create_dir(&elsewhere).unwrap();
            std::os::unix::fs::symlink(&elsewhere, caches.join("aterm")).unwrap();
        }
        std::fs::set_permissions(&caches, std::fs::Permissions::from_mode(0o500)).unwrap();
        let (status, said, words) = rig.build();
        std::fs::set_permissions(&caches, std::fs::Permissions::from_mode(0o700)).unwrap();
        if linked {
            assert_eq!(status.code(), Some(75), "{said}");
            assert!(
                said.contains("quiet build directory could not be owned"),
                "the control's refusal: {said}"
            );
            assert_eq!(words, None, "no build started");
            assert!(!rig.quiet().exists());
            continue;
        }
        assert!(status.success(), "{status}: {said}");
        let words = words.expect("the build started");
        assert!(
            words.contains(&config_pair(&FallbackRig::fallback())),
            "{words}"
        );
        assert!(
            rig.quiet().is_dir(),
            "the quiet build directory is prepared"
        );
        assert_eq!(
            std::fs::read_dir(&caches).unwrap().count(),
            0,
            "nothing was written under the caches folder"
        );
    }
}

fn fake_cargo(dir: &Path, release: &str) -> PathBuf {
    let path = dir.join("cargo");
    let (release, trust) = match release {
        "trust" => ("1.99.0-dev", " 'trust: 0.1.0'"),
        other => (other, ""),
    };
    write(
        &path,
        &format!(
            "#!/bin/sh\nif [ \"$1\" = -vV ] || [ \"$2\" = -vV ]; then printf '%s\\n' 'cargo {release} (0123456789 2026-07-14)' \
             'release: {release}' 'commit-hash: 0123456789abcdef' 'host: aarch64-apple-darwin'{trust}; exit 0; fi\n\
             if [ -n \"$ATPKG_TEST_PID_TO\" ]; then echo $$ > \"$ATPKG_TEST_PID_TO\"; fi\n\
             printf '%s\\n' \"$@\"\n"
        ),
    );
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let warmed = aterm_uds::spawnfd::command(&path)
        .arg("-vV")
        .output()
        .unwrap();
    assert!(warmed.status.success(), "{warmed:?}");
    path
}

#[test]
fn cargo_in_a_session_gets_the_cache_only_where_it_builds_and_reads_it() {
    let tmp = aterm_tempfile::Builder::new()
        .prefix("atpkg-build-cache-reroute-")
        .tempdir()
        .unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(home.join("Library/Caches")).unwrap();
    let ws = home.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    write(&ws.join("Cargo.toml"), "[workspace]\nmembers = []\n");
    let config = atpkg::build_cache::config_value(&atpkg::build_cache::cache_root(&home)).unwrap();
    let command = |release: &str, args: &[&str], env: &[(&str, &str)]| {
        let bin = root.join(format!("bin-{release}"));
        fake_cargo(&bin, release);
        let mut cmd = aterm_uds::spawnfd::command(aterm_tempfile::bin_exe("atpkg"));
        cmd.arg("__reroute")
            .arg("cargo")
            .args(args)
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env(atpkg::build_admit::LEDGER_ENV, root.join("ledger"))
            .current_dir(&ws);
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd
    };
    let run = |release: &str, args: &[&str], env: &[(&str, &str)]| {
        let out = command(release, args, env).output().unwrap();
        assert!(out.status.success(), "{out:?}");
        String::from_utf8(out.stdout).unwrap()
    };
    assert_eq!(
        run("1.97.1", &["build", "-q"], &[]),
        format!("--config\n{config}\nbuild\n-q\n")
    );
    // A rerouted build becomes cargo by exec, in the process its caller started, and claims
    // nothing (BUILD-ADMIT): a front that claimed and stayed as cargo's parent would be what a
    // tab's readers name (`program=atpkg`), the naming gate the shim's hop failed.
    let pid_to = root.join("cargo-pid");
    let pid_env = pid_to.to_string_lossy().into_owned();
    let child = command(
        "1.97.1",
        &["build", "-q"],
        &[("ATPKG_TEST_PID_TO", &pid_env)],
    )
    .stdout(aterm_uds::spawnfd::Stdio::null())
    .spawn()
    .unwrap();
    let started = child.id();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(&pid_to).unwrap().trim(),
        started.to_string(),
        "cargo ran in the rerouting process itself"
    );
    assert!(
        !root.join("ledger").exists(),
        "a rerouted build claims nothing, in the scratch ledger its test names or this user's"
    );
    assert_eq!(
        run("1.97.1", &["+1.97.1", "test"], &[]),
        format!("+1.97.1\n--config\n{config}\ntest\n")
    );
    assert_eq!(run("1.97.1", &["metadata"], &[]), "metadata\n");
    let target = root.join("outside-home-target");
    assert_eq!(
        run(
            "1.97.1",
            &["build"],
            &[("CARGO_TARGET_DIR", target.to_str().unwrap())]
        ),
        format!(
            "--config\nbuild.build-dir='{}/.ubd/build.noindex'\nbuild\n",
            target.display()
        )
    );
    assert!(target.join(".ubd/build.noindex").is_dir());
    assert_eq!(
        run("1.97.1", &["build"], &[("CARGO_BUILD_BUILD_DIR", "/b")]),
        "build\n"
    );
    assert_eq!(run("1.85.0", &["build"], &[]), "build\n");
    assert_eq!(
        run("trust", &["check"], &[]),
        format!("--config\n{config}\ncheck\n")
    );
    assert_eq!(
        run("trust", &["build", "--unverified"], &[]),
        format!("--config\n{config}\nbuild\n--unverified\n")
    );
    for var in ["TARGO_TARGET_DIR", "TARGO_BUILD_TARGET_DIR"] {
        let target = home.join(format!("managed-{var}"));
        assert_eq!(
            run(
                "trust",
                &["+trust", "build"],
                &[(var, target.to_str().unwrap())]
            ),
            format!(
                "+trust\n--config\nbuild.build-dir='{}/.ubd/build.noindex'\nbuild\n",
                target.display()
            ),
            "{var}"
        );
        assert!(
            atpkg::build_cache::registered::roots(&home)
                .iter()
                .any(|r| r.root == target)
        );
        assert_eq!(
            run("1.97.1", &["build"], &[(var, target.to_str().unwrap())]),
            format!("--config\n{config}\nbuild\n"),
            "stock Cargo ignores {var}"
        );
    }
    // A directory-neutral CLI file preserves explicit custody without changing forwarded argv.
    write(&ws.join("settings.toml"), "[build]\njobs = 2\n");
    let neutral_args = [
        "+trust",
        "--config",
        "settings.toml",
        "build",
        "--target-dir",
        "configured.noindex",
    ];
    assert_eq!(
        run("trust", &neutral_args, &[]),
        format!("{}\n", neutral_args.join("\n"))
    );
    assert!(
        atpkg::build_cache::registered::roots(&home)
            .iter()
            .any(|r| r.root == ws.join("configured.noindex"))
    );
    for (value, leaf) in [
        (r#"env.ATPKG_NEUTRAL = 'a\b'"#, "inline-literal.noindex"),
        (
            r#"env.ATPKG_NEUTRAL = 'target-dir build-dir include'"#,
            "inline-data.noindex",
        ),
    ] {
        let args = ["+trust", "--config", value, "build", "--target-dir", leaf];
        assert_eq!(run("trust", &args, &[]), format!("{}\n", args.join("\n")));
        assert!(
            atpkg::build_cache::registered::roots(&home)
                .iter()
                .any(|r| r.root == ws.join(leaf))
        );
    }
    // Explicit command/environment targets outrank a config target. The original config
    // still reaches Cargo unchanged; the front records only the selected output.
    write(
        &ws.join("target-settings.toml"),
        "build.target-dir = 'unused-config-output'\n",
    );
    for (value, leaf) in [
        ("target-settings.toml", "override-file.noindex"),
        (
            r#"build."target\u002ddir"='unused-config-output'"#,
            "override-inline.noindex",
        ),
    ] {
        let args = ["+trust", "--config", value, "build", "--target-dir", leaf];
        assert_eq!(run("trust", &args, &[]), format!("{}\n", args.join("\n")));
        assert!(
            atpkg::build_cache::registered::roots(&home)
                .iter()
                .any(|r| r.root == ws.join(leaf))
        );
    }
    let env_target = ws.join("override-env.noindex");
    let args = ["+trust", "--config", "target-settings.toml", "build"];
    assert_eq!(
        run(
            "trust",
            &args,
            &[("CARGO_TARGET_DIR", env_target.to_str().unwrap())]
        ),
        format!("{}\n", args.join("\n"))
    );
    assert!(
        atpkg::build_cache::registered::roots(&home)
            .iter()
            .any(|r| r.root == env_target)
    );
    assert!(!ws.join("unused-config-output").exists());
    assert!(!home.join("unused-config-output").exists());
    let lower = ws.join("lower-priority.noindex");
    assert_eq!(
        run(
            "trust",
            &args,
            &[("CARGO_BUILD_TARGET_DIR", lower.to_str().unwrap())]
        ),
        format!("{}\n", args.join("\n"))
    );
    assert!(!lower.exists());
    assert!(!ws.join("unused-config-output").exists());
    assert!(
        atpkg::build_cache::registered::roots(&home)
            .iter()
            .any(|root| root.root == home.join("unused-config-output"))
    );
    for configs in [
        vec![
            "target-settings.toml",
            "build.target-dir='last-config.noindex'",
        ],
        vec![
            "build.target-dir='first-config.noindex'",
            "target-settings.toml",
        ],
    ] {
        let file_wins = configs.last() == Some(&"target-settings.toml");
        let mut args = vec!["build"];
        for config in configs {
            args.extend(["--config", config]);
        }
        assert_eq!(run("trust", &args, &[]), format!("{}\n", args.join("\n")));
        let expected = if file_wins {
            home.join("unused-config-output")
        } else {
            ws.join("last-config.noindex")
        };
        assert!(
            atpkg::build_cache::registered::roots(&home)
                .iter()
                .any(|root| root.root == expected)
        );
        assert!(!ws.join("first-config.noindex").exists());
    }
    for release in ["trust", "1.97.1"] {
        let child = format!("layers/include-{release}.toml");
        let settings = format!("include-{release}.toml");
        let leaf = format!("included-{release}.noindex");
        write(&ws.join(&child), &format!("build.target-dir='{leaf}'"));
        write(&ws.join(&settings), &format!("include=['{child}']"));
        for config in [settings.as_str(), &format!("include=['{child}']")] {
            let args = ["build", "--config", config];
            if release == "trust" {
                assert_eq!(run(release, &args, &[]), format!("{}\n", args.join("\n")));
                assert!(
                    atpkg::build_cache::registered::roots(&home)
                        .iter()
                        .any(|root| root.root == ws.join(&leaf))
                );
            } else {
                let refused = command(release, &args, &[]).output().unwrap();
                assert_eq!(refused.status.code(), Some(75), "{refused:?}");
                assert!(
                    refused.stdout.is_empty(),
                    "unknown upstream include capability starts no build"
                );
                assert!(!ws.join(&leaf).exists());
            }
        }
        assert!(!home.join(&leaf).exists());
        let cycle = format!("cycle-{release}.toml");
        write(&ws.join(&cycle), &format!("include=['{cycle}']"));
        let target = format!("refused-cycle-{release}.noindex");
        let refused = command(
            release,
            &["build", "--config", &cycle, "--target-dir", &target],
            &[],
        )
        .output()
        .unwrap();
        assert_eq!(refused.status.code(), Some(75), "{refused:?}");
        assert!(refused.stdout.is_empty());
        assert!(!ws.join(target).exists());
    }
    // Directory-changing calls retain the original argv and register the effective package,
    // while a query owns nothing. The public boundary only prints argv; custody is real.
    let moved = ws.join("moved");
    write(&moved.join("Cargo.toml"), "[workspace]\nmembers = []\n");
    let directory_args = [
        "+trust",
        "-Zunstable-options",
        "-C",
        "moved",
        "build",
        "--target-dir",
        "fresh.noindex",
    ];
    assert_eq!(
        run("trust", &directory_args, &[]),
        format!("{}\n", directory_args.join("\n"))
    );
    assert!(
        atpkg::build_cache::registered::roots(&home)
            .iter()
            .any(|r| r.root == moved.join("fresh.noindex"))
    );
    assert!(!ws.join("fresh.noindex").exists());
    let query_args = [
        "+trust",
        "-Zunstable-options",
        "-C",
        "moved",
        "help",
        "--target-dir",
        "query.noindex",
    ];
    assert_eq!(
        run("trust", &query_args, &[]),
        format!("{}\n", query_args.join("\n"))
    );
    assert!(!moved.join("query.noindex").exists());
    for release in ["trust", "1.97.1"] {
        let target = format!("escaped-{release}.noindex");
        let args = [
            "build",
            "--target-dir",
            target.as_str(),
            "--config",
            r#"http.user-agent = "aterm\u0020test""#,
            "--config",
            r#"build."target\u002ddir" = "elsewhere""#,
        ];
        assert_eq!(run(release, &args, &[]), format!("{}\n", args.join("\n")));
        assert!(
            atpkg::build_cache::registered::roots(&home)
                .iter()
                .any(|root| root.root == ws.join(&target)),
            "{release}: the actual escaped-value call owns its explicit output"
        );
        for (suffix, config) in [
            ("key", r#"build."target\u002ddir" = true"#),
            ("invalid", r#"http.user-agent = "bad\qescape""#),
        ] {
            let target = format!("refused-{release}-{suffix}.noindex");
            let refused = command(
                release,
                &["build", "--target-dir", &target, "--config", config],
                &[],
            )
            .output()
            .unwrap();
            assert_eq!(refused.status.code(), Some(75), "{refused:?}");
            assert!(
                refused.stdout.is_empty(),
                "a refused config starts no build"
            );
            assert!(
                !ws.join(target).exists(),
                "a refused config prepares no output"
            );
        }
    }
    write(
        &ws.join("includes/root.toml"),
        "include = ['nested/one.toml']\n",
    );
    write(
        &ws.join("includes/nested/one.toml"),
        "include = ['../leaf.toml']\n",
    );
    write(&ws.join("includes/leaf.toml"), "[build]\njobs = 2\n");
    for release in ["trust", "1.97.1"] {
        for (suffix, config) in [
            ("file", "includes/root.toml"),
            ("inline", "include=['includes/root.toml']"),
        ] {
            let target = format!("included-{release}-{suffix}.noindex");
            let args = ["build", "--target-dir", &target, "--config", config];
            if release != "trust" {
                let refused = command(release, &args, &[]).output().unwrap();
                assert_eq!(refused.status.code(), Some(75), "{refused:?}");
                assert!(refused.stdout.is_empty());
                continue;
            }
            assert_eq!(run(release, &args, &[]), format!("{}\n", args.join("\n")));
            assert!(
                atpkg::build_cache::registered::roots(&home)
                    .iter()
                    .any(|entry| entry.root == ws.join(&target))
            );
        }
    }
    for release in ["trust", "1.97.1"] {
        for (suffix, config) in [
            ("file", "includes/root.toml"),
            ("inline", "include=['includes/root.toml']"),
        ] {
            let chosen = format!("graph-target-{release}-{suffix}.noindex");
            write(
                &ws.join("includes/nested/one.toml"),
                &format!("include = ['../leaf.toml']\nbuild.target-dir = '{chosen}'\n"),
            );
            let expected = ws.join("includes").join(&chosen);
            assert!(!expected.exists());
            let args = ["build", "--config", config];
            if release != "trust" {
                let refused = command(release, &args, &[]).output().unwrap();
                assert_eq!(refused.status.code(), Some(75), "{refused:?}");
                assert!(refused.stdout.is_empty());
                continue;
            }
            assert_eq!(run(release, &args, &[]), format!("{}\n", args.join("\n")));
            assert!(
                atpkg::build_cache::registered::roots(&home)
                    .iter()
                    .any(|entry| entry.root == expected)
            );
            assert!(
                !ws.join(chosen).exists(),
                "target keeps the included file's definition base"
            );
        }
    }
    for release in ["trust", "1.97.1"] {
        for form in ["file", "inline", "environment"] {
            let target = format!("paired-target-{release}-{form}.noindex");
            let build = format!("paired-build-{release}-{form}.noindex");
            write(
                &ws.join("outputs/settings.toml"),
                "include=['nested/layout.toml']",
            );
            write(
                &ws.join("outputs/nested/layout.toml"),
                &format!("build.build-dir='{build}'"),
            );
            let mut args = vec!["build", "--target-dir", &target];
            if form != "environment" {
                args.extend([
                    "--config",
                    if form == "file" {
                        "outputs/settings.toml"
                    } else {
                        "include=['outputs/nested/layout.toml']"
                    },
                ]);
            }
            let environment = if form == "environment" {
                vec![("CARGO_BUILD_BUILD_DIR", build.as_str())]
            } else {
                vec![]
            };
            let expected_build = if form == "environment" {
                ws.join(&build)
            } else {
                ws.join("outputs").join(&build)
            };
            if release == "trust" {
                assert_eq!(
                    run(release, &args, &environment),
                    format!("{}\n", args.join("\n"))
                );
                let recorded = atpkg::build_cache::registered::roots(&home);
                for output in [ws.join(&target), expected_build] {
                    assert!(output.is_dir());
                    assert!(recorded.iter().any(|entry| entry.root == output));
                }
            } else {
                let refused = command(release, &args, &environment).output().unwrap();
                assert_eq!(refused.status.code(), Some(75), "{refused:?}");
                assert!(refused.stdout.is_empty());
                assert!(!ws.join(&target).exists() && !expected_build.exists());
            }
        }
    }
    // Discovered roots keep their definition bases through the actual routed shim.
    // The legacy file wins without reading the shadowed TOML output selection.
    for release in ["trust", "1.97.1"] {
        for form in ["discovered", "legacy"] {
            let target = ws.join(format!("discovered-target-{release}-{form}.noindex"));
            let build = format!("discovered-build-{release}-{form}.noindex");
            let config_file = ws.join(if form == "legacy" {
                ".cargo/config"
            } else {
                ".cargo/config.toml"
            });
            write(
                &config_file,
                &format!(
                    "include=['layers/layout.toml']\n[build]\ntarget-dir='{}'\n",
                    target.display()
                ),
            );
            write(
                &ws.join(".cargo/layers/layout.toml"),
                &format!("build.build-dir='{build}'"),
            );
            if form == "legacy" {
                write(&ws.join(".cargo/config.toml"), "build.build-dir='shadowed'");
            }
            let expected_build = ws.join(".cargo").join(build);
            if release == "trust" {
                assert_eq!(run(release, &["build"], &[]), "build\n");
                let recorded = atpkg::build_cache::registered::roots(&home);
                for output in [&target, &expected_build] {
                    assert!(output.is_dir());
                    assert!(recorded.iter().any(|entry| &entry.root == output));
                }
                assert!(!ws.join(".cargo/shadowed").exists());
            } else {
                let refused = command(release, &["build"], &[]).output().unwrap();
                assert_eq!(refused.status.code(), Some(75), "{refused:?}");
                assert!(refused.stdout.is_empty());
                assert!(!target.exists() && !expected_build.exists());
            }
            std::fs::remove_file(config_file).unwrap();
            if form == "legacy" {
                std::fs::remove_file(ws.join(".cargo/config.toml")).unwrap();
            }
        }
    }
    let blocked_build = ws.join("caller-intermediates");
    write(&blocked_build.join("keep"), "caller data");
    let refused = command(
        "trust",
        &[
            "build",
            "--target-dir",
            "blocked-pair.noindex",
            "--config",
            "build.build-dir='caller-intermediates'",
        ],
        &[],
    )
    .output()
    .unwrap();
    assert_eq!(refused.status.code(), Some(75), "{refused:?}");
    assert!(
        refused.stdout.is_empty(),
        "no producer starts on partial admission"
    );
    assert_eq!(
        std::fs::read_to_string(blocked_build.join("keep")).unwrap(),
        "caller data"
    );
    assert!(
        !atpkg::build_cache::registered::roots(&home)
            .iter()
            .any(|entry| entry.root == blocked_build)
    );
    write(&ws.join("includes/literal[one].toml"), "build.jobs = 2\n");
    for (suffix, leaf) in [
        ("output", "[build]\nbuild-dir = false\n"),
        ("cycle", "include = ['root.toml']\n"),
        ("brackets", "include = ['literal[one].toml']\n"),
        (
            "glob",
            "include = [{ path = 'absent*.toml', optional = true }]\n",
        ),
        (
            "braces",
            "include = [{ path = 'absent{one}.toml', optional = true }]\n",
        ),
    ] {
        write(&ws.join("includes/leaf.toml"), leaf);
        for release in ["trust", "1.97.1"] {
            let target = format!("refused-include-{release}-{suffix}.noindex");
            let refused = command(
                release,
                &[
                    "build",
                    "--target-dir",
                    &target,
                    "--config",
                    "includes/root.toml",
                ],
                &[],
            )
            .output()
            .unwrap();
            assert_eq!(refused.status.code(), Some(75), "{refused:?}");
            assert!(refused.stdout.is_empty());
            assert!(!ws.join(&target).exists());
            assert!(
                atpkg::build_cache::registered::roots(&home)
                    .iter()
                    .all(|entry| entry.root != ws.join(&target)),
                "a refused include graph grants no custody"
            );
        }
    }
    let package_home = root.join("package-home");
    write(
        &package_home.join("config.toml"),
        &format!("[build]\ntarget-dir = '{}'\n", target.display()),
    );
    let homed = [("TARGO_HOME", package_home.to_str().unwrap())];
    let refused = command("trust", &["check"], &homed).output().unwrap();
    assert_eq!(refused.status.code(), Some(75), "{refused:?}");
    assert!(
        refused.stdout.is_empty(),
        "unresolved package-home config starts no build"
    );
    assert_eq!(
        run("1.97.1", &["check"], &homed),
        format!("--config\n{config}\ncheck\n")
    );
}

/// PUBLIC BOUNDARY: these stock/Trust-identifying scripts only expose which argv the real
/// reroute execs. They never compile Rust; this suite itself runs under Trust.
#[test]
fn actual_cargo_reroutes_prepare_only_the_generated_placement_they_execute() {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    for branded in [false, true] {
        for shape in [
            "safe",
            "outside-target",
            "parent-link",
            "parent-writable",
            "override",
            "unsupported",
            "query",
            "plus-toolchain",
            "passthrough",
        ] {
            if branded && matches!(shape, "unsupported" | "plus-toolchain" | "passthrough") {
                continue;
            }
            let temp = aterm_tempfile::tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap();
            let home = root.join("home");
            let source = home.join("Library/ForeignSource");
            std::fs::create_dir_all(&source).unwrap();
            write(&source.join("Cargo.toml"), "[workspace]\nmembers = []\n");
            let target = if shape == "outside-target" {
                root.join("target")
            } else {
                home.join("target")
            };
            let outside = root.join("retained");
            std::fs::create_dir(&outside).unwrap();
            write(&outside.join("keep"), "retained output");
            let quiet = target.join(".ubd");
            let build = quiet.join("build.noindex");
            if matches!(
                shape,
                "parent-link"
                    | "parent-writable"
                    | "override"
                    | "unsupported"
                    | "query"
                    | "passthrough"
            ) {
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&target)
                    .unwrap();
                if shape == "parent-writable" {
                    std::fs::create_dir(&quiet).unwrap();
                    std::fs::set_permissions(&quiet, std::fs::Permissions::from_mode(0o777))
                        .unwrap();
                } else {
                    std::os::unix::fs::symlink(&outside, &quiet).unwrap();
                }
            }
            let bin = root.join("bin");
            let release = if shape == "unsupported" {
                "1.85.0"
            } else {
                "1.97.1"
            };
            fake_cargo(&bin, release);
            if branded {
                let layout = atpkg::store::Layout {
                    prefix: atpkg::store::default_prefix(&home),
                };
                let owned = layout.build_dir("trust", 1);
                std::fs::create_dir_all(owned.join("bin")).unwrap();
                let tool = fake_cargo(&root.join("managed-bin"), "trust");
                std::fs::copy(&tool, owned.join("bin/targo")).unwrap();
                atpkg::store::mark_build_ready(&owned).unwrap();
                atpkg::activate::install_shims(
                    &layout,
                    &owned,
                    &["targo".into()],
                    atpkg::activate::Aliases::Alab,
                )
                .unwrap();
                atpkg::activate::activate_build(&layout, &owned).unwrap();
            }
            let mut args = Vec::new();
            if branded {
                args.push("--unverified".to_owned());
            }
            if shape == "plus-toolchain" {
                args.push("+1.97.1".to_owned());
            }
            args.extend([
                if shape == "query" {
                    "metadata"
                } else {
                    "build"
                }
                .to_owned(),
                "--manifest-path".into(),
                source.join("Cargo.toml").display().to_string(),
                "--target-dir".into(),
                target.display().to_string(),
            ]);
            if shape == "override" {
                args.push("--config=build.build-dir='caller-layout'".into());
            }
            let pid_to = root.join("compiler-pid");
            let mut command = aterm_uds::spawnfd::command(aterm_tempfile::bin_exe("atpkg"));
            command
                .args(["__reroute", "cargo"])
                .args(&args)
                .env_clear()
                .env("HOME", &home)
                .env("XDG_CONFIG_HOME", root.join("config"))
                .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
                .env("ATPKG_TEST_PID_TO", &pid_to)
                .env(atpkg::build_admit::LEDGER_ENV, root.join("memory-ledger"))
                .stdout(aterm_uds::spawnfd::Stdio::piped())
                .stderr(aterm_uds::spawnfd::Stdio::piped())
                .current_dir(&source);
            if shape == "passthrough" {
                command.env(atpkg::reroute::PASSTHROUGH_ENV, "1");
            }
            let child = command.spawn().unwrap();
            let pid = child.id();
            let output = child.wait_with_output().unwrap();
            let admitted = !matches!(shape, "parent-link" | "parent-writable");
            assert_eq!(
                output.status.success(),
                admitted,
                "{branded} {shape}: {output:?}"
            );
            if admitted {
                assert_eq!(
                    std::fs::read_to_string(&pid_to).unwrap().trim(),
                    pid.to_string()
                );
            } else {
                assert_eq!(output.status.code(), Some(75));
                assert!(
                    !pid_to.exists(),
                    "unsafe automatic placement reached the tool"
                );
            }
            assert!(!root.join("memory-ledger").exists());
            assert!(atpkg::build_cache::registered::roots(&home).is_empty());
            assert_eq!(
                std::fs::read(outside.join("keep")).unwrap(),
                b"retained output"
            );
            assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
            if matches!(shape, "safe" | "outside-target" | "plus-toolchain") {
                assert!(build.is_dir());
                assert_eq!(
                    std::fs::metadata(&quiet).unwrap().permissions().mode() & 0o777,
                    0o700
                );
                assert_eq!(
                    std::fs::metadata(&build).unwrap().permissions().mode() & 0o777,
                    0o700
                );
            } else {
                assert!(!build.exists());
            }
            if shape == "parent-writable" {
                assert_eq!(
                    std::fs::metadata(&quiet).unwrap().permissions().mode() & 0o777,
                    0o777
                );
            }
        }
    }
}

#[test]
fn routed_workspace_owners_follow_deep_and_effective_package_home_boundaries() {
    for case in ["deep", "stopped", "moved-relative"] {
        let temporary = aterm_tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temporary.path()).unwrap();
        let home = root.join("home");
        std::fs::create_dir_all(home.join("Library/Caches")).unwrap();
        let workspace = home.join("workspace");
        let mut member = workspace.clone();
        for _ in 0..if case == "deep" { 34 } else { 2 } {
            member.push("n");
        }
        write(
            &workspace.join("Cargo.toml"),
            &format!(
                "[workspace]\nmembers=['{}']\n",
                member.strip_prefix(&workspace).unwrap().display()
            ),
        );
        write(
            &member.join("Cargo.toml"),
            "[package]\nname='member'\nversion='0.0.0'\n",
        );
        let bin = root.join("bin");
        fake_cargo(&bin, "trust");
        let launcher = home.join("launcher");
        std::fs::create_dir(&launcher).unwrap();
        let target = home.join("selected.noindex");
        let mut command = aterm_uds::spawnfd::command(aterm_tempfile::bin_exe("atpkg"));
        command.arg("__reroute").arg("cargo");
        if case == "moved-relative" {
            command.args(["-Zunstable-options", "-C"]).arg(&member);
        }
        command
            .arg("build")
            .arg("--target-dir")
            .arg(&target)
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env(atpkg::build_admit::LEDGER_ENV, root.join("ledger"))
            .current_dir(if case == "moved-relative" {
                &launcher
            } else {
                &member
            });
        let package_home = match case {
            "deep" => root.join("cargo-home"),
            "stopped" => member.parent().unwrap().to_path_buf(),
            "moved-relative" => PathBuf::from(".."),
            _ => unreachable!(),
        };
        let output = command.env("CARGO_HOME", package_home).output().unwrap();
        assert!(output.status.success(), "{case}: {output:?}");
        let recorded = atpkg::build_cache::registered::roots(&home);
        let selected = recorded.iter().find(|entry| entry.root == target).unwrap();
        let expected = if case == "deep" { &workspace } else { &member };
        assert_eq!(selected.owners, std::slice::from_ref(expected), "{case}");
    }
}
