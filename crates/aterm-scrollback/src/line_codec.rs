// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Serialization helpers for scrollback lines.

use super::{CellAttrs, HyperlinkSpan, Line, LineContent, LineFlags, UnderlineColorSpan};
use aterm_alloc::SmallVec;
use aterm_rle::Rle;
use std::sync::Arc;

/// Absolute cap on a single deserialized line's content byte length.
pub(crate) const MAX_LINE_CONTENT_BYTES: usize = 2 * 1024 * 1024;

/// Cap on a single deserialized hyperlink URL: the OSC 8 ingestion ceiling (`aterm-core`'s `MAX_HYPERLINK_URL_BYTES`).
pub(crate) const MAX_HYPERLINK_URL_BYTES: usize = 8192;

/// Cap on a single deserialized OSC 8 `id=`: the ingestion ceiling (`aterm-core` admits ids of at most 256 bytes).
pub(crate) const MAX_HYPERLINK_ID_BYTES: usize = 256;

/// The most bytes one hyperlink span takes in a v3 record whose URL and id are within the ceilings above: `start_col:2 + end_col:2 + url_len:4 + url + id_len:4 + id`, 8,460 bytes.
pub(crate) const MAX_HYPERLINK_SPAN_BYTES: usize =
    12 + MAX_HYPERLINK_URL_BYTES + MAX_HYPERLINK_ID_BYTES;

/// Cap on the number of hyperlink spans in one deserialized line: `MAX_GRID_COLS` (4096).
pub(crate) const MAX_HYPERLINK_SPANS: usize = 4096;

/// Cap on the number of underline-colour spans in one deserialized line, `MAX_GRID_COLS` (4096), for the same reason as [`MAX_HYPERLINK_SPANS`].
pub(crate) const MAX_UNDERLINE_SPANS: usize = 4096;

impl CellAttrs {
    /// Serialize to bytes (10 bytes).
    #[must_use]
    pub(crate) const fn serialize(&self) -> [u8; 10] {
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
    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        let estimate = self.serialized_size_estimate();
        let mut v = if estimate < crate::codec::MAX_DECOMPRESSED_SCROLLBACK_PAGE_BYTES {
            Vec::with_capacity(estimate)
        } else {
            Vec::new()
        };
        self.append_serialized(&mut v);
        v
    }

    /// Exact per-line serialized-size estimate shared by line and block serialization.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(super) fn serialized_size_estimate(&self) -> usize {
        let content_len = self.content.as_bytes().len();
        let attrs_size = if self.attrs.is_some() {
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

    /// Serialize line by appending its bytes to `result`, reserving this line's size first.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn serialize_into(&self, result: &mut Vec<u8>) {
        let estimate = self.serialized_size_estimate();
        if estimate < crate::codec::MAX_DECOMPRESSED_SCROLLBACK_PAGE_BYTES {
            result.reserve(estimate);
        }
        self.append_serialized(result);
    }

    /// Shared byte writer.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(super) fn append_serialized(&self, result: &mut Vec<u8>) {
        let content = self.content.as_bytes();
        let content_len = content.len();

        result.push(if self.underline_colors.is_some() {
            4
        } else {
            3
        });

        result.push(self.flags.bits());

        let content_len_u32 = u32::try_from(content_len).unwrap_or(u32::MAX);
        result.extend_from_slice(&content_len_u32.to_le_bytes());
        result.extend_from_slice(content);

        if self.attrs.is_some() {
            let runs = super::attr_runs(self.attrs.as_deref());
            result.push(1);
            let run_count = u32::try_from(runs.len()).unwrap_or(u32::MAX);
            result.extend_from_slice(&run_count.to_le_bytes());
            for run in runs {
                result.extend_from_slice(&run.value.serialize());
                result.extend_from_slice(&run.length.to_le_bytes());
            }
        } else {
            result.push(0);
        }

