// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Image and original input custody retained by the existing service owner.
//! Wire build numbers select admitted resources; they never admit an image.
use super::*;
use std::os::unix::ffi::OsStrExt;

pub(super) struct Images {
    custody: ImageCustody,
    source: aterm_agent::harness::native_act::Image,
    target: aterm_agent::harness::native_act::Image,
}
enum ImageCustody {
    Published {
        source: atpkg::prerequisite::HeldTool,
        target: atpkg::prerequisite::HeldTool,
    },
    #[cfg(any(test, debug_assertions))]
    Proof(Arc<aterm_agent::harness::codex_enrollment::SourceProofLaunch>),
}
impl Images {
    pub(super) fn resolve(
        policy: &crate::native_session_policy::SessionPolicy,
        source: &Process,
        source_build: u64,
        target_build: u64,
    ) -> Result<Self, registry::Error> {
        if !policy.retained_constructor_enabled() {
            return Err(registry::Error::Unsupported);
        }
        #[cfg(any(test, debug_assertions))]
        if let Some(launch) = policy
            .fixture
            .as_ref()
            .and_then(|fixture| fixture.retained_proof.as_ref())
        {
            if !launch.native_transport_only() {
                return Err(registry::Error::Denied);
            }
            // These two selectors exist ONLY in this authenticated proof world.
            // The original opaque CandidateLeases, not the integers, admit A/B.
            if source_build != 1 || target_build != 2 {
                return Err(registry::Error::Denied);
            }
            launch.current().map_err(|_| registry::Error::Stale)?;
            let images = launch
                .retained_images()
                .map_err(|_| registry::Error::Stale)?;
            if images.len() != 2 || images[0] != source.image || images[0] == images[1] {
                return Err(registry::Error::Denied);
            }
            let result = Self {
                source: images[0].clone(),
                target: images[1].clone(),
                custody: ImageCustody::Proof(launch.clone()),
            };
            result.validate()?;
            result.compatible()?;
            result.validate()?;
            return Ok(result);
        }
        let rollback = atpkg::prerequisite::configured_tool(
            atpkg::prerequisite::Prerequisite::Codex,
            source_build,
            "retained frontend original image custody",
        )
        .map_err(|_| registry::Error::Unavailable)?;
        let target = atpkg::prerequisite::configured_tool(
            atpkg::prerequisite::Prerequisite::Codex,
            target_build,
            "retained frontend replacement image custody",
        )
        .map_err(|_| registry::Error::Unavailable)?;
        let original = aterm_agent::harness::native_act::Image::at(rollback.physical_path())
            .ok_or(registry::Error::Unavailable)?;
        let replacement = aterm_agent::harness::native_act::Image::at(target.physical_path())
            .ok_or(registry::Error::Unavailable)?;
        if original != source.image || original == replacement {
            return Err(registry::Error::Denied);
        }
        let result = Self {
            source: original,
            target: replacement,
            custody: ImageCustody::Published {
                source: rollback,
                target,
            },
        };
        result.validate()?;
        result.compatible()?;
        result.validate()?;
        Ok(result)
    }
    pub(super) fn target(&self) -> &aterm_agent::harness::native_act::Image {
        &self.target
    }
    pub(super) fn app_components(&self) -> bool {
        match &self.custody {
            ImageCustody::Published { .. } => true,
            // The only current opaque fixture purpose is Native transport.
            // A whole-App driver needs its own immutable admission; request
            // or argv DATA cannot inflate this launch's component support.
            #[cfg(any(test, debug_assertions))]
            ImageCustody::Proof(_) => false,
        }
    }
    pub(super) fn validate(&self) -> Result<(), registry::Error> {
        match &self.custody {
            ImageCustody::Published { source, target } => {
                source.validate().map_err(|_| registry::Error::Stale)?;
                target.validate().map_err(|_| registry::Error::Stale)?;
            }
            #[cfg(any(test, debug_assertions))]
            ImageCustody::Proof(launch) => launch.current().map_err(|_| registry::Error::Stale)?,
        }
        for image in [&self.source, &self.target] {
            let path = std::path::Path::new(std::ffi::OsStr::from_bytes(&image.path));
            if aterm_agent::harness::native_act::Image::at(path).as_ref() != Some(image) {
                return Err(registry::Error::Stale);
            }
        }
        Ok(())
    }
    fn compatible(&self) -> Result<(), registry::Error> {
        // This is only compatibility DATA from an independently admitted image.
        // It grants no full App, Source, constructor or scheduling permission.
        let path = std::path::Path::new(std::ffi::OsStr::from_bytes(&self.target.path));
        let mut command = match &self.custody {
            ImageCustody::Published { target, .. } => {
                target.command().map_err(|_| registry::Error::Stale)?
            }
            #[cfg(any(test, debug_assertions))]
            ImageCustody::Proof(_) => aterm_uds::spawnfd::command(path),
        };
        command.args(["debug", "upgrade-capabilities"]);
        let (pid, output) = capability_output(&mut command)?;
        let proof = match &self.custody {
            ImageCustody::Published { .. } => false,
            #[cfg(any(test, debug_assertions))]
            ImageCustody::Proof(_) => true,
        };
        if !output.status.success() {
            return Err(registry::Error::Unsupported);
        }
        capability_reply(&output.stdout, pid, &self.target, proof)
    }
}

