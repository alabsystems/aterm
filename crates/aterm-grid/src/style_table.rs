// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `StyleTable` — deduplicated style storage with interning and compaction.
//!
//! Extracted from `style.rs` to keep it under 500 lines.

use aterm_hash::FxHashMap;

use super::{ColorType, ExtendedStyle, Style, StyleId};

/// Number of ways in the L1 set-associative style-intern cache.
///
/// SGR-dense workloads cycle through a small working set of distinct styles
/// (e.g. bold+colour, underline+colour, reverse) between resets. A single-entry
/// last-repeat cache thrashes because consecutive styles differ; a small N-way
/// cache keeps the whole working set hot so each repeat hits without a HashMap
/// probe (#7351).
#[cfg(test)]
const STYLE_L1_WAYS: usize = 4;

/// Deduplicated style storage (Ghostty pattern).
///
/// Styles are interned: identical styles share the same ID.
/// Uses FxHashMap for fast lookup (2-3x faster than std HashMap for small keys).
///
/// ## Reference Counting
///
/// Each style has a reference count tracking how many cells use it.
/// This enables future garbage collection of unused styles.
///
/// ## Thread Safety
///
/// StyleTable is `!Sync` - it cannot be shared between threads.
/// This is enforced at compile-time via a `PhantomData` marker.
///
/// For multi-threaded use, wrap in `Mutex<StyleTable>` or `RwLock<StyleTable>`.
/// Note that ref_counts use regular u32 (not AtomicU32) for single-threaded
/// performance - locking at the table level is more efficient than per-cell
/// atomic operations.
///
/// ## Memory Layout
///
/// - `styles`: Vec of Style structs (12 bytes each)
/// - `ref_counts`: Vec of u32 (4 bytes each)
/// - `lookup`: FxHashMap for O(1) intern lookups
///
/// For 100 unique styles: ~1.6 KB storage + HashMap overhead
#[derive(Debug)]
pub struct StyleTable {
    /// Stored styles (index = StyleId).
    pub(crate) styles: Vec<Style>,
    /// Reference counts per style.
    pub(crate) ref_counts: Vec<u32>,
    /// Lookup table for interning (style -> id).
    pub(crate) lookup: FxHashMap<Style, StyleId>,
    /// Extended style information (optional, for round-trip conversion).
    /// Only populated when extended info is needed.
    pub(crate) extended: Vec<Option<ExtendedStyleInfo>>,
    /// L1 style cache: a small set-associative set of recently interned styles.
    /// Avoids HashMap lookup when one of the last few styles is re-interned —
    /// the common case for SGR sequences that alternate among a small working
    /// set (#7351). Round-robin replacement via `l1_next`.
    #[cfg(test)]
    l1_cache: [Option<(Style, StyleId)>; STYLE_L1_WAYS],
    /// Round-robin insertion cursor for `l1_cache`.
    #[cfg(test)]
    l1_next: usize,
    /// Direct-mapped style intern cache for indexed colors (level 2).
    ///
    /// 256 entries keyed by `fg_index` — zero collisions for the full
    /// ANSI 256-color palette. Bypasses HashMap for cycling color workloads
    /// (e.g., `\x1b[38;5;Nm` per character), reducing dense_256color
    /// benchmark latency by ~25%.
    #[cfg(test)]
    intern_cache: Vec<(Style, StyleId)>,
    /// Direct-mapped style intern cache for RGB colors (level 2b).
    ///
    /// 256 entries keyed by `fg.r` — low-collision for typical truecolor
    /// workloads where fg red channel cycles. Bypasses HashMap for cycling
    /// RGB palettes (e.g., `\x1b[38;2;R;G;Bm` per character).
    #[cfg(test)]
    rgb_cache: Vec<(Style, StyleId)>,
    /// Marker to make StyleTable !Sync (not shareable across threads).
    /// This catches accidental concurrent access at compile time.
    _not_sync: std::marker::PhantomData<std::cell::Cell<()>>,
}

/// Extended style information for round-trip conversion.
#[derive(Debug, Clone, Copy)]
pub struct ExtendedStyleInfo {
    fg_type: ColorType,
    bg_type: ColorType,
    fg_index: u8,
    bg_index: u8,
}

impl Default for StyleTable {
    fn default() -> Self {
        Self::new()
    }
}

