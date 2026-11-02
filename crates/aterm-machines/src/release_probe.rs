// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Ordinary Git transport for the explicit release probe, retaining effective owner routes.

use aterm_secrets::client::release::probe::{EffectError, Git, Invocation, Node};
use aterm_secrets::{Error, Result};
use aterm_uds::spawnfd::Command;
use std::path::PathBuf;
use std::process::Output;
use std::sync::Arc;

/// Ordinary configured Git for an explicit owned capability probe.
pub struct Ordinary {
    node: Arc<dyn Fn() -> Result<Node> + Send + Sync>,
    program: Option<PathBuf>,
    #[cfg(test)]
    isolated_home: Option<PathBuf>,
}

impl Ordinary {
    /// Use installed Git and an executable-owned reader of the existing fleet identity.
    #[must_use]
    pub fn new(node: Arc<dyn Fn() -> Result<Node> + Send + Sync>) -> Self {
        Self {
            node,
            program: crate::items::github::installed_git(&crate::port::Live::new()),
            #[cfg(test)]
            isolated_home: None,
        }
    }

    fn base(&self, invocation: &Invocation<'_>) -> Result<Command> {
        let program = self
            .program
            .as_ref()
            .ok_or_else(|| Error::new("probe awaits installed Git"))?;
        let mut command = aterm_uds::spawnfd::command(program);
        command
            .arg("-C")
            .arg(invocation.scratch)
            .args(["-c", "maintenance.auto=false", "-c", "gc.auto=0"])
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GCM_INTERACTIVE", "never")
            .env("GIT_ASKPASS", "/usr/bin/false")
            .env("SSH_ASKPASS_REQUIRE", "never");
        for name in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_QUARANTINE_PATH",
            "GIT_NAMESPACE",
        ] {
            command.env_remove(name);
        }
        #[cfg(test)]
        if let Some(home) = &self.isolated_home {
            command
                .env("HOME", home)
                .env("XDG_CONFIG_HOME", home.join(".config"))
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env_remove("GIT_CONFIG_GLOBAL")
                .env_remove("GIT_CONFIG_PARAMETERS")
                .env_remove("GIT_CONFIG_COUNT");
        }
        Ok(command)
    }

    fn run(&self, invocation: &Invocation<'_>, command: &mut Command) -> Result<Output> {
        (invocation.current)()?;
        if std::time::Instant::now() >= invocation.deadline {
            return Err(Error::new("probe Git deadline"));
        }
        aterm_uds::deadline::output_until_complete_stdout(command, invocation.deadline)
            .map_err(|_| Error::new("bounded probe Git command did not complete"))
    }

    fn effect(
        &self,
        invocation: &Invocation<'_>,
        command: &mut Command,
    ) -> std::result::Result<Output, EffectError> {
        (invocation.current)().map_err(|_| EffectError::NotStarted)?;
        if std::time::Instant::now() >= invocation.deadline {
            return Err(EffectError::NotStarted);
        }
        let started = std::cell::Cell::new(false);
        aterm_uds::deadline::output_until_complete_stdout_observed(
            command,
            invocation.deadline,
            || started.set(true),
        )
        .map_err(|error| match error {
            aterm_uds::deadline::CompleteOutputError::Run(
                aterm_uds::deadline::RunError::Spawn(_),
            ) if !started.get() => EffectError::NotStarted,
            _ => EffectError::Uncertain,
        })
    }

    fn read(&self, invocation: &Invocation<'_>, args: &[&str]) -> Result<String> {
        let output = self.run(invocation, self.base(invocation)?.args(args))?;
        if !output.status.success() {
            return Err(Error::new("probe Git read was refused"));
        }
        std::str::from_utf8(&output.stdout)
            .map(|text| text.trim().to_owned())
            .map_err(|_| Error::new("probe Git returned non-UTF-8 metadata"))
    }

    fn prepare(&self, invocation: &Invocation<'_>) -> Result<()> {
        (invocation.current)()?;
        aterm_uds::private_file::check_private_dir(invocation.scratch)?;
        owned_repo(invocation.scratch)?;
        let marker = invocation.scratch.join("aterm-probe-owner");
        let parent = invocation
            .scratch
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|p| p.to_str())
            .ok_or_else(|| Error::new("probe Git directory has no transaction identity"))?;
        let expected = format!("{parent}\n{}/aterm\n", invocation.owner);
        match aterm_secrets::fsx::read_private(&marker, 256)? {
            Some(bytes) if bytes.as_slice() == expected.as_bytes() => {}
            Some(_) => {
                return Err(Error::new(
                    "probe Git directory belongs to another transaction",
                ));
            }
            None => {
                if std::fs::read_dir(invocation.scratch)?.next().is_some() {
                    return Err(Error::new("probe Git directory is not empty"));
                }
                aterm_secrets::fsx::write_new(&marker, expected.as_bytes(), 0o600)?;
            }
        }
        if !invocation.scratch.join("HEAD").exists() {
            let template = invocation.scratch.join("aterm-empty-template");
            if !template.exists() {
                use std::os::unix::fs::DirBuilderExt as _;
                std::fs::DirBuilder::new().mode(0o700).create(&template)?;
            }
            aterm_uds::private_file::check_private_dir(&template)?;
            if std::fs::read_dir(&template)?.next().is_some() {
                return Err(Error::new("probe Git template is not empty"));
            }
            let output = self.run(
                invocation,
                self.base(invocation)?
                    .args(["init", "--bare", "--template"])
                    .arg(&template)
                    .arg(invocation.scratch),
            )?;
            if !output.status.success() {
                return Err(Error::new("probe Git initialization failed"));
            }
        }
        owned_repo(invocation.scratch)?;
        let output = self.read(invocation, &["config", "--local", "--list"])?;
        for line in output.lines() {
            if !matches!(
                line,
                "core.repositoryformatversion=0"
                    | "core.filemode=true"
                    | "core.bare=true"
                    | "core.ignorecase=true"
                    | "core.precomposeunicode=true"
            ) && line
                != format!(
                    "remote.origin.url=https://github.com/{}/aterm.git",
                    invocation.owner
                )
            {
                return Err(Error::new("probe Git directory has foreign configuration"));
            }
        }
        let origin = format!("https://github.com/{}/aterm.git", invocation.owner);
        let output = self.run(
            invocation,
            self.base(invocation)?
                .args(["config", "--local", "remote.origin.url", &origin]),
        )?;
        if !output.status.success() {
            return Err(Error::new("probe Git origin could not be set"));
        }
        Ok(())
    }

    fn network(&self, invocation: &Invocation<'_>) -> Result<Command> {
        self.prepare(invocation)?;
        if ["GIT_SSH", "GIT_SSH_COMMAND", "GIT_SSH_VARIANT"]
            .iter()
            .any(|name| std::env::var_os(name).is_some())
        {
            return Err(Error::new(
                "probe cannot assert no-prompt behavior for a custom SSH environment",
            ));
        }
        let output = self.run(
            invocation,
            self.base(invocation)?
                .args(["config", "--get", "core.sshCommand"]),
        )?;
        if output.status.code() != Some(1) {
            return Err(Error::new(
                "probe cannot assert no-prompt behavior for a custom SSH command",
            ));
        }
        for args in [
            &["remote", "get-url", "--all", "origin"][..],
            &["remote", "get-url", "--push", "--all", "origin"][..],
        ] {
            let routes = self.read(invocation, args)?;
            let mut lines = routes.lines();
            let route = lines
                .next()
                .ok_or_else(|| Error::new("probe Git has no effective route"))?;
            if lines.next().is_some()
                || !crate::github_writer_origin(
                    route,
                    invocation.owner,
                    "aterm",
                    invocation.principal,
                )
            {
                return Err(Error::new(
                    "probe Git route names another repository or principal",
                ));
            }
        }
        let mut command = self.base(invocation)?;
        command.env("GIT_SSH_COMMAND", "/usr/bin/ssh -oBatchMode=yes -oPasswordAuthentication=no -oKbdInteractiveAuthentication=no -oStrictHostKeyChecking=yes -oUpdateHostKeys=no -oNumberOfPasswordPrompts=0 -oConnectTimeout=10 -oConnectionAttempts=1");
        Ok(command)
    }
}

