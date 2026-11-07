// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared synthetic graph construction and typed phase validation for manual benchmarks.

use agh::fx::NativePushTiming;
use agh::git::NativePushPhase;
use aterm_json::json;
use std::error::Error;
use std::path::Path;
use std::time::{Duration, Instant};

pub(crate) fn git(root: &Path, args: &[&str]) -> Result<String, Box<dyn Error>> {
    let mut command = aterm_uds::fixture_git::git();
    // The same synthetic inputs must create the same commit in each process.
    command
        .current_dir(root)
        .args(["-c", "gc.auto=0", "-c", "maintenance.auto=false"])
        .args(args)
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000");
    let out = aterm_uds::deadline::output_until_complete_stdout(
        &mut command,
        Instant::now() + Duration::from_secs(60),
    )?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}

pub(crate) fn path(path: &Path) -> Result<&str, Box<dyn Error>> {
    path.to_str()
        .ok_or_else(|| "fixture path is not UTF-8".into())
}

pub(crate) fn object_kind(root: &Path, oid: &str) -> Result<String, Box<dyn Error>> {
    let mut command = aterm_uds::fixture_git::git();
    command
        .current_dir(root)
        .args(["cat-file", "--batch-check=%(objecttype)"]);
    let out = aterm_uds::deadline::output_with_input_until(
        &mut command,
        aterm_uds::secret::Secret::new(format!("{oid}\n").as_bytes()),
        Instant::now() + Duration::from_secs(60),
        1024,
    )?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}

pub(crate) fn same_tree_child(root: &Path, parent: &str) -> Result<String, Box<dyn Error>> {
    let tree = git(root, &["rev-parse", &format!("{parent}^{{tree}}")])?;
    let child = git(
        root,
        &[
            "commit-tree",
            &tree,
            "-p",
            parent,
            "-m",
            "isolated admission same-tree child",
        ],
    )?;
    assert_eq!(
        git(root, &["rev-parse", &format!("{child}^{{tree}}")])?,
        tree
    );
    assert_eq!(git(root, &["rev-parse", &format!("{child}^")])?, parent);
    assert_eq!(
        git(
            root,
            &["rev-list", "--objects", &child, &format!("^{parent}")]
        )?,
        child,
        "the new closure is exactly one commit, with no new tree or blob"
    );
    Ok(child)
}

/// One root and a retained chain whose last tree restores that same root tree.
/// All bodies are synthetic; fast-import writes only this owned fixture repository.
pub(crate) fn retained_history(root: &Path) -> Result<(), Box<dyn Error>> {
    fn blob(stream: &mut String, next: &mut usize, content: &str) -> usize {
        let mark = *next;
        *next += 1;
        stream.push_str(&format!(
            "blob\nmark :{mark}\ndata {}\n{content}\n",
            content.len()
        ));
        mark
    }
    fn commit(
        stream: &mut String,
        next: &mut usize,
        reference: &str,
        parent: Option<usize>,
        step: usize,
    ) -> usize {
        let mark = *next;
        *next += 1;
        let message = format!("synthetic retained history {step:08}\n");
        stream.push_str(&format!("commit {reference}\nmark :{mark}\ncommitter fixture <fixture@example.invalid> 1700000000 +0000\ndata {}\n{message}\n", message.len()));
        if let Some(parent) = parent {
            stream.push_str(&format!("from :{parent}\n"));
        }
        mark
    }
    fn record(index: usize, revision: usize) -> String {
        format!(
            "tree = \"{index:040x}\"\nstate = \"judging\"\nlease_until = 120\n# synthetic revision {revision:08}\n# {}\n",
            "fixture".repeat(140)
        )
    }
    let mut stream = String::from("feature done\n");
    let mut next = 1;
    let marker = blob(
        &mut stream,
        &mut next,
        "synthetic retained-history fixture\n",
    );
    let rows: Vec<_> = (0..72)
        .map(|index| blob(&mut stream, &mut next, &record(index, 0)))
        .collect();
    let mut parent = commit(&mut stream, &mut next, "refs/heads/journal", None, 0);
    let restore = |stream: &mut String| {
        stream.push_str(&format!("M 100644 :{marker} .fixture\n"));
        for (index, mark) in rows.iter().enumerate() {
            stream.push_str(&format!("M 100644 :{mark} verdicts/{index:040x}.toml\n"));
        }
    };
    restore(&mut stream);
    stream.push('\n');
    // 2,572 BEFORE commits become 2,573 after the common same-tree child update.
    for step in 1..2572 {
        let last = step == 2571;
        let changed = (!last).then(|| blob(&mut stream, &mut next, &record((step - 1) % 72, step)));
        parent = commit(
            &mut stream,
            &mut next,
            "refs/heads/retained",
            Some(parent),
            step,
        );
        if let Some(mark) = changed {
            stream.push_str(&format!(
                "M 100644 :{mark} verdicts/{:040x}.toml\n",
                (step - 1) % 72
            ));
        } else {
            stream.push_str("deleteall\n");
            restore(&mut stream);
        }
        stream.push('\n');
    }
    stream.push_str("done\n");
    let mut command = aterm_uds::fixture_git::git();
    command
        .current_dir(root)
        .args(["-c", "gc.auto=0", "-c", "maintenance.auto=false"])
        .args(["fast-import", "--quiet", "--date-format=raw"]);
    let out = aterm_uds::deadline::output_with_input_until(
        &mut command,
        aterm_uds::secret::Secret::new(stream.as_bytes()),
        Instant::now() + Duration::from_secs(60),
        4096,
    )?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
    }
    assert_eq!(git(root, &["rev-list", "--count", "journal"])?, "1");
    assert_eq!(git(root, &["rev-list", "--count", "retained"])?, "2572");
    assert_eq!(
        git(root, &["rev-parse", "journal^{tree}"])?,
        git(root, &["rev-parse", "retained^{tree}"])?
    );
    Ok(())
}

pub(crate) fn phase_observation(
    spans: &[NativePushTiming],
    began: Instant,
    finished: Instant,
) -> (aterm_json::Value, bool) {
    let expected = [
        NativePushPhase::MetadataPack,
        NativePushPhase::MetadataVerify,
        NativePushPhase::Connectivity,
    ];
    let valid = spans.len() == expected.len()
        && spans.iter().zip(expected).all(|(span, phase)| {
            span.phase == phase
                && span.success
                && began <= span.started
                && span.started <= span.finished
                && span.finished <= finished
        })
        && spans
            .windows(2)
            .all(|pair| pair[0].finished <= pair[1].started);
    let rows: Vec<_> = spans.iter().map(|span| {
        let name = match span.phase {
            NativePushPhase::MetadataPack => "metadata_pack",
            NativePushPhase::MetadataVerify => "metadata_verify",
            NativePushPhase::Connectivity => "connectivity",
        };
        json!({"phase":name,"start_offset_ns":span.started.checked_duration_since(began).map(|d|d.as_nanos()),"end_offset_ns":span.finished.checked_duration_since(began).map(|d|d.as_nanos()),"wall_ns":span.finished.checked_duration_since(span.started).map(|d|d.as_nanos()),"success":span.success})
    }).collect();
    let total = spans.iter().try_fold(0u128, |total, span| {
        total.checked_add(
            span.finished
                .checked_duration_since(span.started)?
                .as_nanos(),
        )
    });
    let outside =
        total.and_then(|total| finished.duration_since(began).as_nanos().checked_sub(total));
    (
        json!({"spans":rows,"outside_observed_ns":outside}),
        valid && outside.is_some(),
    )
}
