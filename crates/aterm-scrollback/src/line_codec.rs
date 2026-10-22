// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Serialization helpers for scrollback lines.

use super::{CellAttrs, HyperlinkSpan, Line, LineContent, LineFlags, UnderlineColorSpan};
use aterm_alloc::SmallVec;
use aterm_rle::Rle;
use std::sync::Arc;

/// Absolute cap on a single deserialized line's content byte length.
///
/// A real stored line is one grid row: at most `MAX_GRID_COLS` (4096) cells of
/// at most one grapheme unit (`MAX_GRAPHEME_UNIT_BYTES`, 256 B) each, under
/// ~1.05 MiB. The wire `content_len` is otherwise bounded only by the
/// surrounding buffer (up to the 64 MiB cold decode cap), so a crafted record
/// could hand every consumer a multi-MiB line. An over-cap record is skipped;
/// its `line_size` still advances the decode offset.
pub(crate) const MAX_LINE_CONTENT_BYTES: usize = 2 * 1024 * 1024;

/// Cap on a single deserialized hyperlink URL: the OSC 8 ingestion ceiling
/// (`aterm-core`'s `MAX_HYPERLINK_URL_BYTES`). A larger `url_len` is a crafted
/// checkpoint or cold page, and [`MAX_LINE_CONTENT_BYTES`] does not cover the
/// hyperlink section. An over-cap span is skipped; its bytes are stepped over.
pub(crate) const MAX_HYPERLINK_URL_BYTES: usize = 8192;

/// Cap on a single deserialized OSC 8 `id=`: the ingestion ceiling (`aterm-core`
/// admits ids of at most 256 bytes). An over-cap id is dropped (the span keeps
/// its URL with `id = None`); its bytes are still stepped over.
pub(crate) const MAX_HYPERLINK_ID_BYTES: usize = 256;

/// The most bytes one hyperlink span takes in a v3 record whose URL and id are
/// within the ceilings above: `start_col:2 + end_col:2 + url_len:4 + url +
/// id_len:4 + id`, 8,460 bytes. An honest writer's links add at most this per
/// column to one line's record (`line_codec_block::max_linked_record_bytes`).
pub(crate) const MAX_HYPERLINK_SPAN_BYTES: usize =
    12 + MAX_HYPERLINK_URL_BYTES + MAX_HYPERLINK_ID_BYTES;

/// Cap on the number of hyperlink spans in one deserialized line: `MAX_GRID_COLS`
/// (4096). The write path coalesces to at most one span per cell, so a larger
/// stored count (a u16) is crafted; at ~40 B in memory per span, 65535 spans
/// would cost ~2.6 MiB in one line. The per-cell restore loops
/// (`fill_row_from_line`, `restore_hyperlinks`) are separately bounded to
/// O(cols) against overlapping spans.
pub(crate) const MAX_HYPERLINK_SPANS: usize = 4096;

/// Cap on the number of underline-colour spans in one deserialized line,
/// `MAX_GRID_COLS` (4096), for the same reason as [`MAX_HYPERLINK_SPANS`].
pub(crate) const MAX_UNDERLINE_SPANS: usize = 4096;

impl CellAttrs {
    /// Serialize to bytes (10 bytes).
    #[must_use]
    pub(crate) const fn serialize(&self) -> [u8; 10] {
        // An array literal, not range-IndexMut + copy_from_slice, whose std
        // bodies (index_mut, the len assert) the verifier does not have.
        let fg = self.fg.to_le_bytes();
        let bg = self.bg.to_le_bytes();
        let fl = self.flags.to_le_bytes();
        [
            fg[0], fg[1], fg[2], fg[3], bg[0], bg[1], bg[2], bg[3], fl[0], fl[1],
        ]
    }

    /// Deserialize from bytes.
    #[must_use]
    pub(crate) const fn deserialize(data: &[u8]) -> Option<Self> {
        if data.len() < 10 {
            return None;
        }
        let fg = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let bg = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let flags = u16::from_le_bytes([data[8], data[9]]);
        Some(Self { fg, bg, flags })
    }
}

