// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Lab sleep clock injection ([`crate::sleep`], [`Layout`],
//! [`crate::protected::SCRATCH_DIRS`], [`vars`], [`crate::lab::instance_vars`],
//! [`is_clock_var`]).

use std::fs;
use std::path::{Path, PathBuf};

use aterm_uds::spawnfd::Stdio;

use crate::pyjson::J;
use crate::run::Run;
use crate::sys::{EntryKind, PutSrc};

/// The library's file name, as its build names it.
const LIB_NAME: &str = "libaterm_lab_clock.dylib";

/// The signature flag of a hardened runtime (`CS_RUNTIME`).
const CS_RUNTIME: u32 = 0x1_0000;

/// Where a lab's clock files sit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Layout {
    /// `runs/<tag>/clock/`.
    pub dir: PathBuf,
    /// The library the lab's processes load.
    pub lib: PathBuf,
    /// The offset file.
    pub offset: PathBuf,
    /// The ad hoc signed clone of Claude Code, when one was needed.
    pub claude: PathBuf,
    /// The record directory (`scratch/clock/`).
    pub records: PathBuf,
}

impl Layout {
    /// The layout of lab `run`.
    #[must_use]
    pub(crate) fn of(run: &Run) -> Layout {
        let dir = run.run.join("clock");
        Layout {
            lib: dir.join(LIB_NAME),
            offset: dir.join("offset"),
            claude: dir.join("claude"),
            records: run.w.join("clock"),
            dir,
        }
    }
}

/// Whether `name` is a variable the clock sets: `LAB_CLOCK=1` alone hands it.
#[must_use]
pub(crate) fn is_clock_var(name: &str) -> bool {
    name == "DYLD_INSERT_LIBRARIES" || name.starts_with("LABCLOCK_")
}

/// The variables that load the clock (none unless the lab is up with it).
#[must_use]
pub(crate) fn vars(run: &Run) -> Vec<(String, String)> {
    if run.clock != "1" {
        return Vec::new();
    }
    let l = Layout::of(run);
    let s = |p: &Path| p.to_string_lossy().into_owned();
    vec![
        ("DYLD_INSERT_LIBRARIES".into(), s(&l.lib)),
        ("LABCLOCK_FILE".into(), s(&l.offset)),
        ("LABCLOCK_DIR".into(), s(&l.records)),
        ("LABCLOCK_CONTINUOUS".into(), "1".into()),
    ]
}

/// Whether `codesign -dv`'s text says a hardened runtime (`flags=0x10000(runtime)`).
#[must_use]
pub(crate) fn hardened(codesign_dv: &str) -> bool {
    codesign_dv
        .split_whitespace()
        .filter_map(|w| w.strip_prefix("flags=0x"))
        .filter_map(|h| u32::from_str_radix(h.split('(').next().unwrap_or(h), 16).ok())
        .any(|f| f & CS_RUNTIME != 0)
}

/// The library a build made, from its `--message-format json` lines.
#[must_use]
pub(crate) fn artifact_of(json_lines: &str) -> Option<PathBuf> {
    json_lines.lines().find_map(|line| {
        let j = J::parse(line).ok()?;
        let target = j.get("target")?;
        let ours = j.get("reason")?.as_str() == Some("compiler-artifact")
            && target.get("name")?.as_str() == Some("aterm_lab_clock")
            && target
                .get("kind")?
                .as_arr()?
                .iter()
                .any(|k| k.as_str() == Some("cdylib"));
        if !ours {
            return None;
        }
        j.get("filenames")?
            .as_arr()?
            .iter()
            .filter_map(J::as_str)
            .find(|f| f.ends_with(".dylib"))
            .map(PathBuf::from)
    })
}

/// Build the library from checkout `repo` with the `targo` that built this program.
///
/// # Errors
/// No `targo` is known, the build failed, or it named no library.
fn build_library(repo: &Path) -> Result<PathBuf, String> {
    let targo = option_env!("CARGO")
        .ok_or("the lab was built by no cargo it can name, so it cannot build its clock")?;
    let out = aterm_uds::spawnfd::command(targo)
        .args([
            "--unverified",
            "build",
            "--offline",
            "--lib",
            "-p",
            "aterm-lab-clock",
            "--message-format",
            "json-render-diagnostics",
            "--manifest-path",
        ])
        .arg(repo.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(repo.join("target/lab-clock.noindex"))
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_BUILD_TARGET_DIR")
        .env_remove("CARGO_BUILD_BUILD_DIR")
        .current_dir(repo)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {targo}: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = err.lines().rev().take(12).collect();
        return Err(format!(
            "the clock library did not build ({targo} --unverified build -p aterm-lab-clock --lib):\n{}",
            tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        ));
    }
    artifact_of(&String::from_utf8_lossy(&out.stdout))
        .ok_or_else(|| "the clock library's build named no library".to_string())
}

/// `codesign -dv` of `bin`: what it printed.
fn codesign_dv(bin: &Path) -> Result<String, String> {
    let out = aterm_uds::spawnfd::command("/usr/bin/codesign")
        .arg("-dv")
        .arg(bin)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("codesign: {e}"))?;
    Ok(format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    ))
}

