// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Outgoing control-socket listener file and token identity across admitted handoffs.

use crate::control_auth::SocketPlan;
use aterm_uds::CtlListener;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

pub(crate) const ENV_IDENTITY: crate::handoff_env::HandoffName =
    crate::handoff_env::HandoffName::new("ATERM_HANDOFF_CONTROL_SOCKET_IDENTITY");
const MAX_WIRE_BYTES: usize = 2048;
#[cfg(unix)]
const MAX_TOKEN_BYTES: usize = 256;

/// No token plaintext is carried or formatted. Token file identity also rejects
/// an atomic replacement that happens to contain the same credential bytes.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SocketIdentity {
    schema: u32,
    path: String,
    /// The listener's working directory, for a relative socket path (LaunchServices does not
    /// inherit it). `None` for an absolute path, the wire shape older receivers read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    directory: Option<std::path::PathBuf>,
    device: u64,
    inode: u64,
    token_device: u64,
    token_inode: u64,
    token_sha256: [u8; 32],
}

impl SocketIdentity {
    /// Capture immediately after binding, using the token actually provisioned
    /// for this listener. A path alone, or a token read from a replacement file,
    /// cannot mint an outgoing witness.
    pub(crate) fn capture(
        plan: &SocketPlan,
        listener: &CtlListener,
        expected_token: &str,
    ) -> Option<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if !explicit_plan(plan)
                || listener.local_addr().ok()?.as_pathname()?
                    != std::path::Path::new(&plan.sock_path)
                || expected_token.is_empty()
                || expected_token.len() > MAX_TOKEN_BYTES
            {
                return None;
            }
            let socket = socket_metadata(&plan.sock_path)?;
            let token = read_token(plan)?;
            if token.digest != aterm_digest::Sha256::digest(expected_token.as_bytes()) {
                return None;
            }
            let identity = Self {
                schema: 1,
                path: plan.sock_path.clone(),
                directory: if std::path::Path::new(&plan.sock_path).is_relative() {
                    Some(std::env::current_dir().ok()?)
                } else {
                    None
                },
                device: socket.dev(),
                inode: socket.ino(),
                token_device: token.device,
                token_inode: token.inode,
                token_sha256: token.digest,
            };
            identity.encode()?;
            identity.matches_current(plan).then_some(identity)
        }
        #[cfg(not(unix))]
        {
            let _ = (plan, listener, expected_token);
            None
        }
    }

    /// Pure admission check. Filesystem revalidation belongs at the actual bind.
    pub(crate) fn matches_plan_path(&self, plan: &SocketPlan) -> bool {
        self.schema == 1 && explicit_plan(plan) && self.path == plan.sock_path
    }

    /// Requires the same socket vnode and the same token file and contents. Missing paths,
    /// symlinks, unsupported platforms and unreadable files refuse.
    pub(crate) fn matches_current(&self, plan: &SocketPlan) -> bool {
        self.matches_plan_path(plan) && self.matches_files(plan)
    }

    /// Restore a relative endpoint's binding directory after validating its socket and credential.
    ///
    /// # Safety
    /// Called only in single-threaded startup of an admitted Ready/Commit candidate: changing
    /// cwd must not redirect concurrent filesystem work.
    #[cfg(unix)]
    pub(crate) unsafe fn prepare_incoming_directory(&self, plan: &SocketPlan) -> bool {
        if !self.matches_plan_path(plan) {
            return false;
        }
        let Some(directory) = &self.directory else {
            return self.matches_current(plan);
        };
        if !directory.is_absolute() || !std::path::Path::new(&plan.sock_path).is_relative() {
            return false;
        }
        let Some(socket) = directory.join(&plan.sock_path).to_str().map(str::to_owned) else {
            return false;
        };
        let anchored = SocketPlan {
            sock_path: socket,
            token_path: directory.join(&plan.token_path),
            latest_link: None,
        };
        self.matches_files(&anchored)
            && std::env::set_current_dir(directory).is_ok()
            && self.matches_current(plan)
    }

    fn matches_files(&self, plan: &SocketPlan) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let Some(socket) = socket_metadata(&plan.sock_path) else {
                return false;
            };
            if (socket.dev(), socket.ino()) != (self.device, self.inode) {
                return false;
            }
            let Some(token) = read_token(plan) else {
                return false;
            };
            (token.device, token.inode, token.digest)
                == (self.token_device, self.token_inode, self.token_sha256)
                && socket_metadata(&plan.sock_path)
                    .is_some_and(|after| (after.dev(), after.ino()) == (self.device, self.inode))
        }
        #[cfg(not(unix))]
        {
            let _ = plan;
            false
        }
    }

    #[cfg(unix)]
    fn encode(&self) -> Option<String> {
        let wire = aterm_json::to_string(self).ok()?;
        (wire.len() <= MAX_WIRE_BYTES).then_some(wire)
    }

    fn decode(wire: &str) -> Option<Self> {
        if wire.len() > MAX_WIRE_BYTES {
            return None;
        }
        let identity: Self = aterm_json::from_str(wire).ok()?;
        (identity.schema == 1
            && !identity.path.is_empty()
            && !identity.path.as_bytes().contains(&0)
            && identity.directory.as_ref().is_none_or(|directory| {
                directory.is_absolute()
                    && !directory.as_os_str().as_encoded_bytes().contains(&0)
                    && std::path::Path::new(&identity.path).is_relative()
            }))
        .then_some(identity)
    }
}

