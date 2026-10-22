// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! An outgoing listener's file identity, carried only by an admitted handoff.
//! Connection errors cannot establish that a fixed socket path is unowned: a
//! live listener with a full backlog can also answer ECONNREFUSED. This witness
//! binds the path and token to the listener this process actually published.

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
            // Recheck both files after the bounded token read: a replacement
            // during capture must not be published as this listener's identity.
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

    /// Restore a relative endpoint's binding directory, only after the socket and credential
    /// there validate; the files are rechecked against the new cwd before this returns true.
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
            // An absolute endpoint, or an older witness: validate in place, failing closed.
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
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
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
    let mut bytes = Vec::with_capacity(MAX_TOKEN_BYTES);
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

/// The witness this process publishes: set once at bind, moved only by [`republish_rebound`]
/// when the supervised listener replaces its own wedged socket. A leaf lock, never held across
/// another acquisition; the only I/O under it is the rebind's file revalidation.
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

/// Move the published witness onto the socket the supervised listener just
/// renamed over its OWN endpoint (`control_listener::rebind_socket`). The token
/// file is untouched by a rebind, so only the socket vnode changes — and only
/// when the witness being replaced names `previous` (the vnode that rebind
/// replaced): a witness for anything else is left exactly as it was. The new
/// witness must still verify against the files ([`SocketIdentity::matches_current`]),
/// or nothing is published and a later handoff fails closed rather than
/// trusting a stale inode. Returns whether the witness moved.
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
    // LaunchServices accepts merge-only environment additions. Removing a key
    // would force the fork fallback, so an explicit empty value clears inherited
    // authority while preserving the outgoing transport's process identity.
    command.envs([ENV_IDENTITY.pair("")]);
    if let Some(wire) = identity.and_then(SocketIdentity::encode) {
        command.envs([ENV_IDENTITY.pair(wire)]);
    }
}

/// [`consume_incoming_from`] over the process environment: the env-shaped entry
/// point this module's suite drives (`handoff_env::through_process_env`).
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

