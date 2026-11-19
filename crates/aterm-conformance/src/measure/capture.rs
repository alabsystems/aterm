// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Ordinary production capture and the Mac's explicit unavailable lease capability.
//! The PNG/text observations establish service and composed-capture progress, not native-glass
//! presentation. The caller owns functional desktop/memory admission through checked retirement.

use std::path::{Component, Path, PathBuf};

use super::Rig;
use super::frame::Frame;
use super::instance::{CouldNotRun, Instance, Reply, Scratch, World};
use super::starvation::ShapeVerdict;

const TOKEN: &str = "aterm_capture_after_refusal";
const BEFORE: &str = "capture-before.png";
const AFTER: &str = "capture-after.png";
const HELD: &str = "capture-refused.png";

#[derive(Debug)]
pub struct CaptureReport {
    pub verdict: ShapeVerdict,
    pub line: String,
}

enum Fault {
    Failed(String),
    Unavailable(CouldNotRun),
}

impl From<CouldNotRun> for Fault {
    fn from(error: CouldNotRun) -> Self {
        Self::Unavailable(error)
    }
}

fn reply_available(reply: &Reply) -> Result<(), Fault> {
    if reply.code.is_none() {
        return Err(Fault::Unavailable(CouldNotRun::Refused(format!(
            "the control client supplied no terminal status: {} {}",
            reply.stdout.trim(),
            reply.stderr.trim()
        ))));
    }
    Ok(())
}

fn canonical_number(word: &str) -> Option<usize> {
    (!word.is_empty() && word.bytes().all(|b| b.is_ascii_digit()))
        .then(|| word.parse::<usize>().ok())
        .flatten()
        .filter(|n| *n > 0 && n.to_string() == word)
}

fn image(reply: &Reply, root: &Path, name: &str) -> Result<Frame, String> {
    if !reply.ok() {
        return Err(format!(
            "ordinary image refused: {} {}",
            reply.stdout.trim(),
            reply.stderr.trim()
        ));
    }
    let mut lines = reply.stdout.lines();
    let line = lines.next().ok_or("ordinary image returned no receipt")?;
    if lines.next().is_some() {
        return Err("ordinary image returned more than one receipt line".into());
    }
    let mut fields = line.splitn(4, ' ');
    if fields.next() != Some("OK") {
        return Err("ordinary image returned no OK receipt".into());
    }
    let width = fields
        .next()
        .and_then(canonical_number)
        .ok_or("ordinary image has no canonical positive width")?;
    let height = fields
        .next()
        .and_then(canonical_number)
        .ok_or("ordinary image has no canonical positive height")?;
    let path = PathBuf::from(fields.next().ok_or("ordinary image returned no path")?);
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
    {
        return Err("ordinary image path is not an absolute confined path".into());
    }
    let root = root
        .canonicalize()
        .map_err(|error| format!("private world: {error}"))?;
    let images = root.join("images");
    let directory =
        std::fs::symlink_metadata(&images).map_err(|error| format!("private images: {error}"))?;
    if !directory.is_dir() || directory.file_type().is_symlink() {
        return Err("private image directory is not a real directory".into());
    }
    let meta =
        std::fs::symlink_metadata(&path).map_err(|error| format!("ordinary image: {error}"))?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err("ordinary image is not a regular non-symlink file".into());
    }
    if path
        .canonicalize()
        .map_err(|error| format!("ordinary image path: {error}"))?
        != images.join(name)
    {
        return Err("ordinary image escaped its exact private named target".into());
    }
    let frame = Frame::load(&path).map_err(|error| format!("ordinary image PNG: {error}"))?;
    if (frame.width(), frame.height()) != (width, height) {
        return Err("ordinary image receipt dimensions differ from its decoded PNG".into());
    }
    Ok(frame)
}

fn control_line(line: &str) -> &str {
    let line = line.trim();
    line.strip_prefix("aterm-ctl: ").unwrap_or(line)
}

fn lease_refusal(
    reply: &Reply,
    marker_present: bool,
    held_present: bool,
) -> Result<String, String> {
    if !reply.code.is_some_and(|code| code != 0) {
        return Err("the lease request supplied no explicit nonzero terminal status".into());
    }
    let lines: Vec<_> = reply
        .stdout
        .lines()
        .chain(reply.stderr.lines())
        .map(control_line)
        .collect();
    if lines.iter().any(|line| {
        line.starts_with("OK presentation-image") || line.starts_with("OK presentation-released")
    }) {
        return Err("the unavailable lease published a success or release receipt".into());
    }
    if marker_present || held_present {
        return Err(format!(
            "the unavailable lease left a side effect: producer_marker={marker_present} lease_image={held_present}"
        ));
    }
    let errors: Vec<_> = lines
        .into_iter()
        .filter(|line| line.starts_with("ERR "))
        .collect();
    let [error] = errors.as_slice() else {
        return Err("the lease request supplied no single structured capability refusal".into());
    };
    let words: Vec<_> = error.split_whitespace().collect();
    if !words.starts_with(&["ERR", "presentation", "capture", "requires"])
        || !words.windows(2).any(|pair| pair == ["Linux", "CPU"])
        || words.last() != Some(&"backend")
    {
        return Err(format!(
            "the lease request did not report its unavailable production capability: {error}"
        ));
    }
    Ok((*error).to_string())
}

