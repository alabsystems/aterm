// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `StyleTable`: deduplicated style storage with interning and compaction.

use aterm_hash::FxHashMap;

use super::{ColorType, ExtendedStyle, Style, StyleId};

/// Number of ways in the L1 set-associative style-intern cache.
///
/// SGR-dense workloads cycle through a small working set of styles between
/// resets, which a single-entry cache would thrash; an N-way cache keeps the
/// set hot so repeats skip the HashMap probe.
#[cfg(test)]
const STYLE_L1_WAYS: usize = 4;

/// Deduplicated style storage (Ghostty pattern).
///
/// Styles are interned: identical styles share the same ID. FxHashMap keys
/// the lookup.
///
/// ## Reference counting
///
/// Each style has a count of the cells using it, which compaction uses to
/// drop unused styles.
///
/// ## Thread safety
///
/// `!Sync` (enforced by a `PhantomData` marker). For multi-threaded use wrap it
/// in `Mutex` or `RwLock`; ref counts are plain `u32`, since one table-level
/// lock is cheaper than per-cell atomics.
///
/// ## Memory layout
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
    /// L1 style cache: a small set-associative set of recently interned
    /// styles, skipping the HashMap when SGR sequences alternate among a small
    /// working set. Round-robin replacement via `l1_next`.
    #[cfg(test)]
    l1_cache: [Option<(Style, StyleId)>; STYLE_L1_WAYS],
    /// Round-robin insertion cursor for `l1_cache`.
    #[cfg(test)]
    l1_next: usize,
    /// Direct-mapped style intern cache for indexed colors (level 2).
    ///
    /// 256 entries keyed by `fg_index`, so the full ANSI 256-color palette
    /// never collides; bypasses the HashMap for cycling-color workloads (e.g.
    /// `\x1b[38;5;Nm` per character).
    #[cfg(test)]
    intern_cache: Vec<(Style, StyleId)>,
    /// Direct-mapped style intern cache for RGB colors (level 2b).
    ///
    /// 256 entries keyed by `fg.r` — low-collision for typical truecolor
    /// workloads where fg red channel cycles. Bypasses HashMap for cycling
    /// RGB palettes (e.g., `\x1b[38;2;R;G;Bm` per character).
    #[cfg(test)]
    rgb_cache: Vec<(Style, StyleId)>,
    /// Makes the table `!Sync`, so concurrent access fails to compile.
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

    /// L1 cache probe: check if the given style is in the L1 cache.
    ///
    /// On hit, increments the refcount and returns the cached `StyleId`.
    /// On miss, returns `None`; the caller falls back to `intern_extended`.
    ///
    /// Avoids building the full `ExtendedStyle` (color types, palette indices)
    /// when the same style repeats, the common case for SGR sequences.
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

    /// 90% of u16::MAX — diagnostic warning threshold.
    #[cfg(test)]
    const CAPACITY_WARNING_THRESHOLD: usize = (u16::MAX as usize) * 9 / 10;

    /// Insert a new style (not in table yet).
    ///
    /// At u16::MAX capacity, returns `StyleId::DEFAULT` rather than compacting:
    /// compaction remaps IDs that grid cells still hold, which would corrupt
    /// existing styles. New styles degrade to DEFAULT while existing cells stay
    /// correct.
    #[cfg(test)]
    pub(crate) fn insert_new_style(
        &mut self,
        style: Style,
        ext_info: Option<ExtendedStyleInfo>,
    ) -> StyleId {
        // At u16::MAX capacity: degrade new styles to DEFAULT. Compacting here
        // would remap IDs that grid cells still reference.
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

        // Warn once at 90% capacity as early warning.
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
    #[cfg(test)]
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
    use crate::style::ColorType;

    // =========================================================================
    // Helper: create distinct styles for testing
    // =========================================================================

    // =========================================================================
    // intern_extended() deduplication: same style returns same StyleId
    // =========================================================================

    // =========================================================================
    // get(): retrieve style by StyleId
    // =========================================================================

    #[test]
    fn test_get_invalid_id_returns_none() {
        let table = StyleTable::new();
        let result = table.get(StyleId::new(999));
        assert_eq!(result, None, "out-of-bounds StyleId should return None");
    }

    // =========================================================================
    // Reference counting: intern same style multiple times, verify refcount
    // =========================================================================

    // =========================================================================
    // release(): decrement refcount, verify behavior at 0
    // =========================================================================

    // =========================================================================
    // Boundary: intern many distinct styles
    // =========================================================================

    // =========================================================================
    // Style with different combinations of flags, colors
    // =========================================================================

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
    fn test_extended_invalid_id_returns_none() {
        let table = StyleTable::new();
        let result = table.extended(StyleId::new(999));
        assert!(result.is_none());
    }

    // =========================================================================
    // try_intern_l1(): L1 cache hit/miss
    // =========================================================================

    // =========================================================================
    // insert_new_style: capacity boundary
    // =========================================================================

    // =========================================================================
    // clear(): reset table
    // =========================================================================

    // =========================================================================
    // build_compaction_map(): read-only compaction mapping
    // =========================================================================

    // =========================================================================
    // L2 caches: indexed and RGB
    // =========================================================================

    // =========================================================================
    // Edge case: default style intern
    // =========================================================================
}
