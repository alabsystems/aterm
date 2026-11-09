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
mod tests {
    use super::{Input, Scenario, control_refused, dimensions, finish, record_row, run};

    #[cfg(unix)]
    #[test]
    fn a_closed_progress_reader_preserves_the_measurement_and_the_run() {
        let (mut progress, reader) = std::os::unix::net::UnixStream::pair().unwrap();
        drop(reader);
        let mut records = Vec::new();
        record_row(&mut records, &mut progress, "completed\n").unwrap();
        assert_eq!(records, b"completed\n");
    }

    #[test]
    fn an_unwritable_journal_does_not_announce_an_unrecorded_row() {
        let mut storage = [];
        let mut records = std::io::Cursor::new(&mut storage[..]);
        let mut progress = Vec::new();
        assert!(record_row(&mut records, &mut progress, "completed\n").is_err());
        assert!(progress.is_empty());
    }

    #[test]
    fn a_progress_write_error_keeps_the_record_but_fails_the_write() {
        let mut records = Vec::new();
        let mut storage = [];
        let mut progress = std::io::Cursor::new(&mut storage[..]);
        assert!(record_row(&mut records, &mut progress, "completed\n").is_err());
        assert_eq!(records, b"completed\n");
    }

    #[cfg(unix)]
    #[test]
    fn an_unrepresentable_native_input_path_is_an_error_instead_of_a_panic() {
        use std::os::unix::ffi::OsStringExt as _;
        let path = std::path::PathBuf::from(std::ffi::OsString::from_vec(b"runner\xff".to_vec()));
        let input = Input {
            path: &path,
            sha256: "0".repeat(64),
        };
        assert!(input.record().is_err());
    }

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
        assert_eq!(inputs["schema"].as_u64(), Some(2));
        let observer = std::env::current_exe().unwrap();
        for (arm, path) in [
            ("before", &runner),
            ("after", &after),
            ("aterm", &app),
            ("observer", &observer),
        ] {
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
    fn a_fifo_capture_is_refused_without_waiting_for_a_writer() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let image = scratch.path().join("frame.png");
        crate::sys::make_fifo(&image).unwrap();
        let (done, completed) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            done.send(dimensions(&image)).unwrap();
        });
        let result = completed
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("a capture FIFO must be refused without waiting for a writer");
        worker.join().unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn image_validation_distinguishes_absent_valid_and_corrupt_files() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let image = scratch.path().join("frame.png");
        assert_eq!(dimensions(&image).unwrap(), None);
        {
            let mut encoder = aterm_png::Encoder::new(std::fs::File::create(&image).unwrap(), 1, 1);
            encoder.set_color(aterm_png::ColorType::Rgb);
            encoder.set_depth(aterm_png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[1, 2, 3]).unwrap();
            writer.finish().unwrap();
        }
        assert_eq!(dimensions(&image).unwrap(), Some((1, 1)));
        std::fs::write(&image, b"not a PNG").unwrap();
        assert!(dimensions(&image).is_err());
    }

    #[test]
    fn a_failed_fixture_is_not_an_expected_control_refusal() {
        for reply in ["ERR refused", "aterm-ctl: ERR refused"] {
            assert!(control_refused(
                &format!("ctl spawn split=h -> {reply}\n"),
                "spawn split=h"
            ));
        }
        assert!(!control_refused(
            "ctl spawn split=h -> OK payload=ERR refused\n",
            "spawn split=h"
        ));
        assert!(!control_refused(
            "ctl spawn split=v -> aterm-ctl: ERR refused\n",
            "spawn split=h"
        ));
        assert!(!Scenario::PreRefusal.valid(Some(1), None, false));
        assert!(Scenario::PreRefusal.valid(Some(1), None, true));
        assert!(!Scenario::PostRefusal.valid(Some(1), Some((794, 462)), false));
        assert!(Scenario::PostRefusal.valid(Some(1), Some((794, 462)), true));
        for code in [
            Some(0),
            Some(2),
            Some(7),
            Some(101),
            Some(126),
            Some(127),
            Some(130),
            None,
        ] {
            assert!(!Scenario::PreRefusal.valid(code, None, true), "{code:?}");
            assert!(
                !Scenario::PostRefusal.valid(code, Some((794, 462)), true),
                "{code:?}"
            );
        }
        assert!(!Scenario::NativeOnly.valid(Some(1), None, false));
        assert!(Scenario::NativeOnly.valid(Some(0), Some((794, 462)), false));
    }
}
