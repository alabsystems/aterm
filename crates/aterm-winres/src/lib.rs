// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Compile a Win32 resource script and link it into the calling package's
//! binaries — the build-script step behind the shipped exe's icon, side-by-side
//! manifest and `VS_VERSION_INFO` block.
//!
//! ## Why this is first-party
//!
//! `embed-resource` pulled `vswhom`/`vswhom-sys` on Windows-msvc hosts, which
//! need `libc::wchar_t`/`wcslen`; `crates/aterm-libc` declares only the
//! `core::ffi` primitives on Windows by design (`crates/aterm-libc-oracle`'s cell table).
//! Build-dependencies compile for the HOST, so a cross build from a Mac never
//! sees that graph (`verify cells`' `win` cell passes right through it):
//! only a native Windows-msvc build breaks. Declaring `wchar_t`/`wcslen` in
//! aterm-libc would trade its design for one build helper; dropping the helper
//! cost nothing the product uses.
//!
//! ## What it does
//!
//! Exactly the three things a build script needs: pick a resource compiler for
//! the TARGET (never the host — build scripts run on the host), run it on the
//! `.rc`, and print `cargo:rustc-link-arg-bins=<artifact>` so the artifact
//! reaches every `[[bin]]` of the calling package — and only those.
//! `rc.exe` and `llvm-rc` produce a `.res` the MSVC and LLD linkers take on the
//! command line as-is; `windres` produces a COFF object the GNU linker takes the
//! same way.
//!
//! ## What it does not do
//!
//! No registry reads, no `vswhere`, no Visual Studio discovery. `rc.exe` is
//! found on `PATH` (a Developer prompt / `vcvars64`, which is how
//! `apps/aterm-win/build.ps1` runs), through the `WindowsSdkVerBinPath` variable
//! `vcvars` exports, or by scanning the documented Windows Kits install root for
//! the newest SDK. `ATERM_RC` names a compiler explicitly when none of those
//! apply. A machine with no resource compiler gets an [`Outcome`] the caller
//! turns into a warning or an error (`crates/aterm/build.rs`), never a silent
//! no-op.
#![allow(
    clippy::disallowed_types,
    reason = "never linked into the shipped binary: a build-script step"
)]

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// What happened to the resource script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The resource compiled and the link argument was emitted for this
    /// package's bins. Carries the compiled artifact's path.
    Linked(PathBuf),
    /// The TARGET is not Windows: nothing was compiled and nothing was emitted,
    /// so the binary is byte-identical to one built without the step.
    NotWindows,
    /// No resource compiler could be found; the message says where it looked.
    NoCompiler(String),
    /// A compiler was found and ran, and failed; the message carries its
    /// exit status and stderr.
    Failed(String),
}

impl Outcome {
    /// `true` only when the resources are actually in the binary.
    #[must_use]
    pub const fn is_linked(&self) -> bool {
        matches!(self, Outcome::Linked(_))
    }

    /// `true` when resource compilation failed or no compiler was found.
    #[must_use]
    pub const fn is_failure(&self) -> bool {
        matches!(self, Outcome::NoCompiler(_) | Outcome::Failed(_))
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Outcome::Linked(p) => write!(f, "linked {}", p.display()),
            Outcome::NotWindows => f.write_str("target is not Windows; no resources apply"),
            Outcome::NoCompiler(why) => write!(f, "no resource compiler found: {why}"),
            Outcome::Failed(why) => write!(f, "resource compiler failed: {why}"),
        }
    }
}

/// Which command-line dialect a resource compiler speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dialect {
    /// Microsoft `rc.exe`: `/nologo /I dir /fo out.res in.rc`.
    Rc,
    /// LLVM's `llvm-rc`: `/I dir /FO out.res in.rc`.
    LlvmRc,
    /// GNU `windres`: `-I dir -i in.rc -o out.o -O coff`.
    Windres,
}

