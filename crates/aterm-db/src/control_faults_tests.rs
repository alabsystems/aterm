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
