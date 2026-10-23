// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Core X11 protocol requests, replies, and connection setup.

use super::{ParseError, Reader, Reply, Request, ValueList, Void, Writer, reply_head};

/// The protocol's id types, here as well, where x11rb's `xproto` module has them.
pub use super::{
    Atom, Colormap, Cursor, Drawable, Gcontext, Keycode, Pixmap, Timestamp, Visualid, Window,
};

/// CreateWindow / ChangeWindowAttributes value-list bits.
pub mod cw {
    pub const BACK_PIXMAP: u32 = 1 << 0;
    pub const BACK_PIXEL: u32 = 1 << 1;
    pub const BORDER_PIXMAP: u32 = 1 << 2;
    pub const BORDER_PIXEL: u32 = 1 << 3;
    pub const BIT_GRAVITY: u32 = 1 << 4;
    pub const WIN_GRAVITY: u32 = 1 << 5;
    pub const BACKING_STORE: u32 = 1 << 6;
    pub const BACKING_PLANES: u32 = 1 << 7;
    pub const BACKING_PIXEL: u32 = 1 << 8;
    pub const OVERRIDE_REDIRECT: u32 = 1 << 9;
    pub const SAVE_UNDER: u32 = 1 << 10;
    pub const EVENT_MASK: u32 = 1 << 11;
    pub const DONT_PROPAGATE: u32 = 1 << 12;
    pub const COLORMAP: u32 = 1 << 13;
    pub const CURSOR: u32 = 1 << 14;
}

/// ConfigureWindow value-list bits (a 16-bit mask on the wire).
pub mod config {
    pub const X: u32 = 1 << 0;
    pub const Y: u32 = 1 << 1;
    pub const WIDTH: u32 = 1 << 2;
    pub const HEIGHT: u32 = 1 << 3;
    pub const BORDER_WIDTH: u32 = 1 << 4;
    pub const SIBLING: u32 = 1 << 5;
    pub const STACK_MODE: u32 = 1 << 6;
}

/// CreateGC value-list bits.
pub mod gc {
    pub const FUNCTION: u32 = 1 << 0;
    pub const PLANE_MASK: u32 = 1 << 1;
    pub const FOREGROUND: u32 = 1 << 2;
    pub const BACKGROUND: u32 = 1 << 3;
    pub const LINE_WIDTH: u32 = 1 << 4;
    pub const SUBWINDOW_MODE: u32 = 1 << 15;
    pub const GRAPHICS_EXPOSURES: u32 = 1 << 16;
}

/// Event-mask bits.
pub mod event_mask {
    pub const NO_EVENT: u32 = 0;
    pub const KEY_PRESS: u32 = 1 << 0;
    pub const KEY_RELEASE: u32 = 1 << 1;
    pub const BUTTON_PRESS: u32 = 1 << 2;
    pub const BUTTON_RELEASE: u32 = 1 << 3;
    pub const ENTER_WINDOW: u32 = 1 << 4;
    pub const LEAVE_WINDOW: u32 = 1 << 5;
    pub const POINTER_MOTION: u32 = 1 << 6;
    pub const POINTER_MOTION_HINT: u32 = 1 << 7;
    pub const BUTTON1_MOTION: u32 = 1 << 8;
    pub const BUTTON2_MOTION: u32 = 1 << 9;
    pub const BUTTON3_MOTION: u32 = 1 << 10;
    pub const BUTTON4_MOTION: u32 = 1 << 11;
    pub const BUTTON5_MOTION: u32 = 1 << 12;
    pub const BUTTON_MOTION: u32 = 1 << 13;
    pub const KEYMAP_STATE: u32 = 1 << 14;
    pub const EXPOSURE: u32 = 1 << 15;
    pub const VISIBILITY_CHANGE: u32 = 1 << 16;
    pub const STRUCTURE_NOTIFY: u32 = 1 << 17;
    pub const RESIZE_REDIRECT: u32 = 1 << 18;
    pub const SUBSTRUCTURE_NOTIFY: u32 = 1 << 19;
    pub const SUBSTRUCTURE_REDIRECT: u32 = 1 << 20;
    pub const FOCUS_CHANGE: u32 = 1 << 21;
    pub const PROPERTY_CHANGE: u32 = 1 << 22;
    pub const COLOR_MAP_CHANGE: u32 = 1 << 23;
    pub const OWNER_GRAB_BUTTON: u32 = 1 << 24;
}

