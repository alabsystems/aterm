// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual source descriptions/name changes only. These fixtures create no
//! OriginalSourceRoot, SDK Keeper, NativeClosed, output guard or launch rights.
use super::*;
use std::{fs, os::unix::fs::OpenOptionsExt as _, time::Duration};
use trust_resource_controller::{
    ContextQuery, FileIdentity, JobBinding,
    bootstrap_graph::{Key, query::OperationKey},
};
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(60)
}
struct Inputs {
    _scratch: aterm_tempfile::TempDir,
    path: std::path::PathBuf,
    request: wire::Request,
    source: Source,
}
impl Inputs {
    fn new() -> Self {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = fs::canonicalize(scratch.path()).unwrap();
        fs::create_dir_all(path.join("src/bootstrap")).unwrap();
        let name = path.join("src/bootstrap/native_object.rs");
        fs::write(&name, wire::SOURCE_BYTES).unwrap();
        let directory = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)
            .unwrap();
        let file = File::open(&name).unwrap();
        let root = directory.metadata().unwrap();
        let item = file.metadata().unwrap();
        let metadata = |path: &[u8]| MetadataFile {
            path: path.to_vec(),
            sha256: [1; 32],
            bytes: 7,
            device: 1,
            inode: 9,
        };
        let request = wire::Request {
            key: OperationKey {
                graph: Key {
                    query: ContextQuery {
                        binding: JobBinding {
                            session: "s-abcd".into(),
                            root_attempt: [1; 16],
                            capability: [9; 32],
                        },
                        call_attempt: [2; 16],
                    },
                    graph: [3; 16],
                },
                sequence: 1,
                operation: [4; 16],
            },
            gnu: GnuIdentity::NamedFifo {
                path: b"/original/credits".to_vec(),
                backing: FileIdentity {
                    device: 1,
                    inode: 2,
                },
            },
            sdk_root: b"/sdk".to_vec(),
            driver: metadata(b"/sdk/bin/tippy-driver"),
            compiler: metadata(b"/sdk/bin/trustc"),
            source_kind: wire::SourceKind::NativeLeafV1,
            source_root: path.as_os_str().as_bytes().to_vec(),
            source_directory: FileIdentity {
                device: root.dev(),
                inode: root.ino(),
            },
            source: MetadataFile {
                path: name.as_os_str().as_bytes().to_vec(),
                sha256: wire::SOURCE_SHA256,
                bytes: wire::SOURCE_BYTES.len() as u64,
                device: item.dev(),
                inode: item.ino(),
            },
            output_root: b"/output".to_vec(),
            output_directory: FileIdentity {
                device: 1,
                inode: 12,
            },
            output_lock: FileIdentity {
                device: 1,
                inode: 13,
            },
            destination: b"native_leaf.o".to_vec(),
            timeout_ns: wire::MAX_COMPILE_NS,
        };
        let mut source = Source {
            descriptions: [directory.into(), file.into()],
            stamps: Some([Stamp::of(&root), Stamp::of(&item)]),
            parents: Vec::new(),
        };
        source.capture_parents().unwrap();
        source.validate(&request, deadline()).unwrap();
        Self {
            _scratch: scratch,
            path,
            request,
            source,
        }
    }
}
#[test]
fn received_original_source_rechecks_preserve_actual_ofd_cursor_and_flags() {
    let inputs = Inputs::new();
    let fd = inputs.source.descriptions[1].as_raw_fd();
    assert_eq!(unsafe { libc::lseek(fd, 17, libc::SEEK_SET) }, 17);
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    let local = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    inputs.source.validate(&inputs.request, deadline()).unwrap();
    assert_eq!(unsafe { libc::lseek(fd, 0, libc::SEEK_CUR) }, 17);
    assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFL) }, flags);
    assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, local);
    assert_eq!(
        aterm_digest::Sha256::digest(wire::SOURCE_BYTES),
        wire::SOURCE_SHA256
    );
}
#[test]
fn same_source_bytes_restored_after_real_write_do_not_erase_original_drift() {
    let inputs = Inputs::new();
    let name = inputs.path.join("src/bootstrap/native_object.rs");
    let mut changed = wire::SOURCE_BYTES.to_vec();
    changed[0] ^= 1;
    fs::write(&name, &changed).unwrap();
    assert!(inputs.source.validate(&inputs.request, deadline()).is_err());
    fs::write(&name, wire::SOURCE_BYTES).unwrap();
    // Fresh transport equality can see restored canonical bytes; the actual
    // earlier native file stamp remains an independent mandatory bookend.
    wire::transport::check_source(
        &inputs.request,
        &inputs.source.descriptions.each_ref().map(AsFd::as_fd),
        deadline(),
    )
    .unwrap();
    assert!(inputs.source.validate(&inputs.request, deadline()).is_err());
}
#[test]
fn identical_replacement_source_is_rejected_while_original_file_stays_held() {
    let inputs = Inputs::new();
    let name = inputs.path.join("src/bootstrap/native_object.rs");
    fs::rename(&name, inputs.path.join("original-source.rs")).unwrap();
    fs::write(&name, wire::SOURCE_BYTES).unwrap();
    assert!(inputs.source.validate(&inputs.request, deadline()).is_err());
    let m = spawnfd::descriptor_metadata(inputs.source.descriptions[1].as_raw_fd()).unwrap();
    assert_eq!(
        (m.dev(), m.ino()),
        (inputs.request.source.device, inputs.request.source.inode)
    );
    let mut actual = [0; wire::SOURCE_BYTES.len()];
    assert_eq!(
        unsafe {
            libc::pread(
                inputs.source.descriptions[1].as_raw_fd(),
                actual.as_mut_ptr().cast(),
                actual.len(),
                0,
            )
        },
        actual.len() as isize
    );
    assert_eq!(actual.as_slice(), wire::SOURCE_BYTES);
}
#[test]
fn renamed_and_restored_source_root_remains_original_but_fails_earlier_stamp() {
    let inputs = Inputs::new();
    let moved = inputs.path.with_extension("moved");
    fs::rename(&inputs.path, &moved).unwrap();
    fs::create_dir(&inputs.path).unwrap();
    assert!(inputs.source.validate(&inputs.request, deadline()).is_err());
    fs::remove_dir(&inputs.path).unwrap();
    fs::rename(&moved, &inputs.path).unwrap();
    wire::transport::check_source(
        &inputs.request,
        &inputs.source.descriptions.each_ref().map(AsFd::as_fd),
        deadline(),
    )
    .unwrap();
    assert!(inputs.source.validate(&inputs.request, deadline()).is_err());
}

