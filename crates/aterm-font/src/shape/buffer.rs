// Copyright (c) HarfBuzz developers
// Copyright (c) 2020 Yevhenii Reizner
// SPDX-License-Identifier: MIT
//
// Derived from rustybuzz 0.20.1 src/hb/buffer.rs and modified by the aterm
// project in 2026.
// See ../../LICENSE-RUSTYBUZZ for the upstream license.

//! Shaping buffer for input characters and output glyphs.

use super::Direction;
use aterm_grapheme::{GeneralCategory, Script};

/// One glyph of the output (one character of the input before shaping).
#[derive(Clone, Copy, Debug)]
pub struct GlyphInfo {
    /// The glyph id after shaping.
    pub glyph_id: u32,
    /// The UTF-8 byte offset, in the shaped string, of the first character of
    /// the grapheme cluster this glyph belongs to (the `cluster` passed to
    /// [`super::UnicodeBuffer::add`] for a buffer built by hand).
    pub cluster: u32,
    pub(crate) mask: u32,
    /// The character's general category.
    pub(crate) gc: GeneralCategory,
    pub(crate) unicode_props: u16,
    pub(crate) glyph_props: u16,
    pub(crate) lig_props: u8,
    /// The Arabic joining action (upstream's `ot_shaper_var_u8_auxiliary`).
    pub(crate) shaper_action: u8,
    /// The glyph the character maps to, copied into `glyph_id` after mapping.
    pub(crate) glyph_index: u32,
}

/// Where one output glyph goes, in font units.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct GlyphPosition {
    /// How far the pen moves after this glyph, horizontally.
    pub x_advance: i32,
    /// How far the pen moves after this glyph, vertically.
    pub y_advance: i32,
    /// How far the glyph is drawn from the pen, horizontally.
    pub x_offset: i32,
    /// How far the glyph is drawn from the pen, vertically.
    pub y_offset: i32,
}

impl Default for GlyphInfo {
    #[inline]
    fn default() -> Self {
        GlyphInfo {
            glyph_id: 0,
            cluster: 0,
            mask: 0,
            gc: GeneralCategory::Unassigned,
            unicode_props: 0,
            glyph_props: 0,
            lig_props: 0,
            shaper_action: 0,
            glyph_index: 0,
        }
    }
}

impl GlyphInfo {
    #[inline]
    pub(crate) const fn as_char(&self) -> char {
        match char::from_u32(self.glyph_id) {
            Some(c) => c,
            None => '\u{FFFD}',
        }
    }

    #[inline]
    pub(crate) const fn as_glyph(&self) -> crate::GlyphId {
        crate::GlyphId(self.glyph_id as u16)
    }
}

pub(crate) mod unicode_props {
    pub(crate) const IGNORABLE: u16 = 0x0020;
    /// Mongolian free variation selectors, TAG characters, CGJ.
    pub(crate) const HIDDEN: u16 = 0x0040;
    pub(crate) const CONTINUATION: u16 = 0x0080;
    pub(crate) const CF_ZWJ: u16 = 0x0100;
    pub(crate) const CF_ZWNJ: u16 = 0x0200;
    pub(crate) const CF_VS: u16 = 0x0400;
}

pub(crate) mod glyph_props {
    pub(crate) const BASE_GLYPH: u16 = 0x02;
    pub(crate) const LIGATURE: u16 = 0x04;
    pub(crate) const MARK: u16 = 0x08;
    pub(crate) const SUBSTITUTED: u16 = 0x10;
    pub(crate) const LIGATED: u16 = 0x20;
    pub(crate) const MULTIPLIED: u16 = 0x40;
    pub(crate) const PRESERVE: u16 = SUBSTITUTED | LIGATED | MULTIPLIED;
}

pub(crate) const SCRATCH_HAS_NON_ASCII: u32 = 0x0000_0001;
pub(crate) const SCRATCH_HAS_DEFAULT_IGNORABLES: u32 = 0x0000_0002;
pub(crate) const SCRATCH_HAS_CGJ: u32 = 0x0000_0010;

pub(crate) struct Buffer {
    pub(crate) direction: Direction,
    pub(crate) script: Option<Script>,

