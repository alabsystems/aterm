// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `release-build`: the Windows half of a release cut, as `targo --unverified ship
//! linux-build` is the Linux half (crates/aterm-release/src/linux_release.rs, `run_worker`).
//!
//! The release cutter runs on the Mac and claims a build number for a commit; it cannot
//! compile Windows images (aterm-release does not build on Windows, and no Mac cross-builds
//! the MSVC target). So a person carries the claim here, runs this verb in a clean checkout
//! at exactly that commit, and carries the result back: the portable zip
//! (`aterm-<version>-windows-x86_64.zip`) and its receipt ([`crate::handoff::Receipt`]),
//! which the cutter admits with `ship cut --windows-artifacts DIRECTORY`.
//!
//! The verb pins the identity the Linux worker pins (`release_env` in
//! crates/aterm-release/src/buildplan.rs: `ATERM_STAMP=commit`, the build number as
//! `SOURCE_DATE_EPOCH`, the short and full commit, no dev commits, `ATERM_RELEASE_BUILD`),
//! builds through the checkout's own `apps\aterm-win\build.ps1 -Zip` with the lane named
//! (never `auto`) and its driver given by path (`-Builder`; the script pins `RUSTC` to the
//! compiler beside it), after refusing any cargo configuration the commit does not hold
//! ([`check_cargo_inputs`]), which could put another compiler, wrapper or linker under the
//! receipt's hashes; and then holds the result to what it asked for: `BUILD.json` names the
//! version, build, commit and lane and is not dirty; the checkout is still the clean commit;
//! the compiler's files hash as they did before the build; and the directory it wrote is
//! admitted by the very check the cutter runs ([`crate::handoff::admit`]).
//!
//! The lane is the pinned Trust build when the `targo` on `PATH` is the build the commit's
//! `trust.lock` pins (its `commit-hash`), and otherwise the STOCK EXCEPTION release the
//! commit's `stock_pin.rs` pins. A Trust the lock does not pin is used in neither lane.

use aterm_uds::spawnfd::Stdio;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::handoff::{self, Cut, Lane, Pins};
use crate::pointer::Build;
use crate::proc::{self, Place};
use crate::repo::{GIT_LOCAL, git};

/// The longest a compiler's `-vV` or `rustup which` may take.
const ASK: Duration = Duration::from_secs(60);

/// The name of the scratch folder `build.ps1` lays out and zips in, inside `--out` while it
/// runs. It starts with `.`, so admission passes it over even if it outlives a failed run.
pub const SCRATCH: &str = ".building";

/// What `release-build` was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// The release version the cut froze.
    pub version: String,
    /// The build number the cut claimed.
    pub build: u64,
    /// The release commit, forty lowercase hex digits.
    pub commit: String,
    /// The directory the pair is written to: absent, or an empty directory.
    pub out: PathBuf,
    /// The checkout to build in; the working directory when `None`.
    pub repo: Option<PathBuf>,
}

/// Whether `version` is a release version: dot-separated decimal numbers, at least two.
#[must_use]
pub fn is_version(version: &str) -> bool {
    let mut parts = 0;
    for part in version.split('.') {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        parts += 1;
    }
    parts >= 2
}

impl Options {
    /// The options name a release: a version, a positive build and a full lowercase commit.
    ///
    /// # Errors
    /// Which of those they do not.
    pub fn check(&self) -> Result<(), String> {
        if !is_version(&self.version) {
            return Err(format!(
                "release-build: --version {:?} is no release version",
                self.version
            ));
        }
        if self.build == 0 {
            return Err("release-build: --build-number must be the cut's positive claim".into());
        }
        if !aterm_codec::hex::is_lower(&self.commit, 40) {
            return Err(format!(
                "release-build: --commit {:?} is not a full lowercase commit",
                self.commit
            ));
        }
        Ok(())
    }
}

/// The version `build.ps1` stamps: the first `version = "…"` line of the commit's root
/// `Cargo.toml`, read as `build.ps1` reads it.
#[must_use]
pub fn workspace_version(manifest: &str) -> Option<&str> {
    manifest.lines().find_map(|line| {
        let rest = line.trim_start().strip_prefix("version")?.trim_start();
        let rest = rest.strip_prefix('=')?.trim_start().strip_prefix('"')?;
        let (version, _) = rest.split_once('"')?;
        (!version.is_empty()).then_some(version)
    })
}

