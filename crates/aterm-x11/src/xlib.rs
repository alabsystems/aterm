// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Runtime-loaded Xlib, XKB, XIM, XInput 2, and Xcursor declarations.

#![allow(
    non_camel_case_types,
    non_snake_case,
    reason = "the C names, so a declaration reads against its header"
)]

use std::ffi::{CStr, c_char, c_int, c_long, c_short, c_uchar, c_uint, c_ulong, c_ushort, c_void};
use std::sync::OnceLock;

/// `Display`, opaque.
pub type Display = c_void;
/// `XID`.
pub type XID = c_ulong;
/// `Window`.
pub type Window = XID;
/// `Pixmap`.
pub type Pixmap = XID;
/// `Cursor`.
pub type Cursor = XID;
/// `Drawable`.
pub type Drawable = XID;
/// `Atom`.
pub type Atom = c_ulong;
/// `Time`.
pub type Time = c_ulong;
/// `KeySym`.
pub type KeySym = c_ulong;
/// `KeyCode`.
pub type KeyCode = c_uchar;
/// `Bool`.
pub type Bool = c_int;
/// `Status`.
pub type Status = c_int;
/// `XPointer`.
pub type XPointer = *mut c_char;
/// `XIM`, opaque pointer.
pub type XIM = *mut c_void;
/// `XIC`, opaque pointer.
pub type XIC = *mut c_void;
/// `XIMStyle`.
pub type XIMStyle = c_ulong;
/// `XIMFeedback`.
pub type XIMFeedback = c_ulong;
/// `XIMProc`.
pub type XIMProc = Option<unsafe extern "C" fn(XIM, XPointer, XPointer)>;
/// `XIDProc` (XRegisterIMInstantiateCallback's callback).
pub type XIDProc = Option<unsafe extern "C" fn(*mut Display, XPointer, XPointer)>;
/// `XErrorHandler`.
pub type XErrorHandler = Option<unsafe extern "C" fn(*mut Display, *mut XErrorEvent) -> c_int>;
/// `XIOErrorHandler`.
pub type XIOErrorHandler = Option<unsafe extern "C" fn(*mut Display) -> c_int>;
/// `XIOErrorExitHandler` (libX11 1.7).
pub type XIOErrorExitHandler = Option<unsafe extern "C" fn(*mut Display, *mut c_void)>;
/// `XcursorPixel`.
pub type XcursorPixel = u32;
/// `XcursorDim`.
pub type XcursorDim = u32;

/// `True` and `False`.
pub const TRUE: Bool = 1;
pub const FALSE: Bool = 0;

/// Event types (`X11/X.h`).
pub mod event_type {
    use std::ffi::c_int;
    pub const KEY_PRESS: c_int = 2;
    pub const KEY_RELEASE: c_int = 3;
    pub const EXPOSE: c_int = 12;
    pub const VISIBILITY_NOTIFY: c_int = 15;
    pub const DESTROY_NOTIFY: c_int = 17;
    pub const UNMAP_NOTIFY: c_int = 18;
    pub const MAP_NOTIFY: c_int = 19;
    pub const REPARENT_NOTIFY: c_int = 21;
    pub const CONFIGURE_NOTIFY: c_int = 22;
    pub const PROPERTY_NOTIFY: c_int = 28;
    pub const SELECTION_NOTIFY: c_int = 31;
    pub const CLIENT_MESSAGE: c_int = 33;
    pub const GENERIC_EVENT: c_int = 35;
}

/// `VisibilityFullyObscured`.
pub const VISIBILITY_FULLY_OBSCURED: c_int = 2;
/// `XBufferOverflow` (an Xutf8LookupString status).
pub const X_BUFFER_OVERFLOW: c_int = -1;
/// `XLookupChars`, `XLookupKeySym`, `XLookupBoth`.
pub const X_LOOKUP_CHARS: c_int = 2;
pub const X_LOOKUP_KEYSYM: c_int = 3;
pub const X_LOOKUP_BOTH: c_int = 4;
/// `XkbUseCoreKbd`.
pub const XKB_USE_CORE_KBD: c_uint = 0x0100;

