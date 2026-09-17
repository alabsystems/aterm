// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! An outgoing listener's file identity, carried only by an admitted handoff.
//! Connection errors cannot establish that a fixed socket path is unowned: a
//! live listener with a full backlog can also answer ECONNREFUSED. This witness
//! binds the path and token to the listener this process actually published.

use crate::control_auth::SocketPlan;
use aterm_uds::CtlListener;
use serde::{Deserialize, Serialize};
use std::process::Command;
use std::sync::OnceLock;

const ENV_IDENTITY: &str = "ATERM_HANDOFF_CONTROL_SOCKET_IDENTITY";
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
    /// Relative Unix socket names are resolved against the listener's original
    /// working directory. LaunchServices does not inherit that directory.
    /// Absolute endpoints retain the original wire shape for older receivers.
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

    /// Requires the same socket vnode AND the same token file and contents.
    /// Missing paths, symlinks, unsupported platforms and unreadable files refuse.
    pub(crate) fn matches_current(&self, plan: &SocketPlan) -> bool {
        self.matches_plan_path(plan) && self.matches_files(plan)
    }

    /// Restore a relative endpoint's binding directory only after validating
    /// the existing socket AND credential there. The final ordinary validation
    /// and binder still recheck the files against the now-current directory.
    /// No environment string or directory alone grants endpoint ownership.
    ///
    /// # Safety
    /// Called only in single-threaded startup of an admitted Ready/Commit
    /// candidate. Changing cwd must not redirect concurrent filesystem work.
    #[cfg(unix)]
    pub(crate) unsafe fn prepare_incoming_directory(&self, plan: &SocketPlan) -> bool {
        if !self.matches_plan_path(plan) {
            return false;
        }
        let Some(directory) = &self.directory else {
            // Legacy witnesses have no directory. Fork inheritance still works;
            // a launched candidate must fail closed if its files do not match.
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

static PUBLISHED: OnceLock<SocketIdentity> = OnceLock::new();

/// Called only after the real listener has bound and its witness was captured.
pub(crate) fn publish(identity: SocketIdentity) -> bool {
    PUBLISHED.set(identity).is_ok()
}

pub(crate) fn published() -> Option<SocketIdentity> {
    PUBLISHED.get().cloned()
}

/// Clear inherited authority, then carry only this process's bound listener.
pub(crate) fn bind_command(command: &mut Command) {
    bind_command_identity(command, PUBLISHED.get());
}

fn bind_command_identity(command: &mut Command, identity: Option<&SocketIdentity>) {
    // LaunchServices accepts merge-only environment additions. Removing a key
    // would force the fork fallback, so an explicit empty value clears inherited
    // authority while preserving the outgoing transport's process identity.
    command.env(ENV_IDENTITY, "");
    if let Some(wire) = identity.and_then(SocketIdentity::encode) {
        command.env(ENV_IDENTITY, wire);
    }
}

/// Consume the launch value once. An error is a refused handoff, never an absent
/// witness that permits the ordinary binder. Parsed data gains authority only
/// beside the existing admitted Ready/Commit gate and matching explicit plan.
///
/// Read-and-remove goes through the workspace's ONE lock-scoped environment
/// mutator — the shape `env_mutation` asks for — and that helper is also the
/// reason the pair is now ATOMIC: a split read-then-remove lets two callers both
/// observe a value that is meant to be consumed exactly once, and a one-shot
/// authority consumed twice is not one-shot.
///
/// The lock cannot serialize a bare `getenv` on another thread, so this must
/// still be called from single-threaded startup. That is a positioning
/// requirement on the caller, not a memory-safety contract: the mutation itself
/// is the blessed helper's, so this function is safe to call.
pub(crate) fn consume_incoming() -> Result<Option<SocketIdentity>, String> {
    let Some(raw) = aterm_log::env::take(ENV_IDENTITY) else {
        return Ok(None);
    };
    let invalid = || "invalid handoff control-socket identity".to_string();
    let wire = raw.to_str().ok_or_else(invalid)?;
    if wire.is_empty() {
        return Ok(None);
    }
    SocketIdentity::decode(wire).map(Some).ok_or_else(invalid)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::symlink;

    fn fixture() -> (aterm_tempfile::TempDir, SocketPlan, CtlListener, String) {
        let dir = aterm_tempfile::TempDir::new_in("/tmp").unwrap();
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

    /// `consume_incoming` takes the launch witness ONCE, through the workspace's
    /// lock-scoped env helper: absent and empty are `Ok(None)`, a malformed
    /// witness is the refusal, a real one decodes to the identity the parent
    /// captured — and after every case the variable is gone, so nothing later in
    /// the process, and no child it spawns, can observe it. The env helpers are
    /// serialized under one lock, so this runs beside the parallel suite. The
    /// identity is compared with `==`, never formatted: the type carries no
    /// `Debug` on purpose (nothing about a listener's files is for a log line).
    #[test]
    fn consume_incoming_takes_the_witness_once_through_the_blessed_helper() {
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
        const CHILD: &str = "ATERM_TEST_RELATIVE_SOCKET_DIRECTORY";
        if std::env::var_os(CHILD).is_none() {
            // Cwd is process-wide; the normal parallel test runner never changes
            // it. This single-test child owns every file and listener below.
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "control_socket_identity::tests::relative_listener_witness_restores_directory_across_transport",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "relative socket child failed:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let original = aterm_tempfile::TempDir::new_in("/tmp").unwrap();
        let foreign = aterm_tempfile::TempDir::new_in("/tmp").unwrap();
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
        let mut command = Command::new("unused-test-command");
        bind_command_identity(&mut command, Some(&identity));
        let wire = command
            .get_envs()
            .find(|(key, _)| *key == ENV_IDENTITY)
            .and_then(|(_, value)| value)
            .unwrap()
            .to_str()
            .unwrap();
        let received = SocketIdentity::decode(wire).unwrap();
        assert!(!wire.contains(&token));

        // LaunchServices can start from another cwd. The original raw relative
        // spelling must remain usable without lengthening the Unix bind path.
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

        // The valid endpoint and credential are checked BEFORE cwd changes.
        // SAFETY: same isolated child and the fixture's admitted witness.
        assert!(unsafe { received.prepare_incoming_directory(&plan) });
        assert_eq!(std::env::current_dir().unwrap(), original_dir);
        assert!(received.matches_current(&plan));
        let client = aterm_uds::CtlStream::connect(&plan.sock_path).unwrap();
        let (accepted, _) = listener.accept().unwrap();
        assert!(crate::control_auth::peer_check(&accepted).is_ok());
        assert_eq!(std::fs::read(&plan.token_path).unwrap(), token.as_bytes());
        // Legacy receivers still see the byte-compatible absolute witness.
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
        let mut command = Command::new("unused-test-command");
        command.env(ENV_IDENTITY, "inherited-authority");
        bind_command_identity(&mut command, None);
        assert!(
            command
                .get_envs()
                .any(|(key, value)| key == ENV_IDENTITY && value == Some(std::ffi::OsStr::new("")))
        );
        bind_command_identity(&mut command, Some(&identity));
        assert!(
            command
                .get_envs()
                .any(|(key, value)| key == ENV_IDENTITY
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
