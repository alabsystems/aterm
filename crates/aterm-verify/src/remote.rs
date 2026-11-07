// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The verifier's remote boundary: GitHub goes through agh, local repositories through git.
//! Objects are imported at an exact observed OID; writes carry the expected ref and stop on
//! an unknown outcome. Neither a failed agh command nor an unrecognised URL falls back to git.

use aterm_json::{Value, json};
use aterm_uds::spawnfd::Command;
use std::ffi::OsString;
use std::path::Path;
use std::process::Output;
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tool {
    Git,
    Agh,
}

type Runner<'a> = dyn Fn(Tool, &[OsString]) -> Result<Output, String> + 'a;

pub(crate) fn run(
    root: &Path,
    git: &dyn Fn(&Path) -> Command,
    tool: Tool,
    args: &[OsString],
    until: Instant,
) -> Result<Output, String> {
    let mut cmd = match tool {
        Tool::Git => git(root),
        Tool::Agh => {
            let mut cmd = aterm_uds::spawnfd::command("agh");
            cmd.current_dir(root);
            cmd
        }
    };
    cmd.args(args);
    aterm_uds::deadline::output_until_complete_stdout(&mut cmd, until)
        .map_err(|e| format!("{tool:?} remote operation: {e}"))
}

#[derive(Debug, PartialEq, Eq)]
enum Kind {
    Local(String),
    Github(String),
}

