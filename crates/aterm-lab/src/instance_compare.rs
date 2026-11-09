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
                [("before", &before), ("after", &after)]
            } else {
                [("after", &after), ("before", &before)]
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
                    .arg(&aterm)
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
                        .map_err(|e| format!("{stem}: {e}"))?;
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
    if failed {
        Err(Fail::die(
            "instance compare: candidate violates a capture scenario; see measurements.tsv",
        ))
    } else {
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::{Scenario, dimensions};

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