#[test]
fn restored_bootstrap_directory_cannot_hide_a_different_compiler_read_window() {
    let inputs = Inputs::new();
    let root_before = Stamp::of(
        &spawnfd::descriptor_metadata(inputs.source.descriptions[0].as_raw_fd()).unwrap(),
    );
    let file_before = Stamp::of(
        &spawnfd::descriptor_metadata(inputs.source.descriptions[1].as_raw_fd()).unwrap(),
    );
    let bootstrap = inputs.path.join("src/bootstrap");
    let saved = inputs.path.join("src/original-bootstrap");
    fs::rename(&bootstrap, &saved).unwrap();
    fs::create_dir(&bootstrap).unwrap();
    let mut replacement = wire::SOURCE_BYTES.to_vec();
    replacement[0] ^= 1;
    fs::write(bootstrap.join("native_object.rs"), replacement).unwrap();
    assert!(inputs.source.validate(&inputs.request, deadline()).is_err());
    fs::remove_dir_all(&bootstrap).unwrap();
    fs::rename(&saved, &bootstrap).unwrap();
    assert!(
        Stamp::of(
            &spawnfd::descriptor_metadata(inputs.source.descriptions[0].as_raw_fd()).unwrap()
        ) == root_before
    );
    assert!(
        Stamp::of(
            &spawnfd::descriptor_metadata(inputs.source.descriptions[1].as_raw_fd()).unwrap()
        ) == file_before
    );
    // Both selected Root and source File are genuinely unchanged. Fresh wire
    // snapshots also see the restored canonical chain, so neither substitutes
    // for the actual intermediate directory origins held across execution.
    wire::transport::check_source(
        &inputs.request,
        &inputs.source.descriptions.each_ref().map(AsFd::as_fd),
        deadline(),
    )
    .unwrap();
    assert!(inputs.source.validate(&inputs.request, deadline()).is_err());
    for original in &inputs.source.parents {
        assert!(original.file.metadata().unwrap().is_dir());
    }
}
