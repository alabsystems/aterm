// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Raw descriptor/state and real fresh-cache lifetime controls. The test-only
//! State census supplies no authenticated Job, OutputLock, SDK, GNU grant or
//! Root closure; those remain the separate native engineering actor fixture.
use super::super::tests::Inputs;
use super::*;
use std::os::unix::fs::OpenOptionsExt as _;

struct Fixture {
    state: State,
    _handler: Preparing,
    source: Inputs,
    _parent: aterm_tempfile::TempDir,
    workspace_path: std::path::PathBuf,
    cache: std::path::PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let source = Inputs::new();
        let preparing = source.preparing();
        let bytes = b"[build]\njobs=2\n";
        let name = source.original.path.join("config.toml");
        std::fs::write(&name, bytes).unwrap();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(name)
            .unwrap();
        let metadata = file.metadata().unwrap();
        let request = wire::ConfigurationAttachment {
            key: preparing.request.key.clone(),
            sequence: 1,
            relative: b"config.toml".to_vec(),
            file: trust_resource_controller::FileIdentity {
                device: metadata.dev(),
                inode: metadata.ino(),
            },
            bytes: bytes.len() as u64,
            sha256: aterm_digest::Sha256::digest(bytes),
            modified: wire::configuration::Timestamp {
                seconds: metadata.mtime(),
                nanoseconds: metadata.mtime_nsec().try_into().unwrap(),
            },
            changed: wire::configuration::Timestamp {
                seconds: metadata.ctime(),
                nanoseconds: metadata.ctime_nsec().try_into().unwrap(),
            },
            input_remaining_ns: 60_000_000_000,
        };
        let configuration = Arc::new(OriginalConfiguration {
            request: request.clone(),
            source: Arc::clone(&source.original),
            description: file.into(),
            deadline: preparing.deadline,
            revoked: AtomicBool::new(false),
        });
        let parent = aterm_tempfile::tempdir().unwrap();
        let workspace = atpkg::private_input::MaterializationWorkspace::create(
            parent.path(),
            "workspace",
            preparing.deadline,
        )
        .unwrap();
        let workspace_path = workspace.path().to_owned();
        let cache = workspace.cargo_home().join(".package-cache-mutate");
        let mut budget = wire::configuration::ConfigurationBudget::default();
        budget.reserve(&request).unwrap();
        // These are only retained descriptor-population sentinels. No output
        // ownership is inferred from them by this private State unit control.
        let output = [
            source.original.descriptions[0].try_clone().unwrap(),
            source.original.descriptions[1].try_clone().unwrap(),
        ];
        let state = State {
            attempt: Some(Attempt {
                request: preparing.request.clone(),
                binding: Binding {
                    session: preparing.request.key.query.binding.session.clone(),
                    attempt: preparing.request.key.query.binding.root_attempt,
                },
                group: 42,
                leader_serial: 77,
                birth: 77,
                active: Arc::clone(&source.original.active),
                gnu: None,
                control: None,
                source_requested: Some(source.original.request.clone()),
                source: Some(Arc::clone(&source.original)),
                configuration_requested: Some(request),
                materialization_requested: Some(wire::MaterializationPreparation {
                    key: preparing.request.key.clone(),
                    input_remaining_ns: 60_000_000_000,
                }),
                configuration: vec![configuration],
                configuration_budget: budget,
                configuration_rejected: Vec::new(),
                workspace: Some(workspace),
                materialization_inputs: None,
                configuration_anchor: preparing.configuration_anchor,
                output_requested: None,
                output: Some(Arc::new(output)),
            }),
        };
        // Preparing normally owns handler activity. This test retains its
        // original Arc separately, without pretending to authenticate a peer.
        Self {
            state,
            _handler: preparing,
            source,
            _parent: parent,
            workspace_path,
            cache,
        }
    }
    fn key(&self) -> wire::Key {
        self.source.original.request.key.clone()
    }
    fn deadline(&self) -> Instant {
        self.source.original.deadline
    }
    fn seal(&mut self) -> io::Result<()> {
        self.state
            .seal_materialization_inputs(&self.key(), self.deadline())
    }
}
fn locked(path: &std::path::Path) -> bool {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    matches!(
        aterm_toolchain::record_lock::RecordLock::try_lock(file, 0),
        Err(std::fs::TryLockError::WouldBlock)
    )
}