/// XKB event types (`X11/XKB.h`), the `xkb_type` of an XkbAnyEvent.
pub mod xkb_event {
    use std::ffi::c_int;
    pub const NEW_KEYBOARD_NOTIFY: c_int = 0;
    pub const MAP_NOTIFY: c_int = 1;
    pub const STATE_NOTIFY: c_int = 2;
    pub const BELL_NOTIFY: c_int = 8;
}

/// XIM styles (`X11/Xlib.h`).
pub mod xim_style {
    use super::XIMStyle;
    pub const PREEDIT_AREA: XIMStyle = 0x0001;
    pub const PREEDIT_CALLBACKS: XIMStyle = 0x0002;
    pub const PREEDIT_POSITION: XIMStyle = 0x0004;
    pub const PREEDIT_NOTHING: XIMStyle = 0x0008;
    pub const PREEDIT_NONE: XIMStyle = 0x0010;
    pub const STATUS_AREA: XIMStyle = 0x0100;
    pub const STATUS_CALLBACKS: XIMStyle = 0x0200;
    pub const STATUS_NOTHING: XIMStyle = 0x0400;
    pub const STATUS_NONE: XIMStyle = 0x0800;
}

/// XIM value names (`XN*`), the strings variadic IC and IM calls take.
pub mod xn {
    use std::ffi::CStr;
    pub const CLIENT_WINDOW: &CStr = c"clientWindow";
    pub const DESTROY_CALLBACK: &CStr = c"destroyCallback";
    pub const INPUT_STYLE: &CStr = c"inputStyle";
    pub const PREEDIT_ATTRIBUTES: &CStr = c"preeditAttributes";
    pub const PREEDIT_CARET_CALLBACK: &CStr = c"preeditCaretCallback";
    pub const PREEDIT_DONE_CALLBACK: &CStr = c"preeditDoneCallback";
    pub const PREEDIT_DRAW_CALLBACK: &CStr = c"preeditDrawCallback";
    pub const PREEDIT_START_CALLBACK: &CStr = c"preeditStartCallback";
    pub const QUERY_INPUT_STYLE: &CStr = c"queryInputStyle";
    pub const SPOT_LOCATION: &CStr = c"spotLocation";
}

/// `XIMCaretDirection`'s values.
pub mod xim_caret {
    use std::ffi::c_int;
    pub const FORWARD_CHAR: c_int = 0;
    pub const BACKWARD_CHAR: c_int = 1;
    pub const ABSOLUTE_POSITION: c_int = 10;
}

