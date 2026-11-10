// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Dev-only real Git pass-through for the abrupt-stop integration test.

#[cfg(unix)]
fn main() -> std::io::Result<()> {
    use aterm_uds::spawnfd::{self, Stdio};
    use std::io::Read as _;
    use std::path::{Path, PathBuf};

    fn line(root: &Path, name: &str) -> std::io::Result<String> {
        let mut bytes = Vec::new();
        std::fs::File::open(root.join(name))?
            .take(8193)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 8192 || bytes.last() != Some(&b'\n') {
            return Err(std::io::Error::other("invalid fixture data length"));
        }
        bytes.pop();
        if bytes.contains(&b'\n') || bytes.contains(&b'\r') || bytes.contains(&0) {
            return Err(std::io::Error::other("invalid fixture data line"));
        }
        String::from_utf8(bytes).map_err(std::io::Error::other)
    }

    let mut args = std::env::args_os();
    let invocation = PathBuf::from(
        args.next()
            .ok_or_else(|| std::io::Error::other("missing argv0"))?,
    );
    let root = invocation
        .parent()
        .filter(|root| root.is_absolute())
        .ok_or_else(|| std::io::Error::other("fixture needs an absolute invocation path"))?;
    let program = PathBuf::from(line(root, "program")?);
    if !program.is_absolute() {
        return Err(std::io::Error::other("fixture Git must be absolute"));
    }
    let args = args.collect::<Vec<_>>();
    // The actual Git preflight (including certificate defaults) is unchanged.
    if !args.iter().any(|arg| arg == "ls-remote") {
        return Err(spawnfd::transparent(program).args(args).exec());
    }
    let repo = PathBuf::from(line(root, "repo")?);
    let url = line(root, "url")?;
    let port = url
        .strip_prefix("http://127.0.0.1:")
        .and_then(|tail| tail.strip_suffix("/fixture.git"))
        .and_then(|port| port.parse::<u16>().ok());
    if !repo.is_absolute() || !matches!(port, Some(1..)) {
        return Err(std::io::Error::other(
            "fixture needs its owned repository and loopback URL",
        ));
    }
    // Preserve this PID and its inherited process group across the real Git exec.
    std::fs::write(root.join("pid"), format!("{}\n", std::process::id()))?;
    let mut command = spawnfd::transparent(program).into_command();
    command
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "protocol.http.allow=always",
            "-c",
            "fetch.unpackLimit=1",
            "-c",
            "protocol.version=0",
            "fetch",
            "--quiet",
            "--no-progress",
            "--no-tags",
            "--no-write-fetch-head",
        ])
        .arg(url)
        .arg("+refs/heads/main:refs/agh/test")
        .env("GIT_TRACE2_EVENT", root.join("git-trace.json"))
        .stdout(Stdio::from(std::fs::File::create(
            root.join("native.stdout"),
        )?))
        .stderr(Stdio::from(std::fs::File::create(
            root.join("native.stderr"),
        )?));
    Err(command.exec())
}

#[cfg(not(unix))]
fn main() {}
