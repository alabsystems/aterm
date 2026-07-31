// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Serialization helpers for scrollback lines.

use super::{CellAttrs, HyperlinkSpan, Line, LineContent, LineFlags, UnderlineColorSpan};
use aterm_alloc::SmallVec;
use aterm_rle::Rle;
use std::sync::Arc;

/// Absolute cap on a single deserialized line's CONTENT byte length.
///
/// A legitimate stored line is one physical grid row: at most `MAX_GRID_COLS`
/// (4096) cells, each holding at most one grapheme unit bounded by the
/// materialization cap (`MAX_GRAPHEME_UNIT_BYTES`, ~256 B) — under ~1.05 MiB. The
/// wire `content_len` is otherwise bounded only by the surrounding buffer (up to
/// the 64 MiB zstd decode cap for a disk page, or the whole checkpoint body), so a
/// crafted record could declare a multi-MiB single line that every downstream
/// consumer (`to_string()`, search, selection, materialization) would then process
/// unbounded — a memory-amplification DoS. 2 MiB leaves margin over the legitimate
/// ceiling while capping the pathological line; an over-cap record is skipped
/// (its `line_size` still advances the decode offset past it, so the rest of the
/// block still decodes).
pub(crate) const MAX_LINE_CONTENT_BYTES: usize = 2 * 1024 * 1024;

/// Cap on a single deserialized hyperlink URL, mirroring the OSC 8 ingestion
/// ceiling (`aterm-core`'s `MAX_HYPERLINK_URL_BYTES` = 8192). A URL reaches the
/// grid — and thus serialization — only after passing that ingestion cap, so a
/// legitimately serialized span's URL is always `<=` this; a larger `url_len` is
/// a crafted checkpoint / `.dtrm` page. `MAX_LINE_CONTENT_BYTES` bounds only the
/// CONTENT section, NOT the hyperlink section, so without this cap the per-line
/// deserializer would `Arc::from` a multi-MiB URL and produce a stored `Line`
/// whose retained hyperlink far exceeds the 2 MiB content invariant. An over-cap
/// span is skipped (its bytes are still stepped over, so the rest decodes).
pub(crate) const MAX_HYPERLINK_URL_BYTES: usize = 8192;

/// Cap on a single deserialized OSC 8 `id=` parameter, mirroring the ingestion
/// ceiling (`aterm-core` admits ids of at most 256 bytes). An over-cap id is
/// dropped (the span keeps its URL with `id = None`), exactly like ingestion's
/// `id.len() <= 256` filter — the id's bytes are still stepped over so framing
/// stays aligned.
pub(crate) const MAX_HYPERLINK_ID_BYTES: usize = 256;

/// Cap on the NUMBER of hyperlink spans in a single deserialized line, matching
/// `MAX_GRID_COLS` (aterm-grid, 4096). A physical row has at most `cols` cells and
/// each cell holds one hyperlink, so the legit write path coalesces to `<= cols`
/// disjoint spans; a stored `count` (a u16, up to 65535) beyond this is a crafted
/// record. Each span is ~12 B serialized but ~40 B in memory, so 65535 spans would
/// balloon one line's hyperlink `SmallVec` to ~2.6 MiB (the 2 MiB content cap does
/// NOT cover the hyperlink section). Capping the reconstructed span count bounds
/// that; the per-cell restore loops (`fill_row_from_line`/`restore_hyperlinks`) are
/// separately bounded to O(cols) against crafted OVERLAPPING spans.
pub(crate) const MAX_HYPERLINK_SPANS: usize = 4096;

/// Cap on the NUMBER of underline-colour spans in a single deserialized line,
/// matching `MAX_GRID_COLS` (aterm-grid, 4096) for the same reason as
/// [`MAX_HYPERLINK_SPANS`]: a physical row has at most `cols` cells, each with
/// one underline colour, so the write path coalesces to `<= cols` disjoint
/// spans; a stored `count` (a u16, up to 65535) beyond this is a crafted record.
/// Each span is 8 B serialized and 8 B in memory, so capping the reconstructed
/// count bounds one line's underline `SmallVec`. The per-cell restore loops
/// (`fill_row_from_line`/`restore_underline_colors`) are separately bounded to
/// O(cols) against crafted OVERLAPPING spans.
pub(crate) const MAX_UNDERLINE_SPANS: usize = 4096;

