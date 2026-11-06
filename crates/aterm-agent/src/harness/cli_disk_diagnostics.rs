// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Human diagnostics before the disk result, without discovery or journal changes.
//! Delivery remains best effort and leaves flushing to the caller.

use std::io::{self, Write};
use std::path::Path;

use aterm_codec::tty::write_line;

pub(super) fn config_note(note: &str, err: &mut dyn Write) {
    let _ = write_line(err, format_args!("aterm harness disk: {note}"));
}

pub(super) fn ledger_cut_error(path: &Path, e: &io::Error, err: &mut dyn Write) {
    let _ = write_line(
        err,
        format_args!(
            "aterm harness disk: cannot cut {} back: {e}",
            path.display()
        ),
    );
}
