// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The vz lane's guest agent build (docs/DESIGN-vz-guest-2026-10-23.md §6): [`build_agent`]
//! cross-builds `crates/aterm-vz-agent` as a static `aarch64-unknown-linux-musl` executable and
//! places it content-addressed at `vz.noindex/agent/<sha256>/aterm-vz-agent`, the directory the
//! guest's boot share is.
//!
//! STOCK EXCEPTION: the build runs on the checkout's stock pin
//! (`aterm_toolchain::rustup::stock_pin`), because Trust has no Linux std
//! ([`crate::linux_guest::RUST`]'s note), linked by the pin's own `rust-lld` against the musl std
//! `targo --unverified verify provision --linux` places, configured by `--config` tables, never
//! `RUSTFLAGS`. rustup's floating `stable` never builds it: its release is whatever a machine
//! last installed.

use std::ffi::OsString;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use aterm_uds::spawnfd::Stdio;

use crate::linux::not_run;
use crate::{Fail, LAB, Res};

/// The agent's package and executable.
pub const AGENT: &str = "aterm-vz-agent";

/// The guest's triple: a static musl executable needs no guest libc.
pub const AGENT_TRIPLE: &str = "aarch64-unknown-linux-musl";

/// What places a missing piece of the build: the stock pin, its musl std and its `rust-lld`.
pub const PROVISION: &str = "targo --unverified verify provision --linux";

/// The labelled reason the build is stock Rust, handed to the build in `ATERM_STOCK_REASON`.
pub const STOCK_REASON: &str = "STOCK EXCEPTION: the vz guest agent is a static \
                                aarch64-unknown-linux-musl executable and Trust has no Linux std \
                                (docs/DESIGN-vz-guest-2026-10-23.md §6)";

/// The variables that would steer the stock build off its recipe (its flags, its compiler,
/// its target, a wrapper), removed from what it inherits: the recipe's `--config` tables
/// and [`Recipe::set`] are the build's whole configuration beyond `.cargo/config.toml`.
const STEERING: [&str; 17] = [
    "RUSTFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
    "RUSTDOCFLAGS",
    "CARGO_ENCODED_RUSTDOCFLAGS",
    "CARGO_BUILD_RUSTFLAGS",
    "CARGO_BUILD_RUSTDOCFLAGS",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "CARGO_BUILD_RUSTC",
    "CARGO_BUILD_RUSTC_WRAPPER",
    "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
    "CARGO_BUILD_TARGET",
    "CARGO_BUILD_TARGET_DIR",
    "CARGO_BUILD_BUILD_DIR",
    "RUSTUP_TOOLCHAIN",
    "CARGO",
    "RUSTDOC",
];

/// The prefixes of the per-target and per-profile variables that would do the same.
const STEERING_PREFIXES: [&str; 2] = [
    "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_",
    "CARGO_PROFILE_RELEASE_",
];

/// The checkout's stock pin, as the build needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stock {
    /// The `rustup` that runs the build.
    pub rustup: PathBuf,
    /// The pin (`1.97.1`), the toolchain `rustup run` names.
    pub name: String,
    /// The pin's own `rustc` (`rustup which --toolchain <pin> rustc`), handed to cargo in
    /// `RUSTC` so no `rustc` on PATH answers instead.
    pub rustc: PathBuf,
    /// Its sysroot (`rustc --print sysroot`).
    pub sysroot: PathBuf,
    /// Its host triple (`rustc -vV`'s `host:`).
    pub host: String,
}

impl Stock {
    /// The linker: the pin's own `rust-lld`, which links a musl executable on macOS.
    #[must_use]
    pub fn rust_lld(&self) -> PathBuf {
        self.sysroot
            .join("lib/rustlib")
            .join(&self.host)
            .join("bin/rust-lld")
    }

    /// Where `triple`'s std is, when it is installed.
    #[must_use]
    pub fn std_dir(&self, triple: &str) -> PathBuf {
        self.sysroot.join("lib/rustlib").join(triple).join("lib")
    }