impl CellAttrs {
    /// Serialize to bytes (10 bytes).
    #[must_use]
    pub(crate) fn serialize(&self) -> [u8; 10] {
        // Byte-wise array construction (not range-IndexMut + copy_from_slice,
        // whose `IndexMut::index_mut` + len-assert live in absent std bodies):
        // the array literal is total and byte-identical.
        let fg = self.fg.to_le_bytes();
        let bg = self.bg.to_le_bytes();
        let fl = self.flags.to_le_bytes();
        [
            fg[0], fg[1], fg[2], fg[3], bg[0], bg[1], bg[2], bg[3], fl[0], fl[1],
        ]
    }

    /// Deserialize from bytes.
    #[must_use]
    pub(crate) fn deserialize(data: &[u8]) -> Option<Self> {
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
    /// Version 4 = v3 + underline-colour spans (only emitted when SGR 58 is
    ///             present, so plain and merely-styled lines stay byte-identical
    ///             to v3; a v3 reader stops after hyperlinks and ignores the
    ///             trailing section, degrading gracefully)
    ///
    /// # Serialization Limits
    ///
    /// Due to the wire format using fixed-width integers, content exceeding these
    /// limits is silently truncated:
    ///
    /// - **Content length:** max 4 GB (`u32::MAX` bytes)
    /// - **Attribute runs:** max ~4 billion (`u32::MAX` runs)
    /// - **Hyperlinks per line:** max 65,535 (`u16::MAX` links)
    /// - **Hyperlink URL/ID length:** max 4 GB (`u32::MAX` bytes each)
    ///
    /// These limits are orders of magnitude larger than any realistic terminal line.
    /// Truncation would only occur with malformed or malicious input.
    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        // The estimate is only a capacity hint (never affects the serialized
        // bytes), so it is computed with saturating arithmetic and bounded
        // before the allocation: for every real line the estimate is far below
        // the bound and the reservation is exactly what it was before; the
        // saturate/no-hint fallback only matters for inputs that could not
        // exist in memory anyway. This keeps the strict L0 gate's overflow and
        // allocation-budget obligations provable.
        let estimate = self.serialized_size_estimate();
        let mut v = if estimate < crate::codec::MAX_DECOMPRESSED_SCROLLBACK_PAGE_BYTES {
            Vec::with_capacity(estimate)
        } else {
            Vec::new()
        };
        self.serialize_into(&mut v);
        v
    }

