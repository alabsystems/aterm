// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Paired lab-runner comparisons against one shared debug app.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{count, take_value};
use crate::{Fail, Res};

#[derive(Clone, Copy, Debug)]
enum Scenario {
    Warmup,
    Ordinary,
    PreRefusal,
    PostRefusal,
    SpaceWorld,
    NativeOnly,
}

impl Scenario {
    fn name(self) -> &'static str {
        match self {
            Self::Warmup => "warmup",
            Self::Ordinary => "ordinary",
            Self::PreRefusal => "pre-refusal",
            Self::PostRefusal => "post-refusal",
            Self::SpaceWorld => "space-world",
            Self::NativeOnly => "native-only",
        }
    }

    fn valid(self, code: Option<i32>, image: Option<(u32, u32)>, refused: bool) -> bool {
        let refused_exit = code == Some(1);
        match self {
            Self::PreRefusal => refused_exit && refused && image.is_none(),
            Self::PostRefusal => refused_exit && refused && image.is_some(),
            Self::Warmup | Self::Ordinary | Self::SpaceWorld | Self::NativeOnly => {
                code == Some(0) && image.is_some()
            }
        }
    }

    fn commands(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::PreRefusal => &[("--pre", "settings open"), ("--pre", "spawn split=h")],
            Self::PostRefusal => &[("--post", "settings open"), ("--post", "spawn split=h")],
            Self::NativeOnly => &[
                ("--pre", "settings open"),
                ("--pre", "tab close 1"),
                ("--pre", "sessions"),
            ],
            Self::Warmup | Self::Ordinary | Self::SpaceWorld => &[],
        }
    }
}

fn control_refused(stdout: &str, verb: &str) -> bool {
    let prefix = format!("ctl {verb} -> ");
    stdout
        .lines()
        .filter_map(|line| line.strip_prefix(&prefix))
        .any(|reply| {
            reply
                .strip_prefix("aterm-ctl: ")
                .unwrap_or(reply)
                .starts_with("ERR ")
        })
}

fn record_row(
    records: &mut impl std::io::Write,
    progress: &mut impl std::io::Write,
    row: &str,
) -> Result<(), String> {
    records
        .write_all(row.as_bytes())
        .map_err(|error| format!("write measurements: {error}"))?;
    aterm_uds::stdout::reader_gone_is_written(
        progress
            .write_all(row.as_bytes())
            .and_then(|()| progress.flush()),
    )
    .map_err(|error| format!("write comparison progress: {error}"))
}

fn dimensions(path: &Path) -> Result<Option<(u32, u32)>, String> {
    let file = match crate::util::open_regular_file(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let mut reader = aterm_png::Decoder::new(file)
        .read_info()
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let size = (reader.info().width, reader.info().height);
    let mut pixels = vec![0; reader.output_buffer_size()];
    reader
        .next_frame(&mut pixels)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    if size.0 == 0 || size.1 == 0 {
        return Err(format!("{}: empty image", path.display()));
    }
    Ok(Some(size))
}

/// Run every scenario with both runners, alternating order for ordinary captures.
/// # Errors
/// Invalid arguments, an output or process failure, or a candidate outcome that violates the scenario.
pub(super) fn run(args: &[String]) -> Res {
    crate::stop::install();
    let mut args = args.to_vec();
    let mut required = |flag| {
        take_value(&mut args, flag)?
            .map(|path| PathBuf::from(crate::paths::from_caller(&path)))
            .ok_or_else(|| Fail::usage(format!("instance compare needs {flag}")))
    };
    let before = std::fs::canonicalize(required("--before")?).map_err(|e| e.to_string())?;
    let after = std::fs::canonicalize(required("--after")?).map_err(|e| e.to_string())?;
    let aterm = std::fs::canonicalize(required("--aterm")?).map_err(|e| e.to_string())?;
    let output = required("--output")?;
    let samples = count(
        "--samples",
        &take_value(&mut args, "--samples")?.unwrap_or_else(|| "10".into()),
    )?;
    if !args.is_empty() {
        return Err(Fail::usage(format!(
            "instance compare: unexpected {}",
            args[0]
        )));
    }
    std::fs::create_dir(&output).map_err(|e| format!("fresh output {}: {e}", output.display()))?;
    let output = std::fs::canonicalize(output).map_err(|e| e.to_string())?;
    write_receipt(&output, None)
        .map_err(|error| format!("initialize comparison receipt: {error}"))?;
    finish(&output, measure(&before, &after, &aterm, &output, samples))
}

fn write_receipt(output: &Path, result: Option<&Res>) -> std::io::Result<()> {
    let (state, code, signal, detail) = match result {
        None => ("running", None, None, None),
        Some(Ok(code)) => (
            if *code == 0 { "passed" } else { "failed" },
            Some(*code),
            None,
            None,
        ),
        Some(Err(fail)) => (
            if fail.stopped.is_some() {
                "interrupted"
            } else {
                "failed"
            },
            Some(fail.code),
            fail.stopped,
            fail.msg.as_deref(),
        ),
    };
    let receipt = aterm_json::json!({
        "schema": 1, "state": state, "code": code, "signal": signal, "detail": detail,
    });
    crate::util::atomic_write(
        &output.join("result.json"),
        format!("{receipt}\n").as_bytes(),
    )
}

fn finish(output: &Path, result: Res) -> Res {
    if let Err(error) = write_receipt(output, Some(&result)) {
        return match result {
            Ok(_) => Err(Fail::die(format!(
                "cannot record comparison result: {error}"
            ))),
            Err(mut fail) => {
                fail.msg = Some(format!(
                    "{}; cannot record comparison result: {error}",
                    fail.msg.as_deref().unwrap_or("comparison interrupted")
                ));
                Err(fail)
            }
        };
    }
    result
}

struct Input<'a> {
    path: &'a Path,
    sha256: String,
}

