// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Keep launchd's existing log inode bounded; renaming its path would leave launchd writing
//! the old inode. All broker lines use this one serialized writer.

use std::fs::File;
use std::io::{self, Seek as _, Write as _};
use std::os::fd::AsFd as _;
use std::sync::{Mutex, OnceLock, PoisonError};

const CAP: u64 = 512 * 1024;
const LINE_CAP: usize = 4096;

static LOG: OnceLock<Mutex<Sink>> = OnceLock::new();

enum Sink {
    File(Bounded),
    Stream,
    Failed,
}

pub(crate) fn line(words: std::fmt::Arguments<'_>) {
    let mut text = format!("aterm-lan: {words}");
    if text.len() >= LINE_CAP {
        let mut end = LINE_CAP - 1;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    // A diagnostic is one line even when a client's value contains a newline.
    text = text.replace(['\r', '\n'], " ");
    text.push('\n');
    let mut sink = LOG
        .get_or_init(|| {
            let sink = aterm_uds::kept_out(|| io::stderr().as_fd().try_clone_to_owned())
                .map(File::from)
                .and_then(|file| file.metadata().map(|meta| (file, meta)))
                .map(|(file, meta)| {
                    if meta.is_file() {
                        Sink::File(Bounded { file, cap: CAP })
                    } else {
                        Sink::Stream
                    }
                })
                .unwrap_or(Sink::Failed);
            Mutex::new(sink)
        })
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if let Sink::File(sink) = &mut *sink {
        // A failed trim must not fall back to an unbounded append to the same file.
        let _ = sink.write(text.as_bytes());
    } else if matches!(*sink, Sink::Stream) {
        let _ = io::stderr().write_all(text.as_bytes());
    }
}

struct Bounded {
    file: File,
    cap: u64,
}

impl Bounded {
    fn write(&mut self, line: &[u8]) -> io::Result<()> {
        if line.len() as u64 > self.cap {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "log line exceeds cap",
            ));
        }
        if self
            .file
            .metadata()?
            .len()
            .saturating_add(line.len() as u64)
            > self.cap
        {
            self.file.set_len(0)?;
        }
        // launchd normally opens with O_APPEND. Seeking also covers a regular stderr file
        // opened without it, whose old offset would otherwise make a hole after truncation.
        self.file.seek(io::SeekFrom::End(0))?;
        self.file.write_all(line)
    }
}

#[cfg(test)]
#[path = "log_tests.rs"]
mod tests;