/// The commit's root `Cargo.toml` (`manifest`) stamps `version`, the cut's.
///
/// # Errors
/// The version it stamps instead, in one line.
pub fn check_stamped(manifest: &str, version: &str) -> Result<(), String> {
    let stamped = workspace_version(manifest);
    if stamped == Some(version) {
        return Ok(());
    }
    Err(format!(
        "the commit's Cargo.toml is version {stamped:?}, not the cut's {version}: the images \
         would carry another version"
    ))
}

/// The identity a release build of `commit` (abbreviated `short`) as build `build` is pinned
/// to, as the Linux worker's (`release_env`, non-macOS rows).
#[must_use]
pub fn release_env(commit: &str, short: &str, build: u64) -> Vec<(&'static str, String)> {
    vec![
        ("ATERM_STAMP", "commit".into()),
        ("SOURCE_DATE_EPOCH", build.to_string()),
        ("ATERM_BUILD_GIT_COMMIT", short.into()),
        ("ATERM_BUILD_GIT_COMMIT_FULL", commit.into()),
        ("ATERM_BUILD_DEV_COMMITS", "0".into()),
        ("ATERM_RELEASE_BUILD", "1".into()),
    ]
}

/// One toolchain's files and its compiler's answer to `-vV`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toolchain {
    /// Its lane.
    pub lane: Lane,
    /// The compiler executable.
    pub compiler: PathBuf,
    /// The build driver `build.ps1` runs.
    pub driver: PathBuf,
    /// The compiler's library.
    pub library: PathBuf,
    /// The compiler's `-vV`.
    pub version: String,
}

/// The digests of a toolchain's three files: compiler, driver, library.
pub type Digests = [String; 3];

impl Toolchain {
    /// The toolchain whose files sit in `bin`: `compiler` and `driver` there, and the one
    /// `rustc_driver-*.dll` beside them.
    ///
    /// # Errors
    /// A file that is not there, no library, or more than one.
    pub fn in_bin(
        lane: Lane,
        bin: &Path,
        compiler: &str,
        driver: &str,
        version: String,
    ) -> Result<Self, String> {
        let mut libraries = Vec::new();
        for entry in std::fs::read_dir(bin).map_err(|e| format!("{}: {e}", bin.display()))? {
            let entry = entry.map_err(|e| format!("{}: {e}", bin.display()))?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("rustc_driver-") && name.ends_with(".dll") {
                libraries.push(entry.path());
            }
        }
        let [library] = <[PathBuf; 1]>::try_from(libraries).map_err(|found| {
            format!(
                "{} holds {} rustc_driver-*.dll files, not one",
                bin.display(),
                found.len()
            )
        })?;
        let toolchain = Self {
            lane,
            compiler: bin.join(compiler),
            driver: bin.join(driver),
            library,
            version,
        };
        for file in [&toolchain.compiler, &toolchain.driver] {
            if !file.is_file() {
                return Err(format!("{} is not there", file.display()));
            }
        }
        Ok(toolchain)
    }

    /// The SHA-256 of its compiler, driver and library.
    ///
    /// # Errors
    /// A file that cannot be read.
    pub fn digests(&self) -> Result<Digests, String> {
        let one = |path: &Path| handoff::hash_regular(path, u64::MAX).map(|(sha, _, _)| sha);
        Ok([
            one(&self.compiler)?,
            one(&self.driver)?,
            one(&self.library)?,
        ])
    }
}

/// Which lane builds: [`Lane::Trust`] when `trust` (the Trust toolchain found on `PATH`, or
/// why there is none) is one the pins admit, else [`Lane::Stock`]; and why Trust was not
/// taken.
#[must_use]
pub fn choose(trust: &Result<Toolchain, String>, pins: &Pins<'_>) -> (Lane, Option<String>) {
    match trust {
        Ok(found) => match handoff::check_compiler(Lane::Trust, &found.version, pins) {
            Ok(()) => (Lane::Trust, None),
            Err(why) => (Lane::Stock, Some(why)),
        },
        Err(why) => (Lane::Stock, Some(why.clone())),
    }
}