/// Lay lab `run`'s clock at `up`, before its jail is rendered.
///
/// # Errors
/// Build, file write, or ad-hoc code-signing failure.
pub(crate) fn lay(run: &mut Run, repo: &Path) -> Result<(), String> {
    let l = Layout::of(run);
    fs::create_dir_all(&l.dir).map_err(|e| format!("cannot make {}: {e}", l.dir.display()))?;
    fs::create_dir_all(&l.records)
        .map_err(|e| format!("cannot make {}: {e}", l.records.display()))?;
    let built = build_library(repo)?;
    let built = fs::canonicalize(&built).unwrap_or(built);
    crate::sys::put(&l.lib, 0o755, &PutSrc::File(&built))
        .map_err(|e| format!("cannot lay {}: {e}", l.lib.display()))?;
    write_offset(run, 0)?;
    if hardened(&codesign_dv(Path::new(&run.claude_bin))?) {
        crate::sys::put(&l.claude, 0o755, &PutSrc::File(Path::new(&run.claude_bin)))
            .map_err(|e| format!("cannot clone Claude Code into {}: {e}", l.claude.display()))?;
        let signed = aterm_uds::spawnfd::command("/usr/bin/codesign")
            .args(["--force", "--sign", "-"])
            .arg(&l.claude)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("codesign: {e}"))?;
        if !signed.status.success() || hardened(&codesign_dv(&l.claude)?) {
            return Err(format!(
                "the clone of Claude Code at {} could not be signed ad hoc: {}",
                l.claude.display(),
                String::from_utf8_lossy(&signed.stderr).trim()
            ));
        }
        run.claude_bin = l.claude.to_string_lossy().into_owned();
    }
    run.clock = "1".into();
    Ok(())
}

/// The offset file's text for `secs`.
#[must_use]
pub(crate) fn offset_text(secs: i64) -> String {
    format!("{secs}\n")
}

/// The offset the lab wrote, in whole seconds.
#[must_use]
pub(crate) fn parse_offset(text: &str) -> Option<i64> {
    text.trim().parse().ok()
}

/// Lab `run`'s offset now.
///
/// # Errors
/// The file is missing, or holds no offset the lab writes.
pub(crate) fn read_offset(run: &Run) -> Result<i64, String> {
    let f = Layout::of(run).offset;
    let text = fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()))?;
    parse_offset(&text).ok_or_else(|| format!("{} holds no offset: {text:?}", f.display()))
}

/// Set lab `run`'s offset to `secs`, by a rename.
///
/// # Errors
/// The write failed.
pub(crate) fn write_offset(run: &Run, secs: i64) -> Result<(), String> {
    let f = Layout::of(run).offset;
    crate::util::atomic_write(&f, offset_text(secs).as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", f.display()))
}

/// What a process's record says (the library's `model::render`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Record {
    /// Its pid.
    pub pid: i32,
    /// Its parent's.
    pub ppid: i32,
    /// Its executable.
    pub exe: String,
    /// The offset it last took, in nanoseconds.
    pub offset_ns: i128,
}

impl Record {
    /// Whether it took an offset of `secs`.
    #[must_use]
    pub(crate) fn at(&self, secs: i64) -> bool {
        self.offset_ns == i128::from(secs) * 1_000_000_000
    }

    /// Its executable's name.
    #[must_use]
    pub(crate) fn name(&self) -> &str {
        self.exe.rsplit('/').next().unwrap_or(&self.exe)
    }
}

/// A record's text read: `None` when its first two lines are not a record's.
#[must_use]
pub(crate) fn parse_record(text: &str) -> Option<Record> {
    let mut lines = text.lines();
    let head = lines.next()?;
    let (ids, exe) = head.split_once(" exe=")?;
    let word = |line: &str, key: &str| {
        line.split_whitespace()
            .find_map(|w| w.strip_prefix(key)?.strip_prefix('=').map(str::to_string))
    };
    let pid = word(ids, "pid")?.parse().ok()?;
    let ppid = word(ids, "ppid")?.parse().ok()?;
    let offset_ns = word(lines.next()?, "offset_ns")?.parse().ok()?;
    Some(Record {
        pid,
        ppid,
        exe: exe.to_string(),
        offset_ns,
    })
}

/// Every record of lab `run`'s processes, read through no link.
#[must_use]
pub(crate) fn records(run: &Run) -> Vec<Record> {
    let dir = Layout::of(run).records;
    let Ok(Some(entries)) = crate::sys::scratch_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<Record> = entries
        .into_iter()
        .filter(|(name, kind)| *kind == EntryKind::File && name.to_string_lossy().ends_with(".txt"))
        .filter_map(|(name, _)| {
            let b = crate::sys::scratch_read(&dir.join(name)).ok()??;
            parse_record(&String::from_utf8_lossy(&b))
        })
        .collect();
    out.sort_by_key(|r| r.pid);
    out
}

