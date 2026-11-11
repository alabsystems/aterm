// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `ay` certificate bundles: hand-encoded SMT-LIB2 obligations under
//! `aterm-spec-models/proofs/ay`, each declaring the verdict it must get.

use std::path::{Path, PathBuf};

const TIMEOUT: &str = "-t:120000";

/// What a solve must answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Want {
    /// The obligation is proved (its negation is unsatisfiable).
    Unsat,
    /// A control: the witness exists.
    Sat,
}

impl Want {
    const fn word(self) -> &'static str {
        match self {
            Want::Unsat => "unsat",
            Want::Sat => "sat",
        }
    }
}

/// One obligation and what it declares.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Obligation {
    pub(crate) path: PathBuf,
    pub(crate) want: Want,
    /// Minutes-class: solved only by `--full`, with no timeout.
    pub(crate) minutes: bool,
}

/// What an obligation's text declares: its verdict, and whether it is
/// minutes-class.
pub(crate) fn declared(text: &str) -> Result<(Want, bool), String> {
    let mut want = None;
    let mut minutes = false;
    for line in text.lines().map(str::trim) {
        let Some(info) = line
            .strip_prefix("(set-info ")
            .and_then(|rest| rest.split(')').next())
        else {
            continue;
        };
        match info.split_whitespace().collect::<Vec<_>>().as_slice() {
            [":status", "unsat"] => want = Some(Want::Unsat),
            [":status", "sat"] => want = Some(Want::Sat),
            [":status", other] => return Err(format!("declares `:status {other}`")),
            [":aterm-cost", "minutes"] => minutes = true,
            [":aterm-cost", other] => return Err(format!("declares `:aterm-cost {other}`")),
            _ => {}
        }
    }
    want.map(|want| (want, minutes))
        .ok_or_else(|| "declares no `(set-info :status unsat|sat)`".to_string())
}

/// Every obligation under `root`, sorted by path, or why a file declares
/// nothing it can be held to.
pub(crate) fn obligations(root: &Path) -> (Vec<Obligation>, Vec<String>) {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let ft = entry.file_type()?;
            let path = entry.path();
            if ft.is_dir() {
                walk(&path, out)?;
            } else if (ft.is_file() || (ft.is_symlink() && path.is_file()))
                && path.extension().is_some_and(|e| e == "smt2")
            {
                out.push(path);
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    if let Err(e) = walk(root, &mut files) {
        return (Vec::new(), vec![format!("{}: {e}", root.display())]);
    }
    files.sort();
    if files.is_empty() {
        return (
            Vec::new(),
            vec![format!("{}: no .smt2 obligation was found", root.display())],
        );
    }
    let (mut found, mut faults) = (Vec::new(), Vec::new());
    for path in files {
        match read_smt2_text(&path).and_then(|text| declared(&text)) {
            Ok((want, minutes)) => found.push(Obligation {
                path,
                want,
                minutes,
            }),
            Err(why) => faults.push(format!("{}: {why}", path.display())),
        }
    }
    (found, faults)
}

fn read_smt2_text(path: &Path) -> Result<String, String> {
    use std::io::Read as _;
    const MAX_SMT2_BYTES: u64 = 32 << 20;
    let go = || -> std::io::Result<String> {
        let mut opts = std::fs::OpenOptions::new();
        opts.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
        }
        let file = opts.open(path)?;
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        }
        if meta.len() > MAX_SMT2_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "file exceeds 32 MiB limit",
            ));
        }
        let cap = usize::try_from(meta.len().min(MAX_SMT2_BYTES).saturating_add(1)).unwrap_or(0);
        let mut buf = Vec::with_capacity(cap);
        file.take(MAX_SMT2_BYTES + 1).read_to_end(&mut buf)?;
        if buf.len() as u64 > MAX_SMT2_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "file grew past 32 MiB limit while reading",
            ));
        }
        String::from_utf8(buf).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    };
    go().map_err(|e| e.to_string())
}

/// One solve's outcome: what it answered and which build answered it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Answer {
    pub(crate) verdict: String,
    pub(crate) stamp: Option<String>,
}

/// The verdict on `ay solve`'s stdout: its first line that is exactly `sat`,
/// `unsat` or `unknown` (a CHC certificate and the session comments follow).
pub(crate) fn verdict(stdout: &str) -> String {
    stdout
        .lines()
        .map(str::trim)
        .find(|line| {
            ["sat", "unsat", "unknown"]
                .iter()
                .any(|word| line.eq_ignore_ascii_case(word))
        })
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// The build that answered, from the `build.stamp=` the solver writes to
/// stderr on every solve.
pub(crate) fn stamp(stderr: &str) -> Option<String> {
    stderr.split_whitespace().find_map(|word| {
        word.strip_prefix("build.stamp=")
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    })
}

// Skip: spawns the solver. Verification tooling.
/// Solve one obligation.
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn solve(ay: &Path, obligation: &Obligation) -> Result<Answer, String> {
    let mut cmd = aterm_uds::spawnfd::command(ay);
    cmd.arg("solve").arg("--no-proof").arg(&obligation.path);
    if !obligation.minutes {
        cmd.arg(TIMEOUT);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("could not run {}: {e}", ay.display()))?;
    if !out.status.success() {
        return Err(format!(
            "{} failed ({}); its printed verdict is not a discharge\n--- stdout ---\n{}\n--- stderr ---\n{}",
            ay.display(),
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        ));
    }
    Ok(Answer {
        verdict: verdict(&String::from_utf8_lossy(&out.stdout)),
        stamp: stamp(&String::from_utf8_lossy(&out.stderr)),
    })
}

/// Judge one obligation's answer: the declared verdict, from an attributed
/// build.
pub(crate) fn judge(file: &str, want: Want, answer: &Answer) -> Result<(), String> {
    let Some(build) = &answer.stamp else {
        return Err(format!(
            "{file}: got `{}` with no build.stamp — the solver did not say which build answered",
            answer.verdict
        ));
    };
    if answer.verdict == want.word() {
        Ok(())
    } else {
        Err(format!(
            "{file}: got `{}`, want `{}` [ay {build}]",
            answer.verdict,
            want.word()
        ))
    }
}

#[cfg(test)]
#[path = "ay_tests.rs"]
mod tests;