    /// Asks `rustup` for the pin `name`'s compiler, its sysroot and its host, once rustup lists
    /// the pin.
    ///
    /// # Errors
    /// rustup does not hold the pin, or its `rustc` would not say.
    pub fn find(rustup: &Path, name: &str) -> Result<Self, String> {
        let ask = |program: &Path, args: &[&str]| -> Result<String, String> {
            let mut cmd = aterm_uds::spawnfd::command(program);
            cmd.args(args)
                .env_remove("RUSTUP_TOOLCHAIN")
                .stdin(Stdio::null());
            let out = cmd
                .output()
                .map_err(|e| format!("could not run {} ({e})", program.display()))?;
            if out.status.success() {
                Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
            } else {
                Err(format!(
                    "`{} {}` failed: {}",
                    program.display(),
                    args.join(" "),
                    String::from_utf8_lossy(&out.stderr).trim()
                ))
            }
        };
        let listing = ask(rustup, &["toolchain", "list"])?;
        let host = aterm_toolchain::rustup::default_host_from_env();
        if !aterm_toolchain::rustup::holds_toolchain(&listing, name, &host) {
            return Err(format!(
                "rustup holds no `{name}`, the checkout's stock pin"
            ));
        }
        let rustc = PathBuf::from(ask(rustup, &["which", "--toolchain", name, "rustc"])?);
        let sysroot = PathBuf::from(ask(&rustc, &["--print", "sysroot"])?);
        let host = host_of(&ask(&rustc, &["-vV"])?)
            .ok_or_else(|| format!("`{} -vV` names no host", rustc.display()))?;
        Ok(Self {
            rustup: rustup.to_path_buf(),
            name: name.to_string(),
            rustc,
            sysroot,
            host,
        })
    }

    /// Whether the build has what it links with: the cross std and `rust-lld`.
    ///
    /// # Errors
    /// One is missing: which, by its path.
    pub fn ready(&self, triple: &str) -> Result<(), String> {
        let std = self.std_dir(triple);
        if !std.is_dir() {
            return Err(format!(
                "`{}`, the checkout's stock pin, has no {triple} std ({} is absent)",
                self.name,
                std.display()
            ));
        }
        let lld = self.rust_lld();
        if !crate::sys::executable(&lld.to_string_lossy()) {
            return Err(format!(
                "`{}`, the checkout's stock pin, has no rust-lld ({} is absent)",
                self.name,
                lld.display()
            ));
        }
        Ok(())
    }
}

/// `rustc -vV`'s host triple.
fn host_of(verbose_version: &str) -> Option<String> {
    verbose_version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(|host| host.trim().to_string())
        .filter(|host| !host.is_empty())
}

