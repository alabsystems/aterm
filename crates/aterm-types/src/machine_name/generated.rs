// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! OS names generated from the saved machine number and nickname, never an observed hostname.

use super::{DNS_LABEL_MAX, Nickname, Number, OsSlot};

/// One canonical choice's human-readable computer name and DNS-safe network name.
/// The stable fleet identity remains [`Self::number`]. An OS collision suffix is an
/// observation, never input to this derivation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratedNames {
    number: Number,
    nickname: Option<Nickname>,
    computer: String,
    host: String,
}

impl GeneratedNames {
    /// Generate `m85 Dog` and `m85-dog` from the saved number and nickname.
    /// Computer punctuation becomes word boundaries. Network apostrophes are omitted;
    /// other punctuation separates labels. Non-ASCII network letters use their
    /// Unicode scalar spelling (`u7814`). Each OS value stays within 63 bytes.
    #[must_use]
    pub fn new(number: Number, nickname: &Nickname) -> Self {
        let human = nickname
            .display()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let mut computer = format!("{number} {human}");
        cut(&mut computer);
        let mut slug = String::new();
        for c in nickname.as_str().chars() {
            if c.is_ascii_alphanumeric() {
                slug.push(c.to_ascii_lowercase());
            } else if c.is_alphanumeric() {
                boundary(&mut slug);
                slug.push_str(&format!("u{:x}", u32::from(c)));
                slug.push('-');
            } else if c != '\'' {
                boundary(&mut slug);
            }
        }
        let mut host = format!("{number}-{}", slug.trim_end_matches('-'));
        cut(&mut host);
        Self {
            number,
            nickname: Some(nickname.clone()),
            computer,
            host,
        }
    }

    /// A bounded provisional name before a canonical nickname exists. An unread
    /// canonical record must not fall back to this value.
    #[must_use]
    pub fn number_only(number: Number) -> Self {
        let word = number.to_string();
        Self {
            number,
            nickname: None,
            computer: word.clone(),
            host: word,
        }
    }

    /// The stable machine number, independent of OS collision handling.
    #[must_use]
    pub const fn number(&self) -> Number {
        self.number
    }

    /// The exact saved nickname, absent only for a provisional number.
    #[must_use]
    pub const fn nickname(&self) -> Option<&Nickname> {
        self.nickname.as_ref()
    }

    /// Finder, AirDrop and Sharing's computer name.
    #[must_use]
    pub fn computer_name(&self) -> &str {
        &self.computer
    }

    /// HostName and the Bonjour label, without `.local`.
    #[must_use]
    pub fn host_name(&self) -> &str {
        &self.host
    }

    /// The expected value of a macOS slot; no value for another OS's slot.
    #[must_use]
    pub fn for_slot(&self, slot: OsSlot) -> Option<&str> {
        match slot {
            OsSlot::MacComputerName => Some(self.computer_name()),
            OsSlot::MacHostName | OsSlot::MacLocalHostName => Some(self.host_name()),
            _ => None,
        }
    }

    /// Whether the observed value exactly matches the generated target.
    #[must_use]
    pub fn exact(&self, slot: OsSlot, observed: &str) -> bool {
        self.for_slot(slot) == Some(observed)
    }

    /// A numeric Bonjour collision alias of this exact target. It remains a
    /// mismatch for display and completion, but must not trigger a rename loop.
    #[must_use]
    pub fn collision(&self, slot: OsSlot, observed: &str) -> bool {
        slot.collision(self.host_name(), observed)
    }

    /// Values a setter leaves alone: exact names and a diagnosed Bonjour alias.
    #[must_use]
    pub fn accepts(&self, slot: OsSlot, observed: &str) -> bool {
        self.exact(slot, observed) || self.collision(slot, observed)
    }
}

fn boundary(word: &mut String) {
    if !word.is_empty() && !word.ends_with('-') {
        word.push('-');
    }
}

fn cut(word: &mut String) {
    let mut end = word.len().min(DNS_LABEL_MAX);
    while !word.is_char_boundary(end) {
        end -= 1;
    }
    word.truncate(end);
    while word.ends_with(' ') || word.ends_with('-') {
        word.pop();
    }
}