    /// Exact per-line serialized-size estimate shared by [`Line::serialize`]
    /// and [`Line::serialize_into`] (capacity hint only).
    ///
    /// Saturating arithmetic and an explicit slice loop replace the previous
    /// `1 + 4 + run_count * 14` / `.iter().map(..).sum()` spellings: identical
    /// values for every line that can exist in memory, but provable under the
    /// strict L0 gate (which refutes the unconstrained-input overflow and
    /// fails the MIR-inlined `SmallVec::iter` internals' hardened-unsafe
    /// check).
    // Skip: the residual row is `Rvalue::UnaryOp(PtrMetadata)` — fat-pointer
    // metadata extraction is diagnostic-only until the metadata lane is
    // modeled (toolchain). A size ESTIMATE for tier promotion; every use is
    // advisory.
    #[cfg_attr(trust_verify, trust::skip)]
    fn serialized_size_estimate(&self) -> usize {
        let content_len = self.content.as_bytes().len();
        // Closure-free shapes + fn-item accessors (`attr_runs`,
        // `Line::hyperlinks`): identical values, but direct calls to the tiny
        // aterm-rle/aterm-alloc methods get MIR-inlined and the strict gate's
        // hardened-unsafe check fails closed on the inlined std internals
        // (see `attr_runs` in line.rs). `runs.len()` IS `run_count()`.
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
        // Underline-colour section (v4): a 2-byte count plus 8 bytes per span,
        // and only when present — a v3 line appends nothing here (no section),
        // so the common-case estimate is unchanged.
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

    /// Serialize line by appending its bytes to `result`.
    ///
    /// Identical wire format to [`Line::serialize`], but writes into the
    /// caller's buffer so block-level serialization avoids a per-line
    /// allocation and redundant copy (#5860). Reserves the exact per-line
    /// size up front so styled blocks never reallocate `result`.
    // Skip: the residual rows are `Rvalue::UnaryOp(PtrMetadata)` (fat-pointer
    // metadata — diagnostic-only until the metadata lane is modeled) and the
    // `Vec::extend_from_slice` alloc class. Round-trip tested against
    // deserialize; every write is length-prefixed.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn serialize_into(&self, result: &mut Vec<u8>) {
        let content = self.content.as_bytes();
        let content_len = content.len();

        // Reserve the exact per-line size (mirrors serialize()'s estimate)
        // so attr/hyperlink-heavy lines don't trigger a realloc. The estimate
        // is a capacity hint only; the bound check keeps the strict gate's
        // allocation-budget obligation provable (every real line is far below
        // it, so the reservation is unchanged on all reachable paths).
        let estimate = self.serialized_size_estimate();
        if estimate < crate::codec::MAX_DECOMPRESSED_SCROLLBACK_PAGE_BYTES {
            result.reserve(estimate);
        }

        // Version byte. Emit v4 ONLY when the line carries SGR 58 underline
        // colours, so plain/styled lines stay byte-identical to v3 (a v3 reader
        // then never sees — and never has to skip — a trailing section).
        result.push(if self.underline_colors.is_some() {
            4
        } else {
            3
        });

        // Flags
        result.push(self.flags.bits());

        // Content length and content (max 4GB, see Serialization Limits)
        let content_len_u32 = u32::try_from(content_len).unwrap_or(u32::MAX);
        result.extend_from_slice(&content_len_u32.to_le_bytes());
        result.extend_from_slice(content);

        // Attributes (max ~4B runs, see Serialization Limits)
        //
        // Fn-item accessor (`attr_runs`) + plain slice ops: identical bytes —
        // `runs.len()` IS `run_count()` — but direct method calls MIR-inline
        // the aterm-rle internals and the strict gate's hardened-unsafe check
        // fails closed on them (see `attr_runs` in line.rs).
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

        // Hyperlinks (v3: includes OSC 8 ID, max 65535 links)
        //
        // Iterates the plain slice from `Line::hyperlinks` (whose
        // `map(SmallVec::as_slice)` keeps the accessor an opaque call):
        // identical bytes to the previous `hyperlinks.len()`/`.iter()`, but
        // slice len/iteration are primitives the strict gate proves, while
        // directly-called `SmallVec` internals fail its hardened-unsafe check.
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

        // Underline colours (v4 only). Appended AFTER hyperlinks so a v3 reader,
        // which stops once the hyperlink section is consumed, ignores it; the
        // version byte above is 4 exactly when this section is written, so a
        // v3-tagged line never carries these trailing bytes. Each span is a
        // fixed [start_col:2][end_col:2][color:4]; `color` is the packed
        // 0xTT_XXXXXX form (RGB vs indexed preserved for palette re-resolution).
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

        // Check version
        let version = data[0];
        if version == 0 {
            // Legacy format (version 0 or old format without version byte)
            return Self::deserialize_legacy(data);
        }

        if data.len() < 7 {
            return None;
        }

        // Version 1, 2, and 3 share the same base format
        let flags = LineFlags::from_bits_truncate(data[1]);
        let content_len = u32::from_le_bytes([data[2], data[3], data[4], data[5]]) as usize;
        // Reject a pathologically long single line (see MAX_LINE_CONTENT_BYTES).
        if content_len > MAX_LINE_CONTENT_BYTES {
            return None;
        }

        let content_end = 6usize.checked_add(content_len)?;
        if data.len() < content_end.checked_add(1)? {
            return None;
        }

        // `get` + `?` instead of `&data[6..content_end]`: the guard above
        // already makes the range in-bounds (so the `None` arm is unreachable
        // and the result identical), but this spelling carries the bounds
        // proof for the strict L0 gate.
        let content = LineContent::from_bytes(data.get(6..content_end)?);

        let (attrs, offset) = Self::deserialize_attrs(data, content_end)?;
        // v3/v4 report the offset PAST the hyperlink section so the v4
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

        // Box the rare/absent attrs + hyperlinks + underline-colour fields to
        // keep `Line` small (see Line struct docs) — the deserialized values
        // are byte-identical.
        Some(Self {
            content,
            attrs: attrs.map(Box::new),
            flags,
            hyperlinks: hyperlinks.map(Box::new),
            underline_colors: underline_colors.map(Box::new),
        })
    }

