// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A maintained executable is not a GUI Store role. A custodian must challenge
//! the actual GUI's authenticated Instance endpoint before accepting delegation.
use aterm_capacity::host_executable::{Instance, Program};
use aterm_ctl::resource_admission_transport::{self as wire, Peer};
use aterm_uds::{listener::ListenerId, process::Birth};
use std::{
    fs::{File, Metadata},
    io,
    os::unix::fs::MetadataExt as _,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

/// Connection-scoped, already challenged GUI image/lifetime witness. Fresh
/// native identity and a held original file stamp avoid hashing the complete
/// executable on every queued solver submission.
pub(crate) struct GuiAuthority {
    identity: aterm_uds::process::Identity,
    file: File,
    metadata: Metadata,
}
impl GuiAuthority {
    fn capture(peer: Peer) -> io::Result<Self> {
        let pid = i32::try_from(peer.pid).map_err(|_| refused())?;
        let identity = aterm_uds::process::read_identity(pid).ok_or_else(refused)?;
        if identity.birth != Birth(peer.birth) {
            return Err(refused());
        }
        let image = aterm_uds::process::LoadedImage::read(pid).ok_or_else(refused)?;
        let (file, metadata) = aterm_uds::regular_file::open(image.launch_path())?;
        if !image.matches_metadata(&metadata) {
            return Err(refused());
        }
        let result = Self {
            identity,
            file,
            metadata,
        };
        result.check(peer)?;
        Ok(result)
    }
    pub(crate) fn check(&self, peer: Peer) -> io::Result<()> {
        if u32::try_from(self.identity.pid).ok() != Some(peer.pid)
            || self.identity.birth != Birth(peer.birth)
            || aterm_uds::process::read_identity(self.identity.pid) != Some(self.identity)
        {
            return Err(refused());
        }
        let current = self.file.metadata()?;
        let stamp = |m: &Metadata| {
            (
                m.dev(),
                m.ino(),
                m.len(),
                m.mode(),
                m.uid(),
                m.gid(),
                m.nlink(),
                m.mtime(),
                m.mtime_nsec(),
                m.ctime(),
                m.ctime_nsec(),
            )
        };
        if stamp(&current) != stamp(&self.metadata)
            || !aterm_uds::process::LoadedImage::read(self.identity.pid)
                .is_some_and(|image| image.matches_metadata(&self.metadata))
            || aterm_uds::process::read_identity(self.identity.pid) != Some(self.identity)
        {
            return Err(refused());
        }
        Ok(())
    }
}

fn refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "GUI instance enrollment refused",
    )
}
struct Pending {
    host: Instance,
    nonce: String,
    path: PathBuf,
    endpoint: ListenerId,
    expires: Instant,
}
fn pending() -> &'static Mutex<Option<Pending>> {
    static PENDING: OnceLock<Mutex<Option<Pending>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(None))
}
/// A one-use pending challenge, prepared only by the GUI's owner dispatcher.
pub(crate) struct Challenge {
    nonce: String,
}
impl Challenge {
    pub(crate) fn begin(host: &Instance, program: &Program, endpoint: &Path) -> io::Result<Self> {
        host.validate()?;
        program.validate()?;
        if host.program_path() != program.path() {
            return Err(refused());
        }
        let mut slot = pending().try_lock().map_err(|_| refused())?;
        if slot
            .as_ref()
            .is_some_and(|old| old.expires > Instant::now())
        {
            return Err(refused());
        }
        let nonce = aterm_uds::rand::hex_token::<32>()?;
        let listener = aterm_uds::listener::listener_read(endpoint)?.ok_or_else(refused)?;
        *slot = Some(Pending {
            host: host.clone(),
            nonce: nonce.clone(),
            path: endpoint.to_owned(),
            endpoint: listener,
            expires: Instant::now() + Duration::from_secs(8),
        });
        Ok(Self { nonce })
    }
    pub(crate) fn nonce(&self) -> &str {
        &self.nonce
    }
}
impl Drop for Challenge {
    fn drop(&mut self) {
        if let Ok(mut slot) = pending().try_lock()
            && slot.as_ref().is_some_and(|old| old.nonce == self.nonce)
        {
            *slot = None;
        }
    }
}

