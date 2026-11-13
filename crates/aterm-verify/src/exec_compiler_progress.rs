// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A bounded sample from the owned compiler stdout, independent of its final capture.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Read as _};
use std::path::Path;

const BYTES_PER_POLL: usize = 64 << 10;
const LINE_BYTES: usize = 64 << 10;
const EVENTS_PER_POLL: usize = 16;
const EVENTS_TOTAL: usize = 4096;

pub(super) struct Progress {
    file: File,
    stage: String,
    pending: VecDeque<u8>,
    line: Vec<u8>,
    oversized: bool,
    unavailable: bool,
    emitted: usize,
}

impl Progress {
    /// Open a new description, never a clone sharing the compiler writer's cursor.
    pub(super) fn open(path: &Path, writer: &File, stage: &str) -> io::Result<Self> {
        let (file, _) = aterm_uds::regular_file::open(path)?;
        if !writer.metadata()?.is_file()
            || crate::run_log::file_key(&file)? != crate::run_log::file_key(writer)?
        {
            return Err(io::Error::other("compiler progress capture changed"));
        }
        Ok(Self {
            file,
            stage: stage.to_owned(),
            pending: VecDeque::new(),
            line: Vec::new(),
            oversized: false,
            unavailable: false,
            emitted: 0,
        })
    }

    /// One ordinary sample, also used once after positive reap. EOF is not retirement proof.
    pub(super) fn poll(&mut self, events: Option<&crate::event_log::EventLog>) {
        let batch = self.sample();
        crate::event_log::emit_progress_batch(events, &batch);
    }

    fn sample(&mut self) -> Vec<String> {
        let mut batch = Vec::new();
        if self.unavailable || self.emitted >= EVENTS_TOTAL {
            return batch;
        }
        let mut consumed = 0;
        let mut read = 0;
        while consumed < BYTES_PER_POLL
            && batch.len() < EVENTS_PER_POLL
            && self.emitted < EVENTS_TOTAL
        {
            if self.pending.is_empty() {
                let mut bytes = [0; 4096];
                let wanted = bytes.len().min(BYTES_PER_POLL - read);
                if wanted == 0 {
                    break;
                }
                match self.file.read(&mut bytes[..wanted]) {
                    Ok(0) => break,
                    Ok(count) => {
                        read += count;
                        self.pending.extend(&bytes[..count]);
                    }
                    Err(_) => {
                        self.unavailable = true;
                        break;
                    }
                }
            }
            let Some(byte) = self.pending.pop_front() else {
                break;
            };
            consumed += 1;
            if byte == b'\n' {
                if !self.oversized {
                    let line = self.line.strip_suffix(b"\r").unwrap_or(&self.line);
                    if let Ok(line) = std::str::from_utf8(line)
                        && let Some(event) =
                            crate::report::provisional_compile_event(&self.stage, line)
                    {
                        batch.push(event);
                        self.emitted += 1;
                    }
                }
                self.line.clear();
                self.oversized = false;
            } else if !self.oversized {
                if self.line.len() == LINE_BYTES {
                    self.line.clear();
                    self.oversized = true;
                } else {
                    self.line.push(byte);
                }
            }
        }
        batch
    }
}

#[cfg(test)]
#[path = "exec_compiler_progress_tests.rs"]
mod tests;
