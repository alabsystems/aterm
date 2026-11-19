// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LOGIN FILES OF GROK BUILD, KIMI CODE AND AGY, AS FACTS (plan package SF-34b): what each
//! client's own login file says of the credential it holds, without its value, for
//! [`super::read`]. Each reader opens the file as a regular file (no final link) and reads it
//! whole, at most [`MAX`] bytes, into one buffer that never grows and is scrubbed however the
//! read ends ([`whole`]): a file whose length changed while it was read is no file it reads. It
//! parses the bytes only once a first pass that decodes no string found them one whole JSON
//! document, so a cut file, or one with bytes after its document, makes no string at all; it
//! takes only whether the file holds a credential, whether it holds a refresh token and the end
//! its access token states, and wipes every string of the parsed document before it returns
//! ([`wipe_json`]). What `aterm_json`'s parse itself frees on the way is not wiped, as in every
//! JSON login reader here: the smaller buffers a string written with an escape (a backslash)
//! grows through as it is decoded, and a value a repeated key replaces. It returns closed words
//! and Unix seconds ([`Stored`]) or nothing, and no error text: no byte of the file can leave it.
//!
//! Each reads only the fields the code that writes or reads its file names:
//! - **Grok Build's `auth.json`** ([`grok`]): an object of login records, one per sign-in scope,
//!   a record's token in `key` beside its `user_id` and `email`, as `observe::grok_named` reads it
//!   (grok 1.0.77's strings). No signed-in file was at hand to measure, so what else a record
//!   holds is unknown: this reader reads no end and no refresh token from it, and a file that
//!   states no `key` at all is a login whose token it cannot read, held as the harness counts it
//!   (`grok_signin::login_accepted`: a non-empty file grok has not refused).
//! - **Kimi Code's credential file** ([`kimi`]): `access_token`, `refresh_token`, `expires_at`
//!   (Unix seconds), `expires_in`, `scope` and `token_type` (`docs/measured/kimi-profile-2026-10-30.md`;
//!   aterm's own refresh writes the same, `cli_shared_kimi_refresh.rs`); a refresh the issuer
//!   refused leaves both tokens empty.
//! - **agy's grant** ([`agy`]): `auth_method` and `token`, Go's `oauth2.Token` (`access_token`,
//!   `token_type`, `refresh_token`, `expiry` as RFC 3339), as `agy_signin.rs` reads it. aterm never
//!   empties a row's grant (a refused grant with no refresh token is removed, and only from an
//!   owned staging copy, `agy_signin::prepare_rejected_staging`), so an empty one is none aterm
//!   wrote, and it holds no credential.

use crate::harness::accounts;
use crate::harness::cli::shared_provider::wipe_json;
use crate::harness::observe::{self, Identity, Named};
use aterm_json::Value;
use aterm_uds::secret::Secret;
use std::io::Read;
use std::path::Path;

/// The most bytes of a login file these readers read: more is no login file they read.
pub(super) const MAX: u64 = 1 << 20;

/// What a login file says of the credential it holds, value-free: no field holds a token, a
/// digest of one, a claim's text, an address or a path, so neither can a print of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Stored {
    /// It holds a credential: a non-empty token of a field its client signs in with, or, in a
    /// Grok Build file that states no `key`, a token its reader cannot read ([`grok`]).
    pub held: bool,
    /// It holds a non-empty refresh token, by which its CLI renews its access token.
    pub refreshable: bool,
    /// The end its access token states, in Unix seconds after the epoch: `None` where the file
    /// states none (Go's zero time, and a refused Kimi grant's `0`, are none).
    pub expires_at: Option<i64>,
}

/// A file that holds nothing: an empty one.
const NOTHING: Stored = Stored {
    held: false,
    refreshable: false,
    expires_at: None,
};

/// What the login file `file` holds.
enum Doc {
    /// No byte.
    Empty,
    /// A JSON object.
    Object(Value),
}

/// The login file `file`, opened as a regular file (no final link,
/// [`aterm_uds::regular_file::open`]) and read whole, as long as its length says and at most
/// [`MAX`] bytes, into one scrubbed buffer ([`whole`]), then parsed once a first pass that decodes
/// no string ([`serde::de::IgnoredAny`]) found it one whole JSON document: `None` where it is
/// missing, no regular file, unreadable, larger, changed length while it was read, or no JSON
/// object.
fn read(file: &Path) -> Option<Doc> {
    let (opened, meta) = aterm_uds::regular_file::open(file).ok()?;
    if meta.len() > MAX {
        return None;
    }
    let bytes = whole(opened, usize::try_from(meta.len()).ok()?)?;
    if bytes.is_empty() {
        return Some(Doc::Empty);
    }
    aterm_json::from_slice::<serde::de::IgnoredAny>(bytes.expose()).ok()?;
    let doc: Value = aterm_json::from_slice(bytes.expose()).ok()?;
    if doc.is_object() {
        Some(Doc::Object(doc))
    } else {
        wipe_json(doc);
        None
    }
}

/// The `len` bytes `reader` holds, read into one buffer of `len + 1` bytes that never grows and
/// is scrubbed however the read ends ([`Secret::from_vec`]): `None` where it holds more (a file
/// that grew after its length was read) or fewer, or a read fails.
pub(super) fn whole(mut reader: impl Read, len: usize) -> Option<Secret> {
    let mut buf = vec![0_u8; len.checked_add(1)?];
    let mut filled = 0_usize;
    let ended = loop {
        let Some(rest) = buf.get_mut(filled..).filter(|rest| !rest.is_empty()) else {
            break false;
        };
        match reader.read(rest) {
            Ok(0) => break true,
            Ok(read) => filled = filled.saturating_add(read),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break false,
        }
    };
    buf.truncate(filled);
    let bytes = Secret::from_vec(buf);
    (ended && bytes.len() == len).then_some(bytes)
}