#[test]
fn same_actual_bundle_survives_state_drop_and_retains_real_cache_until_last_arc() {
    let mut fixture = Fixture::new();
    let point = fixture.state.input_deadline().unwrap();
    fixture.seal().unwrap();
    let first = fixture
        .state
        .original_materialization_inputs(&fixture.key(), fixture.deadline())
        .unwrap();
    let second = fixture
        .state
        .original_materialization_inputs(&fixture.key(), fixture.deadline())
        .unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(first.original_deadline(), point);
    assert!(fixture.state.attempt.as_ref().unwrap().workspace.is_none());
    let cache = first
        .with_original_directories(fixture.deadline(), |source, directory| {
            let original = std::fs::File::from(source.try_clone_to_owned()?).metadata()?;
            let named_source = std::fs::metadata(&fixture.source.original.path)?;
            assert_eq!(
                (original.dev(), original.ino()),
                (named_source.dev(), named_source.ino())
            );
            let metadata = std::fs::File::from(directory.try_clone_to_owned()?).metadata()?;
            assert_eq!((metadata.dev(), metadata.ino()), {
                let named = std::fs::metadata(&fixture.workspace_path)?;
                (named.dev(), named.ino())
            });
            Ok(fixture.cache.clone())
        })
        .unwrap();
    drop(fixture.state);
    assert!(locked(&cache));
    first
        .check_current(fixture.source.original.deadline)
        .unwrap();
    drop(first);
    assert!(locked(&cache));
    drop(second);
    assert!(!locked(&cache));
}

#[test]
fn expired_original_directory_borrow_starts_no_callback_and_stays_quarantined() {
    let mut fixture = Fixture::new();
    fixture.seal().unwrap();
    let original = fixture
        .state
        .original_materialization_inputs(&fixture.key(), fixture.deadline())
        .unwrap();
    let mut called = false;
    assert!(
        original
            .with_original_directories(Instant::now(), |_, _| {
                called = true;
                Ok(())
            })
            .is_err()
    );
    assert!(!called);
    assert!(original.check_current(fixture.deadline()).is_err());
    assert!(locked(&fixture.cache));
}

#[test]
fn pending_missing_and_revoked_configuration_cannot_issue_a_bundle() {
    for variant in 0..3 {
        let mut fixture = Fixture::new();
        let attempt = fixture.state.attempt.as_mut().unwrap();
        match variant {
            0 => {
                attempt.configuration.clear();
            }
            1 => {
                let mut next = attempt.configuration_requested.clone().unwrap();
                next.sequence = 2;
                attempt.configuration_budget.reserve(&next).unwrap();
                attempt.configuration_requested = Some(next);
            }
            _ => {
                attempt.configuration[0].quarantine();
            }
        }
        assert!(fixture.seal().is_err());
        let attempt = fixture.state.attempt.as_ref().unwrap();
        assert!(attempt.materialization_inputs.is_none());
        assert!(attempt.workspace.as_ref().unwrap().check_current().is_err());
        assert!(locked(
            &attempt
                .workspace
                .as_ref()
                .unwrap()
                .cargo_home()
                .join(".package-cache-mutate")
        ));
    }
}

#[test]
fn callback_refusal_retains_bundle_and_sticky_veto_preserves_the_handler_barrier() {
    let mut fixture = Fixture::new();
    fixture.seal().unwrap();
    let inputs = fixture
        .state
        .original_materialization_inputs(&fixture.key(), fixture.deadline())
        .unwrap();
    assert!(!fixture.state.preparation_finished());
    assert!(
        inputs
            .with_contents_directory::<()>(fixture.deadline(), |_| Err(
                io::ErrorKind::PermissionDenied.into()
            ))
            .is_err()
    );
    assert!(inputs.check_current(fixture.deadline()).is_err());
    assert!(!fixture.state.preparation_finished());
    assert!(locked(&fixture.cache));
    assert!(
        fixture
            .state
            .attempt
            .as_ref()
            .unwrap()
            .materialization_inputs
            .as_ref()
            .is_some_and(|same| Arc::ptr_eq(same, &inputs))
    );
}

#[test]
fn callback_unwind_checks_actual_configuration_and_permanently_quarantines_same_bundle() {
    let mut fixture = Fixture::new();
    fixture.seal().unwrap();
    let inputs = fixture
        .state
        .original_materialization_inputs(&fixture.key(), fixture.deadline())
        .unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _: io::Result<()> = inputs.with_contents_directory(fixture.deadline(), |_| {
            std::fs::write(fixture.source.original.path.join("config.toml"), b"changed")?;
            panic!("actual materialization callback unwound");
        });
    }));
    assert!(result.is_err());
    assert!(fixture.source.original.revoked.load(Ordering::Acquire));
    std::fs::write(
        fixture.source.original.path.join("config.toml"),
        b"[build]\njobs=2\n",
    )
    .unwrap();
    assert!(inputs.check_current(fixture.deadline()).is_err());
    assert!(locked(&fixture.cache));
    assert!(!fixture.state.preparation_finished());
}
