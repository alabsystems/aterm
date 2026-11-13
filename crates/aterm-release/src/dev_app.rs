// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `ship dev-app`: build this checkout into `aterm (dev).app`, sign it locally, and install it.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Output;

use aterm_uds::deadline::{FIRST_EXEC_HANG, RAN, RunError, stamp_exec};
use aterm_uds::spawnfd::Command;

use crate::dev_identity::{self, CODESIGN, Run, WAIT};

/// The dev build's `CFBundleIdentifier`: never the release's.
pub const DEV_BUNDLE_ID: &str = "com.aterm.aterm.dev";

/// The dev build's `CFBundleDisplayName`.
pub const DEV_DISPLAY_NAME: &str = "aterm (dev)";

/// The dev build's bundle name on disk, in `--out` and where it is installed: never `aterm.app`.
pub const DEV_APP_NAME: &str = "aterm (dev).app";

/// The updater's dev mark (`aterm_update::bundle::DEV_BUILD_KEY`).
pub const DEV_MARK_KEY: &str = "ATermDevBuild";

/// The SHA-256 of the `aterm` executable a dev bundle was assembled from. Two builds of one
/// dirty tree share their version, build and commit, and a running aCaffeine mug tells that its
/// bundle was replaced by the `Info.plist` alone (`aterm_acaffeine::code::code_fingerprint`), so
/// this key is what changes when only the code did.
pub const DEV_BINARY_KEY: &str = "ATermDevBinarySHA256";

const LSREGISTER: &str = "/System/Library/Frameworks/CoreServices.framework/Frameworks/\
                          LaunchServices.framework/Support/lsregister";

const APPLICATIONS: &str = "/Applications";

/// Beside the install, never inside it (that would break its seal): the stamp of its first
/// exec, kept until that exec showed it ran ([`first_run`]).
const EXEC_MARKS: &str = ".aterm-dev-app.exec.noindex";

/// Beside the install and outside [`EXEC_MARKS`] (which a proven first run removes): the lock
/// one [`install`] into a directory holds while it runs.
const INSTALL_LOCK: &str = ".aterm-dev-app.lock";

