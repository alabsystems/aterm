// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What non-Secret ATT/1 cells may not hold (design §3.4): a credential-shaped string, free text that names
//! a secret (so a plain-word password cannot ride a note), a full phone number (it belongs in a
//! phone row's `number` column), or an address outside a typed `email` cell (so an address is
//! held only where its column says one is). A refusal names the rule and never quotes the
//! value. Secret columns bypass these text guards only with an opaque inner seal;
//! plaintext is rejected by their type check (aterm-db's `typed::cell::validate`).
//!
//! aterm-db's typed cells apply these rules by column type; aterm-file's outbox refuses a value
//! shaped like a credential ([`credential_shaped`]) or holding a phone number ([`phone_shaped`])
//! in every value an intent carries, whatever its column, so no writer of a shared file queues
//! one (docs/DESIGN-shared-files-2026-11-11.md §5, rule 6).

#![forbid(unsafe_code)]

/// What a surface shows in place of a string another writer's build stored though this one
/// refuses to show it: in a table's views one shaped like a credential or naming a secret
/// ([`withheld`]), in a shared file's outbox one shaped like a credential or holding a phone
/// number.
pub const REFUSED: &str = "(refused value: not shown)";

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
    /// Nothing where the column needs something.
    Empty,
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
            Self::Phone => "a phone number belongs in a phone row's number column",
            Self::MachineLabel => {
                "an account's label names no machine: a preference row maps it to one"
            }
            Self::Empty => "this column needs something in it",
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
/// provider key's prefix with its run, a base32 TOTP seed, a URL carrying a password, or a long
/// unbroken run of token characters.
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
    if base32_seed(text) || url_password(text) {
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

/// Whether `text` holds a URL that carries a password: `://`, then `user:password@` before the
/// authority ends (at `/`, `?`, `#` or white space). `ssh://git@host` names only a user and
/// stays, as does an address in prose.
fn url_password(text: &str) -> bool {
    text.match_indices("://").any(|(at, separator)| {
        text[at + separator.len()..]
            .split(|c: char| matches!(c, '/' | '?' | '#') || c.is_whitespace())
            .next()
            .and_then(|authority| authority.rsplit_once('@'))
            .and_then(|(userinfo, _)| userinfo.split_once(':'))
            .is_some_and(|(_, password)| !password.is_empty())
    })
}

/// The fewest digits a phone number has.
const PHONE_DIGITS: usize = 7;
/// The most digits a phone number has (E.164).
const PHONE_MOST: usize = 15;
/// The fewest digits a stored number has: a phone row keeps its last four.
const PHONE_STORED: usize = 4;

/// Whether `text` is a phone number as a person gives one to a table: 7 to 15 digits with an
/// optional leading `+` and nothing else. What the table then stores is its own form of those
/// digits (aterm-db's registry keeps digits only).
#[must_use]
pub fn is_phone(text: &str) -> bool {
    let digits = text.strip_prefix('+').unwrap_or(text);
    (PHONE_DIGITS..=PHONE_MOST).contains(&digits.len())
        && digits.bytes().all(|b| b.is_ascii_digit())
}

/// Whether `text` reads as a phone number some build stored: an importer's digits (four or
/// more) or what a person wrote before [`is_phone`] was asked (digits that spaces, `-`, `.`,
/// `(` and `)` separate, after an optional leading `+`). It is 4 to 15 digits once the
/// separators go, and nothing else: no letter, so no word and no token passes.
#[must_use]
pub fn stored_phone(text: &str) -> bool {
    let written = text.strip_prefix('+').unwrap_or(text);
    written
        .bytes()
        .all(|b| b.is_ascii_digit() || matches!(b, b' ' | b'-' | b'.' | b'(' | b')'))
        && (PHONE_STORED..=PHONE_MOST).contains(&written.bytes().filter(u8::is_ascii_digit).count())
}

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

/// Provider tokens by their prefix and the fewest characters of a real one's body (letters,
/// digits, `_` and `-`, [`body_byte`]): what [`secret_shaped`] refuses at the start of a word. A
/// prefix with less after it is a name in prose (`sk-ssh-ed25519-cert-v01`, `the ghp_ prefix`,
/// `xoxb-/xoxp-`, `npm_config_cache=`), never a token.
const TOKENS: [(&str, usize); 24] = [
    ("github_pat_", 60),
    ("ghp_", 30),
    ("gho_", 30),
    ("ghu_", 30),
    ("ghs_", 30),
    ("ghr_", 30),
    ("glpat-", 20),
    ("sk-ant-", 40),
    ("sk-", 40),
    ("xoxa-", 30),
    ("xoxb-", 30),
    ("xoxp-", 30),
    ("xoxr-", 30),
    ("xoxs-", 30),
    ("xapp-", 30),
    ("https://hooks.slack.com/services/", 20),
    ("AGE-SECRET-KEY-1", 50),
    ("AIza", 30),
    ("xai-", 40),
    ("ya29.", 30),
    ("npm_", 30),
    ("hf_", 30),
    ("AKIA", 16),
    ("ASIA", 16),
];

/// Whether a byte belongs to a provider token's body: a letter, a digit, `_` or `-` (a Slack
/// webhook's `/` too, which the body of [`TOKENS`]' webhook row holds between its parts).
const fn body_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')
}