impl StyleTable {
    /// Create a new style table with the default style at index 0.
    ///
    /// The default style is always at `StyleId::DEFAULT` and has a permanent
    /// reference count of 1 (never garbage collected).
    #[must_use]
    pub fn new() -> Self {
        let mut table = Self {
            styles: Vec::with_capacity(64),
            ref_counts: Vec::with_capacity(64),
            lookup: FxHashMap::default(),
            extended: Vec::with_capacity(64),
            #[cfg(test)]
            l1_cache: [None; STYLE_L1_WAYS],
            #[cfg(test)]
            l1_next: 0,
            #[cfg(test)]
            intern_cache: vec![(Style::DEFAULT, StyleId::DEFAULT); Self::INTERN_CACHE_SIZE],
            #[cfg(test)]
            rgb_cache: vec![(Style::DEFAULT, StyleId::DEFAULT); Self::INTERN_CACHE_SIZE],
            _not_sync: std::marker::PhantomData,
        };
        // Style 0 is always the default
        table.styles.push(Style::default());
        table.ref_counts.push(1); // Permanent reference
        table.lookup.insert(Style::default(), StyleId::DEFAULT);
        table.extended.push(None);
        table
    }

    /// Create a style table optimized for Kani verification.
    ///
    /// Uses an empty HashMap (never populated) to avoid the symbolic state
    /// explosion that FxHashMap/hashbrown causes in CBMC. Pair with
    /// `kani_intern()`, which also avoids HashMap.
    #[cfg(kani)]
    #[must_use]
    pub(crate) fn kani_stub() -> Self {
        let mut table = Self {
            styles: Vec::new(),
            ref_counts: Vec::new(),
            lookup: FxHashMap::default(),
            extended: Vec::new(),
            #[cfg(test)]
            l1_cache: [None; STYLE_L1_WAYS],
            #[cfg(test)]
            l1_next: 0,
            #[cfg(test)]
            intern_cache: Vec::new(),
            #[cfg(test)]
            rgb_cache: Vec::new(),
            _not_sync: std::marker::PhantomData,
        };
        table.styles.push(Style::default());
        table.ref_counts.push(1);
        table.extended.push(None);
        table
    }

    /// Intern a style using linear Vec scan (O(n)) instead of HashMap (O(1)).
    ///
    /// Semantically identical to `intern()` but avoids HashMap operations
    /// that are intractable for CBMC. Safe for Kani proofs where tables
    /// contain at most a few entries.
    #[cfg(kani)]
    pub(crate) fn kani_intern(&mut self, style: Style) -> StyleId {
        // Linear dedup scan — equivalent to HashMap lookup for small tables
        for (idx, existing) in self.styles.iter().enumerate() {
            if *existing == style {
                self.ref_counts[idx] = self.ref_counts[idx].saturating_add(1);
                return StyleId::new(u16::try_from(idx).unwrap_or(u16::MAX));
            }
        }

        let id = StyleId::new(u16::try_from(self.styles.len()).unwrap_or(u16::MAX));
        self.styles.push(style);
        self.ref_counts.push(1);
        self.extended.push(None);
        id
    }

    /// L1 cache probe: check if the given style matches the last interned style.
    ///
    /// On hit, increments the refcount and returns the cached `StyleId`.
    /// On miss, returns `None` — the caller should fall back to `intern_extended`.
    ///
    /// This avoids constructing the full `ExtendedStyle` (with color types and
    /// palette indices) when the same style repeats consecutively, which is the
    /// common case for SGR sequences that don't change the style (#7351).
    #[inline]
    #[cfg(test)]
    pub fn try_intern_l1(&mut self, style: &Style) -> Option<StyleId> {
        let id = self.l1_get(style)?;
        let slot = id.raw() as usize;
        self.ref_counts[slot] = self.ref_counts[slot].saturating_add(1);
        Some(id)
    }

    /// Look up a style in the L1 set-associative cache (pure read, no refcount).
    #[inline]
    #[cfg(test)]
    fn l1_get(&self, style: &Style) -> Option<StyleId> {
        for entry in &self.l1_cache {
            if let Some((ref s, id)) = *entry
                && *s == *style
            {
                return Some(id);
            }
        }
        None
    }

