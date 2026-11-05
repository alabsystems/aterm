// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Native development builds from atpkg declarations (`atpkg-publish build-local`).
//! Registered harness rows carry their checked main identity and exact build stamp.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::pack::{self, Metadata};
use super::tools::{self, HostTools, Run, Tools};
use super::{arch, toolchain};
use crate::ctx::Ctx;
use crate::error::{Error, Result};
use crate::triple::Triple;

#[cfg(unix)]
#[path = "build_local_harness.rs"]
mod harness;

/// Original prepared harness custody and the shared artifact-validation path.
#[cfg(unix)]
pub use harness::{HarnessBuild, HarnessCacheWork, HarnessCompletion, prepare as prepare_harness};

/// Which lane builds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// The seal's `cargo` with `RUSTC` its `trustc`: the checkout's configuration holds.
    Repository,
    /// `targo --unverified`, the pack's lane.
    Unverified,
}

impl Policy {
    const fn as_str(self) -> &'static str {
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
    const fn as_str(self) -> &'static str {
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
    /// A registered harness's clean main identity; ordinary local builds retain their own lane.
    pub harness: Option<pack::HarnessIdentity>,
    /// Whether this exact driver implements complete content freshness for reusable inputs.
    pub cache_freshness: CacheFreshness,
    /// The original immutable driver uniquely reports genuine JSON session markers.
    pub analysis_session: bool,
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

/// Cache eligibility, not proof or resource authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheFreshness {
    /// Keep compiler state isolated by full source identity.
    ExactSource,
    /// The actual driver reports content freshness for Rust and declared build inputs.
    ContentInputsV1,
}

pub(crate) const CONTENT_CONFIG: &str = "build.fingerprint=\"content\"";
pub(crate) const CHECKSUM_FLAG: &str = "-Zchecksum-freshness";

fn driver_capabilities(tools: &dyn Tools, program: &Path) -> (CacheFreshness, bool) {
    let mut query = Run::new(program, ["--unverified", "-vV"]).bounded();
    query.clear_env = true;
    let Ok(output) = tools.run(&query) else {
        return (CacheFreshness::ExactSource, false);
    };
    let complete = output.success()
        && output.stdout.len() <= 8192
        && std::str::from_utf8(&output.stdout).is_ok_and(|text| {
            text.starts_with("targo ")
                && text
                    .lines()
                    .any(|line| line == "freshness: rust-and-build-input-content-v1")
                && text
                    .lines()
                    .any(|line| line == "cache-integrity: build-script-control-output-v1")
                && text
                    .lines()
                    .any(|line| line == "cache-integrity: compiled-build-script-executable-v1")
                && text
                    .lines()
                    .any(|line| line == "cache-integrity: build-script-generated-tree-v1")
        });
    let analysis = output.success()
        && output.stdout.len() <= 8192
        && std::str::from_utf8(&output.stdout).is_ok_and(|text| {
            text.starts_with("targo ")
                && text
                    .lines()
                    .filter(|line| *line == "analysis-session: build-json-v1")
                    .count()
                    == 1
        });
    (
        if complete {
            CacheFreshness::ContentInputsV1
        } else {
            CacheFreshness::ExactSource
        },
        analysis,
    )
}

