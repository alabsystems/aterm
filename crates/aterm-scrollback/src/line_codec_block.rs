// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Block-level serialization for multiple scrollback lines.

use super::Line;

/// Serialize multiple lines for block compression.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn serialize_lines(lines: &[Line]) -> Vec<u8> {
    let estimate = lines
        .iter()
        .map(Line::serialized_size_estimate)
        .fold(4usize, usize::saturating_add);
    let mut result = if estimate < crate::codec::MAX_DECOMPRESSED_SCROLLBACK_PAGE_BYTES {
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

/// [`serialize_lines`] for a store that must decode what it writes.
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn serialize_pages(lines: &[Line], max_page_bytes: usize) -> Vec<(usize, Vec<u8>)> {
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

fn seal_page(mut page: Vec<u8>, count: usize) -> (usize, Vec<u8>) {
    if let Some(header) = page.get_mut(..4) {
        header.copy_from_slice(&u32::try_from(count).unwrap_or(u32::MAX).to_le_bytes());
    }
    (count, page)
}

const _: () = assert!(
    max_linked_record_bytes(
        super::line_codec::MAX_LINE_CONTENT_BYTES
            + 15
            + super::line_codec::MAX_HYPERLINK_SPANS * (14 + 8),
        super::line_codec::MAX_HYPERLINK_SPANS,
    ) + 4
        <= crate::codec::MAX_DECOMPRESSED_SCROLLBACK_PAGE_BYTES
);

const fn line_size_v0(data: &[u8]) -> Option<usize> {
    if data.len() < 5 {
        return None;
    }
    let content_len = u32::from_le_bytes([data[1], data[2], data[3], data[4]]) as usize;
    5usize.checked_add(content_len)
}

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

fn underline_colors_size_v4(data: &[u8], base_size: usize) -> Option<usize> {
    let hdr_end = base_size.checked_add(2)?;
    let count_bytes = data.get(base_size..hdr_end)?;
    if count_bytes.len() < 2 {
        return None;
    }
    let count = u16::from_le_bytes([count_bytes[0], count_bytes[1]]) as usize;
    count.checked_mul(8)?.checked_add(2)
}

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

/// Absolute cap on the number of lines a single [`deserialize_lines_capped`] call reconstructs from a size-bounded block (a decoded cold page).
pub(crate) const MAX_DECODE_PAGE_LINES: usize = 1 << 20;

#[cfg(test)]
thread_local! {
    static RECORDS_WALKED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Read and reset this thread's [`RECORDS_WALKED`].
#[cfg(test)]
pub(crate) fn take_records_walked() -> usize {
    RECORDS_WALKED.with(|c| c.replace(0))
}

/// Walk the framed records of a serialized block, offering each complete
/// record's bytes to `accept`.
#[cfg_attr(trust_verify, trust::skip)]
fn walk_records(
    data: &[u8],
    count: usize,
    max_lines: usize,
    mut accept: impl FnMut(&[u8]) -> bool,
) -> (usize, usize) {
    let mut accepted = 0usize;
    let mut offset = 4;

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
        let Some(record_bytes) = data.get(offset..line_end) else {
            break;
        };

        if accept(record_bytes) {
            accepted = accepted.saturating_add(1);
        }
        offset = line_end;
    }

    #[cfg(test)]
    RECORDS_WALKED.with(|c| c.set(c.get() + accepted));
    (accepted, offset)
}

fn declared_count(data: &[u8]) -> Option<usize> {
    let header = data.get(..4)?;
    if header.len() < 4 {
        return None;
    }
    Some(u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize)
}

/// Count the valid records in a serialized block without reconstructing them (capped like [`deserialize_lines_capped`]).
#[must_use]
pub(crate) fn count_page_lines(data: &[u8], max_lines: usize) -> usize {
    let Some(count) = declared_count(data) else {
        return 0;
    };
    let (accepted, _offset) = walk_records(data, count, max_lines, Line::record_is_valid);
    accepted
}

/// Deserialize multiple lines from a block, capped at `max_lines` reconstructed lines.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn deserialize_lines_capped(data: &[u8], max_lines: usize) -> Vec<Line> {
    let Some(count) = declared_count(data) else {
        return Vec::new();
    };
    let max_possible = data.len().saturating_sub(4) / 5;
    const MAX_PREALLOC_LINES: usize = 16_384;
    let mut lines = Vec::with_capacity(count.min(max_possible).min(MAX_PREALLOC_LINES));

    let (_accepted, offset) = walk_records(data, count, max_lines, |record_bytes| {
        let decoded = Line::deserialize(record_bytes);
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
        let mut msg = String::from("deserialize_lines_capped truncated at ");
        msg.push_str(&crate::error::dec_string(max_lines));
        msg.push_str(" lines (block has more records)");
        crate::log_shim::warn_str(&msg);
    }

    lines.shrink_to_fit();
    lines
}

/// Strict bounded block decode for checkpoint ingestion.
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

/// The largest line record [`deserialize_lines_strict_dropping_over_cap_links`] decodes.
#[must_use]
pub const fn max_linked_record_bytes(max_record_bytes: usize, max_cells_per_line: usize) -> usize {
    max_record_bytes.saturating_add(
        max_cells_per_line.saturating_mul(super::line_codec::MAX_HYPERLINK_SPAN_BYTES),
    )
}

/// [`deserialize_lines_strict`], dropping hyperlinks on records over `max_record_bytes` instead of refusing the block.
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
                if size <= max_record_bytes || !line.has_hyperlinks() {
                    return None;
                }
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

fn line_char_count(content: &[u8]) -> usize {
    content.iter().filter(|&&byte| byte & 0xC0 != 0x80).count()
}

/// Strictly validate a bounded scrollback-then-visible checkpoint, keeping only its final `tail_lines` records.
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
#[must_use]
pub fn deserialize_page_lines(data: &[u8]) -> Vec<Line> {
    deserialize_lines_capped(data, MAX_DECODE_PAGE_LINES)
}

/// Deserialize multiple lines from a block, uncapped.
#[must_use]
pub fn deserialize_lines(data: &[u8]) -> Vec<Line> {
    deserialize_lines_capped(data, usize::MAX)
}
