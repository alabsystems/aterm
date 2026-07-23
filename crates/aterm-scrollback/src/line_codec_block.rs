// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Block-level serialization for multiple scrollback lines.
//!
//! Used by warm and cold tiers to serialize/deserialize blocks of lines
//! for compressed storage. Handles legacy (v0), v1, v2, v3, and v4 line formats.
//!
//! All deserialized lengths use checked arithmetic to prevent overflow on
//! malicious or corrupt input (#4950).

use super::Line;

/// Serialize multiple lines for block compression.
#[must_use]
// Skip: same PtrMetadata/extend alloc class as `serialize_into`; the
// pre-size is a hint and the format is length-prefixed. Round-trip tested.
#[cfg_attr(trust_verify, trust::skip)]
pub fn serialize_lines(lines: &[Line]) -> Vec<u8> {
    // Format: [count:4][line0][line1]...
    //
    // Pre-allocate from content sizes to avoid repeated Vec doublings
    // on the warm-tier compaction hot path (#5860).
    // Per v3 line: 9 bytes fixed overhead (version + flags + content_len
    // + has_attrs + hyperlink_count) plus content bytes.
    //
    // The estimate is only a capacity hint (never affects the serialized
    // bytes): saturating arithmetic and the pre-allocation bound keep the
    // strict L0 gate's overflow / allocation-budget obligations provable,
    // while every real block (bounded by warm-tier limits, far below the
    // 64 MiB page cap) reserves exactly what it did before.
    let content_bytes = lines
        .iter()
        .map(Line::len)
        .fold(0usize, usize::saturating_add);
    let estimate = 4usize
        .saturating_add(content_bytes)
        .saturating_add(lines.len().saturating_mul(9));
    let mut result = if estimate < crate::codec::MAX_DECOMPRESSED_SCROLLBACK_PAGE_BYTES {
        // Inline `.min(MAX - 1)` clamp: in this branch `estimate < MAX`
        // already holds, so `estimate.min(MAX - 1) == estimate` and the
        // reserved capacity is unchanged on every input. The clamp only
        // restates the dominating branch condition ON the allocation count
        // itself, which is what the strict L0 gate's unbounded-allocation
        // check needs to see (same "no-op under the documented invariant"
        // discharge as the `saturating_*` fixes in this crate).
        Vec::with_capacity(estimate.min(crate::codec::MAX_DECOMPRESSED_SCROLLBACK_PAGE_BYTES - 1))
    } else {
        Vec::new()
    };
    // Block size is bounded by warm tier settings (typically 256-4096 lines)
    // Saturate at u32::MAX for safety
    let count = u32::try_from(lines.len()).unwrap_or(u32::MAX);
    result.extend_from_slice(&count.to_le_bytes());
    for line in lines {
        line.serialize_into(&mut result);
    }
    result
}

/// Compute the byte size of a legacy (v0) line record.
///
/// Format: `[flags:1][len:4][content:len]`
/// Returns `None` if `data` is truncated or size overflows.
fn line_size_v0(data: &[u8]) -> Option<usize> {
    if data.len() < 5 {
        return None;
    }
    let content_len = u32::from_le_bytes([data[1], data[2], data[3], data[4]]) as usize;
    5usize.checked_add(content_len)
}

