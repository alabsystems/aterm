// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Explicit before-effect cancellation. A dropped reservation, missing reply,
//! or prepared record never proves that installation did not start.

use super::*;

pub(in crate::harness) struct Cancelled {
    caller: Caller,
    operation: String,
    namespace: BookNamespace,
}

/// Borrowed only while the exact signed no-effect source Book is claimed.
/// Kernel restoration grants no credential, member, or target authority.
pub(in crate::harness) struct CancelledSource<'a> {
    intent: &'a TargetIntent,
    namespace: &'a BookNamespace,
    _claimed: &'a Claimed,
}
impl CancelledSource<'_> {
    pub(in crate::harness) fn caller(&self) -> &Caller {
        self.intent.caller()
    }
    pub(in crate::harness) fn matches_source(
        &self,
        source: &crate::harness::codex_session_slot::SourceHandle,
    ) -> bool {
        source.binding() == &self.intent.record.source
            && source.source_authority() == &self.intent.record.source_authority
            && source.assignment_generation() == self.intent.record.body.expected_assignment
            && source
                .namespaces()
                .is_some_and(|scope| scope.book() == self.namespace)
    }
}

impl Cancelled {
    pub(in crate::harness) fn with_current_source<R>(
        &self,
        env: &Env,
        intent: &TargetIntent,
        namespace: &BookNamespace,
        restore: impl FnOnce(&CancelledSource<'_>) -> Result<R, Failure>,
    ) -> Result<R, Failure> {
        if !self.matches(intent.caller(), intent.operation(), namespace) {
            return Err(Failure::Unavailable);
        }
        let lease = open(env, intent.caller())?;
        let claimed = lease.try_lock().map_err(|_| Failure::Busy)?;
        if claimed.book.consumer != Consumer::observe(intent.caller())? {
            return Err(Failure::Unavailable);
        }
        with_cancelled_claim(&claimed, intent, namespace, restore)
    }
    pub(in crate::harness) fn matches(
        &self,
        caller: &Caller,
        operation: &str,
        namespace: &BookNamespace,
    ) -> bool {
        &self.caller == caller && self.operation == operation && &self.namespace == namespace
    }
}

fn with_cancelled_claim<R>(
    claimed: &Claimed,
    intent: &TargetIntent,
    namespace: &BookNamespace,
    restore: impl FnOnce(&CancelledSource<'_>) -> Result<R, Failure>,
) -> Result<R, Failure> {
    claimed.store.unchanged().map_err(str::to_owned)?;
    if claimed.pending_install.is_some()
        || !namespace.current()
        || claimed.store.physical_identity().map_err(str::to_owned)? != *namespace
        || !claimed.book.valid(intent.caller())
        || !origin_namespace_matches(&claimed.book, intent)?
        || !claimed
            .book
            .prepared
            .iter()
            .any(|record| record.body.operation == intent.operation() && record.cancelled)
    {
        return Err(Failure::Unavailable);
    }
    let result = restore(&CancelledSource {
        intent,
        namespace,
        _claimed: claimed,
    })?;
    claimed.store.unchanged().map_err(str::to_owned)?;
    if !namespace.current() {
        return Err(Failure::Unavailable);
    }
    Ok(result)
}

impl std::fmt::Debug for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CancelledAssignment(<private>)")
    }
}

pub(in crate::harness) enum Progress {
    #[cfg_attr(
        not(unix),
        expect(
            dead_code,
            reason = "the opaque cancellation receipt contains an uninhabited unsupported physical namespace"
        )
    )]
    Cancelled(Cancelled),
    // Reconciliation is required. This is no target, source, or Applied proof.
    EffectsStarted,
}

pub(in crate::harness) enum Failure {
    Busy,
    Unavailable,
}
impl From<String> for Failure {
    fn from(_: String) -> Self {
        Self::Unavailable
    }
}
impl From<&str> for Failure {
    fn from(_: &str) -> Self {
        Self::Unavailable
    }
}