// Mutable compiler state spans source generations. Cargo owns dependency/configuration
// invalidation; the original TargetLease serializes compilation through acceptance.
// This path is never artifact or proof authority: immutable destinations, stamps and
// source rechecks still bind the exact source generation.
pub(crate) fn harness_cache_key(
    checkout: &Path,
    source: &str,
    compiler: &str,
    triple: Triple,
    profile: Profile,
    policy: Policy,
    freshness: CacheFreshness,
) -> Result<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let directory = std::fs::symlink_metadata(checkout)
            .map_err(|e| Error::io("read original compiler checkout identity", &e))?;
        if !directory.file_type().is_dir() {
            return Err(Error::refused(
                "compiler checkout is not an original directory",
            ));
        }
        // Birth time distinguishes a recreated checkout even if its inode was reused.
        // Filesystems without a usable birth time keep exact-source isolation;
        // missing reuse metadata must not prevent an otherwise valid build.
        let generation = match (freshness, directory.created()) {
            (CacheFreshness::ExactSource, _) => format!("source-{source}"),
            (_, Ok(created)) => match created.duration_since(std::time::UNIX_EPOCH) {
                Ok(age) if !age.is_zero() => format!("content-v1-generation-{:x}", age.as_nanos()),
                _ => format!("source-{source}"),
            },
            (_, Err(error)) if error.kind() == std::io::ErrorKind::Unsupported => {
                format!("source-{source}")
            }
            (_, Err(error)) => {
                return Err(Error::io(
                    "read original compiler checkout generation",
                    &error,
                ));
            }
        };
        // Package-cache admission requires a single direct child, never nested targets.
        Ok(PathBuf::from(format!(
            "checkout-{:x}-{:x}-{generation}-{compiler}-{}-{}-{}.noindex",
            directory.dev(),
            directory.ino(),
            triple.as_str(),
            profile.as_str(),
            policy.as_str(),
        )))
    }
    #[cfg(not(unix))]
    {
        let _ = (
            checkout, source, compiler, triple, profile, policy, freshness,
        );
        Err(Error::refused(
            "local harness compiler reuse requires an original Unix checkout identity",
        ))
    }
}

/// The producer's immutable local harness namespace, without opening or authorizing it.
/// A manager may ask the installer to validate a completed prior artifact here. A collision
/// that lacks a valid complete receipt must be preserved, never rebuilt over or removed.
/// # Errors
/// A plan that does not identify a registered harness.
#[cfg(unix)]
pub fn harness_artifact_path(req: &Request, plan: &Plan) -> Result<PathBuf> {
    if plan.harness.is_none() {
        return Err(Error::refused(
            "the local manager requires a registered harness plan",
        ));
    }
    Ok(harness::destination(req, plan))
}

fn safe_name(s: &str) -> bool {
    let mut b = s.bytes();
    b.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && b.all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
}

/// A harness receipt and its immutable output path name one full compiler commit.
fn harness_compiler(text: &str) -> Result<&str> {
    let mut fields = text
        .lines()
        .filter_map(|line| line.strip_prefix("commit-hash:"));
    let commit = fields.next().and_then(|field| field.strip_prefix(' '));
    match commit {
        Some(commit)
            if fields.next().is_none()
                && commit.len() == 40
                && commit
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) =>
        {
            Ok(commit)
        }
        _ => Err(Error::refused(
            "a harness compiler's -vV must name exactly one full lowercase 40-hex commit-hash",
        )),
    }
}