impl Dialect {
    /// The artifact extension the dialect produces.
    const fn artifact_extension(self) -> &'static str {
        match self {
            Dialect::Rc | Dialect::LlvmRc => "res",
            Dialect::Windres => "o",
        }
    }
}

/// Encode a Win32 `VS_FIXEDFILEINFO` four-part version (`major`, `minor`, `patch`, `build`) into 16 LE bytes.
#[must_use]
pub const fn encode_winres_version_le(major: u32, minor: u32, patch: u32, build: u32) -> [u8; 16] {
    [
        major as u8,
        (major >> 8) as u8,
        (major >> 16) as u8,
        (major >> 24) as u8,
        minor as u8,
        (minor >> 8) as u8,
        (minor >> 16) as u8,
        (minor >> 24) as u8,
        patch as u8,
        (patch >> 8) as u8,
        (patch >> 16) as u8,
        (patch >> 24) as u8,
        build as u8,
        (build >> 8) as u8,
        (build >> 16) as u8,
        (build >> 24) as u8,
    ]
}

/// Decode 16 LE bytes into a Win32 `VS_FIXEDFILEINFO` four-part version (`major`, `minor`, `patch`, `build`).
#[must_use]
pub const fn decode_winres_version_le(bytes: [u8; 16]) -> (u32, u32, u32, u32) {
    let major = (bytes[0] as u32)
        | ((bytes[1] as u32) << 8)
        | ((bytes[2] as u32) << 16)
        | ((bytes[3] as u32) << 24);
    let minor = (bytes[4] as u32)
        | ((bytes[5] as u32) << 8)
        | ((bytes[6] as u32) << 16)
        | ((bytes[7] as u32) << 24);
    let patch = (bytes[8] as u32)
        | ((bytes[9] as u32) << 8)
        | ((bytes[10] as u32) << 16)
        | ((bytes[11] as u32) << 24);
    let build = (bytes[12] as u32)
        | ((bytes[13] as u32) << 8)
        | ((bytes[14] as u32) << 16)
        | ((bytes[15] as u32) << 24);
    (major, minor, patch, build)
}

/// Compile `rc` for the TARGET and link the result into this package's bins.
///
/// `rc` may be relative, in which case it is resolved against
/// `CARGO_MANIFEST_DIR` — the directory cargo runs build scripts from. The
/// script's own directory is put on the compiler's include path, so a
/// `1 ICON "aterm.ico"` beside the `.rc` resolves wherever the compiler runs.
///
/// Emits `cargo:rerun-if-changed` for the script and
/// `cargo:rerun-if-env-changed` for `ATERM_RC` (and, when no compiler was
/// found, for the variables the search read, so installing one re-runs the
/// step); the caller still owns the rerun lines for anything the script pulls
/// in (the `.ico`, the manifest).
pub fn compile(rc: &Path) -> Outcome {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return Outcome::NotWindows;
    }
    println!("cargo:rerun-if-env-changed=ATERM_RC");

    let manifest_dir = match std::env::var_os("CARGO_MANIFEST_DIR") {
        Some(d) => PathBuf::from(d),
        None => PathBuf::new(),
    };
    let rc = if rc.is_absolute() {
        rc.to_path_buf()
    } else {
        manifest_dir.join(rc)
    };
    println!("cargo:rerun-if-changed={}", rc.display());

    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let (compiler, dialect) = match locate_compiler(&target_env, &target_arch) {
        Ok(found) => found,
        Err(why) => {
            // A miss can be fixed without a source change (a Developer prompt,
            // an SDK install), so declare the env vars the search read. Only
            // on the miss path: a hit must not re-run on every PATH change.
            for var in [
                "PATH",
                "WindowsSdkVerBinPath",
                "ProgramFiles(x86)",
                "ProgramFiles",
            ] {
                println!("cargo:rerun-if-env-changed={var}");
            }
            return Outcome::NoCompiler(why);
        }
    };

    let out_dir = match std::env::var_os("OUT_DIR") {
        Some(d) => PathBuf::from(d),
        None => return Outcome::Failed("OUT_DIR is not set (not running under cargo?)".into()),
    };
    #[allow(clippy::manual_unwrap_or)]
    let stem = match rc.file_stem() {
        Some(s) => match s.to_str() {
            Some(s) => s,
            None => "resource",
        },
        None => "resource",
    };
    let artifact = out_dir.join(format!("{stem}.{}", dialect.artifact_extension()));
    let include_dir = match rc.parent() {
        Some(p) => p.to_path_buf(),
        None => PathBuf::new(),
    };

    let mut cmd = Command::new(&compiler);
    cmd.args(arguments(dialect, &include_dir, &rc, &artifact));
    // rc.exe resolves the file names inside the script against ITS working
    // directory as well as /I; running beside the script covers both.
    if include_dir.is_dir() {
        cmd.current_dir(&include_dir);
    }
    let output = match cmd.output() {
        Ok(o) => o,
        Err(e) => {
            return Outcome::Failed(format!("could not run {}: {e}", compiler.display()));
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Outcome::Failed(format!(
            "{} exited with {}: {}{}",
            compiler.display(),
            output.status,
            stderr.trim(),
            stdout.trim()
        ));
    }
    if !artifact.is_file() {
        return Outcome::Failed(format!(
            "{} reported success but wrote no {}",
            compiler.display(),
            artifact.display()
        ));
    }

    println!("cargo:rustc-link-arg-bins={}", artifact.display());
    Outcome::Linked(artifact)
}