/// XInput 2 (`X11/extensions/XI2.h`).
pub mod xi {
    use std::ffi::c_int;
    pub const ALL_DEVICES: c_int = 0;
    pub const ALL_MASTER_DEVICES: c_int = 1;
    pub const MASTER_POINTER: c_int = 1;
    pub const MASTER_KEYBOARD: c_int = 2;
    pub const SLAVE_POINTER: c_int = 3;
    pub const SLAVE_KEYBOARD: c_int = 4;
    pub const FLOATING_SLAVE: c_int = 5;
    pub const KEY_CLASS: c_int = 0;
    pub const BUTTON_CLASS: c_int = 1;
    pub const VALUATOR_CLASS: c_int = 2;
    pub const SCROLL_CLASS: c_int = 3;
    pub const TOUCH_CLASS: c_int = 8;
    pub const SCROLL_TYPE_VERTICAL: c_int = 1;
    pub const SCROLL_TYPE_HORIZONTAL: c_int = 2;
    pub const DEVICE_CHANGED: c_int = 1;
    pub const KEY_PRESS: c_int = 2;
    pub const KEY_RELEASE: c_int = 3;
    pub const BUTTON_PRESS: c_int = 4;
    pub const BUTTON_RELEASE: c_int = 5;
    pub const MOTION: c_int = 6;
    pub const ENTER: c_int = 7;
    pub const LEAVE: c_int = 8;
    pub const FOCUS_IN: c_int = 9;
    pub const FOCUS_OUT: c_int = 10;
    pub const HIERARCHY_CHANGED: c_int = 11;
    pub const RAW_KEY_PRESS: c_int = 13;
    pub const RAW_KEY_RELEASE: c_int = 14;
    pub const RAW_BUTTON_PRESS: c_int = 15;
    pub const RAW_BUTTON_RELEASE: c_int = 16;
    pub const RAW_MOTION: c_int = 17;
    pub const TOUCH_BEGIN: c_int = 18;
    pub const TOUCH_UPDATE: c_int = 19;
    pub const TOUCH_END: c_int = 20;
    pub const MASTER_ADDED: c_int = 1 << 0;
    pub const MASTER_REMOVED: c_int = 1 << 1;
    pub const SLAVE_ADDED: c_int = 1 << 2;
    pub const SLAVE_REMOVED: c_int = 1 << 3;
    pub const SLAVE_ATTACHED: c_int = 1 << 4;
    pub const SLAVE_DETACHED: c_int = 1 << 5;
    pub const DEVICE_ENABLED: c_int = 1 << 6;
    pub const DEVICE_DISABLED: c_int = 1 << 7;
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XAnyEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub window: Window,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XKeyEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub window: Window,
    pub root: Window,
    pub subwindow: Window,
    pub time: Time,
    pub x: c_int,
    pub y: c_int,
    pub x_root: c_int,
    pub y_root: c_int,
    pub state: c_uint,
    pub keycode: c_uint,
    pub same_screen: Bool,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XExposeEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub window: Window,
    pub x: c_int,
    pub y: c_int,
    pub width: c_int,
    pub height: c_int,
    pub count: c_int,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XVisibilityEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub window: Window,
    pub state: c_int,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XDestroyWindowEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub event: Window,
    pub window: Window,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XUnmapEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub event: Window,
    pub window: Window,
    pub from_configure: Bool,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XMapEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub event: Window,
    pub window: Window,
    pub override_redirect: Bool,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XReparentEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub event: Window,
    pub window: Window,
    pub parent: Window,
    pub x: c_int,
    pub y: c_int,
    pub override_redirect: Bool,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XConfigureEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub event: Window,
    pub window: Window,
    pub x: c_int,
    pub y: c_int,
    pub width: c_int,
    pub height: c_int,
    pub border_width: c_int,
    pub above: Window,
    pub override_redirect: Bool,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XPropertyEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub window: Window,
    pub atom: Atom,
    pub time: Time,
    pub state: c_int,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XSelectionEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub requestor: Window,
    pub selection: Atom,
    pub target: Atom,
    pub property: Atom,
    pub time: Time,
}

