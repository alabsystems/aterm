// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The vz lane's guest agent build (docs/DESIGN-vz-guest-2026-10-23.md §6, slice V1):
//! [`build_agent`] cross-builds `crates/aterm-vz-agent` as a static
//! `aarch64-unknown-linux-musl` executable and places it content-addressed at
//! `vz.noindex/agent/<sha256>/aterm-vz-agent`, the directory the guest's boot share is.
//!
//! STOCK EXCEPTION: the build runs on rustup stable, because Trust has no Linux std
//! ([`crate::linux_guest::RUST`]'s note), linked by stable's own `rust-lld` against the
//! musl std `targo --unverified verify provision --linux` places. It is spelt as the design
//! spells it: `rustup run stable cargo build --locked … --target aarch64-unknown-linux-musl`
//! with `--config` tables for the linker and its flags, never `RUSTFLAGS`.

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

/// What places a missing piece of the build: stable, its musl std and its `rust-lld`.
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

/// rustup's stable toolchain, as the build needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stable {
    /// The `rustup` that runs the build.
    pub rustup: PathBuf,
    /// Stable's own `rustc` (`rustup which --toolchain stable rustc`), handed to cargo in
    /// `RUSTC` so no `rustc` on PATH answers instead.
    pub rustc: PathBuf,
    /// Its sysroot (`rustc --print sysroot`).
    pub sysroot: PathBuf,
    /// Its host triple (`rustc -vV`'s `host:`).
    pub host: String,
}

impl Stable {
    /// The linker: stable's own `rust-lld`, which links a musl executable on macOS.
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

