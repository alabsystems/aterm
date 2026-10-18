// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Block-level serialization for multiple scrollback lines.
//!
//! Used by the warm and cold tiers; handles the v0 to v4 line formats. Every
//! deserialized length uses checked arithmetic against corrupt input.

use super::Line;

/// Serialize multiple lines for block compression.
#[must_use]
// Skip: same PtrMetadata/extend alloc class as `serialize_into`; the
// pre-size is a hint and the format is length-prefixed. Round-trip tested.
#[cfg_attr(trust_verify, trust::skip)]
pub fn serialize_lines(lines: &[Line]) -> Vec<u8> {
    // Format: [count:4][line0][line1]...
    //
    // Reserve the full wire size (styles, hyperlinks, underline colours) so
    // colourful output does not regrow the block during warm-tier compression.
    // A capacity hint only; saturating arithmetic and the bound keep the strict
    // L0 gate's overflow and allocation-budget obligations provable.
    let estimate = lines
        .iter()
        .map(Line::serialized_size_estimate)
        .fold(4usize, usize::saturating_add);
    let mut result = if estimate < crate::codec::MAX_DECOMPRESSED_SCROLLBACK_PAGE_BYTES {
        // The `.min(MAX - 1)` is a no-op in this branch; it restates the branch
        // condition on the allocation count for the strict L0 gate's
        // unbounded-allocation check.
        Vec::with_capacity(estimate.min(crate::codec::MAX_DECOMPRESSED_SCROLLBACK_PAGE_BYTES - 1))
    } else {
        Vec::new()
    };
    let count = u32::try_from(lines.len()).unwrap_or(u32::MAX);
    result.extend_from_slice(&count.to_le_bytes());
    for line in lines {
        line.append_serialized(&mut result);
    }
    result
}

/// Serialize a block only if its entire wire form fits `max_bytes`.
///
/// Every line is measured before any output allocation. Saturated measurements and
/// aggregate overflow refuse the block; the wire form is the same as [`serialize_lines`].
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn serialize_lines_bounded(lines: &[Line], max_bytes: usize) -> Option<Vec<u8>> {
    let count = u32::try_from(lines.len()).ok()?;
    let size = lines.iter().try_fold(4usize, |size, line| {
        let line_size = line.serialized_size_estimate();
        if line_size == usize::MAX {
            return None;
        }
        size.checked_add(line_size)
            .filter(|size| *size <= max_bytes)
    })?;
    if size > max_bytes {
        return None;
    }
    let mut result = Vec::new();
    result.try_reserve_exact(size).ok()?;
    result.extend_from_slice(&count.to_le_bytes());
    for line in lines {
        line.append_serialized(&mut result);
    }
    (result.len() <= max_bytes).then_some(result)
}

/// [`serialize_lines`] for a store that must decode what it writes: the lines
/// in consecutive pages, oldest first, as `(lines in the page, page bytes)`,
/// none longer than `max_page_bytes` unless one line alone is, which then gets
/// a page of its own. A page is byte-identical to `serialize_lines` of its
/// lines, and lines that fit make exactly one page.
///
/// Measured, not estimated: a record that overflows its page is taken back off
/// it and starts the next one, so the bound holds whatever the estimate says.
// Skip: same PtrMetadata/extend alloc class as `serialize_lines`.
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn serialize_pages(lines: &[Line], max_page_bytes: usize) -> Vec<(usize, Vec<u8>)> {
    // The `serialize_lines` capacity hint, for the one page lines that fit
    // make; a page past the first starts unreserved.
    let estimate = lines
        .iter()
        .map(Line::serialized_size_estimate)
        .fold(4usize, usize::saturating_add)
        .min(max_page_bytes);
    let mut pages = Vec::with_capacity(1);
    let mut page = Vec::with_capacity(estimate);
    page.extend_from_slice(&[0; 4]);
    let mut count = 0usize;
    for line in lines {
        let mark = page.len();
        line.append_serialized(&mut page);
        if page.len() > max_page_bytes && count > 0 {
            page.truncate(mark);
            pages.push(seal_page(page, count));
            page = Vec::from([0; 4]);
            line.append_serialized(&mut page);
            count = 0;
        }
        count = count.saturating_add(1);
    }
    if count > 0 {
        pages.push(seal_page(page, count));
    }
    pages
}