impl Line {
    /// Serialize line to bytes for compression.
    ///
    /// Format v4 (v3 plus a trailing SGR 58 underline-colour section):
    /// ```text
    /// [version:1][flags:1][content_len:4][content:content_len]
    /// [has_attrs:1][if has_attrs: run_count:4 + runs...]
    /// [hyperlink_count:2][foreach hyperlink:
    ///   start_col:2 + end_col:2 + url_len:4 + url + id_len:4 + id]
    /// [if version>=4: underline_count:2 + foreach: start_col:2 + end_col:2 + color:4]
    /// ```
    ///
    /// Version 0 = legacy format (no attrs)
    /// Version 1 = with RLE attrs (no hyperlinks)
    /// Version 2 = with RLE attrs + hyperlinks (no IDs)
    /// Version 3 = with RLE attrs + hyperlinks + OSC 8 IDs
    /// Version 4 = v3 + underline-colour spans, emitted only when SGR 58 is present
    ///             (a v3 reader stops after the hyperlinks and ignores the section)
    ///
    /// # Serialization limits
    ///
    /// Counts are fixed-width: content and each URL/ID are capped at `u32::MAX`
    /// bytes, attribute runs at `u32::MAX`, hyperlinks per line at `u16::MAX`.
    /// Past those the length field saturates; no real terminal line comes close.
    ///
    /// # Inline images are not on the wire
    ///
    /// [`Line::images`] has no wire section. An inline image lives as long as its
    /// line stays in the in-memory hot tier and is dropped at the one boundary
    /// where a surviving line is serialized, `promote_hot_to_warm`; the warm and
    /// cold tiers carry already-serialized bytes. The wire has no cross-line
    /// dictionary, so a payload shared across a footprint would be written once
    /// per covered row (a 2 MiB screenshot over 20 rows is 40 MiB of warm blocks);
    /// keeping it needs a page-level payload table, a wire-format change.
    ///
    /// Every line that crosses the boundary is counted by
    /// [`Scrollback::image_rows_dropped_by_compression`] (hosts read
    /// `Terminal::scrollback_image_rows_dropped`); content never carries a
    /// sentinel. The line keeps its text, attributes, hyperlinks and underline
    /// colours, so a footprint ages out one row at a time from its top, like the
    /// text around it.
    ///
    /// Outside the tests, `aterm-gui`'s `seamless::strip_blob_links` re-serializes
    /// every decoded record and compares it with the bytes it was read from: a line
    /// that does not round-trip makes the handoff's link-stripping rung give up.
    ///
    /// [`Scrollback::image_rows_dropped_by_compression`]: crate::Scrollback::image_rows_dropped_by_compression
    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        // A capacity hint only, bounded so the strict L0 gate's overflow and
        // allocation-budget obligations stay provable; real lines are far below
        // the bound.
        let estimate = self.serialized_size_estimate();
        let mut v = if estimate < crate::codec::MAX_DECOMPRESSED_SCROLLBACK_PAGE_BYTES {
            Vec::with_capacity(estimate)
        } else {
            Vec::new()
        };
        self.append_serialized(&mut v);
        v
    }

    /// Exact per-line serialized-size estimate shared by line and block
    /// serialization. Saturation is safe for bounded serialization: a finite
    /// output budget refuses the saturated value before allocation.
    // Skip: the residual row is `Rvalue::UnaryOp(PtrMetadata)`, diagnostic-only
    // until the metadata lane is modeled.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(super) fn serialized_size_estimate(&self) -> usize {
        let content_len = self.content.as_bytes().len();
        // Closure-free shapes and fn-item accessors (`attr_runs`,
        // `Line::hyperlinks`): called directly, the tiny aterm-rle/aterm-alloc
        // methods get MIR-inlined and the Trust lane's hardened-unsafe check
        // fails closed on their std internals (see `attr_runs` in line.rs).
        let attrs_size = if self.attrs.is_some() {
            // has_attrs + run_count + runs
            super::attr_runs(self.attrs.as_deref())
                .len()
                .saturating_mul(14)
                .saturating_add(5)
        } else {
            1
        };
        let hyperlinks_size = match self.hyperlinks() {
            Some(spans) => {
                let mut size = 2usize;
                for span in spans {
                    size = size.saturating_add(span.serialized_size());
                }
                size
            }
            None => 2,
        };
        // v4 underline section: a 2-byte count plus 8 bytes per span, only when
        // present.
        let underline_colors_size = match self.underline_colors() {
            Some(spans) => spans.len().saturating_mul(8).saturating_add(2),
            None => 0,
        };
        6usize
            .saturating_add(content_len)
            .saturating_add(attrs_size)
            .saturating_add(hyperlinks_size)
            .saturating_add(underline_colors_size)
    }

    /// Serialize line by appending its bytes to `result`, reserving this line's
    /// size first. Same wire format as [`Line::serialize`].
    // Skip: the residual rows are `Rvalue::UnaryOp(PtrMetadata)` (diagnostic-only
    // until the metadata lane is modeled) and the `Vec::extend_from_slice` alloc
    // class. Round-trip tested against deserialize; every write is length-prefixed.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn serialize_into(&self, result: &mut Vec<u8>) {
        // Bounded capacity hint, as in `serialize`.
        let estimate = self.serialized_size_estimate();
        if estimate < crate::codec::MAX_DECOMPRESSED_SCROLLBACK_PAGE_BYTES {
            result.reserve(estimate);
        }
        self.append_serialized(result);
    }

    /// Shared byte writer. Callers reserve at the line or block boundary, so a
    /// sized block does not remeasure every line; `Vec` still grows when the
    /// bounded reservation hint was skipped.
    // Skip: same PtrMetadata/extend allocation class as `serialize_into`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(super) fn append_serialized(&self, result: &mut Vec<u8>) {
        let content = self.content.as_bytes();
        let content_len = content.len();

        // Version byte: 4 only when the line has SGR 58 underline colours, so
        // every other line stays byte-identical to v3.
        result.push(if self.underline_colors.is_some() {
            4
        } else {
            3
        });

        result.push(self.flags.bits());

        // Content length and content (see Serialization limits).
        let content_len_u32 = u32::try_from(content_len).unwrap_or(u32::MAX);
        result.extend_from_slice(&content_len_u32.to_le_bytes());
        result.extend_from_slice(content);

        // Attributes, via `attr_runs` for the Trust lane (see
        // `serialized_size_estimate`).
        if self.attrs.is_some() {
            let runs = super::attr_runs(self.attrs.as_deref());
            result.push(1); // has_attrs = true
            let run_count = u32::try_from(runs.len()).unwrap_or(u32::MAX);
            result.extend_from_slice(&run_count.to_le_bytes());
            for run in runs {
                // Each run: [value:10][length:4]
                result.extend_from_slice(&run.value.serialize());
                result.extend_from_slice(&run.length.to_le_bytes());
            }
        } else {
            result.push(0); // has_attrs = false
        }

        // Hyperlinks (v3: with OSC 8 IDs), from the plain slice `Line::hyperlinks`
        // returns, for the Trust lane (see `serialized_size_estimate`).
        if let Some(spans) = self.hyperlinks() {
            let count = u16::try_from(spans.len()).unwrap_or(u16::MAX);
            result.extend_from_slice(&count.to_le_bytes());
            for span in spans {
                result.extend_from_slice(&span.start_col.to_le_bytes());
                result.extend_from_slice(&span.end_col.to_le_bytes());
                let url_len = u32::try_from(span.url.len()).unwrap_or(u32::MAX);
                result.extend_from_slice(&url_len.to_le_bytes());
                result.extend_from_slice(span.url.as_bytes());
                // v3: hyperlink ID
                let id_len = span
                    .id
                    .as_ref()
                    .map_or(0u32, |id| u32::try_from(id.len()).unwrap_or(u32::MAX));
                result.extend_from_slice(&id_len.to_le_bytes());
                if let Some(id) = &span.id {
                    result.extend_from_slice(id.as_bytes());
                }
            }
        } else {
            result.extend_from_slice(&0u16.to_le_bytes()); // 0 hyperlinks
        }

        // Underline colours (v4 only), after the hyperlinks so a v3 reader that
        // stops there ignores them. Each span is [start_col:2][end_col:2][color:4],
        // `color` the packed 0xTT_XXXXXX form (RGB vs indexed kept for palette
        // re-resolution).
        if let Some(spans) = self.underline_colors() {
            let count = u16::try_from(spans.len()).unwrap_or(u16::MAX);
            result.extend_from_slice(&count.to_le_bytes());
            for span in spans {
                result.extend_from_slice(&span.start_col.to_le_bytes());
                result.extend_from_slice(&span.end_col.to_le_bytes());
                result.extend_from_slice(&span.color.to_le_bytes());
            }
        }
    }

    /// Deserialize line from bytes.
    #[must_use]
    // Skip: the decode reader's strict-UTF-8 / SmallVec-with_capacity class
    // (its sibling deserializers carry the same). Round-trip tested; every
    // malformed input returns None (fail-closed).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn deserialize(data: &[u8]) -> Option<Self> {
        if data.is_empty() {
            return None;
        }

        let version = data[0];
        if version == 0 {
            // Legacy format (version 0 or old format without version byte)
            return Self::deserialize_legacy(data);
        }

        if data.len() < 7 {
            return None;
        }

        // v1 to v4 share the base layout.
        let flags = LineFlags::from_bits_truncate(data[1]);
        let content_len = u32::from_le_bytes([data[2], data[3], data[4], data[5]]) as usize;
        if content_len > MAX_LINE_CONTENT_BYTES {
            return None;
        }

        let content_end = 6usize.checked_add(content_len)?;
        if data.len() < content_end.checked_add(1)? {
            return None;
        }

        // `get` rather than indexing: the spelling the strict L0 gate proves.
        let content = LineContent::from_bytes(data.get(6..content_end)?);

        let (attrs, offset) = Self::deserialize_attrs(data, content_end)?;
        // v3/v4 report the offset past the hyperlink section so the v4
        // underline-colour section can be located; v2 has no following section.
        let (hyperlinks, offset) = if version >= 3 {
            Self::deserialize_hyperlinks_v3(data, offset)
        } else if version >= 2 {
            (Self::deserialize_hyperlinks(data, offset), offset)
        } else {
            (None, offset)
        };
        let underline_colors = if version >= 4 {
            Self::deserialize_underline_colors(data, offset)
        } else {
            None
        };

        Some(Self {
            content,
            attrs: attrs.map(Box::new),
            flags,
            hyperlinks: hyperlinks.map(Box::new),
            underline_colors: underline_colors.map(Box::new),
            // No wire section for images (see `serialize`).
            images: None,
        })
    }

    /// Whether `data` frames a record [`Self::deserialize`] would accept, decided
    /// from the header without building the `Line`.
    ///
    /// The warm→cold eviction path uses it to validate a page whose bytes it reuses
    /// verbatim (`WarmBlock::try_decompress_serialized`). It must encode exactly
    /// `deserialize`'s `None` conditions: `walk_records` pins the two together with
    /// a `debug_assert_eq!` on every record. Only the header and the attrs run count
    /// can reject; the hyperlink and underline sections clamp rather than fail.
    #[must_use]
    pub(crate) fn record_is_valid(data: &[u8]) -> bool {
        let Some(&version) = data.first() else {
            return false;
        };

        if version == 0 {
            // Legacy: `[flags:1][len:4][content:len]` (`deserialize_legacy`), read in the
            // decoders' sub-slice shape for the strict L0 gate.
            let Some(header) = data.get(1..5) else {
                return false;
            };
            if header.len() < 4 {
                return false;
            }
            let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
            if len > MAX_LINE_CONTENT_BYTES {
                return false;
            }
            let Some(end) = 5usize.checked_add(len) else {
                return false;
            };
            return data.len() >= end;
        }

        // v1+: `[version:1][flags:1][len:4][content:len][has_attrs:1]…`
        if data.len() < 7 {
            return false;
        }
        let Some(header) = data.get(2..6) else {
            return false;
        };
        if header.len() < 4 {
            return false;
        }
        let content_len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
        if content_len > MAX_LINE_CONTENT_BYTES {
            return false;
        }
        let Some(content_end) = 6usize.checked_add(content_len) else {
            return false;
        };
        let Some(min_len) = content_end.checked_add(1) else {
            return false;
        };
        if data.len() < min_len {
            return false;
        }
        // `deserialize_attrs` rejects a record that ends inside the 4-byte run count.
        // The block walk never offers one (`line_size_v1v2` needs those bytes), but
        // this predicate mirrors `deserialize`, not the walk.
        let Some(&has_attrs) = data.get(content_end) else {
            return false;
        };
        if has_attrs == 0 {
            return true;
        }
        // `content_end + 5` is `attrs_start(+1) + run_count(4)`; one checked
        // add covers both of `deserialize_attrs`'s overflow checks.
        let Some(runs_start) = content_end.checked_add(5) else {
            return false;
        };
        data.len() >= runs_start
    }

    /// Deserialize RLE attributes starting at `content_end` in `data`.
    ///
    /// Returns `(attrs, next_offset)` or `None` if data is truncated.
    fn deserialize_attrs(
        data: &[u8],
        content_end: usize,
    ) -> Option<(Option<Rle<CellAttrs>>, usize)> {
        if content_end >= data.len() {
            return None;
        }
        if data[content_end] == 0 {
            // saturating_add cannot wrap here (guard above); it discharges an obligation
            // the verifier cannot chain.
            return Some((None, content_end.saturating_add(1)));
        }

        let attrs_start = content_end.checked_add(1)?;
        let runs_start = attrs_start.checked_add(4)?;
        // Sub-slice + length guard + constant indexing is the shape the strict
        // L0 gate proves: the never-taken `len < 4` branch is the comparison
        // that proves the constant indexes (a slice pattern emits per-element
        // obligations it cannot discharge).
        let count_bytes = data.get(attrs_start..runs_start)?;
        if count_bytes.len() < 4 {
            return None;
        }
        let run_count = u32::from_le_bytes([
            count_bytes[0],
            count_bytes[1],
            count_bytes[2],
            count_bytes[3],
        ]) as usize;

        // Each run is 14 bytes: never loop past what the data holds.
        let remaining = data.len().saturating_sub(runs_start);
        let max_runs = remaining / 14;
        let clamped_count = run_count.min(max_runs);

        let mut rle = Rle::new();
        let mut offset = runs_start;
        for _ in 0..clamped_count {
            let Some(end) = offset.checked_add(14) else {
                break;
            };
            // Each run: [value:10][length:4].
            let Some(run_bytes) = data.get(offset..end) else {
                break;
            };
            if run_bytes.len() < 14 {
                break;
            }
            if let Some(value) = CellAttrs::deserialize(run_bytes) {
                let length = u32::from_le_bytes([
                    run_bytes[10],
                    run_bytes[11],
                    run_bytes[12],
                    run_bytes[13],
                ]);
                rle.extend_with(value, length);
            }
            offset = end;
        }
        Some((Some(rle), offset))
    }

    /// Deserialize v2 hyperlink spans (no IDs) starting at `offset`.
    ///
    /// Returns `None` if there are no hyperlinks or data is truncated.
    // Skip: same strict-UTF-8 decode-reader class as the v3 sibling below.
    #[cfg_attr(trust_verify, trust::skip)]
    fn deserialize_hyperlinks(data: &[u8], offset: usize) -> Option<SmallVec<HyperlinkSpan, 2>> {
        // Checked offsets and sub-slice reads carry the bounds proofs the strict L0
        // gate needs (see `deserialize_attrs`); `pushed` stands in for `spans.len()`,
        // whose MIR-inlined `SmallVec` internals fail its hardened-unsafe check.
        let header_end = offset.checked_add(2)?;
        let count_bytes = data.get(offset..header_end)?;
        if count_bytes.len() < 2 {
            return None;
        }
        let count = u16::from_le_bytes([count_bytes[0], count_bytes[1]]) as usize;
        if count == 0 {
            return None;
        }

        // A span has at least an 8-byte header: bound the reservation by the data.
        let remaining = data.len().saturating_sub(header_end);
        let max_spans = remaining / 8;
        let mut spans = SmallVec::with_capacity(count.min(max_spans).min(MAX_HYPERLINK_SPANS));
        let mut pushed = 0usize;
        let mut pos = header_end;
        for _ in 0..count {
            if pushed >= MAX_HYPERLINK_SPANS {
                break;
            }
            let Some(hdr_end) = pos.checked_add(8) else {
                break;
            };
            let Some(hdr) = data.get(pos..hdr_end) else {
                break;
            };
            if hdr.len() < 8 {
                break;
            }
            let start_col = u16::from_le_bytes([hdr[0], hdr[1]]);
            let end_col = u16::from_le_bytes([hdr[2], hdr[3]]);
            let url_len = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as usize;
            pos = hdr_end;
            let Some(url_end) = pos.checked_add(url_len) else {
                break;
            };
            let Some(url_bytes) = data.get(pos..url_end) else {
                break;
            };
            // Skip an over-cap URL (crafted; see MAX_HYPERLINK_URL_BYTES), stepping past it.
            if url_len <= MAX_HYPERLINK_URL_BYTES
                && let Ok(url) = std::str::from_utf8(url_bytes)
            {
                spans.push(HyperlinkSpan::new(start_col, end_col, Arc::from(url)));
                // saturating_add: bounded by the span count (a slice len).
                pushed = pushed.saturating_add(1);
            }
            pos = url_end;
        }
        if pushed == 0 { None } else { Some(spans) }
    }

    /// Deserialize v3 hyperlink spans (with OSC 8 IDs) starting at `offset`.
    ///
    /// Returns the spans (`None` if there are none or the data is truncated) and the
    /// offset just past the section, where a v4 underline-colour section starts
    /// (`data.len()` when truncated before the count).
    // Skip: strict `str::from_utf8` on decoded URL bytes (the hardened reject class;
    // a malformed URL takes the None arm). Round-trip tested.
    #[cfg_attr(trust_verify, trust::skip)]
    fn deserialize_hyperlinks_v3(
        data: &[u8],
        offset: usize,
    ) -> (Option<SmallVec<HyperlinkSpan, 2>>, usize) {
        // Shaped for the strict L0 gate as in `deserialize_hyperlinks`.
        let Some(header_end) = offset.checked_add(2) else {
            return (None, data.len());
        };
        let Some(count_bytes) = data.get(offset..header_end) else {
            return (None, data.len());
        };
        if count_bytes.len() < 2 {
            return (None, data.len());
        }
        let count = u16::from_le_bytes([count_bytes[0], count_bytes[1]]) as usize;
        if count == 0 {
            // No links; a v4 section starts right after the count.
            return (None, header_end);
        }

        // A v3 span has at least a 12-byte header: bound the reservation by the data.
        let remaining = data.len().saturating_sub(header_end);
        let max_spans = remaining / 12;
        let mut spans = SmallVec::with_capacity(count.min(max_spans).min(MAX_HYPERLINK_SPANS));
        let mut pushed = 0usize;
        let mut pos = header_end;
        for _ in 0..count {
            if pushed >= MAX_HYPERLINK_SPANS {
                break;
            }
            let Some(hdr_end) = pos.checked_add(8) else {
                break;
            };
            let Some(hdr) = data.get(pos..hdr_end) else {
                break;
            };
            if hdr.len() < 8 {
                break;
            }
            let start_col = u16::from_le_bytes([hdr[0], hdr[1]]);
            let end_col = u16::from_le_bytes([hdr[2], hdr[3]]);
            let url_len = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as usize;
            pos = hdr_end;
            let Some(url_end) = pos.checked_add(url_len) else {
                break;
            };
            let Some(url_bytes) = data.get(pos..url_end) else {
                break;
            };
            pos = url_end;

            // v3: id_len + id.
            let Some(id_hdr_end) = pos.checked_add(4) else {
                break;
            };
            let Some(id_hdr) = data.get(pos..id_hdr_end) else {
                break;
            };
            if id_hdr.len() < 4 {
                break;
            }
            let id_len = u32::from_le_bytes([id_hdr[0], id_hdr[1], id_hdr[2], id_hdr[3]]) as usize;
            pos = id_hdr_end;
            let id = if id_len > 0 {
                let Some(id_end) = pos.checked_add(id_len) else {
                    break;
                };
                let Some(id_bytes) = data.get(pos..id_end) else {
                    break;
                };
                // Drop an over-cap id (see MAX_HYPERLINK_ID_BYTES), still stepping past it.
                let id_str = if id_len <= MAX_HYPERLINK_ID_BYTES {
                    std::str::from_utf8(id_bytes).ok()
                } else {
                    None
                };
                pos = id_end;
                id_str.map(Arc::from)
            } else {
                None
            };

            // Skip an over-cap URL (crafted; see MAX_HYPERLINK_URL_BYTES). `pos` is already
            // past the url and id, so the next span stays framed.
            if url_len <= MAX_HYPERLINK_URL_BYTES
                && let Ok(url) = std::str::from_utf8(url_bytes)
            {
                spans.push(HyperlinkSpan::with_id(
                    start_col,
                    end_col,
                    Arc::from(url),
                    id,
                ));
                pushed += 1;
            }
        }
        let result = if pushed == 0 { None } else { Some(spans) };
        // `pos` is past the last consumed hyperlink byte: where a v4 section starts.
        (result, pos)
    }

    /// Deserialize the v4 underline-colour section at `offset`: `[count:2]` then
    /// `count` records `[start_col:2][end_col:2][color:4]`, `color` the packed
    /// `0xTT_XXXXXX` form. `None` when there are no spans or the data is truncated.
    // Skip: same SmallVec::with_capacity / PtrMetadata decode-reader class as the
    // hyperlink readers. Round-trip tested; length-prefixed.
    #[cfg_attr(trust_verify, trust::skip)]
    fn deserialize_underline_colors(
        data: &[u8],
        offset: usize,
    ) -> Option<SmallVec<UnderlineColorSpan, 2>> {
        // Shaped for the strict L0 gate as in `deserialize_hyperlinks`.
        let header_end = offset.checked_add(2)?;
        let count_bytes = data.get(offset..header_end)?;
        if count_bytes.len() < 2 {
            return None;
        }
        let count = u16::from_le_bytes([count_bytes[0], count_bytes[1]]) as usize;
        if count == 0 {
            return None;
        }

        // Each span is 8 bytes: never reserve more than the data holds.
        let remaining = data.len().saturating_sub(header_end);
        let max_spans = remaining / 8;
        let mut spans = SmallVec::with_capacity(count.min(max_spans).min(MAX_UNDERLINE_SPANS));
        let mut pushed = 0usize;
        let mut pos = header_end;
        for _ in 0..count {
            if pushed >= MAX_UNDERLINE_SPANS {
                break;
            }
            let Some(end) = pos.checked_add(8) else {
                break;
            };
            let Some(rec) = data.get(pos..end) else {
                break;
            };
            if rec.len() < 8 {
                break;
            }
            let start_col = u16::from_le_bytes([rec[0], rec[1]]);
            let end_col = u16::from_le_bytes([rec[2], rec[3]]);
            let color = u32::from_le_bytes([rec[4], rec[5], rec[6], rec[7]]);
            spans.push(UnderlineColorSpan::new(start_col, end_col, color));
            // saturating_add: bounded by the span count (a slice len).
            pushed = pushed.saturating_add(1);
            pos = end;
        }
        if pushed == 0 { None } else { Some(spans) }
    }

    /// Deserialize legacy format (without version byte or attrs).
    fn deserialize_legacy(data: &[u8]) -> Option<Self> {
        if data.len() < 5 {
            return None;
        }

        let flags = LineFlags::from_bits_truncate(data[0]);
        let len = u32::from_le_bytes([data[1], data[2], data[3], data[4]]) as usize;
        if len > MAX_LINE_CONTENT_BYTES {
            return None;
        }

        let end = 5usize.checked_add(len)?;
        // `get` rather than indexing: the spelling the strict L0 gate proves.
        let content = LineContent::from_bytes(data.get(5..end)?);
        Some(Self {
            content,
            attrs: None,
            flags,
            hyperlinks: None,
            underline_colors: None,
            images: None,
        })
    }
}

// Block-level serialization lives in line_codec_block.rs; it is public because
// `TerminalCheckpoint` (aterm-core) encodes both grid bodies with this codec.
pub(crate) use super::line_codec_block::{
    MAX_DECODE_PAGE_LINES, count_page_lines, serialize_pages,
};
pub use super::line_codec_block::{
    deserialize_lines, deserialize_lines_strict, deserialize_lines_strict_dropping_over_cap_links,
    deserialize_lines_tail_strict, deserialize_page_lines, max_linked_record_bytes,
    serialize_lines, serialize_lines_bounded,
};