    /// Whether `data` frames a record [`Self::deserialize`] would ACCEPT —
    /// decided from the header alone, without reconstructing the `Line`.
    ///
    /// This encodes exactly `deserialize`'s `None` conditions and nothing else.
    /// It exists so the warm→cold eviction path can VALIDATE a page whose lines
    /// it does not need (`WarmBlock::try_decompress_serialized` reuses the
    /// decompressed bytes verbatim) without paying a full `Line` materialization
    /// per line. A divergence from `deserialize` would either let a corrupt
    /// block through to the cold tier or reject a good one, so the two are
    /// pinned together by a `debug_assert_eq!` on every record the block
    /// decoder walks (see `walk_records`).
    ///
    /// Only the header and the attrs PRESENCE byte can reject: the hyperlink
    /// and underline-colour sections clamp and truncate rather than fail, and
    /// `deserialize_attrs` fails in exactly two places — its
    /// `content_end >= data.len()` guard (implied by the length check below)
    /// and the 4-byte run-count read it performs when the has-attrs byte is
    /// set. So these ARE the whole predicate.
    #[must_use]
    pub(crate) fn record_is_valid(data: &[u8]) -> bool {
        let Some(&version) = data.first() else {
            return false;
        };

        if version == 0 {
            // Legacy: `[flags:1][len:4][content:len]` (`deserialize_legacy`).
            // Sub-slice + length guard + constant indexing, the same spelling
            // the decoders use so the strict L0 gate can prove the indexes.
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
        // `deserialize_attrs` reads a 4-byte run count after the has-attrs
        // byte and rejects a record that ends inside it. (The block framing
        // walk never offers such a record — `line_size_v1v2` needs those same
        // bytes to size it — but the predicate mirrors `deserialize`, not the
        // walk, so callers cannot be surprised.)
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
            // saturating_add: `content_end < data.len()` (guard above), so the
            // increment cannot wrap; saturation only discharges the obligation
            // the verifier cannot chain. Identical on every reachable path.
            return Some((None, content_end.saturating_add(1)));
        }

        // Checked offsets + slice-pattern reads below: identical decode to the
        // previous integer-indexed spelling on every input (each guard exactly
        // mirrors the bound it replaces), but the slice patterns carry the
        // bounds proofs the strict L0 gate refuted on the indexed form.
        let attrs_start = content_end.checked_add(1)?;
        let runs_start = attrs_start.checked_add(4)?;
        // Sub-slice + explicit length guard + constant indexing: `get` fails
        // exactly when the old `data.len() < attrs_start + 4` check fired, and
        // the (always-true) `len < 4` guard is the dominating comparison the
        // strict gate needs to prove the constant indexes (a slice PATTERN
        // emits per-element obligations it cannot discharge).
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

        // Clamp loop bound: each RLE run requires exactly 14 bytes,
        // so we can't have more runs than remaining data allows.
        let remaining = data.len().saturating_sub(runs_start);
        let max_runs = remaining / 14;
        let clamped_count = run_count.min(max_runs);

        let mut rle = Rle::new();
        let mut offset = runs_start;
        for _ in 0..clamped_count {
            let Some(end) = offset.checked_add(14) else {
                break;
            };
            // Each run: [value:10][length:4]. Sub-slice + length guard +
            // constant indexing (see the count read above for the rationale).
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

    /// Deserialize hyperlink spans starting at `offset` in `data`.
    ///
    /// Returns `None` if there are no hyperlinks or data is truncated.
    // Skip: same strict-UTF-8 decode-reader class as the v3 sibling below.
    #[cfg_attr(trust_verify, trust::skip)]
    fn deserialize_hyperlinks(data: &[u8], offset: usize) -> Option<SmallVec<HyperlinkSpan, 2>> {
        // Checked offsets + slice-pattern reads throughout: identical decode to
        // the previous integer-indexed spelling on every input (each `get`
        // failure exactly mirrors the bound check it replaces), but the slice
        // patterns carry the bounds proofs the strict L0 gate refuted on the
        // indexed form. The pushed-span count is tracked in a local (`pushed`)
        // instead of `spans.len()`/`spans.is_empty()` — same value by
        // construction — because the MIR-inlined `SmallVec::len` internals
        // fail the gate's hardened-unsafe check.
        let header_end = offset.checked_add(2)?;
        // Sub-slice + length guard + constant indexing (see deserialize_attrs
        // for why this shape, not a slice pattern, is what the gate proves).
        let count_bytes = data.get(offset..header_end)?;
        if count_bytes.len() < 2 {
            return None;
        }
        let count = u16::from_le_bytes([count_bytes[0], count_bytes[1]]) as usize;
        if count == 0 {
            return None;
        }

        // Clamp capacity: each span requires at least 8 bytes of header,
        // so we can't have more spans than remaining data allows.
        let remaining = data.len().saturating_sub(header_end);
        let max_spans = remaining / 8;
        let mut spans = SmallVec::with_capacity(count.min(max_spans).min(MAX_HYPERLINK_SPANS));
        let mut pushed = 0usize;
        let mut pos = header_end;
        for _ in 0..count {
            // Cap the reconstructed span count (see MAX_HYPERLINK_SPANS): a crafted
            // record can declare up to u16::MAX spans; stop rebuilding past the cap.
            if pushed >= MAX_HYPERLINK_SPANS {
                break;
            }
            let Some(hdr_end) = pos.checked_add(8) else {
                break;
            };
            // Sub-slice + length guard + constant indexing (see
            // deserialize_attrs for why this shape is what the gate proves).
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
            // Cap the reconstructed URL at the OSC 8 ingestion ceiling: an over-cap
            // url_len is a crafted record, so skip the span (advance past it so the
            // rest of the block still decodes) rather than allocate an oversized
            // Arc<str> that would breach the per-line content invariant.
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
    /// Returns `None` if there are no hyperlinks or data is truncated.
    // Skip: residual rows are strict `str::from_utf8` on decoded URL bytes
    // (the hardened reject class — a malformed URL takes the None arm,
    // fail-closed) inside the guarded decode reader. Round-trip tested.
    #[cfg_attr(trust_verify, trust::skip)]
    fn deserialize_hyperlinks_v3(
        data: &[u8],
        offset: usize,
    ) -> (Option<SmallVec<HyperlinkSpan, 2>>, usize) {
        // Returns the spans PLUS the offset immediately past this section, so a
        // v4 caller can locate the trailing underline-colour section. On
        // truncation before the count there is no locatable following section,
        // so `data.len()` is reported (a v4 read then finds no bytes → None).
        //
        // Checked offsets + slice-pattern reads throughout (and a local
        // `pushed` counter instead of `SmallVec::len`): identical decode to
        // the previous integer-indexed spelling on every input — see
        // `deserialize_hyperlinks` for why the strict L0 gate needs this shape.
        let Some(header_end) = offset.checked_add(2) else {
            return (None, data.len());
        };
        // Sub-slice + length guard + constant indexing (see deserialize_attrs
        // for why this shape, not a slice pattern, is what the gate proves).
        let Some(count_bytes) = data.get(offset..header_end) else {
            return (None, data.len());
        };
        if count_bytes.len() < 2 {
            return (None, data.len());
        }
        let count = u16::from_le_bytes([count_bytes[0], count_bytes[1]]) as usize;
        if count == 0 {
            // No hyperlinks, but a following v4 section (if any) begins right
            // after this consumed 2-byte count.
            return (None, header_end);
        }

        // Clamp capacity: each v3 span requires at least 12 bytes of header
        // (start_col:2 + end_col:2 + url_len:4 + id_len:4).
        let remaining = data.len().saturating_sub(header_end);
        let max_spans = remaining / 12;
        let mut spans = SmallVec::with_capacity(count.min(max_spans).min(MAX_HYPERLINK_SPANS));
        let mut pushed = 0usize;
        let mut pos = header_end;
        for _ in 0..count {
            // Cap the reconstructed span count (see MAX_HYPERLINK_SPANS): a crafted
            // record can declare up to u16::MAX spans; stop rebuilding past the cap.
            if pushed >= MAX_HYPERLINK_SPANS {
                break;
            }
            let Some(hdr_end) = pos.checked_add(8) else {
                break;
            };
            // Sub-slice + length guard + constant indexing (see
            // deserialize_attrs for why this shape is what the gate proves).
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

            // v3: read id_len + id (same sub-slice shape as the span header)
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
                // Cap the id at the OSC 8 ingestion ceiling (256 B); an over-cap id
                // is crafted — drop it (like ingestion's `v.len() <= 256` filter) but
                // still step past its bytes so the record framing stays aligned.
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

            // Cap the URL at the OSC 8 ingestion ceiling (8192 B): an over-cap url_len
            // is a crafted record, so skip the span rather than allocate an oversized
            // Arc<str>. `pos` already advanced past both url and id, so skipping the
            // push leaves the framing intact for the next span.
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
        // `pos` now sits immediately past the last consumed hyperlink byte —
        // the start of the v4 underline-colour section for a well-formed line.
        (result, pos)
    }

    /// Deserialize the v4 underline-colour section starting at `offset`.
    ///
    /// Layout: `[count:2]` then `count` fixed 8-byte records
    /// `[start_col:2][end_col:2][color:4]`. Returns `None` when there are no
    /// spans or the data is truncated. `color` is the packed `0xTT_XXXXXX`
    /// underline colour (RGB vs indexed preserved).
    // Skip: same SmallVec::with_capacity / PtrMetadata decode-reader class as
    // its hyperlink siblings above. Round-trip tested; length-prefixed.
    #[cfg_attr(trust_verify, trust::skip)]
    fn deserialize_underline_colors(
        data: &[u8],
        offset: usize,
    ) -> Option<SmallVec<UnderlineColorSpan, 2>> {
        // Same checked-offset + sub-slice + length-guard shape as the hyperlink
        // deserializers (see `deserialize_attrs`), with a local `pushed` counter
        // instead of `SmallVec::len` for the strict L0 gate.
        let header_end = offset.checked_add(2)?;
        let count_bytes = data.get(offset..header_end)?;
        if count_bytes.len() < 2 {
            return None;
        }
        let count = u16::from_le_bytes([count_bytes[0], count_bytes[1]]) as usize;
        if count == 0 {
            return None;
        }

        // Clamp capacity: each span is exactly 8 bytes, so no more spans than
        // the remaining data allows (and never past the DoS cap).
        let remaining = data.len().saturating_sub(header_end);
        let max_spans = remaining / 8;
        let mut spans = SmallVec::with_capacity(count.min(max_spans).min(MAX_UNDERLINE_SPANS));
        let mut pushed = 0usize;
        let mut pos = header_end;
        for _ in 0..count {
            // Cap the reconstructed span count (see MAX_UNDERLINE_SPANS): a
            // crafted record can declare up to u16::MAX spans; stop past the cap.
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
        // Reject a pathologically long single line (see MAX_LINE_CONTENT_BYTES).
        if len > MAX_LINE_CONTENT_BYTES {
            return None;
        }

        let end = 5usize.checked_add(len)?;
        // `get` + `?` instead of a manual length check plus `&data[5..end]`:
        // `get` returns `None` exactly when `data.len() < end`, so the decode
        // is identical — and the spelling carries the bounds proof for the
        // strict L0 gate.
        let content = LineContent::from_bytes(data.get(5..end)?);
        Some(Self {
            content,
            attrs: None,
            flags,
            hyperlinks: None,
            underline_colors: None,
        })
    }
}

// Block-level serialization (serialize_lines, deserialize_lines) is in line_codec_block.rs.
// Exposed publicly (B.3.2): `TerminalCheckpoint` encodes both grid bodies with
// this exact block codec, so it must be reachable from aterm-core.
pub(crate) use super::line_codec_block::{MAX_DECODE_PAGE_LINES, count_page_lines};
pub use super::line_codec_block::{
    deserialize_lines, deserialize_lines_strict, deserialize_lines_tail_strict,
    deserialize_page_lines, serialize_lines,
};