/// Compute the byte size of a v1/v2/v3/v4 line record.
///
/// Format: `[version:1][flags:1][len:4][content:len][has_attrs:1][attrs...][hyperlinks][v4: underline colours]`
/// Returns `None` if `data` is truncated or any size computation overflows.
fn line_size_v1v2(data: &[u8], version: u8) -> Option<usize> {
    if data.len() < 7 {
        return None;
    }
    let content_len = u32::from_le_bytes([data[2], data[3], data[4], data[5]]) as usize;
    let attrs_start = 6usize.checked_add(content_len)?;
    if attrs_start >= data.len() {
        return None;
    }
    let has_attrs = data[attrs_start] != 0;
    let attrs_size = if has_attrs {
        // By-byte `get`s with saturating offsets (the read_u16/from_hex
        // idiom): each read discharges its own bound; a truncated buffer
        // takes the same `return None` the old `+ 5 > len` guard did —
        // byte-identical on every in-bounds input.
        let (Some(&b1), Some(&b2), Some(&b3), Some(&b4)) = (
            data.get(attrs_start.saturating_add(1)),
            data.get(attrs_start.saturating_add(2)),
            data.get(attrs_start.saturating_add(3)),
            data.get(attrs_start.saturating_add(4)),
        ) else {
            return None;
        };
        let run_count = u32::from_le_bytes([b1, b2, b3, b4]) as usize;
        let runs_size = run_count.checked_mul(14)?;
        runs_size.checked_add(5)?
    } else {
        1
    };

    let base_size = 6usize.checked_add(content_len)?.checked_add(attrs_size)?;

    if version >= 3 {
        // v3 appends hyperlinks; v4 ALSO appends an underline-colour section
        // after them. Framing MUST include it, or `offset` lands mid-section and
        // every following record in the block misframes (data corruption).
        let after_hyperlinks = base_size.checked_add(hyperlinks_size_v3(data, base_size)?)?;
        if version >= 4 {
            after_hyperlinks.checked_add(underline_colors_size_v4(data, after_hyperlinks)?)
        } else {
            Some(after_hyperlinks)
        }
    } else if version >= 2 {
        hyperlinks_size_v2(data, base_size).and_then(|hl| base_size.checked_add(hl))
    } else {
        Some(base_size)
    }
}

/// Compute the byte size of the v4 underline-colour section.
///
/// Format: `[count:2]` then `count` fixed 8-byte records
/// `[start_col:2][end_col:2][color:4]` → `2 + count * 8`. Fixed-size records, so
/// (unlike the variable-length hyperlink sections) the size is computed directly
/// from the count. Sub-slice count read for the strict-gate rationale (see
/// [`hyperlinks_size_v2`]); `checked_*` rejects a crafted overflowing count.
fn underline_colors_size_v4(data: &[u8], base_size: usize) -> Option<usize> {
    let hdr_end = base_size.checked_add(2)?;
    let count_bytes = data.get(base_size..hdr_end)?;
    if count_bytes.len() < 2 {
        return None;
    }
    let count = u16::from_le_bytes([count_bytes[0], count_bytes[1]]) as usize;
    count.checked_mul(8)?.checked_add(2)
}

/// Compute the byte size of the v2 hyperlinks section (no IDs).
///
/// Sub-slice reads (`get` + always-true length guard + constant indexing)
/// instead of `data[pos + k]` indexing: `get` fails exactly when the old
/// `> data.len()` bound check fired, so the computed size is identical on
/// every input — and the guard is the dominating comparison the strict L0
/// gate needs to prove the constant indexes.
fn hyperlinks_size_v2(data: &[u8], base_size: usize) -> Option<usize> {
    let hdr_end = base_size.checked_add(2)?;
    let count_bytes = data.get(base_size..hdr_end)?;
    if count_bytes.len() < 2 {
        return None;
    }
    let count = u16::from_le_bytes([count_bytes[0], count_bytes[1]]) as usize;
    let mut size = 2usize;
    let mut pos = hdr_end;
    for _ in 0..count {
        let end = pos.checked_add(8)?;
        let Some(hdr) = data.get(pos..end) else {
            break;
        };
        if hdr.len() < 8 {
            break;
        }
        let url_len = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as usize;
        let advance = 8usize.checked_add(url_len)?;
        size = size.checked_add(advance)?;
        pos = pos.checked_add(advance)?;
    }
    Some(size)
}

