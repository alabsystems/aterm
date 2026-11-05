// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Stamped local harness artifacts. No store installation or publication authority.

use std::io::{Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use aterm_uds::private_file::{self, PrivateDir};

use super::{Built, Plan, Profile, Request, executable_of, safe_name};
use crate::artifact::tools::{Ran, Run, Tools};
use crate::artifact::{arch, pack, toolchain};
use crate::ctx::Ctx;
use crate::error::{Error, Result};

#[derive(PartialEq, Eq)]
struct Payload {
    size: u64,
    sha256: String,
}

fn ancestry(home: &Path, path: &Path, create: bool) -> Result<()> {
    if !home.is_absolute() || home.canonicalize().ok().as_deref() != Some(home) {
        return Err(Error::refused(
            "the producer home must be a canonical owned directory",
        ));
    }
    let relative = path
        .strip_prefix(home)
        .map_err(|_| Error::refused("harness outputs must be inside the producer home"))?;
    let mut current = home.to_path_buf();
    for part in std::iter::once(None).chain(relative.components().map(Some)) {
        if let Some(part) = part {
            let std::path::Component::Normal(name) = part else {
                return Err(Error::refused(
                    "harness output ancestry must have ordinary components",
                ));
            };
            current.push(name);
        }
        match private_file::judge_private_dir(&current)
            .map_err(|e| Error::io("judge harness output ancestry", &e))?
        {
            PrivateDir::Private => {}
            PrivateDir::Open(mode) if mode & 0o022 == 0 => {}
            PrivateDir::Missing if create => private_file::create_private_dir(&current)
                .map_err(|e| Error::io("create private harness directory", &e))?,
            _ => {
                return Err(Error::refused(format!(
                    "harness output ancestry is unavailable or writable by others: {}",
                    current.display()
                )));
            }
        }
    }
    Ok(())
}

fn reserve_private_dir(home: &Path, path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::refused("private directory has no parent"))?;
    ancestry(home, parent, false)?;
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(path)
        .map_err(|e| Error::io(format!("reserve {}", path.display()), &e))?;
    // This call made the name exclusively; only that new directory is normalized.
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .and_then(|f| f.set_permissions(std::fs::Permissions::from_mode(0o700)))
            .map_err(|e| Error::io("set new private directory mode", &e))?;
    }
    private_file::check_private_dir(path)
        .map_err(|e| Error::io("check private harness directory", &e))
}

fn payload(path: &Path, private: bool) -> Result<Payload> {
    let (mut file, opened) = if private {
        let parent = path
            .parent()
            .ok_or_else(|| Error::refused("executable has no parent"))?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| Error::refused("executable has no ordinary name"))?;
        private_file::open_private(parent, name)
            .map_err(|e| Error::refused(format!("private harness executable: {e}")))?
    } else {
        crate::identity::open_regular_nonblock(path)
            .map_err(|e| Error::io("open compiler output", &e))?
    };
    if !opened.is_file() || !atpkg::platform::dir_meta_is_private(&opened) {
        return Err(Error::refused(
            "the executable is not an owned non-writable regular file",
        ));
    }
    let mut hash = aterm_digest::Sha256::new();
    let mut bytes = [0_u8; 64 * 1024];
    let mut size = 0_u64;
    loop {
        let read = file
            .read(&mut bytes)
            .map_err(|e| Error::io("read executable bytes", &e))?;
        if read == 0 {
            break;
        }
        hash.update(&bytes[..read]);
        size += read as u64;
    }
    if size != opened.len() {
        return Err(Error::refused("the executable changed while hashing"));
    }
    let sha256 = aterm_codec::hex::encode_lower(&hash.finalize());
    Ok(Payload { size, sha256 })
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    private_file::check_private_dir(
        path.parent()
            .ok_or_else(|| Error::refused("private file has no parent"))?,
    )
    .map_err(|e| Error::io("check private record directory", &e))?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
        .open(path)
        .map_err(|e| Error::io(format!("create {}", path.display()), &e))?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .and_then(|()| file.write_all(bytes))
        .and_then(|()| aterm_durable::durable(&file))
        .map_err(|e| Error::io(format!("record {}", path.display()), &e))
}

