// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! AudioQueue's C boundary, acquired completely before a caller creates a queue.
//!
//! Calls through the table are unsafe: queue and buffer references must be live,
//! callback userdata must outlive synchronous disposal, and callbacks must return
//! normally. The queue owns its buffers; only sample storage, byte size and user
//! data may be written as AudioQueue.h permits. The table and library remain valid
//! for the process lifetime. Acquiring the table opens no audio device.

use std::ffi::c_void;
use std::sync::OnceLock;

use crate::framework::Framework;

static AUDIO: Framework =
    Framework::new(&[c"/System/Library/Frameworks/AudioToolbox.framework/AudioToolbox"]);

/// A queue owned and disposed by its caller.
pub type AudioQueueRef = *mut c_void;

/// `AudioStreamBasicDescription` as CoreAudioTypes lays it out.
#[repr(C)]
pub struct AudioStreamBasicDescription {
    pub m_sample_rate: f64,
    pub m_format_id: u32,
    pub m_format_flags: u32,
    pub m_bytes_per_packet: u32,
    pub m_frames_per_packet: u32,
    pub m_bytes_per_frame: u32,
    pub m_channels_per_frame: u32,
    pub m_bits_per_channel: u32,
    pub m_reserved: u32,
}

/// `AudioQueueBuffer` header; AudioQueue owns the sample storage and pointers.
#[repr(C)]
pub struct AudioQueueBuffer {
    pub m_audio_data_bytes_capacity: u32,
    pub m_audio_data: *mut c_void,
    pub m_audio_data_byte_size: u32,
    pub m_user_data: *mut c_void,
    pub m_packet_description_capacity: u32,
    pub m_packet_descriptions: *mut c_void,
    pub m_packet_description_count: u32,
}

/// A queue's render callback, with caller-owned userdata.
pub type AudioQueueOutputCallback =
    extern "C" fn(*mut c_void, AudioQueueRef, *mut AudioQueueBuffer);
/// AudioQueue's four-byte property identifier.
pub type AudioQueuePropertyID = u32;
/// A property notification callback, with caller-owned userdata.
pub type AudioQueuePropertyListenerProc =
    extern "C" fn(*mut c_void, AudioQueueRef, AudioQueuePropertyID);

aterm_dl::table! {
    /// The complete native queue API; acquire it before allocating callback state.
    #[non_exhaustive]
    pub struct AudioApi {
        new_output = c"AudioQueueNewOutput": unsafe extern "C" fn(
            *const AudioStreamBasicDescription, AudioQueueOutputCallback, *mut c_void,
            *const c_void, *const c_void, u32, *mut AudioQueueRef
        ) -> i32;
        allocate_buffer = c"AudioQueueAllocateBuffer": unsafe extern "C" fn(AudioQueueRef, u32, *mut *mut AudioQueueBuffer) -> i32;
        enqueue_buffer = c"AudioQueueEnqueueBuffer": unsafe extern "C" fn(AudioQueueRef, *mut AudioQueueBuffer, u32, *const c_void) -> i32;
        start = c"AudioQueueStart": unsafe extern "C" fn(AudioQueueRef, *const c_void) -> i32;
        stop = c"AudioQueueStop": unsafe extern "C" fn(AudioQueueRef, u8) -> i32;
        add_listener = c"AudioQueueAddPropertyListener": unsafe extern "C" fn(AudioQueueRef, AudioQueuePropertyID, AudioQueuePropertyListenerProc, *mut c_void) -> i32;
        get_property = c"AudioQueueGetProperty": unsafe extern "C" fn(AudioQueueRef, AudioQueuePropertyID, *mut c_void, *mut u32) -> i32;
        dispose = c"AudioQueueDispose": unsafe extern "C" fn(AudioQueueRef, u8) -> i32;
        remove_listener = c"AudioQueueRemovePropertyListener": unsafe extern "C" fn(AudioQueueRef, AudioQueuePropertyID, AudioQueuePropertyListenerProc, *mut c_void) -> i32;
    }
}

/// Acquire every required entry point without creating a native queue.
///
/// # Panics
/// Panics with the loader diagnostic if the framework or a required symbol is absent.
#[must_use]
pub fn api() -> &'static AudioApi {
    static API: OnceLock<AudioApi> = OnceLock::new();
    API.get_or_init(|| {
        // SAFETY: the rows preserve AudioQueue.h's C ABI. The framework owner
        // keeps its library loaded through callback completion and disposal.
        unsafe { AudioApi::load(AUDIO.library()) }.unwrap_or_else(|why| panic!("{why}"))
    })
}

#[cfg(test)]
#[path = "audio_tests.rs"]
mod tests;