/// Write a [`serialize_pages`] page's `[count:4]` header (saturating at
/// `u32::MAX`, as [`serialize_lines`] does).
fn seal_page(mut page: Vec<u8>, count: usize) -> (usize, Vec<u8>) {
    if let Some(header) = page.get_mut(..4) {
        header.copy_from_slice(&u32::try_from(count).unwrap_or(u32::MAX).to_le_bytes());
    }
    (count, page)
}

// Pages split only at line boundaries, so the store's pages all decode only if
// the largest record an honest grid writes fits one alone: the content cap, 15
// bytes of framing, and per column of the widest grid (`MAX_HYPERLINK_SPANS`)
// an attribute run (14 bytes), an underline colour (8) and a link at the
// ingestion caps. About 37 MB against the 64 MiB page cap.
const _: () = assert!(
    max_linked_record_bytes(
        super::line_codec::MAX_LINE_CONTENT_BYTES
            + 15
            + super::line_codec::MAX_HYPERLINK_SPANS * (14 + 8),
        super::line_codec::MAX_HYPERLINK_SPANS,
    ) + 4
        <= crate::codec::MAX_DECOMPRESSED_SCROLLBACK_PAGE_BYTES
);

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

/// Compute the byte size of a v1/v2/v3/v4 line record:
/// `[version:1][flags:1][len:4][content:len][has_attrs:1][attrs...][hyperlinks]`
/// then, in v4, the underline colours. `None` if `data` is truncated or a size
/// computation overflows.
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
        // By-byte `get`s with saturating offsets: each read discharges its own
        // bound for the strict gate.
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
        // v3 appends hyperlinks and v4 an underline-colour section after them;
        // framing must include it, or every following record misframes.
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

/// Compute the byte size of the v4 underline-colour section: `[count:2]` then
/// `count` fixed 8-byte records, so `2 + count * 8`. Sub-slice count read as in
/// [`hyperlinks_size_v2`]; `checked_*` rejects an overflowing count.
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
/// Sub-slice reads (`get`, a never-taken length guard, constant indexing) rather
/// than `data[pos + k]`: the guard is the comparison the strict L0 gate needs to
/// prove the constant indexes.
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

/// Compute the byte size of the v3 hyperlinks section (with IDs), with the
/// sub-slice reads of [`hyperlinks_size_v2`].
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

/// Absolute cap on the number of lines a single [`deserialize_lines_capped`] call
/// reconstructs from a size-bounded block (a decoded cold page).
///
/// A 64 MiB decoded page of 5-byte empty records declares ~13.4M lines, ~850 MiB
/// of `Line` structs; this cap keeps the `Vec<Line>` near the 64 MiB decode
/// budget. It is ~100× a real cold page (one warm block, `DEFAULT_WARM_LIMIT` =
/// 10K lines). Not applied to the checkpoint path, whose body is uncompressed and
/// input-proportional and serializes the visible rows last, so a cap there would
/// drop the live screen.
///
/// It also clamps a configured `block_size` (`Scrollback::with_block_size`), so
/// the cap never clips a legitimate block.
pub(crate) const MAX_DECODE_PAGE_LINES: usize = 1 << 20;

