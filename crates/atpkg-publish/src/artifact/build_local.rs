// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg-publish build-local <program> <checkout> [--link]`: a native development build
//! from a checkout's atpkg declaration, never signed or uploaded. It is [`super::pack`]'s
//! first half (toolchain resolution, metadata read, build, arch check). The artifact lands
//! under `~/.local/share/aterm/dev-builds/artifacts/` with its `local-build.json` and
//! `SHA256SUMS`; `--link` hands it to the client's dev-link lane (`atpkg::link`), the only
//! thing that touches the store. Either [`Policy`] builds with the sealed toolchain's
//! compiler, and a mutable bootstrap stage is refused.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::pack::{self, Metadata};
use super::tools::{self, HostTools, Run, Tools};
use super::{arch, toolchain};
use crate::ctx::Ctx;
use crate::error::{Error, Result};
use crate::triple::Triple;

/// Which lane builds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// The seal's `cargo` with `RUSTC` its `trustc`: the checkout's configuration holds.
    Repository,
    /// `targo --unverified`, the pack's lane.
    Unverified,
}

impl Policy {
    fn as_str(self) -> &'static str {
        match self {
            Self::Repository => "repository",
            Self::Unverified => "unverified",
        }
    }
}

/// Which cargo profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// `--profile release`.
    Release,
    /// `--profile dev`.
    Dev,
}

impl Profile {
    fn as_str(self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::Dev => "dev",
        }
    }
}

/// One development build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// The program.
    pub program: String,
    /// Its checkout.
    pub checkout: PathBuf,
    /// Print the plan and build nothing.
    pub plan: bool,
    /// The build lane.
    pub policy: Policy,
    /// The cargo profile.
    pub profile: Profile,
    /// Parallel jobs.
    pub jobs: u32,
    /// Hand the result to the client's dev-link lane.
    pub link: bool,
    /// A sealed Trust bin dir; `None` is the store's.
    pub toolchain: Option<PathBuf>,
    /// The build and artifact root; `None` is `~/.local/share/aterm/dev-builds`.
    pub out: Option<PathBuf>,
}

/// What a build will do, decided before anything is built.
#[derive(Clone, Debug)]
pub struct Plan {
    /// The build command (without `--message-format=json`).
    pub command: Run,
    /// The checkout's commit.
    pub source: String,
    /// The compiler's commit.
    pub compiler: String,
    /// The compiler's `-vV`, verbatim.
    pub compiler_text: String,
    /// The sealed sysroot.
    pub sysroot: PathBuf,
    /// The target (the compiler's host: a native build).
    pub triple: Triple,
    /// The declaration.
    pub metadata: Metadata,
    /// The bins built (expose ∪ bundle).
    pub bins: Vec<String>,
    /// The artifact root.
    pub out: PathBuf,
}

fn safe_name(s: &str) -> bool {
    let mut b = s.bytes();
    b.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && b.all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
}