/// `XClientMessageEvent`'s data: 20 bytes, 10 shorts or 5 longs.
#[repr(C)]
#[derive(Clone, Copy)]
pub union ClientMessageData {
    pub b: [c_char; 20],
    pub s: [c_short; 10],
    pub l: [c_long; 5],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct XClientMessageEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub window: Window,
    pub message_type: Atom,
    pub format: c_int,
    pub data: ClientMessageData,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XErrorEvent {
    pub type_: c_int,
    pub display: *mut Display,
    pub resourceid: XID,
    pub serial: c_ulong,
    pub error_code: c_uchar,
    pub request_code: c_uchar,
    pub minor_code: c_uchar,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XGenericEventCookie {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub extension: c_int,
    pub evtype: c_int,
    pub cookie: c_uint,
    pub data: *mut c_void,
}

/// `XEvent`: every member the backend reads, and the 24 longs that size it.
#[repr(C)]
#[derive(Clone, Copy)]
pub union XEvent {
    pub type_: c_int,
    pub any: XAnyEvent,
    pub key: XKeyEvent,
    pub expose: XExposeEvent,
    pub visibility: XVisibilityEvent,
    pub destroy_window: XDestroyWindowEvent,
    pub unmap: XUnmapEvent,
    pub map: XMapEvent,
    pub reparent: XReparentEvent,
    pub configure: XConfigureEvent,
    pub property: XPropertyEvent,
    pub selection: XSelectionEvent,
    pub client_message: XClientMessageEvent,
    pub error: XErrorEvent,
    pub generic_event_cookie: XGenericEventCookie,
    pub pad: [c_long; 24],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XModifierKeymap {
    pub max_keypermod: c_int,
    pub modifiermap: *mut KeyCode,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct XColor {
    pub pixel: c_ulong,
    pub red: c_ushort,
    pub green: c_ushort,
    pub blue: c_ushort,
    pub flags: c_char,
    pub pad: c_char,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct XPoint {
    pub x: c_short,
    pub y: c_short,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XkbAnyEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub time: Time,
    pub xkb_type: c_int,
    pub device: c_uint,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XkbNewKeyboardNotifyEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub time: Time,
    pub xkb_type: c_int,
    pub device: c_int,
    pub old_device: c_int,
    pub min_key_code: c_int,
    pub max_key_code: c_int,
    pub old_min_key_code: c_int,
    pub old_max_key_code: c_int,
    pub changed: c_uint,
    pub req_major: c_char,
    pub req_minor: c_char,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XkbStateNotifyEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub time: Time,
    pub xkb_type: c_int,
    pub device: c_int,
    pub changed: c_uint,
    pub group: c_int,
    pub base_group: c_int,
    pub latched_group: c_int,
    pub locked_group: c_int,
    pub mods: c_uint,
    pub base_mods: c_uint,
    pub latched_mods: c_uint,
    pub locked_mods: c_uint,
    pub compat_state: c_int,
    pub grab_mods: c_uchar,
    pub compat_grab_mods: c_uchar,
    pub lookup_mods: c_uchar,
    pub compat_lookup_mods: c_uchar,
    pub ptr_buttons: c_int,
    pub keycode: KeyCode,
    pub event_type: c_char,
    pub req_major: c_char,
    pub req_minor: c_char,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XkbBellNotifyEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub time: Time,
    pub xkb_type: c_int,
    pub device: c_int,
    pub percent: c_int,
    pub pitch: c_int,
    pub duration: c_int,
    pub bell_class: c_int,
    pub bell_id: c_int,
    pub name: Atom,
    pub window: Window,
    pub event_only: Bool,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct XkbStateRec {
    pub group: c_uchar,
    pub locked_group: c_uchar,
    pub base_group: c_ushort,
    pub latched_group: c_ushort,
    pub mods: c_uchar,
    pub base_mods: c_uchar,
    pub latched_mods: c_uchar,
    pub locked_mods: c_uchar,
    pub compat_state: c_uchar,
    pub grab_mods: c_uchar,
    pub compat_grab_mods: c_uchar,
    pub lookup_mods: c_uchar,
    pub compat_lookup_mods: c_uchar,
    pub ptr_buttons: c_ushort,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XIMCallback {
    pub client_data: XPointer,
    pub callback: XIMProc,
}

/// `XIMText`'s string: multibyte or wide.
#[repr(C)]
#[derive(Clone, Copy)]
pub union XIMTextString {
    pub multi_byte: *mut c_char,
    pub wide_char: *mut c_int,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct XIMText {
    pub length: c_ushort,
    pub feedback: *mut XIMFeedback,
    pub encoding_is_wchar: Bool,
    pub string: XIMTextString,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XIMPreeditDrawCallbackStruct {
    pub caret: c_int,
    pub chg_first: c_int,
    pub chg_length: c_int,
    pub text: *mut XIMText,
}

/// `XIMPreeditCaretCallbackStruct`: `direction` is an `XIMCaretDirection`,
/// `style` an `XIMCaretStyle` (C enums).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XIMPreeditCaretCallbackStruct {
    pub position: c_int,
    pub direction: c_int,
    pub style: c_int,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XIMStyles {
    pub count_styles: c_ushort,
    pub supported_styles: *mut XIMStyle,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct XIModifierState {
    pub base: c_int,
    pub latched: c_int,
    pub locked: c_int,
    pub effective: c_int,
}

/// `XIGroupState`.
pub type XIGroupState = XIModifierState;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XIButtonState {
    pub mask_len: c_int,
    pub mask: *mut c_uchar,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XIValuatorState {
    pub mask_len: c_int,
    pub mask: *mut c_uchar,
    pub values: *mut f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XIDeviceEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub extension: c_int,
    pub evtype: c_int,
    pub time: Time,
    pub deviceid: c_int,
    pub sourceid: c_int,
    pub detail: c_int,
    pub root: Window,
    pub event: Window,
    pub child: Window,
    pub root_x: f64,
    pub root_y: f64,
    pub event_x: f64,
    pub event_y: f64,
    pub flags: c_int,
    pub buttons: XIButtonState,
    pub valuators: XIValuatorState,
    pub mods: XIModifierState,
    pub group: XIGroupState,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XIRawEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub extension: c_int,
    pub evtype: c_int,
    pub time: Time,
    pub deviceid: c_int,
    pub sourceid: c_int,
    pub detail: c_int,
    pub flags: c_int,
    pub valuators: XIValuatorState,
    pub raw_values: *mut f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XIEnterEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub extension: c_int,
    pub evtype: c_int,
    pub time: Time,
    pub deviceid: c_int,
    pub sourceid: c_int,
    pub detail: c_int,
    pub root: Window,
    pub event: Window,
    pub child: Window,
    pub root_x: f64,
    pub root_y: f64,
    pub event_x: f64,
    pub event_y: f64,
    pub mode: c_int,
    pub focus: Bool,
    pub same_screen: Bool,
    pub buttons: XIButtonState,
    pub mods: XIModifierState,
    pub group: XIGroupState,
}

/// `XILeaveEvent`.
pub type XILeaveEvent = XIEnterEvent;
/// `XIFocusInEvent`.
pub type XIFocusInEvent = XIEnterEvent;
/// `XIFocusOutEvent`.
pub type XIFocusOutEvent = XIEnterEvent;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XIHierarchyInfo {
    pub deviceid: c_int,
    pub attachment: c_int,
    pub use_: c_int,
    pub enabled: Bool,
    pub flags: c_int,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XIHierarchyEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub extension: c_int,
    pub evtype: c_int,
    pub time: Time,
    pub flags: c_int,
    pub num_info: c_int,
    pub info: *mut XIHierarchyInfo,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XIAnyClassInfo {
    pub type_: c_int,
    pub sourceid: c_int,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XIValuatorClassInfo {
    pub type_: c_int,
    pub sourceid: c_int,
    pub number: c_int,
    pub label: Atom,
    pub min: f64,
    pub max: f64,
    pub value: f64,
    pub resolution: c_int,
    pub mode: c_int,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XIScrollClassInfo {
    pub type_: c_int,
    pub sourceid: c_int,
    pub number: c_int,
    pub scroll_type: c_int,
    pub increment: f64,
    pub flags: c_int,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XIDeviceInfo {
    pub deviceid: c_int,
    pub name: *mut c_char,
    pub use_: c_int,
    pub attachment: c_int,
    pub enabled: Bool,
    pub num_classes: c_int,
    pub classes: *mut *mut XIAnyClassInfo,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XcursorImage {
    pub version: XcursorDim,
    pub size: XcursorDim,
    pub width: XcursorDim,
    pub height: XcursorDim,
    pub xhot: XcursorDim,
    pub yhot: XcursorDim,
    pub delay: XcursorPixel,
    pub pixels: *mut XcursorPixel,
}

aterm_dl::table! {
    /// libX11: the display, events, keyboard, XKB's Xlib half and XIM.
    pub struct Xlib {
        XOpenDisplay = c"XOpenDisplay": unsafe extern "C" fn(*const c_char) -> *mut Display;
        XCloseDisplay = c"XCloseDisplay": unsafe extern "C" fn(*mut Display) -> c_int;
        XInitThreads = c"XInitThreads": unsafe extern "C" fn() -> Status;
        XDefaultScreen = c"XDefaultScreen": unsafe extern "C" fn(*mut Display) -> c_int;
        XRootWindow = c"XRootWindow": unsafe extern "C" fn(*mut Display, c_int) -> Window;
        XDefaultRootWindow = c"XDefaultRootWindow": unsafe extern "C" fn(*mut Display) -> Window;
        XPending = c"XPending": unsafe extern "C" fn(*mut Display) -> c_int;
        XCheckIfEvent = c"XCheckIfEvent": unsafe extern "C" fn(
            *mut Display,
            *mut XEvent,
            Option<unsafe extern "C" fn(*mut Display, *mut XEvent, XPointer) -> Bool>,
            XPointer,
        ) -> Bool;
        XFilterEvent = c"XFilterEvent": unsafe extern "C" fn(*mut XEvent, Window) -> Bool;
        XFlush = c"XFlush": unsafe extern "C" fn(*mut Display) -> c_int;
        XSync = c"XSync": unsafe extern "C" fn(*mut Display, Bool) -> c_int;
        XFree = c"XFree": unsafe extern "C" fn(*mut c_void) -> c_int;
        XSetErrorHandler = c"XSetErrorHandler": unsafe extern "C" fn(XErrorHandler) -> XErrorHandler;
        XSetIOErrorHandler = c"XSetIOErrorHandler":
            unsafe extern "C" fn(XIOErrorHandler) -> XIOErrorHandler;
        XGetErrorText = c"XGetErrorText":
            unsafe extern "C" fn(*mut Display, c_int, *mut c_char, c_int) -> c_int;
        XGetEventData = c"XGetEventData":
            unsafe extern "C" fn(*mut Display, *mut XGenericEventCookie) -> Bool;
        XFreeEventData = c"XFreeEventData":
            unsafe extern "C" fn(*mut Display, *mut XGenericEventCookie);
        XQueryKeymap = c"XQueryKeymap": unsafe extern "C" fn(*mut Display, *mut c_char) -> c_int;
        XGetModifierMapping = c"XGetModifierMapping":
            unsafe extern "C" fn(*mut Display) -> *mut XModifierKeymap;
        XFreeModifiermap = c"XFreeModifiermap": unsafe extern "C" fn(*mut XModifierKeymap) -> c_int;
        XGetAtomNames = c"XGetAtomNames":
            unsafe extern "C" fn(*mut Display, *mut Atom, c_int, *mut *mut c_char) -> Status;
        XCreateBitmapFromData = c"XCreateBitmapFromData": unsafe extern "C" fn(
            *mut Display,
            Drawable,
            *const c_char,
            c_uint,
            c_uint,
        ) -> Pixmap;
        XCreatePixmapCursor = c"XCreatePixmapCursor": unsafe extern "C" fn(
            *mut Display,
            Pixmap,
            Pixmap,
            *mut XColor,
            *mut XColor,
            c_uint,
            c_uint,
        ) -> Cursor;
        XFreePixmap = c"XFreePixmap": unsafe extern "C" fn(*mut Display, Pixmap) -> c_int;
        XFreeCursor = c"XFreeCursor": unsafe extern "C" fn(*mut Display, Cursor) -> c_int;
        XkbGetState = c"XkbGetState":
            unsafe extern "C" fn(*mut Display, c_uint, *mut XkbStateRec) -> Status;
        XkbSelectEvents = c"XkbSelectEvents":
            unsafe extern "C" fn(*mut Display, c_uint, c_ulong, c_ulong) -> Bool;
        XkbSetDetectableAutoRepeat = c"XkbSetDetectableAutoRepeat":
            unsafe extern "C" fn(*mut Display, Bool, *mut Bool) -> Bool;
        XkbBell = c"XkbBell": unsafe extern "C" fn(*mut Display, Window, c_int, Atom) -> Bool;
        XSupportsLocale = c"XSupportsLocale": unsafe extern "C" fn() -> Bool;
        XSetLocaleModifiers = c"XSetLocaleModifiers":
            unsafe extern "C" fn(*const c_char) -> *mut c_char;
        XOpenIM = c"XOpenIM": unsafe extern "C" fn(
            *mut Display,
            *mut c_void,
            *mut c_char,
            *mut c_char,
        ) -> XIM;
        XCloseIM = c"XCloseIM": unsafe extern "C" fn(XIM) -> Status;
        XGetIMValues = c"XGetIMValues": unsafe extern "C" fn(XIM, ...) -> *mut c_char;
        XSetIMValues = c"XSetIMValues": unsafe extern "C" fn(XIM, ...) -> *mut c_char;
        XCreateIC = c"XCreateIC": unsafe extern "C" fn(XIM, ...) -> XIC;
        XDestroyIC = c"XDestroyIC": unsafe extern "C" fn(XIC);
        XSetICFocus = c"XSetICFocus": unsafe extern "C" fn(XIC);
        XUnsetICFocus = c"XUnsetICFocus": unsafe extern "C" fn(XIC);
        XSetICValues = c"XSetICValues": unsafe extern "C" fn(XIC, ...) -> *mut c_char;
        XVaCreateNestedList = c"XVaCreateNestedList": unsafe extern "C" fn(c_int, ...) -> *mut c_void;
        XRegisterIMInstantiateCallback = c"XRegisterIMInstantiateCallback": unsafe extern "C" fn(
            *mut Display,
            *mut c_void,
            *mut c_char,
            *mut c_char,
            XIDProc,
            XPointer,
        ) -> Bool;
        XUnregisterIMInstantiateCallback = c"XUnregisterIMInstantiateCallback": unsafe extern "C" fn(
            *mut Display,
            *mut c_void,
            *mut c_char,
            *mut c_char,
            XIDProc,
            XPointer,
        ) -> Bool;
        Xutf8LookupString = c"Xutf8LookupString": unsafe extern "C" fn(
            XIC,
            *mut XKeyEvent,
            *mut c_char,
            c_int,
            *mut KeySym,
            *mut Status,
        ) -> c_int;
    }
    optional {
        XSetIOErrorExitHandler = c"XSetIOErrorExitHandler":
            unsafe extern "C" fn(*mut Display, XIOErrorExitHandler, *mut c_void);
    }
}

aterm_dl::table! {
    /// libXi: XInput 2's device query.
    pub struct Xi {
        XIQueryDevice = c"XIQueryDevice":
            unsafe extern "C" fn(*mut Display, c_int, *mut c_int) -> *mut XIDeviceInfo;
        XIFreeDeviceInfo = c"XIFreeDeviceInfo": unsafe extern "C" fn(*mut XIDeviceInfo);
    }
}

aterm_dl::table! {
    /// libXcursor: themed cursors and cursors from pixels.
    pub struct Xcursor {
        XcursorImageCreate = c"XcursorImageCreate": unsafe extern "C" fn(c_int, c_int) -> *mut XcursorImage;
        XcursorImageDestroy = c"XcursorImageDestroy": unsafe extern "C" fn(*mut XcursorImage);
        XcursorImageLoadCursor = c"XcursorImageLoadCursor":
            unsafe extern "C" fn(*mut Display, *const XcursorImage) -> Cursor;
        XcursorLibraryLoadCursor = c"XcursorLibraryLoadCursor":
            unsafe extern "C" fn(*mut Display, *const c_char) -> Cursor;
        XcursorSetTheme = c"XcursorSetTheme":
            unsafe extern "C" fn(*mut Display, *const c_char) -> Bool;
        XcursorGetTheme = c"XcursorGetTheme": unsafe extern "C" fn(*mut Display) -> *mut c_char;
        XcursorSetDefaultSize = c"XcursorSetDefaultSize":
            unsafe extern "C" fn(*mut Display, c_int) -> Bool;
        XcursorGetDefaultSize = c"XcursorGetDefaultSize":
            unsafe extern "C" fn(*mut Display) -> c_int;
    }
}

static X11_SONAMES: &[&CStr] = &[c"libX11.so.6", c"libX11.so"];
static XI_SONAMES: &[&CStr] = &[c"libXi.so.6", c"libXi.so"];
static XCURSOR_SONAMES: &[&CStr] = &[c"libXcursor.so.1", c"libXcursor.so"];

/// libX11, loaded once per process.
pub fn xlib() -> Result<&'static Xlib, aterm_dl::Error> {
    static T: OnceLock<Result<Xlib, aterm_dl::Error>> = OnceLock::new();
    T.get_or_init(|| {
        let lib = aterm_dl::Library::open(X11_SONAMES)?;
        // SAFETY: every row transcribes Xlib.h, XKBlib.h or Xutil.h.
        unsafe { Xlib::load(&lib) }
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// libXi, loaded once per process.
pub fn xi() -> Result<&'static Xi, aterm_dl::Error> {
    static T: OnceLock<Result<Xi, aterm_dl::Error>> = OnceLock::new();
    T.get_or_init(|| {
        let lib = aterm_dl::Library::open(XI_SONAMES)?;
        // SAFETY: every row transcribes XInput2.h.
        unsafe { Xi::load(&lib) }
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// libXcursor, loaded once per process.
pub fn xcursor() -> Result<&'static Xcursor, aterm_dl::Error> {
    static T: OnceLock<Result<Xcursor, aterm_dl::Error>> = OnceLock::new();
    T.get_or_init(|| {
        let lib = aterm_dl::Library::open(XCURSOR_SONAMES)?;
        // SAFETY: every row transcribes Xcursor.h.
        unsafe { Xcursor::load(&lib) }
    })
    .as_ref()
    .map_err(Clone::clone)
}