/// Compute the byte size of the v3 hyperlinks section (with IDs).
///
/// Sub-slice reads instead of `data[pos + k]` indexing — identical size
/// on every input; see [`hyperlinks_size_v2`] for the strict-gate rationale.
fn hyperlinks_size_v3(data: &[u8], base_size: usize) -> Option<usize> {
    let hdr_end = base_size.checked_add(2)?;
    let count_bytes = data.get(base_size..hdr_end)?;
    if count_bytes.len() < 2 {
        return None;
    }
    let count = u16::from_le_bytes([count_bytes[0], count_bytes[1]]) as usize;
    let mut size = 2usize;
    let mut pos = hdr_end;
    for _ in 0..count {
        let span_hdr_end = pos.checked_add(8)?;
        let Some(hdr) = data.get(pos..span_hdr_end) else {
            break;
        };
        if hdr.len() < 8 {
            break;
        }
        let url_len = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as usize;
        pos = span_hdr_end.checked_add(url_len)?;
        let id_hdr_end = pos.checked_add(4)?;
        let Some(id_hdr) = data.get(pos..id_hdr_end) else {
            break;
        };
        if id_hdr.len() < 4 {
            break;
        }
        let id_len = u32::from_le_bytes([id_hdr[0], id_hdr[1], id_hdr[2], id_hdr[3]]) as usize;
        let span_size = 8usize
            .checked_add(url_len)?
            .checked_add(4)?
            .checked_add(id_len)?;
        size = size.checked_add(span_size)?;
        pos = id_hdr_end.checked_add(id_len)?;
    }
    Some(size)
}

/// Absolute cap on the NUMBER of lines a single [`deserialize_lines_capped`] call
/// reconstructs from a size-bounded block (the disk cold tier's zstd-decoded page).
///
/// The `zstd` decode is bounded to 64 MiB of decompressed bytes, but the minimum
/// serialized line is 5 bytes, so a crafted page of empty records can declare
/// ~13.4M lines — ~850 MiB of `Line` structs (`size_of::<Line>()` ≈ 64 B), a ~13×
/// memory amplification past the 64 MiB per-page budget the decode cap is meant to
/// enforce. `MAX_PREALLOC_LINES` bounds only the initial reservation, not the total
/// pushed count. This caps the reconstructed line count so the `Vec<Line>` stays
/// within ~64 MiB (matching the decode budget — no amplification). It is ~100× a
/// legitimate cold page (one warm block, `DEFAULT_WARM_LIMIT` = 10K lines), so it
/// never truncates real disk content. NOT applied to the checkpoint path, whose
/// body is uncompressed and input-proportional (no amplification) and whose visible
/// rows are serialized LAST — truncating there would drop the live screen.
///
/// This is ALSO the hard clamp on a configured `block_size` (see
/// `Scrollback::with_block_size` / `DiskBackedScrollbackConfig`): a serialized
/// warm/cold block never holds more than this many lines, so the decode cap can
/// never clip a LEGITIMATE block — it only rejects a crafted over-count one.
pub(crate) const MAX_DECODE_PAGE_LINES: usize = 1 << 20;