pub const USAGE: &str = "
  targo --unverified ship dev-app [--profile release|test] [--sign-id IDENTITY] [--out DIRECTORY]
                 [--no-install | --install-dir DIRECTORY] [--no-link] [--no-pin]
      Build this checkout, dirty or not, into `aterm (dev).app` with the Trust
      toolchain the cutter resolves (by default `targo --unverified build
      --release -p aterm --bin aterm`): its own bundle id (com.aterm.aterm.dev),
      this Mac's slice only, HEAD's committer epoch as its build number. It is signed locally and
      installed beside the release; nothing is claimed, notarized or published.
        --profile PROFILE   release (default) or test. Test builds the ordinary app
                            with `build --profile test`, using debug/aterm for
                            native validation; it does not build a test harness.
        --sign-id IDENTITY  sign with this codesign identity; `-` signs ad hoc.
                            Default: the \"aterm dev\" identity once `ship dev-app
                            identity` made it (macOS privacy grants then survive a
                            rebuild), else ad hoc; refuses, saying why, when its
                            keychain exists but cannot be used
        --out DIRECTORY     assemble there (default: target/.ubd/dev-app.noindex/)
        --no-install        assemble only
        --install-dir DIRECTORY
                            install there instead of /Applications (or
                            ~/Applications when /Applications is not writable),
                            and tell LaunchServices nothing
        --no-link           leave ~/.local/bin/aterm and amacs as they are; by
                            default each becomes a link to this build (made if
                            absent), which never updates, and what it displaced
                            is named (a file that is not a link is left alone)
        --no-pin            omit the ATermDevBuild mark: the updater then tries
                            this install and every update fails

  targo --unverified ship dev-app identity [--remove] [--keychain FILE]
      Make the self-signed \"aterm dev\" code-signing identity `ship dev-app` signs
      with by default, in ~/Library/Keychains/aterm-dev.keychain-db (empty
      password, never locks, on the search list): no dialog, nothing typed.
      Idempotent; ends by signing a scratch bundle with it.
        --remove            delete that keychain (only one that opens with the
                            empty password); privacy grants made under it are lost
        --keychain FILE     act on FILE instead; it is not put on the search list,
                            so the signing self-check is skipped unless FILE is
                            on it already
";

/// A parsed `ship dev-app …`.
#[derive(Debug, PartialEq, Eq)]
pub enum Cmd {
    Help,
    Build(Options),
    /// `identity`: make (or `--remove`) the "aterm dev" signing identity.
    Identity {
        remove: bool,
        keychain: Option<PathBuf>,
    },
}

/// How a dev build is made and where it goes.
#[derive(Debug, PartialEq, Eq)]
pub struct Options {
    /// Cargo profile for the ordinary executable, never a prebuilt binary override.
    pub profile: Profile,
    /// Where the bundle is assembled; `None` is the checkout's quiet development output.
    pub out: Option<PathBuf>,
    pub install: Install,
    /// The [`DEV_MARK_KEY`] mark (off with `--no-pin`).
    pub pin: bool,
    /// Point `~/.local/bin/aterm` and `amacs` at the installed build (off with `--no-link`).
    pub link: bool,
    pub sign: SignWith,
}

/// Supported profiles for building the executable before canonical bundle assembly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// The existing optimized development bundle default.
    Release,
    /// The ordinary app built in Cargo's test profile for native validation.
    Test,
}

impl Profile {
    const fn name(self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::Test => "test",
        }
    }

    const fn cargo_args(self) -> &'static [&'static str] {
        match self {
            Self::Release => &["--release"],
            Self::Test => &["--profile", "test"],
        }
    }

    fn binary(self, target: &Path) -> PathBuf {
        target
            .join(match self {
                Self::Release => "release",
                Self::Test => "debug",
            })
            .join("aterm")
    }
}

/// Where the assembled bundle is installed.
#[derive(Debug, PartialEq, Eq)]
pub enum Install {
    /// `/Applications`, or `~/Applications` when that is not writable.
    Applications,
    /// `--install-dir`: there, and nothing else is told.
    Into(PathBuf),
    /// `--no-install`.
    No,
}

/// What signs the bundle.
#[derive(Debug, PartialEq, Eq)]
pub enum SignWith {
    /// The "aterm dev" identity when its keychain exists, else ad hoc.
    Default,
    /// `--sign-id -`.
    AdHoc,
    /// `--sign-id IDENTITY`.
    Identity(String),
}

/// Parse what follows `dev-app`.
/// # Errors
/// A usage error, said in one line.
pub fn parse<'a>(it: &mut impl Iterator<Item = &'a str>) -> Result<Cmd, String> {
    let mut args = it.peekable();
    if args.peek() == Some(&"identity") {
        args.next();
        let (mut remove, mut keychain) = (false, None);
        while let Some(flag) = args.next() {
            match flag {
                "--remove" if !remove => remove = true,
                "--keychain" if keychain.is_none() => {
                    let file = args
                        .next()
                        .filter(|f| !f.is_empty() && !f.starts_with('-'))
                        .ok_or("--keychain needs a file")?;
                    keychain = Some(PathBuf::from(file));
                }
                "-h" | "--help" => return Ok(Cmd::Help),
                other => {
                    return Err(format!(
                        "dev-app identity takes --remove and --keychain FILE, once each (got \
                         {other:?})"
                    ));
                }
            }
        }
        return Ok(Cmd::Identity { remove, keychain });
    }
    let mut opts = Options {
        profile: Profile::Release,
        out: None,
        install: Install::Applications,
        pin: true,
        link: true,
        sign: SignWith::Default,
    };
    let (mut no_install, mut install_dir, mut sign, mut profile) = (false, None, None, None);
    let value = |flag: &str, v: Option<&str>, dash: bool| match v {
        Some(v) if !v.is_empty() && (!v.starts_with('-') || (dash && v == "-")) => {
            Ok(v.to_string())
        }
        _ => Err(format!("{flag} needs a value")),
    };
    while let Some(flag) = args.next() {
        let again = || format!("{flag} given twice");
        match flag {
            "-h" | "--help" => return Ok(Cmd::Help),
            "--profile" => {
                let selected = match value(flag, args.next(), false)?.as_str() {
                    "release" => Profile::Release,
                    "test" => Profile::Test,
                    _ => return Err("--profile takes release or test".into()),
                };
                if profile.replace(selected).is_some() {
                    return Err(again());
                }
            }
            "--out" => {
                let dir = value(flag, args.next(), false)?;
                if opts.out.replace(PathBuf::from(dir)).is_some() {
                    return Err(again());
                }
            }
            "--install-dir" => {
                let dir = value(flag, args.next(), false)?;
                if install_dir.replace(PathBuf::from(dir)).is_some() {
                    return Err(again());
                }
            }
            "--sign-id" => {
                let id = value(flag, args.next(), true)?;
                if sign.replace(id).is_some() {
                    return Err(again());
                }
            }
            "--no-install" if !no_install => no_install = true,
            "--no-pin" if opts.pin => opts.pin = false,
            "--no-link" if opts.link => opts.link = false,
            "--no-install" | "--no-pin" | "--no-link" => return Err(again()),
            other => return Err(format!("unknown dev-app flag {other:?}")),
        }
    }
    opts.install = match (no_install, install_dir) {
        (true, Some(_)) => {
            return Err("--no-install and --install-dir contradict each other".into());
        }
        (true, None) => Install::No,
        (false, Some(dir)) => Install::Into(dir),
        (false, None) => Install::Applications,
    };
    opts.profile = profile.unwrap_or(Profile::Release);
    opts.sign = match sign.as_deref() {
        None => SignWith::Default,
        Some("-") => SignWith::AdHoc,
        Some(id) => SignWith::Identity(id.to_string()),
    };
    Ok(Cmd::Build(opts))
}

/// `ship dev-app identity [--remove] [--keychain FILE]`.
/// # Errors
/// [`dev_identity::create`]'s or [`dev_identity::remove`]'s refusal.
pub fn run_identity(remove: bool, keychain: Option<&Path>) -> Result<(), String> {
    let home = aterm_types::dirs::home_dir().ok_or("ship dev-app identity needs HOME")?;
    let cwd = std::env::current_dir().map_err(|e| format!("the current directory: {e}"))?;
    let (path, manage) = identity_keychain(keychain, &home, &cwd);
    if remove {
        dev_identity::remove(&path, &dev_identity::Bounded)
    } else {
        dev_identity::create(&path, manage, &dev_identity::Bounded)
    }
}

fn identity_keychain(keychain: Option<&Path>, home: &Path, cwd: &Path) -> (PathBuf, bool) {
    match keychain {
        Some(path) => (cwd.join(path), false),
        None => (dev_identity::default_keychain(home), true),
    }
}

/// The paths a build stamp is of, as `crates/aterm/build.rs` asks git of them.
#[path = "../../aterm/stamp_paths.rs"]
mod stamp_paths;

/// The build facts, read from the checkout once, as `crates/aterm/build.rs` would read them: the
/// plist is stamped with them and the build is pinned to them ([`Stamp::pins`]), so plist and
/// binary agree even when HEAD moves mid-build.
#[derive(Debug, PartialEq, Eq)]
struct Stamp {
    version: String,
    /// HEAD's committer epoch.
    build: u64,
    /// HEAD's 12-digit id, `-dirty` when the tree has changes outside the measured ledger
    /// ([`stamp_paths::PATHSPEC`]).
    commit: String,
    commit_full: String,
    /// Commits since the newest stable product tag; `0` without one.
    dev_commits: u64,
}

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = aterm_uds::spawnfd::command(aterm_uds::os_path!("git"))
        .current_dir(root)
        .args(args)
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

impl Stamp {
    fn of(root: &Path) -> Result<Self, String> {
        let manifest = root.join("Cargo.toml");
        let text = crate::durable::read_text(&manifest)
            .map_err(|e| format!("read {}: {e}", manifest.display()))?;
        let version = crate::publish::workspace_version(&text).map_err(|e| e.to_string())?;
        let epoch = git(root, &["show", "-s", "--format=%ct", "HEAD"])?;
        let build = Some(epoch.trim())
            .filter(|s| !s.starts_with('+'))
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| format!("HEAD's committer epoch {epoch:?} is not a number"))?;
        let mut commit = git(root, &["rev-parse", "--short=12", "HEAD"])?;
        let status = [&["status", "--porcelain"][..], &stamp_paths::PATHSPEC].concat();
        if !git(root, &status)?.is_empty() {
            commit.push_str("-dirty");
        }
        let commit_full = git(root, &["rev-parse", "HEAD"])?;
        let dev_commits = git(root, &["tag", "--list"])
            .ok()
            .and_then(|tags| aterm_stamp::describe_release_args(&tags))
            .and_then(|args| git(root, &args.iter().map(String::as_str).collect::<Vec<_>>()).ok())
            .and_then(|tag| git(root, &["rev-list", &format!("{tag}..HEAD"), "--count"]).ok())
            .and_then(|count| {
                let count = count.trim();
                (!count.starts_with('+'))
                    .then(|| count.parse().ok())
                    .flatten()
            })
            .unwrap_or(0);
        Ok(Self {
            version,
            build,
            commit,
            commit_full,
            dev_commits,
        })
    }

    /// The identity record's inputs (`aterm_stamp::input`), `commit` mode with this stamp's pins.
    fn pins(&self) -> [(&'static str, String); 5] {
        use aterm_stamp::input;
        [
            (input::MODE, "commit".to_string()),
            (input::EPOCH, self.build.to_string()),
            (input::COMMIT, self.commit.clone()),
            (input::FULL_COMMIT, self.commit_full.clone()),
            (input::DEV_COMMITS, self.dev_commits.to_string()),
        ]
    }
}

fn built_binary(target: &Path, profile: Profile) -> Result<PathBuf, String> {
    let bin = profile.binary(target);
    if !bin.is_file() {
        return Err(format!(
            "no {} after the {} profile build — a target setting redirected it",
            bin.display(),
            profile.name()
        ));
    }
    Ok(bin)
}

fn build_command(
    stage2: &Path,
    root: &Path,
    target: &Path,
    build_dir: Option<&str>,
    path: &OsStr,
    stamp: &Stamp,
    profile: Profile,
) -> Command {
    let mut joined = OsString::from(stage2.as_os_str());
    if !path.is_empty() {
        joined.push(":");
        joined.push(path);
    }
    let mut cmd = aterm_uds::spawnfd::command(stage2.join("targo"));
    if let Some(value) = build_dir {
        cmd.arg("--config").arg(value);
    }
    cmd.current_dir(root)
        .args(["--unverified", "build"])
        .args(profile.cargo_args())
        .args(["-p", "aterm", "--bin", "aterm", "--target-dir"])
        .arg(target)
        .env("PATH", joined);
    for key in [
        "RUSTC",
        "CARGO_BUILD_RUSTC",
        "RUSTUP_TOOLCHAIN",
        "CARGO_BUILD_TARGET",
        // A dev app is no release product and carries no published index run.
        aterm_stamp::input::RELEASE,
        aterm_stamp::input::INDEX_HINT,
    ] {
        cmd.env_remove(key);
    }
    for (key, value) in stamp.pins() {
        cmd.env(key, value);
    }
    cmd
}

fn first_line(run: &dyn Run, program: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = aterm_uds::spawnfd::command(program);
    cmd.args(args);
    let out = run.run(&mut cmd, WAIT).ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().next().map(str::to_string)
}

/// The first line a fresh install's `exe --version` prints: its first exec, stamped in
/// `marks` before it starts and held to [`FIRST_EXEC_HANG`]. Only an exit of its own shows
/// it ran and takes `marks` away; a probe killed or interrupted leaves them holding the
/// install in place ([`install`]).
fn first_run(run: &dyn Run, exe: &Path, marks: &Path) -> Option<String> {
    let _ = std::fs::create_dir_all(marks);
    let _ = std::fs::remove_file(marks.join(RAN));
    stamp_exec(marks);
    let mut cmd = aterm_uds::spawnfd::command(exe);
    cmd.arg("--version");
    let out = run.run(&mut cmd, FIRST_EXEC_HANG).ok()?;
    if out.status.code().is_some() {
        let _ = aterm_uds::deadline::try_mark_ran(marks);
        let _ = std::fs::remove_dir_all(marks);
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().next().map(str::to_string)
}

/// `ship dev-app [...]`: build, assemble, sign and (unless `--no-install`) install.
/// # Errors
/// The first step that could not be done, said with its remedy; nothing is installed after a failed
/// step.
pub fn run_build(root: &Path, opts: &Options) -> Result<(), String> {
    let run = &dev_identity::Bounded;
    if !opts.pin {
        eprintln!(
            "ship dev-app: WARNING — --no-pin leaves this build UNMARKED, so the in-session \
             update swap refuses it as a rollback source and every update fails on it; \
             reinstall a signed release (tools/install.sh) to restore updates"
        );
    }
    let home = aterm_types::dirs::home_dir().ok_or("ship dev-app needs HOME")?;
    let stamp = Stamp::of(root)?;
    let targo = crate::gates::resolve_targo().map_err(|e| {
        format!(
            "{e}\nship dev-app builds with the Trust toolchain only; fix: {}",
            crate::gates::TRUST_TOOLCHAIN_REMEDIES
        )
    })?;
    let stage2 = targo.parent().unwrap_or(Path::new("")).to_path_buf();
    let version =
        first_line(run, &targo, &["--version"]).unwrap_or_else(|| "version unknown".into());
    println!("==> toolchain: {version} ({})", targo.display());
    let env = aterm_verify::EnvSnapshot::capture();
    let plan = aterm_verify::NativeBuildPlan::capture(&env, root)
        .map_err(|e| format!("select dev build directory: {e}"))?;
    let target = plan.target();
    println!(
        "==> targo --unverified build {} -p aterm --bin aterm  [{}, build {}]",
        opts.profile.cargo_args().join(" "),
        stamp.commit,
        stamp.build
    );
    plan.prepare()
        .map_err(|e| format!("prepare dev build directory: {e}"))?;
    let mut command = build_command(
        &stage2,
        root,
        target,
        plan.build_dir(),
        &std::env::var_os("PATH").unwrap_or_default(),
        &stamp,
        opts.profile,
    );
    plan.apply_to_command(&mut command)
        .map_err(|e| format!("configure dev build environment: {e}"))?;
    let status = command
        .status()
        .map_err(|e| format!("start {}: {e}", targo.display()))?;
    if !status.success() {
        return Err(format!(
            "the build failed ({status}; its errors are above). If they are deny-by-default \
             lints of the Trust toolchain itself rather than errors in your edits, the store's \
             Trust predates their fix: aterm pkg update trust"
        ));
    }
    let bin = built_binary(target, opts.profile)?;
    let out = opts
        .out
        .clone()
        .unwrap_or_else(|| root.join("target/.ubd/dev-app.noindex"));
    let app = assemble(root, &out, &stamp, &bin, opts.pin)?;
    sign(&app, &opts.sign, &home, run)?;
    let dest = match &opts.install {
        Install::No => {
            println!("ship dev-app: assembled {} (not installed)", app.display());
            None
        }
        Install::Applications => Some((applications_dir(&home)?, true)),
        Install::Into(dir) => Some((dir.clone(), false)),
    };
    if let Some((dir, register)) = dest {
        let bin_dir = opts.link.then(|| home.join(".local/bin"));
        install(&app, &dir, register, bin_dir.as_deref(), run)?;
    }
    if opts.pin {
        println!("  marked {DEV_MARK_KEY} — the auto-updater will leave this build alone.");
    } else {
        println!("  NOT marked (--no-pin) — every update will fail on this build.");
    }
    println!("  to rejoin the release channel: tools/install.sh [--version X.Y.Z]");
    Ok(())
}

fn assemble(
    root: &Path,
    out: &Path,
    stamp: &Stamp,
    bin: &Path,
    pin: bool,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(out).map_err(|e| format!("create {}: {e}", out.display()))?;
    let app = out.join(DEV_APP_NAME);
    note_running(&app);
    if crate::bundle::exists_or_symlink(&app) {
        crate::bundle::remove_path_any(&app)
            .map_err(|e| format!("rm -rf {}: {e}", app.display()))?;
    }
    let spec = crate::bundle::BundleSpec {
        repo_root: root.to_path_buf(),
        out_dir: out.to_path_buf(),
        short_version: stamp.version.clone(),
        build_number: stamp.build,
        bundle_id: DEV_BUNDLE_ID.to_string(),
        git_commit: stamp.commit.clone(),
        aterm_bin: bin.to_path_buf(),
    };
    crate::bundle::assemble_at(&spec, &app)?;
    let plist_path = app.join("Contents/Info.plist");
    let plist = crate::durable::read_text(&plist_path)
        .map_err(|e| format!("read {}: {e}", plist_path.display()))?;
    let mut plist =
        crate::bundle::set_plist_string(&plist, "CFBundleDisplayName", DEV_DISPLAY_NAME)?;
    plist =
        crate::bundle::set_plist_string(&plist, DEV_BINARY_KEY, &crate::dmg::sha256_file(bin)?)?;
    if pin {
        plist = crate::bundle::set_plist_string(&plist, DEV_MARK_KEY, "true")?;
    }
    crate::durable::write_fresh(&plist_path, plist)
        .map_err(|e| format!("write {}: {e}", plist_path.display()))?;
    Ok(app)
}

fn note_running(app: &Path) {
    let prefix = format!("{}/", app.join("Contents/MacOS").display());
    let running = crate::bundle::running_processes()
        .unwrap_or_default()
        .into_iter()
        .any(|(_, exe)| exe.starts_with(&prefix));
    if running {
        eprintln!(
            "ship dev-app: NOTE — {} is running; the new build lands for its NEXT launch",
            app.display()
        );
    }
}

fn codesign(run: &dyn Run, args: &[&dyn AsRef<OsStr>], state: &str) -> Result<Output, String> {
    let mut cmd = aterm_uds::spawnfd::command(CODESIGN);
    cmd.args(args.iter().map(|a| a.as_ref()));
    match run.run(&mut cmd, WAIT) {
        Ok(out) if out.status.success() => Ok(out),
        Ok(out) => Err(format!(
            "codesign {} failed: {}",
            args.iter()
                .map(|a| a.as_ref().to_string_lossy())
                .collect::<Vec<_>>()
                .join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Err(RunError::TimedOut) => Err(format!(
            "macOS's code-signing service (trustd) did not answer within {} s; {state} and \
             nothing was installed — build again once it answers (a restart clears a wedged \
             trustd)",
            WAIT.as_secs()
        )),
        Err(e) => Err(format!("codesign {e}")),
    }
}

fn sign(app: &Path, with: &SignWith, home: &Path, run: &dyn Run) -> Result<(), String> {
    let identity = match with {
        SignWith::AdHoc => None,
        SignWith::Identity(id) => Some((id.clone(), id.clone())),
        SignWith::Default => {
            match dev_identity::arm(&dev_identity::default_keychain(home), true, run) {
                Ok(Some(hash)) => {
                    let label = format!("{} ({hash})", dev_identity::NAME);
                    Some((hash, label))
                }
                Ok(None) => None,
                Err(why) => {
                    return Err(format!(
                        "{why}\nthe \"{}\" keychain cannot be used now — build again once that \
                         is resolved, or sign ad hoc with --sign-id -",
                        dev_identity::NAME
                    ));
                }
            }
        }
    };
    let shown = app.display().to_string();
    let unsigned = format!("{shown} is assembled but not signed");
    let unverified = format!("{shown} is signed but not verified");
    match &identity {
        Some((id, label)) => {
            println!("==> codesign ({label}): runs locally, not distributable to other Macs");
            codesign(run, &[&"--force", &"--sign", &id, &app], &unsigned)?;
            let read = codesign(run, &[&"-d", &"-r-", &app], &unverified)?;
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&read.stdout),
                String::from_utf8_lossy(&read.stderr)
            );
            let requirement = crate::sign::designated_requirement(&text).unwrap_or("<none>");
            if crate::sign::classify_dr(&text) != crate::sign::DrClass::Identity {
                return Err(format!(
                    "signed with {id:?} but the designated requirement is not identity-based: \
                     {requirement}"
                ));
            }
            println!("  designated requirement: {requirement}");
        }
        None => {
            println!("==> codesign (ad-hoc): runs locally, not distributable to other Macs");
            codesign(run, &[&"--force", &"--sign", &"-", &app], &unsigned)?;
            if *with == SignWith::AdHoc {
                println!(
                    "ad-hoc signature (--sign-id -) — macOS access grants will not survive the \
                     next build of this bundle."
                );
            } else {
                println!(
                    "ad-hoc signature — macOS access grants will not survive the next build of \
                     this bundle (`ship dev-app identity`, once, fixes that)."
                );
            }
        }
    }
    codesign(run, &[&"--verify", &"--strict", &app], &unverified)?;
    Ok(())
}

fn applications_dir(home: &Path) -> Result<PathBuf, String> {
    let system = Path::new(APPLICATIONS);
    if atpkg::platform::dir_writable_by_caller(system) {
        return Ok(system.to_path_buf());
    }
    let user = home.join("Applications");
    std::fs::create_dir_all(&user).map_err(|e| format!("create {}: {e}", user.display()))?;
    Ok(user)
}

fn copy_tree(src: &Path, dst: &Path) -> Result<(), String> {
    let meta =
        std::fs::symlink_metadata(src).map_err(|e| format!("stat {}: {e}", src.display()))?;
    copy_tree_kind(src, dst, meta.file_type(), Some(&meta))
}

fn copy_tree_kind(
    src: &Path,
    dst: &Path,
    kind: std::fs::FileType,
    dir_meta: Option<&std::fs::Metadata>,
) -> Result<(), String> {
    if kind.is_symlink() {
        let target =
            std::fs::read_link(src).map_err(|e| format!("readlink {}: {e}", src.display()))?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, dst)
            .map_err(|e| format!("symlink {}: {e}", dst.display()))?;
        #[cfg(not(unix))]
        let _ = target;
        return Ok(());
    }
    if kind.is_dir() {
        let owned_meta;
        let meta = match dir_meta {
            Some(meta) => meta,
            None => {
                owned_meta = std::fs::symlink_metadata(src)
                    .map_err(|e| format!("stat {}: {e}", src.display()))?;
                &owned_meta
            }
        };
        std::fs::create_dir(dst).map_err(|e| format!("create {}: {e}", dst.display()))?;
        let entries = std::fs::read_dir(src).map_err(|e| format!("read {}: {e}", src.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("read {}: {e}", src.display()))?;
            let ft = entry
                .file_type()
                .map_err(|e| format!("stat {}: {e}", src.display()))?;
            let name = entry.file_name();
            copy_tree_kind(&src.join(&name), &dst.join(&name), ft, None)?;
        }
        return atpkg::platform::set_mode(dst, atpkg::platform::permission_mode(meta) & 0o7777)
            .map_err(|e| format!("chmod {}: {e}", dst.display()));
    }
    if !kind.is_file() {
        return Err(format!(
            "{} is not a regular file, directory, or symlink",
            src.display()
        ));
    }
    let (mut source, src_meta) = crate::durable::open_regular_file_with_meta(src)
        .map_err(|e| format!("open {}: {e}", src.display()))?;
    let mut sink = crate::durable::open_create_new(dst)
        .map_err(|e| format!("create {}: {e}", dst.display()))?;
    std::io::copy(&mut source, &mut sink)
        .map_err(|e| format!("copy {} -> {}: {e}", src.display(), dst.display()))?;
    let perms = src_meta.permissions();
    #[cfg(unix)]
    if sink.metadata().is_ok_and(|m| {
        use std::os::unix::fs::PermissionsExt as _;
        m.permissions().mode() & 0o7777 == perms.mode() & 0o7777
    }) {
        return Ok(());
    }
    sink.set_permissions(perms)
        .map_err(|e| format!("chmod {}: {e}", dst.display()))
}

/// Install `app` into `dir` as [`DEV_APP_NAME`], replacing a previous dev install: a
/// verified copy is moved into place, so a failed copy leaves the old one. A previous
/// install whose first run never showed it ran stays until its hold is spent. One install
/// into `dir` at a time: from its hold check to its own first run's stamp, no other install
/// there removes an install or a stamp it has not seen.
fn install(
    app: &Path,
    dir: &Path,
    register: bool,
    bin_dir: Option<&Path>,
    run: &dyn Run,
) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let me = aterm_toolchain::named_lock::Holder::me(
        "ship dev-app",
        &std::env::current_dir().unwrap_or_default(),
    );
    let _installing = crate::trust_lane::RunLock::take(
        &dir.join(INSTALL_LOCK),
        &me,
        "another ship dev-app is installing here; install again once it ends",
    )?;
    let installed = dir.join(DEV_APP_NAME);
    let marks = dir.join(EXEC_MARKS);
    if crate::bundle::exists_or_symlink(&installed)
        && let Some(left) = crate::gates::first_exec_hold(&marks)
    {
        return Err(format!(
            "{} was started and never showed it ran, so macOS may still be assessing its \
             first exec; it stays as it is — install again in {} s (or after a restart)",
            installed.display(),
            left.as_secs()
        ));
    }
    let staging = dir.join(format!(".aterm-dev-app.{}.noindex", std::process::id()));
    if crate::bundle::exists_or_symlink(&staging) {
        crate::bundle::remove_path_any(&staging)
            .map_err(|e| format!("rm -rf {}: {e}", staging.display()))?;
    }
    std::fs::create_dir(&staging).map_err(|e| format!("create {}: {e}", staging.display()))?;
    let copy = staging.join(DEV_APP_NAME);
    let placed = copy_tree(app, &copy)
        .and_then(|()| {
            codesign(
                run,
                &[&"--verify", &"--strict", &copy],
                &format!("{} is copied but not verified", copy.display()),
            )
        })
        .and_then(|_| {
            note_running(&installed);
            if crate::bundle::exists_or_symlink(&installed) {
                crate::bundle::remove_path_any(&installed)
                    .map_err(|e| format!("rm -rf {}: {e}", installed.display()))?;
            }
            std::fs::rename(&copy, &installed)
                .map_err(|e| format!("move {} to {}: {e}", copy.display(), installed.display()))
        });
    let _ = crate::bundle::remove_path_any(&staging);
    placed?;
    let exe = installed.join("Contents/MacOS/aterm");
    if let Some(bin_dir) = bin_dir {
        link_cli(&exe, bin_dir, run)?;
    }
    if register {
        let mut cmd = aterm_uds::spawnfd::command(LSREGISTER);
        cmd.arg("-f").arg(&installed);
        let _ = run.run(&mut cmd, WAIT);
    }
    let version = first_run(run, &exe, &marks).unwrap_or_else(|| "aterm (version unknown)".into());
    println!(
        "ship dev-app: installed {version} -> {}",
        installed.display()
    );
    match bin_dir {
        Some(bin_dir) => println!(
            "  CLI path: {} (aterm pkg shell-setup --path adds it in new shells)",
            bin_dir.join("aterm").display()
        ),
        None => println!(
            "  CLI path: unchanged (--no-link); run it at {}",
            exe.display()
        ),
    }
    let release_plist = dir.join("aterm.app/Contents/Info.plist");
    if crate::durable::read_text(&release_plist).is_ok_and(|text| text.contains(DEV_MARK_KEY)) {
        eprintln!(
            "ship dev-app: {} is an old dev build on the release path — tools/install.sh puts a \
             release back",
            dir.join("aterm.app").display()
        );
    }
    Ok(())
}

fn link_cli(exe: &Path, bin_dir: &Path, run: &dyn Run) -> Result<Option<PathBuf>, String> {
    std::fs::create_dir_all(bin_dir).map_err(|e| format!("create {}: {e}", bin_dir.display()))?;
    let link = bin_dir.join("aterm");
    let displaced = point_link(exe, &link)?;
    if let Some(prior) = &displaced {
        let version =
            first_line(run, prior, &["--version"]).unwrap_or_else(|| "version unknown".into());
        eprintln!(
            "ship dev-app: {} now runs this dev build, which never updates (was: {}, {version})",
            link.display(),
            prior.display()
        );
        eprintln!(
            "ship dev-app:   put it back: ln -sfn '{}' '{}' (to skip this next time: --no-link)",
            prior.display(),
            link.display()
        );
    }
    // The bundle's other command links follow `aterm`, as atpkg's pass makes them follow it
    // (`hooks::link_act`): a tab finds `~/.local/bin` ahead of its bundle, so a link left on
    // the release would run the release's code in a dev tab.
    for (name, what) in [("amacs", "editor"), ("agh", "GitHub client")] {
        let alias = exe.with_file_name(name);
        if std::fs::symlink_metadata(&alias).is_err() {
            continue;
        }
        let link = bin_dir.join(name);
        if let Some(prior) = point_link(&alias, &link)? {
            eprintln!(
                "ship dev-app: {} now runs this dev build's {what} (was: {})",
                link.display(),
                prior.display()
            );
        }
    }
    Ok(displaced)
}

fn point_link(target: &Path, link: &Path) -> Result<Option<PathBuf>, String> {
    let dir = link.parent().unwrap_or(Path::new("."));
    let prior = match std::fs::symlink_metadata(link) {
        Ok(meta) if !meta.file_type().is_symlink() => {
            eprintln!(
                "ship dev-app: {} is not a link, so it is left as it is (--no-link skips this)",
                link.display()
            );
            return Ok(None);
        }
        Ok(_) => std::fs::read_link(link).ok().map(|old| dir.join(old)),
        Err(_) => None,
    };
    let displaced = prior.filter(|p| p != target && p.is_file());
    let name = link.file_name().unwrap_or_default().to_string_lossy();
    let tmp = dir.join(format!(".{name}.dev-app.{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, &tmp)
        .map_err(|e| format!("symlink {}: {e}", tmp.display()))?;
    if let Err(e) = std::fs::rename(&tmp, link) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("move {} to {}: {e}", tmp.display(), link.display()));
    }
    Ok(displaced)
}

#[cfg(test)]
#[path = "dev_app_tests.rs"]
mod tests;