    /// Asks `rustup` for stable's compiler, its sysroot and its host.
    ///
    /// # Errors
    /// rustup has no stable toolchain, or its `rustc` would not say.
    pub fn find(rustup: &Path) -> Result<Self, String> {
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
        let rustc = PathBuf::from(ask(rustup, &["which", "--toolchain", "stable", "rustc"])?);
        let sysroot = PathBuf::from(ask(&rustc, &["--print", "sysroot"])?);
        let host = host_of(&ask(&rustc, &["-vV"])?)
            .ok_or_else(|| format!("`{} -vV` names no host", rustc.display()))?;
        Ok(Self {
            rustup: rustup.to_path_buf(),
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
                "rustup stable has no {triple} std ({} is absent)",
                std.display()
            ));
        }
        let lld = self.rust_lld();
        if !crate::sys::executable(&lld.to_string_lossy()) {
            return Err(format!(
                "rustup stable has no rust-lld ({} is absent)",
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
    /// `run stable cargo build …`.
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
    stable: &Stable,
    repo: &Path,
    target_dir: &Path,
    inherited: &[String],
) -> Result<Recipe, String> {
    let manifest = repo.join("Cargo.toml");
    let manifest = manifest
        .to_str()
        .ok_or_else(|| format!("{} is not UTF-8", manifest.display()))?
        .to_string();
    let lld = stable.rust_lld();
    let linker = lld
        .to_str()
        .and_then(toml_string)
        .ok_or_else(|| format!("{} cannot be a --config value", lld.display()))?;
    let table = format!("target.{AGENT_TRIPLE}");
    let args = [
        "run",
        "stable",
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
        program: stable.rustup.clone(),
        args,
        set: vec![
            ("RUSTC".into(), stable.rustc.clone().into_os_string()),
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
    repo.join("target").join("vz-agent.noindex")
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

fn sha256_hex(bytes: &[u8]) -> String {
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
    let bytes =
        std::fs::read(built).map_err(|e| format!("could not read {} ({e})", built.display()))?;
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
    let holds = || std::fs::read(&path).is_ok_and(|there| sha256_hex(&there) == sha256);
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
        let mut file = std::fs::File::create(&file_at)?;
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
/// NOT RUN when rustup, stable, its musl std or `rust-lld` is missing (each named with
/// [`PROVISION`]); a failure when the build or the placing fails.
pub fn build_agent(repo: &Path, vz: &Path) -> Result<Placed, Fail> {
    let rustup = aterm_toolchain::rustup::find_rustup().ok_or_else(|| {
        not_run(format!(
            "no rustup {} (fix: {PROVISION})",
            aterm_toolchain::rustup::RUSTUP_LOOKED
        ))
    })?;
    let stable =
        Stable::find(&rustup).map_err(|why| not_run(format!("{why} (fix: {PROVISION})")))?;
    stable
        .ready(AGENT_TRIPLE)
        .map_err(|why| not_run(format!("{why} (fix: {PROVISION})")))?;
    let target = target_dir(repo);
    let inherited: Vec<String> = std::env::vars_os()
        .filter_map(|(name, _)| name.into_string().ok())
        .collect();
    let recipe = agent_recipe(&stable, repo, &target, &inherited).map_err(Fail::die)?;
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
mod tests {
    use super::*;

    fn stable() -> Stable {
        Stable {
            rustup: "/opt/rustup/bin/rustup".into(),
            rustc: "/r/toolchains/stable-aarch64-apple-darwin/bin/rustc".into(),
            sysroot: "/r/toolchains/stable-aarch64-apple-darwin".into(),
            host: "aarch64-apple-darwin".into(),
        }
    }

    fn recipe(inherited: &[&str]) -> Recipe {
        let inherited: Vec<String> = inherited.iter().map(|s| (*s).to_string()).collect();
        agent_recipe(
            &stable(),
            Path::new("/src/aterm"),
            Path::new("/src/aterm/target/vz-agent.noindex"),
            &inherited,
        )
        .expect("a recipe")
    }

    #[test]
    fn the_recipe_is_the_designs_stock_cross_build() {
        let r = recipe(&[]);
        assert_eq!(r.program, Path::new("/opt/rustup/bin/rustup"));
        assert_eq!(
            r.args,
            [
                "run",
                "stable",
                "cargo",
                "build",
                "--locked",
                "-p",
                "aterm-vz-agent",
                "--bin",
                "aterm-vz-agent",
                "--release",
                "--target",
                "aarch64-unknown-linux-musl",
                "--manifest-path",
                "/src/aterm/Cargo.toml",
                "--config",
                "target.aarch64-unknown-linux-musl.linker=\"/r/toolchains/stable-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/bin/rust-lld\"",
                "--config",
                "target.aarch64-unknown-linux-musl.rustflags=[\"-C\", \"linker-flavor=ld.lld\", \"-C\", \"link-self-contained=yes\"]",
            ]
        );
        assert_eq!(r.cwd, Path::new("/src/aterm"));
        // The cross triple, never the host's.
        assert!(
            !r.args
                .iter()
                .any(|a| a.contains("apple-darwin") && !a.contains("rust-lld"))
        );
    }

    #[test]
    fn the_recipe_sets_its_compiler_and_target_dir_and_never_rustflags() {
        let r = recipe(&[]);
        let set: Vec<&str> = r.set.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(set, ["RUSTC", "CARGO_TARGET_DIR", "ATERM_STOCK_REASON"]);
        assert_eq!(
            r.set[0].1,
            OsString::from("/r/toolchains/stable-aarch64-apple-darwin/bin/rustc")
        );
        assert_eq!(
            r.set[1].1,
            OsString::from("/src/aterm/target/vz-agent.noindex")
        );
        assert!(r.set[2].1.to_string_lossy().starts_with("STOCK EXCEPTION"));
        for flags in [
            "RUSTFLAGS",
            "CARGO_ENCODED_RUSTFLAGS",
            "RUSTDOCFLAGS",
            "CARGO_BUILD_RUSTFLAGS",
        ] {
            assert!(!set.contains(&flags), "{flags} is set");
            assert!(r.remove.iter().any(|v| v == flags), "{flags} is inherited");
        }
        for steer in [
            "RUSTC_WRAPPER",
            "RUSTUP_TOOLCHAIN",
            "CARGO_BUILD_TARGET",
            "CARGO",
        ] {
            assert!(r.remove.iter().any(|v| v == steer), "{steer} is inherited");
        }
    }

    #[test]
    fn inherited_per_target_and_release_profile_variables_are_removed_and_no_others() {
        let r = recipe(&[
            "PATH",
            "HOME",
            "CARGO_HOME",
            "RUSTUP_HOME",
            "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER",
            "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_RUSTFLAGS",
            "CARGO_PROFILE_RELEASE_LTO",
            "CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER",
            "RUSTFLAGS",
        ]);
        for gone in [
            "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER",
            "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_RUSTFLAGS",
            "CARGO_PROFILE_RELEASE_LTO",
        ] {
            assert!(r.remove.iter().any(|v| v == gone), "{gone}");
        }
        for kept in [
            "PATH",
            "HOME",
            "CARGO_HOME",
            "RUSTUP_HOME",
            "CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER",
        ] {
            assert!(!r.remove.iter().any(|v| v == kept), "{kept}");
        }
        assert_eq!(r.remove.iter().filter(|v| *v == "RUSTFLAGS").count(), 1);
    }

    #[test]
    fn a_linker_path_is_quoted_as_toml_or_refused() {
        let mut s = stable();
        s.sysroot = r#"/odd "dir"\x"#.into();
        let r = agent_recipe(&s, Path::new("/src"), Path::new("/t"), &[]).expect("quoted");
        assert!(r.args.contains(
            &r#"target.aarch64-unknown-linux-musl.linker="/odd \"dir\"\\x/lib/rustlib/aarch64-apple-darwin/bin/rust-lld""#
                .to_string()
        ));
        s.sysroot = "/a\nb".into();
        assert!(agent_recipe(&s, Path::new("/src"), Path::new("/t"), &[]).is_err());
    }

    #[test]
    fn the_hosts_triple_is_read_from_rustc_verbose_version() {
        let text = "rustc 1.99.1 (aaaa 2026-10-15)\nbinary: rustc\nhost: aarch64-apple-darwin\nrelease: 1.99.1\n";
        assert_eq!(host_of(text).as_deref(), Some("aarch64-apple-darwin"));
        assert_eq!(host_of("rustc 1.99.1\n"), None);
        assert_eq!(host_of("host: \n"), None);
    }

    #[test]
    fn a_missing_std_or_linker_is_named_by_its_path() {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let s = Stable {
            sysroot: dir.path().to_path_buf(),
            ..stable()
        };
        let why = s.ready(AGENT_TRIPLE).expect_err("no std");
        assert!(why.contains("no aarch64-unknown-linux-musl std"), "{why}");
        std::fs::create_dir_all(s.std_dir(AGENT_TRIPLE)).expect("std dir");
        let why = s.ready(AGENT_TRIPLE).expect_err("no lld");
        assert!(why.contains("no rust-lld"), "{why}");
        let lld = s.rust_lld();
        std::fs::create_dir_all(lld.parent().expect("bin")).expect("bin dir");
        std::fs::write(&lld, b"").expect("lld");
        std::fs::set_permissions(&lld, std::fs::Permissions::from_mode(0o755)).expect("mode");
        assert_eq!(s.ready(AGENT_TRIPLE), Ok(()));
    }

    /// A minimal 64-bit little-endian ELF header of `kind` for `machine`, with one program
    /// header of `segment`.
    fn elf(kind: u16, machine: u16, segment: u32) -> Vec<u8> {
        let mut b = vec![0u8; 64 + 56];
        b[..4].copy_from_slice(b"\x7fELF");
        b[4] = 2;
        b[5] = 1;
        b[6] = 1;
        b[16..18].copy_from_slice(&kind.to_le_bytes());
        b[18..20].copy_from_slice(&machine.to_le_bytes());
        b[32..40].copy_from_slice(&64u64.to_le_bytes());
        b[54..56].copy_from_slice(&56u16.to_le_bytes());
        b[56..58].copy_from_slice(&1u16.to_le_bytes());
        b[64..68].copy_from_slice(&segment.to_le_bytes());
        b
    }

    #[test]
    fn only_a_static_aarch64_elf_is_a_guest_agent() {
        // PT_LOAD (1) and PT_DYNAMIC (2: a static-pie's own relocations) need no loader.
        assert_eq!(not_a_static_guest_elf(&elf(2, 183, 1)), None);
        assert_eq!(not_a_static_guest_elf(&elf(3, 183, 2)), None);
        let why = |b: &[u8]| not_a_static_guest_elf(b).expect("refused");
        assert!(why(&elf(3, 183, 3)).contains("dynamically linked"));
        assert!(why(&elf(2, 62, 1)).contains("aarch64"));
        assert!(why(&elf(1, 183, 1)).contains("executable"));
        assert!(why(b"\xcf\xfa\xed\xfe\x0c\x00\x00\x01").contains("not an ELF"));
        let mut thirty_two = elf(2, 183, 1);
        thirty_two[4] = 1;
        assert!(why(&thirty_two).contains("64-bit"));
        assert!(why(&elf(2, 183, 1)[..40]).contains("truncated"));
        let mut far = elf(2, 183, 1);
        far[32..40].copy_from_slice(&4096u64.to_le_bytes());
        assert!(why(&far).contains("past the file"));
    }

    /// A corrupt table is refused before placement, including omitted entries and offsets
    /// that would wrap. Complete static output is accepted; a named loader is refused.
    #[test]
    fn malformed_program_headers_are_refused_before_an_agent_is_placed() {
        let good = elf(2, 183, 1);
        assert_eq!(not_a_static_guest_elf(&good), None);
        assert!(not_a_static_guest_elf(&elf(2, 183, 3)).is_some());
        let with = |offset: u64, stride: u16, count: u16| {
            let mut bytes = good.clone();
            bytes[32..40].copy_from_slice(&offset.to_le_bytes());
            bytes[54..56].copy_from_slice(&stride.to_le_bytes());
            bytes[56..58].copy_from_slice(&count.to_le_bytes());
            bytes
        };
        let mut hidden_loader = good.clone();
        hidden_loader.extend_from_slice(&[0u8; 56]);
        hidden_loader[120..124].copy_from_slice(&3u32.to_le_bytes());
        hidden_loader[56..58].copy_from_slice(&2u16.to_le_bytes());
        assert!(not_a_static_guest_elf(&hidden_loader).is_some());
        let malformed = [
            with(u64::MAX, 56, 1),
            with(u64::MAX - 55, 56, 2),
            with(64, 0, 1),
            with(64, 4, 1),
            with(64, 56, 0),
            with(0, 56, 1),
            good[..58].to_vec(),
            good[..68].to_vec(),
            hidden_loader[..124].to_vec(),
        ];
        let scratch = aterm_tempfile::tempdir().expect("private malformed ELF fixtures");
        let built = scratch.path().join("built");
        let vz = scratch.path().join("vz.noindex");
        for bytes in malformed {
            assert!(not_a_static_guest_elf(&bytes).is_some());
            std::fs::write(&built, &bytes).expect("fixture");
            assert!(place(&built, &vz).is_err());
            assert!(!vz.exists(), "a refused file creates no agent store");
        }
    }

    #[test]
    fn the_agent_is_placed_under_its_hash_read_only_and_kept_when_there() {
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let built = scratch.path().join("built");
        let bytes = elf(3, 183, 1);
        std::fs::write(&built, &bytes).expect("built");
        let vz = scratch.path().join("vz.noindex");

        let first = place(&built, &vz).expect("placed");
        assert!(first.fresh);
        assert_eq!(first.sha256, sha256_hex(&bytes));
        assert_eq!(first.sha256.len(), 64);
        assert_eq!(
            first.path,
            vz.join("agent").join(&first.sha256).join("aterm-vz-agent")
        );
        assert_eq!(first.bytes, bytes.len() as u64);
        assert_eq!(std::fs::read(&first.path).expect("read"), bytes);
        let mode = std::fs::metadata(&first.path)
            .expect("meta")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o555);
        let names: Vec<String> = std::fs::read_dir(vz.join("agent"))
            .expect("agents")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            std::slice::from_ref(&first.sha256),
            "no partial left"
        );

        let again = place(&built, &vz).expect("again");
        assert!(!again.fresh);
        assert_eq!(again.path, first.path);

        // A second agent goes beside the first: a running guest's agent is never overwritten.
        let other = elf(2, 183, 1);
        std::fs::write(&built, &other).expect("rebuilt");
        let second = place(&built, &vz).expect("second");
        assert!(second.fresh);
        assert_ne!(second.sha256, first.sha256);
        assert_eq!(std::fs::read(&first.path).expect("first kept"), bytes);
    }

    #[test]
    fn a_hash_directory_that_does_not_hold_its_agent_is_replaced() {
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let built = scratch.path().join("built");
        let bytes = elf(3, 183, 1);
        std::fs::write(&built, &bytes).expect("built");
        let vz = scratch.path().join("vz.noindex");
        let dir = vz.join("agent").join(sha256_hex(&bytes));
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("aterm-vz-agent"), b"cut short").expect("stale");

        let placed = place(&built, &vz).expect("placed");
        assert!(placed.fresh);
        assert_eq!(std::fs::read(&placed.path).expect("read"), bytes);
    }

    /// A concurrent build that places the same agent while this one writes its copy wins,
    /// whether it lands before this one looks or between the look and the rename: its copy
    /// (which a guest may already run) is kept, never removed or renamed over, and this
    /// one's partial goes.
    #[test]
    fn a_copy_another_build_placed_meanwhile_is_kept() {
        use std::os::unix::fs::MetadataExt as _;
        for race in [Race::BeforeLook, Race::BeforeRename] {
            let scratch = aterm_tempfile::tempdir().expect("scratch");
            let built = scratch.path().join("built");
            let bytes = elf(3, 183, 1);
            std::fs::write(&built, &bytes).expect("built");
            let vz = scratch.path().join("vz.noindex");
            let mut theirs = None;
            let placed = place_racing(&built, &vz, &mut |at, dir| {
                if at == race {
                    std::fs::create_dir(dir).expect("their directory");
                    let agent = dir.join(AGENT);
                    std::fs::write(&agent, &bytes).expect("their copy");
                    theirs = Some(std::fs::metadata(&agent).expect("meta").ino());
                }
            })
            .unwrap_or_else(|why| panic!("{race:?}: {why}"));
            assert!(!placed.fresh, "{race:?}");
            let kept = std::fs::metadata(&placed.path).expect("kept").ino();
            assert_eq!(Some(kept), theirs, "{race:?}: their copy, not this one's");
            let names: Vec<String> = std::fs::read_dir(vz.join("agent"))
                .expect("agents")
                .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
                .collect();
            assert_eq!(
                names,
                std::slice::from_ref(&placed.sha256),
                "{race:?}: no partial left"
            );
        }
    }

    /// The negative control: a directory that appears between the look and the rename
    /// without this agent in it is a failure, never a placing.
    #[test]
    fn a_rename_lost_to_a_directory_without_the_agent_fails() {
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let built = scratch.path().join("built");
        std::fs::write(&built, elf(3, 183, 1)).expect("built");
        let vz = scratch.path().join("vz.noindex");
        let why = place_racing(&built, &vz, &mut |at, dir| {
            if at == Race::BeforeRename {
                std::fs::create_dir(dir).expect("a directory");
                std::fs::write(dir.join(AGENT), b"another agent").expect("another");
            }
        })
        .expect_err("not this agent");
        assert!(why.contains("could not place"), "{why}");
    }

    #[test]
    fn a_build_that_is_not_a_guest_agent_is_never_placed() {
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let built = scratch.path().join("built");
        std::fs::write(&built, elf(3, 183, 3)).expect("built");
        let vz = scratch.path().join("vz.noindex");
        let why = place(&built, &vz).expect_err("dynamic");
        assert!(why.contains("dynamically linked"), "{why}");
        assert!(!vz.exists());
        let why = place(&scratch.path().join("absent"), &vz).expect_err("absent");
        assert!(why.contains("could not read"), "{why}");
    }
}