/// Window classes.
pub mod window_class {
    pub const COPY_FROM_PARENT: u16 = 0;
    pub const INPUT_OUTPUT: u16 = 1;
    pub const INPUT_ONLY: u16 = 2;
}

/// ChangeProperty modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PropMode {
    Replace = 0,
    Prepend = 1,
    Append = 2,
}

/// PutImage / GetImage formats.
pub mod image_format {
    pub const XY_BITMAP: u8 = 0;
    pub const XY_PIXMAP: u8 = 1;
    pub const Z_PIXMAP: u8 = 2;
}

/// The setup's image byte orders.
pub mod image_order {
    pub const LSB_FIRST: u8 = 0;
    pub const MSB_FIRST: u8 = 1;
}

/// Visual classes.
pub mod visual_class {
    pub const STATIC_GRAY: u8 = 0;
    pub const GRAY_SCALE: u8 = 1;
    pub const STATIC_COLOR: u8 = 2;
    pub const PSEUDO_COLOR: u8 = 3;
    pub const TRUE_COLOR: u8 = 4;
    pub const DIRECT_COLOR: u8 = 5;
}

/// SetInputFocus revert-to values.
pub mod input_focus {
    pub const NONE: u8 = 0;
    pub const POINTER_ROOT: u8 = 1;
    pub const PARENT: u8 = 2;
}

/// CreateColormap's `alloc`.
pub mod colormap_alloc {
    pub const NONE: u8 = 0;
    pub const ALL: u8 = 1;
}

/// GrabPointer's reply statuses.
pub mod grab_status {
    pub const SUCCESS: u8 = 0;
    pub const ALREADY_GRABBED: u8 = 1;
    pub const INVALID_TIME: u8 = 2;
    pub const NOT_VIEWABLE: u8 = 3;
    pub const FROZEN: u8 = 4;
}

/// GrabPointer modes.
pub mod grab_mode {
    pub const SYNC: u8 = 0;
    pub const ASYNC: u8 = 1;
}

/// CreateWindow (1).
#[allow(
    clippy::too_many_arguments,
    reason = "the request's own fields, in wire order"
)]
pub fn create_window(
    depth: u8,
    wid: Window,
    parent: Window,
    x: i16,
    y: i16,
    width: u16,
    height: u16,
    border_width: u16,
    class: u16,
    visual: Visualid,
    values: &ValueList,
) -> Request<Void> {
    let mut w = Writer::new(1, depth);
    w.u32(wid)
        .u32(parent)
        .i16(x)
        .i16(y)
        .u16(width)
        .u16(height)
        .u16(border_width)
        .u16(class)
        .u32(visual)
        .u32(values.mask());
    values.write(&mut w);
    w.finish()
}

/// ChangeWindowAttributes (2).
pub fn change_window_attributes(window: Window, values: &ValueList) -> Request<Void> {
    let mut w = Writer::new(2, 0);
    w.u32(window).u32(values.mask());
    values.write(&mut w);
    w.finish()
}

/// GetWindowAttributes (3).
pub fn get_window_attributes(window: Window) -> Request<GetWindowAttributesReply> {
    Writer::new(3, 0).u32(window).finish()
}

/// DestroyWindow (4).
pub fn destroy_window(window: Window) -> Request<Void> {
    Writer::new(4, 0).u32(window).finish()
}

/// ReparentWindow (7).
pub fn reparent_window(window: Window, parent: Window, x: i16, y: i16) -> Request<Void> {
    Writer::new(7, 0)
        .u32(window)
        .u32(parent)
        .i16(x)
        .i16(y)
        .finish()
}

/// MapWindow (8).
pub fn map_window(window: Window) -> Request<Void> {
    Writer::new(8, 0).u32(window).finish()
}

