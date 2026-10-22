// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `ship dev-app`: build THIS checkout, dirty or not, into `aterm (dev).app`, sign it
//! locally and install it BESIDE the release — the code in front of you running as the
//! app you launch. The release's own build cannot do this job: it builds only a
//! published commit, from a clean tree, under a claimed build number.
//!
//! What it makes, and how it differs from a release:
//! * the release's layout ([`crate::bundle::assemble_at`], [`crate::bundle::ARGV0_ALIASES`])
//!   around this Mac's slice alone, built with the Trust toolchain the cutter resolves
//!   ([`crate::gates::trust_stage2_bin`]) on targo's `--unverified` lane;
//! * its OWN code identity — [`DEV_BUNDLE_ID`], [`DEV_DISPLAY_NAME`], installed as
//!   [`DEV_APP_NAME`]: macOS keys a privacy grant to a bundle id plus its designated
//!   requirement, so a dev build sharing the release's id would stale the release's
//!   grants, and two ids at one path leave LaunchServices holding a stale record;
//! * CFBundleVersion = HEAD's committer epoch and ATermGitCommit = its short id (with
//!   `-dirty`), read once and pinned into the build ([`Stamp::pins`]), so plist and
//!   binary agree;
//! * signed with the "aterm dev" identity when `ship dev-app identity` made it (grants
//!   then survive a rebuild), else ad hoc — never distributable, never notarized;
//! * marked [`DEV_MARK_KEY`] (unless `--no-pin`), which the updater leaves alone: a dev
//!   build of an older commit sits below the channel head, so without the mark the
//!   updater tries it, and every update fails on it.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Output;

use aterm_uds::deadline::RunError;
use aterm_uds::spawnfd::Command;

use crate::dev_identity::{self, CODESIGN, Run, WAIT};

/// The dev build's `CFBundleIdentifier`: never the release's.
pub const DEV_BUNDLE_ID: &str = "com.aterm.aterm.dev";

/// The dev build's `CFBundleDisplayName`.
pub const DEV_DISPLAY_NAME: &str = "aterm (dev)";

/// The dev build's bundle name on disk, in `--out` and where it is installed: never
/// `aterm.app`, which is the release's.
pub const DEV_APP_NAME: &str = "aterm (dev).app";

/// The updater's dev mark (`aterm_update::bundle::DEV_BUILD_KEY`): a bundle whose sealed
/// Info.plist says `true` here is never replaced by an update.
pub const DEV_MARK_KEY: &str = "ATermDevBuild";

const LSREGISTER: &str = "/System/Library/Frameworks/CoreServices.framework/Frameworks/\
                          LaunchServices.framework/Support/lsregister";

/// The release's default home, and the user's when that is not writable.
const APPLICATIONS: &str = "/Applications";