        if let Some(spans) = self.hyperlinks() {
            let count = u16::try_from(spans.len()).unwrap_or(u16::MAX);
            result.extend_from_slice(&count.to_le_bytes());
            for span in spans {
                result.extend_from_slice(&span.start_col.to_le_bytes());
                result.extend_from_slice(&span.end_col.to_le_bytes());
                let url_len = u32::try_from(span.url.len()).unwrap_or(u32::MAX);
                result.extend_from_slice(&url_len.to_le_bytes());
                result.extend_from_slice(span.url.as_bytes());
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
            result.extend_from_slice(&0u16.to_le_bytes());
        }

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
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn deserialize(data: &[u8]) -> Option<Self> {
        if data.is_empty() {
            return None;
        }

        let version = data[0];
        if version == 0 {
            return Self::deserialize_legacy(data);
        }

        if data.len() < 7 {
            return None;
        }

        let flags = LineFlags::from_bits_truncate(data[1]);
        let content_len = u32::from_le_bytes([data[2], data[3], data[4], data[5]]) as usize;
        if content_len > MAX_LINE_CONTENT_BYTES {
            return None;
        }

        let content_end = 6usize.checked_add(content_len)?;
        if data.len() < content_end.checked_add(1)? {
            return None;
        }

        let content = LineContent::from_bytes(data.get(6..content_end)?);

        let (attrs, offset) = Self::deserialize_attrs(data, content_end)?;
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
            images: None,
        })
    }

    /// Whether `data` frames a record [`Self::deserialize`] would accept, decided from the header without building the `Line`.
    #[must_use]
    pub(crate) fn record_is_valid(data: &[u8]) -> bool {
        let Some(&version) = data.first() else {
            return false;
        };

        if version == 0 {
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
        let Some(&has_attrs) = data.get(content_end) else {
            return false;
        };
        if has_attrs == 0 {
            return true;
        }
        let Some(runs_start) = content_end.checked_add(5) else {
            return false;
        };
        data.len() >= runs_start
    }

    fn deserialize_attrs(
        data: &[u8],
        content_end: usize,
    ) -> Option<(Option<Rle<CellAttrs>>, usize)> {
        if content_end >= data.len() {
            return None;
        }
        if data[content_end] == 0 {
            return Some((None, content_end.saturating_add(1)));
        }

        let attrs_start = content_end.checked_add(1)?;
        let runs_start = attrs_start.checked_add(4)?;
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

        let remaining = data.len().saturating_sub(runs_start);
        let max_runs = remaining / 14;
        let clamped_count = run_count.min(max_runs);

        let mut rle = Rle::new();
        let mut offset = runs_start;
        for _ in 0..clamped_count {
            let Some(end) = offset.checked_add(14) else {
                break;
            };
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
    #[cfg_attr(trust_verify, trust::skip)]
    fn deserialize_hyperlinks(data: &[u8], offset: usize) -> Option<SmallVec<HyperlinkSpan, 2>> {
        let header_end = offset.checked_add(2)?;
        let count_bytes = data.get(offset..header_end)?;
        if count_bytes.len() < 2 {
            return None;
        }
        let count = u16::from_le_bytes([count_bytes[0], count_bytes[1]]) as usize;
        if count == 0 {
            return None;
        }

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
            if url_len <= MAX_HYPERLINK_URL_BYTES
                && let Ok(url) = std::str::from_utf8(url_bytes)
            {
                spans.push(HyperlinkSpan::new(start_col, end_col, Arc::from(url)));
                pushed = pushed.saturating_add(1);
            }
            pos = url_end;
        }
        if pushed == 0 { None } else { Some(spans) }
    }

    /// Deserialize v3 hyperlink spans (with OSC 8 IDs) starting at `offset`.
    #[cfg_attr(trust_verify, trust::skip)]
    fn deserialize_hyperlinks_v3(
        data: &[u8],
        offset: usize,
    ) -> (Option<SmallVec<HyperlinkSpan, 2>>, usize) {
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
            return (None, header_end);
        }

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
        (result, pos)
    }

    /// Deserialize the v4 underline-colour section at `offset`: `[count:2]` then
    /// `count` records `[start_col:2][end_col:2][color:4]`, `color` the packed
    /// `0xTT_XXXXXX` form.
    #[cfg_attr(trust_verify, trust::skip)]
    fn deserialize_underline_colors(
        data: &[u8],
        offset: usize,
    ) -> Option<SmallVec<UnderlineColorSpan, 2>> {
        let header_end = offset.checked_add(2)?;
        let count_bytes = data.get(offset..header_end)?;
        if count_bytes.len() < 2 {
            return None;
        }
        let count = u16::from_le_bytes([count_bytes[0], count_bytes[1]]) as usize;
        if count == 0 {
            return None;
        }

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
            pushed = pushed.saturating_add(1);
            pos = end;
        }
        if pushed == 0 { None } else { Some(spans) }
    }

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

pub(crate) use super::line_codec_block::{
    MAX_DECODE_PAGE_LINES, count_page_lines, serialize_pages,
};
pub use super::line_codec_block::{
    deserialize_lines, deserialize_lines_strict, deserialize_lines_strict_dropping_over_cap_links,
    deserialize_lines_tail_strict, deserialize_page_lines, max_linked_record_bytes,
    serialize_lines, serialize_lines_bounded,
};