/// Answer only from the exact native GUI Instance/Store dispatcher, never a
/// forwarded, remote, harness or tokenless compatibility principal. The caller
/// must supply the actual accepted control stream's local listener pathname.
pub fn answer(peer: Peer, nonce: &str, endpoint: &Path) -> io::Result<String> {
    let mut slot = pending().try_lock().map_err(|_| refused())?;
    let expected = slot.as_ref().ok_or_else(refused)?;
    if Instant::now() >= expected.expires
        || peer.pid != expected.host.pid()
        || Birth(peer.birth) != expected.host.birth()
        || endpoint != expected.path
        || !aterm_digest::ct_eq(nonce.as_bytes(), expected.nonce.as_bytes())
        || aterm_uds::listener::listener_read(endpoint)? != Some(expected.endpoint)
    {
        return Err(refused());
    }
    expected.host.validate()?;
    if Instant::now() >= expected.expires {
        return Err(refused());
    }
    let response = expected.nonce.clone();
    *slot = None;
    Ok(response)
}

/// Called by the actual custodian before using a GUI delegation lane. The
/// kernel server peer of the challenged owner endpoint must equal the native
/// GUI peer of that lane. Content equality alone is insufficient.
pub(crate) fn verify_gui(
    peer: Peer,
    program: &Program,
    endpoint: &Path,
    nonce: &str,
) -> io::Result<GuiAuthority> {
    verify_gui_at(peer, program, endpoint, nonce, Instant::now())
}

fn verify_gui_at(
    peer: Peer,
    program: &Program,
    endpoint: &Path,
    nonce: &str,
    started: Instant,
) -> io::Result<GuiAuthority> {
    // Enrollment hashes executable content once per connection, not per job.
    // Serialize that expensive authentication work; unknown clients cannot
    // turn the bounded socket pool into 128 parallel whole-image readers.
    static AUTHENTICATING: Mutex<()> = Mutex::new(());
    let _authenticating = AUTHENTICATING.try_lock().map_err(|_| refused())?;
    if nonce.len() != 64 || !aterm_codec::hex::is_lower(nonce, 64) {
        return Err(refused());
    }
    program.validate_process(peer.pid, Birth(peer.birth))?;
    let before = aterm_uds::listener::listener_read(endpoint)?.ok_or_else(refused)?;
    let deadline = started + Duration::from_secs(60);
    let stream = aterm_uds::connect::until(endpoint, deadline)?;
    stream.set_nonblocking(true)?;
    if wire::local_peer(&stream)? != peer {
        return Err(refused());
    }
    let token = aterm_uds::secret::Secret::from_vec(
        aterm_ctl::read_token_beside(endpoint.to_str().ok_or_else(refused)?)?.into_bytes(),
    );
    let mut auth = b"AUTH ".to_vec();
    auth.extend(token.expose());
    auth.extend(b"\nversion\n");
    let auth = aterm_uds::secret::Secret::from_vec(auth);
    wire::write_all(&stream, auth.expose(), deadline)?;
    if !wire::read_line(&stream, deadline)?.starts_with("OK ") {
        return Err(refused());
    }
    wire::write_all(
        &stream,
        format!("resource-admission-v3 enroll nonce={nonce}\n").as_bytes(),
        deadline,
    )?;
    if wire::read_line(&stream, deadline)? != "OK resource-admission-v3 enroll" {
        return Err(refused());
    }
    let body: aterm_json::Value =
        aterm_json::from_str(&wire::read_line(&stream, deadline)?).map_err(|_| refused())?;
    if body["version"].as_u64() != Some(3)
        || body["nonce"].as_str() != Some(nonce)
        || wire::local_peer(&stream)? != peer
        || aterm_uds::listener::listener_read(endpoint)? != Some(before)
    {
        return Err(refused());
    }
    let authority = GuiAuthority::capture(peer)?;
    program.validate_process(peer.pid, Birth(peer.birth))?;
    authority.check(peer)?;
    Ok(authority)
}

#[cfg(all(test, target_os = "macos"))]
#[path = "enrollment_tests.rs"]
mod tests;