/// Decide the build.
///
/// # Errors
///
/// A usage error for a bad program name or checkout; a refusal for a mutable bootstrap
/// stage, a toolchain missing one of its four drivers, tracked source changes, a
/// bundle-only program or a missing declaration.
pub fn plan(ctx: &Ctx, req: &Request, tools: &dyn Tools) -> Result<Plan> {
    let program = &req.program;
    if !program
        .bytes()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        || !program
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
    {
        return Err(Error::usage(format!("`{program}` is not a program name")));
    }
    let checkout = std::fs::canonicalize(&req.checkout)
        .map_err(|e| Error::usage(format!("{}: {e}", req.checkout.display())))?;
    if !checkout.join("Cargo.toml").is_file() {
        return Err(Error::usage(format!(
            "{} has no Cargo.toml",
            ctx.shown(&checkout)
        )));
    }
    let tc = toolchain::resolve(ctx, req.toolchain.as_deref())?;
    let sysroot = tc.bin.parent().unwrap_or(&tc.bin).to_path_buf();
    if sysroot.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        s.len() == 6 && s.starts_with("stage") && s.as_bytes()[5].is_ascii_digit()
    }) {
        return Err(Error::refused(format!(
            "{} is a mutable bootstrap stage; promote the toolchain first",
            ctx.shown(&sysroot)
        )));
    }
    for tool in ["cargo", "targo", "trustc", "trustdoc"] {
        if !atpkg::toolchain::is_executable_file(&tc.bin.join(tool)) {
            return Err(Error::refused(format!(
                "the sealed toolchain {} lacks {tool}",
                ctx.shown(&sysroot)
            )));
        }
    }
    let compiler_text = tools::version(tools, &tc.trustc()).map_err(Error::refused)?;
    let field = |k: &str| {
        compiler_text
            .lines()
            .find_map(|l| l.strip_prefix(k).map(|v| v.trim().to_string()))
    };
    let compiler = field("commit-hash: ")
        .ok_or_else(|| Error::refused("the compiler's -vV names no commit-hash"))?;
    let triple = Triple::parse(
        &field("host: ").ok_or_else(|| Error::refused("the compiler's -vV names no host"))?,
    )
    .map_err(|e| Error::refused(e.to_string()))?;
    let source = crate::git::head(&checkout)?;
    if !crate::git::run(
        &checkout,
        &["status", "--porcelain", "--untracked-files=no"],
    )?
    .is_empty()
    {
        return Err(Error::refused(
            "the checkout has tracked source changes; commit the development source first",
        ));
    }
    let out = req
        .out
        .clone()
        .unwrap_or_else(|| ctx.home.join(".local/share/aterm/dev-builds"));
    let mut meta_run = Run::new(
        tc.targo(),
        ["metadata", "--no-deps", "--format-version", "1"],
    );
    meta_run.cwd = Some(checkout.clone());
    let ran = tools
        .run(&meta_run)
        .map_err(|e| Error::cannot_run(format!("`targo metadata`: {e}")))?;
    if !ran.success() {
        return Err(Error::refused(format!(
            "`targo metadata` failed: {}",
            String::from_utf8_lossy(&ran.stderr).trim()
        )));
    }
    let metadata = pack::parse_metadata(&ran.stdout, program)?;
    if metadata.bundle_only {
        return Err(Error::refused(format!(
            "{program} needs `atpkg-publish pack-bundle` and a complete sysroot"
        )));
    }
    if metadata.expose.is_empty() {
        return Err(Error::refused(
            "the checkout has no [workspace.metadata.atpkg] expose declaration",
        ));
    }
    let mut bins: Vec<String> = metadata
        .expose
        .iter()
        .chain(&metadata.bundle)
        .cloned()
        .collect();
    bins.sort();
    bins.dedup();
    let (program_bin, lane): (PathBuf, &[&str]) = match req.policy {
        Policy::Repository => (tc.bin.join("cargo"), &[]),
        Policy::Unverified => (tc.targo(), &["--unverified"]),
    };
    let mut args: Vec<OsString> = lane.iter().map(OsString::from).collect();
    args.extend(
        [
            "build",
            "--locked",
            "--profile",
            req.profile.as_str(),
            "-j",
            &req.jobs.to_string(),
        ]
        .map(OsString::from),
    );
    args.extend(metadata.build_args.iter().map(OsString::from));
    for b in &bins {
        args.extend([OsString::from("--bin"), OsString::from(b)]);
    }
    let mut command = Run::new(program_bin, args);
    command.cwd = Some(checkout);
    let mut path = tc.bin.clone().into_os_string();
    if let Some(inherited) = std::env::var_os("PATH") {
        path.push(":");
        path.push(inherited);
    }
    command.env = vec![
        ("PATH".into(), path),
        ("RUSTC".into(), tc.trustc().into()),
        ("RUSTDOC".into(), tc.bin.join("trustdoc").into()),
        (
            "CARGO_TARGET_DIR".into(),
            // Named `*.noindex`: Spotlight skips the build with all it holds.
            out.join("targets.noindex").join(program).into(),
        ),
    ];
    Ok(Plan {
        command,
        source,
        compiler,
        compiler_text,
        sysroot,
        triple,
        metadata,
        bins,
        out,
    })
}

/// What a build produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Built {
    /// The artifact directory (`bin/`, `local-build.json`, `SHA256SUMS`).
    pub dest: PathBuf,
    /// The build's logs.
    pub logs: PathBuf,
    /// The dev-link command that installs it.
    pub link: Vec<String>,
}

fn write(path: &Path, bytes: impl AsRef<[u8]>) -> Result<()> {
    std::fs::write(path, bytes).map_err(|e| Error::io(format!("write {}", path.display()), &e))
}

/// The one executable the build reported for `bin`.
fn executable_of(stream: &[u8], bin: &str) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = String::from_utf8_lossy(stream)
        .lines()
        .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
        .filter(|v| {
            v.get("reason").and_then(aterm_json::Value::as_str) == Some("compiler-artifact")
                && v.get("target")
                    .and_then(|t| t.get("name"))
                    .and_then(aterm_json::Value::as_str)
                    == Some(bin)
        })
        .filter_map(|v| {
            v.get("executable")
                .and_then(aterm_json::Value::as_str)
                .map(PathBuf::from)
        })
        .collect();
    found.sort();
    found.dedup();
    match found.as_slice() {
        [one] => Some(one.clone()),
        _ => None,
    }
}

