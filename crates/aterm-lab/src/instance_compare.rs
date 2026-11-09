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
        let failed = code.is_some_and(|code| code != 0);
        match self {
            Self::PreRefusal => failed && refused && image.is_none(),
            Self::PostRefusal => failed && refused && image.is_some(),
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

fn dimensions(path: &Path) -> Result<Option<(u32, u32)>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let mut reader = aterm_png::Decoder::new(std::io::Cursor::new(bytes))
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

    fn record(&self) -> aterm_json::Value {
        aterm_json::json!({ "path": self.path, "sha256": self.sha256 })
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
    let before_input = Input::read(before)?;
    let after_input = Input::read(after)?;
    let aterm_input = Input::read(aterm)?;
    let inputs = aterm_json::json!({
        "schema": 1,
        "before": before_input.record(),
        "after": after_input.record(),
        "aterm": aterm_input.record(),
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
                let refused = stdout
                    .lines()
                    .any(|line| line.starts_with("ctl spawn split=h ->") && line.contains("ERR "));
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
                print!("{row}");
                records
                    .write_all(row.as_bytes())
                    .map_err(|e| e.to_string())?;
            }
        }
    }
    crate::stop::check()?;
    if failed {
        Err(Fail::die(
            "instance compare: candidate violates a capture scenario; see measurements.tsv",
        ))
    } else {
        for input in [before_input, after_input, aterm_input] {
            crate::stop::check()?;
            input.recheck()?;
        }
        crate::stop::check()?;
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::{Input, Scenario, dimensions, finish, run};

    #[test]
    fn replacing_input_bytes_without_changing_length_invalidates_the_identity() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = scratch.path().join("runner");
        std::fs::write(&path, b"before").unwrap();
        let input = Input::read(&path).unwrap();
        assert!(input.recheck().is_ok());
        std::fs::write(&path, b"after!").unwrap();
        assert!(input.recheck().is_err());
        std::fs::remove_file(&path).unwrap();
        assert!(input.recheck().is_err());
    }

    #[test]
    fn a_receipt_write_failure_keeps_the_original_failure() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let output = scratch.path().join("not-a-directory");
        std::fs::write(&output, b"occupied").unwrap();
        let result = finish(&output, Err(crate::Fail::exit(7, "original cause")));
        let fail = result.err().unwrap();
        assert_eq!(fail.code, 7);
        assert!(fail.msg.unwrap().contains("original cause"));
    }

    #[test]
    fn a_runner_that_cannot_start_leaves_a_failed_result_receipt() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let runner = scratch.path().join("not-executable");
        std::fs::write(&runner, b"not an executable\n").unwrap();
        let after = scratch.path().join("candidate");
        std::fs::write(&after, b"candidate bytes\n").unwrap();
        let app = scratch.path().join("app");
        std::fs::write(&app, b"shared app bytes\n").unwrap();
        let output = scratch.path().join("results");
        let args = [
            "--before".into(),
            runner.to_string_lossy().into_owned(),
            "--after".into(),
            after.to_string_lossy().into_owned(),
            "--aterm".into(),
            app.to_string_lossy().into_owned(),
            "--output".into(),
            output.to_string_lossy().into_owned(),
            "--samples".into(),
            "1".into(),
        ];
        assert!(run(&args).is_err());
        let text = std::fs::read_to_string(output.join("result.json")).unwrap();
        let receipt: aterm_json::Value = aterm_json::from_str(&text).unwrap();
        assert_eq!(receipt["state"].as_str(), Some("failed"));
        let inputs: aterm_json::Value =
            aterm_json::from_str(&std::fs::read_to_string(output.join("inputs.json")).unwrap())
                .unwrap();
        assert_eq!(inputs["samples"].as_u64(), Some(1));
        for (arm, path) in [("before", &runner), ("after", &after), ("aterm", &app)] {
            let canonical_path = std::fs::canonicalize(path).unwrap();
            assert_eq!(inputs[arm]["path"].as_str(), canonical_path.to_str());
            assert_eq!(
                inputs[arm]["sha256"].as_str(),
                Some(aterm_update_core::sha256_file(path).unwrap().as_str())
            );
        }
        assert!(
            receipt["detail"]
                .as_str()
                .unwrap()
                .contains("warmup-before-0")
        );
    }

    #[test]
    fn a_corrupt_image_is_not_an_absent_image() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let image = scratch.path().join("frame.png");
        assert_eq!(dimensions(&image).unwrap(), None);
        std::fs::write(&image, b"not a PNG").unwrap();
        assert!(dimensions(&image).is_err());
    }

    #[test]
    fn a_failed_fixture_is_not_an_expected_control_refusal() {
        assert!(!Scenario::PreRefusal.valid(Some(1), None, false));
        assert!(Scenario::PreRefusal.valid(Some(1), None, true));
        assert!(!Scenario::PostRefusal.valid(Some(1), Some((794, 462)), false));
        assert!(Scenario::PostRefusal.valid(Some(1), Some((794, 462)), true));
        assert!(!Scenario::NativeOnly.valid(Some(1), None, false));
        assert!(Scenario::NativeOnly.valid(Some(0), Some((794, 462)), false));
    }
}
