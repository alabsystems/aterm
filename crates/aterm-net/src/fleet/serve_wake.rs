// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One listener's coalesced Live deadline hint, independent of TCP and stop bells.

use std::io::{self, Read as _, Write as _};
use std::sync::{Arc, Mutex, PoisonError};

pub(super) struct Wake {
    reader: aterm_uds::CtlStream,
    pub(super) signal: Arc<Signal>,
}

pub(super) struct Signal {
    state: Mutex<State>,
}

struct State {
    writer: Option<aterm_uds::CtlStream>,
    rung: bool,
}

impl Wake {
    pub(super) fn new() -> io::Result<Self> {
        let (reader, writer) = aterm_uds::kept_out(aterm_uds::CtlStream::pair)?;
        reader.set_nonblocking(true)?;
        writer.set_nonblocking(true)?;
        Ok(Self {
            reader,
            signal: Arc::new(Signal {
                state: Mutex::new(State {
                    writer: Some(writer),
                    rung: false,
                }),
            }),
        })
    }

    pub(super) fn reader(&self) -> &aterm_uds::CtlStream {
        &self.reader
    }

    /// Clear the hint before reading the state it announces. A later change then either
    /// appears in that snapshot or leaves this stream readable before the native wait.
    pub(super) fn drain(&self) -> io::Result<()> {
        let mut state = self
            .signal
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if state.writer.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "fleet wake closed",
            ));
        }
        let mut reader = &self.reader;
        loop {
            match reader.read(&mut [0; 64]) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "fleet wake ended",
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error),
            }
        }
        state.rung = false;
        Ok(())
    }
}

impl Drop for Wake {
    fn drop(&mut self) {
        // Serialize closing the writer before the reader closes. A late proof or tenure drop
        // may outlive the listener, but cannot write to its closed peer.
        self.signal
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .writer = None;
    }
}

impl Signal {
    pub(super) fn ring(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.rung {
            return;
        }
        let Some(mut writer) = state.writer.as_ref() else {
            return;
        };
        let result = loop {
            match writer.write(&[1]) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => break result,
            }
        };
        match result {
            Ok(1) => state.rung = true,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => state.rung = true,
            // Dropping the failed writer also wakes the listener through EOF.
            _ => state.writer = None,
        }
    }
}