fn copy_new(from: &Path, to: &Path) -> Result<()> {
    let (mut input, meta) = crate::identity::open_regular_nonblock(from)
        .map_err(|e| Error::io(format!("open {}", from.display()), &e))?;
    if !meta.is_file() || !atpkg::platform::dir_meta_is_private(&meta) {
        return Err(Error::refused(
            "the compiler output is not a regular executable",
        ));
    }
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
        .open(to)
        .map_err(|e| Error::io(format!("reserve {}", to.display()), &e))?;
    std::io::copy(&mut input, &mut output)
        .and_then(|_| output.set_permissions(std::fs::Permissions::from_mode(0o700)))
        .and_then(|()| aterm_durable::durable(&output))
        .map_err(|e| Error::io(format!("copy {}", to.display()), &e))
}

fn version_proof(stdout: &[u8], program: &str, version: &str) -> bool {
    if stdout.len() > 4096 {
        return false;
    }
    let Ok(line) = std::str::from_utf8(stdout) else {
        return false;
    };
    let line = line.strip_suffix('\n').unwrap_or(line);
    if line.chars().any(char::is_control) {
        return false;
    }
    let prefix = format!("{program} {version}");
    line.strip_prefix(&prefix)
        .is_some_and(|tail| tail.is_empty() || (tail.starts_with(" (") && tail.ends_with(')')))
}

pub(super) fn destination(req: &Request, plan: &Plan) -> PathBuf {
    plan.out.join("artifacts").join(&req.program).join(format!(
        "{}-{}-{}-{}-{}",
        plan.source,
        plan.compiler,
        plan.triple.as_str(),
        req.profile.as_str(),
        req.policy.as_str()
    ))
}

/// Prepared artifact custody retaining the original pack and toolchain leases.
/// Only the existing checked preparation path can construct this state.
pub struct HarnessBuild {
    req: Request,
    plan: Plan,
    home: PathBuf,
    checkout: PathBuf,
    target: PathBuf,
    dest: PathBuf,
    logs: PathBuf,
    analysis_home: Option<PathBuf>,
    _turn: aterm_update_core::FileLock,
    _lease: std::sync::Arc<toolchain::Held>,
    _target_lease: aterm_capacity::epoch_target::TargetLease,
}

/// Original artifact acceptance and ancillary observations, kept independent.
pub struct HarnessCompletion {
    /// The unchanged native/source/artifact acceptance result.
    pub built: Result<Built>,
    /// Exact-session observations, or a closed unavailable reason.
    pub cache_work: aterm_capacity::cache_work::Collection,
}

#[path = "build_local_harness_epoch.rs"]
mod epoch;

#[path = "build_local_harness_cache_work.rs"]
mod cache_work;
pub use cache_work::HarnessCacheWork;

impl HarnessBuild {
    /// Original manifest to dispatch through the execution owner.
    #[must_use]
    pub fn manifest(&self) -> PathBuf {
        self.checkout.join("Cargo.toml")
    }

    /// Stable compiler target, independent of epoch transport and artifact.
    #[must_use]
    pub fn target(&self) -> &Path {
        &self.target
    }

    /// Lend this original target to one actual owner compile while retaining
    /// exclusivity through subsequent artifact acceptance.
    /// # Errors
    /// Another live loan or a changed original target/lock inode.
    pub fn target_loan(&mut self) -> Result<aterm_capacity::epoch_target::TargetLoan> {
        self._target_lease
            .loan()
            .map_err(|e| Error::io("lend original compiler target", &e))
    }
}

