// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The verifier's remote boundary: GitHub goes through agh, local repositories through git.
//! Pinned reads import an exact observed OID; ordinary main refreshes reuse the checkout's
//! objects. Writes carry the expected ref and stop on an unknown outcome. Neither a failed
//! agh command nor an unrecognised URL falls back to git.

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

/// How a remote step is run: [`run`], or [`run_unless`].
pub(crate) type Runner<'a> = dyn Fn(Tool, &[OsString]) -> Result<Output, String> + 'a;

pub(crate) fn run(
    root: &Path,
    git: &dyn Fn(&Path) -> Command,
    tool: Tool,
    args: &[OsString],
    until: Instant,
) -> Result<Output, String> {
    let mut cmd = command(root, git, tool, args)?;
    aterm_uds::deadline::output_until_complete_stdout(&mut cmd, until)
        .map_err(|e| format!("{tool:?} remote operation: {e}"))
}

/// [`run`], ended once `stopped` reads `true` (read at least every
/// [`aterm_uds::deadline::STOP_TICK`]): an ended step publishes no output and is an error.
pub(crate) fn run_unless(
    root: &Path,
    git: &dyn Fn(&Path) -> Command,
    tool: Tool,
    args: &[OsString],
    until: Instant,
    stopped: &mut dyn FnMut() -> bool,
) -> Result<Output, String> {
    let mut cmd = command(root, git, tool, args)?;
    aterm_uds::deadline::output_until_complete_stdout_from_unless(
        &mut cmd,
        aterm_uds::spawnfd::Stdio::null(),
        until,
        stopped,
    )
    .map_err(|e| format!("{tool:?} remote operation: {e}"))
}

/// The command [`run`] runs, for a caller that waits on it its own way.
pub(crate) fn command(
    root: &Path,
    git: &dyn Fn(&Path) -> Command,
    tool: Tool,
    args: &[OsString],
) -> Result<Command, String> {
    let mut cmd = match tool {
        Tool::Git => {
            let mut cmd = git(root);
            // Local discovery and snapshot reads answer for this checkout, even
            // when the caller inherited another repository's Git environment.
            for (key, _) in std::env::vars_os() {
                if key.as_encoded_bytes().starts_with(b"GIT_") {
                    cmd.env_remove(key);
                }
            }
            cmd.env("GIT_NO_REPLACE_OBJECTS", "1")
                .env("GIT_NO_LAZY_FETCH", "1")
                .env("GIT_TERMINAL_PROMPT", "0");
            cmd
        }
        Tool::Agh => {
            #[cfg(target_os = "linux")]
            let image = std::path::PathBuf::from("/proc/self/exe");
            #[cfg(target_os = "macos")]
            let image = crate::remote_host::image()?;
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            let image = std::env::current_exe()
                .map_err(|e| format!("cannot name the remote transport host: {e}"))?;
            let mut cmd = aterm_uds::spawnfd::command(image);
            cmd.arg(crate::remote_cli::FLAG)
                .arg(std::process::id().to_string())
                .current_dir(root);
            cmd
        }
    };
    cmd.args(args);
    Ok(cmd)
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
    /// Another atomic batch prevented this attempt from sending anything.
    Blocked {
        intent: agh::IntentId,
        why: String,
    },
}