fn kind(url: &str) -> Result<Kind, String> {
    let tail = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
        .or_else(|| url.strip_prefix("git@github.com:"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"));
    if let Some(tail) = tail {
        let tail = tail.strip_suffix('/').unwrap_or(tail);
        let slug = tail.strip_suffix(".git").unwrap_or(tail);
        let parts: Vec<_> = slug.split('/').collect();
        if parts.len() == 2
            && parts.iter().all(|part| {
                !part.is_empty()
                    && *part != "."
                    && *part != ".."
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            })
        {
            return Ok(Kind::Github(slug.to_ascii_lowercase()));
        }
    } else if !url.is_empty()
        && !url.starts_with('-')
        && !url.contains(':')
        && !url.chars().any(char::is_control)
    {
        return Ok(Kind::Local(url.to_owned()));
    }
    // A remote URL can contain credentials; never include it in a diagnostic.
    Err("remote must name an unambiguous GitHub OWNER/REPO or a local repository path".into())
}

fn words(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

/// A changed URL or an insteadOf rewrite cannot turn a local operation into network traffic.
fn local_words(args: &[&str]) -> Vec<OsString> {
    let mut command = words(&[
        "-c",
        "protocol.allow=never",
        "-c",
        "protocol.file.allow=always",
    ]);
    command.extend(words(args));
    command
}

fn why(tool: Tool, out: &Output) -> String {
    format!(
        "{tool:?} remote operation exited {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).trim()
    )
}

fn success(tool: Tool, out: Output) -> Result<Vec<u8>, String> {
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(why(tool, &out))
    }
}

pub(crate) struct Remote<'a> {
    root: &'a Path,
    name: &'a str,
    kind: Kind,
    run: &'a Runner<'a>,
}

pub(crate) enum Pushed {
    Done,
    Rejected(String),
}

impl<'a> Remote<'a> {
    pub(crate) fn open(root: &'a Path, name: &'a str, run: &'a Runner<'a>) -> Result<Self, String> {
        let url = |push| {
            let mut args = words(&["remote", "get-url", "--all"]);
            if push {
                args.push("--push".into());
            }
            args.push(name.into());
            let out = run(Tool::Git, &args)?;
            // git may echo a malformed configured URL on failure. Keep this read's error local.
            if !out.status.success() {
                return Err("cannot read the remote's expanded URL".to_owned());
            }
            let text = std::str::from_utf8(&out.stdout).map_err(|_| "remote URL is not UTF-8")?;
            let lines: Vec<_> = text.lines().collect();
            if lines.len() != 1 {
                return Err("remote must have exactly one fetch URL and one push URL".into());
            }
            kind(lines[0])
        };
        let kind = url(false)?;
        if kind != url(true)? {
            return Err("remote fetch and push URLs must address the same repository".into());
        }
        Ok(Self {
            root,
            name,
            kind,
            run,
        })
    }

    /// Import the exact commit a ref names, or report an absent ref. No branch/index is moved.
    pub(crate) fn fetch(&self, reference: &str) -> Result<Option<String>, String> {
        match &self.kind {
            Kind::Local(_) => {
                let out = success(
                    Tool::Git,
                    (self.run)(
                        Tool::Git,
                        &local_words(&["ls-remote", self.name, reference]),
                    )?,
                )?;
                let text = std::str::from_utf8(&out).map_err(|_| "remote refs are not UTF-8")?;
                let mut found = text
                    .lines()
                    .filter_map(|line| line.split_once('\t'))
                    .filter(|(_, name)| *name == reference);
                let Some((sha, _)) = found.next() else {
                    return Ok(None);
                };
                if found.next().is_some() || !aterm_types::land_trailer::object_id(sha) {
                    return Err("remote did not return one exact commit ID".into());
                }
                success(
                    Tool::Git,
                    (self.run)(
                        Tool::Git,
                        &local_words(&["fetch", "--quiet", "--no-tags", self.name, sha]),
                    )?,
                )?;
                Ok(Some(sha.to_owned()))
            }
            Kind::Github(slug) => {
                let name = reference
                    .strip_prefix("refs/")
                    .ok_or("remote ref must start with refs/")?;
                let endpoint = format!("repos/{slug}/git/matching-refs/{name}");
                let out = success(
                    Tool::Agh,
                    (self.run)(Tool::Agh, &words(&["api", &endpoint]))?,
                )?;
                let value: Value =
                    aterm_json::from_slice(&out).map_err(|e| format!("agh refs: {e}"))?;
                let value = value.get("answer").unwrap_or(&value);
                let rows = value
                    .as_array()
                    .ok_or("agh did not return a complete ref array")?;
                let mut found = rows
                    .iter()
                    .filter(|row| row["ref"].as_str() == Some(reference));
                let Some(row) = found.next() else {
                    return Ok(None);
                };
                let sha = row["object"]["sha"]
                    .as_str()
                    .ok_or("agh ref has no object ID")?;
                if found.next().is_some()
                    || row["object"]["type"].as_str() != Some("commit")
                    || !aterm_types::land_trailer::object_id(sha)
                {
                    return Err("agh did not return one exact commit ID".into());
                }
                let dir = aterm_tempfile::tempdir().map_err(|e| e.to_string())?;
                let input = dir.path().join("fetch.json");
                let into = format!("refs/agh/verify/{name}");
                // The ref can advance after observation; import the immutable snapshot itself.
                std::fs::write(
                    &input,
                    json!([{"source":sha,"into":into,"expected_oid":sha}]).to_string(),
                )
                .map_err(|e| e.to_string())?;
                let mut args = words(&["repo", "fetch", slug]);
                args.extend([
                    self.root.as_os_str().to_owned(),
                    "--input".into(),
                    input.into_os_string(),
                ]);
                success(Tool::Agh, (self.run)(Tool::Agh, &args)?)?;
                Ok(Some(sha.to_owned()))
            }
        }
    }

    /// Publish exact local objects, expected ref included. An unknown outcome stops the caller;
    /// agh's durable intent prevents another batch until its outcome is reconciled.
    pub(crate) fn push(
        &self,
        reference: &str,
        before: Option<&str>,
        after: &str,
    ) -> Result<Pushed, String> {
        match &self.kind {
            Kind::Local(_) => {
                let destination = format!("{after}:{reference}");
                let out = (self.run)(
                    Tool::Git,
                    &local_words(&["push", "--porcelain", self.name, &destination]),
                )?;
                Ok(if out.status.success() {
                    Pushed::Done
                } else {
                    Pushed::Rejected(format!(
                        "git push {} {destination}: {}",
                        self.name,
                        String::from_utf8_lossy(&out.stderr).trim()
                    ))
                })
            }
            Kind::Github(slug) => {
                let dir = aterm_tempfile::tempdir().map_err(|e| e.to_string())?;
                let input = dir.path().join("push.json");
                let before = before.unwrap_or("0000000000000000000000000000000000000000");
                std::fs::write(
                    &input,
                    json!([{"ref":reference,"before":before,"after":after}]).to_string(),
                )
                .map_err(|e| e.to_string())?;
                let mut args = words(&["repo", "push", slug]);
                args.extend([
                    self.root.as_os_str().to_owned(),
                    "--input".into(),
                    input.into_os_string(),
                ]);
                let mut dry = args.clone();
                dry.push("--dry-run".into());
                for args in [&dry, &args] {
                    let out = (self.run)(Tool::Agh, args)?;
                    if !out.status.success() {
                        let why = why(Tool::Agh, &out);
                        return if out.status.code() == Some(9) {
                            Ok(Pushed::Rejected(why))
                        } else if out.status.code() == Some(13) {
                            Err(format!(
                                "{why}; outcome unknown: reconcile agh's intent before another write"
                            ))
                        } else {
                            Err(why)
                        };
                    }
                }
                Ok(Pushed::Done)
            }
        }
    }
}

#[cfg(test)]
#[path = "remote_tests.rs"]
mod tests;