/// Whether `value` is a non-empty string.
fn text(value: &Value) -> bool {
    value.as_str().is_some_and(|text| !text.is_empty())
}

/// `at` where it is after the epoch.
fn after_epoch(at: Option<i64>) -> Option<i64> {
    at.filter(|at| *at > 0)
}

/// Whether the account a login file's own fields name (`identity`) is another than the one the
/// row expects (`expected`): an address it names that does not answer to the row's. A file that
/// names no address, and a row that expects none, say nothing against it.
fn names_another(identity: &Identity, expected: Option<&str>) -> bool {
    identity
        .email
        .as_deref()
        .zip(expected)
        .is_some_and(|(email, expected)| !accounts::email_matches(expected, email))
}

/// What Grok Build's `auth.json` at `file` says of its credential, where it is the login of the
/// account `expected` names (any account where it is `None`): it holds none where a record (the
/// file itself, or a value of it) states a `key` and no record's `key` is non-empty. A file that
/// states no `key` at all holds a token this reader cannot read: it is held, as the harness
/// counts a non-empty file grok has not refused (`grok_signin::login_accepted`), so the row's
/// state follows its word on record rather than a field nobody measured. It reads no refresh
/// token and no end. `None` for a file [`read`] does not read, and for one whose records name
/// another account ([`names_another`]) or two users (`observe::grok_named`'s unsaid), since
/// nothing in it then says the row's login is the one grok runs on.
pub(super) fn grok(file: &Path, expected: Option<&str>) -> Option<Stored> {
    let doc = match read(file)? {
        Doc::Empty => return Some(NOTHING),
        Doc::Object(doc) => doc,
    };
    let named = observe::grok_named(&doc);
    let records = || {
        std::iter::once(&doc).chain(
            doc.as_object()
                .into_iter()
                .flat_map(|object| object.values()),
        )
    };
    let held = records().any(|record| text(&record["key"]))
        || !records().any(|record| record["key"].as_str().is_some());
    wipe_json(doc);
    match named {
        Named::Unsaid => None,
        Named::One(identity) if names_another(&identity, expected) => None,
        Named::One(_) | Named::Nobody => Some(Stored {
            held,
            refreshable: false,
            expires_at: None,
        }),
    }
}

/// What Kimi Code's credential file at `file` says of its credential, where it is the login of
/// the account `expected` names (any account where it is `None`): it holds one where its
/// `access_token` or `refresh_token` is non-empty, is renewed by a non-empty `refresh_token`, and
/// its access token ends at `expires_at`. `None` for a file [`read`] does not read, and for one
/// whose access token's claims name another account (`observe::kimi_named`, [`names_another`]);
/// a token that is no set of claims names none.
pub(super) fn kimi(file: &Path, expected: Option<&str>) -> Option<Stored> {
    let doc = match read(file)? {
        Doc::Empty => return Some(NOTHING),
        Doc::Object(doc) => doc,
    };
    let named = observe::kimi_named(&doc);
    let token = |name: &str| text(&doc[name]);
    let stored = Stored {
        held: token("access_token") || token("refresh_token"),
        refreshable: token("refresh_token"),
        expires_at: after_epoch(doc["expires_at"].as_i64()),
    };
    wipe_json(doc);
    match named {
        Named::One(identity) if names_another(&identity, expected) => None,
        Named::One(_) | Named::Nobody | Named::Unsaid => Some(stored),
    }
}

/// What agy's grant at `file` says of its credential: its `token` holds one where its
/// `access_token` or `refresh_token` is non-empty, is renewed by a non-empty `refresh_token`, and
/// its access token ends at its `expiry` ([`rfc3339`]), whatever its `auth_method`. `None` for a
/// file [`read`] does not read, and for one with no `token` object. The file names no account
/// this reader may take as proof (its cached `id_token_email` is none, `agy_signin.rs`), so the
/// row's own login home is what makes it the row's.
pub(super) fn agy(file: &Path) -> Option<Stored> {
    let doc = match read(file)? {
        Doc::Empty => return Some(NOTHING),
        Doc::Object(doc) => doc,
    };
    let token = &doc["token"];
    let stored = token.is_object().then(|| {
        let field = |name: &str| text(&token[name]);
        Stored {
            held: field("access_token") || field("refresh_token"),
            refreshable: field("refresh_token"),
            expires_at: after_epoch(token["expiry"].as_str().and_then(rfc3339)),
        }
    });
    wipe_json(doc);
    stored
}

/// An RFC 3339 time as Go's `time.Time` writes it to JSON (with nanoseconds where it has any, in
/// the zone it was made in: `Z`, or a `+HH:MM` or `-HH:MM` offset), in Unix seconds.
pub(super) fn rfc3339(text: &str) -> Option<i64> {
    if text.ends_with('Z') {
        return aterm_types::rfc3339::parse_utc_fractional(text);
    }
    let split = text.len().checked_sub(6)?;
    let (local, zone) = (text.get(..split)?, text.get(split..)?.as_bytes());
    let sign = match zone[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    if zone[3] != b':' {
        return None;
    }
    let digit = |at: usize| {
        zone[at]
            .is_ascii_digit()
            .then(|| i64::from(zone[at] - b'0'))
    };
    let hours = digit(1)? * 10 + digit(2)?;
    let minutes = digit(4)? * 10 + digit(5)?;
    if hours > 23 || minutes > 59 {
        return None;
    }
    let local = aterm_types::rfc3339::parse_utc_fractional(&format!("{local}Z"))?;
    local.checked_sub(sign * (hours * 3600 + minutes * 60))
}