/// UnmapWindow (10).
pub fn unmap_window(window: Window) -> Request<Void> {
    Writer::new(10, 0).u32(window).finish()
}

/// ConfigureWindow (12).
pub fn configure_window(window: Window, values: &ValueList) -> Request<Void> {
    let mut w = Writer::new(12, 0);
    w.u32(window)
        .u16(u16::try_from(values.mask() & 0xffff).unwrap_or(0))
        .pad(2);
    values.write(&mut w);
    w.finish()
}

/// GetGeometry (14).
pub fn get_geometry(drawable: Drawable) -> Request<GetGeometryReply> {
    Writer::new(14, 0).u32(drawable).finish()
}

/// QueryTree (15).
pub fn query_tree(window: Window) -> Request<QueryTreeReply> {
    Writer::new(15, 0).u32(window).finish()
}

/// InternAtom (16).
pub fn intern_atom(only_if_exists: bool, name: &[u8]) -> Request<InternAtomReply> {
    let len = u16::try_from(name.len()).unwrap_or(u16::MAX);
    let name = &name[..usize::from(len)];
    Writer::new(16, u8::from(only_if_exists))
        .u16(len)
        .pad(2)
        .bytes(name)
        .finish()
}

/// GetAtomName (17).
pub fn get_atom_name(atom: Atom) -> Request<GetAtomNameReply> {
    Writer::new(17, 0).u32(atom).finish()
}

/// ChangeProperty (18).
pub fn change_property(
    mode: PropMode,
    window: Window,
    property: Atom,
    type_: Atom,
    format: u8,
    data: &[u8],
) -> Request<Void> {
    let unit = usize::from(format / 8).max(1);
    let items = data.len() / unit;
    let data = &data[..items * unit];
    Writer::new(18, mode as u8)
        .u32(window)
        .u32(property)
        .u32(type_)
        .u8(format)
        .pad(3)
        .u32(u32::try_from(items).unwrap_or(u32::MAX))
        .bytes(data)
        .finish()
}

/// ChangeProperty of 32-bit items, each in this client's byte order.
pub fn change_property32(
    mode: PropMode,
    window: Window,
    property: Atom,
    type_: Atom,
    data: &[u32],
) -> Request<Void> {
    let bytes: Vec<u8> = data.iter().flat_map(|v| v.to_ne_bytes()).collect();
    change_property(mode, window, property, type_, 32, &bytes)
}

/// DeleteProperty (19).
pub fn delete_property(window: Window, property: Atom) -> Request<Void> {
    Writer::new(19, 0).u32(window).u32(property).finish()
}

/// GetProperty (20).
pub fn get_property(
    delete: bool,
    window: Window,
    property: Atom,
    type_: Atom,
    long_offset: u32,
    long_length: u32,
) -> Request<GetPropertyReply> {
    Writer::new(20, u8::from(delete))
        .u32(window)
        .u32(property)
        .u32(type_)
        .u32(long_offset)
        .u32(long_length)
        .finish()
}

/// SetSelectionOwner (22).
pub fn set_selection_owner(owner: Window, selection: Atom, time: Timestamp) -> Request<Void> {
    Writer::new(22, 0)
        .u32(owner)
        .u32(selection)
        .u32(time)
        .finish()
}

/// GetSelectionOwner (23).
pub fn get_selection_owner(selection: Atom) -> Request<GetSelectionOwnerReply> {
    Writer::new(23, 0).u32(selection).finish()
}

/// ConvertSelection (24).
pub fn convert_selection(
    requestor: Window,
    selection: Atom,
    target: Atom,
    property: Atom,
    time: Timestamp,
) -> Request<Void> {
    Writer::new(24, 0)
        .u32(requestor)
        .u32(selection)
        .u32(target)
        .u32(property)
        .u32(time)
        .finish()
}

/// SendEvent (25): `event` is one 32-byte event packet.
pub fn send_event(
    propagate: bool,
    destination: Window,
    event_mask: u32,
    event: &[u8; 32],
) -> Request<Void> {
    Writer::new(25, u8::from(propagate))
        .u32(destination)
        .u32(event_mask)
        .bytes(event)
        .finish()
}