/// Prepare the existing native artifact builder before compilation.
/// The returned value retains the original pack and toolchain leases.
/// # Errors
/// Invalid source/compiler identity, unsafe ancestry, an existing immutable
/// artifact reservation, or unavailable original locks and toolchain custody.
pub fn prepare(ctx: &Ctx, req: &Request, plan: &Plan) -> Result<HarnessBuild> {
    if super::harness_compiler(&plan.compiler_text)? != plan.compiler {
        return Err(Error::refused(
            "the harness plan's compiler differs from its checked -vV identity",
        ));
    }
    let identity = plan
        .harness
        .as_ref()
        .ok_or_else(|| Error::refused("missing harness identity"))?;
    if !plan.out.is_absolute() {
        return Err(Error::usage("a harness artifact root must be absolute"));
    }
    let checkout = plan
        .command
        .cwd
        .as_deref()
        .ok_or_else(|| Error::refused("the harness build has no source directory"))?;
    let _turn = pack::pack_lock(ctx, &req.program, pack::LOCK_WAIT)?;
    identity.recheck(checkout)?;
    ancestry(&ctx.home, &plan.out, true)?;
    let target = plan
        .command
        .env_of("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .filter(|target| target.starts_with(&plan.out))
        .ok_or_else(|| Error::refused("the harness target must be in its admitted output root"))?;
    ancestry(&ctx.home, &target, true)?;
    let _target_lease = aterm_capacity::epoch_target::TargetLease::acquire(&target)
        .map_err(|e| Error::io("retain original compiler target", &e))?;
    let profile = target.join(match req.profile {
        Profile::Release => "release",
        Profile::Dev => "debug",
    });
    ancestry(&ctx.home, &profile, true)?;
    let dest = destination(req, plan);
    match std::fs::symlink_metadata(&dest) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => {
            return Err(Error::refused(format!(
                "the harness artifact already exists: {}",
                dest.display()
            )));
        }
        Err(e) => return Err(Error::io("inspect the harness artifact", &e)),
    }
    let _lease = toolchain::lease(
        ctx,
        &plan.sysroot.join("bin"),
        &format!("local harness {}", req.program),
    )?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let logs = plan.out.join(".ubd/build-logs.noindex").join(format!(
        "{}-run.{}.{stamp}.noindex",
        req.program,
        std::process::id()
    ));
    ancestry(
        &ctx.home,
        logs.parent()
            .ok_or_else(|| Error::refused("logs have no parent"))?,
        true,
    )?;
    reserve_private_dir(&ctx.home, &logs)?;
    write_new(&logs.join("compiler.txt"), plan.compiler_text.as_bytes())?;
    let shown_command = std::iter::once(plan.command.program.display().to_string())
        .chain(plan.command.argv())
        .collect::<Vec<_>>()
        .join(" ");
    write_new(&logs.join("build-command.txt"), shown_command.as_bytes())?;
    eprintln!("atpkg-publish: local harness logs in {}", ctx.shown(&logs));
    Ok(HarnessBuild {
        req: req.clone(),
        plan: plan.clone(),
        home: ctx.home.clone(),
        checkout: checkout.to_path_buf(),
        target,
        dest,
        logs,
        analysis_home: cache_work::original_cargo_home(plan),
        _turn,
        _lease: std::sync::Arc::new(_lease),
        _target_lease,
    })
}

pub(super) fn build(ctx: &Ctx, req: &Request, plan: &Plan, tools: &dyn Tools) -> Result<Built> {
    let prepared = prepare(ctx, req, plan)?;
    let mut command = plan.command.clone();
    command.args.push("--message-format=json".into());
    let ran = tools
        .run(&command)
        .map_err(|e| Error::cannot_run(format!("the build: {e}")))?;
    prepared.complete(ctx, tools, ran)
}

