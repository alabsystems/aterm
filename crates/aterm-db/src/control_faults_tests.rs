// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

thread_local! {
    static RECORDS_UNREAD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `run` with every read of this Mac's membership records by a peer check failing in
/// this thread, as a read fails while a writer replaces the file under it.
pub(crate) fn records_unread<T>(run: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            RECORDS_UNREAD.with(|slot| slot.set(self.0));
        }
    }
    let _restore = Restore(RECORDS_UNREAD.with(|slot| slot.replace(true)));
    run()
}

/// What a peer check's read of this Mac's membership records meets in this thread: an
/// error under [`records_unread`].
pub(crate) fn records_read() -> aterm_secrets::Result<()> {
    if RECORDS_UNREAD.with(std::cell::Cell::get) {
        return Err(aterm_secrets::Error::new(
            "membership records changed between reads",
        ));
    }
    Ok(())
}

/// What a test's Mac serves in place of its own roots and records on the records wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Served {
    /// A stub's clock moved a millisecond: a stub set the root's digest does not bind.
    Forged,
    /// One share of the root left out: a root its other shares alone do not make final.
    Unfinal,
    /// The frame names another vault's log.
    Foreign,
}

thread_local! {
    static SERVED: std::cell::Cell<Option<Served>> = const { std::cell::Cell::new(None) };
}

/// Run `run` with every served batch this thread sends rewritten as `fault` says.
pub(crate) fn serving<T>(fault: Served, run: impl FnOnce() -> T) -> T {
    struct Restore(Option<Served>);
    impl Drop for Restore {
        fn drop(&mut self) {
            SERVED.with(|slot| slot.set(self.0));
        }
    }
    let _restore = Restore(SERVED.with(|slot| slot.replace(Some(fault))));
    run()
}

/// The records a served batch of this thread sends beside `served`, which it rewrites, under
/// [`serving`]: `records` as they are where no fault is held.
pub(crate) fn served_as(
    served: &mut aterm_secrets::membership_log::Served,
    records: &[aterm_secrets::membership_log::Record],
) -> Vec<aterm_secrets::membership_log::Record> {
    let mut records = records.to_vec();
    match SERVED.with(std::cell::Cell::get) {
        None => {}
        Some(Served::Forged) => {
            if let Some(stub) = served.stubs.values_mut().next() {
                stub.authored_ms += 1;
            }
        }
        Some(Served::Unfinal) => {
            if let Some(at) = records.iter().position(|record| {
                aterm_secrets::membership_log::checkpoint::checkpoint_id(record.body()).is_some()
            }) {
                records.remove(at);
            }
        }
        Some(Served::Foreign) => served.network = [0xa5; 32],
    }
    records
}

thread_local! {
    static LATER: std::cell::Cell<std::time::Duration> =
        const { std::cell::Cell::new(std::time::Duration::ZERO) };
}

/// Run `run` with this thread's records exchange reading its holds `by` later than the wall
/// ([`clock`]).
pub(crate) fn later<T>(by: std::time::Duration, run: impl FnOnce() -> T) -> T {
    struct Restore(std::time::Duration);
    impl Drop for Restore {
        fn drop(&mut self) {
            LATER.with(|slot| slot.set(self.0));
        }
    }
    let _restore = Restore(LATER.with(|slot| slot.replace(by)));
    run()
}

/// `now` as this thread's records exchange reads it under [`later`].
pub(crate) fn clock(now: std::time::Instant) -> std::time::Instant {
    now + LATER.with(std::cell::Cell::get)
}