/// The argument vector for one dialect. Pure, so the shape is unit-tested.
fn arguments(dialect: Dialect, include_dir: &Path, rc: &Path, artifact: &Path) -> Vec<String> {
    let inc = include_dir.display().to_string();
    let rc = rc.display().to_string();
    let out = artifact.display().to_string();
    match dialect {
        Dialect::Rc => vec!["/nologo".into(), "/I".into(), inc, "/fo".into(), out, rc],
        Dialect::LlvmRc => vec!["/I".into(), inc, "/FO".into(), out, rc],
        Dialect::Windres => vec![
            "-I".into(),
            inc,
            "-i".into(),
            rc,
            "-o".into(),
            out,
            "-O".into(),
            "coff".into(),
        ],
    }
}

/// Which dialect a compiler speaks, from its file name.
fn dialect_of(program: &Path) -> Dialect {
    let name = match program.file_name() {
        Some(f) => match f.to_str() {
            Some(s) => s.to_ascii_lowercase(),
            None => String::new(),
        },
        None => String::new(),
    };
    if name.contains("windres") {
        Dialect::Windres
    } else if name.contains("llvm-rc") {
        Dialect::LlvmRc
    } else {
        Dialect::Rc
    }
}

/// Find a resource compiler for the target, in a fixed order that is stated
/// in the error when every step misses.
fn locate_compiler(target_env: &str, target_arch: &str) -> Result<(PathBuf, Dialect), String> {
    let mut looked = Vec::new();

    let gnu = target_env == "gnu";

    // 1. An explicit choice wins outright, but is held to the TARGET's
    //    linker: a `.res` handed to GNU `ld` fails later with "file format
    //    not recognized", so refuse that dialect here.
    if let Some(explicit) = std::env::var_os("ATERM_RC") {
        let p = PathBuf::from(explicit);
        let found = if p.is_file() {
            Some(p.clone())
        } else {
            find_on_path(&p)
        };
        let Some(found) = found else {
            return Err(format!(
                "ATERM_RC={} is not a file and is not on PATH",
                p.display()
            ));
        };
        let dialect = dialect_of(&found);
        if gnu && dialect != Dialect::Windres {
            return Err(format!(
                "ATERM_RC={} speaks the rc.exe/llvm-rc dialect and produces a .res, which the \
                 GNU linker of a *-windows-gnu target cannot consume; name a windres",
                found.display()
            ));
        }
        return Ok((found, dialect));
    }

    let path_candidates: &[&str] = if gnu {
        // A target-prefixed windres first: on a cross host the bare name, if
        // present at all, is the HOST's binutils and emits the wrong COFF.
        match target_arch {
            "x86_64" => &["x86_64-w64-mingw32-windres", "windres"],
            "x86" => &["i686-w64-mingw32-windres", "windres"],
            "aarch64" => &["aarch64-w64-mingw32-windres", "windres"],
            _ => &["windres"],
        }
    } else {
        &["rc", "llvm-rc"]
    };
    for name in path_candidates {
        if let Some(found) = find_on_path(Path::new(name)) {
            let dialect = dialect_of(&found);
            return Ok((found, dialect));
        }
        looked.push(format!("{name} on PATH"));
    }

    if !gnu {
        // 2. vcvars exports the versioned SDK bin directory it selected.
        if let Some(dir) = std::env::var_os("WindowsSdkVerBinPath") {
            let p = PathBuf::from(dir).join(host_sdk_arch()).join("rc.exe");
            if p.is_file() {
                return Ok((p, Dialect::Rc));
            }
            looked.push(format!("{}", p.display()));
        }
        // 3. The documented Windows Kits root, every installed SDK version,
        //    newest first: partial kits (an arch directory with no rc.exe)
        //    are common, and a partial newer kit must not hide a full older
        //    one. Both program roots are tried; x64 installs under (x86).
        for root_var in ["ProgramFiles(x86)", "ProgramFiles"] {
            let Some(root) = std::env::var_os(root_var) else {
                continue;
            };
            let bin = PathBuf::from(root)
                .join("Windows Kits")
                .join("10")
                .join("bin");
            let mut names = Vec::new();
            if let Ok(rd) = std::fs::read_dir(&bin) {
                for entry in rd {
                    if let Ok(e) = entry
                        && let Ok(s) = e.file_name().into_string()
                    {
                        names.push(s);
                    }
                }
            }
            let mut name_refs = Vec::with_capacity(names.len());
            for name in &names {
                name_refs.push(name.as_str());
            }
            let versions = sdk_versions_newest_first(name_refs);
            if versions.is_empty() {
                looked.push(format!("{}\\<10.x.y.z>", bin.display()));
            }
            for ver in versions {
                let p = bin.join(ver).join(host_sdk_arch()).join("rc.exe");
                if p.is_file() {
                    return Ok((p, Dialect::Rc));
                }
                looked.push(format!("{}", p.display()));
            }
        }
    }

    let advice = if gnu {
        "install the mingw-w64 binutils (x86_64-w64-mingw32-windres, or windres on a Windows \
         host) or set ATERM_RC to a windres"
    } else {
        "build inside a Visual Studio Developer prompt / vcvars64, install the Windows SDK \
         (rc.exe) or LLVM (llvm-rc), or set ATERM_RC to the compiler"
    };
    Err(format!(
        "looked for {}. To fix: {advice}.",
        looked.join(", ")
    ))
}