/// Run the plan: build, check every bin (one executable each, the host's architecture,
/// a native startup smoke), and lay the artifact with its record and digests.
///
/// # Errors
///
/// The build's failure, a source change during it, an existing artifact, or a bin that
/// fails a check.
pub fn build(ctx: &Ctx, req: &Request, plan: &Plan, tools: &dyn Tools) -> Result<Built> {
    let program = &req.program;
    // The same lease a pack holds: gc never reclaims the planned toolchain mid-build.
    let _lease = toolchain::lease(
        ctx,
        &plan.sysroot.join("bin"),
        &format!("atpkg-publish build-local {program}"),
    )?;
    std::fs::create_dir_all(&plan.out)
        .map_err(|e| Error::io(format!("mkdir {}", ctx.shown(&plan.out)), &e))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let run_dir = plan
        .out
        .join(format!("{program}-run.{}.{stamp}", std::process::id()));
    std::fs::create_dir_all(run_dir.join("root/bin"))
        .map_err(|e| Error::io(format!("mkdir {}", run_dir.display()), &e))?;
    write(&run_dir.join("compiler.txt"), &plan.compiler_text)?;
    let shown_command = std::iter::once(plan.command.program.display().to_string())
        .chain(plan.command.argv())
        .collect::<Vec<_>>()
        .join(" ");
    write(
        &run_dir.join("build-command.txt"),
        format!("{shown_command}\n"),
    )?;
    eprintln!("atpkg-publish: build-local logs in {}", ctx.shown(&run_dir));
    let mut command = plan.command.clone();
    command.args.push("--message-format=json".into());
    let ran = tools
        .run(&command)
        .map_err(|e| Error::cannot_run(format!("the build: {e}")))?;
    write(&run_dir.join("artifacts.jsonl"), &ran.stdout)?;
    write(&run_dir.join("build.log"), &ran.stderr)?;
    if !ran.success() {
        let log = String::from_utf8_lossy(&ran.stderr);
        let lines: Vec<&str> = log.lines().collect();
        return Err(Error::refused(format!(
            "the build failed:\n{}",
            lines[lines.len().saturating_sub(30)..].join("\n")
        )));
    }
    let checkout = plan.command.cwd.clone().unwrap_or_default();
    if crate::git::head(&checkout)? != plan.source
        || !crate::git::run(
            &checkout,
            &["status", "--porcelain", "--untracked-files=no"],
        )?
        .is_empty()
    {
        return Err(Error::refused(
            "the source changed during the build; refusing to stage it",
        ));
    }
    let dest = plan.out.join("artifacts").join(program).join(format!(
        "{}-{}-{}-{}",
        &plan.source[..plan.source.len().min(12)],
        &plan.compiler[..plan.compiler.len().min(12)],
        req.profile.as_str(),
        req.policy.as_str()
    ));
    if std::fs::symlink_metadata(&dest).is_ok() {
        return Err(Error::refused(format!(
            "the artifact already exists: {}",
            ctx.shown(&dest)
        )));
    }
    let mut sums = String::new();
    for bin in &plan.bins {
        if !safe_name(bin) {
            return Err(Error::refused(format!("unsafe exposed name: {bin}")));
        }
        let exe = executable_of(&ran.stdout, bin).ok_or_else(|| {
            Error::refused(format!("the build produced no unique executable for {bin}"))
        })?;
        arch::gate(plan.triple, std::slice::from_ref(&exe))?;
        let staged = run_dir.join("root/bin").join(bin);
        std::fs::copy(&exe, &staged).map_err(|e| Error::io(format!("stage {bin}"), &e))?;
        let mut started = false;
        for probe in ["--help", "--version"] {
            let r = tools
                .run(&Run::new(&staged, [probe]).bounded())
                .map_err(|e| Error::refused(format!("{bin} {probe}: {e}")))?;
            write(
                &run_dir.join(format!("{bin}-{}.txt", probe.trim_start_matches('-'))),
                r.text(),
            )?;
            if r.success() {
                started = true;
                break;
            }
        }
        if !started {
            return Err(Error::refused(format!(
                "{bin} did not pass its native startup smoke check; see {}",
                ctx.shown(&run_dir)
            )));
        }
        let sha = atpkg::sha256_file(&staged).map_err(|e| Error::io(format!("hash {bin}"), &e))?;
        sums.push_str(&format!("{sha}  bin/{bin}\n"));
    }
    let record = aterm_json::json!({
        "schema": 1,
        "program": program.clone(),
        "source": plan.source.clone(),
        "compiler": plan.compiler.clone(),
        "sysroot": plan.sysroot.display().to_string(),
        "target": plan.triple.as_str(),
        "policy": req.policy.as_str(),
        "profile": req.profile.as_str(),
        "checkout": checkout.display().to_string(),
        "logs": run_dir.display().to_string(),
        "exposes": plan.metadata.expose.clone(),
    });
    let record = aterm_json::to_string(&record)
        .map_err(|e| Error::cannot_run(format!("render local-build.json: {e}")))?;
    write(
        &run_dir.join("root/local-build.json"),
        format!("{record}\n"),
    )?;
    write(&run_dir.join("root/SHA256SUMS"), sums)?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::io(format!("mkdir {}", parent.display()), &e))?;
    }
    std::fs::rename(run_dir.join("root"), &dest)
        .map_err(|e| Error::io(format!("move the artifact to {}", dest.display()), &e))?;
    let mut link = vec![
        "aterm".to_string(),
        "pkg".into(),
        "link".into(),
        program.clone(),
        dest.display().to_string(),
    ];
    link.extend(plan.metadata.expose.iter().map(|e| format!("bin/{e}")));
    Ok(Built {
        dest,
        logs: run_dir,
        link,
    })
}

