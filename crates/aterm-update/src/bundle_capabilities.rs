// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Project declared capabilities from the root plist dictionary, never incidental XML text.

use super::{
    Capabilities, HANDOFF_GRANT_CHUNKS_TOKEN, HANDOFF_GRANT_DESK_TOKEN, HANDOFF_GRANT_EMPTY_TOKEN,
    NATIVE_RECIPIENT_RECOVERY_TOKEN,
};
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
            let declaration = match key.as_str() {
                "ATermHandoffGrantChunks" => {
                    Some((&mut result.grant_chunks, HANDOFF_GRANT_CHUNKS_TOKEN))
                }
                "ATermHandoffGrantEmpty" => {
                    Some((&mut result.grant_empty, HANDOFF_GRANT_EMPTY_TOKEN))
                }
                "ATermHandoffGrantDesk" => Some((&mut result.grant_desk, HANDOFF_GRANT_DESK_TOKEN)),
                "ATermNativeRecipientRecovery" => Some((
                    &mut result.recipient_recovery,
                    NATIVE_RECIPIENT_RECOVERY_TOKEN,
                )),
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
mod tests {
    use super::*;

    /// A substring search admits the comment and nested-dictionary negative controls.
    #[test]
    fn only_unique_root_dictionary_declarations_are_capabilities() {
        let declarations = "<key>ATermHandoffGrantChunks</key><string>chunks1</string>\
            <key>ATermHandoffGrantEmpty</key><string>empty1</string>\
            <key>ATermHandoffGrantDesk</key><string>desk1</string>\
            <key>ATermNativeRecipientRecovery</key><string>recipients5</string>";
        let wrap = |body: &str| format!("<plist version=\"1.0\"><dict>{body}</dict></plist>");
        let all = Capabilities {
            grant_chunks: true,
            grant_empty: true,
            grant_desk: true,
            recipient_recovery: true,
        };
        assert_eq!(Capabilities::from_plist(&wrap(declarations)), all);
        for text in [
            wrap(&format!("<!-- {declarations} -->")),
            wrap(&format!("<key>nested</key><dict>{declarations}</dict>")),
            wrap(&format!(
                "<key>nested</key><array><dict>{declarations}</dict></array>"
            )),
            wrap(&format!("{declarations}{declarations}")),
            format!("<plist><dict>{declarations}</dict>"),
            format!("{}<plist><dict/></plist>", wrap(declarations)),
            declarations.to_string(),
        ] {
            assert_eq!(
                Capabilities::from_plist(&text),
                Capabilities::default(),
                "{text}"
            );
        }
        let recovery =
            |value: &str| wrap(&format!("<key>ATermNativeRecipientRecovery</key>{value}"));
        for value in [
            "<string>recipients6</string>",
            "<string>Recipients5</string>",
            "<true/>",
            "<string/>",
            "<string><string>recipients5</string></string>",
        ] {
            assert_eq!(
                Capabilities::from_plist(&recovery(value)),
                Capabilities::default()
            );
        }
        assert!(
            Capabilities::from_plist(&recovery("<string> recipients&#53; </string>"))
                .recipient_recovery
        );
    }
}
