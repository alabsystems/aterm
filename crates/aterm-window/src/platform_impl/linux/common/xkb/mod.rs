// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/common/xkb/mod.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// Modified by the aterm project in 2026; see the repository NOTICE. (libxkbcommon's entry
// points, types and keysyms are first-party aterm-xkb's, read at run time, where upstream
// used xkbcommon-dl and xkeysym: docs/AUDIT-third-party-2026-10-15.md, LW.7.)

use std::ops::Deref;
use std::os::raw::c_char;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::keyboard::SmolStr;
#[cfg(x11_platform)]
use aterm_xkb::XcbConnection as xcb_connection_t;
use aterm_xkb::{self as xkb, compose_status, Xkb as XkbCommon, XkbCompose as XkbCommonCompose};
use tracing::warn;

use crate::event::{ElementState, KeyEvent};
use crate::keyboard::{Key, KeyLocation};
use crate::platform_impl::KeyEventExtra;

mod compose;
mod keymap;
mod state;

use compose::{ComposeStatus, XkbComposeState, XkbComposeTable};
use keymap::XkbKeymap;

#[cfg(x11_platform)]
pub use keymap::raw_keycode_to_physicalkey;
pub use state::XkbState;

// TODO: Wire this up without using a static `AtomicBool`.
static RESET_DEAD_KEYS: AtomicBool = AtomicBool::new(false);

/// libxkbcommon's table. aterm-xkb loads it once per process; a missing library panics, as
/// upstream's `xkbcommon_handle` did, and `Context::new` checks first.
fn xkbh() -> &'static XkbCommon {
    xkb::xkb().expect("Library libxkbcommon.so could not be loaded.")
}

/// libxkbcommon's compose table.
fn xkbch() -> &'static XkbCommonCompose {
    xkb::compose().expect("Library libxkbcommon.so could not be loaded.")
}

/// libxkbcommon-x11's table.
#[cfg(feature = "x11")]
fn xkbxh() -> &'static xkb::XkbX11 {
    xkb::x11().expect("Library libxkbcommon-x11.so could not be loaded.")
}

#[inline(always)]
pub fn reset_dead_keys() {
    RESET_DEAD_KEYS.store(true, Ordering::SeqCst);
}

#[derive(Debug)]
pub enum Error {
    /// libxkbcommon is not available
    XKBNotFound,
}

#[derive(Debug)]
pub struct Context {
    // NOTE: field order matters.
    #[cfg(x11_platform)]
    pub core_keyboard_id: i32,
    state: Option<XkbState>,
    keymap: Option<XkbKeymap>,
    compose_state1: Option<XkbComposeState>,
    compose_state2: Option<XkbComposeState>,
    _compose_table: Option<XkbComposeTable>,
    context: XkbContext,
    scratch_buffer: Vec<u8>,
}

impl Context {
    pub fn new() -> Result<Self, Error> {
        if xkb::xkb().is_err() || xkb::compose().is_err() {
            return Err(Error::XKBNotFound);
        }

        let context = XkbContext::new()?;
        let mut compose_table = XkbComposeTable::new(&context);
        let mut compose_state1 = compose_table.as_ref().and_then(|table| table.new_state());
        let mut compose_state2 = compose_table.as_ref().and_then(|table| table.new_state());

        // Disable compose if anything compose related failed to initialize.
        if compose_table.is_none() || compose_state1.is_none() || compose_state2.is_none() {
            compose_state2 = None;
            compose_state1 = None;
            compose_table = None;
        }

        Ok(Self {
            state: None,
            keymap: None,
            compose_state1,
            compose_state2,
            #[cfg(x11_platform)]
            core_keyboard_id: 0,
            _compose_table: compose_table,
            context,
            scratch_buffer: Vec::with_capacity(8),
        })
    }

    #[cfg(feature = "x11")]
    pub fn from_x11_xkb(xcb: *mut xcb_connection_t) -> Result<Self, Error> {
        let result = unsafe {
            (xkbxh().setup_xkb_extension)(
                xcb,
                1,
                2,
                xkb::X11_SETUP_NO_FLAGS,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };

        if result != 1 {
            return Err(Error::XKBNotFound);
        }

        let mut this = Self::new()?;
        this.core_keyboard_id = unsafe { (xkbxh().get_core_keyboard_device_id)(xcb) };
        this.set_keymap_from_x11(xcb);
        Ok(this)
    }

    pub fn state_mut(&mut self) -> Option<&mut XkbState> {
        self.state.as_mut()
    }

    pub fn keymap_mut(&mut self) -> Option<&mut XkbKeymap> {
        self.keymap.as_mut()
    }

    /// Compile the keymap a compositor sent (its text, as aterm-wayland's seat read it).
    #[cfg(wayland_platform)]
    pub fn set_keymap_from_text(&mut self, text: &[u8]) {
        let keymap = XkbKeymap::from_text(&self.context, text);
        let state = keymap.as_ref().and_then(XkbState::new_wayland);
        if keymap.is_none() || state.is_none() {
            warn!("failed to update xkb keymap");
        }
        self.state = state;
        self.keymap = keymap;
    }

    #[cfg(x11_platform)]
    pub fn set_keymap_from_x11(&mut self, xcb: *mut xcb_connection_t) {
        let keymap = XkbKeymap::from_x11_keymap(&self.context, xcb, self.core_keyboard_id);
        let state = keymap.as_ref().and_then(|keymap| XkbState::new_x11(xcb, keymap));
        if keymap.is_none() || state.is_none() {
            warn!("failed to update xkb keymap");
        }
        self.state = state;
        self.keymap = keymap;
    }

    /// Key builder context with the user provided xkb state.
    pub fn key_context(&mut self) -> Option<KeyContext<'_>> {
        let state = self.state.as_mut()?;
        let keymap = self.keymap.as_mut()?;
        let compose_state1 = self.compose_state1.as_mut();
        let compose_state2 = self.compose_state2.as_mut();
        let scratch_buffer = &mut self.scratch_buffer;
        Some(KeyContext { state, keymap, compose_state1, compose_state2, scratch_buffer })
    }