/// `validate` checks the already-fenced actual reservation while Book is
/// claimed. It must perform no I/O or acquire owner/account custody.
pub(in crate::harness) fn before_effects(
    env: &Env,
    intent: &TargetIntent,
    namespace: &BookNamespace,
    validate: impl FnOnce() -> Result<(), Failure>,
) -> Result<Progress, Failure> {
    platform::require_available()?;
    let home = env.home.as_deref().ok_or("assignment home unavailable")?;
    let identity = platform::machine_identity(home)
        .map_err(|_| "assignment identity unavailable")?
        .ok_or("assignment identity unavailable")?;
    let lease = open(env, intent.caller())?;
    let mut claimed = lease.try_lock().map_err(|_| Failure::Busy)?;
    claimed.store.unchanged().map_err(str::to_owned)?;
    if !namespace.current()
        || claimed.store.physical_identity().map_err(str::to_owned)? != *namespace
    {
        return Err("assignment cancellation namespace changed".into());
    }
    validate()?;
    persist_cancelled(&mut claimed, &identity, intent, namespace).map_err(Failure::from)
}

fn persist_cancelled(
    claimed: &mut Claimed,
    identity: &age::Identity,
    intent: &TargetIntent,
    namespace: &BookNamespace,
) -> Result<Progress, String> {
    claimed.store.unchanged().map_err(str::to_owned)?;
    if !namespace.current()
        || claimed.store.physical_identity().map_err(str::to_owned)? != *namespace
    {
        return Err("assignment cancellation namespace changed".into());
    }
    let index = claimed
        .book
        .prepared
        .iter()
        .position(|record| record.body.operation == intent.operation())
        .ok_or("assignment cancellation intent unavailable")?;
    let record = &claimed.book.prepared[index];
    let mut expected = intent.record.clone();
    expected.cancelled = record.cancelled;
    if !same_prepared(record, &expected) {
        return Err("assignment cancellation intent changed".into());
    }
    if claimed.pending_install.is_some()
        || record.commit_boundary.is_some()
        || claimed.book.current.authority.native_intent() == Some(intent.operation())
    {
        return Ok(Progress::EffectsStarted);
    }
    if claimed.book.current.binding != record.source
        || claimed.book.current.authority != record.source_authority
        || claimed.book.current.generation != record.body.expected_assignment
        || !matches!(claimed.book.current.phase, Phase::Startup | Phase::Applied)
    {
        return Err("assignment cancellation source changed".into());
    }
    // The exact terminal intent remains signed and retained. A replay retries
    // the same write, including after a save whose result was not observed.
    claimed.book.prepared[index].cancelled = true;
    publish(claimed, identity)?;
    claimed.store.unchanged().map_err(str::to_owned)?;
    if !namespace.current() {
        return Err("assignment cancellation namespace changed".into());
    }
    #[cfg(unix)]
    {
        Ok(Progress::Cancelled(Cancelled {
            caller: intent.caller().clone(),
            operation: intent.operation().into(),
            namespace: namespace.clone(),
        }))
    }
    #[cfg(not(unix))]
    {
        Err("native credential custody is unsupported on this platform".into())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn cancelled_intent_reopens_signed_and_cannot_be_admitted_again() {
        // Actual encrypted store/MAC and exact replay, synthetic consumer and
        // target metadata. This does not qualify a supported SDK/owner pair.
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        aterm_secrets::fsx::ensure_private_dir(&home).unwrap();
        let paths = aterm_secrets::client::Paths::locate_for_home(&home);
        aterm_secrets::client::machine_identity_as(
            &paths,
            aterm_secrets::Making {
                test_binary: true,
                ..aterm_secrets::Making::default()
            },
        )
        .unwrap();
        let identity = aterm_secrets::client::read_machine_identity(&paths)
            .unwrap()
            .unwrap();
        let directory = root.path().join("book");
        let (store, _) =
            crate::harness::pending_native_store::Store::open_owned(&directory, &home, MAX_BOOK)
                .unwrap();
        let mut record = super::super::tests::record();
        record.body.expected_assignment = 7;
        record.source_authority = SourceAuthority::Assignment {
            operation: "6".repeat(32),
        };
        let mut book = super::super::tests::book(&record);
        book.clock = 7;
        book.current.generation = 7;
        book.current.previous_generation = 6;
        book.current.phase = Phase::Applied;
        book.current.boundary = Some("7".repeat(32));
        book.schema = 3;
        book.prepared.push(record.clone());
        let intent = TargetIntent {
            record: record.clone(),
            caller: book.caller.clone(),
        };
        let mut claimed = Claimed {
            store,
            book,
            pending_install: None,
        };
        publish(&mut claimed, &identity).unwrap();
        let namespace = claimed.store.physical_identity().unwrap();
        let original = aterm_json::to_vec(&claimed.book.current).unwrap();
        let cancelled =
            match persist_cancelled(&mut claimed, &identity, &intent, &namespace).unwrap() {
                Progress::Cancelled(receipt) => receipt,
                Progress::EffectsStarted => panic!("no target effect was started"),
            };
        assert!(cancelled.matches(&intent.caller, intent.operation(), &namespace));
        assert_eq!(aterm_json::to_vec(&claimed.book.current).unwrap(), original);
        // Source metadata injection tests matching only; it does not mint a
        // native Slot, supported image, owner or rollback kernel authority.
        use crate::harness::codex_session_slot::metadata_fixture as slot_fixture;
        let source = |binding, authority, book_namespace| {
            slot_fixture::with_namespaces(
                slot_fixture::with_authority(
                    slot_fixture::with_assignment(
                        slot_fixture::source(binding, home.join("auth.json"), 1),
                        7,
                        &"6".repeat(32),
                    ),
                    authority,
                ),
                book_namespace,
                namespace.clone(),
            )
        };
        let actual = source(
            record.source.clone(),
            record.source_authority.clone(),
            namespace.clone(),
        );
        let wrong = source(
            record.target.as_ref().unwrap().binding.clone(),
            record.source_authority.clone(),
            namespace.clone(),
        );
        let wrong_epoch = source(
            record.source.clone(),
            SourceAuthority::Assignment {
                operation: "8".repeat(32),
            },
            namespace.clone(),
        );
        let (foreign, _) = crate::harness::pending_native_store::Store::open_owned(
            &root.path().join("foreign-book"),
            &home,
            MAX_BOOK,
        )
        .unwrap();
        let wrong_namespace = source(
            record.source.clone(),
            record.source_authority.clone(),
            foreign.physical_identity().unwrap(),
        );
        let restored = std::cell::Cell::new(false);
        assert!(
            with_cancelled_claim(&claimed, &intent, &namespace, |permit| {
                assert!(permit.caller() == intent.caller());
                assert!(permit.matches_source(&actual));
                assert!(!permit.matches_source(&wrong));
                assert!(!permit.matches_source(&wrong_epoch));
                assert!(!permit.matches_source(&wrong_namespace));
                restored.set(true);
                Ok(())
            })
            .is_ok()
        );
        assert!(restored.get());
        let saved = claimed.book.prepared[0].commit_boundary.clone();
        claimed.book.prepared[0].commit_boundary = Some("a".repeat(32));
        restored.set(false);
        assert!(
            with_cancelled_claim(&claimed, &intent, &namespace, |_| {
                restored.set(true);
                Ok(())
            })
            .is_err()
        );
        assert!(
            !restored.get(),
            "effects must never produce a rollback permit"
        );
        claimed.book.prepared[0].commit_boundary = saved;
        // Retrying an unobserved write returns the same semantic receipt and
        // does not allocate another operation or mutate the current source.
        assert!(matches!(
            persist_cancelled(&mut claimed, &identity, &intent, &namespace).unwrap(),
            Progress::Cancelled(_)
        ));
        assert_eq!(claimed.book.prepared.len(), 1);
        assert!(admitted(&claimed.book.prepared, &record.body).is_err());
        assert!(
            origin_namespace_matches(&claimed.book, &intent).unwrap(),
            "signed cancellation of B does not change already-admitted A's namespace"
        );
        drop(claimed);
        let (_store, bytes) = crate::harness::pending_native_store::Store::open_owned_existing(
            &directory, &home, MAX_BOOK,
        )
        .unwrap()
        .unwrap();
        let envelope: Envelope = aterm_json::from_slice(bytes.unwrap().as_slice()).unwrap();
        assert_eq!(signature(&identity, &envelope.book).unwrap(), envelope.mac);
        assert!(envelope.book.valid(&envelope.book.caller));
        assert!(admitted(&envelope.book.prepared, &record.body).is_err());
        assert!(origin_namespace_matches(&envelope.book, &intent).unwrap());
        assert_eq!(
            aterm_json::to_vec(&envelope.book.current).unwrap(),
            original
        );
        let mut tampered = envelope.book;
        tampered.prepared[0].cancelled = false;
        assert_ne!(signature(&identity, &tampered).unwrap(), envelope.mac);
    }
}
