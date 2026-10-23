// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TextPattern and retained TextRange COM objects over shared model text.

use super::*;
use crate::uia_text::{Document, Unit};

fn unit(unit: TextUnit) -> Result<Unit> {
    Ok(match unit {
        TextUnit_Character => Unit::Character,
        TextUnit_Format | TextUnit_Word => Unit::Word,
        TextUnit_Line => Unit::Line,
        TextUnit_Paragraph => Unit::Paragraph,
        TextUnit_Page | TextUnit_Document => Unit::Document,
        _ => return Err(E_INVALIDARG.into()),
    })
}

fn endpoint(endpoint: TextPatternRangeEndpoint) -> Result<usize> {
    match endpoint {
        TextPatternRangeEndpoint_Start => Ok(0),
        TextPatternRangeEndpoint_End => Ok(1),
        _ => Err(E_INVALIDARG.into()),
    }
}

#[implement(nullable::Range)]
pub(super) struct TextRange {
    key: Key,
    bounds: Mutex<[usize; 2]>,
}

impl TextRange {
    fn new(key: Key, start: usize, end: usize) -> ITextRangeProvider {
        ComObject::new(Self {
            key,
            bounds: Mutex::new([start.min(end), start.max(end)]),
        })
        .to_interface::<nullable::Range>()
        .0
    }

    fn read<T>(
        &self,
        f: impl FnOnce(Node<'_>, &Document, &State, &mut [usize; 2]) -> Result<T>,
    ) -> Result<T> {
        self.key.read(|node, state| {
            let document = Document::new(node).ok_or_else(unsupported)?;
            let mut bounds = self.bounds.lock_or_recover();
            bounds[0] = document.floor(bounds[0]);
            bounds[1] = document.floor(bounds[1]).max(bounds[0]);
            f(node, &document, state, &mut bounds)
        })
    }

    fn other<'a>(&self, other: &'a ITextRangeProvider) -> Result<&'a TextRange_Impl> {
        let other = other
            .cast_object_ref::<TextRange>()
            .map_err(|_| Error::from(E_INVALIDARG))?;
        if !self.key.shared.ptr_eq(&other.key.shared) {
            return Err(E_INVALIDARG.into());
        }
        self.key.read(|node, state| {
            let other_node = match &other.key.handle {
                Some(handle) => state.publication.node(handle),
                None => state.publication.tree().map(crate::tree::State::root),
            }
            .ok_or_else(unavailable)?;
            if node.id() != other_node.id() {
                return Err(E_INVALIDARG.into());
            }
            Ok(())
        })?;
        Ok(other)
    }
}

impl ITextProvider_Impl for Provider_Impl {
    fn GetSelection(&self) -> Result<*mut SAFEARRAY> {
        let (key, selection) = self.key().read(|node, state| {
            if !node.supports_text_ranges() {
                return Err(unsupported());
            }
            Ok((
                Key::capture(&self.shared, node.id(), state)?,
                crate::platform::selection(node),
            ))
        })?;
        let values = selection
            .map(|(start, len)| TextRange::new(key, start, start + len).cast::<IUnknown>())
            .transpose()?
            .into_iter()
            .collect::<Vec<_>>();
        array_com(&values)
    }
    fn GetVisibleRanges(&self) -> Result<*mut SAFEARRAY> {
        array_com(&[self.DocumentRange()?.cast()?])
    }
    fn RangeFromChild(&self, child: Ref<IRawElementProviderSimple>) -> Result<ITextRangeProvider> {
        let child = child.as_ref().ok_or_else(|| Error::from(E_INVALIDARG))?;
        let child = child
            .cast_object_ref::<Provider>()
            .map_err(|_| Error::from(E_INVALIDARG))?;
        if !self.shared.ptr_eq(&child.shared) {
            return Err(E_INVALIDARG.into());
        }
        self.key().read(|node, _| {
            if node.supports_text_ranges() {
                Err(unsupported())
            } else {
                Err(E_INVALIDARG.into())
            }
        })
    }
    fn RangeFromPoint(&self, point: &UiaPoint) -> Result<ITextRangeProvider> {
        if !point.x.is_finite() || !point.y.is_finite() {
            return Err(E_INVALIDARG.into());
        }
        let (key, offset) = self.key().read(|node, state| {
            if !node.supports_text_ranges() {
                return Err(unsupported());
            }
            let local = node.transform().inverse()
                * Point::new(point.x - state.origin.0, point.y - state.origin.1);
            let position = node.text_position_at_point(local);
            let text = crate::platform::text(node).ok_or_else(unsupported)?;
            Ok((
                Key::capture(&self.shared, node.id(), state)?,
                text.chars()
                    .take(position.to_global_usv_index())
                    .map(char::len_utf16)
                    .sum(),
            ))
        })?;
        Ok(TextRange::new(key, offset, offset))
    }
    fn DocumentRange(&self) -> Result<ITextRangeProvider> {
        let (key, len) = self.key().read(|node, state| {
            Ok((
                Key::capture(&self.shared, node.id(), state)?,
                Document::new(node).ok_or_else(unsupported)?.len(),
            ))
        })?;
        Ok(TextRange::new(key, 0, len))
    }
    fn SupportedTextSelection(&self) -> Result<SupportedTextSelection> {
        self.key().read(|node, _| {
            Ok(if node.data().supports_action(Action::SetTextSelection) {
                SupportedTextSelection_Single
            } else {
                SupportedTextSelection_None
            })
        })
    }
}