/// GrabPointer (26).
#[allow(
    clippy::too_many_arguments,
    reason = "the request's own fields, in wire order"
)]
pub fn grab_pointer(
    owner_events: bool,
    grab_window: Window,
    event_mask: u16,
    pointer_mode: u8,
    keyboard_mode: u8,
    confine_to: Window,
    cursor: Cursor,
    time: Timestamp,
) -> Request<GrabPointerReply> {
    Writer::new(26, u8::from(owner_events))
        .u32(grab_window)
        .u16(event_mask)
        .u8(pointer_mode)
        .u8(keyboard_mode)
        .u32(confine_to)
        .u32(cursor)
        .u32(time)
        .finish()
}

/// UngrabPointer (27).
pub fn ungrab_pointer(time: Timestamp) -> Request<Void> {
    Writer::new(27, 0).u32(time).finish()
}

/// QueryPointer (38).
pub fn query_pointer(window: Window) -> Request<QueryPointerReply> {
    Writer::new(38, 0).u32(window).finish()
}

/// TranslateCoordinates (40).
pub fn translate_coordinates(
    src_window: Window,
    dst_window: Window,
    src_x: i16,
    src_y: i16,
) -> Request<TranslateCoordinatesReply> {
    Writer::new(40, 0)
        .u32(src_window)
        .u32(dst_window)
        .i16(src_x)
        .i16(src_y)
        .finish()
}

/// WarpPointer (41).
#[allow(
    clippy::too_many_arguments,
    reason = "the request's own fields, in wire order"
)]
pub fn warp_pointer(
    src_window: Window,
    dst_window: Window,
    src_x: i16,
    src_y: i16,
    src_width: u16,
    src_height: u16,
    dst_x: i16,
    dst_y: i16,
) -> Request<Void> {
    Writer::new(41, 0)
        .u32(src_window)
        .u32(dst_window)
        .i16(src_x)
        .i16(src_y)
        .u16(src_width)
        .u16(src_height)
        .i16(dst_x)
        .i16(dst_y)
        .finish()
}

/// SetInputFocus (42).
pub fn set_input_focus(revert_to: u8, focus: Window, time: Timestamp) -> Request<Void> {
    Writer::new(42, revert_to).u32(focus).u32(time).finish()
}

/// GetInputFocus (43).
pub fn get_input_focus() -> Request<GetInputFocusReply> {
    Writer::new(43, 0).finish()
}

/// QueryKeymap (44).
pub fn query_keymap() -> Request<QueryKeymapReply> {
    Writer::new(44, 0).finish()
}

/// CreateGC (55).
pub fn create_gc(cid: Gcontext, drawable: Drawable, values: &ValueList) -> Request<Void> {
    let mut w = Writer::new(55, 0);
    w.u32(cid).u32(drawable).u32(values.mask());
    values.write(&mut w);
    w.finish()
}

/// CreatePixmap (53).
pub fn create_pixmap(
    depth: u8,
    pid: Pixmap,
    drawable: Drawable,
    width: u16,
    height: u16,
) -> Request<Void> {
    Writer::new(53, depth)
        .u32(pid)
        .u32(drawable)
        .u16(width)
        .u16(height)
        .finish()
}

/// FreePixmap (54).
pub fn free_pixmap(pixmap: Pixmap) -> Request<Void> {
    Writer::new(54, 0).u32(pixmap).finish()
}

/// CreateColormap (78).
pub fn create_colormap(
    alloc: u8,
    mid: Colormap,
    window: Window,
    visual: Visualid,
) -> Request<Void> {
    Writer::new(78, alloc)
        .u32(mid)
        .u32(window)
        .u32(visual)
        .finish()
}

/// FreeGC (60).
pub fn free_gc(gc: Gcontext) -> Request<Void> {
    Writer::new(60, 0).u32(gc).finish()
}

