// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Project declared capabilities from the root plist dictionary, never incidental XML text.

use super::{
    Capabilities, HANDOFF_GRANT_CHUNKS_TOKEN, HANDOFF_GRANT_DESK_TOKEN, HANDOFF_GRANT_EMPTY_TOKEN,
    NATIVE_LEGACY_RECIPIENT_RECOVERY_TOKEN, NATIVE_RECIPIENT_RECOVERY_TOKEN,
};
use crate::NativeRecipientRecovery;
use aterm_codec::xml::{Event, Reader, Tag};
use std::collections::BTreeSet;

impl Capabilities {
    pub(super) fn from_plist(text: &str) -> Self {
        parse(text).unwrap_or_default()
    }
}

fn significant<'a>(reader: &mut Reader<'a>) -> Option<Event<'a>> {
    loop {
        match reader.next_event().ok()?? {
            Event::Text(text) if text.trim().is_empty() => {}
            event => return Some(event),
        }
    }
}

fn scalar(reader: &mut Reader<'_>, tag: &Tag<'_>) -> Option<String> {
    if tag.attrs().next().is_some() {
        return None;
    }
    let mut value = String::new();
    if !tag.empty {
        loop {
            match reader.next_event().ok()?? {
                Event::Text(text) => value.push_str(&text),
                Event::End(name) if name == tag.name => break,
                _ => return None,
            }
        }
    }
    Some(value)
}

fn skip_value(reader: &mut Reader<'_>, tag: &Tag<'_>) -> Option<()> {
    if !matches!(
        tag.name,
        "dict" | "array" | "string" | "integer" | "real" | "data" | "date" | "true" | "false"
    ) {
        return None;
    }
    let mut depth = usize::from(!tag.empty);
    while depth != 0 {
        match reader.next_event().ok()?? {
            Event::Start(tag) if !tag.empty => depth += 1,
            Event::End(_) => depth -= 1,
            _ => {}
        }
    }
    Some(())
}

fn parse(text: &str) -> Option<Capabilities> {
    let mut reader = Reader::new(text);
    let Event::Start(root) = significant(&mut reader)? else {
        return None;
    };
    if root.name != "plist" || root.empty {
        return None;
    }
    let Event::Start(dict) = significant(&mut reader)? else {
        return None;
    };
    if dict.name != "dict" || dict.attrs().next().is_some() {
        return None;
    }
    let mut result = Capabilities::default();
    let mut seen = BTreeSet::new();
    if !dict.empty {
        loop {
            let event = significant(&mut reader)?;
            if event == Event::End("dict") {
                break;
            }
            let Event::Start(key) = event else {
                return None;
            };
            if key.name != "key" {
                return None;
            }
            let key = scalar(&mut reader, &key)?;
            if key.is_empty() || !seen.insert(key.clone()) {
                return None;
            }
            let Event::Start(value) = significant(&mut reader)? else {
                return None;
            };
            if key == "ATermNativeRecipientRecovery" {
                result.recipient_recovery = if value.name == "string" {
                    match scalar(&mut reader, &value)?.trim() {
                        NATIVE_RECIPIENT_RECOVERY_TOKEN => NativeRecipientRecovery::BoundJournal,
                        NATIVE_LEGACY_RECIPIENT_RECOVERY_TOKEN => {
                            NativeRecipientRecovery::LegacyJournal
                        }
                        _ => NativeRecipientRecovery::Absent,
                    }
                } else {
                    skip_value(&mut reader, &value)?;
                    NativeRecipientRecovery::Absent
                };
                continue;
            }
            let declaration = match key.as_str() {
                "ATermHandoffGrantChunks" => {
                    Some((&mut result.grant_chunks, HANDOFF_GRANT_CHUNKS_TOKEN))
                }
                "ATermHandoffGrantEmpty" => {
                    Some((&mut result.grant_empty, HANDOFF_GRANT_EMPTY_TOKEN))
                }
                "ATermHandoffGrantDesk" => Some((&mut result.grant_desk, HANDOFF_GRANT_DESK_TOKEN)),
                _ => None,
            };
            if let Some((declared, token)) = declaration
                && value.name == "string"
            {
                *declared = scalar(&mut reader, &value)?.trim() == token;
            } else {
                skip_value(&mut reader, &value)?;
            }
        }
    }
    if significant(&mut reader)? != Event::End("plist") {
        return None;
    }
    loop {
        match reader.next_event().ok()? {
            Some(Event::Text(text)) if text.trim().is_empty() => {}
            None => return Some(result),
            _ => return None,
        }
    }
}

#[cfg(test)]
#[path = "bundle_capabilities_tests.rs"]
mod tests;
