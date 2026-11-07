// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What non-Secret ATT/1 cells may not hold (design §3.4): a credential-shaped string, free text that names
//! a secret (so a plain-word password cannot ride a note), a full phone number (it lives in the
//! vault; a phone row keeps its last four digits), or an address outside a typed `email` cell
//! (so every address is masked). A refusal names the rule and never quotes the
//! value. Secret columns bypass these text guards only with an opaque inner seal;
//! plaintext is rejected by their type check ([`super::cell::validate`]).

/// Why a value was refused: a rule, never the value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A known token, key or seed shape.
    CredentialShape,
    /// Free text that names a secret.
    SecretWord,
    /// An address outside a typed `email` cell.
    Address,
    /// A control character.
    Control,
    /// Longer than the column allows.
    Length,
    /// A full phone number.
    Phone,
    /// An account's name that names a machine.
    MachineLabel,
}

impl Refusal {
    /// The rule in words, with no part of the value.
    pub const fn rule(self) -> &'static str {
        match self {
            Self::CredentialShape => "a credential-shaped value is never stored",
            Self::SecretWord => "free text naming a secret is never stored; keep it in the vault",
            Self::Address => "an address belongs in a typed email column",
            Self::Control => "text holds no control characters",
            Self::Length => "text is longer than its column allows",
            Self::Phone => {
                "a phone number lives in the vault; a phone row keeps its last four digits"
            }
            Self::MachineLabel => {
                "an account's label names no machine: a preference row maps it to one"
            }
        }
    }
}

/// Token prefixes that mark a credential when they start a word.
const PREFIXES: [&str; 16] = [
    "github_pat_",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "sk-ant-",
    "sk-",
    "xoxa-",
    "xoxb-",
    "xoxp-",
    "xoxr-",
    "xoxs-",
    "AKIA",
    "-----BEGIN",
    "otpauth:",
];

/// Prefixes that mark a provider key only when a key's run of token characters follows (a
/// Google API key, an xAI key, a Google OAuth access token), with the shortest such run, so a
/// word such as `xai-web` stays.
const KEYED: [(&str, usize); 3] = [("AIza", 30), ("xai-", 20), ("ya29.", 20)];

/// A run of token characters this long reads as a key or secret.
const LONG_RUN: usize = 40;

/// The fewest characters of a base32 TOTP seed this guard refuses.
const SEED_CHARS: usize = 16;

/// Words that name a secret in free text, matched without case.
const SECRET_WORDS: [&str; 16] = [
    "password",
    "passwd",
    "passcode",
    "passphrase",
    "totp",
    "otpauth",
    "recovery code",
    "backup code",
    "private key",
    "secret key",
    "api key",
    "api_key",
    "apikey",
    "2fa code",
    "pw:",
    "pwd:",
];

/// Whether a byte belongs to a token run.
const fn token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'_' | b'-' | b'.')
}