/// `s` as a TOML basic string, for a `--config` value.
fn toml_string(s: &str) -> Option<String> {
    if s.chars().any(char::is_control) {
        return None;
    }
    Some(format!(
        "\"{}\"",
        s.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

/// One build, as data: what [`build_agent`] runs and the tests read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recipe {
    /// `rustup`.
    pub program: PathBuf,
    /// `run <pin> cargo build …`.
    pub args: Vec<String>,
    /// The variables set: `RUSTC`, `CARGO_TARGET_DIR` and `ATERM_STOCK_REASON`.
    pub set: Vec<(String, OsString)>,
    /// The variables removed: [`STEERING`], and any inherited one under [`STEERING_PREFIXES`].
    pub remove: Vec<String>,
    /// The checkout, so its `.cargo/config.toml` applies.
    pub cwd: PathBuf,
}

impl Recipe {
    /// The command line, as a person would type it.
    #[must_use]
    pub fn line(&self) -> String {
        let mut words = vec![self.program.display().to_string()];
        words.extend(self.args.iter().map(|a| {
            if a.contains(['"', ' ', '[']) {
                format!("'{a}'")
            } else {
                a.clone()
            }
        }));
        words.join(" ")
    }

    /// The child that runs it, its output on this program's.
    #[must_use]
    pub fn command(&self) -> aterm_uds::spawnfd::Command {
        let mut cmd = aterm_uds::spawnfd::command(&self.program);
        cmd.args(&self.args)
            .current_dir(&self.cwd)
            .stdin(Stdio::null());
        for name in &self.remove {
            cmd.env_remove(name);
        }
        for (name, value) in &self.set {
            cmd.env(name, value);
        }
        cmd
    }
}

/// The design's build of the agent in the checkout at `repo`, into `target_dir`, for a
/// process whose environment names `inherited`.
///
/// # Errors
/// `repo` or `rust-lld`'s path is not one a `--config` value can carry.
pub fn agent_recipe(
    stock: &Stock,
    repo: &Path,
    target_dir: &Path,
    inherited: &[String],
) -> Result<Recipe, String> {
    let manifest = repo.join("Cargo.toml");
    let manifest = manifest
        .to_str()
        .ok_or_else(|| format!("{} is not UTF-8", manifest.display()))?
        .to_string();
    let lld = stock.rust_lld();
    let linker = lld
        .to_str()
        .and_then(toml_string)
        .ok_or_else(|| format!("{} cannot be a --config value", lld.display()))?;
    let table = format!("target.{AGENT_TRIPLE}");
    let args = [
        "run",
        &stock.name,
        "cargo",
        "build",
        "--locked",
        "-p",
        AGENT,
        "--bin",
        AGENT,
        "--release",
        "--target",
        AGENT_TRIPLE,
        "--manifest-path",
        &manifest,
        "--config",
        &format!("{table}.linker={linker}"),
        "--config",
        &format!(
            "{table}.rustflags=[\"-C\", \"linker-flavor=ld.lld\", \"-C\", \"link-self-contained=yes\"]"
        ),
    ]
    .map(str::to_string)
    .to_vec();
    let mut remove: Vec<String> = STEERING.map(str::to_string).to_vec();
    for name in inherited {
        if STEERING_PREFIXES.iter().any(|p| name.starts_with(p)) && !remove.contains(name) {
            remove.push(name.clone());
        }
    }
    Ok(Recipe {
        program: stock.rustup.clone(),
        args,
        set: vec![
            ("RUSTC".into(), stock.rustc.clone().into_os_string()),
            (
                "CARGO_TARGET_DIR".into(),
                target_dir.to_path_buf().into_os_string(),
            ),
            ("ATERM_STOCK_REASON".into(), STOCK_REASON.into()),
        ],
        remove,
        cwd: repo.to_path_buf(),
    })
}

/// The stock build's own target directory in the checkout at `repo`, apart from targo's.
#[must_use]
pub fn target_dir(repo: &Path) -> PathBuf {
    repo.join("target/.ubd/vz-agent.noindex")
}

/// Where the build in `target_dir` leaves the agent.
#[must_use]
pub fn built(target_dir: &Path) -> PathBuf {
    target_dir.join(AGENT_TRIPLE).join("release").join(AGENT)
}

/// Why `bytes` is not an executable the guest can run without a loader: a 64-bit
/// little-endian aarch64 ELF executable with no `PT_INTERP`.
#[must_use]
pub fn not_a_static_guest_elf(bytes: &[u8]) -> Option<String> {
    const PT_INTERP: u32 = 3;
    const EM_AARCH64: u16 = 183;
    const ELF_HEADER_BYTES: usize = 64;
    const PROGRAM_HEADER_BYTES: usize = 56;
    let u16_at = |at: usize| {
        at.checked_add(2)
            .and_then(|end| bytes.get(at..end))
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    };
    let u32_at = |at: usize| {
        at.checked_add(4)
            .and_then(|end| bytes.get(at..end))
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let u64_at = |at: usize| {
        at.checked_add(8)
            .and_then(|end| bytes.get(at..end))
            .and_then(|b| <[u8; 8]>::try_from(b).ok())
            .map(u64::from_le_bytes)
    };
    if bytes.get(..4) != Some(b"\x7fELF".as_slice()) {
        return Some("not an ELF file".into());
    }
    if bytes.get(4..6) != Some([2, 1].as_slice()) {
        return Some("not a 64-bit little-endian ELF".into());
    }
    if !matches!(u16_at(16), Some(2 | 3)) {
        return Some("not an executable ELF".into());
    }
    if u16_at(18) != Some(EM_AARCH64) {
        return Some("not an aarch64 ELF".into());
    }
    if bytes.len() < ELF_HEADER_BYTES {
        return Some("a truncated ELF header".into());
    }
    let (Some(phoff), Some(phentsize), Some(phnum)) = (u64_at(32), u16_at(54), u16_at(56)) else {
        return Some("a truncated ELF header".into());
    };
    let Ok(phoff) = usize::try_from(phoff) else {
        return Some("program headers past the file".into());
    };
    let stride = usize::from(phentsize);
    if phoff < ELF_HEADER_BYTES || stride < PROGRAM_HEADER_BYTES || phnum == 0 {
        return Some("an invalid ELF program header table".into());
    }
    let table = usize::from(phnum)
        .checked_mul(stride)
        .and_then(|size| phoff.checked_add(size))
        .and_then(|end| bytes.get(phoff..end));
    if table.is_none() {
        return Some("program headers past the file".into());
    }
    for i in 0..usize::from(phnum) {
        // The complete table was bounded above, including this entry's whole stride.
        let Some(kind) = u32_at(phoff + i * stride) else {
            return Some("program headers past the file".into());
        };
        if kind == PT_INTERP {
            return Some("a dynamically linked ELF (it names a loader)".into());
        }
    }
    None
}

/// The agent once placed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placed {
    /// Its SHA-256, lowercase hex: the name of its directory, and the `Hello` the guest
    /// sends.
    pub sha256: String,
    /// `vz.noindex/agent/<sha256>/aterm-vz-agent`.
    pub path: PathBuf,
    /// Its size.
    pub bytes: u64,
    /// Whether this placing made it (else the same agent was there).
    pub fresh: bool,
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = aterm_digest::Sha256::new();
    h.update(bytes);
    hex(&h.finalize())
}

/// Place the agent `built` under `vz` (`vz.noindex`) at `agent/<sha256>/aterm-vz-agent`,
/// mode 0555: written whole in a partial directory, flushed, then renamed into place, so
/// the content-addressed name only ever holds the agent it names. An agent already there
/// under its own hash is kept; one that is not is replaced.
///
/// # Errors
/// `built` is missing or not a static aarch64 ELF, or a write failed.
pub fn place(built: &Path, vz: &Path) -> Result<Placed, String> {
    place_racing(built, vz, &mut |_, _| {})
}

/// Where a concurrent build may place the same agent while [`place_racing`] writes its
/// own copy: before it looks for one, or after it looked and before its rename.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Race {
    BeforeLook,
    BeforeRename,
}

/// [`place`], with `meanwhile` run at each [`Race`] point on the hash directory: what a
/// concurrent build could do there (the tests' seam; [`place`] does nothing there).
fn place_racing(
    built: &Path,
    vz: &Path,
    meanwhile: &mut dyn FnMut(Race, &Path),
) -> Result<Placed, String> {
    let bytes = crate::util::read_regular_file(built)
        .map_err(|e| format!("could not read {} ({e})", built.display()))?;
    if let Some(why) = not_a_static_guest_elf(&bytes) {
        return Err(format!("{} is {why}", built.display()));
    }
    let sha256 = sha256_hex(&bytes);
    let agents = vz.join("agent");
    let dir = agents.join(&sha256);
    let path = dir.join(AGENT);
    let size = bytes.len() as u64;
    let placed = |fresh| Placed {
        sha256: sha256.clone(),
        path: path.clone(),
        bytes: size,
        fresh,
    };
    let holds =
        || crate::util::read_regular_file(&path).is_ok_and(|there| sha256_hex(&there) == sha256);
    if holds() {
        return Ok(placed(false));
    }
    std::fs::create_dir_all(&agents)
        .map_err(|e| format!("could not make {} ({e})", agents.display()))?;
    let partial = agents.join(format!(".partial-{sha256}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&partial);
    let written = (|| -> std::io::Result<bool> {
        std::fs::create_dir(&partial)?;
        let file_at = partial.join(AGENT);
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o555)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&file_at)?;
        file.write_all(&bytes)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o555))?;
        aterm_durable::durable(&file)?;
        aterm_durable::durable_dir(&partial)?;
        meanwhile(Race::BeforeLook, &dir);
        if dir.exists() {
            if holds() {
                // Another build placed this agent meanwhile: keep its copy, which a guest
                // may already run.
                return Ok(false);
            }
            // A directory under this hash that does not hold this agent: a cut-short or
            // altered placing, which only this rename may replace.
            std::fs::remove_dir_all(&dir)?;
        }
        meanwhile(Race::BeforeRename, &dir);
        if let Err(e) = std::fs::rename(&partial, &dir) {
            // A concurrent build renamed its copy in first.
            return if holds() { Ok(false) } else { Err(e) };
        }
        aterm_durable::durable_dir(&agents)?;
        Ok(true)
    })();
    let _ = std::fs::remove_dir_all(&partial);
    match written {
        Ok(fresh) => Ok(placed(fresh)),
        Err(e) => Err(format!("could not place {} ({e})", path.display())),
    }
}