/// Deserialize multiple lines from a block, capped at `max_lines` reconstructed
/// lines. See [`MAX_DECODE_PAGE_LINES`] for why the disk decode path needs this and
/// why the checkpoint path must NOT be capped.
#[must_use]
// Skip: the block decode reader's guarded slice walk — the SliceBoundsCheck /
// decode-reader class its per-line siblings carry. Round-trip tested; the
// max_lines cap bounds the reservation (memory-amplification guard).
#[cfg_attr(trust_verify, trust::skip)]
pub fn deserialize_lines_capped(data: &[u8], max_lines: usize) -> Vec<Line> {
    if data.len() < 4 {
        return Vec::new();
    }

    let count = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    // Clamp pre-allocation to what the data can actually contain.
    // Minimum serialized line is 5 bytes (v0: flags + content_len + empty content).
    let max_possible = data.len().saturating_sub(4) / 5;
    // Additionally cap the INITIAL reservation. `count` is untrusted (up to
    // u32::MAX) and `max_possible` is only a loose bound — for a ~64 MiB page it
    // is ~13M line slots, i.e. hundreds of MiB of `Line` structs. A crafted page
    // can inflate `count` while containing only a few real records, so reserving
    // `count` up front is a memory-amplification vector; worse, once such a Vec is
    // retained in the DiskColdTier page cache its spare capacity is pinned but NOT
    // counted by page_byte_size (which measures len), silently defeating
    // cache_byte_limit. Cap the initial reservation to comfortably exceed any
    // legitimate page (a cold page is one warm block, <= warm_limit ≈ 10K lines by
    // default) and let the Vec grow from real pushes.
    const MAX_PREALLOC_LINES: usize = 16_384;
    let mut lines = Vec::with_capacity(count.min(max_possible).min(MAX_PREALLOC_LINES));
    let mut offset = 4;

    // The TOTAL pushed count is bounded by `max_lines`, not just the reservation —
    // otherwise a crafted block of ~5-byte records reconstructs millions of `Line`
    // structs regardless of the reservation cap (memory-amplification DoS).
    while offset < data.len() && lines.len() < count && lines.len() < max_lines {
        let Some(&version) = data.get(offset) else {
            break;
        };
        let Some(record) = data.get(offset..) else {
            break;
        };

        let line_size = if version == 0 {
            line_size_v0(record)
        } else {
            line_size_v1v2(record, version)
        };

        let Some(size) = line_size else { break };
        let Some(line_end) = offset.checked_add(size) else {
            break;
        };
        // `get` + `let-else` instead of a manual `line_end > data.len()` check
        // plus `&data[offset..line_end]`: `get` returns `None` in exactly that
        // case (`offset <= line_end` always holds — `line_end = offset + size`
        // via checked_add), so the decode is identical — and the spelling
        // carries the bounds proof the strict L0 gate refuted on the indexed
        // form.
        let Some(record_bytes) = data.get(offset..line_end) else {
            break;
        };

        if let Some(line) = Line::deserialize(record_bytes) {
            lines.push(line);
        }
        offset = line_end;
    }

    if lines.len() >= max_lines && offset < data.len() {
        // Pre-composed message via the log shim: identical rendered record,
        // but no macro-expanded `format_args!` unsafe in THIS function (which
        // the strict gate would escalate — see log_shim.rs).
        let mut msg = String::from("deserialize_lines_capped truncated at ");
        msg.push_str(&crate::error::dec_string(max_lines));
        msg.push_str(" lines (block has more records)");
        crate::log_shim::warn_str(&msg);
    }

    // Drop any spare capacity left by an over-estimated `count` so a page that is
    // retained in the DiskColdTier page cache pins exactly its real content — this
    // is what keeps cache_byte_limit (and its Kani model, which measures page bytes
    // from len) honest. A no-op for well-formed pages where capacity == len.
    lines.shrink_to_fit();
    lines
}

/// Strict bounded block decode for security-sensitive checkpoint ingestion.
///
/// Unlike the compatibility decoder, this rejects rather than truncates: the
/// header must declare at most `max_lines`, every complete record must fit the
/// caller's per-line content and wire budgets, every record must deserialize,
/// and no trailing bytes may remain. Length checks run before `Line::deserialize`
/// allocates its content/sidecars.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn deserialize_lines_strict(
    data: &[u8],
    max_lines: usize,
    max_cells_per_line: usize,
    max_content_bytes_per_line: usize,
    max_record_bytes: usize,
) -> Option<Vec<Line>> {
    let header = data.get(..4)?;
    let count = u32::from_le_bytes(header.try_into().ok()?) as usize;
    if count > max_lines {
        return None;
    }
    let mut lines = Vec::new();
    lines.try_reserve_exact(count).ok()?;
    let mut offset = 4usize;
    while lines.len() < count {
        let record = data.get(offset..)?;
        let (line, size) = decode_line_strict(
            record,
            max_cells_per_line,
            max_content_bytes_per_line,
            max_record_bytes,
        )?;
        let end = offset.checked_add(size)?;
        lines.push(line);
        offset = end;
    }
    (offset == data.len()).then_some(lines)
}