fn explicit_plan(plan: &SocketPlan) -> bool {
    plan.latest_link.is_none()
        && !plan.sock_path.is_empty()
        && plan.token_path == crate::control_auth::token_path_for_socket(&plan.sock_path)
}

#[cfg(unix)]
fn socket_metadata(path: &str) -> Option<std::fs::Metadata> {
    use std::os::unix::fs::FileTypeExt;
    let metadata = std::fs::symlink_metadata(path).ok()?;
    metadata.file_type().is_socket().then_some(metadata)
}

#[cfg(unix)]
struct TokenIdentity {
    device: u64,
    inode: u64,
    digest: [u8; 32],
}

#[cfg(unix)]
fn read_token(plan: &SocketPlan) -> Option<TokenIdentity> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let path = &plan.token_path;
    let before = std::fs::symlink_metadata(path).ok()?;
    if !before.is_file() || before.len() == 0 || before.len() > MAX_TOKEN_BYTES as u64 {
        return None;
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .ok()?;
    let opened = file.metadata().ok()?;
    if !opened.is_file()
        || opened.len() == 0
        || opened.len() > MAX_TOKEN_BYTES as u64
        || (before.dev(), before.ino()) != (opened.dev(), opened.ino())
    {
        return None;
    }
    let mut bytes =
        Vec::with_capacity(usize::try_from(opened.len().saturating_add(1)).unwrap_or(0));
    file.take(MAX_TOKEN_BYTES as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    let after = std::fs::symlink_metadata(path).ok()?;
    if bytes.is_empty()
        || bytes.len() > MAX_TOKEN_BYTES
        || !after.is_file()
        || (after.dev(), after.ino(), after.len())
            != (opened.dev(), opened.ino(), bytes.len() as u64)
    {
        return None;
    }
    Some(TokenIdentity {
        device: opened.dev(),
        inode: opened.ino(),
        digest: aterm_digest::Sha256::digest(&bytes),
    })
}

static PUBLISHED: Mutex<Option<SocketIdentity>> = Mutex::new(None);

/// Called only after the real listener has bound and its witness was captured. The first
/// publication wins.
pub(crate) fn publish(identity: SocketIdentity) -> bool {
    let mut slot = PUBLISHED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if slot.is_some() {
        return false;
    }
    *slot = Some(identity);
    true
}

pub(crate) fn published() -> Option<SocketIdentity> {
    PUBLISHED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Move the published witness onto the socket the supervised listener just renamed over its
/// own endpoint (`control_listener::rebind_socket`), only when the witness names `previous`
/// (the replaced vnode). The token file is untouched, so only the socket vnode changes; the
/// moved witness must still pass [`SocketIdentity::matches_current`] or nothing moves and a
/// later handoff fails closed. Returns whether the witness moved.
#[cfg(unix)]
pub(crate) fn republish_rebound(plan: &SocketPlan, previous: (u64, u64)) -> bool {
    use std::os::unix::fs::MetadataExt;
    let mut slot = PUBLISHED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(current) = slot.as_ref() else {
        return false;
    };
    if (current.device, current.inode) != previous {
        return false;
    }
    let Some(socket) = socket_metadata(&plan.sock_path) else {
        return false;
    };
    let mut next = current.clone();
    next.device = socket.dev();
    next.inode = socket.ino();
    if !next.matches_current(plan) {
        return false;
    }
    *slot = Some(next);
    true
}

/// Clear inherited authority, then carry only this process's bound listener.
#[cfg(unix)]
pub(crate) fn bind_command(command: &mut aterm_uds::spawnfd::StdCommand) {
    bind_command_identity(command, published().as_ref());
}

#[cfg(unix)]
fn bind_command_identity(
    command: &mut aterm_uds::spawnfd::StdCommand,
    identity: Option<&SocketIdentity>,
) {
    command.envs([ENV_IDENTITY.pair("")]);
    if let Some(wire) = identity.and_then(SocketIdentity::encode) {
        command.envs([ENV_IDENTITY.pair(wire)]);
    }
}

/// [`consume_incoming_from`] over the process environment, for this module's tests.
#[cfg(all(test, unix))]
pub(crate) fn consume_incoming() -> Result<Option<SocketIdentity>, String> {
    crate::handoff_env::through_process_env(&[ENV_IDENTITY], consume_incoming_from)
}

/// Consume the launch value once. An error is a refused handoff, never an absent witness that
/// permits the ordinary binder; parsed data gains authority only beside the admitted
/// Ready/Commit gate and a matching explicit plan. It is taken from the snapshot `main_entry`
/// captured (`HandoffEnv::capture`), so it is one-shot by construction.
pub(crate) fn consume_incoming_from(
    env: &mut crate::handoff_env::HandoffEnv,
) -> Result<Option<SocketIdentity>, String> {
    let Some(raw) = env.take(ENV_IDENTITY) else {
        return Ok(None);
    };
    let invalid = || "invalid handoff control-socket identity".to_string();
    let wire = raw.to_str().ok_or_else(invalid)?;
    if wire.is_empty() {
        return Ok(None);
    }
    SocketIdentity::decode(wire).map(Some).ok_or_else(invalid)
}

#[cfg(test)]
#[cfg(unix)]
#[path = "control_socket_identity_tests.rs"]
mod tests;
