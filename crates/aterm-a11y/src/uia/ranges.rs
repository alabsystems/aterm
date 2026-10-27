// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TextPattern and retained TextRange COM objects over shared model text.

use super::com::held;
use super::objects::{RangeObject, range};
use super::values::{Bstr, Variant, array_com, array_f64, bstr_text, variant_bool};
use super::*;
use crate::uia_text::{Document, Unit};

use aterm_win32::Win32::System::Variant::VARIANT;
use aterm_win32::Win32::UI::Accessibility::{
    SupportedTextSelection, SupportedTextSelection_None, SupportedTextSelection_Single,
    TextPatternRangeEndpoint, TextPatternRangeEndpoint_End, TextPatternRangeEndpoint_Start,
    TextUnit, TextUnit_Character, TextUnit_Document, TextUnit_Format, TextUnit_Line, TextUnit_Page,
    TextUnit_Paragraph, TextUnit_Word, UIA_IsHiddenAttributeId, UIA_IsReadOnlyAttributeId,
    UIA_TEXTATTRIBUTE_ID, UiaGetReservedNotSupportedValue, UiaPoint,
};
use aterm_win32::core::BSTR;

fn unit(unit: TextUnit) -> Result<Unit> {
    Ok(match unit {
        TextUnit_Character => Unit::Character,
        TextUnit_Format | TextUnit_Word => Unit::Word,
        TextUnit_Line => Unit::Line,
        TextUnit_Paragraph => Unit::Paragraph,
        TextUnit_Page | TextUnit_Document => Unit::Document,
        _ => return Err(E_INVALIDARG),
    })
}

fn endpoint(endpoint: TextPatternRangeEndpoint) -> Result<usize> {
    match endpoint {
        TextPatternRangeEndpoint_Start => Ok(0),
        TextPatternRangeEndpoint_End => Ok(1),
        _ => Err(E_INVALIDARG),
    }
}

/// One range of a node's text, in UTF-16 offsets, kept on character boundaries.
pub(super) struct TextRange {
    key: Key,
    bounds: Mutex<[usize; 2]>,
}

impl TextRange {
    /// A new range object, as `ITextRangeProvider`.
    fn spanning(key: Key, start: usize, end: usize) -> Com {
        range(Self {
            key,
            bounds: Mutex::new([start.min(end), start.max(end)]),
        })
    }

    fn read<T>(
        &self,
        f: impl FnOnce(Node<'_>, &Document, &State, &mut [usize; 2]) -> Result<T>,
    ) -> Result<T> {
        self.key.read(|node, state| {
            let document = Document::new(node).ok_or(NOT_SUPPORTED)?;
            let mut bounds = self.bounds.lock_or_recover();
            bounds[0] = document.floor(bounds[0]);
            bounds[1] = document.floor(bounds[1]).max(bounds[0]);
            f(node, &document, state, &mut bounds)
        })
    }

    /// The range `other` (a client's borrowed `ITextRangeProvider`), when it is
    /// one of this window's over the same node.
    fn other(&self, other: Raw) -> Result<com::Held<RangeObject>> {
        // SAFETY: a client's interface argument, live for the call.
        let other = unsafe { held::<RangeObject>(other) }.ok_or(E_INVALIDARG)?;
        if !self.key.shared.ptr_eq(&other.range().key.shared) {
            return Err(E_INVALIDARG);
        }
        self.key.read(|node, state| {
            let other_node = match &other.range().key.handle {
                Some(handle) => state.publication.node(handle),
                None => state.publication.tree().map(crate::tree::State::root),
            }
            .ok_or(ELEMENT_NOT_AVAILABLE)?;
            if node.id() != other_node.id() {
                return Err(E_INVALIDARG);
            }
            Ok(())
        })?;
        Ok(other)
    }
}

/// `ITextProvider` and `ITextProvider2`.
impl Provider {
    pub(super) fn text_selection(&self) -> Result<*mut SAFEARRAY> {
        let (key, selection) = self.key().read(|node, state| {
            if !node.supports_text_ranges() {
                return Err(NOT_SUPPORTED);
            }
            Ok((
                Key::capture(&self.shared, node.id(), state)?,
                crate::platform::selection(node),
            ))
        })?;
        let values = selection
            .map(|(start, len)| TextRange::spanning(key, start, start + len).query(&IID_IUnknown))
            .transpose()?
            .into_iter()
            .collect::<Vec<_>>();
        array_com(&values)
    }