/// Keep lab `run`'s offset and records in `to` (`down`).
pub(crate) fn keep(run: &Run, to: &Path) {
    if run.clock != "1" {
        return;
    }
    let l = Layout::of(run);
    let _ = fs::create_dir_all(to);
    crate::util::keep_file(&l.offset, &to.join("offset"));
    crate::util::keep_tree(&l.records, &to.join("records"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lab(clock: &str) -> Run {
        Run {
            tag: "c1".into(),
            run: "/s/runs/c1".into(),
            w: "/s/runs/c1/scratch".into(),
            clock: clock.into(),
            ..Run::default()
        }
    }

    /// THE CLOCK'S VARIABLES ARE ITS LAB'S: a lab up with it loads the
    /// library from its run directory, reads its offset there, and records
    /// in its scratch (the one place a jailed process writes); a lab without
    /// it hands none.
    #[test]
    fn a_lab_up_with_the_clock_hands_its_own_files_and_one_without_it_none() {
        assert_eq!(
            vars(&lab("1")),
            [
                (
                    "DYLD_INSERT_LIBRARIES",
                    "/s/runs/c1/clock/libaterm_lab_clock.dylib"
                ),
                ("LABCLOCK_FILE", "/s/runs/c1/clock/offset"),
                ("LABCLOCK_DIR", "/s/runs/c1/scratch/clock"),
                ("LABCLOCK_CONTINUOUS", "1"),
            ]
            .map(|(k, v)| (k.to_string(), v.to_string()))
        );
        assert!(vars(&lab("0")).is_empty() && vars(&lab("")).is_empty());
        for (k, _) in vars(&lab("1")) {
            assert!(is_clock_var(&k), "{k}");
        }
        assert!(!is_clock_var("DYLD_LIBRARY_PATH") && !is_clock_var("TMPDIR"));
    }

    /// A HARDENED RUNTIME IS READ OFF ITS FLAG (Claude Code 2.1.303 as the
    /// store holds it; the ad hoc clone and a linker-signed build have none).
    #[test]
    fn a_hardened_runtime_is_read_off_the_signature_flags() {
        let store = "Executable=/S/claude\nIdentifier=com.anthropic.claude-code\nFormat=Mach-O thin (arm64)\n\
                     CodeDirectory v=20500 size=522693 flags=0x10000(runtime) hashes=16323+7 location=embedded\n";
        assert!(hardened(store));
        assert!(hardened(
            "CodeDirectory v=20500 flags=0x12000(library-validation,runtime) x"
        ));
        assert!(!hardened(
            "CodeDirectory v=20400 size=522514 flags=0x2(adhoc) hashes=16323+2"
        ));
        assert!(!hardened(
            "CodeDirectory v=20400 flags=0x20002(adhoc,linker-signed) hashes=66192+0"
        ));
        assert!(!hardened("code object is not signed at all"));
    }

    /// THE BUILD'S LIBRARY IS ITS CDYLIB'S `.dylib`, whatever else it printed.
    #[test]
    fn the_build_names_its_library_in_its_artifact_line() {
        let lines = concat!(
            r#"{"reason":"compiler-artifact","target":{"kind":["bin"],"name":"lab-clock-probe"},"filenames":["/t/debug/lab-clock-probe"]}"#,
            "\n",
            r#"{"reason":"compiler-artifact","target":{"kind":["cdylib"],"crate_types":["cdylib"],"name":"aterm_lab_clock"},"filenames":["/t/lab-clock/debug/libaterm_lab_clock.dylib"]}"#,
            "\n",
            r#"{"reason":"build-finished","success":true}"#,
            "\n"
        );
        assert_eq!(
            artifact_of(lines),
            Some(PathBuf::from("/t/lab-clock/debug/libaterm_lab_clock.dylib"))
        );
        assert_eq!(
            artifact_of(r#"{"reason":"build-finished","success":true}"#),
            None
        );
        assert_eq!(artifact_of("not json\n"), None);
    }

    /// A RECORD SAYS ITS PROCESS AND THE OFFSET IT TOOK (`model::render`'s
    /// text, an executable path with a space in it); the offset file holds
    /// whole seconds.
    #[test]
    fn a_record_and_the_offset_read_back() {
        let text = "pid=95226 ppid=1 exe=/lab/bin dir/aterm\n\
                    offset_ns=1800000000000 continuous=1 file_reads=2\n\
                    gettimeofday=0 time=3 mach_continuous_time=19 mach_continuous_approximate_time=0\n\
                    clock 0: clock_gettime=11698 clock_gettime_nsec_np=0\n";
        let r = parse_record(text).expect("a record");
        assert_eq!(
            (r.pid, r.ppid, r.exe.as_str(), r.name()),
            (95226, 1, "/lab/bin dir/aterm", "aterm")
        );
        assert!(r.at(1800) && !r.at(0));
        assert_eq!(parse_record("pid=1 ppid=0\n"), None);
        assert_eq!(parse_record("pid=x ppid=0 exe=/a\noffset_ns=0\n"), None);
        assert_eq!(parse_offset(&offset_text(1800)), Some(1800));
        assert_eq!(parse_offset(&offset_text(0)), Some(0));
        assert_eq!(parse_offset("1800.5\n"), None);
    }
}