    /// Key builder context with the user provided xkb state.
    ///
    /// Should be used when the original context must not be altered.
    #[cfg(x11_platform)]
    pub fn key_context_with_state<'a>(
        &'a mut self,
        state: &'a mut XkbState,
    ) -> Option<KeyContext<'a>> {
        let keymap = self.keymap.as_mut()?;
        let compose_state1 = self.compose_state1.as_mut();
        let compose_state2 = self.compose_state2.as_mut();
        let scratch_buffer = &mut self.scratch_buffer;
        Some(KeyContext { state, keymap, compose_state1, compose_state2, scratch_buffer })
    }
}

pub struct KeyContext<'a> {
    pub state: &'a mut XkbState,
    pub keymap: &'a mut XkbKeymap,
    compose_state1: Option<&'a mut XkbComposeState>,
    compose_state2: Option<&'a mut XkbComposeState>,
    scratch_buffer: &'a mut Vec<u8>,
}

impl KeyContext<'_> {
    pub fn process_key_event(
        &mut self,
        keycode: u32,
        state: ElementState,
        repeat: bool,
    ) -> KeyEvent {
        let mut event =
            KeyEventResults::new(self, keycode, !repeat && state == ElementState::Pressed);
        let physical_key = keymap::raw_keycode_to_physicalkey(keycode);
        let (logical_key, location) = event.key();
        let text = event.text();
        let (key_without_modifiers, _) = event.key_without_modifiers();
        let text_with_all_modifiers = event.text_with_all_modifiers();

        let platform_specific = KeyEventExtra { text_with_all_modifiers, key_without_modifiers };

        KeyEvent { physical_key, logical_key, text, location, state, repeat, platform_specific }
    }

    fn keysym_to_utf8_raw(&mut self, keysym: u32) -> Option<SmolStr> {
        self.scratch_buffer.clear();
        self.scratch_buffer.reserve(8);
        loop {
            let bytes_written = unsafe {
                (xkbh().keysym_to_utf8)(
                    keysym,
                    self.scratch_buffer.as_mut_ptr().cast(),
                    self.scratch_buffer.capacity(),
                )
            };
            if bytes_written == 0 {
                return None;
            } else if bytes_written == -1 {
                self.scratch_buffer.reserve(8);
            } else {
                unsafe { self.scratch_buffer.set_len(bytes_written.try_into().unwrap()) };
                break;
            }
        }

        // Remove the null-terminator
        self.scratch_buffer.pop();
        byte_slice_to_smol_str(self.scratch_buffer)
    }
}

struct KeyEventResults<'a, 'b> {
    context: &'a mut KeyContext<'b>,
    keycode: u32,
    keysym: u32,
    compose: ComposeStatus,
}

impl<'a, 'b> KeyEventResults<'a, 'b> {
    fn new(context: &'a mut KeyContext<'b>, keycode: u32, compose: bool) -> Self {
        let keysym = context.state.get_one_sym_raw(keycode);

        let compose = if let Some(state) = context.compose_state1.as_mut().filter(|_| compose) {
            if RESET_DEAD_KEYS.swap(false, Ordering::SeqCst) {
                state.reset();
                context.compose_state2.as_mut().unwrap().reset();
            }
            state.feed(keysym)
        } else {
            ComposeStatus::None
        };

        KeyEventResults { context, keycode, keysym, compose }
    }

