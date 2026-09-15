// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Compile a Win32 resource script and link it into the calling package's
//! binaries — the build-script step behind the shipped exe's icon, side-by-side
//! manifest and `VS_VERSION_INFO` block.
//!
//! ## Why this is first-party
//!
//! `embed-resource` did this job until 2026-09-14, and it stopped compiling on
//! the one lane that matters — the native `x86_64-pc-windows-msvc` build — the
//! day `libc` became first-party (`c4441a9ca`, 2026-08-30). `embed-resource`
//! pulls `vswhom` (and `vswhom-sys`) only under
//! `cfg(all(target_os = "windows", target_env = "msvc"))`, to locate a Visual
//! Studio install when `rc.exe` is not on `PATH`, and those two crates reach
//! into `libc::wchar_t` (vswhom-sys) and `libc::wcslen` (vswhom).
//! `crates/aterm-libc` declares NO target-specific ABI on Windows on purpose —
//! the Windows cell is the 14 `core::ffi` primitives and nothing else, and "a
//! target consumer that starts using a non-primitive item fails closed when
//! compiled" is the documented contract (`libc-oracle/gen/README.md`).
//!
//! No cross build from a Mac could ever see this, and the reason is not the
//! target triple: `embed-resource` is a BUILD-dependency, build-dependencies
//! compile for the HOST, and that `cfg` is evaluated against the host — so on
//! an Apple host `vswhom` is simply not in the graph, whatever `--target` says
//! (`xtask gate cells --cell win` type-checks `-p aterm` for the msvc triple
//! and passes right through it). Only a native Windows-msvc host compiles
//! `vswhom`, and nobody built there between 2026-08-29 and 2026-09-14, so the
//! shipped Windows binary was unbuildable for two weeks without a single gate
//! going red. Declaring `wchar_t` and `wcslen` for Windows would have traded
//! the crate's stated design for one third-party build helper; deleting the
//! helper costs nothing the product uses.
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
//! apply. A machine with no resource compiler gets an honest [`Outcome`] the
//! caller turns into a warning or a hard error — the same warn-don't-fail
//! posture `crates/aterm/build.rs` documents — never a silent no-op.

use std::ffi::OsStr;
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
    pub fn is_linked(&self) -> bool {
        matches!(self, Outcome::Linked(_))
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
    fn artifact_extension(self) -> &'static str {
        match self {
            Dialect::Rc | Dialect::LlvmRc => "res",
            Dialect::Windres => "o",
        }
    }
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

    let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_default();
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
            // A miss is a fact about the BOX, and the box changes without the
            // source changing: the next build may run inside a Developer
            // prompt, or after the SDK was installed. Cargo fingerprints only
            // the env vars a build script declares, so declare the ones the
            // search read — on the miss path only, so a build that FOUND its
            // compiler is not re-run by every PATH difference between shells.
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
    let stem = rc.file_stem().and_then(OsStr::to_str).unwrap_or("resource");
    let artifact = out_dir.join(format!("{stem}.{}", dialect.artifact_extension()));
    let include_dir = rc.parent().map(Path::to_path_buf).unwrap_or_default();

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
    let name = program
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
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

    // 1. An explicit choice wins outright — including on a box where the
    //    heuristics below would also have found something. It is still held
    //    to the TARGET's linker: a `.res` handed to GNU `ld` fails at link
    //    time with "file format not recognized", a failure no `Outcome` could
    //    explain, so a dialect the target cannot consume is refused here.
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
        // 3. The documented Windows Kits root, EVERY installed SDK version,
        //    newest first. Not just the newest: a version directory without
        //    rc.exe is a real layout, not a hypothetical — this x64 box carries
        //    10.0.14393.0, 10.0.15063.0, 10.0.16299.0 and 10.0.17134.0, each
        //    with an `x64\` subdirectory and no rc.exe in it, beside the one
        //    complete 10.0.26100.0 — and a partial NEWER kit (debuggers or
        //    signing tools only) beside a full older SDK would otherwise hide
        //    the compiler one directory over. Both program roots are tried;
        //    on this x64 box the SDK lives under the (x86) one.
        for root_var in ["ProgramFiles(x86)", "ProgramFiles"] {
            let Some(root) = std::env::var_os(root_var) else {
                continue;
            };
            let bin = PathBuf::from(root)
                .join("Windows Kits")
                .join("10")
                .join("bin");
            let names = std::fs::read_dir(&bin)
                .map(|rd| {
                    rd.filter_map(Result::ok)
                        .filter_map(|e| e.file_name().into_string().ok())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let versions = sdk_versions_newest_first(names.iter().map(String::as_str));
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
    let mut versions: Vec<(Vec<u64>, &str)> = names
        .into_iter()
        .filter_map(|n| {
            let parts: Vec<u64> = n
                .split('.')
                .map(str::parse)
                .collect::<Result<_, _>>()
                .ok()?;
            (parts.len() == 4 && parts[0] == 10).then_some((parts, n))
        })
        .collect();
    versions.sort_by(|a, b| b.0.cmp(&a.0));
    versions.into_iter().map(|(_, n)| n).collect()
}

/// Resolve a bare program name against `PATH`, trying the Windows executable
/// extensions when the name has none. A name with a directory component is
/// returned as-is when it exists.
fn find_on_path(program: &Path) -> Option<PathBuf> {
    if program.components().count() > 1 {
        return program.is_file().then(|| program.to_path_buf());
    }
    let paths = std::env::var_os("PATH")?;
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
