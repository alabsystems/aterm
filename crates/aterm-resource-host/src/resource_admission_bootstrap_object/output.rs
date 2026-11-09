// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Publish only under the SAME already enrolled output descriptions, after the
//! private issuer's actual native closure. No file or metadata creates a lock.

use super::super::bootstrap_graph::OriginalOutput;
use super::execution::NativeClosed;
use std::{
    ffi::{CString, OsStr, OsString},
    fs::{File, Metadata, Permissions},
    io,
    os::{
        fd::{AsRawFd as _, FromRawFd as _},
        unix::{
            ffi::OsStrExt as _,
            fs::{FileExt as _, MetadataExt as _, PermissionsExt as _},
        },
    },
    time::Instant,
};
use trust_resource_controller::MetadataFile;

const MAX_OBJECT: u64 = super::wire::MAX_OBJECT_BYTES;

fn refused(why: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}
fn alive(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        Err(io::ErrorKind::TimedOut.into())
    } else {
        Ok(())
    }
}
fn leaf(bytes: &[u8]) -> io::Result<OsString> {
    if !super::wire::destination(bytes) {
        return Err(refused(
            "object destination is outside its closed relative leaf profile",
        ));
    }
    Ok(OsStr::from_bytes(bytes).to_owned())
}
fn cstring(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes()).map_err(io::Error::other)
}
fn absent(output: &OriginalOutput, name: &OsStr) -> io::Result<()> {
    let name = cstring(name)?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    let result = unsafe {
        libc::fstatat(
            output.directory()?.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        return Err(refused(
            "object operation cannot overwrite an existing name",
        ));
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::NotFound {
        Ok(())
    } else {
        Err(error)
    }
}
fn open(output: &OriginalOutput, name: &OsStr) -> io::Result<File> {
    let name = cstring(name)?;
    let fd = unsafe {
        libc::openat(
            output.directory()?.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

#[derive(PartialEq, Eq)]
struct Stamp {
    device: u64,
    inode: u64,
    bytes: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    links: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
impl Stamp {
    fn of(meta: &Metadata) -> Self {
        Self {
            device: meta.dev(),
            inode: meta.ino(),
            bytes: meta.len(),
            mode: meta.mode(),
            uid: meta.uid(),
            gid: meta.gid(),
            links: meta.nlink(),
            modified: (meta.mtime(), meta.mtime_nsec()),
            changed: (meta.ctime(), meta.ctime_nsec()),
        }
    }
    fn regular(meta: &Metadata) -> bool {
        meta.is_file()
            && meta.nlink() == 1
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.mode() & 0o022 == 0
            && meta.len() > 0
            && meta.len() <= MAX_OBJECT
    }
}
fn seal(file: &File) -> io::Result<Stamp> {
    let before = file.metadata()?;
    if !Stamp::regular(&before) {
        return Err(refused(
            "staged object is not a bounded protected original file",
        ));
    }
    trust_resource_provenance::require_no_acl_grants(file)?;
    file.set_permissions(Permissions::from_mode(0o400))?;
    trust_resource_provenance::require_no_acl_grants(file)?;
    let stamp = Stamp::of(&file.metadata()?);
    if stamp.device != before.dev() || stamp.inode != before.ino() || stamp.bytes != before.len() {
        return Err(refused("staged object changed during readonly sealing"));
    }
    Ok(stamp)
}

fn read_at(file: &File, mut bytes: &mut [u8], mut at: u64, deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        alive(deadline)?;
        match file.read_at(bytes, at) {
            Ok(0) => return Err(refused("object ended inside its original byte observation")),
            Ok(n) => {
                at += n as u64;
                bytes = &mut bytes[n..];
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    alive(deadline)
}
fn native_object(file: &File, bytes: u64, deadline: Instant) -> io::Result<()> {
    let mut header = [0; 32];
    read_at(file, &mut header, 0, deadline)?;
    let word = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
    // Native ARM64 MH_OBJECT only. This shape observation is not machine-code
    // semantics, a compiler proof or an executable/SDK capability.
    if word(0) != 0xfeedfacf || word(4) != 0x0100000c || word(12) != 1 {
        return Err(refused("produced bytes are not a native ARM64 object"));
    }
    let end = 32_u64
        .checked_add(u64::from(word(20)))
        .filter(|end| *end <= bytes)
        .ok_or_else(|| refused("object command population exceeds actual bytes"))?;
    let count = word(16);
    if count == 0 || count > 4096 {
        return Err(refused("object command population is outside its bound"));
    }
    let mut at = 32_u64;
    for _ in 0..count {
        let mut command = [0; 8];
        if at.checked_add(8).is_none_or(|next| next > end) {
            return Err(refused("object command header exceeds its population"));
        }
        read_at(file, &mut command, at, deadline)?;
        let size = u64::from(u32::from_le_bytes(command[4..].try_into().unwrap()));
        if size < 8 || size % 8 != 0 {
            return Err(refused("object command length is malformed"));
        }
        at = at
            .checked_add(size)
            .filter(|next| *next <= end)
            .ok_or_else(|| refused("object command exceeds its actual population"))?;
    }
    if at != end {
        return Err(refused("object command population is incomplete"));
    }
    Ok(())
}
fn digest(file: &File, stamp: &Stamp, deadline: Instant) -> io::Result<[u8; 32]> {
    if Stamp::of(&file.metadata()?) != *stamp {
        return Err(refused("original object changed before hash"));
    }
    let mut hash = aterm_digest::Sha256::new();
    let mut buffer = [0; 65536];
    let mut at = 0;
    while at < stamp.bytes {
        let count = (stamp.bytes - at).min(buffer.len() as u64) as usize;
        read_at(file, &mut buffer[..count], at, deadline)?;
        hash.update(&buffer[..count]);
        at += count as u64;
    }
    if Stamp::of(&file.metadata()?) != *stamp {
        return Err(refused("original object changed while hashing"));
    }
    alive(deadline)?;
    Ok(hash.finalize())
}

pub(super) struct Pending {
    output: OriginalOutput,
    operation: [u8; 16],
    staging: OsString,
    destination: OsString,
    current: OsString,
    captured: Option<File>,
}
pub(super) struct PublishFailure {
    pub(super) original: Pending,
    pub(super) error: io::Error,
}
pub(super) struct Published {
    output: OriginalOutput,
    name: OsString,
    file: File,
    stamp: Stamp,
    observation: MetadataFile,
}
impl Pending {
    pub(super) fn new(
        output: OriginalOutput,
        operation: [u8; 16],
        destination: &[u8],
        deadline: Instant,
    ) -> io::Result<Self> {
        alive(deadline)?;
        output.check_current()?;
        if operation == [0; 16] {
            return Err(refused("object operation has no original nonce"));
        }
        let destination = leaf(destination)?;
        let staging = OsString::from(format!(
            ".trust-object-{}-{}.o",
            aterm_codec::hex::encode_lower(&operation),
            aterm_uds::rand::hex_token::<16>()?
        ));
        absent(&output, &destination)?;
        absent(&output, &staging)?;
        alive(deadline)?;
        Ok(Self {
            output,
            operation,
            current: staging.clone(),
            staging,
            destination,
            captured: None,
        })
    }
    pub(super) fn inherited(&self) -> io::Result<[std::os::fd::OwnedFd; 2]> {
        self.output.inherited()
    }
    pub(super) fn staging(&self) -> &OsStr {
        &self.staging
    }
    pub(super) fn validate(&self) -> io::Result<()> {
        self.output.check_current()?;
        absent(&self.output, &self.destination)
    }
    pub(super) fn check_unlaunched(&self) -> io::Result<()> {
        self.validate()?;
        absent(&self.output, &self.staging)
    }
    pub(super) fn retained_name(&self) -> &[u8] {
        self.current.as_bytes()
    }

    /// The opaque proof can only come from this issuer's actual owning wait,
    /// no-fork subgroup retirement, member journal and original GNU return.
    pub(super) fn publish(
        mut self,
        closed: &NativeClosed,
        deadline: Instant,
    ) -> Result<Published, Box<PublishFailure>> {
        let request = closed.request();
        let accepted = self.output.attachment();
        let result = if request.key.graph != accepted.key
            || request.key.operation != self.operation
            || request.output_root != self.output.path().as_os_str().as_bytes()
            || request.output_directory != accepted.root
            || request.output_lock != accepted.lock
            || request.destination != self.destination.as_bytes()
            || closed.staging() != self.staging.as_bytes()
        {
            Err(refused(
                "native closure belongs to another original object publication",
            ))
        } else {
            self.publish_current(deadline)
        };
        match result {
            Ok((stamp, sha256)) => {
                let observation = MetadataFile {
                    path: self
                        .output
                        .path()
                        .join(&self.destination)
                        .as_os_str()
                        .as_bytes()
                        .to_vec(),
                    sha256,
                    bytes: stamp.bytes,
                    device: stamp.device,
                    inode: stamp.inode,
                };
                Ok(Published {
                    output: self.output,
                    name: self.destination,
                    file: self.captured.take().expect("same captured original object"),
                    stamp,
                    observation,
                })
            }
            Err(error) => Err(Box::new(PublishFailure {
                original: self,
                error,
            })),
        }
    }
    fn publish_current(&mut self, deadline: Instant) -> io::Result<(Stamp, [u8; 32])> {
        alive(deadline)?;
        self.output.check_current()?;
        self.captured = Some(open(&self.output, &self.staging)?);
        let file = self.captured.as_ref().expect("same original object");
        let stamp = seal(file)?;
        native_object(file, stamp.bytes, deadline)?;
        let hash = digest(file, &stamp, deadline)?;
        let named = open(&self.output, &self.staging)?;
        if Stamp::of(&named.metadata()?) != stamp {
            return Err(refused("staged object name changed before publication"));
        }
        // Persist sealed bytes before publishing their directory entry.
        aterm_durable::durable(file)?;
        alive(deadline)?;
        self.output.check_current()?;
        absent(&self.output, &self.destination)?;
        let source = cstring(&self.staging)?;
        let destination = cstring(&self.destination)?;
        let dir = self.output.directory()?.as_raw_fd();
        #[cfg(target_os = "macos")]
        let renamed = unsafe {
            libc::renameatx_np(
                dir,
                source.as_ptr(),
                dir,
                destination.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        #[cfg(target_os = "linux")]
        let renamed = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                dir,
                source.as_ptr(),
                dir,
                destination.as_ptr(),
                libc::RENAME_NOREPLACE,
            ) as i32
        };
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let renamed = return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native no-replace object publication unavailable",
        ));
        if renamed != 0 {
            return Err(io::Error::last_os_error());
        }
        self.current = self.destination.clone();
        aterm_durable::primitive::fsync(self.output.directory()?)?;
        alive(deadline)?;
        self.output.check_current()?;
        let after = file.metadata()?;
        if !Stamp::regular(&after)
            || after.dev() != stamp.device
            || after.ino() != stamp.inode
            || after.len() != stamp.bytes
            || after.mode() != stamp.mode
            || (after.mtime(), after.mtime_nsec()) != stamp.modified
        {
            return Err(refused("published original object changed"));
        }
        let stamp = Stamp::of(&after);
        let named = open(&self.output, &self.destination)?;
        if Stamp::of(&named.metadata()?) != stamp || digest(file, &stamp, deadline)? != hash {
            return Err(refused("published original object name or bytes changed"));
        }
        Ok((stamp, hash))
    }
}
impl Published {
    pub(super) fn observation(&self) -> &MetadataFile {
        &self.observation
    }
    pub(super) fn validate(&self, deadline: Instant) -> io::Result<()> {
        alive(deadline)?;
        self.output.check_current()?;
        let named = open(&self.output, &self.name)?;
        trust_resource_provenance::require_no_acl_grants(&self.file)?;
        trust_resource_provenance::require_no_acl_grants(&named)?;
        if Stamp::of(&named.metadata()?) != self.stamp
            || digest(&self.file, &self.stamp, deadline)? != self.observation.sha256
        {
            return Err(refused("retained original object changed"));
        }
        alive(deadline)
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    #[test]
    fn readonly_posix_mode_does_not_hide_a_real_acl_write_grant() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = scratch.path().join("object-sealing-input");
        // This checks actual sealing mechanics only, not MH_OBJECT production
        // or an opaque NativeClosed/SDK/output-lock constructor.
        std::fs::write(&path, b"actual bounded original bytes").unwrap();
        let file = File::open(&path).unwrap();
        seal(&file).unwrap();
        assert_eq!(file.metadata().unwrap().mode() & 0o777, 0o400);
        let result = crate::actor::owner::spawnfd::command("/bin/chmod")
            .args(["+a", "everyone allow write"])
            .arg(&path)
            .status()
            .unwrap();
        assert!(result.success(), "actual fixture ACL setup failed");
        file.set_permissions(Permissions::from_mode(0o400)).unwrap();
        assert_eq!(file.metadata().unwrap().mode() & 0o777, 0o400);
        assert!(
            seal(&file).is_err(),
            "a real allow ACL survives chmod0400 and must refuse"
        );
        assert!(
            crate::actor::owner::spawnfd::command("/bin/chmod")
                .arg("-N")
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        seal(&file).unwrap();
    }
}