    /// Insert (or refresh in place) a style→id mapping in the L1 cache.
    ///
    /// If the style is already cached, its slot is refreshed (keeping each way
    /// distinct); otherwise the round-robin slot is overwritten. Only correct
    /// (style→id) mappings established by the authoritative lookup are inserted,
    /// so a hit always returns the same id a HashMap probe would.
    #[inline]
    #[cfg(test)]
    fn l1_put(&mut self, style: Style, id: StyleId) {
        for entry in &mut self.l1_cache {
            if let Some((ref s, _)) = *entry
                && *s == style
            {
                *entry = Some((style, id));
                return;
            }
        }
        self.l1_cache[self.l1_next] = Some((style, id));
        self.l1_next = (self.l1_next + 1) % STYLE_L1_WAYS;
    }

    /// Intern an extended style with color type information.
    ///
    /// This preserves the color type (default/indexed/rgb) for later
    /// conversion back to `PackedColors` format.
    #[cfg(test)]
    pub fn intern_extended(&mut self, ext_style: ExtendedStyle) -> StyleId {
        // Level 1: set-associative cache for recently interned styles.
        if let Some(last_id) = self.l1_get(&ext_style.style) {
            self.ref_counts[last_id.raw() as usize] =
                self.ref_counts[last_id.raw() as usize].saturating_add(1);
            return last_id;
        }

        // Level 2a: direct-mapped cache for indexed colors (256-color palette).
        // Uses fg_index as the cache slot — zero collisions for 0-255 palette.
        if matches!(ext_style.fg_type, ColorType::Indexed) {
            let slot = usize::from(ext_style.fg_index);
            let (ref cached_style, cached_id) = self.intern_cache[slot];
            if *cached_style == ext_style.style {
                let id_slot = cached_id.raw() as usize;
                self.ref_counts[id_slot] = self.ref_counts[id_slot].saturating_add(1);
                // extended[id_slot] is always Some — set by insert_new_style.
                // (intern() without extended info is test-only.)
                self.l1_put(ext_style.style, cached_id);
                return cached_id;
            }
        } else if matches!(ext_style.fg_type, ColorType::Rgb) {
            // Level 2b: direct-mapped cache for RGB colors keyed by fg.r.
            // Low-collision for cycling truecolor palettes where the red
            // channel varies per character.
            let slot = usize::from(ext_style.style.fg.r);
            let (ref cached_style, cached_id) = self.rgb_cache[slot];
            if *cached_style == ext_style.style {
                let id_slot = cached_id.raw() as usize;
                self.ref_counts[id_slot] = self.ref_counts[id_slot].saturating_add(1);
                self.l1_put(ext_style.style, cached_id);
                return cached_id;
            }
        }

        // Level 3: HashMap lookup.
        if let Some(&id) = self.lookup.get(&ext_style.style) {
            self.ref_counts[id.raw() as usize] =
                self.ref_counts[id.raw() as usize].saturating_add(1);
            // extended may be None if style was first interned via test-only intern().
            // In production only intern_extended is used, so this is always Some.
            if self.extended[id.raw() as usize].is_none() {
                self.extended[id.raw() as usize] = Some(ExtendedStyleInfo {
                    fg_type: ext_style.fg_type,
                    bg_type: ext_style.bg_type,
                    fg_index: ext_style.fg_index,
                    bg_index: ext_style.bg_index,
                });
            }
            self.l1_put(ext_style.style, id);
            // Populate L2 cache on HashMap hit.
            match ext_style.fg_type {
                ColorType::Indexed => {
                    self.intern_cache[usize::from(ext_style.fg_index)] = (ext_style.style, id);
                }
                ColorType::Rgb => {
                    self.rgb_cache[usize::from(ext_style.style.fg.r)] = (ext_style.style, id);
                }
                _ => {}
            }
            return id;
        }

        // Not found — insert new style.
        let info = ExtendedStyleInfo {
            fg_type: ext_style.fg_type,
            bg_type: ext_style.bg_type,
            fg_index: ext_style.fg_index,
            bg_index: ext_style.bg_index,
        };
        let id = self.insert_new_style(ext_style.style, Some(info));
        self.l1_put(ext_style.style, id);
        match ext_style.fg_type {
            ColorType::Indexed => {
                self.intern_cache[usize::from(ext_style.fg_index)] = (ext_style.style, id);
            }
            ColorType::Rgb => {
                self.rgb_cache[usize::from(ext_style.style.fg.r)] = (ext_style.style, id);
            }
            _ => {}
        }
        id
    }