fn path_dirs(env: &[(String, String)]) -> Vec<PathBuf> {
    crate::env::lookup(env, "PATH")
        .map(|path| std::env::split_paths(path).collect())
        .unwrap_or_default()
}

fn ask_version(compiler: &Path) -> Result<String, String> {
    let ran = proc::run(compiler, &["-vV"], Place::default(), ASK)?;
    if ran.ok() {
        Ok(ran.stdout.replace("\r\n", "\n").trim_end().to_string())
    } else {
        Err(format!("{} -vV exited {:?}", compiler.display(), ran.code))
    }
}

/// The Trust toolchain: the `targo.exe` on `PATH`, with `trustc.exe` (or `rustc.exe`) beside
/// it, the compiler `build.ps1 -Builder <that targo>` pins.
fn find_trust(env: &[(String, String)]) -> Result<Toolchain, String> {
    let targo = path_dirs(env)
        .into_iter()
        .map(|dir| dir.join("targo.exe"))
        .find(|exe| exe.is_file())
        .ok_or("no targo.exe on PATH")?;
    let bin = targo
        .parent()
        .ok_or_else(|| format!("{} has no folder", targo.display()))?;
    let compiler = ["trustc.exe", "rustc.exe"]
        .into_iter()
        .find(|name| bin.join(name).is_file())
        .ok_or_else(|| format!("{} has no trustc.exe or rustc.exe", bin.display()))?;
    let version = ask_version(&bin.join(compiler))?;
    Toolchain::in_bin(Lane::Trust, bin, compiler, "targo.exe", version)
}

/// The stock toolchain: rustup's install of `pin`, its `cargo.exe` the driver
/// `build.ps1 -Builder` is given.
fn find_stock(env: &[(String, String)], pin: &str) -> Result<Toolchain, String> {
    let rustup = path_dirs(env)
        .into_iter()
        .map(|dir| dir.join("rustup.exe"))
        .find(|exe| exe.is_file())
        .or_else(|| {
            let home = crate::env::lookup(env, "CARGO_HOME")
                .map(PathBuf::from)
                .or_else(|| {
                    crate::env::lookup(env, "USERPROFILE").map(|p| Path::new(p).join(".cargo"))
                })?;
            Some(home.join("bin").join("rustup.exe"))
        })
        .filter(|exe| exe.is_file())
        .ok_or("rustup is not installed")?;
    let ran = proc::run(
        &rustup,
        &["which", "--toolchain", pin, "cargo"],
        Place::default(),
        ASK,
    )?;
    let cargo = PathBuf::from(ran.stdout.trim());
    if !ran.ok() || !cargo.is_file() {
        return Err(format!(
            "stock Rust {pin} is not installed: `rustup toolchain install {pin} --profile minimal`"
        ));
    }
    let bin = cargo
        .parent()
        .ok_or_else(|| format!("{} has no folder", cargo.display()))?;
    let version = ask_version(&bin.join("rustc.exe"))?;
    Toolchain::in_bin(Lane::Stock, bin, "rustc.exe", "cargo.exe", version)
}

/// What a finished run made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Made {
    /// The zip, in `--out`.
    pub zip: PathBuf,
    /// Its receipt, beside it.
    pub receipt: PathBuf,
    /// What the receipt says.
    pub admitted: handoff::Receipt,
}

/// What a run that did not make the pair says, and the exit code it ends with.
pub type Refusal = (i32, String);

fn not_run(why: String) -> Refusal {
    (crate::NOT_RUN, format!("release-build: not run: {why}"))
}

fn failed(why: String) -> Refusal {
    (crate::FAILED, format!("release-build: {why}"))
}