fn present(path: &Path) -> Result<bool, Fault> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(Fault::Unavailable(CouldNotRun::Refused(format!(
            "cannot observe {}: {error}",
            path.display()
        )))),
    }
}

fn ordinary(inst: &Instance, world: &World, name: &str) -> Result<Frame, Fault> {
    let reply = inst.ctl.call(&["image", name]);
    reply_available(&reply)?;
    image(&reply, &world.root, name).map_err(Fault::Failed)
}

struct Evidence {
    width: usize,
    height: usize,
    refusal: String,
}

fn check(inst: &Instance, world: &World) -> Result<Evidence, Fault> {
    let first_text = inst.ctl.call(&["text"]);
    reply_available(&first_text)?;
    if !first_text.ok() || first_text.stdout.contains(TOKEN) {
        return Err(Fault::Failed(
            "the fresh private text premise was not established".into(),
        ));
    }
    let before = ordinary(inst, world, BEFORE)?;
    let marker = world.root.join("photo-producer-ran");
    let held = world.root.join("images").join(HELD);
    if present(&marker)? || present(&held)? {
        return Err(Fault::Failed(
            "the private negative-control targets already exist".into(),
        ));
    }
    let marker_word = marker.to_str().ok_or_else(|| {
        Fault::Unavailable(CouldNotRun::Refused(
            "private producer marker is not UTF-8".into(),
        ))
    })?;
    let refusal = inst.ctl.call(&[
        "image",
        "--presentation-lease",
        HELD,
        "--",
        "/usr/bin/touch",
        marker_word,
    ]);
    reply_available(&refusal)?;
    let refusal =
        lease_refusal(&refusal, present(&marker)?, present(&held)?).map_err(Fault::Failed)?;

    inst.ctl.ok(&["send", "--", TOKEN])?;
    // One native semantic watcher parks until the PTY text changes. Its explicit protocol
    // timeout may rearm the watcher; this is one ongoing functional wait, not a test retry.
    loop {
        let reply = inst.ctl.call(&["await", "match", TOKEN, "timeout=600000"]);
        reply_available(&reply)?;
        if reply.ok() && reply.line() == "OK timeout" {
            continue;
        }
        if !reply.ok() || !reply.line().starts_with("OK ") {
            return Err(Fault::Failed(format!(
                "native text acknowledgement refused: {} {}",
                reply.stdout.trim(),
                reply.stderr.trim()
            )));
        }
        break;
    }
    let text = inst.ctl.call(&["text"]);
    reply_available(&text)?;
    if !text.ok() || !text.stdout.contains(TOKEN) {
        return Err(Fault::Failed(
            "the native acknowledgement was not followed by its actual PTY text".into(),
        ));
    }
    let after = ordinary(inst, world, AFTER)?;
    if (before.width(), before.height()) != (after.width(), after.height()) || before == after {
        return Err(Fault::Failed("ordinary composed captures did not keep their dimensions and change with the acknowledged text".into()));
    }
    Ok(Evidence {
        width: after.width(),
        height: after.height(),
        refusal,
    })
}

/// Observe one ordinary production Mac instance. The caller retains non-timing desktop/memory
/// admission until this call returns, including the explicit original-instance/helper stop.
/// This row makes no latency, native-glass presentation or universal descendant-drain claim.
/// # Errors
/// The Mac, private world, original process identity, complete control reply or checked
/// retirement is unavailable. Other operating systems name this Mac-only operation unavailable.
pub fn measure(rig: &Rig) -> Result<CaptureReport, CouldNotRun> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = rig;
        return Err(CouldNotRun::Refused(
            "production Mac capture capability coverage requires macOS".into(),
        ));
    }
    #[cfg(target_os = "macos")]
    {
        let mut scratch = Scratch::new("aterm-production-capture")
            .map_err(|error| CouldNotRun::Refused(error.to_string()))?;
        let world = World::prepare(scratch.path(), "")
            .map_err(|error| CouldNotRun::Refused(error.to_string()))?;
        let inst = Instance::launch_windowed_functional(&rig.aterm, &world)?;
        let checked = check(&inst, &world);
        let retired = inst.finish();
        let result = match checked {
            Ok(evidence) => retired.map(|helpers| CaptureReport {
                verdict: ShapeVerdict::Pass,
                line: format!("CAPTURE ordinary_png=2 text_ack=1 width={} height={} lease_payload=absent producer_marker=absent original_reaped=1 observed_helpers_retired={helpers} refusal={:?}", evidence.width, evidence.height, evidence.refusal),
            }),
            Err(Fault::Failed(mut why)) => {
                if let Err(cleanup) = retired { why.push_str(&format!("; checked cleanup: {cleanup}")); }
                Ok(CaptureReport { verdict: ShapeVerdict::Fail, line: format!("CAPTURE FAILED: {why}") })
            }
            Err(Fault::Unavailable(error)) => Err(error.map(|why| match retired {
                Ok(_) => why,
                Err(cleanup) => format!("{why}; checked cleanup: {cleanup}"),
            })),
        };
        if !result
            .as_ref()
            .is_ok_and(|report| report.verdict == ShapeVerdict::Pass)
        {
            let path = scratch.path().display().to_string();
            scratch.keep();
            return result
                .map(|mut report| {
                    report.line.push_str(&format!("; private_world={path}"));
                    report
                })
                .map_err(|error| error.map(|why| format!("{why}; private_world={path}")));
        }
        result
    }
}

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;