/// PutImage (72).
#[allow(
    clippy::too_many_arguments,
    reason = "the request's own fields, in wire order"
)]
pub fn put_image(
    format: u8,
    drawable: Drawable,
    gc: Gcontext,
    width: u16,
    height: u16,
    dst_x: i16,
    dst_y: i16,
    left_pad: u8,
    depth: u8,
    data: &[u8],
) -> Request<Void> {
    Writer::new(72, format)
        .u32(drawable)
        .u32(gc)
        .u16(width)
        .u16(height)
        .i16(dst_x)
        .i16(dst_y)
        .u8(left_pad)
        .u8(depth)
        .pad(2)
        .bytes(data)
        .finish()
}

/// GetImage (73).
pub fn get_image(
    format: u8,
    drawable: Drawable,
    x: i16,
    y: i16,
    width: u16,
    height: u16,
    plane_mask: u32,
) -> Request<GetImageReply> {
    Writer::new(73, format)
        .u32(drawable)
        .i16(x)
        .i16(y)
        .u16(width)
        .u16(height)
        .u32(plane_mask)
        .finish()
}

/// QueryExtension (98).
pub fn query_extension(name: &[u8]) -> Request<QueryExtensionReply> {
    let len = u16::try_from(name.len()).unwrap_or(u16::MAX);
    let name = &name[..usize::from(len)];
    Writer::new(98, 0).u16(len).pad(2).bytes(name).finish()
}

/// GetModifierMapping (119).
pub fn get_modifier_mapping() -> Request<GetModifierMappingReply> {
    Writer::new(119, 0).finish()
}

/// GetWindowAttributes' reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GetWindowAttributesReply {
    pub backing_store: u8,
    pub visual: Visualid,
    pub class: u16,
    pub bit_gravity: u8,
    pub win_gravity: u8,
    pub backing_planes: u32,
    pub backing_pixel: u32,
    pub save_under: bool,
    pub map_is_installed: bool,
    pub map_state: u8,
    pub override_redirect: bool,
    pub colormap: Colormap,
    pub all_event_masks: u32,
    pub your_event_mask: u32,
    pub do_not_propagate_mask: u16,
}

impl Reply for GetWindowAttributesReply {
    fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let (mut r, backing_store, _) = reply_head(bytes)?;
        let visual = r.u32()?;
        let class = r.u16()?;
        let bit_gravity = r.u8()?;
        let win_gravity = r.u8()?;
        let backing_planes = r.u32()?;
        let backing_pixel = r.u32()?;
        let save_under = r.bool()?;
        let map_is_installed = r.bool()?;
        let map_state = r.u8()?;
        let override_redirect = r.bool()?;
        let colormap = r.u32()?;
        let all_event_masks = r.u32()?;
        let your_event_mask = r.u32()?;
        let do_not_propagate_mask = r.u16()?;
        Ok(GetWindowAttributesReply {
            backing_store,
            visual,
            class,
            bit_gravity,
            win_gravity,
            backing_planes,
            backing_pixel,
            save_under,
            map_is_installed,
            map_state,
            override_redirect,
            colormap,
            all_event_masks,
            your_event_mask,
            do_not_propagate_mask,
        })
    }
}

/// GetGeometry's reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GetGeometryReply {
    pub depth: u8,
    pub root: Window,
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
    pub border_width: u16,
}

impl Reply for GetGeometryReply {
    fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let (mut r, depth, _) = reply_head(bytes)?;
        Ok(GetGeometryReply {
            depth,
            root: r.u32()?,
            x: r.i16()?,
            y: r.i16()?,
            width: r.u16()?,
            height: r.u16()?,
            border_width: r.u16()?,
        })
    }
}

/// QueryTree's reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryTreeReply {
    pub root: Window,
    pub parent: Window,
    pub children: Vec<Window>,
}

impl Reply for QueryTreeReply {
    fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let (mut r, _, _) = reply_head(bytes)?;
        let root = r.u32()?;
        let parent = r.u32()?;
        let n = r.u16()?;
        r.skip(14)?;
        let children = r.u32s(usize::from(n))?;
        Ok(QueryTreeReply {
            root,
            parent,
            children,
        })
    }
}

/// InternAtom's reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InternAtomReply {
    pub atom: Atom,
}

impl Reply for InternAtomReply {
    fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let (mut r, _, _) = reply_head(bytes)?;
        Ok(InternAtomReply { atom: r.u32()? })
    }
}