    pub(super) fn visible_ranges(&self) -> Result<*mut SAFEARRAY> {
        array_com(&[self.document_range()?.query(&IID_IUnknown)?])
    }

    /// Never a range: a child of this window's is refused as not supported
    /// (text nodes have no embedded objects) or as an invalid argument.
    pub(super) fn range_from_child(&self, child: Raw) -> Result<Com> {
        // SAFETY: a client's interface argument, live for the call.
        let child = unsafe { held::<objects::ProviderObject>(child) }.ok_or(E_INVALIDARG)?;
        if !self.shared.ptr_eq(&child.provider().shared) {
            return Err(E_INVALIDARG);
        }
        self.key().read(|node, _| {
            if node.supports_text_ranges() {
                Err(NOT_SUPPORTED)
            } else {
                Err(E_INVALIDARG)
            }
        })
    }

    pub(super) fn range_from_point(&self, point: UiaPoint) -> Result<Com> {
        if !point.x.is_finite() || !point.y.is_finite() {
            return Err(E_INVALIDARG);
        }
        let (key, offset) = self.key().read(|node, state| {
            if !node.supports_text_ranges() {
                return Err(NOT_SUPPORTED);
            }
            let local = node.transform().inverse()
                * Point::new(point.x - state.origin.0, point.y - state.origin.1);
            let position = node.text_position_at_point(local);
            let text = crate::platform::text(node).ok_or(NOT_SUPPORTED)?;
            Ok((
                Key::capture(&self.shared, node.id(), state)?,
                text.chars()
                    .take(position.to_global_usv_index())
                    .map(char::len_utf16)
                    .sum(),
            ))
        })?;
        Ok(TextRange::spanning(key, offset, offset))
    }

    pub(super) fn document_range(&self) -> Result<Com> {
        let (key, len) = self.key().read(|node, state| {
            Ok((
                Key::capture(&self.shared, node.id(), state)?,
                Document::new(node).ok_or(NOT_SUPPORTED)?.len(),
            ))
        })?;
        Ok(TextRange::spanning(key, 0, len))
    }

    pub(super) fn supported_text_selection(&self) -> Result<SupportedTextSelection> {
        self.key().read(|node, _| {
            Ok(if node.data().supports_action(Action::SetTextSelection) {
                SupportedTextSelection_Single
            } else {
                SupportedTextSelection_None
            })
        })
    }

    pub(super) fn caret_range(&self, active: *mut BOOL) -> Result<Com> {
        if active.is_null() {
            return Err(E_INVALIDARG);
        }
        let (key, offset, focused) = self.key().read(|node, state| {
            let position = node.text_selection_focus().ok_or(NOT_SUPPORTED)?;
            let text = crate::platform::text(node).ok_or(NOT_SUPPORTED)?;
            Ok((
                Key::capture(&self.shared, node.id(), state)?,
                text.chars()
                    .take(position.to_global_usv_index())
                    .map(char::len_utf16)
                    .sum(),
                node.is_focused(),
            ))
        })?;
        // SAFETY: the caller's non-null slot, checked above.
        unsafe { active.write(boolean(focused)) };
        Ok(TextRange::spanning(key, offset, offset))
    }
}

/// `ITextRangeProvider`.
impl TextRange {
    pub(super) fn clone_range(&self) -> Result<Com> {
        self.read(|_, _, _, bounds| Ok(TextRange::spanning(self.key.clone(), bounds[0], bounds[1])))
    }

    pub(super) fn compare(&self, other: Raw) -> Result<BOOL> {
        let other = self.other(other)?;
        let theirs = *other.range().bounds.lock_or_recover();
        self.read(|_, doc, _, bounds| {
            Ok(boolean(
                *bounds == [doc.floor(theirs[0]), doc.floor(theirs[1])],
            ))
        })
    }