/// Cross-build the agent in the checkout at `repo` and place it under `vz`: the recipe's
/// line, the build's own output, its time, then where the agent is and its SHA-256.
///
/// # Errors
/// NOT RUN when the checkout's engine gate disagrees with its stock pin, or rustup, the pin
/// (for rustup's host), its musl std or `rust-lld` is missing (each named with [`PROVISION`]); a
/// failure when the build or the placing fails.
pub fn build_agent(repo: &Path, vz: &Path) -> Result<Placed, Fail> {
    let rustup = aterm_uds::host::tool("rustup")
        .locate()
        .map_err(|absent| not_run(format!("{} (fix: {PROVISION})", absent.why)))?;
    let pin = aterm_toolchain::rustup::stock_pin(repo).map_err(not_run)?;
    let stock =
        Stock::find(&rustup, &pin).map_err(|why| not_run(format!("{why} (fix: {PROVISION})")))?;
    stock
        .ready(AGENT_TRIPLE)
        .map_err(|why| not_run(format!("{why} (fix: {PROVISION})")))?;
    let target = target_dir(repo);
    let inherited: Vec<String> = std::env::vars_os()
        .filter_map(|(name, _)| name.into_string().ok())
        .collect();
    let recipe = agent_recipe(&stock, repo, &target, &inherited).map_err(Fail::die)?;
    println!("vz-agent: STOCK EXCEPTION, in {}", repo.display());
    println!("vz-agent: {}", recipe.line());
    let t0 = Instant::now();
    let status = recipe
        .command()
        .status()
        .map_err(|e| Fail::die(format!("vz-agent: could not run rustup ({e})")))?;
    let secs = t0.elapsed().as_secs_f64();
    if !status.success() {
        return Err(Fail::die(format!(
            "vz-agent: the build failed after {secs:.1} s ({status})"
        )));
    }
    println!("vz-agent: built in {secs:.1} s");
    place(&built(&target), vz).map_err(|why| Fail::die(format!("vz-agent: {why}")))
}

/// `linux vz-agent`: [`build_agent`] for this checkout, into this user's `vz.noindex`.
///
/// # Errors
/// A usage error, or [`build_agent`]'s.
pub fn cmd_vz_agent(rest: &[String]) -> Res {
    if let Some(extra) = rest.first() {
        return Err(Fail::usage(format!(
            "linux vz-agent takes no arguments, not '{extra}' ({LAB} linux help)"
        )));
    }
    let vz = aterm_uds::host::host_user_cache("vz")
        .ok_or_else(|| Fail::die("vz-agent: no HOME, so no vz.noindex"))?;
    let repo = crate::paths::physical(&crate::paths::paths().repo);
    let placed = build_agent(&repo, &vz)?;
    println!(
        "vz-agent: {} ({} B, {})",
        placed.path.display(),
        placed.bytes,
        if placed.fresh {
            "placed"
        } else {
            "already there"
        }
    );
    println!("vz-agent: sha256 {}", placed.sha256);
    Ok(0)
}

#[cfg(test)]
#[path = "linux_vz_tests.rs"]
mod tests;