/// The SDK `bin\<version>\<arch>` subdirectory for the machine running the
/// build script. Build scripts run on the HOST, and `rc.exe` is a host tool.
fn host_sdk_arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86" => "x86",
        _ => "x64",
    }
}

/// The `10.a.b.c` directory names, newest first, compared numerically so
/// `10.0.26100.0` outranks `10.0.9600.0` — a lexical sort gets that wrong.
/// Anything that is not four numeric components starting with 10 is dropped.
fn sdk_versions_newest_first<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    let mut versions: Vec<([u64; 4], &str)> = Vec::new();
    for n in names {
        let mut split = n.split('.');
        let (Some(p0), Some(p1), Some(p2), Some(p3), None) = (
            split.next(),
            split.next(),
            split.next(),
            split.next(),
            split.next(),
        ) else {
            continue;
        };
        let (Ok(v0), Ok(v1), Ok(v2), Ok(v3)) = (
            p0.parse::<u64>(),
            p1.parse::<u64>(),
            p2.parse::<u64>(),
            p3.parse::<u64>(),
        ) else {
            continue;
        };
        if v0 == 10 {
            versions.push(([!v0, !v1, !v2, !v3], n));
        }
    }
    versions.sort_unstable();
    let mut out = Vec::with_capacity(versions.len());
    for (_, n) in versions {
        out.push(n);
    }
    out
}

