// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Receiver DATA/descriptor controls only. These fixtures mint no Manager,
//! ProtectedWorkScope, SDK Keeper, authenticated Job or positive root closure.
use super::*;
use std::{
    fs::{self, File, OpenOptions},
    os::{fd::AsRawFd as _, unix::fs::OpenOptionsExt as _},
};
use trust_resource_controller::{
    BirthKind, Context, ContextQuery, Cpu, FileIdentity, Identity, JobBinding, Memory, Subject,
};

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(60)
}
fn key() -> wire::Key {
    wire::Key {
        query: ContextQuery {
            binding: JobBinding {
                session: "s-abcd".into(),
                root_attempt: [1; 16],
                capability: [9; 32],
            },
            call_attempt: [2; 16],
        },
        graph: [3; 16],
    }
}
fn context() -> trust_resource_controller::ContextReply {
    let identity = Identity {
        pid: 42,
        birth: 77,
        birth_kind: BirthKind::MacosWallMicroseconds,
    };
    trust_resource_controller::ContextReply {
        call_attempt: key().query.call_attempt,
        context: Context {
            session: key().query.binding.session,
            root_attempt: key().query.binding.root_attempt,
            host: "fixture".into(),
            boot: "fixture".into(),
            original_root: identity,
            subject: Subject::Root { identity },
            cpu: Cpu::RetainedFixed {
                units: 1,
                native_manifest: FileIdentity {
                    device: 1,
                    inode: 1,
                },
            },
            memory: Memory {
                bytes: 1024,
                namespace: [4; 16],
                marker: "fixture".into(),
                epoch: [5; 16],
                guard: FileIdentity {
                    device: 1,
                    inode: 2,
                },
            },
            sdk: None,
        },
    }
}
fn identity(file: &File) -> FileIdentity {
    let m = file.metadata().unwrap();
    FileIdentity {
        device: m.dev(),
        inode: m.ino(),
    }
}
fn record(file: &File, bytes: &[u8]) -> wire::SourceRecord {
    wire::SourceRecord {
        identity: identity(file),
        bytes: bytes.len() as u64,
        sha256: aterm_digest::Sha256::digest(bytes),
    }
}
struct Inputs {
    _scratch: aterm_tempfile::TempDir,
    original: Arc<OriginalSource>,
}
impl Inputs {
    fn new() -> Self {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = scratch.path().canonicalize().unwrap();
        let holder = path.join(".trust-build-lock");
        fs::create_dir(&holder).unwrap();
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)
            .unwrap();
        let holder_directory = File::open(&holder).unwrap();
        let acquired = b"fixture-original-record";
        let withheld = b"fixture-withheld-record";
        fs::write(holder.join("info"), acquired).unwrap();
        let original_record = File::open(holder.join("info")).unwrap();
        fs::rename(holder.join("info"), holder.join("old-info")).unwrap();
        fs::write(holder.join("info"), withheld).unwrap();
        let withheld_record = File::open(holder.join("info")).unwrap();
        let request = wire::SourceAttachment {
            key: key(),
            source_directory: identity(&directory),
            holder_directory: identity(&holder_directory),
            holder_name: b".trust-build-lock".to_vec(),
            original_record: record(&original_record, acquired),
            withheld_record: record(&withheld_record, withheld),
        };
        let original = Arc::new(OriginalSource {
            request,
            path,
            descriptions: [
                directory.into(),
                holder_directory.into(),
                original_record.into(),
                withheld_record.into(),
            ],
            revoked: AtomicBool::new(false),
            active: Arc::new(AtomicBool::new(true)),
            deadline: deadline(),
        });
        original.check_current(deadline()).unwrap();
        Self {
            _scratch: scratch,
            original,
        }
    }
    fn preparing(&self) -> Preparing {
        Preparing {
            request: wire::Enrollment {
                key: key(),
                source: self.original.path.as_os_str().as_bytes().to_vec(),
                output: b"/fixture-output".to_vec(),
                gnu: wire::GnuIdentity::NumericPipe {
                    read: FileIdentity {
                        device: 1,
                        inode: 10,
                    },
                    write: FileIdentity {
                        device: 1,
                        inode: 11,
                    },
                },
            },
            context: context(),
            peer: Peer { pid: 42, birth: 77 },
            active: Arc::clone(&self.original.active),
            deadline: self.original.deadline,
            submitted: false,
            source_submitted: false,
            output_submitted: false,
        }
    }
}