/// Read the machine diagnostic, never its human `why` or suggested command. Unknown outcomes
/// without this exact pre-send refusal remain fatal, including older agh diagnostics.
fn blocking_intent(out: &Output, repo: &str) -> Option<agh::IntentId> {
    if out.status.code() != Some(13) {
        return None;
    }
    #[derive(serde::Deserialize)]
    struct Diagnostic {
        error: Refusal,
    }
    #[derive(serde::Deserialize)]
    struct Refusal {
        v: u8,
        exit: u8,
        kind: String,
        sub: String,
        side_effect: String,
        repo: String,
        intent: String,
    }
    let mut records = out
        .stderr
        .split(|byte| *byte == b'\n')
        .map(<[u8]>::trim_ascii)
        .filter(|line| line.starts_with(b"{") || line.starts_with(b"["))
        .map(aterm_json::from_slice::<Diagnostic>);
    let diagnostic = records.next()?.ok()?;
    if records.next().is_some() {
        return None;
    }
    let error = diagnostic.error;
    if error.v == 1
        && error.exit == 13
        && error.kind == "outcome_unknown"
        && error.sub == "intent_open"
        && error.side_effect == "none"
        && error.repo == repo
    {
        agh::IntentId::parse(&error.intent)
    } else {
        None
    }
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

    /// Reject a source agh cannot push before the caller spends time judging it.
    /// Local Git remotes do not impose agh's complete-history requirement.
    pub(crate) fn admit_push_source(&self) -> Result<(), String> {
        if matches!(self.kind, Kind::Local(_)) {
            return Ok(());
        }
        let stdout = success(
            Tool::Git,
            (self.run)(Tool::Git, &words(&["rev-parse", "--is-shallow-repository"]))?,
        )?;
        agh::git::require_complete_source(&stdout).map_err(|error| error.to_string())
    }

    /// Refresh ordinary live main through agh's incremental snapshot operation.
    /// Pinned provenance reads still use [`Self::fetch`]. A failed refresh never
    /// turns a previous local snapshot into a successful current observation.
    pub(crate) fn refresh_main(&self) -> Result<String, String> {
        let Kind::Github(slug) = &self.kind else {
            return self
                .fetch("refs/heads/main")?
                .ok_or_else(|| "remote main is absent".into());
        };
        const INTO: &str = "refs/agh/fetched-main";
        let direct = (self.run)(Tool::Git, &words(&["symbolic-ref", "--quiet", INTO]))?;
        match direct.status.code() {
            Some(1) => {}
            Some(0) => return Err("agh's main snapshot ref is symbolic".into()),
            _ => return Err("cannot inspect agh's main snapshot ref".into()),
        }
        let mut args = words(&["repo", "fetch-main", slug]);
        args.push(self.root.as_os_str().to_owned());
        success(Tool::Agh, (self.run)(Tool::Agh, &args)?)?;
        let out = success(
            Tool::Git,
            (self.run)(Tool::Git, &words(&["rev-parse", "--verify", INTO]))?,
        )?;
        let tip = std::str::from_utf8(&out)
            .map_err(|_| "agh's main snapshot is not UTF-8")?
            .trim();
        if !aterm_types::land_trailer::object_id(tip) {
            return Err("agh's main snapshot is not one exact object ID".into());
        }
        let kind = success(
            Tool::Git,
            (self.run)(Tool::Git, &words(&["cat-file", "-t", tip]))?,
        )?;
        if kind != b"commit\n" {
            return Err("agh's main snapshot is not a commit object".into());
        }
        Ok(tip.into())
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
                let out = success(
                    Tool::Agh,
                    (self.run)(
                        Tool::Agh,
                        &words(&["repo", "refs", slug, "--ref", reference]),
                    )?,
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
                let sha = row["oid"].as_str().ok_or("agh ref has no object ID")?;
                if found.next().is_some() || !aterm_types::land_trailer::object_id(sha) {
                    return Err("agh did not return one exact commit ID".into());
                }
                // The ref can advance after observation; import the immutable snapshot itself.
                self.agh_commit(slug, sha, &format!("refs/agh/verify/{name}"))?;
                Ok(Some(sha.to_owned()))
            }
        }
    }

    /// Import the exact commit `sha` and what it reaches, publishing it at `into` alone: no
    /// branch, index or checkout moves. Unlike [`Self::fetch`], `sha` need be no ref's tip, as a
    /// superproject's gitlink can pin any commit the remote holds. A local remote is asked over
    /// the file transport, GitHub through agh.
    pub(crate) fn import_commit(&self, sha: &str, into: &str) -> Result<(), String> {
        if !aterm_types::land_trailer::object_id(sha) {
            return Err("a commit import needs one exact object ID".into());
        }
        match &self.kind {
            Kind::Local(_) => {
                let wanted = format!("+{sha}:{into}");
                success(
                    Tool::Git,
                    (self.run)(
                        Tool::Git,
                        &local_words(&[
                            "fetch",
                            "--quiet",
                            "--no-tags",
                            "--no-write-fetch-head",
                            "--recurse-submodules=no",
                            self.name,
                            &wanted,
                        ]),
                    )?,
                )?;
                self.require_commit(sha)
            }
            Kind::Github(slug) => self.agh_commit(slug, sha, into),
        }
    }

    /// Import commit `sha` from GitHub's `slug` through agh into ref `into`, then require that
    /// the object arrived as a commit.
    fn agh_commit(&self, slug: &str, sha: &str, into: &str) -> Result<(), String> {
        let dir = aterm_tempfile::tempdir().map_err(|e| e.to_string())?;
        let input = dir.path().join("fetch.json");
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
        self.require_commit(sha)
    }

    /// `sha` is a commit object in this repository.
    fn require_commit(&self, sha: &str) -> Result<(), String> {
        let object = success(
            Tool::Git,
            (self.run)(Tool::Git, &words(&["cat-file", "-t", sha]))?,
        )?;
        if object == b"commit\n" {
            Ok(())
        } else {
            Err(format!("{sha} is not a commit object"))
        }
    }

    /// Import exact raw objects (a partial clone's missing blobs), publishing no ref: the
    /// explicit, bounded counterpart of the lazy fetch every local read here forbids. A local
    /// remote is asked as git's own lazy fetch asks it (no negotiation, no tags, no
    /// `FETCH_HEAD`). GitHub goes through agh's routed object import, into a scratch bare
    /// repository: agh refuses to run git in a partial clone, whose promisor keys sit outside
    /// its transport policy, so the objects then cross from the scratch over the file transport.
    pub(crate) fn import(&self, objects: &[String]) -> Result<(), String> {
        if objects.is_empty() {
            return Ok(());
        }
        if objects.len() > agh::git::FETCH_ROWS_MAX
            || !objects
                .iter()
                .all(|oid| aterm_types::land_trailer::object_id(oid))
        {
            return Err("an object import needs 1–2048 exact object IDs".into());
        }
        match &self.kind {
            Kind::Local(_) => self.objects_from(self.name.as_ref(), objects),
            Kind::Github(slug) => {
                let dir = aterm_tempfile::tempdir().map_err(|e| e.to_string())?;
                let input = dir.path().join("objects.json");
                std::fs::write(&input, json!(objects).to_string()).map_err(|e| e.to_string())?;
                let scratch = dir.path().join("objects.git");
                let mut init = words(&["init", "--quiet", "--bare"]);
                init.push(scratch.clone().into_os_string());
                success(Tool::Git, (self.run)(Tool::Git, &init)?)?;
                let mut config = vec![OsString::from("--git-dir"), scratch.clone().into()];
                config.extend(words(&["config", "uploadpack.allowAnySHA1InWant", "true"]));
                success(Tool::Git, (self.run)(Tool::Git, &config)?)?;
                let mut args = words(&["repo", "fetch", slug]);
                args.extend([
                    scratch.clone().into_os_string(),
                    "--objects".into(),
                    input.into_os_string(),
                ]);
                success(Tool::Agh, (self.run)(Tool::Agh, &args)?)?;
                self.objects_from(scratch.as_os_str(), objects)
            }
        }
    }

    /// Fetch exact `objects` from the local repository `source` (a remote's name or a path)
    /// into this checkout, over the file transport only and with no ref, tag or `FETCH_HEAD`.
    fn objects_from(&self, source: &std::ffi::OsStr, objects: &[String]) -> Result<(), String> {
        let mut args = local_words(&[
            "-c",
            "fetch.negotiationAlgorithm=noop",
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-write-fetch-head",
            "--recurse-submodules=no",
        ]);
        args.push(source.to_owned());
        args.extend(objects.iter().map(OsString::from));
        success(Tool::Git, (self.run)(Tool::Git, &args)?).map(drop)
    }

    /// Publish exact local objects, expected ref included. An unknown outcome stops the caller;
    /// only a typed refusal which sent nothing permits observation of the blocking intent.
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
                        } else if let Some(intent) = blocking_intent(&out, slug) {
                            Ok(Pushed::Blocked { intent, why })
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

    /// Observe only the exact foreign journal entry which refused our pre-send attempt. The
    /// caller owns the deadline; neither journal reads nor superseding writers extend it.
    pub(crate) fn wait_for_intent(
        &self,
        intent: &agh::IntentId,
        pause: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        let Kind::Github(slug) = &self.kind else {
            return Err("a local remote has no agh intent".into());
        };
        let intent = intent.as_str();
        loop {
            let args = words(&["intent", "show", slug, intent, "--format", "json"]);
            let bytes = success(Tool::Agh, (self.run)(Tool::Agh, &args)?)?;
            #[derive(serde::Deserialize)]
            struct Intent {
                id: String,
                repo: String,
                op: String,
                key: String,
                open: bool,
            }
            let doc: Intent = aterm_json::from_slice(&bytes)
                .map_err(|_| "agh intent returned invalid JSON".to_owned())?;
            if doc.id != intent
                || doc.repo != *slug
                || doc.op != "ref.update-atomic"
                || doc.key != "atomic refs"
            {
                return Err("agh intent identity does not match the refused atomic push".into());
            }
            if !doc.open {
                return Ok(());
            }
            pause()?;
        }
    }
}

#[cfg(test)]
#[path = "remote_tests.rs"]
mod tests;
