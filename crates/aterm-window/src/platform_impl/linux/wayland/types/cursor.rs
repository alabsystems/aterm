// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/wayland/types/cursor.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use crate::window::CursorIcon;

use sctk::reexports::client::protocol::wl_shm::Format;
use sctk::shm::slot::{Buffer, SlotPool};

use crate::cursor::CursorImage;

/// LOCAL PATCH (aterm), 2026-10-15 (winit-core step 5): `CursorIcon` is the crate's own type,
/// and smithay-client-toolkit's themed pointer takes cursor-icon's, which Linux still links
/// through it. Each of the six names the same CSS keyword in both.
pub(crate) fn sctk_icon(icon: CursorIcon) -> cursor_icon::CursorIcon {
    match icon {
        CursorIcon::Default => cursor_icon::CursorIcon::Default,
        CursorIcon::Pointer => cursor_icon::CursorIcon::Pointer,
        CursorIcon::Text => cursor_icon::CursorIcon::Text,
        CursorIcon::Grabbing => cursor_icon::CursorIcon::Grabbing,
        CursorIcon::ColResize => cursor_icon::CursorIcon::ColResize,
        CursorIcon::RowResize => cursor_icon::CursorIcon::RowResize,
    }
}

#[derive(Debug)]
pub enum SelectedCursor {
    Named(CursorIcon),
    Custom(CustomCursor),
}

impl Default for SelectedCursor {
    fn default() -> Self {
        Self::Named(Default::default())
    }
}

#[derive(Debug)]
pub struct CustomCursor {
    pub buffer: Buffer,
    pub w: i32,
    pub h: i32,
    pub hotspot_x: i32,
    pub hotspot_y: i32,
}

impl CustomCursor {
    pub(crate) fn new(pool: &mut SlotPool, image: &CursorImage) -> Self {
        let (buffer, canvas) = pool
            .create_buffer(
                image.width as i32,
                image.height as i32,
                4 * (image.width as i32),
                Format::Argb8888,
            )
            .unwrap();

        for (canvas_chunk, rgba) in canvas.chunks_exact_mut(4).zip(image.rgba.chunks_exact(4)) {
            // Alpha in buffer is premultiplied.
            let alpha = rgba[3] as f32 / 255.;
            let r = (rgba[0] as f32 * alpha) as u32;
            let g = (rgba[1] as f32 * alpha) as u32;
            let b = (rgba[2] as f32 * alpha) as u32;
            let color = ((rgba[3] as u32) << 24) + (r << 16) + (g << 8) + b;
            let array: &mut [u8; 4] = canvas_chunk.try_into().unwrap();
            *array = color.to_le_bytes();
        }

        CustomCursor {
            buffer,
            w: image.width as i32,
            h: image.height as i32,
            hotspot_x: image.hotspot_x as i32,
            hotspot_y: image.hotspot_y as i32,
        }
    }
}