#[test]
fn original_preparing_requires_ordered_same_graph_one_use_source() {
    let inputs = Inputs::new();
    let mut preparing = inputs.preparing();
    let source = inputs.original.request.clone();
    let output = wire::OutputAttachment {
        key: key(),
        root: FileIdentity {
            device: 1,
            inode: 20,
        },
        lock: FileIdentity {
            device: 1,
            inode: 21,
        },
    };
    assert!(preparing.source_transfer(source.clone()).is_err());
    preparing.enrollment_attachment().unwrap();
    assert!(preparing.output_transfer(output.clone()).is_err());
    let mut foreign = source.clone();
    foreign.key.graph = [7; 16];
    assert!(preparing.source_transfer(foreign).is_err());
    let transfer = preparing.source_transfer(source.clone()).unwrap();
    assert!(Arc::ptr_eq(&transfer.active, &preparing.active));
    assert!(preparing.source_transfer(source).is_err());
    preparing.output_transfer(output.clone()).unwrap();
    assert!(preparing.output_transfer(output).is_err());
    drop(preparing);
    assert!(!transfer.active.load(Ordering::Acquire));
    assert!(inputs.original.check_current(deadline()).is_err());
}

#[test]
fn source_checks_leave_original_cursors_and_ofd_flags_unchanged() {
    let inputs = Inputs::new();
    for fd in &inputs.original.descriptions[2..] {
        let raw = fd.as_raw_fd();
        assert_eq!(unsafe { libc::lseek(raw, 3, libc::SEEK_SET) }, 3);
        let flags = unsafe { libc::fcntl(raw, libc::F_GETFL) };
        let local = unsafe { libc::fcntl(raw, libc::F_GETFD) };
        inputs.original.check_current(deadline()).unwrap();
        assert_eq!(unsafe { libc::lseek(raw, 0, libc::SEEK_CUR) }, 3);
        assert_eq!(unsafe { libc::fcntl(raw, libc::F_GETFL) }, flags);
        assert_eq!(unsafe { libc::fcntl(raw, libc::F_GETFD) }, local);
    }
}

#[test]
fn source_revocation_retains_close_only_originals_but_refuses_new_borrows() {
    let inputs = Inputs::new();
    let preparing = inputs.preparing();
    let retained = Arc::clone(&inputs.original);
    drop(preparing);
    assert!(retained.directory(deadline()).is_err());
    assert!(
        retained
            .matches(
                &key(),
                retained.path.as_os_str().as_bytes(),
                retained.request.source_directory,
                deadline()
            )
            .is_err()
    );
    // Revocation is not retirement: the SAME originals remain held and readable.
    let mut byte = [0];
    assert_eq!(
        unsafe {
            libc::pread(
                retained.descriptions[2].as_raw_fd(),
                byte.as_mut_ptr().cast(),
                1,
                0,
            )
        },
        1
    );
    assert_eq!(byte[0], b'f');
}

#[test]
fn source_root_restore_cannot_reactivate_the_original_offer() {
    let inputs = Inputs::new();
    let source = &inputs.original;
    let old = source.path.with_extension("old");
    fs::rename(&source.path, &old).unwrap();
    fs::create_dir(&source.path).unwrap();
    assert!(source.check_current(deadline()).is_err());
    fs::remove_dir(&source.path).unwrap();
    fs::rename(&old, &source.path).unwrap();
    assert!(source.check_current(deadline()).is_err());
    assert!(source.directory(deadline()).is_err());
    assert!(source.revoked.load(Ordering::Acquire));
    assert!(source.active.load(Ordering::Acquire));
    // Refusal retains the actual old descriptions rather than retiring them.
    assert_eq!(
        spawnfd::descriptor_metadata(source.descriptions[0].as_raw_fd())
            .unwrap()
            .ino(),
        source.request.source_directory.inode
    );
}

