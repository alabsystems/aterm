// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Profile-bound DATA encoding over the shared actual native pipe reader.
pub(super) use super::super::bootstrap_capture::{Capture, CaptureObservation};
use super::wire;
use std::io;

pub(super) fn append_frames(
    capture: &Capture,
    stream: wire::Stream,
    request: &wire::Request,
    digest: [u8; 32],
    frames: &mut Vec<Vec<u8>>,
) -> io::Result<()> {
    capture.append_frames(
        frames,
        |offset, bytes| {
            wire::Event::Output {
                stream,
                offset,
                bytes,
            }
            .encode(request, digest)
        },
        |bytes| wire::Event::Eof { stream, bytes }.encode(request, digest),
    )
}

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;