// Two attributes rather than `all(test, unix)`: the lock-order census masks a
// `#[cfg(test)]` item and reads any other gate as shipping code.
#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::symlink;

    fn fixture() -> (aterm_tempfile::TempDir, SocketPlan, CtlListener, String) {
        let dir = crate::test_scratch::short_dir("the control socket");
        let path = dir.path().join("control.sock");
        let plan = SocketPlan {
            sock_path: path.to_str().unwrap().to_string(),
            token_path: crate::control_auth::token_path_for_socket(path.to_str().unwrap()),
            latest_link: None,
        };
        let token = crate::control_auth::provision_token(&plan.token_path).unwrap();
        let listener = CtlListener::bind(&path).unwrap();
        (dir, plan, listener, token)
    }

    /// The witness is taken once: absent and empty are `Ok(None)`, malformed is the refusal,
    /// and the variable is gone after every case. `SocketIdentity` has no `Debug` on purpose
    /// (nothing about a listener's files belongs in a log), so it is compared with `==`.
    #[test]
    fn consume_incoming_takes_the_witness_once_through_the_blessed_helper() {
        // `HandoffEnv::capture`'s conformance takes this name out of the process too.
        let Some(_env) = crate::seamless::tests::env_alone() else {
            return;
        };
        aterm_log::env::unset(ENV_IDENTITY);
        assert!(consume_incoming() == Ok(None), "absent: nothing to consume");
        assert!(std::env::var_os(ENV_IDENTITY).is_none());

        aterm_log::env::set(ENV_IDENTITY, "");
        assert!(consume_incoming() == Ok(None), "empty: an explicit nothing");
        assert!(
            std::env::var_os(ENV_IDENTITY).is_none(),
            "and it is consumed"
        );

        aterm_log::env::set(ENV_IDENTITY, "{not a witness");
        assert!(
            consume_incoming() == Err("invalid handoff control-socket identity".to_string()),
            "malformed: a refused handoff, never an absent witness"
        );
        assert!(
            std::env::var_os(ENV_IDENTITY).is_none(),
            "consumed even when refused"
        );

        let (_dir, plan, listener, token) = fixture();
        let identity = SocketIdentity::capture(&plan, &listener, &token).expect("a witness");
        aterm_log::env::set(ENV_IDENTITY, identity.encode().expect("encodes"));
        assert!(
            consume_incoming() == Ok(Some(identity)),
            "a real witness decodes to what the parent captured"
        );
        assert!(
            std::env::var_os(ENV_IDENTITY).is_none(),
            "taken once: a second reader sees nothing"
        );
        assert!(consume_incoming() == Ok(None));
    }

    #[test]
    fn relative_listener_witness_restores_directory_across_transport() {
        // Cwd is process-wide: this runs only as a single-test child.
        if !crate::test_alone::alone() {
            return;
        }
        let original = crate::test_scratch::short_dir("the original socket");
        let foreign = crate::test_scratch::short_dir("the foreign socket");
        std::env::set_current_dir(original.path()).unwrap();
        let original_dir = std::env::current_dir().unwrap();
        std::fs::create_dir("nested").unwrap();
        let plan = SocketPlan {
            sock_path: "nested/control.sock".into(),
            token_path: crate::control_auth::token_path_for_socket("nested/control.sock"),
            latest_link: None,
        };
        let token = crate::control_auth::provision_token(&plan.token_path).unwrap();
        let listener = CtlListener::bind(&plan.sock_path).unwrap();
        let identity = SocketIdentity::capture(&plan, &listener, &token).unwrap();
        assert_eq!(identity.directory.as_ref(), Some(&original_dir));
        // std's Command on purpose: never spawned; the shipped `bind_command_identity` takes std's.
        let mut command = std::process::Command::new("unused-test-command");
        bind_command_identity(&mut command, Some(&identity));
        let wire = command
            .get_envs()
            .find(|(key, _)| *key == ENV_IDENTITY.name())
            .and_then(|(_, value)| value)
            .unwrap()
            .to_str()
            .unwrap();
        let received = SocketIdentity::decode(wire).unwrap();
        assert!(!wire.contains(&token));

        // LaunchServices can start from another cwd; the relative spelling must stay usable
        // without lengthening the Unix bind path.
        std::env::set_current_dir(foreign.path()).unwrap();
        let foreign_dir = std::env::current_dir().unwrap();
        assert!(
            !received.matches_current(&plan),
            "old cwd-free witness would reject here"
        );
        std::fs::create_dir("nested").unwrap();
        std::fs::write(&plan.token_path, &token).unwrap();
        let foreign_listener = CtlListener::bind(&plan.sock_path).unwrap();
        let mut redirected = received.clone();
        redirected.directory = Some(foreign_dir.clone());
        // SAFETY: isolated single-test child; no concurrent filesystem workers.
        assert!(!unsafe { redirected.prepare_incoming_directory(&plan) });
        assert_eq!(std::env::current_dir().unwrap(), foreign_dir);
        assert_eq!(std::fs::read(&plan.token_path).unwrap(), token.as_bytes());

        // The endpoint and credential validate in the original directory before cwd moves.
        // SAFETY: same isolated child and the fixture's admitted witness.
        assert!(unsafe { received.prepare_incoming_directory(&plan) });
        assert_eq!(std::env::current_dir().unwrap(), original_dir);
        assert!(received.matches_current(&plan));
        let client = aterm_uds::CtlStream::connect(&plan.sock_path).unwrap();
        let (accepted, _) = listener.accept().unwrap();
        assert!(crate::control_auth::peer_check(&accepted).is_ok());
        assert_eq!(std::fs::read(&plan.token_path).unwrap(), token.as_bytes());
        // An absolute endpoint's witness keeps the wire shape older receivers read.
        let (_absolute_dir, absolute, absolute_listener, absolute_token) = fixture();
        let absolute_identity =
            SocketIdentity::capture(&absolute, &absolute_listener, &absolute_token).unwrap();
        assert!(!absolute_identity.encode().unwrap().contains("directory"));
        drop((client, accepted, listener, foreign_listener));
        std::env::set_current_dir("/tmp").unwrap();
    }

    #[test]
    fn actual_listener_identity_roundtrips_and_command_clears_inherited_authority() {
        let (_dir, plan, listener, token) = fixture();
        let identity = SocketIdentity::capture(&plan, &listener, &token).unwrap();
        let wire = identity.encode().unwrap();
        assert!(
            !wire.contains(&token),
            "token plaintext must not enter the wire"
        );
        let decoded = SocketIdentity::decode(&wire).unwrap();
        assert!(decoded == identity);
        assert!(decoded.matches_current(&plan));
        // std's Command on purpose: never spawned; the shipped `bind_command_identity` takes std's.
        let mut command = std::process::Command::new("unused-test-command");
        command.env(ENV_IDENTITY, "inherited-authority");
        bind_command_identity(&mut command, None);
        assert!(
            command
                .get_envs()
                .any(|(key, value)| key == ENV_IDENTITY.name()
                    && value == Some(std::ffi::OsStr::new("")))
        );
        bind_command_identity(&mut command, Some(&identity));
        assert!(
            command
                .get_envs()
                .any(|(key, value)| key == ENV_IDENTITY.name()
                    && value == Some(std::ffi::OsStr::new(&wire)))
        );
        assert!(SocketIdentity::decode("").is_none());
        assert!(SocketIdentity::decode("{}").is_none());
        assert!(SocketIdentity::decode(&"x".repeat(MAX_WIRE_BYTES + 1)).is_none());
        let mut unsupported = identity.clone();
        unsupported.schema += 1;
        assert!(SocketIdentity::decode(&unsupported.encode().unwrap()).is_none());
    }

    #[test]
    fn capture_requires_the_actual_listener_path_and_provisioned_token() {
        let (dir, plan, listener, token) = fixture();
        assert!(SocketIdentity::capture(&plan, &listener, "a different token").is_none());
        let other = CtlListener::bind(dir.path().join("other.sock")).unwrap();
        assert!(SocketIdentity::capture(&plan, &other, &token).is_none());
        let mut per_process = plan.clone();
        per_process.latest_link = Some(dir.path().join("latest.sock"));
        assert!(SocketIdentity::capture(&per_process, &listener, &token).is_none());
        let mut wrong_token_path = plan.clone();
        wrong_token_path.token_path = dir.path().join("other.token");
        assert!(SocketIdentity::capture(&wrong_token_path, &listener, &token).is_none());
    }

    #[test]
    fn token_rewrite_replacement_symlink_and_oversize_all_refuse() {
        let (dir, plan, listener, token) = fixture();
        let identity = SocketIdentity::capture(&plan, &listener, &token).unwrap();
        std::fs::write(&plan.token_path, "changed token").unwrap();
        assert!(!identity.matches_current(&plan));
        std::fs::write(&plan.token_path, &token).unwrap();
        assert!(identity.matches_current(&plan));
        let saved = dir.path().join("original.token");
        std::fs::rename(&plan.token_path, &saved).unwrap();
        std::fs::write(&plan.token_path, &token).unwrap();
        assert!(
            !identity.matches_current(&plan),
            "same bytes in a replacement inode are not the captured file"
        );
        std::fs::remove_file(&plan.token_path).unwrap();
        symlink(&saved, &plan.token_path).unwrap();
        assert!(!identity.matches_current(&plan));
        assert!(SocketIdentity::capture(&plan, &listener, &token).is_none());
        std::fs::remove_file(&plan.token_path).unwrap();
        std::fs::write(&plan.token_path, vec![b'x'; MAX_TOKEN_BYTES + 1]).unwrap();
        assert!(SocketIdentity::capture(&plan, &listener, &token).is_none());
        std::fs::remove_file(&plan.token_path).unwrap();
        assert!(!identity.matches_current(&plan));
    }

    /// The parent's own rebind invalidates a carried witness; the successor's refusal exit is
    /// filed Transient by a parent that reads it (`seamless::endpoint_refusal_exit`).
    #[test]
    fn a_witness_the_parents_rebind_moved_is_filed_a_moment() {
        use crate::app_native::PhysicalFailureShape as Shape;
        let (_dir, plan, listener, token) = fixture();
        let identity = SocketIdentity::capture(&plan, &listener, &token).unwrap();
        assert!(
            identity.matches_current(&plan),
            "PRECONDITION: the witness holds"
        );
        let mut published = crate::control_listener::socket_file_id(&plan.sock_path);
        let _fresh = crate::control_listener::rebind_socket(&plan.sock_path, &mut published)
            .expect("the rebind");
        assert!(
            !identity.matches_current(&plan),
            "the carried witness no longer matches: the successor refuses"
        );
        let filed = |parent_reads_passing| {
            Shape::of_child_death(crate::ChildDeathEvidence::Exited {
                code: crate::seamless::endpoint_refusal_exit(parent_reads_passing),
            })
        };
        assert_eq!(filed(true), Shape::Transient, "a moment, retried");
        assert_eq!(
            crate::seamless::endpoint_refusal_exit(false),
            74,
            "an older parent is sent what it always was"
        );
    }

    #[test]
    fn a_replaced_socket_refuses_even_with_a_prefilled_foreign_backlog() {
        let (dir, plan, listener, token) = fixture();
        let identity = SocketIdentity::capture(&plan, &listener, &token).unwrap();
        let saved = dir.path().join("retiring.sock");
        std::fs::rename(&plan.sock_path, &saved).unwrap();
        let foreign = CtlListener::bind(&plan.sock_path).unwrap();
        // SAFETY: this test owns the listener whose backlog it reduces.
        assert_eq!(unsafe { libc::listen(foreign.as_raw_fd(), 1) }, 0);
        let mut pending = Vec::new();
        for _ in 0..16 {
            if let Ok(stream) = crate::control_auth::connect_socket_nonblocking(&plan.sock_path) {
                pending.push(stream);
            }
        }
        assert!(
            !pending.is_empty(),
            "the fixture must really reach the foreign listener"
        );
        assert!(!identity.matches_current(&plan));
        // Token-only matching is the negative control: the old credential is
        // untouched while the listening endpoint belongs to someone else.
        assert_eq!(read_token(&plan).unwrap().digest, identity.token_sha256);
        assert!(std::fs::read(&plan.token_path).unwrap() == token.as_bytes());
        drop((pending, foreign));
        std::fs::remove_file(&plan.sock_path).unwrap();
        assert!(!identity.matches_current(&plan));
        symlink(&saved, &plan.sock_path).unwrap();
        assert!(!identity.matches_current(&plan));
    }
}