#[test]
fn source_refusal_does_not_finish_the_actual_receiving_handler() {
    let inputs = Inputs::new();
    let preparing = inputs.preparing();
    // A test-only input census exercises the production handler barrier, not
    // an authenticated Job or a synthetic positive root-closure capability.
    let state = State {
        attempt: Some(Attempt {
            request: preparing.request.clone(),
            binding: Binding {
                session: key().query.binding.session,
                attempt: key().query.binding.root_attempt,
            },
            group: 42,
            leader_serial: 77,
            birth: 77,
            active: Arc::clone(&preparing.active),
            gnu: None,
            control: None,
            source_requested: Some(inputs.original.request.clone()),
            source: Some(Arc::clone(&inputs.original)),
            output_requested: None,
            output: None,
        }),
    };
    assert!(!state.preparation_finished());
    let name = inputs.original.path.join(".trust-build-lock/info");
    fs::write(&name, b"fixture-changed-record!").unwrap();
    assert!(inputs.original.check_current(deadline()).is_err());
    fs::write(&name, b"fixture-withheld-record").unwrap();
    assert!(inputs.original.check_current(deadline()).is_err());
    assert!(
        !state.preparation_finished(),
        "input refusal cannot stand in for handler Drop"
    );
    drop(preparing);
    assert!(state.preparation_finished());
    let source = state.attempt.as_ref().unwrap().source.as_ref().unwrap();
    assert!(
        spawnfd::descriptor_metadata(source.descriptions[2].as_raw_fd())
            .unwrap()
            .is_file()
    );
}

#[test]
fn offered_source_rejects_holder_record_substitution() {
    let inputs = Inputs::new();
    let source = &inputs.original;
    let holder = source.path.join(".trust-build-lock");
    fs::rename(holder.join("info"), holder.join("held-info")).unwrap();
    fs::write(holder.join("info"), b"fixture-withheld-record").unwrap();
    assert!(source.check_current(deadline()).is_err());
    assert!(source.revoked.load(Ordering::Acquire));
}

#[test]
fn source_deadline_and_writable_original_are_not_admitted() {
    let mut inputs = Inputs::new();
    let source = Arc::get_mut(&mut inputs.original).unwrap();
    let original_deadline = source.deadline;
    source.deadline = Instant::now();
    assert_eq!(
        source.check_current(deadline()).unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
    source.deadline = original_deadline;
    assert!(source.directory(deadline()).is_err());
    assert!(source.revoked.load(Ordering::Acquire));
    let mut writable = Inputs::new();
    let source = Arc::get_mut(&mut writable.original).unwrap();
    source.descriptions[2] = OpenOptions::new()
        .read(true)
        .write(true)
        .open(source.path.join(".trust-build-lock/old-info"))
        .unwrap()
        .into();
    assert!(source.check_current(deadline()).is_err());
    assert!(source.revoked.load(Ordering::Acquire));
}

#[test]
fn native_child_reads_the_same_accepted_unlinked_source_record_description() {
    let inputs = Inputs::new();
    fs::remove_file(inputs.original.path.join(".trust-build-lock/old-info")).unwrap();
    inputs.original.check_current(deadline()).unwrap();
    let mut command = spawnfd::command("/bin/cat");
    command.arg("/dev/fd/6");
    command
        .inherit_guards_at(vec![(
            aterm_uds::kept_out(|| inputs.original.descriptions[2].try_clone()).unwrap(),
            6,
        )])
        .unwrap();
    let output = aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60)).unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"fixture-original-record");
    inputs.original.check_current(deadline()).unwrap();
}

#[test]
fn object_source_requires_actual_acceptance_and_exact_enrolled_identity() {
    let inputs = Inputs::new();
    assert!(
        State::default()
            .original_source(&key(), deadline())
            .is_err()
    );
    let source = &inputs.original;
    source
        .matches(
            &key(),
            source.path.as_os_str().as_bytes(),
            source.request.source_directory,
            deadline(),
        )
        .unwrap();
    let mut foreign = key();
    foreign.graph = [7; 16];
    assert!(
        source
            .matches(
                &foreign,
                source.path.as_os_str().as_bytes(),
                source.request.source_directory,
                deadline()
            )
            .is_err()
    );
    assert!(
        source
            .matches(
                &key(),
                b"/other-source",
                source.request.source_directory,
                deadline()
            )
            .is_err()
    );
    let mut wrong = source.request.source_directory;
    wrong.inode += 1;
    assert!(
        source
            .matches(
                &key(),
                source.path.as_os_str().as_bytes(),
                wrong,
                deadline()
            )
            .is_err()
    );
}