    /// Allocations stayed within `max_len`.
    pub(crate) successful: bool,
    pub(crate) have_output: bool,
    pub(crate) have_separate_output: bool,
    pub(crate) have_positions: bool,

    pub(crate) idx: usize,
    pub(crate) len: usize,
    pub(crate) out_len: usize,

    pub(crate) info: Vec<GlyphInfo>,
    pub(crate) pos: Vec<GlyphPosition>,
    /// The out buffer while it cannot share `info` (upstream reuses `pos`).
    out: Vec<GlyphInfo>,

    serial: u8,
    pub(crate) scratch_flags: u32,
    max_len: usize,
    pub(crate) max_ops: i32,
}

impl Buffer {
    const MAX_LEN_FACTOR: usize = 64;
    const MAX_LEN_MIN: usize = 16384;
    const MAX_LEN_DEFAULT: usize = 0x3FFF_FFFF;
    const MAX_OPS_FACTOR: i32 = 1024;
    const MAX_OPS_MIN: i32 = 16384;
    const MAX_OPS_DEFAULT: i32 = 0x1FFF_FFFF;

    #[inline]
    pub(crate) fn new() -> Self {
        Buffer {
            direction: Direction::Invalid,
            script: None,
            successful: true,
            have_output: false,
            have_separate_output: false,
            have_positions: false,
            idx: 0,
            len: 0,
            out_len: 0,
            info: Vec::new(),
            pos: Vec::new(),
            out: Vec::new(),
            serial: 0,
            scratch_flags: 0,
            max_len: Self::MAX_LEN_DEFAULT,
            max_ops: Self::MAX_OPS_DEFAULT,
        }
    }

    #[inline]
    pub(crate) fn clear(&mut self) {
        self.direction = Direction::Invalid;
        self.script = None;
        self.successful = true;
        self.have_output = false;
        self.have_positions = false;
        self.idx = 0;
        self.info.clear();
        self.pos.clear();
        self.out.clear();
        self.len = 0;
        self.out_len = 0;
        self.have_separate_output = false;
        self.serial = 0;
        self.scratch_flags = 0;
    }

    #[inline]
    pub(crate) fn out_info(&self) -> &[GlyphInfo] {
        if self.have_separate_output {
            &self.out
        } else {
            &self.info
        }
    }

    #[inline]
    pub(crate) fn out_info_mut(&mut self) -> &mut [GlyphInfo] {
        if self.have_separate_output {
            &mut self.out
        } else {
            &mut self.info
        }
    }

    #[inline]
    fn set_out_info(&mut self, i: usize, info: GlyphInfo) {
        if let Some(slot) = self.out_info_mut().get_mut(i) {
            *slot = info;
        }
    }

    #[inline]
    pub(crate) fn cur(&self, i: usize) -> &GlyphInfo {
        &self.info[self.idx + i]
    }

    #[inline]
    pub(crate) fn cur_mut(&mut self, i: usize) -> &mut GlyphInfo {
        let idx = self.idx + i;
        &mut self.info[idx]
    }

    #[inline]
    pub(crate) fn backtrack_len(&self) -> usize {
        if self.have_output {
            self.out_len
        } else {
            self.idx
        }
    }

    #[inline]
    pub(crate) fn lookahead_len(&self) -> usize {
        self.len - self.idx
    }

    #[inline]
    fn next_serial(&mut self) -> u8 {
        self.serial = self.serial.wrapping_add(1);
        if self.serial == 0 {
            self.serial += 1;
        }
        self.serial
    }

    #[inline]
    pub(crate) fn add(&mut self, codepoint: u32, cluster: u32) {
        if !self.ensure(self.len + 1) {
            return;
        }
        let i = self.len;
        self.info[i] = GlyphInfo {
            glyph_id: codepoint,
            cluster,
            ..GlyphInfo::default()
        };
        self.len += 1;
    }

    #[inline]
    pub(crate) fn push_str(&mut self, text: &str) {
        if !self.ensure(self.len + text.chars().count()) {
            return;
        }
        for (i, c) in text.char_indices() {
            self.add(c as u32, i as u32);
        }
    }

