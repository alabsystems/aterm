// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A private reader crosses both native exec holds; its sole writer stays with
//! the actual Host resource owner. No requested descriptor number is authority.
use std::{io, os::fd::OwnedFd};

// The direct linker profile reserves CPU5. Prelude control chooses a different
// target from [CPU5, lifeline6, close-only SDK7] before constructing its plan.
pub(super) const TARGET: u32 = 6;
pub(super) const SDK_TARGET: u32 = 7;
pub(super) const ARGUMENT: &str = "--resource-lifeline-fd=6";

pub(super) struct Lifeline {
    reader: Option<io::PipeReader>,
    // Never cloned, mapped into a child, or included in an exec plan. Resources
    // retain it through actual native retirement, including uncertain custody.
    _writer: Option<io::PipeWriter>,
}
impl Lifeline {
    pub(super) fn new() -> io::Result<Self> {
        let (reader, writer) = crate::resource_telemetry::host_pipe()?;
        Ok(Self {
            reader: Some(reader),
            _writer: Some(writer),
        })
    }
    pub(super) fn extra(&mut self) -> io::Result<(OwnedFd, i32)> {
        let reader = self.reader.take().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "linker lifeline already configured",
            )
        })?;
        Ok((reader.into(), TARGET as i32))
    }
    #[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
    pub(super) fn close(&mut self) {
        self._writer.take();
    }
}