    pub(super) fn compare_endpoints(
        &self,
        end: TextPatternRangeEndpoint,
        other: Raw,
        other_end: TextPatternRangeEndpoint,
    ) -> Result<i32> {
        let end = endpoint(end)?;
        let other_end = endpoint(other_end)?;
        let other = self.other(other)?;
        let theirs = other.range().bounds.lock_or_recover()[other_end];
        self.read(|_, doc, _, bounds| {
            Ok(match bounds[end].cmp(&doc.floor(theirs)) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            })
        })
    }

    pub(super) fn expand_to_enclosing_unit(&self, by: TextUnit) -> Result<()> {
        let by = unit(by)?;
        self.read(|_, doc, _, bounds| {
            let (start, end) = doc.enclosing(bounds[0], by);
            *bounds = [start, end];
            Ok(())
        })
    }

    /// The whole range when its read-only state is `value`, else none; only
    /// IsReadOnly is searchable. `value` stays the caller's.
    pub(super) fn find_attribute(
        &self,
        id: UIA_TEXTATTRIBUTE_ID,
        value: &VARIANT,
        backward: BOOL,
    ) -> Result<Option<Com>> {
        if id != UIA_IsReadOnlyAttributeId {
            return Err(NOT_SUPPORTED);
        }
        let expected = variant_bool(value).map_err(|_| E_INVALIDARG)?;
        self.read(|node, _, _, bounds| {
            let read_only = !node.data().supports_action(Action::ReplaceSelectedText)
                && !node.data().supports_action(Action::SetValue);
            if expected != read_only {
                return Ok(None);
            }
            let _ = backward;
            Ok(Some(TextRange::spanning(
                self.key.clone(),
                bounds[0],
                bounds[1],
            )))
        })
    }

    /// The first (or, `backward`, last) match of `text` within the range, or
    /// none.
    pub(super) fn find_text(
        &self,
        text: BSTR,
        backward: BOOL,
        ignore_case: BOOL,
    ) -> Result<Option<Com>> {
        // SAFETY: the caller's BSTR, live for the call.
        let needle = unsafe { bstr_text(text) };
        if needle.is_empty() {
            return Err(E_INVALIDARG);
        }
        self.read(|_, doc, _, bounds| {
            let source = doc.slice(bounds[0], bounds[1]);
            let (haystack, needle, map) = if ignore_case != 0 {
                let mut folded = String::new();
                let mut map = Vec::new();
                let mut offset = bounds[0];
                for ch in source.chars() {
                    let end = offset + ch.len_utf16();
                    for lower in ch.to_lowercase() {
                        for _ in 0..lower.len_utf8() {
                            map.push((offset, end));
                        }
                        folded.push(lower);
                    }
                    offset = end;
                }
                (folded, needle.to_lowercase(), map)
            } else {
                let mut map = Vec::new();
                let mut offset = bounds[0];
                for ch in source.chars() {
                    let end = offset + ch.len_utf16();
                    for _ in 0..ch.len_utf8() {
                        map.push((offset, end));
                    }
                    offset = end;
                }
                (source, needle.clone(), map)
            };
            let found = if backward != 0 {
                haystack.rfind(&needle)
            } else {
                haystack.find(&needle)
            };
            let Some(byte) = found else {
                return Ok(None);
            };
            let start = map[byte].0;
            let end = map[byte + needle.len() - 1].1;
            Ok(Some(TextRange::spanning(
                self.key.clone(),
                doc.floor(start),
                doc.characters
                    .iter()
                    .copied()
                    .find(|&p| p >= end)
                    .unwrap_or(doc.len()),
            )))
        })
    }

    /// The attribute's value; `None` only when UI Automation answered its
    /// not-supported request with nothing, which leaves the caller's VARIANT
    /// unwritten, as before.
    pub(super) fn get_attribute_value(&self, id: UIA_TEXTATTRIBUTE_ID) -> Result<Option<Variant>> {
        self.read(|node, _, _, _| match id {
            UIA_IsReadOnlyAttributeId => Ok(Some(Variant::bool(
                !node.data().supports_action(Action::ReplaceSelectedText)
                    && !node.data().supports_action(Action::SetValue),
            ))),
            UIA_IsHiddenAttributeId => Ok(Some(Variant::bool(node.is_offscreen()))),
            _ => {
                let mut out = std::ptr::null_mut();
                // SAFETY: `out` receives UI Automation's owned sentinel.
                let hr = unsafe { UiaGetReservedNotSupportedValue(&mut out) };
                if hr < 0 {
                    return Err(hr);
                }
                // SAFETY: a successful call hands over the reference `out` holds.
                Ok(unsafe { Com::from_raw(out) }.map(Variant::unknown))
            }
        })
    }