/// `out` made ready: created when absent, refused when it holds anything. Its absolute
/// path, never a `\\?\` one, which `build.ps1` and git would each read differently.
fn fresh_out(out: &Path) -> Result<PathBuf, String> {
    let out = std::path::absolute(out).map_err(|e| format!("{}: {e}", out.display()))?;
    match std::fs::create_dir(&out) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let mut entries =
                std::fs::read_dir(&out).map_err(|e| format!("{}: {e}", out.display()))?;
            if entries.next().is_some() {
                return Err(format!(
                    "{} is not empty: name an absent or empty directory, so the carry holds \
                     this run's pair and nothing else",
                    out.display()
                ));
            }
        }
        Err(e) => return Err(format!("{}: {e}", out.display())),
    }
    Ok(out)
}

/// The checkout is the clean commit: HEAD is `commit` and `git status` names nothing.
fn clean_at(root: &Path, commit: &str) -> Result<(), String> {
    let head = git(root, &["rev-parse", "HEAD"], GIT_LOCAL)?;
    if head != commit {
        return Err(format!(
            "{} is at {head}, not the release commit {commit}: fetch the reserved commit (the \
             cutter's `git bundle create <file> refs/aterm-release/reserved`), then \
             `git checkout --detach {commit}` and `git submodule update --init vendor/astream`",
            root.display()
        ));
    }
    let status = git(
        root,
        &["status", "--porcelain", "--untracked-files=normal"],
        GIT_LOCAL,
    )?;
    if !status.is_empty() {
        return Err(format!(
            "{} is not the clean commit {commit}: {}",
            root.display(),
            status.lines().take(3).collect::<Vec<_>>().join("; ")
        ));
    }
    Ok(())
}

/// The commit's pins, read from the commit itself: the stock release of its `stock_pin.rs`,
/// the Trust build of its `trust.lock` (none when it has none).
fn commit_pins(root: &Path, commit: &str) -> Result<(String, Option<String>), String> {
    let source = git(
        root,
        &["show", &format!("{commit}:{}", handoff::STOCK_PIN_FILE)],
        GIT_LOCAL,
    )?;
    let stock = handoff::stock_pin(&source)
        .ok_or_else(|| format!("{commit}:{} pins no STOCK_PIN", handoff::STOCK_PIN_FILE))?
        .to_string();
    let lock = match git(
        root,
        &[
            "cat-file",
            "-e",
            &format!("{commit}:{}", handoff::TRUST_LOCK_FILE),
        ],
        GIT_LOCAL,
    ) {
        Ok(_) => Some(
            aterm_toolchain::trust_lock::parse(&git(
                root,
                &["show", &format!("{commit}:{}", handoff::TRUST_LOCK_FILE)],
                GIT_LOCAL,
            )?)
            .map_err(|e| format!("{commit}:{}: {e}", handoff::TRUST_LOCK_FILE))?
            .compiler_commit,
        ),
        Err(_) => None,
    };
    Ok((stock, lock))
}

/// The names cargo reads its configuration from, in a `.cargo` folder or in its home.
const CARGO_CONFIG: [&str; 2] = ["config.toml", "config"];

/// The prefixes of the variables that set cargo configuration (`CARGO_<KEY>` sets `<key>`)
/// able to steer a build off the commit's own: the compiler, a wrapper, the linker or flags
/// (`build`, `target`, `host`), the profile, the sources compiled (`source`, `patch`,
/// `registries`, `paths`), the build's environment (`env`) and unstable switches; and the
/// flag variables that replace the checkout's own flags. `CARGO_TARGET_DIR`, where the build
/// writes, is no part of what it compiles.
const STEERING: [&str; 12] = [
    "CARGO_BUILD_",
    "CARGO_TARGET_",
    "CARGO_HOST_",
    "CARGO_PROFILE_",
    "CARGO_SOURCE_",
    "CARGO_PATCH_",
    "CARGO_REGISTRIES_",
    "CARGO_PATHS",
    "CARGO_ENV_",
    "CARGO_UNSTABLE_",
    "CARGO_ENCODED_RUSTFLAGS",
    "RUSTFLAGS",
];