#[cfg(test)]
thread_local! {
    /// Serialized line records this thread's block walks have accepted: the
    /// decode work every read of a warm block or cold page pays. Counted at the
    /// one walk both the decode and the eviction's framing check run, so a
    /// regression that decompresses the whole scrollback to trim one line shows
    /// as a count proportional to the scrollback. Thread-local for parallel tests.
    static RECORDS_WALKED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Read and reset this thread's [`RECORDS_WALKED`].
#[cfg(test)]
pub(crate) fn take_records_walked() -> usize {
    RECORDS_WALKED.with(|c| c.replace(0))
}

/// Walk the framed records of a serialized block, offering each complete
/// record's bytes to `accept`.
///
/// The module's one framing loop: [`deserialize_lines_capped`] accepts with
/// `Line::deserialize`, [`count_page_lines`] with `Line::record_is_valid`, so the
/// two cannot disagree about where records begin (the cold tier's validation
/// depends on it). Stops after `max_lines` accepted records, at the declared
/// `count`, or at the first misframed record; returns the accepted count and the
/// stop offset.
// Skip: the block decode reader's guarded slice walk (the SliceBoundsCheck /
// decode-reader class of its per-line siblings). Round-trip tested; the
// max_lines cap bounds the accepted count.
#[cfg_attr(trust_verify, trust::skip)]
fn walk_records(
    data: &[u8],
    count: usize,
    max_lines: usize,
    mut accept: impl FnMut(&[u8]) -> bool,
) -> (usize, usize) {
    let mut accepted = 0usize;
    let mut offset = 4;

    // `max_lines` bounds the total accepted, not just the reservation: a crafted
    // block of ~5-byte records would otherwise build millions of `Line`s.
    while offset < data.len() && accepted < count && accepted < max_lines {
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
        // `get` rather than indexing: the spelling the strict L0 gate proves.
        let Some(record_bytes) = data.get(offset..line_end) else {
            break;
        };

        if accept(record_bytes) {
            // Bounded by `max_lines` (the loop guard); saturation is dead code
            // carrying the no-overflow proof.
            accepted = accepted.saturating_add(1);
        }
        offset = line_end;
    }

    #[cfg(test)]
    RECORDS_WALKED.with(|c| c.set(c.get() + accepted));
    (accepted, offset)
}

/// Read a block's declared line count (the 4-byte header), or `None` when the
/// buffer is too short to hold one.
fn declared_count(data: &[u8]) -> Option<usize> {
    let header = data.get(..4)?;
    if header.len() < 4 {
        return None;
    }
    Some(u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize)
}

/// Count the records of a serialized block that [`Line::deserialize`] would
/// accept, without reconstructing them (capped like [`deserialize_lines_capped`]).
///
/// The warm→cold eviction path re-compresses the page bytes verbatim, so it
/// needs only the framing check, not a `Line` for every line aging out on the
/// output thread. Same walk and predicate as the real decode ([`walk_records`],
/// `Line::record_is_valid`). Silent: the truncation warn belongs to the decoder.
#[must_use]
pub(crate) fn count_page_lines(data: &[u8], max_lines: usize) -> usize {
    let Some(count) = declared_count(data) else {
        return 0;
    };
    let (accepted, _offset) = walk_records(data, count, max_lines, Line::record_is_valid);
    accepted
}

/// Deserialize multiple lines from a block, capped at `max_lines` reconstructed
/// lines. See [`MAX_DECODE_PAGE_LINES`] for why the cold-page decode needs this and
/// why the checkpoint path must not be capped.
#[must_use]
// Skip: the block decode reader's guarded slice walk (the SliceBoundsCheck /
// decode-reader class of its per-line siblings). Round-trip tested; the
// max_lines cap bounds the reservation.
#[cfg_attr(trust_verify, trust::skip)]
pub fn deserialize_lines_capped(data: &[u8], max_lines: usize) -> Vec<Line> {
    let Some(count) = declared_count(data) else {
        return Vec::new();
    };
    // Clamp the reservation to what the data can hold: a v0 record is at least
    // 5 bytes.
    let max_possible = data.len().saturating_sub(4) / 5;
    // Also cap the initial reservation: `count` is untrusted and a crafted page
    // can inflate it over a few real records, and a retained Vec would pin spare
    // capacity no len-based byte count sees. A real cold page is one warm block
    // (~10K lines by default).
    const MAX_PREALLOC_LINES: usize = 16_384;
    let mut lines = Vec::with_capacity(count.min(max_possible).min(MAX_PREALLOC_LINES));

    let (_accepted, offset) = walk_records(data, count, max_lines, |record_bytes| {
        let decoded = Line::deserialize(record_bytes);
        // Pin the cheap acceptance predicate to the real decoder:
        // `count_page_lines` (the cold-tier validation) trusts them to agree.
        debug_assert_eq!(
            decoded.is_some(),
            Line::record_is_valid(record_bytes),
            "record_is_valid must mirror Line::deserialize exactly"
        );
        if let Some(line) = decoded {
            lines.push(line);
            true
        } else {
            false
        }
    });

    if lines.len() >= max_lines && offset < data.len() {
        // Pre-composed via the log shim: no macro-expanded `format_args!` unsafe
        // in this function for the strict gate (see log_shim.rs).
        let mut msg = String::from("deserialize_lines_capped truncated at ");
        msg.push_str(&crate::error::dec_string(max_lines));
        msg.push_str(" lines (block has more records)");
        crate::log_shim::warn_str(&msg);
    }

    // Drop spare capacity from an over-estimated `count`: a retained page then
    // pins only its content, which len-based byte counts measure.
    lines.shrink_to_fit();
    lines
}

/// Strict bounded block decode for checkpoint ingestion.
///
/// Rejects rather than truncates: the header declares at most `max_lines`, every
/// record fits the per-line content and wire budgets and deserializes, and no
/// trailing bytes remain. Length checks run before `Line::deserialize` allocates.
///
/// `max_cells_per_line` counts columns and bounds span ends exactly; the attrs
/// RLE is indexed per character, so it is bounded by the larger of that and the
/// line's character count (see `decode_line_strict`).
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

/// The largest line record [`deserialize_lines_strict_dropping_over_cap_links`]
/// decodes: `max_record_bytes` plus one hyperlink span per column at the OSC 8
/// ingestion ceilings (8,460 bytes a column). At 80 columns and the handoff's
/// record cap (`16 KiB + cols * 512`) that is 734,144 bytes; at the widest grid
/// (4,096 columns) 36,765,696.
#[must_use]
pub const fn max_linked_record_bytes(max_record_bytes: usize, max_cells_per_line: usize) -> usize {
    max_record_bytes.saturating_add(
        max_cells_per_line.saturating_mul(super::line_codec::MAX_HYPERLINK_SPAN_BYTES),
    )
}

/// [`deserialize_lines_strict`], except that a line record over
/// `max_record_bytes` because of its hyperlinks is kept with its hyperlinks
/// dropped instead of refusing the whole block. Returns the lines and the
/// index of every line that lost its links, ascending.
///
/// An OSC 8 URL may be 8 KiB and a row of one-cell links carries each URL once
/// per span, so a dozen links in an 80-column row make a ~96 KiB record against
/// a 56 KiB cap; refusing the block for it would lose every line beside it. A
/// record over `max_record_bytes` is kept only when all of these hold, and
/// otherwise the block is refused as [`deserialize_lines_strict`] refuses it:
///
/// - it is at most [`max_linked_record_bytes`], the hard ceiling: no record past
///   it is decoded;
/// - it decodes under every other strict bound (content bytes, attrs entries,
///   hyperlink and underline span columns);
/// - the line re-serializes to exactly the record's bytes, so the decode
///   truncated or skipped nothing;
/// - the line has hyperlinks;
/// - with them dropped ([`Line::clear_hyperlinks`], which keeps the text, attrs,
///   flags and underline colours) the line re-serializes within
///   `max_record_bytes`.
///
/// A block [`deserialize_lines_strict`] admits decodes to the same lines here,
/// none stripped.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn deserialize_lines_strict_dropping_over_cap_links(
    data: &[u8],
    max_lines: usize,
    max_cells_per_line: usize,
    max_content_bytes_per_line: usize,
    max_record_bytes: usize,
) -> Option<(Vec<Line>, Vec<usize>)> {
    let header = data.get(..4)?;
    let count = u32::from_le_bytes(header.try_into().ok()?) as usize;
    if count > max_lines {
        return None;
    }
    let ceiling = max_linked_record_bytes(max_record_bytes, max_cells_per_line);
    let mut lines = Vec::new();
    lines.try_reserve_exact(count).ok()?;
    let mut stripped = Vec::new();
    let mut offset = 4usize;
    while lines.len() < count {
        let record = data.get(offset..)?;
        let decoded = decode_line_strict(
            record,
            max_cells_per_line,
            max_content_bytes_per_line,
            max_record_bytes,
        );
        let (line, size) = match decoded {
            Some(decoded) => decoded,
            None => {
                let (mut line, size) = decode_line_strict(
                    record,
                    max_cells_per_line,
                    max_content_bytes_per_line,
                    ceiling,
                )?;
                // Within the cap it failed the strict decode for another
                // reason (the same call cannot pass under a larger cap then);
                // over it with no link, links are not why it is over.
                if size <= max_record_bytes || !line.has_hyperlinks() {
                    return None;
                }
                // Only a record the serializer itself would write for this line:
                // `Line::deserialize` quietly truncates what it does not keep
                // (underline spans past `MAX_UNDERLINE_SPANS`, for one), so the
                // round trip makes the record's size the line's, and dropping the
                // links removes exactly their bytes.
                if line.serialize().as_slice() != record.get(..size)? {
                    return None;
                }
                line.clear_hyperlinks();
                if line.serialize().len() > max_record_bytes {
                    return None;
                }
                stripped.push(lines.len());
                (line, size)
            }
        };
        let end = offset.checked_add(size)?;
        lines.push(line);
        offset = end;
    }
    (offset == data.len()).then_some((lines, stripped))
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
        // The attrs RLE is indexed by character, not cell: the grid's row→line
        // conversion charges one entry per `char` of a cell's text (a combining
        // mark its own, a ZWJ cluster one per codepoint, a wide spacer none), so
        // a full-width styled row of `e` + U+0301 has more attrs than columns.
        // The bound is `max(cells, the line's character count)`, still bounded
        // by the content-bytes cap above. Span `end_col`s are real columns and
        // keep the column cap.
        let max_attr_entries = max_cells_per_line.max(line_char_count(line.as_bytes()));
        let mut entries = 0usize;
        for run in attrs.runs() {
            entries = entries.checked_add(run.length as usize)?;
            if entries > max_attr_entries {
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

/// The number of characters in a line's content, as the attrs RLE counts them.
///
/// Counts UTF-8 lead bytes: exactly `str::chars().count()` for valid UTF-8 and
/// never more than the byte length otherwise, so the content-bytes cap already
/// bounds it, with no allocation or validation pass.
fn line_char_count(content: &[u8]) -> usize {
    content.iter().filter(|&&byte| byte & 0xC0 != 0x80).count()
}

/// Strictly validate a bounded scrollback-then-visible checkpoint, keeping only
/// its final `tail_lines` records: every record fully framed, deserialized and
/// canonical, trailing bytes rejected, allocation O(`tail_lines`) rather than
/// O(scrollback depth).
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

/// Deserialize a decoded cold page, capped at [`MAX_DECODE_PAGE_LINES`].
///
/// `decode_cold_bounded` bounds the decompressed bytes but not the line count;
/// every cold-page decode goes through here so a crafted page cannot amplify
/// into hundreds of MiB of `Line` structs.
#[must_use]
pub fn deserialize_page_lines(data: &[u8]) -> Vec<Line> {
    deserialize_lines_capped(data, MAX_DECODE_PAGE_LINES)
}

/// Deserialize multiple lines from a block, uncapped.
///
/// For the checkpoint path, whose body is uncompressed and input-proportional.
/// A size-bounded cold-page decode uses [`deserialize_page_lines`] instead.
#[must_use]
pub fn deserialize_lines(data: &[u8]) -> Vec<Line> {
    deserialize_lines_capped(data, usize::MAX)
}