    #[inline]
    pub(crate) fn reverse(&mut self) {
        if self.len == 0 {
            return;
        }
        self.reverse_range(0, self.len);
    }

    #[inline]
    pub(crate) fn reverse_range(&mut self, start: usize, end: usize) {
        if end - start < 2 {
            return;
        }
        self.info[start..end].reverse();
        if self.have_positions {
            self.pos[start..end].reverse();
        }
    }

    pub(crate) fn reverse_groups<F>(&mut self, group: F, merge_clusters: bool)
    where
        F: Fn(&GlyphInfo, &GlyphInfo) -> bool,
    {
        if self.len == 0 {
            return;
        }
        let mut start = 0;
        let mut i = 1;
        while i < self.len {
            if !group(&self.info[i - 1], &self.info[i]) {
                if merge_clusters {
                    self.merge_clusters(start, i);
                }
                self.reverse_range(start, i);
                start = i;
            }
            i += 1;
        }
        if merge_clusters {
            self.merge_clusters(start, i);
        }
        self.reverse_range(start, i);
        self.reverse();
    }

    pub(crate) fn group_end<F>(&self, mut start: usize, group: F) -> usize
    where
        F: Fn(&GlyphInfo, &GlyphInfo) -> bool,
    {
        start += 1;
        while start < self.len && group(&self.info[start - 1], &self.info[start]) {
            start += 1;
        }
        start
    }

    /// Sets an unset script to the first that is not Common, Inherited or Unknown, and an unset direction to its own (left to right by default).
    pub(crate) fn guess_segment_properties(&mut self) {
        if self.script.is_none() {
            for info in &self.info[..self.len] {
                let s = aterm_grapheme::script(info.as_char());
                if s != Script::COMMON && s != Script::INHERITED && s != Script::UNKNOWN {
                    self.script = Some(s);
                    break;
                }
            }
        }
        if self.direction == Direction::Invalid {
            if let Some(script) = self.script {
                self.direction = Direction::from_script(script).unwrap_or_default();
            }
            if self.direction == Direction::Invalid {
                self.direction = Direction::LeftToRight;
            }
        }
    }

    pub(crate) fn sync(&mut self) -> bool {
        if !self.have_output || self.idx > self.len {
            self.successful = false;
        }
        if !self.successful {
            self.have_output = false;
            self.out_len = 0;
            self.idx = 0;
            return false;
        }
        self.next_glyphs(self.len - self.idx);
        if self.have_separate_output {
            std::mem::swap(&mut self.info, &mut self.out);
            self.have_separate_output = false;
        }
        self.len = self.out_len;
        self.have_output = false;
        self.out_len = 0;
        self.idx = 0;
        true
    }

    #[inline]
    pub(crate) fn clear_output(&mut self) {
        self.have_output = true;
        self.have_positions = false;
        self.idx = 0;
        self.out_len = 0;
        self.have_separate_output = false;
    }

    #[inline]
    pub(crate) fn clear_positions(&mut self) {
        self.have_output = false;
        self.have_positions = true;
        self.out_len = 0;
        self.have_separate_output = false;
        for pos in &mut self.pos {
            *pos = GlyphPosition::default();
        }
    }

    pub(crate) fn replace_glyphs(&mut self, num_in: usize, num_out: usize, glyph_data: &[u32]) {
        if !self.make_room_for(num_in, num_out) || self.idx + num_in > self.len {
            return;
        }
        self.merge_clusters(self.idx, self.idx + num_in);
        let orig_info = self.info[self.idx];
        for (i, &glyph) in glyph_data.iter().take(num_out).enumerate() {
            let ii = self.out_len + i;
            self.set_out_info(
                ii,
                GlyphInfo {
                    glyph_id: glyph,
                    ..orig_info
                },
            );
        }
        self.idx += num_in;
        self.out_len += num_out;
    }

    #[inline]
    pub(crate) fn replace_glyph(&mut self, glyph_index: u32) {
        if self.have_separate_output || self.out_len != self.idx {
            if !self.make_room_for(1, 1) {
                return;
            }
            self.set_out_info(self.out_len, self.info[self.idx]);
        }
        let out_len = self.out_len;
        if let Some(slot) = self.out_info_mut().get_mut(out_len) {
            slot.glyph_id = glyph_index;
        }
        self.idx += 1;
        self.out_len += 1;
    }