/// The client's dev-link lane, in-process: the artifact's exposed bins linked under the
/// store prefix.
///
/// # Errors
///
/// The client's refusal.
pub fn link(ctx: &Ctx, program: &str, built: &Built, exposes: &[String]) -> Result<Vec<String>> {
    let layout = atpkg::Layout {
        prefix: ctx.pkg_prefix.clone(),
    };
    let bins: Vec<PathBuf> = exposes
        .iter()
        .map(|e| PathBuf::from("bin").join(e))
        .collect();
    atpkg::link(&layout, program, &built.dest, &bins)
        .map(|o| o.linked)
        .map_err(|e| Error::refused(format!("{program} not linked — {e}")))
}

/// The command line as a [`Request`].
///
/// # Errors
///
/// A usage error naming what is wrong.
pub fn parse_args(args: &[String]) -> Result<Request> {
    let usage = || {
        Error::usage(
            "build-local <program> <checkout> [--plan] [--policy repository|unverified] \
             [--profile release|dev] [--jobs N] [--toolchain BIN] [--out DIR] [--link]",
        )
    };
    let mut positional = Vec::new();
    let mut req = Request {
        program: String::new(),
        checkout: PathBuf::new(),
        plan: false,
        policy: Policy::Repository,
        profile: Profile::Release,
        jobs: 4,
        link: false,
        toolchain: None,
        out: None,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or_else(usage);
        match a.as_str() {
            "--plan" => req.plan = true,
            "--link" => req.link = true,
            "--policy" => {
                req.policy = match value()?.as_str() {
                    "repository" => Policy::Repository,
                    "unverified" => Policy::Unverified,
                    _ => return Err(usage()),
                };
            }
            "--profile" => {
                req.profile = match value()?.as_str() {
                    "release" => Profile::Release,
                    "dev" => Profile::Dev,
                    _ => return Err(usage()),
                };
            }
            "--jobs" => {
                req.jobs = value()?
                    .parse()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| Error::usage("--jobs takes a positive number"))?;
            }
            "--toolchain" => req.toolchain = Some(PathBuf::from(value()?)),
            "--out" => req.out = Some(PathBuf::from(value()?)),
            s if s.starts_with('-') => return Err(usage()),
            s => positional.push(s.to_string()),
        }
    }
    let [program, checkout] = positional.as_slice() else {
        return Err(usage());
    };
    req.program = program.clone();
    req.checkout = PathBuf::from(checkout);
    Ok(req)
}

/// `atpkg-publish build-local …`.
///
/// # Errors
///
/// A usage error for a malformed command line; the plan's or the build's refusals.
pub fn run(ctx: &Ctx, args: &[String]) -> Result<()> {
    let req = parse_args(args)?;
    let tools = HostTools;
    let plan = plan(ctx, &req, &tools)?;
    println!(
        "atpkg-build-local: {} source={} compiler={} policy={}\n  {} {}",
        req.program,
        plan.source,
        plan.compiler,
        req.policy.as_str(),
        plan.command.program.display(),
        plan.command.argv().join(" ")
    );
    if req.plan {
        return Ok(());
    }
    let built = build(ctx, &req, &plan, &tools)?;
    println!("atpkg-build-local: ready {}", ctx.shown(&built.dest));
    if req.link {
        let linked = link(ctx, &req.program, &built, &plan.metadata.expose)?;
        println!(
            "atpkg-build-local: dev-linked {} ({})",
            req.program,
            linked.join(", ")
        );
    } else {
        println!("  {}", built.link.join(" "));
    }
    Ok(())
}