/// GetAtomName's reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GetAtomNameReply {
    pub name: Vec<u8>,
}

impl Reply for GetAtomNameReply {
    fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let (mut r, _, _) = reply_head(bytes)?;
        let n = r.u16()?;
        r.skip(22)?;
        Ok(GetAtomNameReply {
            name: r.take(usize::from(n))?.to_vec(),
        })
    }
}

/// GetProperty's reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GetPropertyReply {
    pub format: u8,
    pub type_: Atom,
    pub bytes_after: u32,
    pub value_len: u32,
    pub value: Vec<u8>,
}

impl GetPropertyReply {
    /// The value as 32-bit items; `None` unless `format` is 32.
    pub fn value32(&self) -> Option<Vec<u32>> {
        (self.format == 32).then(|| {
            self.value
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| u32::from_ne_bytes(*c))
                .collect()
        })
    }
}

impl Reply for GetPropertyReply {
    fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let (mut r, format, _) = reply_head(bytes)?;
        let type_ = r.u32()?;
        let bytes_after = r.u32()?;
        let value_len = r.u32()?;
        r.skip(12)?;
        let len = usize::try_from(value_len)
            .ok()
            .and_then(|n| n.checked_mul(usize::from(format / 8)))
            .ok_or(ParseError::Short)?;
        Ok(GetPropertyReply {
            format,
            type_,
            bytes_after,
            value_len,
            value: r.take(len)?.to_vec(),
        })
    }
}

/// GetSelectionOwner's reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GetSelectionOwnerReply {
    pub owner: Window,
}

impl Reply for GetSelectionOwnerReply {
    fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let (mut r, _, _) = reply_head(bytes)?;
        Ok(GetSelectionOwnerReply { owner: r.u32()? })
    }
}

/// GrabPointer's reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GrabPointerReply {
    pub status: u8,
}

impl Reply for GrabPointerReply {
    fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let (_, status, _) = reply_head(bytes)?;
        Ok(GrabPointerReply { status })
    }
}

/// QueryPointer's reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueryPointerReply {
    pub same_screen: bool,
    pub root: Window,
    pub child: Window,
    pub root_x: i16,
    pub root_y: i16,
    pub win_x: i16,
    pub win_y: i16,
    pub mask: u16,
}

impl Reply for QueryPointerReply {
    fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let (mut r, same_screen, _) = reply_head(bytes)?;
        Ok(QueryPointerReply {
            same_screen: same_screen != 0,
            root: r.u32()?,
            child: r.u32()?,
            root_x: r.i16()?,
            root_y: r.i16()?,
            win_x: r.i16()?,
            win_y: r.i16()?,
            mask: r.u16()?,
        })
    }
}

/// TranslateCoordinates' reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TranslateCoordinatesReply {
    pub same_screen: bool,
    pub child: Window,
    pub dst_x: i16,
    pub dst_y: i16,
}

impl Reply for TranslateCoordinatesReply {
    fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let (mut r, same_screen, _) = reply_head(bytes)?;
        Ok(TranslateCoordinatesReply {
            same_screen: same_screen != 0,
            child: r.u32()?,
            dst_x: r.i16()?,
            dst_y: r.i16()?,
        })
    }
}

/// GetInputFocus' reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GetInputFocusReply {
    pub revert_to: u8,
    pub focus: Window,
}

impl Reply for GetInputFocusReply {
    fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let (mut r, revert_to, _) = reply_head(bytes)?;
        Ok(GetInputFocusReply {
            revert_to,
            focus: r.u32()?,
        })
    }
}

/// QueryKeymap's reply: one bit per keycode, keycode 8 at bit 0 of byte 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueryKeymapReply {
    pub keys: [u8; 32],
}

impl Reply for QueryKeymapReply {
    fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let (mut r, _, _) = reply_head(bytes)?;
        let mut keys = [0u8; 32];
        keys.copy_from_slice(r.take(32)?);
        Ok(QueryKeymapReply { keys })
    }
}

/// GetImage's reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GetImageReply {
    pub depth: u8,
    pub visual: Visualid,
    pub data: Vec<u8>,
}