/// Keep the actual spawned root distinct from the progress reader's live root.
/// Only the stream helper's successful result supplies both EOFs and an owning
/// wait; overflow, inactivity and every error retain its normal retirement.
fn capability_output(
    command: &mut aterm_uds::spawnfd::Command,
) -> Result<(u32, std::process::Output), registry::Error> {
    use aterm_uds::deadline::{ChildOutput, ProcessProgressProbe, ProgressWindow};
    use std::cell::Cell;
    const LIMIT: usize = 16 * 1024;
    let spawned = Cell::new(None);
    let live = Cell::new(None);
    let bytes = Cell::new(0u64);
    let mut progress = ProgressWindow::new(Duration::from_secs(60), Instant::now());
    let mut probe = ProcessProgressProbe::default();
    let mut streams = [Vec::new(), Vec::new()];
    let status = aterm_uds::deadline::stream_until_stopped_observing_exit(
        command,
        None,
        |pid| {
            spawned.set(Some(pid));
            live.set(Some(pid));
        },
        |stream, chunk| {
            let index = match stream {
                ChildOutput::Stdout => 0,
                ChildOutput::Stderr => 1,
            };
            if chunk.len() > LIMIT.saturating_sub(streams[index].len()) {
                return Err(io::ErrorKind::InvalidData.into());
            }
            streams[index].extend_from_slice(chunk);
            bytes.set(bytes.get().saturating_add(chunk.len() as u64));
            Ok(())
        },
        || {
            progress
                .observe(Instant::now(), bytes.get(), || {
                    live.get().map_or(Some(0), |pid| probe.read(pid))
                })
                .is_zero()
        },
        |_| live.set(None),
    )
    .map_err(|_| registry::Error::Unavailable)?
    .ok_or(registry::Error::Unavailable)?;
    let [stdout, stderr] = streams;
    Ok((
        spawned.get().ok_or(registry::Error::Unavailable)?,
        std::process::Output {
            status,
            stdout,
            stderr,
        },
    ))
}

fn capability_reply(
    bytes: &[u8],
    pid: u32,
    target: &aterm_agent::harness::native_act::Image,
    proof: bool,
) -> Result<(), registry::Error> {
    let value: aterm_json::Value =
        aterm_json::from_slice(bytes).map_err(|_| registry::Error::Unsupported)?;
    let extension = &value["extensions"][if proof {
        "alab.nativeRetainedTransportProof"
    } else {
        "alab.retainedImageCustody"
    }];
    if pid <= 1
        || value["protocol"].as_u64() != Some(3)
        || value["pid"].as_u64() != Some(u64::from(pid))
        || value["executable"].as_str().map(str::as_bytes) != Some(target.path.as_slice())
        || value["device"].as_u64() != Some(target.device)
        || value["inode"].as_u64() != Some(target.inode)
        || extension.as_object().is_none_or(|fields| fields.len() != 4)
        || extension["e1Version"].as_u64() != Some(2)
        || extension["imageCustody"].as_u64() != Some(1)
        || extension["preThreadPin"].as_u64() != Some(1)
        || if proof {
            extension["version"].as_u64() != Some(1)
        } else {
            extension["clipboardCustody"].as_u64() != Some(1)
        }
    {
        return Err(registry::Error::Unsupported);
    }
    Ok(())
}