    /// Size of the direct-mapped style intern cache (must be power of 2).
    #[cfg(test)]
    const INTERN_CACHE_SIZE: usize = 256;

    /// 90% of u16::MAX — diagnostic warning threshold (#4548).
    #[cfg(any(test, kani))]
    const CAPACITY_WARNING_THRESHOLD: usize = (u16::MAX as usize) * 9 / 10;

    /// Insert a new style (not in table yet).
    ///
    /// When the table is at u16::MAX capacity, returns `StyleId::DEFAULT` rather
    /// than attempting compaction. Compaction remaps style IDs, but grid cells
    /// still hold old IDs — compacting in place here would silently corrupt all
    /// existing cell styles. The DEFAULT fallback is safe: new styles degrade
    /// gracefully while existing cells remain correct. See #7446.
    #[cfg(any(test, kani))]
    pub(crate) fn insert_new_style(
        &mut self,
        style: Style,
        ext_info: Option<ExtendedStyleInfo>,
    ) -> StyleId {
        // At u16::MAX capacity: degrade new styles to DEFAULT (#7446).
        //
        // Previously this compacted the table in place to reclaim dead slots, but
        // compaction remaps all style IDs and insert_new_style has no way to
        // propagate that remap to grid cells. The result was silent style
        // corruption: existing cells would reference remapped IDs that now
        // point to different styles or DEFAULT. The safe behavior is to let
        // new styles fall back to DEFAULT while preserving existing cell styles.
        if self.styles.len() >= u16::MAX as usize {
            aterm_log::warn!(
                "StyleTable at capacity ({} styles) — \
                 new styles fall back to default. Consider terminal reset.",
                self.styles.len()
            );
            return StyleId::DEFAULT;
        }

        // len < u16::MAX guarded by early return above — try_from cannot fail here
        let id = StyleId::new(u16::try_from(self.styles.len()).unwrap_or(u16::MAX));
        self.styles.push(style);
        self.ref_counts.push(1);
        self.lookup.insert(style, id);
        self.extended.push(ext_info);

        // Warn once at 90% capacity as early warning (#4548).
        if self.styles.len() == Self::CAPACITY_WARNING_THRESHOLD {
            aterm_log::warn!(
                "StyleTable at 90% capacity ({}/{} styles). Rich TUI apps or colorful tools \
                 may exhaust style slots. Consider terminal reset.",
                self.styles.len(),
                u16::MAX
            );
        }

        id
    }

    /// Look up a style by ID.
    #[must_use]
    #[inline]
    pub fn get(&self, id: StyleId) -> Option<&Style> {
        self.styles.get(id.raw() as usize)
    }

    /// Extended style information for round-trip conversion.
    #[must_use]
    pub fn extended(&self, id: StyleId) -> Option<ExtendedStyle> {
        let idx = id.raw() as usize;
        let style = self.styles.get(idx)?;
        let info = self.extended.get(idx)?.as_ref();

        Some(match info {
            Some(info) => ExtendedStyle {
                style: *style,
                fg_type: info.fg_type,
                bg_type: info.bg_type,
                fg_index: info.fg_index,
                bg_index: info.bg_index,
            },
            None => ExtendedStyle {
                style: *style,
                ..ExtendedStyle::DEFAULT
            },
        })
    }

    /// Release a reference to a style.
    ///
    /// Decrements the reference count for the given style ID. When a cell is
    /// overwritten or scrolls off-screen, its style reference should be released
    /// so that [`Self::build_compaction_map`] drops the slot.
    ///
    /// The default style (index 0) is never decremented.
    #[inline]
    #[cfg(any(test, kani))]
    pub fn release(&mut self, id: StyleId) {
        let idx = id.raw() as usize;
        if idx > 0 && idx < self.ref_counts.len() && self.ref_counts[idx] > 0 {
            self.ref_counts[idx] -= 1;
        }
    }