/// Cargo's home in `env`: `CARGO_HOME`, else `%USERPROFILE%\.cargo`.
fn cargo_home(env: &[(String, String)]) -> Option<PathBuf> {
    crate::env::lookup(env, "CARGO_HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::env::lookup(env, "USERPROFILE").map(|p| Path::new(p).join(".cargo")))
}

/// The cargo configuration files outside the checkout that a build in `root` reads: a
/// `config.toml` or `config` in cargo's home and in the `.cargo` folder of every folder above
/// `root` (cargo merges them under the checkout's own, from the folder it runs in up, and
/// `build.ps1` runs it in `root`). Each one that is there.
#[must_use]
pub fn foreign_cargo_config(env: &[(String, String)], root: &Path) -> Vec<PathBuf> {
    let mut folders: Vec<PathBuf> = root
        .ancestors()
        .skip(1)
        .map(|above| above.join(".cargo"))
        .collect();
    folders.extend(cargo_home(env));
    let mut found = Vec::new();
    for folder in folders {
        for name in CARGO_CONFIG {
            let file = folder.join(name);
            if std::fs::symlink_metadata(&file).is_ok() && !found.contains(&file) {
                found.push(file);
            }
        }
    }
    found
}

/// The variables of `env` that set a [`STEERING`] configuration, by name.
#[must_use]
pub fn steering_env(env: &[(String, String)]) -> Vec<String> {
    let mut names: Vec<String> = env
        .iter()
        .filter(|(_, value)| !value.is_empty())
        .map(|(name, _)| name.to_ascii_uppercase())
        .filter(|name| {
            name != "CARGO_TARGET_DIR" && STEERING.iter().any(|steer| name.starts_with(steer))
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The build reads no cargo configuration but the commit's: no variable that sets one
/// ([`steering_env`]), no configuration file outside the checkout
/// ([`foreign_cargo_config`]), and no file in the checkout's own `.cargo` folder that the
/// commit does not hold (an ignored one, which `git status` does not name). Such
/// configuration can name another compiler, wrapper or linker, or other sources, than the ones
/// whose identity the receipt records, and the build would not say so.
///
/// # Errors
/// What would be read, by name.
pub fn check_cargo_inputs(env: &[(String, String)], root: &Path) -> Result<(), String> {
    let mut found: Vec<String> = steering_env(env)
        .into_iter()
        .map(|name| format!("the variable {name}"))
        .collect();
    found.extend(
        foreign_cargo_config(env, root)
            .iter()
            .map(|file| file.display().to_string()),
    );
    let untracked = git(
        root,
        &["ls-files", "--others", "-z", "--", ".cargo"],
        GIT_LOCAL,
    )?;
    found.extend(
        untracked
            .split('\0')
            .filter(|name| !name.is_empty())
            .map(|name| root.join(name).display().to_string()),
    );
    if found.is_empty() {
        return Ok(());
    }
    Err(format!(
        "a release build reads no cargo configuration but its commit's, and this one would read \
         {}: each can name another compiler, wrapper, linker or source than the receipt records; \
         unset or move them aside for the release build",
        found.join(", ")
    ))
}

/// The arguments `release-build` runs `script` (`build.ps1`) with: lay out and zip `dist`, in
/// `toolchain`'s lane, with its driver by the very path whose files were hashed, so the
/// script builds with that program rather than one it finds for itself.
#[must_use]
pub fn build_ps1_args(
    script: &Path,
    dist: &Path,
    toolchain: &Toolchain,
) -> Vec<std::ffi::OsString> {
    let words: [&OsStr; 13] = [
        "-NoProfile".as_ref(),
        "-NonInteractive".as_ref(),
        "-ExecutionPolicy".as_ref(),
        "Bypass".as_ref(),
        "-File".as_ref(),
        script.as_os_str(),
        "-Out".as_ref(),
        dist.as_os_str(),
        "-Zip".as_ref(),
        "-Toolchain".as_ref(),
        toolchain.lane.word().as_ref(),
        "-Builder".as_ref(),
        toolchain.driver.as_os_str(),
    ];
    words.iter().map(|word| word.to_os_string()).collect()
}

/// Runs `apps\aterm-win\build.ps1` ([`build_ps1_args`]) in `root`, with the caller's
/// environment and `pins` over it; its output goes to this process's own streams.
fn build_ps1(
    env: &[(String, String)],
    root: &Path,
    dist: &Path,
    toolchain: &Toolchain,
    pins: &[(&'static str, String)],
) -> Result<(), String> {
    let shell = crate::update::powershell(env);
    let script = root.join("apps").join("aterm-win").join("build.ps1");
    let args = build_ps1_args(&script, dist, toolchain);
    let mut cmd = proc::command(
        &shell,
        &args,
        Place {
            env: None,
            cwd: Some(root),
        },
    );
    cmd.envs(pins.iter().map(|(k, v)| (*k, v.as_str())));
    cmd.stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let status = cmd
        .status()
        .map_err(|e| format!("{}: {e}", shell.display()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "{} exited {:?}; nothing was carried",
            script.display(),
            status.code()
        ))
    }
}

/// `BUILD.json` of the folder `build.ps1` laid out is this release build in `lane`.
///
/// # Errors
/// Which of its fields is not.
pub fn check_record(record: &Build, opts: &Options, lane: Lane) -> Result<(), String> {
    let label = format!("{} ", lane.word());
    if record.version != opts.version
        || record.commit_full != opts.commit
        || record.build_number != opts.build
        || record.dirty
        || !record.toolchain.starts_with(&label)
    {
        return Err(format!(
            "BUILD.json says v{} build {} at {} ({}, dirty {}), not v{} build {} at {} in the \
             {} lane, clean",
            record.version,
            record.build_number,
            record.commit_full,
            record.toolchain,
            record.dirty,
            opts.version,
            opts.build,
            opts.commit,
            lane.word()
        ));
    }
    Ok(())
}

/// Builds `opts`' release zip and writes it and its receipt into `opts.out`.
///
/// # Errors
/// The refusal and the exit code it ends with: [`crate::NOT_RUN`] when nothing was built
/// (the checkout, `--out` or the toolchain is not ready), [`crate::FAILED`] when the build
/// or a check of what it made failed.
pub fn release_build(opts: &Options, env: &[(String, String)]) -> Result<Made, Refusal> {
    opts.check().map_err(|e| (crate::USAGE, e))?;
    let cwd =
        std::env::current_dir().map_err(|e| not_run(format!("the working directory: {e}")))?;
    let start = opts.repo.clone().unwrap_or(cwd);
    let root =
        PathBuf::from(git(&start, &["rev-parse", "--show-toplevel"], GIT_LOCAL).map_err(not_run)?);
    clean_at(&root, &opts.commit).map_err(not_run)?;
    check_cargo_inputs(env, &root).map_err(not_run)?;
    let short =
        git(&root, &["rev-parse", "--short=12", &opts.commit], GIT_LOCAL).map_err(not_run)?;
    if short.len() < 12 || !opts.commit.starts_with(&short) {
        return Err(not_run(format!(
            "git abbreviated {} as {short:?}",
            opts.commit
        )));
    }
    let source_tree = git(
        &root,
        &["rev-parse", &format!("{}^{{tree}}", opts.commit)],
        GIT_LOCAL,
    )
    .map_err(not_run)?;
    let manifest = git(
        &root,
        &["show", &format!("{}:Cargo.toml", opts.commit)],
        GIT_LOCAL,
    )
    .map_err(not_run)?;
    check_stamped(&manifest, &opts.version).map_err(not_run)?;
    let (stock, trust_commit) = commit_pins(&root, &opts.commit).map_err(not_run)?;
    let pins = Pins {
        stock: &stock,
        trust_commit: trust_commit.as_deref(),
    };
    let trust = find_trust(env);
    let (lane, why_not_trust) = choose(&trust, &pins);
    let toolchain = match (lane, trust) {
        (Lane::Trust, Ok(found)) => found,
        _ => find_stock(env, &stock).map_err(not_run)?,
    };
    handoff::check_compiler(lane, &toolchain.version, &pins).map_err(not_run)?;
    println!(
        "release-build: v{} build {} at {} in the {} lane ({}){}",
        opts.version,
        opts.build,
        &opts.commit[..12],
        lane.word(),
        toolchain.compiler.display(),
        why_not_trust.map_or_else(String::new, |why| format!("; not Trust: {why}"))
    );
    let before = toolchain.digests().map_err(not_run)?;
    // A folder inside the checkout or its target folder is refused by build.ps1, and one the
    // checkout would see as untracked fails the clean check after the build.
    let out = fresh_out(&opts.out).map_err(not_run)?;
    let scratch = out.join(SCRATCH);
    std::fs::create_dir(&scratch).map_err(|e| not_run(format!("{}: {e}", scratch.display())))?;
    let dist = scratch.join("aterm");
    let made = build_ps1(
        env,
        &root,
        &dist,
        &toolchain,
        &release_env(&opts.commit, &short, opts.build),
    )
    .and_then(|()| {
        finish(&Built {
            opts,
            root: &root,
            out: &out,
            scratch: &scratch,
            dist: &dist,
            toolchain: &toolchain,
            before: &before,
            source_tree: &source_tree,
            pins: &pins,
        })
    });
    // The scratch is this run's own folder: the zip has left it, and the laid-out folder
    // and anything a failed build left are not part of the carry.
    let _ = std::fs::remove_dir_all(&scratch);
    made.map_err(failed)
}

/// A run whose build exited 0: what [`finish`] holds the build to.
struct Built<'a> {
    opts: &'a Options,
    root: &'a Path,
    out: &'a Path,
    scratch: &'a Path,
    dist: &'a Path,
    toolchain: &'a Toolchain,
    before: &'a Digests,
    source_tree: &'a str,
    pins: &'a Pins<'a>,
}

/// Holds what the build made to what was asked, moves the zip into `--out`, writes its
/// receipt and admits the pair as the cutter will.
fn finish(built: &Built<'_>) -> Result<Made, String> {
    let Built {
        opts,
        root,
        out,
        scratch,
        dist,
        toolchain,
        before,
        source_tree,
        pins,
    } = *built;
    clean_at(root, &opts.commit)
        .map_err(|e| format!("after the build: {e}; its images are not the commit's"))?;
    let after = toolchain.digests()?;
    if after != *before {
        return Err(format!(
            "the {} toolchain's files changed while it built ({}): its identity is not one",
            toolchain.lane.word(),
            toolchain.compiler.display()
        ));
    }
    let record = Build::read(dist)?
        .ok_or_else(|| format!("build.ps1 wrote no BUILD.json in {}", dist.display()))?;
    check_record(&record, opts, toolchain.lane)?;
    let asset = handoff::TARGET.asset_name(&opts.version);
    let built = scratch.join(&asset);
    let zip = out.join(&asset);
    std::fs::rename(&built, &zip)
        .map_err(|e| format!("{} to {}: {e}", built.display(), zip.display()))?;
    let (sha256, size, _) =
        handoff::hash_regular(&zip, aterm_update_core::windows::WINDOWS_ZIP_MAX_BYTES)?;
    let [compiler_sha256, driver_sha256, compiler_library_sha256] = after;
    let receipt = handoff::Receipt {
        schema: handoff::SCHEMA,
        version: opts.version.clone(),
        build_number: opts.build,
        commit: opts.commit.clone(),
        target: handoff::TARGET.triple().to_string(),
        asset: asset.clone(),
        sha256,
        size,
        source_tree: source_tree.to_string(),
        lane: toolchain.lane.word().to_string(),
        compiler_version: toolchain.version.clone(),
        compiler_sha256,
        driver_sha256,
        compiler_library_sha256,
    };
    let text = receipt.to_toml()?;
    let receipt_path = out.join(handoff::receipt_name(&asset));
    {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&receipt_path)
            .map_err(|e| format!("{}: {e}", receipt_path.display()))?;
        // Not flushed: the pair is carried by hand once this run has admitted it, and a receipt
        // a power loss cut short is refused by that admission and by the cutter's.
        file.write_all(text.as_bytes())
            .map_err(|e| format!("{}: {e}", receipt_path.display()))?;
    }
    let admitted = handoff::admit(
        out,
        &Cut {
            version: &opts.version,
            build: opts.build,
            commit: &opts.commit,
            source_tree,
        },
        pins,
    )
    .map_err(|e| format!("the pair this run wrote is not admitted: {e}"))?;
    Ok(Made {
        zip,
        receipt: receipt_path,
        admitted,
    })
}

#[cfg(test)]
#[path = "release_tests.rs"]
mod tests;