    pub fn key(&mut self) -> (Key, KeyLocation) {
        let (key, location) = match self.keysym_to_key(self.keysym) {
            Ok(known) => return known,
            Err(undefined) => undefined,
        };

        if let ComposeStatus::Accepted(compose_status::COMPOSING) = self.compose {
            let compose_state = self.context.compose_state2.as_mut().unwrap();
            // When pressing a dead key twice, the non-combining variant of that character will
            // be produced. Since this function only concerns itself with a single keypress, we
            // simulate this double press here by feeding the keysym to the compose state
            // twice.

            compose_state.feed(self.keysym);
            if matches!(compose_state.feed(self.keysym), ComposeStatus::Accepted(_)) {
                // Extracting only a single `char` here *should* be fine, assuming that no
                // dead key's non-combining variant ever occupies more than one `char`.
                let text = compose_state.get_string(self.context.scratch_buffer);
                let key = Key::Dead(text.and_then(|s| s.chars().next()));
                (key, location)
            } else {
                (key, location)
            }
        } else {
            let key = self
                .composed_text()
                .unwrap_or_else(|_| self.context.keysym_to_utf8_raw(self.keysym))
                .map(Key::Character)
                .unwrap_or(key);
            (key, location)
        }
    }

    pub fn key_without_modifiers(&mut self) -> (Key, KeyLocation) {
        // This will become a pointer to an array which libxkbcommon owns, so we don't need to
        // deallocate it.
        let layout = self.context.state.layout(self.keycode);
        let keysym = self.context.keymap.first_keysym_by_level(layout, self.keycode);

        match self.keysym_to_key(keysym) {
            Ok((key, location)) => (key, location),
            Err((key, location)) => {
                let key =
                    self.context.keysym_to_utf8_raw(keysym).map(Key::Character).unwrap_or(key);
                (key, location)
            },
        }
    }

    fn keysym_to_key(&self, keysym: u32) -> Result<(Key, KeyLocation), (Key, KeyLocation)> {
        let location = keymap::keysym_location(keysym);
        let key = keymap::keysym_to_key(keysym);
        if matches!(key, Key::Unidentified(_)) {
            Err((key, location))
        } else {
            Ok((key, location))
        }
    }

    pub fn text(&mut self) -> Option<SmolStr> {
        self.composed_text().unwrap_or_else(|_| self.context.keysym_to_utf8_raw(self.keysym))
    }

    // The current behaviour makes it so composing a character overrides attempts to input a
    // control character with the `Ctrl` key. We can potentially add a configuration option
    // if someone specifically wants the opposite behaviour.
    pub fn text_with_all_modifiers(&mut self) -> Option<SmolStr> {
        match self.composed_text() {
            Ok(text) => text,
            Err(_) => self.context.state.get_utf8_raw(self.keycode, self.context.scratch_buffer),
        }
    }

    fn composed_text(&mut self) -> Result<Option<SmolStr>, ()> {
        match self.compose {
            ComposeStatus::Accepted(status) => match status {
                compose_status::COMPOSED => {
                    let state = self.context.compose_state1.as_mut().unwrap();
                    Ok(state.get_string(self.context.scratch_buffer))
                },
                compose_status::COMPOSING | compose_status::CANCELLED => Ok(None),
                // NOTHING, and any status a later libxkbcommon adds.
                _ => Err(()),
            },
            _ => Err(()),
        }
    }
}

#[derive(Debug)]
pub struct XkbContext {
    context: NonNull<xkb::XkbContext>,
}

impl XkbContext {
    pub fn new() -> Result<Self, Error> {
        let context = unsafe { (xkbh().context_new)(xkb::context_flags::NO_FLAGS) };

        let context = match NonNull::new(context) {
            Some(context) => context,
            None => return Err(Error::XKBNotFound),
        };

        Ok(Self { context })
    }
}

impl Drop for XkbContext {
    fn drop(&mut self) {
        unsafe {
            (xkbh().context_unref)(self.context.as_ptr());
        }
    }
}

impl Deref for XkbContext {
    type Target = NonNull<xkb::XkbContext>;

    fn deref(&self) -> &Self::Target {
        &self.context
    }
}

/// Shared logic for constructing a string with `xkb_compose_state_get_utf8` and
/// `xkb_state_key_get_utf8`.
fn make_string_with<F>(scratch_buffer: &mut Vec<u8>, mut f: F) -> Option<SmolStr>
where
    F: FnMut(*mut c_char, usize) -> i32,
{
    let size = f(ptr::null_mut(), 0);
    if size == 0 {
        return None;
    }
    let size = usize::try_from(size).unwrap();
    scratch_buffer.clear();
    // The allocated buffer must include space for the null-terminator.
    scratch_buffer.reserve(size + 1);
    unsafe {
        let written = f(scratch_buffer.as_mut_ptr().cast(), scratch_buffer.capacity());
        if usize::try_from(written).unwrap() != size {
            // This will likely never happen.
            return None;
        }
        scratch_buffer.set_len(size);
    };

    byte_slice_to_smol_str(scratch_buffer)
}

// NOTE: This is track_caller so we can have more informative line numbers when logging
#[track_caller]
fn byte_slice_to_smol_str(bytes: &[u8]) -> Option<SmolStr> {
    std::str::from_utf8(bytes)
        .map(SmolStr::new)
        .map_err(|_e| {
            tracing::warn!("UTF-8 received from libxkbcommon ({:?}) was invalid: {_e}", bytes)
        })
        .ok()
}