pub const USAGE: &str = "
  targo --unverified ship dev-app [--sign-id IDENTITY] [--out DIRECTORY]
                 [--no-install | --install-dir DIRECTORY] [--no-link] [--no-pin]
      Build this checkout, dirty or not, into `aterm (dev).app` with the Trust
      toolchain the cutter resolves (`targo --unverified build --release -p
      aterm`): its own bundle id (com.aterm.aterm.dev), this Mac's slice only,
      HEAD's committer epoch as its build number. It is signed locally and
      installed beside the release; nothing is claimed, notarized or published.
        --sign-id IDENTITY  sign with this codesign identity; `-` signs ad hoc.
                            Default: the \"aterm dev\" identity once `ship dev-app
                            identity` made it (macOS privacy grants then survive a
                            rebuild), else ad hoc; refuses, saying why, when its
                            keychain exists but cannot be used
        --out DIRECTORY     assemble there (default: dist/)
        --no-install        assemble only
        --install-dir DIRECTORY
                            install there instead of /Applications (or
                            ~/Applications when /Applications is not writable);
                            only an install in those is told to LaunchServices
        --no-link           leave ~/.local/bin/aterm as it is; by default a link
                            there is pointed at this build, which never updates,
                            and what it displaced is named (a file that is not a
                            link is left alone)
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
                            so the signing self-check is skipped
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
    /// Where the bundle is assembled; `None` is the checkout's `dist/`.
    pub out: Option<PathBuf>,
    pub install: Install,
    /// The [`DEV_MARK_KEY`] mark (off with `--no-pin`).
    pub pin: bool,
    /// Point `~/.local/bin/aterm` and `amacs` at the installed build (off with `--no-link`).
    pub link: bool,
    pub sign: SignWith,
}

/// Where the assembled bundle is installed.
#[derive(Debug, PartialEq, Eq)]
pub enum Install {
    /// `/Applications`, or `~/Applications` when that is not writable; LaunchServices
    /// is told of it.
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
///
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
        out: None,
        install: Install::Applications,
        pin: true,
        link: true,
        sign: SignWith::Default,
    };
    let (mut no_install, mut install_dir, mut sign) = (false, None, None);
    // A value is never a flag; `-` is one only for --sign-id (ad hoc).
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
    opts.sign = match sign.as_deref() {
        None => SignWith::Default,
        Some("-") => SignWith::AdHoc,
        Some(id) => SignWith::Identity(id.to_string()),
    };
    Ok(Cmd::Build(opts))
}

/// `ship dev-app identity [--remove] [--keychain FILE]`.
///
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

/// The keychain `identity` acts on, and whether it belongs on the search list (only the
/// default one does). A relative `--keychain` is made absolute against `cwd`: `security`
/// would read it against ~/Library/Keychains while every check here reads it against
/// `cwd`, so the two would be different files.
fn identity_keychain(keychain: Option<&Path>, home: &Path, cwd: &Path) -> (PathBuf, bool) {
    match keychain {
        Some(path) => (cwd.join(path), false),
        None => (dev_identity::default_keychain(home), true),
    }
}

/// The build facts, read from the checkout ONCE, exactly as `crates/aterm/build.rs`
/// would read them: the plist is stamped with them and the build is pinned to them
/// ([`Stamp::pins`]), so plist and binary agree even when HEAD moves mid-build.
#[derive(Debug, PartialEq, Eq)]
struct Stamp {
    version: String,
    /// HEAD's committer epoch.
    build: u64,
    /// HEAD's 12-digit id, `-dirty` when the tree has changes.
    commit: String,
    commit_full: String,
    /// Commits since the newest `v*.*.0` tag; `0` without one.
    dev_commits: u64,
}

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = aterm_uds::spawnfd::command("git")
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
        let text = std::fs::read_to_string(&manifest)
            .map_err(|e| format!("read {}: {e}", manifest.display()))?;
        let version = crate::publish::workspace_version(&text).map_err(|e| e.to_string())?;
        let epoch = git(root, &["show", "-s", "--format=%ct", "HEAD"])?;
        let build = epoch
            .trim()
            .parse()
            .map_err(|_| format!("HEAD's committer epoch {epoch:?} is not a number"))?;
        let mut commit = git(root, &["rev-parse", "--short=12", "HEAD"])?;
        if !git(root, &["status", "--porcelain"])?.is_empty() {
            commit.push_str("-dirty");
        }
        let commit_full = git(root, &["rev-parse", "HEAD"])?;
        let dev_commits = git(
            root,
            &["describe", "--tags", "--match", "v*.*.0", "--abbrev=0"],
        )
        .and_then(|tag| git(root, &["rev-list", &format!("{tag}..HEAD"), "--count"]))
        .ok()
        .and_then(|count| count.trim().parse().ok())
        .unwrap_or(0);
        Ok(Self {
            version,
            build,
            commit,
            commit_full,
            dev_commits,
        })
    }

    /// The build script's pins (`crates/aterm/build.rs` THE PIN, and the build number
    /// through SOURCE_DATE_EPOCH, as the cutter pins them): given these it asks git
    /// nothing. The binary's build time is then HEAD's commit time.
    fn pins(&self) -> [(&'static str, String); 4] {
        [
            ("SOURCE_DATE_EPOCH", self.build.to_string()),
            ("ATERM_BUILD_GIT_COMMIT", self.commit.clone()),
            ("ATERM_BUILD_GIT_COMMIT_FULL", self.commit_full.clone()),
            ("ATERM_BUILD_DEV_COMMITS", self.dev_commits.to_string()),
        ]
    }
}

/// `--target-dir`: the caller's `CARGO_TARGET_DIR` (relative to the checkout) or the
/// checkout's `target/`. Named on the line, so no config can move the binary elsewhere.
fn target_dir(root: &Path, env: Option<OsString>) -> PathBuf {
    match env.filter(|v| !v.is_empty()).map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => dir,
        Some(dir) => root.join(dir),
        None => root.join("target"),
    }
}

/// The one build: `<stage2>/targo --unverified build --release -p aterm`, from `root`,
/// with the Trust bin dir first on PATH so targo's children resolve trustc beside it,
/// pinned to `stamp`. What would swap the proved toolchain (RUSTC, CARGO_BUILD_RUSTC,
/// RUSTUP_TOOLCHAIN), move the binary out of `<target>/release` (CARGO_BUILD_TARGET)
/// or mark it a release is scrubbed; RUSTFLAGS is not — steering a dev build is the
/// caller's right. Led by `build_dir` (`--config build.build-dir=<target>/build.noindex`,
/// `aterm_verify::fallback_build_dir`) when there is one, so the build's intermediates
/// stay out of Spotlight while the binary still lands in `<target>/release/aterm`.
fn build_command(
    stage2: &Path,
    root: &Path,
    target: &Path,
    build_dir: Option<&str>,
    path: &OsStr,
    stamp: &Stamp,
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
        .args([
            "--unverified",
            "build",
            "--release",
            "-p",
            "aterm",
            "--target-dir",
        ])
        .arg(target)
        .env("PATH", joined);
    for key in [
        "RUSTC",
        "CARGO_BUILD_RUSTC",
        "RUSTUP_TOOLCHAIN",
        "CARGO_BUILD_TARGET",
        crate::buildplan::RELEASE_BUILD_MARKER,
    ] {
        cmd.env_remove(key);
    }
    for (key, value) in stamp.pins() {
        cmd.env(key, value);
    }
    cmd
}

/// The first line `program args` prints, under the hang detector.
fn first_line(run: &dyn Run, program: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = aterm_uds::spawnfd::command(program);
    cmd.args(args);
    let out = run.run(&mut cmd, WAIT).ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().next().map(str::to_string)
}

/// `ship dev-app [...]`: build, assemble, sign and (unless `--no-install`) install.
///
/// # Errors
/// The first step that could not be done, said with its remedy; nothing is installed
/// after a failed step.
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
    let target = target_dir(root, std::env::var_os("CARGO_TARGET_DIR"));
    println!(
        "==> targo --unverified build --release -p aterm  [{}, build {}]",
        stamp.commit, stamp.build
    );
    let build_dir =
        aterm_verify::fallback_build_dir(&aterm_verify::EnvSnapshot::capture(), root, &target);
    let status = build_command(
        &stage2,
        root,
        &target,
        build_dir.as_deref(),
        &std::env::var_os("PATH").unwrap_or_default(),
        &stamp,
    )
    .status()
    .map_err(|e| format!("start {}: {e}", targo.display()))?;
    if !status.success() {
        return Err(format!(
            "the build failed ({status}; its errors are above). If they are deny-by-default \
             lints of the Trust toolchain itself rather than errors in your edits, the store's \
             Trust predates their fix: aterm pkg update trust"
        ));
    }
    let bin = target.join("release/aterm");
    if !bin.is_file() {
        return Err(format!(
            "no {} after the build — a target setting redirected it",
            bin.display()
        ));
    }
    let out = opts.out.clone().unwrap_or_else(|| root.join("dist"));
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

/// The release's layout of `bin` at `<out>/aterm (dev).app` under the dev identity, the
/// [`DEV_MARK_KEY`] mark sealed in when `pin`. A previous dev bundle there is replaced.
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
    if app.exists() {
        std::fs::remove_dir_all(&app).map_err(|e| format!("rm -rf {}: {e}", app.display()))?;
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
    let plist = std::fs::read_to_string(&plist_path)
        .map_err(|e| format!("read {}: {e}", plist_path.display()))?;
    let mut plist =
        crate::bundle::set_plist_string(&plist, "CFBundleDisplayName", DEV_DISPLAY_NAME)?;
    if pin {
        plist = crate::bundle::set_plist_string(&plist, DEV_MARK_KEY, "true")?;
    }
    std::fs::write(&plist_path, plist)
        .map_err(|e| format!("write {}: {e}", plist_path.display()))?;
    Ok(app)
}

/// Say so when a process runs out of `app`: replacing it lands for that process's NEXT
/// launch.
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

/// `codesign args…` under the hang detector, which must succeed; `state` says what a
/// service that did not answer leaves (nothing is installed after it).
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

/// Sign `app` as `with` says, then verify it `--strict`. An identity's signature must
/// yield an identity-based designated requirement — the property it is signed for.
/// No `--options runtime`: the hardened runtime is a notarization rule, and a dev build
/// should behave as the code being debugged.
fn sign(app: &Path, with: &SignWith, home: &Path, run: &dyn Run) -> Result<(), String> {
    // (identity to sign with, how it is named)
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
                // Never a silent fall-back to ad hoc: that orphans the grants the
                // identity exists to keep.
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

/// `/Applications` when this user may write it (access(2), so nothing is written there
/// to find out), else `~/Applications` (made).
fn applications_dir(home: &Path) -> Result<PathBuf, String> {
    let system = Path::new(APPLICATIONS);
    if atpkg::platform::dir_writable_by_caller(system) {
        return Ok(system.to_path_buf());
    }
    let user = home.join("Applications");
    std::fs::create_dir_all(&user).map_err(|e| format!("create {}: {e}", user.display()))?;
    Ok(user)
}

/// Copy the tree at `src` to `dst` (absent): directories, files with their mode, and
/// symlinks as links — what a sealed bundle is made of.
fn copy_tree(src: &Path, dst: &Path) -> Result<(), String> {
    let meta =
        std::fs::symlink_metadata(src).map_err(|e| format!("stat {}: {e}", src.display()))?;
    if meta.file_type().is_symlink() {
        let target =
            std::fs::read_link(src).map_err(|e| format!("readlink {}: {e}", src.display()))?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, dst)
            .map_err(|e| format!("symlink {}: {e}", dst.display()))?;
        #[cfg(not(unix))]
        let _ = target;
        return Ok(());
    }
    if meta.is_dir() {
        std::fs::create_dir(dst).map_err(|e| format!("create {}: {e}", dst.display()))?;
        let entries = std::fs::read_dir(src).map_err(|e| format!("read {}: {e}", src.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("read {}: {e}", src.display()))?;
            copy_tree(&entry.path(), &dst.join(entry.file_name()))?;
        }
        return std::fs::set_permissions(dst, meta.permissions())
            .map_err(|e| format!("chmod {}: {e}", dst.display()));
    }
    std::fs::copy(src, dst)
        .map_err(|e| format!("copy {} -> {}: {e}", src.display(), dst.display()))?;
    Ok(())
}

/// Install `app` into `dir` as [`DEV_APP_NAME`], replacing a previous dev install: a
/// verified copy is moved into place, so a failed copy leaves the old one. Then point
/// `<bin_dir>/aterm` at it (when `bin_dir`), tell LaunchServices (when `register`: its
/// record of a path whose bundle changed is otherwise stale), and run it once with
/// `--version`.
fn install(
    app: &Path,
    dir: &Path,
    register: bool,
    bin_dir: Option<&Path>,
    run: &dyn Run,
) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let installed = dir.join(DEV_APP_NAME);
    // The copy is made in a hidden directory of this run's own beside the install, so
    // the move into place is one rename on one volume, two runs never share one, and
    // (`.noindex`) Spotlight and LaunchServices never offer the half-placed copy.
    let staging = dir.join(format!(".aterm-dev-app.{}.noindex", std::process::id()));
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
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
            if installed.exists() {
                std::fs::remove_dir_all(&installed)
                    .map_err(|e| format!("rm -rf {}: {e}", installed.display()))?;
            }
            std::fs::rename(&copy, &installed)
                .map_err(|e| format!("move {} to {}: {e}", copy.display(), installed.display()))
        });
    let _ = std::fs::remove_dir_all(&staging);
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
    let version =
        first_line(run, &exe, &["--version"]).unwrap_or_else(|| "aterm (version unknown)".into());
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
    // A dev build from before the split sits on the release path under the release's
    // id. Said, never deleted: from out here it differs from a release only by the mark.
    let release_plist = dir.join("aterm.app/Contents/Info.plist");
    if std::fs::read_to_string(&release_plist).is_ok_and(|text| text.contains(DEV_MARK_KEY)) {
        eprintln!(
            "ship dev-app: {} is an old dev build on the release path — tools/install.sh puts a \
             release back",
            dir.join("aterm.app").display()
        );
    }
    Ok(())
}

/// Point `<bin_dir>/aterm` at `exe`, and `<bin_dir>/amacs` at the `amacs` beside it,
/// replacing a link (atomically) and never a file. The `aterm` on PATH is a daily driver
/// and this build never updates, so a link to anything else is named, with the one
/// command that puts it back. Returns the `aterm` target it displaced.
///
/// `amacs` (aterm's editor, `docs/DESIGN-amacs-2026-10-19.md` §8.1) goes with `aterm`: a
/// tab finds `~/.local/bin` ahead of its window's bundle, and the editor client hands its
/// file to the window that runs the tab, so a dev window's `amacs` must be this build's,
/// not the release's. The release's hooks pass leaves the pair aimed here and brings
/// `amacs` back once `aterm` runs the release again (atpkg's `ensure_command_links_from`).
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
    let editor = exe.with_file_name("amacs");
    if std::fs::symlink_metadata(&editor).is_ok() {
        let link = bin_dir.join("amacs");
        // Named, not run: the editor takes no `--version` (it answers a usage error).
        if let Some(prior) = point_link(&editor, &link)? {
            eprintln!(
                "ship dev-app: {} now runs this dev build's editor (was: {})",
                link.display(),
                prior.display()
            );
        }
    }
    Ok(displaced)
}

/// Point the link `link` at `target`, atomically, unless something other than a link
/// holds the name (said, and left). Returns the file the link ran before, when that was
/// another one.
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
    std::fs::rename(&tmp, link)
        .map_err(|e| format!("move {} to {}: {e}", tmp.display(), link.display()))?;
    Ok(displaced)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dev_identity::fake::{Fake, Kc, World};

    fn scratch() -> (aterm_tempfile::TempDir, PathBuf) {
        let dir = aterm_tempfile::tempdir().expect("scratch dir");
        let path = std::fs::canonicalize(dir.path()).expect("canonical scratch");
        (dir, path)
    }

    fn parse_args(args: &[&str]) -> Result<Cmd, String> {
        parse(&mut args.iter().copied())
    }

    fn build(args: &[&str]) -> Options {
        match parse_args(args) {
            Ok(Cmd::Build(opts)) => opts,
            other => panic!("{args:?}: {other:?}"),
        }
    }

    /// The program and arguments of every logged call, joined.
    fn log(fake: &Fake) -> Vec<String> {
        fake.0.borrow().log.iter().map(|c| c.join(" ")).collect()
    }

    #[test]
    fn the_flags_parse_to_what_they_say() {
        assert_eq!(
            build(&[]),
            Options {
                out: None,
                install: Install::Applications,
                pin: true,
                link: true,
                sign: SignWith::Default,
            }
        );
        assert_eq!(
            build(&[
                "--sign-id",
                "-",
                "--out",
                "o",
                "--install-dir",
                "d",
                "--no-link",
                "--no-pin",
            ]),
            Options {
                out: Some(PathBuf::from("o")),
                install: Install::Into(PathBuf::from("d")),
                pin: false,
                link: false,
                sign: SignWith::AdHoc,
            }
        );
        assert_eq!(build(&["--no-install"]).install, Install::No);
        assert_eq!(
            build(&["--sign-id", "Apple Development: Jane"]).sign,
            SignWith::Identity("Apple Development: Jane".into())
        );
        assert_eq!(parse_args(&["--help"]), Ok(Cmd::Help));
        assert_eq!(
            parse_args(&["identity"]),
            Ok(Cmd::Identity {
                remove: false,
                keychain: None
            })
        );
        assert_eq!(
            parse_args(&["identity", "--remove", "--keychain", "k.keychain-db"]),
            Ok(Cmd::Identity {
                remove: true,
                keychain: Some(PathBuf::from("k.keychain-db"))
            })
        );
        for refused in [
            &["--bogus"][..],
            &["--no-install", "--install-dir", "d"],
            &["--no-link", "--no-link"],
            &["--out"],
            &["--out", "--no-pin"],
            &["--install-dir", "-"],
            &["--sign-id", "a", "--sign-id", "b"],
            &["identity", "--bogus"],
            &["identity", "--keychain"],
        ] {
            assert!(parse_args(refused).is_err(), "{refused:?}");
        }
        // Through the cutter's own parser: `ship dev-app --no-link` is a dev-app.
        let args: Vec<String> = ["dev-app", "--no-link"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(matches!(
            crate::cli::parse(&args),
            Ok(crate::cli::Cmd::DevApp(Cmd::Build(Options {
                link: false,
                ..
            })))
        ));
    }

    fn stamp() -> Stamp {
        Stamp {
            version: "0.104.0".into(),
            build: 1_790_000_000,
            commit: "0123456789ab-dirty".into(),
            commit_full: "0123456789abcdef0123456789abcdef01234567".into(),
            dev_commits: 3,
        }
    }

    /// The one build runs the Trust targo on its explicit `--unverified` lane from the
    /// checkout, its bin dir first on PATH, pinned to the plist's stamp, with what would
    /// swap the toolchain, move the binary or mark it a release scrubbed — and
    /// RUSTFLAGS left to the caller.
    #[test]
    fn the_build_is_the_trust_targo_on_its_unverified_lane() {
        let stage2 = Path::new("/store/trust/9192/bin");
        let led = build_command(
            stage2,
            Path::new("/src/aterm"),
            Path::new("/src/aterm/target"),
            Some("build.build-dir='/src/aterm/target/build.noindex'"),
            OsStr::new("/usr/bin:/bin"),
            &stamp(),
        );
        let led: Vec<&OsStr> = led.get_args().collect();
        assert_eq!(
            led[..3],
            [
                "--config",
                "build.build-dir='/src/aterm/target/build.noindex'",
                "--unverified"
            ]
            .map(OsStr::new),
            "the intermediates go where Spotlight never looks"
        );
        let cmd = build_command(
            stage2,
            Path::new("/src/aterm"),
            Path::new("/src/aterm/target"),
            None,
            OsStr::new("/usr/bin:/bin"),
            &stamp(),
        );
        assert_eq!(cmd.get_program(), stage2.join("targo").as_os_str());
        let args: Vec<&OsStr> = cmd.get_args().collect();
        assert_eq!(
            args,
            [
                "--unverified",
                "build",
                "--release",
                "-p",
                "aterm",
                "--target-dir",
                "/src/aterm/target"
            ]
            .map(OsStr::new)
        );
        assert_eq!(cmd.get_current_dir(), Some(Path::new("/src/aterm")));
        let envs: std::collections::BTreeMap<String, Option<String>> = cmd
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert_eq!(
            envs["PATH"].as_deref(),
            Some("/store/trust/9192/bin:/usr/bin:/bin")
        );
        for scrubbed in [
            "RUSTC",
            "CARGO_BUILD_RUSTC",
            "RUSTUP_TOOLCHAIN",
            "CARGO_BUILD_TARGET",
            crate::buildplan::RELEASE_BUILD_MARKER,
        ] {
            assert_eq!(envs.get(scrubbed), Some(&None), "{scrubbed}");
        }
        for (key, value) in [
            ("SOURCE_DATE_EPOCH", "1790000000"),
            ("ATERM_BUILD_GIT_COMMIT", "0123456789ab-dirty"),
            (
                "ATERM_BUILD_GIT_COMMIT_FULL",
                "0123456789abcdef0123456789abcdef01234567",
            ),
            ("ATERM_BUILD_DEV_COMMITS", "3"),
        ] {
            assert_eq!(envs.get(key), Some(&Some(value.to_string())), "{key}");
        }
        assert!(!envs.contains_key("RUSTFLAGS"));
    }

    /// The stamp is read as `crates/aterm/build.rs` reads it: HEAD's committer epoch,
    /// its 12-digit id with `-dirty` once the tree changes, and the commits since the
    /// newest `v*.*.0` tag (0 without one). A scratch repository, never this checkout.
    #[test]
    fn the_stamp_is_heads_facts_as_the_build_script_reads_them() {
        let (_dir, root) = scratch();
        let git = |args: &[&str]| {
            let out = crate::ledger::testing::git()
                .current_dir(&root)
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "tag.gpgsign=false",
                    "-c",
                    "core.hooksPath=/dev/null",
                ])
                .args(args)
                .env("GIT_COMMITTER_DATE", "1790000000 +0000")
                .output()
                .expect("git");
            assert!(out.status.success(), "git {args:?}: {out:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        git(&["init", "-q"]);
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace.package]\nversion = \"0.104.0\"\n",
        )
        .unwrap();
        git(&["add", "Cargo.toml"]);
        git(&["commit", "-q", "-m", "one"]);
        let first = Stamp::of(&root).expect("stamp");
        assert_eq!(first.dev_commits, 0, "no tag");
        git(&["tag", "v0.104.0"]);
        git(&["commit", "-q", "--allow-empty", "-m", "two"]);
        git(&["commit", "-q", "--allow-empty", "-m", "three"]);
        let full = git(&["rev-parse", "HEAD"]);
        assert_eq!(
            Stamp::of(&root).expect("stamp"),
            Stamp {
                version: "0.104.0".into(),
                build: 1_790_000_000,
                commit: full[..12].to_string(),
                commit_full: full.clone(),
                dev_commits: 2,
            }
        );
        std::fs::write(root.join("Cargo.toml"), "edited").unwrap();
        assert!(Stamp::of(&root).is_err(), "no workspace version");
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace.package]\nversion = \"0.104.0\"\n\n",
        )
        .unwrap();
        assert_eq!(
            Stamp::of(&root).expect("stamp").commit,
            format!("{}-dirty", &full[..12])
        );
    }

    /// `identity --keychain` names a file against the current directory, as every check
    /// here reads it, never against ~/Library/Keychains as `security` would; only the
    /// default keychain goes on the search list.
    #[test]
    fn a_relative_keychain_is_the_current_directorys() {
        let (home, cwd) = (Path::new("/Users//u"), Path::new("/work"));
        assert_eq!(
            identity_keychain(Some(Path::new("k.keychain-db")), home, cwd),
            (PathBuf::from("/work/k.keychain-db"), false)
        );
        assert_eq!(
            identity_keychain(Some(Path::new("/abs/k.keychain-db")), home, cwd),
            (PathBuf::from("/abs/k.keychain-db"), false)
        );
        assert_eq!(
            identity_keychain(None, home, cwd),
            (dev_identity::default_keychain(home), true)
        );
    }

    #[test]
    fn the_target_dir_is_the_callers_or_the_checkouts() {
        let root = Path::new("/src/aterm");
        assert_eq!(target_dir(root, None), root.join("target"));
        assert_eq!(target_dir(root, Some("".into())), root.join("target"));
        assert_eq!(target_dir(root, Some("t2".into())), root.join("t2"));
        assert_eq!(target_dir(root, Some("/abs".into())), PathBuf::from("/abs"));
    }

    /// The dev bundle is the release's layout around this checkout's binary under its
    /// own identity, and the updater reads the mark it seals as a dev build — absent
    /// under --no-pin. A second assembly replaces the first.
    #[test]
    fn the_dev_bundle_is_the_release_layout_under_its_own_identity() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let (_dir, out) = scratch();
        let bin = out.join("aterm-binary");
        std::fs::write(&bin, b"\xcf\xfa\xed\xfe not really a Mach-O").unwrap();
        let stamp = stamp();
        for pin in [true, false] {
            let app = assemble(&root, &out, &stamp, &bin, pin).expect("assemble");
            assert_eq!(app, out.join(DEV_APP_NAME));
            let macos = app.join("Contents/MacOS");
            assert!(macos.join("aterm").is_file());
            for alias in crate::bundle::ARGV0_ALIASES {
                assert_eq!(
                    std::fs::read_link(macos.join(alias)).ok(),
                    Some(PathBuf::from("aterm")),
                    "{alias}"
                );
            }
            let plist = std::fs::read_to_string(app.join("Contents/Info.plist")).unwrap();
            for (key, value) in [
                ("CFBundleIdentifier", DEV_BUNDLE_ID),
                ("CFBundleDisplayName", DEV_DISPLAY_NAME),
                ("CFBundleVersion", "1790000000"),
                ("CFBundleShortVersionString", "0.104.0"),
                ("ATermGitCommit", "0123456789ab-dirty"),
            ] {
                assert!(
                    plist.contains(&format!("<key>{key}</key>\n\t<string>{value}</string>")),
                    "{key}: {plist}"
                );
            }
            #[cfg(target_os = "macos")]
            assert_eq!(aterm_update::plist_marks_dev_build(&plist), pin);
        }
    }

    /// A bundle laid out for the sign and install steps: one executable, one alias link.
    fn bundle_at(dir: &Path) -> PathBuf {
        let app = dir.join(DEV_APP_NAME);
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::write(app.join("Contents/MacOS/aterm"), b"exe").unwrap();
        std::os::unix::fs::symlink("aterm", app.join("Contents/MacOS/aterm-ctl")).unwrap();
        std::fs::write(app.join("Contents/Info.plist"), b"<plist/>").unwrap();
        app
    }

    #[test]
    fn ad_hoc_is_signed_and_verified_with_no_keychain_asked() {
        let (_dir, root) = scratch();
        let app = bundle_at(&root);
        let fake = Fake::new(World::default());
        sign(&app, &SignWith::AdHoc, &root, &fake).expect("sign");
        let shown = app.display();
        assert_eq!(
            log(&fake),
            [
                format!("codesign --force --sign - {shown}"),
                format!("codesign --verify --strict {shown}"),
            ]
        );
    }

    /// By default the "aterm dev" identity signs when its keychain exists — by its hash,
    /// its requirement read back and found identity-based — and ad hoc when there is
    /// none; a keychain that exists and cannot be used stops the build before any
    /// codesign.
    #[test]
    fn the_default_signer_is_the_dev_identity_when_it_exists() {
        let (_dir, home) = scratch();
        let app = bundle_at(&home);
        let fake = Fake::new(World::default());
        sign(&app, &SignWith::Default, &home, &fake).expect("ad hoc without a keychain");
        assert!(log(&fake).contains(&format!("codesign --force --sign - {}", app.display())));

        let keychain = dev_identity::default_keychain(&home);
        std::fs::create_dir_all(keychain.parent().unwrap()).unwrap();
        let fake = Fake::new(World {
            search_list: vec![home.join("login.keychain-db")],
            ..World::default()
        });
        dev_identity::create(&keychain, false, &fake).expect("the identity");
        let leaf = fake.0.borrow().keychains[&keychain].identities[0].clone();
        fake.0.borrow_mut().log.clear();
        sign(&app, &SignWith::Default, &home, &fake).expect("signed by the identity");
        let shown = app.display();
        let codesigns: Vec<String> = log(&fake)
            .into_iter()
            .filter(|c| c.starts_with("codesign"))
            .collect();
        assert_eq!(
            codesigns,
            [
                format!("codesign --force --sign {leaf} {shown}"),
                format!("codesign -d -r- {shown}"),
                format!("codesign --verify --strict {shown}"),
            ]
        );

        fake.0.borrow_mut().keychains.insert(
            keychain.clone(),
            Kc {
                password: "not-ours".into(),
                locked: true,
                identities: Vec::new(),
            },
        );
        fake.0.borrow_mut().log.clear();
        let stopped = sign(&app, &SignWith::Default, &home, &fake).unwrap_err();
        assert!(stopped.contains("cannot be used now"), "{stopped}");
        assert!(stopped.contains("--sign-id -"), "{stopped}");
        assert!(!log(&fake).iter().any(|c| c.starts_with("codesign")));
    }

    #[test]
    fn an_identity_whose_requirement_names_bytes_is_refused() {
        let (_dir, root) = scratch();
        let app = bundle_at(&root);
        let fake = Fake::new(World {
            requirement: Some("# designated => cdhash H\"240c6c45\"\n".into()),
            ..World::default()
        });
        let refused = sign(&app, &SignWith::Identity("X".into()), &root, &fake).unwrap_err();
        assert!(refused.contains("not identity-based"), "{refused}");
    }

    /// A codesign that never answers stops the build in one line naming the bundle's
    /// state; codesign's own failure comes through as a failure.
    #[test]
    fn a_codesign_that_does_not_answer_stops_with_the_bundle_state() {
        let (_dir, root) = scratch();
        let app = bundle_at(&root);
        let fake = Fake::new(World {
            hang_on: Some("codesign --force".into()),
            ..World::default()
        });
        let stopped = sign(&app, &SignWith::AdHoc, &root, &fake).unwrap_err();
        assert_eq!(
            stopped,
            format!(
                "macOS's code-signing service (trustd) did not answer within 60 s; {} is \
                 assembled but not signed and nothing was installed — build again once it \
                 answers (a restart clears a wedged trustd)",
                app.display()
            )
        );
        let fake = Fake::new(World {
            fail_on: Some("codesign --verify".into()),
            ..World::default()
        });
        let failed = sign(&app, &SignWith::AdHoc, &root, &fake).unwrap_err();
        assert!(failed.contains("codesign --verify --strict"), "{failed}");
        assert!(failed.contains("scripted failure"), "{failed}");
    }

    /// The install is a verified copy moved into place — links kept, the previous
    /// install replaced — run once with `--version`; LaunchServices is told only of an
    /// install in the Applications folders. A copy that does not verify leaves the
    /// previous install as it was.
    #[test]
    fn install_moves_a_verified_copy_into_place() {
        let (_dir, root) = scratch();
        let app = bundle_at(&root.join("dist"));
        let dest = root.join("Applications");
        let installed = dest.join(DEV_APP_NAME);
        let exe = installed.join("Contents/MacOS/aterm");
        let fake = Fake::new(World::default());
        fake.0
            .borrow_mut()
            .versions
            .insert(exe.clone(), "aterm 0.104.0 (dev)".into());
        install(&app, &dest, false, None, &fake).expect("install");
        assert_eq!(std::fs::read(&exe).unwrap(), b"exe");
        assert_eq!(
            std::fs::read_link(installed.join("Contents/MacOS/aterm-ctl")).ok(),
            Some(PathBuf::from("aterm"))
        );
        assert_eq!(
            std::fs::read_dir(&dest).unwrap().count(),
            1,
            "no staging left"
        );
        let calls = log(&fake);
        assert!(
            calls
                .iter()
                .any(|c| c.starts_with("codesign --verify --strict"))
        );
        assert!(calls.contains(&"aterm --version".to_string()), "{calls:?}");
        assert!(!calls.iter().any(|c| c.starts_with("lsregister")));

        std::fs::write(app.join("Contents/MacOS/aterm"), b"exe 2").unwrap();
        install(&app, &dest, true, None, &fake).expect("reinstall");
        assert_eq!(std::fs::read(&exe).unwrap(), b"exe 2");
        assert!(log(&fake).contains(&format!("lsregister -f {}", installed.display())));

        std::fs::write(app.join("Contents/MacOS/aterm"), b"exe 3").unwrap();
        fake.0.borrow_mut().fail_on = Some("codesign --verify".into());
        assert!(install(&app, &dest, false, None, &fake).is_err());
        assert_eq!(
            std::fs::read(&exe).unwrap(),
            b"exe 2",
            "the previous install stands"
        );
        assert_eq!(
            std::fs::read_dir(&dest).unwrap().count(),
            1,
            "no staging left"
        );
    }

    /// `aterm` on PATH is pointed at the dev build, naming the build it displaced; one
    /// already there displaces nothing, and a file is never replaced.
    #[test]
    fn the_path_link_is_a_choice_that_names_what_it_displaced() {
        let (_dir, root) = scratch();
        let bin = root.join("bin");
        let release = root.join("aterm.app/Contents/MacOS/aterm");
        std::fs::create_dir_all(release.parent().unwrap()).unwrap();
        std::fs::write(&release, b"release").unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(&release, bin.join("aterm")).unwrap();
        let exe = root.join("dev/aterm");
        let fake = Fake::new(World::default());
        assert_eq!(link_cli(&exe, &bin, &fake), Ok(Some(release.clone())));
        assert_eq!(
            std::fs::read_link(bin.join("aterm")).ok(),
            Some(exe.clone())
        );
        assert!(log(&fake).contains(&"aterm --version".to_string()));
        assert_eq!(link_cli(&exe, &bin, &fake), Ok(None), "already there");

        std::fs::remove_file(bin.join("aterm")).unwrap();
        std::fs::write(bin.join("aterm"), b"a copied binary").unwrap();
        assert_eq!(link_cli(&exe, &bin, &fake), Ok(None));
        assert_eq!(
            std::fs::read(bin.join("aterm")).unwrap(),
            b"a copied binary"
        );
    }

    /// `amacs` goes with `aterm`: the dev bundle's editor takes the release's link, and is
    /// named, never run. A file at the name stays; a dev bundle without the editor leaves
    /// the link where it was.
    #[test]
    fn the_editor_link_goes_with_aterm() {
        let (_dir, root) = scratch();
        let bin = root.join("bin");
        let bundle = |app: &str, editor: bool| -> PathBuf {
            let macos = root.join(app).join("Contents/MacOS");
            std::fs::create_dir_all(&macos).unwrap();
            std::fs::write(macos.join("aterm"), app.as_bytes()).unwrap();
            if editor {
                std::os::unix::fs::symlink("aterm", macos.join("amacs")).unwrap();
            }
            macos
        };
        let release = bundle("aterm.app", true);
        let dev = bundle("aterm (dev).app", true);
        let older = bundle("older (dev).app", false);
        std::fs::create_dir_all(&bin).unwrap();
        for name in ["aterm", "amacs"] {
            std::os::unix::fs::symlink(release.join(name), bin.join(name)).unwrap();
        }
        let target = |name: &str| std::fs::read_link(bin.join(name)).ok();
        let fake = Fake::new(World::default());

        assert_eq!(
            link_cli(&dev.join("aterm"), &bin, &fake),
            Ok(Some(release.join("aterm")))
        );
        assert_eq!(target("aterm"), Some(dev.join("aterm")));
        assert_eq!(target("amacs"), Some(dev.join("amacs")));
        assert_eq!(log(&fake), vec!["aterm --version".to_string()]);

        assert_eq!(
            link_cli(&older.join("aterm"), &bin, &fake),
            Ok(Some(dev.join("aterm")))
        );
        assert_eq!(target("aterm"), Some(older.join("aterm")));
        assert_eq!(
            target("amacs"),
            Some(dev.join("amacs")),
            "no editor in the bundle, no link"
        );

        std::fs::remove_file(bin.join("amacs")).unwrap();
        std::fs::write(bin.join("amacs"), b"someone's own").unwrap();
        assert_eq!(
            link_cli(&dev.join("aterm"), &bin, &fake),
            Ok(Some(older.join("aterm")))
        );
        assert_eq!(std::fs::read(bin.join("amacs")).unwrap(), b"someone's own");
    }
}