/// Whether `text` carries a known credential shape: a token prefix at the start of a word, a
/// provider key's prefix with its run, a base32 TOTP seed, or a long unbroken run of token
/// characters.
pub fn credential_shaped(text: &str) -> bool {
    let bytes = text.as_bytes();
    for (at, &b) in bytes.iter().enumerate() {
        if !matches!(b, b'g' | b's' | b'x' | b'A' | b'-' | b'o' | b'y') {
            continue;
        }
        let boundary = at == 0 || !bytes[at - 1].is_ascii_alphanumeric();
        if !boundary {
            continue;
        }
        let rest = &text[at..];
        if PREFIXES.iter().any(|prefix| rest.starts_with(prefix)) {
            return true;
        }
        if KEYED.iter().any(|(prefix, tail)| {
            rest.strip_prefix(prefix)
                .is_some_and(|after| after.bytes().take_while(|b| token_byte(*b)).count() >= *tail)
        }) {
            return true;
        }
    }
    if base32_seed(text) {
        return true;
    }
    let mut run = 0;
    for b in bytes {
        if b.is_ascii_alphanumeric() || b"+/=_-".contains(b) {
            run += 1;
            if run >= LONG_RUN {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

/// The fewest digits a phone number has.
const PHONE_DIGITS: usize = 7;

/// Whether `text` holds a phone number: a run of at least seven digits, standing apart from any
/// word, that spaces, `-`, `.`, `(`, `)` and a leading `+` may separate. A leading calendar date
/// (`2026-11-01`) is not counted, nor is a dotted IPv4 address.
pub fn phone_shaped(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() < PHONE_DIGITS {
        return false;
    }
    let mut at = 0;
    while at < bytes.len() {
        let starts = matches!(bytes[at], b'0'..=b'9' | b'+' | b'(')
            && (at == 0 || !bytes[at - 1].is_ascii_alphanumeric());
        if !starts {
            at += 1;
            continue;
        }
        let mut total_digits = 0usize;
        let mut end = at;
        let mut after_digit = at;
        while end < bytes.len() {
            match bytes[end] {
                b'0'..=b'9' => {
                    total_digits += 1;
                    after_digit = end + 1;
                }
                b' ' | b'-' | b'.' | b'(' | b')' => {}
                b'+' if end == at => {}
                _ => break,
            }
            end += 1;
        }
        let apart = after_digit == bytes.len() || !bytes[after_digit].is_ascii_alphanumeric();
        if total_digits >= PHONE_DIGITS && apart {
            let mut groups = [(0usize, 0u8); 8];
            let mut groups_len = 0usize;
            let mut push_group = |digits: usize, separator: u8| {
                if groups_len < groups.len() {
                    groups[groups_len] = (digits, separator);
                    groups_len += 1;
                } else {
                    groups[groups.len() - 1].0 += digits;
                }
            };
            let mut digits = 0;
            let mut separator = b' ';
            for (idx, &b) in bytes[at..end].iter().enumerate() {
                match b {
                    b'0'..=b'9' => digits += 1,
                    b' ' | b'-' | b'.' | b'(' | b')' => {
                        if digits > 0 {
                            push_group(digits, separator);
                            digits = 0;
                        }
                        separator = b;
                    }
                    b'+' if idx == 0 => {}
                    _ => break,
                }
            }
            if digits > 0 {
                push_group(digits, separator);
            }
            if phone_groups(&groups[..groups_len]) {
                return true;
            }
        }
        at = end.max(at + 1);
    }
    false
}

/// Whether one run's digit groups (each with the separator before it) read as a phone number.
fn phone_groups(groups: &[(usize, u8)]) -> bool {
    let dated = matches!(
        groups,
        [(4, _), (1 | 2, b'-' | b'.'), (1 | 2, b'-' | b'.'), ..]
    );
    let rest = if dated { &groups[3..] } else { groups };
    let ipv4 = rest.len() == 4
        && rest.iter().all(|(n, _)| *n <= 3)
        && rest[1..].iter().all(|(_, separator)| *separator == b'.');
    !ipv4 && rest.iter().map(|(n, _)| n).sum::<usize>() >= PHONE_DIGITS
}

/// Whether `word` is base32 in one case (`A`–`Z` or `a`–`z`, and `2`–`7`).
fn base32(word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    let mut has_upper = false;
    let mut has_lower = false;
    for b in word.bytes() {
        if b.is_ascii_uppercase() {
            if has_lower {
                return false;
            }
            has_upper = true;
        } else if b.is_ascii_lowercase() {
            if has_upper {
                return false;
            }
            has_lower = true;
        } else if !(b'2'..=b'7').contains(&b) {
            return false;
        }
    }
    true
}

/// Whether `text` holds a base32 TOTP seed: one word of at least 16 base32 characters, or at
/// least four space-separated groups of four (`JBSW Y3DP EHPK 3PXP`), holding a digit `2`–`7`
/// (so plain words in capitals stay) and a letter past `f` (so a hex digest stays).
fn base32_seed(text: &str) -> bool {
    if text.len() < SEED_CHARS {
        return false;
    }
    let digit = |word: &str| {
        word.bytes().any(|b| (b'2'..=b'7').contains(&b))
            && word.bytes().any(|b| b.to_ascii_lowercase() > b'f')
    };
    let words = text.split(|c: char| !c.is_ascii_alphanumeric() && c != '@');
    if words
        .into_iter()
        .any(|word| word.len() >= SEED_CHARS && base32(word) && digit(word))
    {
        return true;
    }
    const GROUP_RUN: usize = SEED_CHARS / 4;
    if text.len() < SEED_CHARS + (GROUP_RUN - 1) {
        return false;
    }
    let mut run = [""; GROUP_RUN];
    let mut filled = 0usize;
    for group in text.split(' ') {
        if group.len() == 4 && base32(group) {
            run.copy_within(1.., 0);
            run[GROUP_RUN - 1] = group;
            filled += 1;
            if filled >= GROUP_RUN
                && (run
                    .iter()
                    .all(|g| g.bytes().all(|b| !b.is_ascii_lowercase()))
                    || run
                        .iter()
                        .all(|g| g.bytes().all(|b| !b.is_ascii_uppercase())))
                && run.iter().any(|g| digit(g))
            {
                return true;
            }
        } else {
            filled = 0;
        }
    }
    false
}

/// Whether free text names a secret.
pub fn names_secret(text: &str) -> bool {
    let bytes = text.as_bytes();
    SECRET_WORDS.iter().any(|word| {
        bytes
            .windows(word.len())
            .any(|w| w.eq_ignore_ascii_case(word.as_bytes()))
    })
}

/// Whether `text` is never shown, not even in a local review on the owner's own Mac: it is shaped
/// like a credential or names a secret.
pub fn withheld(text: &str) -> bool {
    credential_shaped(text) || names_secret(text)
}

/// Check one free-text value of at most `max` bytes.
///
/// # Errors
/// The rule it breaks; the value is never quoted.
pub fn text(value: &str, max: usize) -> Result<(), Refusal> {
    if value.len() > max {
        return Err(Refusal::Length);
    }
    if value.chars().any(char::is_control) {
        return Err(Refusal::Control);
    }
    if value.contains('@') {
        return Err(Refusal::Address);
    }
    if credential_shaped(value) {
        return Err(Refusal::CredentialShape);
    }
    if names_secret(value) {
        return Err(Refusal::SecretWord);
    }
    if phone_shaped(value) {
        return Err(Refusal::Phone);
    }
    Ok(())
}

/// Whether `text` is shaped like a machine label or holds one: any of its words (split at
/// anything but a letter or digit) is `m` or `M` and a digit (`m2`, `M109`, `m2.3`, `m106-m38`,
/// `kimi-m16`, `m4max`). A word such as `max` or `muse` stays.
pub fn machine_shaped(text: &str) -> bool {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|word| {
            let mut bytes = word.bytes();
            matches!(bytes.next(), Some(b'm' | b'M'))
                && bytes.next().is_some_and(|b| b.is_ascii_digit())
        })
}

/// Check an account's owner-editable name of at most `max` bytes: free text ([`text`]) with
/// something in it and no machine in it, since a place on a Mac never names an account
/// (design §0). The old name a Mac already calls a row by stays in `legacy_label`.
///
/// # Errors
/// The rule it breaks; the value is never quoted.
pub fn account_label(value: &str, max: usize) -> Result<(), Refusal> {
    text(value, max)?;
    if value.trim().is_empty() {
        return Err(Refusal::Length);
    }
    if machine_shaped(value) {
        return Err(Refusal::MachineLabel);
    }
    Ok(())
}

/// Words that, standing alone, say a note may carry a sign-in secret. A bulk copy of a cell nobody
/// retyped refuses them where [`text`] would not, because `pass: x` or `pin 4821` is not a phrase
/// [`SECRET_WORDS`] can list without refusing `compass` or a journal's `token limit`.
const NOTE_WORDS: [&str; 19] = [
    "pass",
    "pw",
    "pwd",
    "psw",
    "pswd",
    "pword",
    "pin",
    "otp",
    "token",
    "tokens",
    "seed",
    "mnemonic",
    "secret",
    "secrets",
    "key",
    "keys",
    "credential",
    "credentials",
    "creds",
];

/// The fewest characters of a word that reads as a password for mixing letters and digits.
const MIXED_WORD: usize = 6;

/// Whether a word of a note reads as a password rather than prose: a part of it (split at the
/// marks that join ordinary words, `gpt-5.5`, `m106-m38`, `$200/mo`) of at least six characters
/// mixing letters and digits, or holding a mark inside it beside a letter.
fn password_shaped(word: &str) -> bool {
    let edge = |c: char| {
        matches!(
            c,
            '.' | ',' | ';' | ':' | '(' | ')' | '[' | ']' | '"' | '\'' | '!' | '?'
        )
    };
    word.trim_matches(edge)
        .split(['-', '.', '/', '_', ':', ',', '+'])
        .any(|part| {
            let letter = part.chars().any(char::is_alphabetic);
            let digit = part.chars().any(|c| c.is_ascii_digit());
            let mark = part.chars().any(|c| c.is_ascii_punctuation() && c != '\'');
            letter && ((digit && part.chars().count() >= MIXED_WORD) || mark)
        })
}

/// Check a note cell an import would copy lab-wide: [`text`], and no word that says it may carry a
/// secret ([`NOTE_WORDS`]) or reads as a password. This is stricter than a typed cell because
/// nobody retyped it and the table is never compacted, so a false refusal costs one cell's text
/// (its coordinate stays) and a miss is a password on every Mac. A password that is a plain word
/// with no such word beside it cannot be told from prose.
///
/// # Errors
/// The rule it breaks; the value is never quoted.
pub fn note_cell(value: &str, max: usize) -> Result<(), Refusal> {
    text(value, max)?;
    if value
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|word| NOTE_WORDS.iter().any(|nw| word.eq_ignore_ascii_case(nw)))
    {
        return Err(Refusal::SecretWord);
    }
    if value.split_whitespace().any(password_shaped) {
        return Err(Refusal::CredentialShape);
    }
    Ok(())
}

/// Check a string a typed column holds other than free text (an enum word, a label, a URL):
/// it may not carry a credential shape either.
///
/// # Errors
/// The rule it breaks; the value is never quoted.
pub fn word(value: &str) -> Result<(), Refusal> {
    if value.chars().any(char::is_control) {
        return Err(Refusal::Control);
    }
    if credential_shaped(value) {
        return Err(Refusal::CredentialShape);
    }
    Ok(())
}