#[cfg(test)]
#[path = "codex_retained_replacement_tests.rs"]
mod tests;

const INPUT_LIMIT: usize = 256 * 1024 * 1024;

/// All reads keep ancillary data visible: unexpected rights are not silently
/// discarded by a plain Read. One original deadline covers the whole exchange.
struct SocketInput<'a> {
    stream: &'a aterm_uds::CtlStream,
    deadline: Instant,
}
impl Read for SocketInput<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        use std::os::fd::AsFd;
        loop {
            aterm_uds::ready::until(
                self.stream.as_fd(),
                aterm_uds::ready::Interest::Read,
                self.deadline,
            )?;
            match aterm_uds::fdpass::recv_with_fds(self.stream, bytes, 0) {
                Ok(got) if got.fds.is_empty() => return Ok(got.bytes),
                Ok(_) => return Err(io::ErrorKind::InvalidData.into()),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                    ) => {}
                Err(error) => return Err(error),
            }
        }
    }
}
pub(super) fn receive_settings(
    callback: &aterm_uds::CtlStream,
    deadline: Instant,
) -> Result<Settings, registry::Error> {
    callback
        .set_nonblocking(true)
        .map_err(|_| registry::Error::Unavailable)?;
    Settings::read(&mut SocketInput {
        stream: callback,
        deadline,
    })
    .map_err(|_| registry::Error::Unavailable)
}
pub(super) fn receive_input(
    callback: &aterm_uds::CtlStream,
    deadline: Instant,
) -> Result<Vec<u8>, registry::Error> {
    use std::io::Write;
    use std::os::fd::AsFd;
    callback
        .set_nonblocking(true)
        .map_err(|_| registry::Error::Unavailable)?;
    let nonce = aterm_uds::rand::hex_token::<16>().map_err(|_| registry::Error::Unavailable)?;
    let challenge = format!("I1{nonce}");
    let mut offset = 0;
    while offset < challenge.len() {
        aterm_uds::ready::until(
            callback.as_fd(),
            aterm_uds::ready::Interest::Write,
            deadline,
        )
        .map_err(|_| registry::Error::Unavailable)?;
        match (&*callback).write(&challenge.as_bytes()[offset..]) {
            Ok(0) => return Err(registry::Error::Unavailable),
            Ok(count) => offset += count,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) => {}
            Err(_) => return Err(registry::Error::Unavailable),
        }
    }
    callback
        .shutdown(std::net::Shutdown::Write)
        .map_err(|_| registry::Error::Unavailable)?;
    let mut input = SocketInput {
        stream: callback,
        deadline,
    };
    let mut header = [0; 40];
    input
        .read_exact(&mut header)
        .map_err(|_| registry::Error::Unavailable)?;
    if !aterm_digest::ct_eq(&header[..32], nonce.as_bytes()) {
        return Err(registry::Error::Denied);
    }
    let size = usize::try_from(u64::from_be_bytes(
        header[32..]
            .try_into()
            .map_err(|_| registry::Error::Denied)?,
    ))
    .map_err(|_| registry::Error::Denied)?;
    if size == 0 || size > INPUT_LIMIT {
        return Err(registry::Error::Denied);
    }
    let mut body = vec![0; size];
    input
        .read_exact(&mut body)
        .map_err(|_| registry::Error::Unavailable)?;
    if input
        .read(&mut [0])
        .map_err(|_| registry::Error::Unavailable)?
        != 0
    {
        return Err(registry::Error::Denied);
    }
    Ok(body)
}