    pub(super) fn get_bounding_rectangles(&self) -> Result<*mut SAFEARRAY> {
        let boxes = self.read(|node, _, state, bounds| {
            let range = crate::platform::text_range(node, bounds[0], bounds[1] - bounds[0])
                .ok_or(NOT_SUPPORTED)?;
            Ok(range
                .bounding_boxes()
                .into_iter()
                .flat_map(|rect| {
                    let rect = to_uia_rect(rect, state.origin);
                    [rect.left, rect.top, rect.width, rect.height]
                })
                .collect::<Vec<_>>())
        })?;
        array_f64(&boxes)
    }

    pub(super) fn get_enclosing_element(&self) -> Result<Com> {
        self.key.provider()
    }

    pub(super) fn get_text(&self, max: i32) -> Result<Bstr> {
        if max < -1 {
            return Err(E_INVALIDARG);
        }
        self.read(|_, doc, _, bounds| {
            let end = if max == -1 {
                bounds[1]
            } else {
                doc.floor(bounds[1].min(bounds[0].saturating_add(max as usize)))
            };
            Bstr::new(&doc.slice(bounds[0], end))
        })
    }

    pub(super) fn move_by(&self, by: TextUnit, count: i32) -> Result<i32> {
        let by = unit(by)?;
        self.read(|_, doc, _, bounds| {
            if count == 0 {
                return Ok(0);
            }
            if bounds[0] == bounds[1] {
                let (at, moved) = doc.step(bounds[0], by, count);
                *bounds = [at, at];
                return Ok(moved);
            }
            let (start, _) = doc.enclosing(bounds[0], by);
            let (at, mut moved) = doc.step(start, by, count);
            let (next, end) = doc.enclosing(at, by);
            if count > 0 && next < at {
                moved = moved.saturating_sub(1);
            }
            *bounds = [next, end];
            Ok(moved)
        })
    }

    pub(super) fn move_endpoint_by_unit(
        &self,
        end: TextPatternRangeEndpoint,
        by: TextUnit,
        count: i32,
    ) -> Result<i32> {
        let end = endpoint(end)?;
        let by = unit(by)?;
        self.read(|_, doc, _, bounds| {
            let (at, moved) = doc.step(bounds[end], by, count);
            bounds[end] = at;
            if bounds[0] > bounds[1] {
                bounds[1 - end] = at;
            }
            Ok(moved)
        })
    }

    pub(super) fn move_endpoint_by_range(
        &self,
        end: TextPatternRangeEndpoint,
        other: Raw,
        other_end: TextPatternRangeEndpoint,
    ) -> Result<()> {
        let end = endpoint(end)?;
        let other_end = endpoint(other_end)?;
        let other = self.other(other)?;
        let at = other.range().bounds.lock_or_recover()[other_end];
        self.read(|_, doc, _, bounds| {
            let at = doc.floor(at);
            bounds[end] = at;
            if bounds[0] > bounds[1] {
                bounds[1 - end] = at;
            }
            Ok(())
        })
    }

    pub(super) fn select(&self) -> Result<()> {
        let selection = self.read(|node, _, _, bounds| {
            crate::platform::text_selection(node, bounds[0], bounds[1] - bounds[0])
                .ok_or(NOT_SUPPORTED)
        })?;
        self.key.dispatch(
            Action::SetTextSelection,
            Some(ActionData::SetTextSelection(selection)),
        )
    }

    pub(super) fn scroll_into_view(&self) -> Result<()> {
        self.key.dispatch(Action::ScrollIntoView, None)
    }

    pub(super) fn get_children(&self) -> Result<*mut SAFEARRAY> {
        self.read(|_, _, _, _| array_com(&[]))
    }
}