/// Decide the build.
///
/// # Errors
///
/// A usage error for a bad program name or checkout. A refusal for a toolchain
/// `toolchain::resolve` refuses, a mutable bootstrap stage, a toolchain missing one of its four
/// drivers, a compiler whose `-vV` fails or names no commit or no host that parses (a harness
/// requires exactly one full lowercase 40-hex compiler commit), a checkout
/// git fails on or that has tracked source changes, a `targo metadata` that fails or prints JSON
/// `pack::parse_metadata` refuses, a bundle-only program or a missing expose declaration. A
/// could-not-run when the toolchain directory does not resolve, git cannot be spawned, or
/// `targo metadata` cannot run.
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
    let harness_row =
        crate::programs::get(program).and_then(|def| def.harness().map(|row| (def, row)));
    let compiler = if harness_row.is_some() {
        harness_compiler(&compiler_text)?.to_string()
    } else {
        field("commit-hash: ")
            .ok_or_else(|| Error::refused("the compiler's -vV names no commit-hash"))?
    };
    let triple = Triple::parse(
        &field("host: ").ok_or_else(|| Error::refused("the compiler's -vV names no host"))?,
    )
    .map_err(|e| Error::refused(e.to_string()))?;
    let mut source = crate::git::head(&checkout)?;
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
    let mut metadata = pack::parse_metadata(&ran.stdout, program)?;
    let harness = if let Some((def, row)) = harness_row {
        let (held, identity) = pack::harness_identity(
            ctx.hosts.staging.as_ref(),
            program,
            row,
            metadata,
            &checkout,
            None,
            (&ctx.accounts.staging, def.repo),
        )?;
        metadata = held;
        source = identity.source.clone();
        Some(identity)
    } else {
        None
    };
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
    args.extend(
        metadata
            .build_args
            .iter()
            .filter(|arg| harness.is_none() || arg.as_str() != "--locked")
            .map(OsString::from),
    );
    for b in &bins {
        args.extend([OsString::from("--bin"), OsString::from(b)]);
    }
    let (cache_freshness, analysis_session) =
        if harness.is_some() && req.policy == Policy::Unverified {
            driver_capabilities(tools, &tc.targo())
        } else {
            (CacheFreshness::ExactSource, false)
        };
    if cache_freshness == CacheFreshness::ContentInputsV1 {
        args.extend(["--config", CONTENT_CONFIG, CHECKSUM_FLAG].map(OsString::from));
    }
    let mut command = Run::new(program_bin, args);
    command.cwd = Some(checkout.clone());
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
            out.join("targets.noindex").join(program).into(),
        ),
    ];
    if let Some(identity) = &harness {
        command.env_remove = pack::SCRUBBED_ENV.iter().map(|key| (*key).into()).collect();
        command.env.push((
            pack::HARNESS_VERSION_ENV.into(),
            identity.version.clone().into(),
        ));
        command.env.push((
            "CARGO_TARGET_DIR".into(),
            out.join(".ubd")
                .join("targets.noindex")
                .join(program)
                .join(harness_cache_key(
                    &checkout,
                    &source,
                    &compiler,
                    triple,
                    req.profile,
                    req.policy,
                    cache_freshness,
                )?)
                .into(),
        ));
    }
    Ok(Plan {
        command,
        source,
        harness,
        cache_freshness,
        analysis_session,
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
    use std::io::Write as _;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let mut f = opts
        .open(path)
        .map_err(|e| Error::io(format!("write {}", path.display()), &e))?;
    if !f
        .metadata()
        .map_err(|e| Error::io(format!("write {}", path.display()), &e))?
        .is_file()
    {
        return Err(Error::refused(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    f.set_len(0)
        .and_then(|()| f.write_all(bytes.as_ref()))
        .map_err(|e| Error::io(format!("write {}", path.display()), &e))
}

fn executable_of(stream: &[u8], bin: &str) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = String::from_utf8_lossy(stream)
        .lines()
        .filter(|l| l.contains("\"compiler-artifact\""))
        .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
        .filter(|v| {
            v.get("reason").and_then(aterm_json::Value::as_str) == Some("compiler-artifact")
                && v.get("profile")
                    .and_then(|p| p.get("test"))
                    .and_then(aterm_json::Value::as_bool)
                    != Some(true)
                && v.get("target").is_some_and(|t| {
                    t.get("name").and_then(aterm_json::Value::as_str) == Some(bin)
                        && t.get("kind")
                            .and_then(aterm_json::Value::as_array)
                            .is_some_and(|ks| ks.iter().any(|k| k.as_str() == Some("bin")))
                })
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

/// Run the plan, check every bin, and lay the artifact with its record and digests.
///
/// # Errors
///
/// Build failure, source change during the build, existing artifact, or failed bin check.
/// A harness also refuses outputs outside the producer's home, unsafe existing ancestry,
/// or an unavailable Unix private-filesystem check.
pub fn build(ctx: &Ctx, req: &Request, plan: &Plan, tools: &dyn Tools) -> Result<Built> {
    if plan.harness.is_some() {
        #[cfg(unix)]
        return harness::build(ctx, req, plan, tools);
        #[cfg(not(unix))]
        return Err(Error::refused(
            "local harness production needs Unix private-filesystem admission",
        ));
    }
    let program = &req.program;
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
        let mut lines: Vec<&str> = log.lines().rev().take(30).collect();
        lines.reverse();
        return Err(Error::refused(format!(
            "the build failed:\n{}",
            lines.join("\n")
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
        crate::identity::copy_regular_file(&exe, &staged)
            .map_err(|e| Error::io(format!("stage {bin}"), &e))?;
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
    if let Some(parent) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
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

/// Link the artifact's exposed bins under the store prefix.
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
                let v = value()?;
                req.jobs = (!v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                    .then(|| v.parse().ok())
                    .flatten()
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
    if let Some(identity) = &plan.harness {
        println!(
            "  harness build={} version={} origin=local-build",
            identity.build_number, identity.version
        );
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_rejects_plus_prefixed_jobs() {
        let args = ["aterm", "/tmp/aterm", "--jobs", "+4"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>();
        assert!(parse_args(&args).is_err());
    }
}