    #[inline]
    pub(crate) fn output_glyph(&mut self, glyph_index: u32) {
        if !self.make_room_for(0, 1) {
            return;
        }
        if self.idx == self.len && self.out_len == 0 {
            return;
        }
        let out_len = self.out_len;
        let info = if self.idx < self.len {
            self.info[self.idx]
        } else {
            self.out_info()[out_len - 1]
        };
        self.set_out_info(
            out_len,
            GlyphInfo {
                glyph_id: glyph_index,
                ..info
            },
        );
        self.out_len += 1;
    }

    /// Copies the glyph at `idx` to the output and advances.
    #[inline]
    pub(crate) fn next_glyph(&mut self) {
        if self.have_output {
            if self.have_separate_output || self.out_len != self.idx {
                if !self.make_room_for(1, 1) {
                    return;
                }
                self.set_out_info(self.out_len, self.info[self.idx]);
            }
            self.out_len += 1;
        }
        self.idx += 1;
    }

    #[inline]
    pub(crate) fn next_glyphs(&mut self, n: usize) {
        if self.have_output {
            if self.have_separate_output || self.out_len != self.idx {
                if !self.make_room_for(n, n) {
                    return;
                }
                for i in 0..n {
                    self.set_out_info(self.out_len + i, self.info[self.idx + i]);
                }
            }
            self.out_len += n;
        }
        self.idx += n;
    }

    /// Advances `idx` without copying to the output.
    #[inline]
    pub(crate) fn skip_glyph(&mut self) {
        self.idx += 1;
    }

    #[inline]
    pub(crate) fn reset_masks(&mut self, mask: u32) {
        for info in &mut self.info[..self.len] {
            info.mask = mask;
        }
    }

    pub(crate) fn merge_clusters(&mut self, start: usize, end: usize) {
        if end.saturating_sub(start) < 2 {
            return;
        }
        let mut end = end;
        let mut cluster = self.info[start].cluster;
        for i in start + 1..end {
            cluster = cluster.min(self.info[i].cluster);
        }
        if cluster != self.info[end - 1].cluster {
            while end < self.len && self.info[end - 1].cluster == self.info[end].cluster {
                end += 1;
            }
        }
        if self.idx == start && self.info[start].cluster != cluster {
            let old = self.info[start].cluster;
            let mut i = self.out_len;
            while i != 0 && self.out_info()[i - 1].cluster == old {
                self.out_info_mut()[i - 1].cluster = cluster;
                i -= 1;
            }
        }
        for i in start..end {
            self.info[i].cluster = cluster;
        }
    }

    /// Merges clusters for deleting the current glyph, and skips it.
    pub(crate) fn delete_glyph(&mut self) {
        let cluster = self.info[self.idx].cluster;
        if (self.idx + 1 < self.len && cluster == self.info[self.idx + 1].cluster)
            || (self.out_len != 0 && cluster == self.out_info()[self.out_len - 1].cluster)
        {
            self.skip_glyph();
            return;
        }
        if self.out_len != 0 {
            let old_cluster = self.out_info()[self.out_len - 1].cluster;
            if cluster < old_cluster {
                let mut i = self.out_len;
                while i != 0 && self.out_info()[i - 1].cluster == old_cluster {
                    self.out_info_mut()[i - 1].cluster = cluster;
                    i -= 1;
                }
            }
            self.skip_glyph();
            return;
        }
        if self.idx + 1 < self.len {
            self.merge_clusters(self.idx, self.idx + 2);
        }
        self.skip_glyph();
    }

    pub(crate) fn delete_glyphs_inplace(&mut self, filter: impl Fn(&GlyphInfo) -> bool) {
        let mut j = 0;
        for i in 0..self.len {
            if filter(&self.info[i]) {
                let cluster = self.info[i].cluster;
                if i + 1 < self.len && cluster == self.info[i + 1].cluster {
                    continue;
                }
                if j != 0 {
                    let old_cluster = self.info[j - 1].cluster;
                    if cluster < old_cluster {
                        let mut k = j;
                        while k > 0 && self.info[k - 1].cluster == old_cluster {
                            self.info[k - 1].cluster = cluster;
                            k -= 1;
                        }
                    }
                    continue;
                }
                if i + 1 < self.len {
                    self.merge_clusters(i, i + 2);
                }
                continue;
            }
            if j != i {
                self.info[j] = self.info[i];
                self.pos[j] = self.pos[i];
            }
            j += 1;
        }
        self.len = j;
    }