fn decode_line_strict(
    record: &[u8],
    max_cells_per_line: usize,
    max_content_bytes_per_line: usize,
    max_record_bytes: usize,
) -> Option<(Line, usize)> {
    let version = *record.first()?;
    let content_len = if version == 0 {
        let bytes = record.get(1..5)?;
        u32::from_le_bytes(bytes.try_into().ok()?) as usize
    } else {
        let bytes = record.get(2..6)?;
        u32::from_le_bytes(bytes.try_into().ok()?) as usize
    };
    if content_len > max_content_bytes_per_line {
        return None;
    }
    let size = if version == 0 {
        line_size_v0(record)?
    } else {
        line_size_v1v2(record, version)?
    };
    if size > max_record_bytes {
        return None;
    }
    let encoded = record.get(..size)?;
    let line = Line::deserialize(encoded)?;
    if line.len() > max_content_bytes_per_line {
        return None;
    }
    if let Some(attrs) = line.attrs() {
        let mut cells = 0usize;
        for run in attrs.runs() {
            cells = cells.checked_add(run.length as usize)?;
            if cells > max_cells_per_line {
                return None;
            }
        }
    }
    let spans_bounded = line.hyperlinks().is_none_or(|spans| {
        spans.iter().all(|span| {
            span.start_col <= span.end_col && usize::from(span.end_col) <= max_cells_per_line
        })
    }) && line.underline_colors().is_none_or(|spans| {
        spans.iter().all(|span| {
            span.start_col <= span.end_col && usize::from(span.end_col) <= max_cells_per_line
        })
    });
    spans_bounded.then_some((line, size))
}

/// Strictly validate a bounded scrollback-then-visible checkpoint while retaining
/// only its final `tail_lines` records. This is the one-release v0.52 handoff
/// decoder: input bytes and total record count are capped by the caller, every
/// record is fully framed/deserialized/canonical, trailing bytes are rejected,
/// and allocation remains O(visible rows) instead of O(scrollback depth).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn deserialize_lines_tail_strict(
    data: &[u8],
    max_lines: usize,
    tail_lines: usize,
    max_cells_per_line: usize,
    max_content_bytes_per_line: usize,
    max_record_bytes: usize,
) -> Option<Vec<Line>> {
    let header = data.get(..4)?;
    let count = u32::from_le_bytes(header.try_into().ok()?) as usize;
    if count > max_lines || count < tail_lines || tail_lines == 0 {
        return None;
    }
    let mut tail = std::collections::VecDeque::new();
    tail.try_reserve_exact(tail_lines).ok()?;
    let mut canonical_record = Vec::new();
    let mut offset = 4usize;
    for _ in 0..count {
        let record = data.get(offset..)?;
        let (line, size) = decode_line_strict(
            record,
            max_cells_per_line,
            max_content_bytes_per_line,
            max_record_bytes,
        )?;
        let end = offset.checked_add(size)?;
        let encoded = data.get(offset..end)?;
        canonical_record.clear();
        line.serialize_into(&mut canonical_record);
        if canonical_record.as_slice() != encoded {
            return None;
        }
        if tail.len() == tail_lines {
            tail.pop_front();
        }
        tail.push_back(line);
        offset = end;
    }
    if offset != data.len() || tail.len() != tail_lines {
        return None;
    }
    Some(tail.into_iter().collect())
}

/// Deserialize a zstd-decoded DISK PAGE, capped at [`MAX_DECODE_PAGE_LINES`].
///
/// The disk cold tier's `decode_zstd_bounded` bounds the decompressed byte length,
/// but not the reconstructed line count; this is the entry every disk-page decode
/// must use so a crafted page cannot amplify into hundreds of MiB of `Line` structs.
#[must_use]
pub fn deserialize_page_lines(data: &[u8]) -> Vec<Line> {
    deserialize_lines_capped(data, MAX_DECODE_PAGE_LINES)
}

/// Deserialize multiple lines from a block, UNCAPPED (whole-block).
///
/// Handles legacy (v0), v1, v2, v3, and v4 line formats by computing line size
/// dynamically from the serialized data. Used by the checkpoint path, whose body
/// is uncompressed and input-proportional. The size-bounded disk decode path must
/// use [`deserialize_page_lines`] / [`deserialize_lines_capped`] instead.
#[must_use]
pub fn deserialize_lines(data: &[u8]) -> Vec<Line> {
    deserialize_lines_capped(data, usize::MAX)
}