impl Reply for GetImageReply {
    fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let (mut r, depth, length) = reply_head(bytes)?;
        let visual = r.u32()?;
        r.skip(20)?;
        let len = usize::try_from(length)
            .ok()
            .and_then(|n| n.checked_mul(4))
            .ok_or(ParseError::Short)?;
        Ok(GetImageReply {
            depth,
            visual,
            data: r.take(len)?.to_vec(),
        })
    }
}

/// QueryExtension's reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueryExtensionReply {
    pub present: bool,
    pub major_opcode: u8,
    pub first_event: u8,
    pub first_error: u8,
}

impl Reply for QueryExtensionReply {
    fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let (mut r, _, _) = reply_head(bytes)?;
        Ok(QueryExtensionReply {
            present: r.bool()?,
            major_opcode: r.u8()?,
            first_event: r.u8()?,
            first_error: r.u8()?,
        })
    }
}

/// GetModifierMapping's reply: eight rows of `keycodes_per_modifier` keycodes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GetModifierMappingReply {
    pub keycodes_per_modifier: u8,
    pub keycodes: Vec<Keycode>,
}

impl Reply for GetModifierMappingReply {
    fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let (mut r, per, _) = reply_head(bytes)?;
        r.skip(24)?;
        Ok(GetModifierMappingReply {
            keycodes_per_modifier: per,
            keycodes: r.take(usize::from(per) * 8)?.to_vec(),
        })
    }
}

impl Reply for () {
    fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        reply_head(bytes).map(drop)
    }
}

/// A pixmap format the server supports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    pub depth: u8,
    pub bits_per_pixel: u8,
    pub scanline_pad: u8,
}

/// A visual of a depth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Visualtype {
    pub visual_id: Visualid,
    pub class: u8,
    pub bits_per_rgb_value: u8,
    pub colormap_entries: u16,
    pub red_mask: u32,
    pub green_mask: u32,
    pub blue_mask: u32,
}

/// The visuals of one depth on a screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Depth {
    pub depth: u8,
    pub visuals: Vec<Visualtype>,
}

/// One screen of the display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screen {
    pub root: Window,
    pub default_colormap: Colormap,
    pub white_pixel: u32,
    pub black_pixel: u32,
    pub current_input_masks: u32,
    pub width_in_pixels: u16,
    pub height_in_pixels: u16,
    pub width_in_millimeters: u16,
    pub height_in_millimeters: u16,
    pub min_installed_maps: u16,
    pub max_installed_maps: u16,
    pub root_visual: Visualid,
    pub backing_stores: u8,
    pub save_unders: bool,
    pub root_depth: u8,
    pub allowed_depths: Vec<Depth>,
}

/// The server's answer to the connection setup (`xcb_get_setup`'s bytes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Setup {
    pub status: u8,
    pub protocol_major_version: u16,
    pub protocol_minor_version: u16,
    pub release_number: u32,
    pub resource_id_base: u32,
    pub resource_id_mask: u32,
    pub motion_buffer_size: u32,
    pub maximum_request_length: u16,
    pub image_byte_order: u8,
    pub bitmap_format_bit_order: u8,
    pub bitmap_format_scanline_unit: u8,
    pub bitmap_format_scanline_pad: u8,
    pub min_keycode: Keycode,
    pub max_keycode: Keycode,
    pub vendor: Vec<u8>,
    pub pixmap_formats: Vec<Format>,
    pub roots: Vec<Screen>,
}