impl ITextProvider2_Impl for Provider_Impl {
    fn RangeFromAnnotation(
        &self,
        _annotation: Ref<IRawElementProviderSimple>,
    ) -> Result<ITextRangeProvider> {
        Err(unsupported())
    }
    fn GetCaretRange(&self, active: *mut BOOL) -> Result<ITextRangeProvider> {
        if active.is_null() {
            return Err(E_INVALIDARG.into());
        }
        let (key, offset, focused) = self.key().read(|node, state| {
            let position = node.text_selection_focus().ok_or_else(unsupported)?;
            let text = crate::platform::text(node).ok_or_else(unsupported)?;
            Ok((
                Key::capture(&self.shared, node.id(), state)?,
                text.chars()
                    .take(position.to_global_usv_index())
                    .map(char::len_utf16)
                    .sum(),
                node.is_focused(),
            ))
        })?;
        unsafe {
            active.write(focused.into());
        }
        Ok(TextRange::new(key, offset, offset))
    }
}

impl ITextRangeProvider_Impl for TextRange_Impl {
    fn Clone(&self) -> Result<ITextRangeProvider> {
        self.read(|_, _, _, bounds| Ok(TextRange::new(self.key.clone(), bounds[0], bounds[1])))
    }
    fn Compare(&self, other: Ref<ITextRangeProvider>) -> Result<BOOL> {
        let other = self.other(other.as_ref().ok_or_else(|| Error::from(E_INVALIDARG))?)?;
        let theirs = *other.bounds.lock_or_recover();
        self.read(|_, doc, _, bounds| {
            Ok((*bounds == [doc.floor(theirs[0]), doc.floor(theirs[1])]).into())
        })
    }
    fn CompareEndpoints(
        &self,
        end: TextPatternRangeEndpoint,
        other: Ref<ITextRangeProvider>,
        other_end: TextPatternRangeEndpoint,
    ) -> Result<i32> {
        let end = endpoint(end)?;
        let other_end = endpoint(other_end)?;
        let other = self.other(other.as_ref().ok_or_else(|| Error::from(E_INVALIDARG))?)?;
        let theirs = other.bounds.lock_or_recover()[other_end];
        self.read(|_, doc, _, bounds| {
            Ok(match bounds[end].cmp(&doc.floor(theirs)) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            })
        })
    }
    fn ExpandToEnclosingUnit(&self, by: TextUnit) -> Result<()> {
        let by = unit(by)?;
        self.read(|_, doc, _, bounds| {
            let (start, end) = doc.enclosing(bounds[0], by);
            *bounds = [start, end];
            Ok(())
        })
    }
    fn FindAttribute(
        &self,
        id: UIA_TEXTATTRIBUTE_ID,
        value: &VARIANT,
        backward: BOOL,
    ) -> Result<ITextRangeProvider> {
        if id != UIA_IsReadOnlyAttributeId {
            return Err(unsupported());
        }
        let expected = bool::try_from(value).map_err(|_| Error::from(E_INVALIDARG))?;
        self.read(|node, _, _, bounds| {
            let read_only = !node.data().supports_action(Action::ReplaceSelectedText)
                && !node.data().supports_action(Action::SetValue);
            if expected != read_only {
                return Err(Error::empty());
            }
            let _ = backward;
            Ok(TextRange::new(self.key.clone(), bounds[0], bounds[1]))
        })
    }
    fn FindText(
        &self,
        text: &BSTR,
        backward: BOOL,
        ignore_case: BOOL,
    ) -> Result<ITextRangeProvider> {
        let needle = text.to_string();
        if needle.is_empty() {
            return Err(E_INVALIDARG.into());
        }
        self.read(|_, doc, _, bounds| {
            let source = doc.slice(bounds[0], bounds[1]);
            let (haystack, needle, map) = if ignore_case.as_bool() {
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
            let byte = if backward.as_bool() {
                haystack.rfind(&needle)
            } else {
                haystack.find(&needle)
            }
            .ok_or_else(Error::empty)?;
            let start = map[byte].0;
            let end = map[byte + needle.len() - 1].1;
            Ok(TextRange::new(
                self.key.clone(),
                doc.floor(start),
                doc.characters
                    .iter()
                    .copied()
                    .find(|&p| p >= end)
                    .unwrap_or(doc.len()),
            ))
        })
    }
    fn GetAttributeValue(&self, id: UIA_TEXTATTRIBUTE_ID) -> Result<VARIANT> {
        self.read(|node, _, _, _| match id {
            UIA_IsReadOnlyAttributeId => {
                Ok((!node.data().supports_action(Action::ReplaceSelectedText)
                    && !node.data().supports_action(Action::SetValue))
                .into())
            }
            UIA_IsHiddenAttributeId => Ok(node.is_offscreen().into()),
            _ => unsafe { Ok(UiaGetReservedNotSupportedValue()?.into()) },
        })
    }
    fn GetBoundingRectangles(&self) -> Result<*mut SAFEARRAY> {
        let boxes = self.read(|node, _, state, bounds| {
            let range = crate::platform::text_range(node, bounds[0], bounds[1] - bounds[0])
                .ok_or_else(unsupported)?;
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
    fn GetEnclosingElement(&self) -> Result<IRawElementProviderSimple> {
        self.key.provider()
    }
    fn GetText(&self, max: i32) -> Result<BSTR> {
        if max < -1 {
            return Err(E_INVALIDARG.into());
        }
        self.read(|_, doc, _, bounds| {
            let end = if max == -1 {
                bounds[1]
            } else {
                doc.floor(bounds[1].min(bounds[0].saturating_add(max as usize)))
            };
            Ok(BSTR::from(doc.slice(bounds[0], end)))
        })
    }
    fn Move(&self, by: TextUnit, count: i32) -> Result<i32> {
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
    fn MoveEndpointByUnit(
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
    fn MoveEndpointByRange(
        &self,
        end: TextPatternRangeEndpoint,
        other: Ref<ITextRangeProvider>,
        other_end: TextPatternRangeEndpoint,
    ) -> Result<()> {
        let end = endpoint(end)?;
        let other_end = endpoint(other_end)?;
        let other = self.other(other.as_ref().ok_or_else(|| Error::from(E_INVALIDARG))?)?;
        let at = other.bounds.lock_or_recover()[other_end];
        self.read(|_, doc, _, bounds| {
            let at = doc.floor(at);
            bounds[end] = at;
            if bounds[0] > bounds[1] {
                bounds[1 - end] = at;
            }
            Ok(())
        })
    }
    fn Select(&self) -> Result<()> {
        let selection = self.read(|node, _, _, bounds| {
            crate::platform::text_selection(node, bounds[0], bounds[1] - bounds[0])
                .ok_or_else(unsupported)
        })?;
        self.key.dispatch(
            Action::SetTextSelection,
            Some(ActionData::SetTextSelection(selection)),
        )
    }
    fn AddToSelection(&self) -> Result<()> {
        Err(unsupported())
    }
    fn RemoveFromSelection(&self) -> Result<()> {
        Err(unsupported())
    }
    fn ScrollIntoView(&self, _top: BOOL) -> Result<()> {
        self.key.dispatch(Action::ScrollIntoView, None)
    }
    fn GetChildren(&self) -> Result<*mut SAFEARRAY> {
        self.read(|_, _, _, _| array_com(&[]))
    }
}