fn owned_repo(root: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt as _;
    let mut pending = vec![root.to_path_buf()];
    let mut count = 0usize;
    while let Some(parent) = pending.pop() {
        for child in std::fs::read_dir(&parent)? {
            let path = child?.path();
            count += 1;
            let relative = path
                .strip_prefix(root)
                .map_err(|_| Error::new("probe Git path escaped"))?;
            let first = relative
                .components()
                .next()
                .and_then(|p| p.as_os_str().to_str())
                .unwrap_or("");
            let meta = std::fs::symlink_metadata(&path)?;
            if count > 10_000
                || relative.components().count() > 16
                || !matches!(
                    first,
                    "aterm-probe-owner"
                        | "aterm-empty-template"
                        | "HEAD"
                        | "config"
                        | "description"
                        | "objects"
                        | "refs"
                        | "hooks"
                        | "info"
                        | "branches"
                        | "FETCH_HEAD"
                        | "shallow"
                        | "packed-refs"
                        | "logs"
                )
                || meta.uid() != aterm_secrets::fsx::euid()
                || meta.mode() & 0o022 != 0
                || (!meta.is_file() && !meta.is_dir())
                || (meta.is_file() && meta.nlink() != 1)
                || relative == std::path::Path::new("objects/info/alternates")
                || (first == "hooks" && relative.components().count() > 1)
            {
                return Err(Error::new(
                    "probe Git directory contains foreign or unsafe data",
                ));
            }
            if meta.is_dir() {
                pending.push(path);
            }
        }
    }
    Ok(())
}