/// The fewest characters of a run [`random_token`] judges.
const RANDOM_RUN: usize = 32;
/// The least share, in percent, of a random token's characters that are upper case, and again
/// lower case.
const RANDOM_CASE_PCT: usize = 15;
/// The least share, in percent, of a random token's characters that start a run of their class
/// (upper case, lower case, digit, `+` or `/`).
const RANDOM_CHANGE_PCT: usize = 40;

/// Whether `text` carries a real secret, the rule of the lab tracker's prose, command and path
/// types (docs/DESIGN-lab-tracker-2026-11-14.md §7, finding 13): a provider token, its prefix
/// with a token's body after it ([`TOKENS`]); a PEM private key; an `otpauth://` URI with its
/// seed; a TOTP seed on its own ([`seed_shaped`]); a URL carrying a password; or a run of
/// random letters and digits ([`random_token`]). Unlike [`credential_shaped`], it refuses
/// neither a 40-character run (a commit, a path, a test's name) nor a bare prefix in prose, and
/// [`prose`] lets an `@` stand (`s-ed8f5c@m60`, `lean-init@v4.30.0`): the 821 of 1,121
/// packages of publication 384b3a9f that [`text`] refuses hold no secret.
#[must_use]
pub fn secret_shaped(text: &str) -> bool {
    let bytes = text.as_bytes();
    for (at, _) in text.char_indices() {
        if at > 0 && bytes[at - 1].is_ascii_alphanumeric() {
            continue;
        }
        let rest = &text[at..];
        if TOKENS.iter().any(|(prefix, body)| {
            rest.strip_prefix(prefix).is_some_and(|after| {
                let webhook = prefix.ends_with('/');
                after
                    .bytes()
                    .take_while(|b| body_byte(*b) || (webhook && *b == b'/'))
                    .count()
                    >= *body
            })
        }) || pem_private_key(rest)
            || otpauth_seed(rest)
        {
            return true;
        }
    }
    seed_shaped(text) || url_password(text) || random_token(text)
}

/// Whether `rest` starts a PEM private key's header: five dashes and `BEGIN `, then `PRIVATE KEY`
/// before the header's closing dashes (an OpenSSH, EC, RSA or PKCS#8 key). A certificate or a
/// public key is no secret.
fn pem_private_key(rest: &str) -> bool {
    rest.strip_prefix("-----BEGIN ")
        .and_then(|header| header.split("-----").next())
        .is_some_and(|label| label.contains("PRIVATE KEY"))
}

/// Whether `rest` starts an `otpauth://` URI whose `secret=` holds a seed: it starts with at
/// least [`SEED_CHARS`] base32 characters, whatever follows them (`&`, a quote, a `)`).
fn otpauth_seed(rest: &str) -> bool {
    let Some(uri) = rest.strip_prefix("otpauth://") else {
        return false;
    };
    let uri = uri.split(char::is_whitespace).next().unwrap_or_default();
    uri.match_indices("secret=").any(|(at, key)| {
        uri[at + key.len()..]
            .bytes()
            .take_while(|b| b.is_ascii_alphabetic() || (b'2'..=b'7').contains(b))
            .count()
            >= SEED_CHARS
    })
}

/// Whether `text` holds a TOTP seed as an authenticator shows one: a word of at least
/// [`SEED_CHARS`] upper-case base32 characters (`JBSWY3DPEHPK3PXP`), or at least four
/// space-separated groups of four in one case (`jbsw y3dp ehpk 3pxp`), with a digit `2`–`7` and
/// a letter past `f`; lower-case groups hold such a digit in two of the four, since four short
/// words with one digit among them are prose (`test ipv6 with real`). A lower-case run is not
/// one: in the tracker's prose it is words written without spaces (`4alternatingpairs`,
/// `baseline25requests`).
fn seed_shaped(text: &str) -> bool {
    if text.len() < SEED_CHARS {
        return false;
    }
    let seeded = |word: &str| {
        word.bytes().any(|b| (b'2'..=b'7').contains(&b))
            && word.bytes().any(|b| b.to_ascii_lowercase() > b'f')
    };
    if text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|word| {
            word.len() >= SEED_CHARS
                && base32(word)
                && !word.bytes().any(|b| b.is_ascii_lowercase())
                && seeded(word)
        })
    {
        return true;
    }
    const GROUP_RUN: usize = SEED_CHARS / 4;
    let mut run = [""; GROUP_RUN];
    let mut filled = 0usize;
    for group in text.split(' ') {
        if group.len() == 4 && base32(group) {
            run.copy_within(1.., 0);
            run[GROUP_RUN - 1] = group;
            filled += 1;
            let upper = run
                .iter()
                .all(|g| g.bytes().all(|b| !b.is_ascii_lowercase()));
            let lower = run
                .iter()
                .all(|g| g.bytes().all(|b| !b.is_ascii_uppercase()));
            let with_digit = run
                .iter()
                .filter(|g| g.bytes().any(|b| (b'2'..=b'7').contains(&b)))
                .count();
            if filled >= GROUP_RUN
                && (upper || (lower && with_digit >= 2))
                && run.iter().any(|g| seeded(g))
            {
                return true;
            }
        } else {
            filled = 0;
        }
    }
    false
}