    /// Build a compaction map without mutating the table.
    ///
    /// Returns `(id_map, live_count)` where `id_map[old_id]` gives the new
    /// dense `StyleId` for live styles (ref_count > 0) and `StyleId::DEFAULT`
    /// for dead ones. `live_count` is the number of live (non-default) styles.
    /// Used by checkpoint serialization to remap cell style IDs.
    #[must_use]
    #[allow(
        clippy::needless_range_loop,
        reason = "dual-index read_idx→id_map/ref_counts needs explicit index"
    )]
    #[cfg(test)]
    pub fn build_compaction_map(&self) -> (Vec<StyleId>, u16) {
        let len = self.styles.len();
        let mut id_map = vec![StyleId::DEFAULT; len];
        id_map[0] = StyleId::DEFAULT;
        let mut write_idx = 1usize;
        for read_idx in 1..len {
            if self.ref_counts[read_idx] > 0 {
                id_map[read_idx] = StyleId::new(u16::try_from(write_idx).unwrap_or(u16::MAX));
                write_idx += 1;
            }
        }
        let live_count = u16::try_from(write_idx.saturating_sub(1)).unwrap_or(u16::MAX);
        (id_map, live_count)
    }

    /// Clear all styles except the default.
    #[cfg(test)]
    pub fn clear(&mut self) {
        self.l1_cache = [None; STYLE_L1_WAYS];
        self.l1_next = 0;
        self.intern_cache.fill((Style::DEFAULT, StyleId::DEFAULT));
        self.rgb_cache.fill((Style::DEFAULT, StyleId::DEFAULT));
        self.styles.truncate(1);
        self.ref_counts.truncate(1);
        self.extended.truncate(1);
        self.lookup.clear();
        self.lookup.insert(Style::default(), StyleId::DEFAULT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::{Color, ColorType, ExtendedStyle, StyleAttrs};

    // =========================================================================
    // Helper: create distinct styles for testing
    // =========================================================================

    fn make_style(r: u8, g: u8, b: u8) -> Style {
        Style {
            fg: Color::new(r, g, b),
            bg: Color::DEFAULT_BG,
            attrs: StyleAttrs::empty(),
        }
    }

    fn make_extended(r: u8, g: u8, b: u8, fg_type: ColorType, fg_index: u8) -> ExtendedStyle {
        ExtendedStyle {
            style: Style {
                fg: Color::new(r, g, b),
                bg: Color::DEFAULT_BG,
                attrs: StyleAttrs::empty(),
            },
            fg_type,
            bg_type: ColorType::Default,
            fg_index,
            bg_index: 0,
        }
    }

    // =========================================================================
    // intern_extended() deduplication: same style returns same StyleId
    // =========================================================================

    #[test]
    fn test_intern_extended_dedup_no_extra_entry() {
        let mut table = StyleTable::new();
        let ext = make_extended(100, 150, 200, ColorType::Rgb, 0);
        table.intern_extended(ext);
        table.intern_extended(ext);
        assert_eq!(
            table.styles.len(),
            2,
            "duplicate intern should not add another entry"
        );
    }

    #[test]
    fn test_intern_extended_different_styles_different_ids() {
        let mut table = StyleTable::new();
        let ext1 = make_extended(255, 0, 0, ColorType::Rgb, 0);
        let ext2 = make_extended(0, 255, 0, ColorType::Rgb, 0);
        let id1 = table.intern_extended(ext1);
        let id2 = table.intern_extended(ext2);
        assert_ne!(id1, id2, "different styles should get different IDs");
        assert_eq!(table.styles.len(), 3, "two distinct styles + default = 3");
    }

    // =========================================================================
    // get(): retrieve style by StyleId
    // =========================================================================

    #[test]
    fn test_get_interned_style() {
        let mut table = StyleTable::new();
        let ext = make_extended(42, 84, 126, ColorType::Rgb, 0);
        let id = table.intern_extended(ext);
        let retrieved = table.get(id);
        assert_eq!(retrieved, Some(&ext.style));
    }

    #[test]
    fn test_get_invalid_id_returns_none() {
        let table = StyleTable::new();
        let result = table.get(StyleId::new(999));
        assert_eq!(result, None, "out-of-bounds StyleId should return None");
    }

    // =========================================================================
    // Reference counting: intern same style multiple times, verify refcount
    // =========================================================================

    #[test]
    fn test_intern_extended_twice_refcount_is_2() {
        let mut table = StyleTable::new();
        let ext = make_extended(10, 20, 30, ColorType::Rgb, 0);
        let id = table.intern_extended(ext);
        table.intern_extended(ext);
        assert_eq!(table.ref_counts[id.raw() as usize], 2);
    }

    // =========================================================================
    // release(): decrement refcount, verify behavior at 0
    // =========================================================================

    #[test]
    fn test_release_at_zero_does_not_underflow() {
        let mut table = StyleTable::new();
        let ext = make_extended(10, 20, 30, ColorType::Rgb, 0);
        let id = table.intern_extended(ext);
        table.release(id); // refcount = 0
        table.release(id); // should not underflow
        assert_eq!(table.ref_counts[id.raw() as usize], 0);
    }

    #[test]
    fn test_release_out_of_bounds_is_safe() {
        let mut table = StyleTable::new();
        // Should not panic
        table.release(StyleId::new(9999));
    }

    // =========================================================================
    // Boundary: intern many distinct styles
    // =========================================================================

    #[test]
    fn test_intern_many_distinct_styles() {
        let mut table = StyleTable::new();
        let mut ids = Vec::new();
        for i in 0..100u8 {
            let ext = make_extended(i, i.wrapping_mul(2), i.wrapping_mul(3), ColorType::Rgb, 0);
            let id = table.intern_extended(ext);
            ids.push(id);
        }
        assert_eq!(table.styles.len(), 101, "100 distinct styles + default");
        // All IDs should be unique
        let mut sorted: Vec<u16> = ids.iter().map(|id| id.raw()).collect();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 100, "all 100 IDs should be unique");
    }

    // =========================================================================
    // Style with different combinations of flags, colors
    // =========================================================================

    #[test]
    fn test_intern_style_with_attrs() {
        let mut table = StyleTable::new();
        let ext = ExtendedStyle {
            style: Style {
                fg: Color::DEFAULT_FG,
                bg: Color::DEFAULT_BG,
                attrs: StyleAttrs::BOLD | StyleAttrs::ITALIC,
            },
            ..ExtendedStyle::DEFAULT
        };
        let id = table.intern_extended(ext);
        let retrieved = table.get(id).unwrap();
        assert!(retrieved.attrs.contains(StyleAttrs::BOLD));
        assert!(retrieved.attrs.contains(StyleAttrs::ITALIC));
    }

    #[test]
    fn test_intern_style_indexed_fg() {
        let mut table = StyleTable::new();
        let ext = make_extended(205, 0, 0, ColorType::Indexed, 1);
        let id = table.intern_extended(ext);
        let retrieved_ext = table.extended(id);
        assert!(retrieved_ext.is_some());
        let re = retrieved_ext.unwrap();
        assert_eq!(re.fg_type, ColorType::Indexed);
        assert_eq!(re.fg_index, 1);
    }

    #[test]
    fn test_intern_style_rgb_bg() {
        let mut table = StyleTable::new();
        let ext = ExtendedStyle {
            style: Style {
                fg: Color::DEFAULT_FG,
                bg: Color::new(10, 20, 30),
                attrs: StyleAttrs::empty(),
            },
            fg_type: ColorType::Default,
            bg_type: ColorType::Rgb,
            fg_index: 0,
            bg_index: 0,
        };
        let id = table.intern_extended(ext);
        let re = table.extended(id).unwrap();
        assert_eq!(re.bg_type, ColorType::Rgb);
        assert_eq!(re.style.bg, Color::new(10, 20, 30));
    }

    #[test]
    fn test_intern_style_both_indexed() {
        let mut table = StyleTable::new();
        let ext = ExtendedStyle {
            style: Style {
                fg: Color::new(205, 0, 0),
                bg: Color::new(0, 0, 238),
                attrs: StyleAttrs::UNDERLINE,
            },
            fg_type: ColorType::Indexed,
            bg_type: ColorType::Indexed,
            fg_index: 1,
            bg_index: 4,
        };
        let id = table.intern_extended(ext);
        let re = table.extended(id).unwrap();
        assert_eq!(re.fg_type, ColorType::Indexed);
        assert_eq!(re.fg_index, 1);
        assert_eq!(re.bg_type, ColorType::Indexed);
        assert_eq!(re.bg_index, 4);
    }

    // =========================================================================
    // extended(): retrieve ExtendedStyle info
    // =========================================================================

    #[test]
    fn test_extended_default_id() {
        let table = StyleTable::new();
        let ext = table.extended(StyleId::DEFAULT);
        assert!(ext.is_some());
        // Default style has no extended info, so it should return default types
        let re = ext.unwrap();
        assert_eq!(re.fg_type, ColorType::Default);
        assert_eq!(re.bg_type, ColorType::Default);
    }

    #[test]
    fn test_extended_preserves_color_type_info() {
        let mut table = StyleTable::new();
        let ext = ExtendedStyle {
            style: Style {
                fg: Color::new(100, 150, 200),
                bg: Color::DEFAULT_BG,
                attrs: StyleAttrs::empty(),
            },
            fg_type: ColorType::Rgb,
            bg_type: ColorType::Default,
            fg_index: 0,
            bg_index: 0,
        };
        let id = table.intern_extended(ext);
        let re = table.extended(id).unwrap();
        assert_eq!(re.fg_type, ColorType::Rgb);
        assert_eq!(re.style.fg.r, 100);
        assert_eq!(re.style.fg.g, 150);
        assert_eq!(re.style.fg.b, 200);
    }

    #[test]
    fn test_extended_invalid_id_returns_none() {
        let table = StyleTable::new();
        let result = table.extended(StyleId::new(999));
        assert!(result.is_none());
    }

    // =========================================================================
    // try_intern_l1(): L1 cache hit/miss
    // =========================================================================

    #[test]
    fn test_try_intern_l1_miss_on_empty_cache() {
        let mut table = StyleTable::new();
        let style = make_style(255, 0, 0);
        assert!(
            table.try_intern_l1(&style).is_none(),
            "L1 cache should miss when empty"
        );
    }

    #[test]
    fn test_try_intern_l1_hit_after_intern_extended() {
        let mut table = StyleTable::new();
        let ext = make_extended(255, 0, 0, ColorType::Rgb, 0);
        let id = table.intern_extended(ext);
        // L1 cache should now contain this style
        let result = table.try_intern_l1(&ext.style);
        assert_eq!(
            result,
            Some(id),
            "L1 cache should hit for the last interned style"
        );
    }

    #[test]
    fn test_try_intern_l1_miss_for_different_style() {
        let mut table = StyleTable::new();
        let ext = make_extended(255, 0, 0, ColorType::Rgb, 0);
        table.intern_extended(ext);
        let different = make_style(0, 255, 0);
        assert!(
            table.try_intern_l1(&different).is_none(),
            "L1 should miss for different style"
        );
    }

    #[test]
    fn test_try_intern_l1_increments_refcount() {
        let mut table = StyleTable::new();
        let ext = make_extended(255, 0, 0, ColorType::Rgb, 0);
        let id = table.intern_extended(ext);
        assert_eq!(table.ref_counts[id.raw() as usize], 1);
        table.try_intern_l1(&ext.style);
        assert_eq!(
            table.ref_counts[id.raw() as usize],
            2,
            "L1 hit should increment refcount"
        );
    }

    // =========================================================================
    // insert_new_style: capacity boundary
    // =========================================================================

    #[test]
    fn test_insert_new_style_returns_sequential_ids() {
        let mut table = StyleTable::new();
        let s1 = make_style(1, 0, 0);
        let s2 = make_style(2, 0, 0);
        let id1 = table.insert_new_style(s1, None);
        let id2 = table.insert_new_style(s2, None);
        assert_eq!(id1.raw(), 1);
        assert_eq!(id2.raw(), 2);
    }

    // =========================================================================
    // clear(): reset table
    // =========================================================================

    #[test]
    fn test_clear_leaves_only_default() {
        let mut table = StyleTable::new();
        let ext1 = make_extended(255, 0, 0, ColorType::Rgb, 0);
        let ext2 = make_extended(0, 255, 0, ColorType::Rgb, 0);
        table.intern_extended(ext1);
        table.intern_extended(ext2);
        assert_eq!(table.styles.len(), 3);
        table.clear();
        assert_eq!(table.styles.len(), 1);
        assert_eq!(table.ref_counts.len(), 1);
        assert_eq!(table.extended.len(), 1);
    }

    #[test]
    fn test_clear_resets_l1_cache() {
        let mut table = StyleTable::new();
        let ext = make_extended(255, 0, 0, ColorType::Rgb, 0);
        table.intern_extended(ext);
        assert!(table.l1_cache.iter().any(Option::is_some));
        table.clear();
        assert!(table.l1_cache.iter().all(Option::is_none));
    }

    #[test]
    fn test_clear_preserves_lookup_for_default() {
        let mut table = StyleTable::new();
        table.intern_extended(make_extended(255, 0, 0, ColorType::Rgb, 0));
        table.clear();
        assert_eq!(
            table.lookup.len(),
            1,
            "only default should remain in lookup"
        );
        assert_eq!(table.lookup.get(&Style::default()), Some(&StyleId::DEFAULT));
    }

    #[test]
    fn test_intern_after_clear() {
        let mut table = StyleTable::new();
        let ext = make_extended(255, 0, 0, ColorType::Rgb, 0);
        table.intern_extended(ext);
        table.clear();
        // Re-intern should work normally
        let id = table.intern_extended(ext);
        assert_ne!(id, StyleId::DEFAULT);
        assert_eq!(table.styles.len(), 2);
    }

    // =========================================================================
    // build_compaction_map(): read-only compaction mapping
    // =========================================================================

    #[test]
    fn test_build_compaction_map_does_not_mutate() {
        let mut table = StyleTable::new();
        let ext = make_extended(255, 0, 0, ColorType::Rgb, 0);
        let id = table.intern_extended(ext);
        table.release(id);
        let len_before = table.styles.len();
        let (_map, _count) = table.build_compaction_map();
        assert_eq!(
            table.styles.len(),
            len_before,
            "build_compaction_map must not mutate table"
        );
    }

    #[test]
    fn test_build_compaction_map_dead_maps_to_default() {
        let mut table = StyleTable::new();
        let ext = make_extended(255, 0, 0, ColorType::Rgb, 0);
        let id = table.intern_extended(ext);
        table.release(id);
        let (map, live_count) = table.build_compaction_map();
        assert_eq!(map[id.raw() as usize], StyleId::DEFAULT);
        assert_eq!(live_count, 0, "no live non-default styles");
    }

    #[test]
    fn test_build_compaction_map_live_count() {
        let mut table = StyleTable::new();
        let ext1 = make_extended(1, 0, 0, ColorType::Rgb, 0);
        let ext2 = make_extended(2, 0, 0, ColorType::Rgb, 0);
        let ext3 = make_extended(3, 0, 0, ColorType::Rgb, 0);
        table.intern_extended(ext1);
        let id2 = table.intern_extended(ext2);
        table.intern_extended(ext3);
        table.release(id2);
        let (_map, live_count) = table.build_compaction_map();
        assert_eq!(live_count, 2, "2 live non-default styles");
    }

    // =========================================================================
    // L2 cache: indexed color cache
    // =========================================================================

    #[test]
    fn test_indexed_l2_cache_hit() {
        let mut table = StyleTable::new();
        let ext = make_extended(205, 0, 0, ColorType::Indexed, 1);
        let id1 = table.intern_extended(ext);
        // Clear L1 cache by interning a different style
        let other = make_extended(0, 0, 0, ColorType::Default, 0);
        table.intern_extended(other);
        // Now re-intern the indexed style -- should hit L2 cache
        let id2 = table.intern_extended(ext);
        assert_eq!(id1, id2, "indexed color should hit L2 cache on re-intern");
    }

    #[test]
    fn test_rgb_l2_cache_hit() {
        let mut table = StyleTable::new();
        let ext = ExtendedStyle {
            style: Style {
                fg: Color::new(100, 50, 25),
                bg: Color::DEFAULT_BG,
                attrs: StyleAttrs::empty(),
            },
            fg_type: ColorType::Rgb,
            bg_type: ColorType::Default,
            fg_index: 0,
            bg_index: 0,
        };
        let id1 = table.intern_extended(ext);
        // Clear L1 cache
        let other = make_extended(0, 0, 0, ColorType::Default, 0);
        table.intern_extended(other);
        // Re-intern -- should hit RGB L2 cache (keyed by fg.r = 100)
        let id2 = table.intern_extended(ext);
        assert_eq!(id1, id2, "RGB color should hit L2 cache");
    }

    // =========================================================================
    // Edge case: default style intern
    // =========================================================================

    #[test]
    fn test_intern_default_style_returns_default_id() {
        let mut table = StyleTable::new();
        let ext = ExtendedStyle::DEFAULT;
        let id = table.intern_extended(ext);
        assert_eq!(
            id,
            StyleId::DEFAULT,
            "interning the default style should return DEFAULT id"
        );
    }

    #[test]
    fn test_intern_default_style_increments_default_refcount() {
        let mut table = StyleTable::new();
        let before = table.ref_counts[0];
        let ext = ExtendedStyle::DEFAULT;
        table.intern_extended(ext);
        assert_eq!(
            table.ref_counts[0],
            before + 1,
            "interning default increments its refcount"
        );
    }
}