impl Setup {
    /// Parse an accepted setup (status 1).
    pub fn parse(bytes: &[u8]) -> Result<Setup, ParseError> {
        let mut r = Reader::new(bytes);
        let status = r.u8()?;
        r.skip(1)?;
        let protocol_major_version = r.u16()?;
        let protocol_minor_version = r.u16()?;
        let length = r.u16()?;
        let total = 8 + usize::from(length) * 4;
        if bytes.len() < total {
            return Err(ParseError::Short);
        }
        if status != 1 {
            return Err(ParseError::Invalid);
        }
        let release_number = r.u32()?;
        let resource_id_base = r.u32()?;
        let resource_id_mask = r.u32()?;
        let motion_buffer_size = r.u32()?;
        let vendor_len = r.u16()?;
        let maximum_request_length = r.u16()?;
        let roots_len = r.u8()?;
        let formats_len = r.u8()?;
        let image_byte_order = r.u8()?;
        let bitmap_format_bit_order = r.u8()?;
        let bitmap_format_scanline_unit = r.u8()?;
        let bitmap_format_scanline_pad = r.u8()?;
        let min_keycode = r.u8()?;
        let max_keycode = r.u8()?;
        r.skip(4)?;
        let vendor = r.take(usize::from(vendor_len))?.to_vec();
        r.skip(super::pad4(usize::from(vendor_len)))?;
        let mut pixmap_formats = Vec::with_capacity(usize::from(formats_len));
        for _ in 0..formats_len {
            let depth = r.u8()?;
            let bits_per_pixel = r.u8()?;
            let scanline_pad = r.u8()?;
            r.skip(5)?;
            pixmap_formats.push(Format {
                depth,
                bits_per_pixel,
                scanline_pad,
            });
        }
        let mut roots = Vec::with_capacity(usize::from(roots_len));
        for _ in 0..roots_len {
            roots.push(parse_screen(&mut r)?);
        }
        Ok(Setup {
            status,
            protocol_major_version,
            protocol_minor_version,
            release_number,
            resource_id_base,
            resource_id_mask,
            motion_buffer_size,
            maximum_request_length,
            image_byte_order,
            bitmap_format_bit_order,
            bitmap_format_scanline_unit,
            bitmap_format_scanline_pad,
            min_keycode,
            max_keycode,
            vendor,
            pixmap_formats,
            roots,
        })
    }

    /// Length in bytes of the setup packet starting at `bytes`.
    pub fn packet_len(bytes: &[u8]) -> Result<usize, ParseError> {
        let mut r = Reader::new(bytes);
        r.skip(6)?;
        Ok(8 + usize::from(r.u16()?) * 4)
    }
}

fn parse_screen(r: &mut Reader<'_>) -> Result<Screen, ParseError> {
    let root = r.u32()?;
    let default_colormap = r.u32()?;
    let white_pixel = r.u32()?;
    let black_pixel = r.u32()?;
    let current_input_masks = r.u32()?;
    let width_in_pixels = r.u16()?;
    let height_in_pixels = r.u16()?;
    let width_in_millimeters = r.u16()?;
    let height_in_millimeters = r.u16()?;
    let min_installed_maps = r.u16()?;
    let max_installed_maps = r.u16()?;
    let root_visual = r.u32()?;
    let backing_stores = r.u8()?;
    let save_unders = r.bool()?;
    let root_depth = r.u8()?;
    let depths_len = r.u8()?;
    let mut allowed_depths = Vec::with_capacity(usize::from(depths_len));
    for _ in 0..depths_len {
        let depth = r.u8()?;
        r.skip(1)?;
        let visuals_len = r.u16()?;
        r.skip(4)?;
        let mut visuals = Vec::with_capacity(usize::from(visuals_len).min(256));
        for _ in 0..visuals_len {
            let visual_id = r.u32()?;
            let class = r.u8()?;
            let bits_per_rgb_value = r.u8()?;
            let colormap_entries = r.u16()?;
            let red_mask = r.u32()?;
            let green_mask = r.u32()?;
            let blue_mask = r.u32()?;
            r.skip(4)?;
            visuals.push(Visualtype {
                visual_id,
                class,
                bits_per_rgb_value,
                colormap_entries,
                red_mask,
                green_mask,
                blue_mask,
            });
        }
        allowed_depths.push(Depth { depth, visuals });
    }
    Ok(Screen {
        root,
        default_colormap,
        white_pixel,
        black_pixel,
        current_input_masks,
        width_in_pixels,
        height_in_pixels,
        width_in_millimeters,
        height_in_millimeters,
        min_installed_maps,
        max_installed_maps,
        root_visual,
        backing_stores,
        save_unders,
        root_depth,
        allowed_depths,
    })
}