impl<'a> Input<'a> {
    fn read(path: &'a Path) -> Result<Self, String> {
        let sha256 = aterm_update_core::sha256_file(path)
            .map_err(|error| format!("identify {}: {error}", path.display()))?;
        Ok(Self { path, sha256 })
    }

    fn record(&self) -> Result<aterm_json::Value, String> {
        let path = aterm_json::to_value(self.path)
            .map_err(|error| format!("record comparison input {}: {error}", self.path.display()))?;
        Ok(aterm_json::json!({ "path": path, "sha256": self.sha256 }))
    }

    fn recheck(&self) -> Result<(), String> {
        let current = Self::read(self.path)?;
        if current.sha256 != self.sha256 {
            return Err(format!(
                "comparison input changed during measurement: {}",
                self.path.display()
            ));
        }
        Ok(())
    }
}

fn measure(before: &Path, after: &Path, aterm: &Path, output: &Path, samples: u32) -> Res {
    let observer = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|error| format!("locate comparison observer: {error}"))?;
    let observer_input = Input::read(&observer)?;
    let before_input = Input::read(before)?;
    let after_input = Input::read(after)?;
    let aterm_input = Input::read(aterm)?;
    let inputs = aterm_json::json!({
        "schema": 2,
        "observer": observer_input.record()?,
        "before": before_input.record()?,
        "after": after_input.record()?,
        "aterm": aterm_input.record()?,
        "samples": samples,
    });
    crate::util::atomic_write(
        &output.join("inputs.json"),
        format!("{inputs}\n").as_bytes(),
    )
    .map_err(|error| format!("record comparison inputs: {error}"))?;
    let scratch = aterm_tempfile::Builder::new()
        .prefix("ic-")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .map_err(|e| e.to_string())?;
    let mut records =
        std::fs::File::create(output.join("measurements.tsv")).map_err(|e| e.to_string())?;
    records
        .write_all(b"scenario\tarm\tsample\texit\twidth\theight\twall_ms\tvalid\n")
        .map_err(|e| e.to_string())?;
    let mut failed = false;
    let mut serial = 0_u64;
    for scenario in [
        Scenario::Warmup,
        Scenario::Ordinary,
        Scenario::PreRefusal,
        Scenario::PostRefusal,
        Scenario::SpaceWorld,
        Scenario::NativeOnly,
    ] {
        let trials = if matches!(scenario, Scenario::Ordinary) {
            samples
        } else {
            1
        };
        for sample in 0..trials {
            let arms = if sample % 2 == 0 {
                [("before", before), ("after", after)]
            } else {
                [("after", after), ("before", before)]
            };
            for (arm, runner) in arms {
                crate::stop::check()?;
                let stem = format!("{}-{arm}-{sample}", scenario.name());
                let image = output.join(format!("{stem}.png"));
                let world = scratch.path().join(format!(
                    "w{serial}{}",
                    if matches!(scenario, Scenario::SpaceWorld) {
                        " s"
                    } else {
                        ""
                    }
                ));
                serial += 1;
                let mut command = aterm_uds::spawnfd::command(runner);
                command
                    .arg("instance")
                    .arg("--aterm")
                    .arg(aterm)
                    .arg("--world")
                    .arg(&world)
                    .arg("--image")
                    .arg(&image)
                    .args(["--pre", "version"]);
                for (flag, verb) in scenario.commands() {
                    command.args([flag, verb]);
                }
                let start = Instant::now();
                let ran =
                    aterm_uds::deadline::output_within(&mut command, Duration::from_secs(120))
                        .map_err(|e| format!("{stem} ({}): {e}", runner.display()))?;
                crate::stop::check()?;
                let wall_ms = start.elapsed().as_secs_f64() * 1000.0;
                std::fs::write(output.join(format!("{stem}.stdout.txt")), &ran.stdout)
                    .map_err(|e| e.to_string())?;
                std::fs::write(output.join(format!("{stem}.stderr.txt")), &ran.stderr)
                    .map_err(|e| e.to_string())?;
                let stdout = String::from_utf8_lossy(&ran.stdout);
                if !stdout
                    .lines()
                    .any(|line| line.starts_with("ctl version -> OK "))
                {
                    return Err(Fail::die(format!(
                        "{stem}: fixture did not establish a ready instance; see its logs"
                    )));
                }
                let refused = control_refused(&stdout, "spawn split=h");
                let size = dimensions(&image).map_err(|error| format!("{stem}: {error}"))?;
                let valid = scenario.valid(ran.status.code(), size, refused);
                failed |= arm == "after" && !valid;
                let (width, height) = size.unwrap_or((0, 0));
                let row = format!(
                    "{}\t{arm}\t{sample}\t{}\t{width}\t{height}\t{wall_ms:.3}\t{valid}\n",
                    scenario.name(),
                    ran.status
                        .code()
                        .map_or_else(|| "signal".into(), |n| n.to_string())
                );
                record_row(&mut records, &mut std::io::stdout().lock(), &row)
                    .map_err(|error| format!("{stem}: {error}"))?;
            }
        }
    }
    crate::stop::check()?;
    if failed {
        Err(Fail::die(
            "instance compare: candidate violates a capture scenario; see measurements.tsv",
        ))
    } else {
        for input in [before_input, after_input, aterm_input, observer_input] {
            crate::stop::check()?;
            input.recheck()?;
        }
        crate::stop::check()?;
        Ok(0)
    }
}

#[cfg(test)]
#[path = "instance_compare_tests.rs"]
mod tests;