impl Git for Ordinary {
    fn node(&self) -> Result<Node> {
        (self.node)()
    }
    fn fetch_head(&self, invocation: &Invocation<'_>) -> Result<String> {
        let output = self.run(
            invocation,
            self.network(invocation)?
                .args(["fetch", "--no-tags", "--depth=1", "origin", "HEAD"]),
        )?;
        if !output.status.success() {
            return Err(Error::new("probe target HEAD fetch was refused"));
        }
        let oid = self.read(
            invocation,
            &["rev-parse", "--verify", "FETCH_HEAD^{commit}"],
        )?;
        if oid.len() != 40
            || !oid
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(Error::new("probe target HEAD is not a canonical commit"));
        }
        Ok(oid)
    }
    fn tag(&self, invocation: &Invocation<'_>, tag: &str) -> Result<Option<String>> {
        let reference = reference(tag)?;
        let output = self.run(
            invocation,
            self.network(invocation)?
                .args(["ls-remote", "--refs", "origin", &reference]),
        )?;
        if !output.status.success() {
            return Err(Error::new("probe tag read was refused"));
        }
        parse_ref(&output.stdout, &reference)
    }
    fn create_tag(
        &self,
        invocation: &Invocation<'_>,
        tag: &str,
        oid: &str,
    ) -> std::result::Result<(), EffectError> {
        let reference = reference(tag).map_err(|_| EffectError::NotStarted)?;
        if !valid_oid(oid) {
            return Err(EffectError::NotStarted);
        }
        let lease = format!("--force-with-lease={reference}:");
        let refspec = format!("{oid}:{reference}");
        let mut command = self
            .network(invocation)
            .map_err(|_| EffectError::NotStarted)?;
        let output = self.effect(
            invocation,
            command.args(["push", "--porcelain", &lease, "origin", &refspec]),
        )?;
        if !output.status.success()
            || self
                .tag(invocation, tag)
                .map_err(|_| EffectError::Uncertain)?
                .as_deref()
                != Some(oid)
        {
            return Err(EffectError::Uncertain);
        }
        Ok(())
    }
    fn delete_tag(
        &self,
        invocation: &Invocation<'_>,
        tag: &str,
        oid: &str,
    ) -> std::result::Result<(), EffectError> {
        let reference = reference(tag).map_err(|_| EffectError::NotStarted)?;
        if !valid_oid(oid) {
            return Err(EffectError::NotStarted);
        }
        let lease = format!("--force-with-lease={reference}:{oid}");
        let refspec = format!(":{reference}");
        let mut command = self
            .network(invocation)
            .map_err(|_| EffectError::NotStarted)?;
        let output = self.effect(
            invocation,
            command.args(["push", "--porcelain", &lease, "origin", &refspec]),
        )?;
        if !output.status.success()
            || self
                .tag(invocation, tag)
                .map_err(|_| EffectError::Uncertain)?
                .is_some()
        {
            return Err(EffectError::Uncertain);
        }
        Ok(())
    }
}

fn valid_oid(oid: &str) -> bool {
    oid.len() == 40
        && oid
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn reference(tag: &str) -> Result<String> {
    let suffix = tag
        .strip_prefix("aterm-verification/")
        .ok_or_else(|| Error::new("invalid probe tag"))?;
    if suffix.len() != 32
        || !suffix
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(Error::new("invalid probe tag"));
    }
    Ok(format!("refs/tags/{tag}"))
}

fn parse_ref(output: &[u8], expected: &str) -> Result<Option<String>> {
    let text =
        std::str::from_utf8(output).map_err(|_| Error::new("probe ref response is malformed"))?;
    let rows: Vec<_> = text.lines().filter(|line| !line.is_empty()).collect();
    if rows.is_empty() {
        return Ok(None);
    }
    if rows.len() != 1 {
        return Err(Error::new("probe ref response is ambiguous"));
    }
    let (oid, name) = rows[0]
        .split_once('\t')
        .ok_or_else(|| Error::new("probe ref response is malformed"))?;
    if name != expected
        || oid.len() != 40
        || !oid
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(Error::new("probe ref response names another object"));
    }
    Ok(Some(oid.to_owned()))
}

#[cfg(test)]
mod tests;