/// Whether `text` holds a run of at least [`RANDOM_RUN`] letters, digits, `+` and `/` drawn at
/// random (a base62 or base64 key of no known provider): upper and lower case each at least
/// [`RANDOM_CASE_PCT`] percent of it, a digit, and its class (upper case, lower case, digit,
/// `+` or `/`) changing at least [`RANDOM_CHANGE_PCT`] percent of the way along. A run is cut
/// at [`HEX_CUT`] or more lower-case hex digits, so a commit inside words written without
/// spaces (`LANDEDd7d2226…/normal7GREEN22s`) is no key. Such a key changes class at about three
/// characters in five; words written without spaces
/// (`Nativecoldquery160columns161allocationrequests`) keep one case for a word at a time, a
/// CamelCase name has no digit, and a digest or a commit has no upper case. A key in one case
/// is not judged: of the tracker's text, words glued to a commit (`source14c8f27715c7…`) read
/// as one. Of the cells of publication 384b3a9f none is one; of random 40-character base64 keys
/// about one in 300 is missed.
fn random_token(text: &str) -> bool {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '+' || c == '/'))
        .flat_map(hex_cut)
        .filter(|run| run.len() >= RANDOM_RUN)
        .any(|run| {
            let (mut upper, mut lower, mut digits, mut changes) = (0, 0, 0, 0);
            let mut last = None;
            for b in run.bytes() {
                let class = if b.is_ascii_uppercase() {
                    upper += 1;
                    0
                } else if b.is_ascii_lowercase() {
                    lower += 1;
                    1
                } else if b.is_ascii_digit() {
                    digits += 1;
                    2
                } else {
                    3
                };
                if last != Some(class) {
                    changes += 1;
                }
                last = Some(class);
            }
            let n = run.len();
            upper * 100 >= RANDOM_CASE_PCT * n
                && lower * 100 >= RANDOM_CASE_PCT * n
                && digits > 0
                && changes * 100 >= RANDOM_CHANGE_PCT * n
        })
}

/// The fewest lower-case hex digits in a row that [`random_token`] cuts a run at.
const HEX_CUT: usize = 7;

/// `run`'s parts between its stretches of at least [`HEX_CUT`] lower-case hex digits.
fn hex_cut(run: &str) -> Vec<&str> {
    let bytes = run.as_bytes();
    let hex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
    let mut parts = Vec::new();
    let (mut start, mut at) = (0, 0);
    while at < bytes.len() {
        if hex(bytes[at]) {
            let end = at + bytes[at..].iter().take_while(|b| hex(**b)).count();
            if end - at >= HEX_CUT {
                parts.push(&run[start..at]);
                start = end;
            }
            at = end;
        } else {
            at += 1;
        }
    }
    parts.push(&run[start..]);
    parts
}

/// Check one value of the lab tracker's prose, of at most `max` bytes: no control character
/// and no real secret ([`secret_shaped`]). An `@`, a commit, a path and a phone-like run of
/// digits (`ATERM-KI-1114-533`) are the tracker's ordinary text, and a word that names a
/// secret is a package about one.
///
/// # Errors
/// The rule it breaks; the value is never quoted.
pub fn prose(value: &str, max: usize) -> Result<(), Refusal> {
    if value.len() > max {
        return Err(Refusal::Length);
    }
    if value.chars().any(char::is_control) {
        return Err(Refusal::Control);
    }
    if secret_shaped(value) {
        return Err(Refusal::CredentialShape);
    }
    Ok(())
}

/// Check one shell command line of the lab tracker (a package's exit test, a measurement's
/// source), of at most `max` bytes: [`prose`]'s rules, and something to run.
///
/// # Errors
/// The rule it breaks; the value is never quoted.
pub fn command(value: &str, max: usize) -> Result<(), Refusal> {
    if value.trim().is_empty() {
        return Err(Refusal::Empty);
    }
    prose(value, max)
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

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