impl HarnessBuild {
    fn complete(self, ctx: &Ctx, tools: &dyn Tools, ran: Ran) -> Result<Built> {
        if self.home != ctx.home {
            return Err(Error::refused(
                "prepared harness home changed before completion",
            ));
        }
        let Self {
            req,
            plan,
            checkout,
            target,
            dest,
            logs,
            _turn,
            _lease,
            _target_lease,
            ..
        } = self;
        let identity = plan
            .harness
            .as_ref()
            .ok_or_else(|| Error::refused("prepared harness source identity is missing"))?;
        _target_lease
            .check()
            .map_err(|e| Error::io("reprove original compiler target", &e))?;
        write_new(&logs.join("build.stdout"), &ran.stdout)?;
        write_new(&logs.join("build.stderr"), &ran.stderr)?;
        if !ran.success() {
            return Err(Error::refused(format!(
                "the harness build failed; see {}",
                logs.display()
            )));
        }
        identity.recheck(&checkout)?;
        if plan.bins.len() != 1 || plan.metadata.expose != plan.bins {
            return Err(Error::refused(
                "a local harness must produce exactly its declared tool",
            ));
        }
        let bin = &plan.bins[0];
        if !safe_name(bin) {
            return Err(Error::refused("unsafe local harness tool name"));
        }
        let exe = executable_of(&ran.stdout, bin)
            .ok_or_else(|| Error::refused("the harness build produced no unique executable"))?;
        if !exe.starts_with(&target) {
            return Err(Error::refused(
                "the compiler output leaves its admitted target",
            ));
        }
        ancestry(
            &ctx.home,
            exe.parent()
                .ok_or_else(|| Error::refused("compiler output has no parent"))?,
            false,
        )?;
        let before = payload(&exe, false)?;
        ancestry(
            &ctx.home,
            dest.parent()
                .ok_or_else(|| Error::refused("artifact has no parent"))?,
            true,
        )?;
        // Reserve once and write in place. A failed or unproved first start never
        // permits a later attempt to rename, remove or overwrite this path.
        reserve_private_dir(&ctx.home, &dest)?;
        reserve_private_dir(&ctx.home, &dest.join("bin"))?;
        let staged = dest.join("bin").join(bin);
        copy_new(&exe, &staged)?;
        if payload(&staged, true)? != before || payload(&exe, false)? != before {
            return Err(Error::refused(
                "the harness executable changed while staging",
            ));
        }
        arch::gate(plan.triple, std::slice::from_ref(&staged))?;
        ancestry(&ctx.home, &dest, false)?;
        private_file::check_private_dir(&dest)
            .map_err(|e| Error::io("recheck harness artifact", &e))?;
        let _private = private_file::open_private(
            staged
                .parent()
                .ok_or_else(|| Error::refused("harness bin absent"))?,
            bin,
        )
        .map_err(|e| Error::refused(format!("private executable before version proof: {e}")))?;
        let mut version = Run::new(&staged, ["--version"]).bounded();
        version.clear_env = true;
        version.env.push(("HOME".into(), ctx.home.clone().into()));
        let probe = tools
            .run(&version)
            .map_err(|e| Error::refused(format!("harness version probe: {e}")))?;
        write_new(
            &logs.join(format!("{bin}-version.txt")),
            probe.text().as_bytes(),
        )?;
        if !probe.success() || !version_proof(&probe.stdout, &req.program, &identity.version) {
            return Err(Error::refused(
                "the harness did not report its exact compiled build version",
            ));
        }
        identity.recheck(&checkout)?;
        ancestry(&ctx.home, &dest, false)?;
        _target_lease
            .check()
            .map_err(|e| Error::io("reprove target after version proof", &e))?;
        private_file::check_private_dir(&dest)
            .map_err(|e| Error::io("recheck harness artifact", &e))?;
        if payload(&staged, true)? != before || payload(&exe, false)? != before {
            return Err(Error::refused(
                "the harness executable changed during its version proof",
            ));
        }
        let relative = format!("bin/{bin}");
        write_new(
            &dest.join("SHA256SUMS"),
            format!("{}  {relative}\n", before.sha256).as_bytes(),
        )?;
        let record = aterm_json::json!({
            "schema": 2, "origin": "local-build", "program": req.program,
            "source": identity.source, "build_number": identity.build_number,
            "version": identity.version, "source_policy": "clean-main", "command": bin,
            "compiler": plan.compiler, "sysroot": plan.sysroot.display().to_string(),
            "target": plan.triple.as_str(), "policy": req.policy.as_str(),
            "profile": req.profile.as_str(), "checkout": checkout.display().to_string(),
            "logs": logs.display().to_string(), "exposes": plan.metadata.expose,
            "executables": [{"path": relative, "size": before.size, "sha256": before.sha256}]
        });
        let encoded = aterm_json::to_vec(&record)
            .map_err(|e| Error::cannot_run(format!("local receipt: {e}")))?;
        aterm_durable::durable_dir(&dest.join("bin"))
            .map_err(|e| Error::io("flush harness bin", &e))?;
        // The complete receipt is last; a partial artifact is retained and never installed.
        write_new(&dest.join("local-build.json"), &encoded)?;
        aterm_durable::durable_dir(&dest).map_err(|e| Error::io("flush harness receipt", &e))?;
        aterm_durable::durable_dir(
            dest.parent()
                .ok_or_else(|| Error::refused("artifact has no parent"))?,
        )
        .map_err(|e| Error::io("flush harness artifact parent", &e))?;
        Ok(Built {
            link: vec![
                "aterm".into(),
                "pkg".into(),
                "link".into(),
                req.program.clone(),
                dest.display().to_string(),
                relative,
            ],
            dest,
            logs,
        })
    }
}
