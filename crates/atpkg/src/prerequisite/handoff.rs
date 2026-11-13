// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Transfer original signed executable custody to a local updater. The receiver
//! must retain the lease without unlocking it, and require the sender's success.

use std::io::Write;
use std::os::fd::AsFd;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

/// Versioned receipt for two SCM_RIGHTS descriptors: original lease, then image.
/// Authentication comes from the native admitting sender, never these JSON bytes alone.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub protocol: u32,
    pub program: String,
    pub build: u64,
    pub version: String,
    pub root: PathBuf,
    pub executable: PathBuf,
    pub tree_root: String,
    pub device: u64,
    pub inode: u64,
    pub size: u64,
    pub modified_seconds: i64,
    pub modified_nanoseconds: i64,
    pub lease_path: PathBuf,
    pub lease_device: u64,
    pub lease_inode: u64,
}

impl super::HeldTool {
    /// Send one bounded receipt and original custody over an already connected
    /// local socket. The caller owns peer authentication and waits for this
    /// operation to succeed. A partial send never means an admitted handoff.
    ///
    /// # Errors
    /// Changed custody/image, an oversized receipt, or failed descriptor/frame transport.
    pub fn send_handoff(&self, stream: &mut aterm_uds::CtlStream) -> Result<(), String> {
        self.validate()?;
        let lease =
            std::fs::File::from(self.lease_descriptor().map_err(|error| error.to_string())?);
        let lease_metadata = lease.metadata().map_err(|error| error.to_string())?;
        let image = self.image.try_clone().map_err(|error| error.to_string())?;
        let metadata = image.metadata().map_err(|error| error.to_string())?;
        let receipt = Receipt {
            protocol: 1,
            program: self.program().into(),
            build: self.build(),
            version: self.version().into(),
            root: self.root().into(),
            executable: self.physical_path().into(),
            tree_root: self.tree_root().into(),
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            lease_path: self.lease.path().into(),
            lease_device: lease_metadata.dev(),
            lease_inode: lease_metadata.ino(),
        };
        let mut bytes = aterm_json::to_vec(&receipt).map_err(|error| error.to_string())?;
        if bytes.len() > 16 * 1024 {
            return Err("the executable handoff receipt is too large".into());
        }
        bytes.push(b'\n');
        self.validate()?;
        let sent = aterm_uds::fdpass::send_with_fds(stream, b"X", &[lease.as_fd(), image.as_fd()])
            .map_err(|error| error.to_string())?;
        if sent != 1 {
            return Err("the executable custody marker was not sent".into());
        }
        stream.write_all(&bytes).map_err(|error| error.to_string())
    }
}

fn request(args: &[String]) -> Result<(super::Prerequisite, u64, PathBuf), String> {
    let [tool_flag, tool, socket_flag, socket] = args else {
        return Err("expected --tool <name>=<build> --socket <absolute-path>".into());
    };
    if tool_flag != "--tool" || socket_flag != "--socket" {
        return Err("expected --tool <name>=<build> --socket <absolute-path>".into());
    }
    let (name, build) = tool
        .split_once('=')
        .ok_or("an exact named build is required")?;
    let want = super::Prerequisite::named(name).ok_or("unsupported signed executable")?;
    let build = build
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or("a positive exact build is required")?;
    let socket = PathBuf::from(socket);
    if !socket.is_absolute() {
        return Err("the handoff socket must be absolute".into());
    }
    Ok((want, build, socket))
}

pub(crate) fn command(args: &[String]) -> Result<(), String> {
    let (want, build, socket) = request(args)?;
    // The caller creates the private listening socket before spawning this child.
    // Authenticate that original parent before any package/network operation.
    // SAFETY: getppid reads this process's parent identity without pointers.
    let parent = u32::try_from(unsafe { libc::getppid() }).map_err(|error| error.to_string())?;
    if parent <= 1 {
        return Err("the executable handoff has no original parent".into());
    }
    let mut stream = aterm_uds::CtlStream::connect(socket).map_err(|error| error.to_string())?;
    if aterm_uds::fdpass::peer_pid(&stream).map_err(|error| error.to_string())? != parent {
        return Err("the executable handoff socket is not owned by the original parent".into());
    }
    let tool = super::configured_tool(
        want,
        build,
        &format!("executable handoff to parent {parent}"),
    )?;
    tool.send_handoff(&mut stream)?;
    // Keep the authenticated child and its connected socket alive until the
    // original parent has received and validated custody. Darwin cannot always
    // report LOCAL_PEERPID after that peer has closed its end of the socket.
    acknowledge(
        &stream,
        std::time::Instant::now() + std::time::Duration::from_secs(30),
    )
}

fn acknowledge(stream: &aterm_uds::CtlStream, deadline: std::time::Instant) -> Result<(), String> {
    let mut byte = [0];
    match aterm_uds::hangup::read_until(stream, &mut byte, deadline) {
        Ok(1) if byte == *b"K" => Ok(()),
        Ok(_) => Err("the original parent did not acknowledge executable custody".into()),
        Err(error) => Err(format!(
            "executable custody acknowledgement failed: {error}"
        )),
    }
}

#[cfg(test)]
#[path = "handoff_tests.rs"]
mod tests;