    pub(crate) fn move_to(&mut self, i: usize) -> bool {
        if !self.have_output {
            if i > self.len {
                return false;
            }
            self.idx = i;
            return true;
        }
        if !self.successful || i > self.out_len + (self.len - self.idx) {
            return false;
        }
        if self.out_len < i {
            let count = i - self.out_len;
            if !self.make_room_for(count, count) {
                return false;
            }
            for j in 0..count {
                self.set_out_info(self.out_len + j, self.info[self.idx + j]);
            }
            self.idx += count;
            self.out_len += count;
        } else if self.out_len > i {
            let count = self.out_len - i;
            if self.idx < count {
                self.shift_forward(count - self.idx);
            }
            if self.idx < count {
                return false;
            }
            self.idx -= count;
            self.out_len -= count;
            for j in 0..count {
                self.info[self.idx + j] = self.out_info()[self.out_len + j];
            }
        }
        true
    }

    #[inline]
    #[must_use]
    fn ensure(&mut self, size: usize) -> bool {
        if size < self.len {
            return true;
        }
        if size > self.max_len {
            self.successful = false;
            return false;
        }
        if self.info.len() < size {
            self.info.resize(size, GlyphInfo::default());
        }
        if self.pos.len() < size {
            self.pos.resize(size, GlyphPosition::default());
        }
        if self.out.len() < size {
            self.out.resize(size, GlyphInfo::default());
        }
        true
    }

    #[inline]
    #[must_use]
    fn make_room_for(&mut self, num_in: usize, num_out: usize) -> bool {
        if !self.ensure(self.out_len + num_out) {
            return false;
        }
        if !self.have_separate_output && self.out_len + num_out > self.idx + num_in {
            if !self.have_output {
                self.successful = false;
                return false;
            }
            self.have_separate_output = true;
            let n = self.out_len;
            self.out[..n].copy_from_slice(&self.info[..n]);
        }
        true
    }

    fn shift_forward(&mut self, count: usize) {
        if !self.have_output || !self.ensure(self.len + count) {
            self.successful = false;
            return;
        }
        for i in (0..(self.len - self.idx)).rev() {
            self.info[self.idx + count + i] = self.info[self.idx + i];
        }
        if self.idx + count > self.len {
            for info in &mut self.info[self.len..self.idx + count] {
                *info = GlyphInfo::default();
            }
        }
        self.len += count;
        self.idx += count;
    }

    /// Called around shaping.
    #[inline]
    pub(crate) fn enter(&mut self) {
        self.serial = 0;
        self.scratch_flags = 0;
        if let Some(len) = self.len.checked_mul(Self::MAX_LEN_FACTOR) {
            self.max_len = len.max(Self::MAX_LEN_MIN);
        }
        if let Ok(len) = i32::try_from(self.len)
            && let Some(ops) = len.checked_mul(Self::MAX_OPS_FACTOR)
        {
            self.max_ops = ops.max(Self::MAX_OPS_MIN);
        }
    }

    #[inline]
    pub(crate) fn leave(&mut self) {
        self.max_len = Self::MAX_LEN_DEFAULT;
        self.max_ops = Self::MAX_OPS_DEFAULT;
        self.serial = 0;
    }

    #[inline]
    pub(crate) fn allocate_lig_id(&mut self) -> u8 {
        loop {
            let lig_id = self.next_serial() & 0x07;
            if lig_id != 0 {
                return lig_id;
            }
        }
    }
}

/// Grapheme groups: a glyph continues the group before it.
#[inline]
pub(crate) const fn grapheme_group(_: &GlyphInfo, b: &GlyphInfo) -> bool {
    b.unicode_props & unicode_props::CONTINUATION != 0
}