/// Resolve a bare program name against `PATH`, trying the Windows executable
/// extensions when the name has none. A name with a directory component is
/// returned as-is when it exists.
#[allow(clippy::question_mark)]
fn find_on_path(program: &Path) -> Option<PathBuf> {
    if program.components().count() > 1 {
        return if program.is_file() {
            Some(program.to_path_buf())
        } else {
            None
        };
    }
    let Some(paths) = std::env::var_os("PATH") else {
        return None;
    };
    let exts: &[&str] = if cfg!(windows) {
        &["exe", "cmd", "bat"]
    } else {
        &[]
    };
    for dir in std::env::split_paths(&paths) {
        let bare = dir.join(program);
        if bare.is_file() {
            return Some(bare);
        }
        if program.extension().is_none() {
            for ext in exts {
                let with = bare.with_extension(ext);
                if with.is_file() {
                    return Some(with);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdk_versions_sort_numerically_newest_first() {
        let names = [
            "10.0.9600.0",
            "10.0.26100.0",
            "10.0.19041.0",
            "notes.txt",
            "10.0.1",
        ];
        assert_eq!(
            sdk_versions_newest_first(names),
            ["10.0.26100.0", "10.0.19041.0", "10.0.9600.0"]
        );
    }

    #[test]
    fn no_sdk_dirs_means_an_empty_list() {
        assert!(sdk_versions_newest_first(["x86", "arm64", "README"]).is_empty());
    }

    #[test]
    fn dialect_follows_the_program_name() {
        assert_eq!(dialect_of(Path::new("C:/kits/x64/rc.exe")), Dialect::Rc);
        assert_eq!(dialect_of(Path::new("/usr/bin/llvm-rc")), Dialect::LlvmRc);
        assert_eq!(dialect_of(Path::new("LLVM-RC.EXE")), Dialect::LlvmRc);
        assert_eq!(
            dialect_of(Path::new("x86_64-w64-mingw32-windres")),
            Dialect::Windres
        );
    }

    #[test]
    fn argument_shapes_per_dialect() {
        let inc = Path::new("inc");
        let rc = Path::new("inc/a.rc");
        let out = Path::new("out/a.res");
        assert_eq!(
            arguments(Dialect::Rc, inc, rc, out),
            ["/nologo", "/I", "inc", "/fo", "out/a.res", "inc/a.rc"]
        );
        assert_eq!(
            arguments(Dialect::LlvmRc, inc, rc, out),
            ["/I", "inc", "/FO", "out/a.res", "inc/a.rc"]
        );
        assert_eq!(
            arguments(Dialect::Windres, inc, rc, Path::new("out/a.o")),
            ["-I", "inc", "-i", "inc/a.rc", "-o", "out/a.o", "-O", "coff"]
        );
    }

    #[test]
    fn artifact_extension_matches_what_the_linker_accepts() {
        assert_eq!(Dialect::Rc.artifact_extension(), "res");
        assert_eq!(Dialect::LlvmRc.artifact_extension(), "res");
        assert_eq!(Dialect::Windres.artifact_extension(), "o");
    }

    #[test]
    fn outcome_display_names_the_state() {
        assert!(Outcome::NotWindows.to_string().contains("not Windows"));
        assert!(
            Outcome::NoCompiler("x".into())
                .to_string()
                .contains("no resource compiler")
        );
        assert!(Outcome::Linked(PathBuf::from("a.res")).is_linked());
        assert!(!Outcome::Failed("x".into()).is_linked());
    }
}
