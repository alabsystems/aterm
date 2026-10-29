// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PER-EMAIL MAILBOX ROUTING (`docs/DESIGN-alab-swarm-2026-10-27.md` §6.2): which mailbox login
//! an AI account's sign-in code is read from.
//!
//! Most AI logins are addresses whose mail lands in one shared inbox (every Proton alias in
//! the owner's Proton mailbox), while a Gmail or Outlook address has its own mailbox and login.
//! `ai-mailbox.toml` names each such mailbox in a `[[mailboxes]]` entry:
//!
//! ```toml
//! [[mailboxes]]
//! login = "owner@proton.example"            # alias: user
//! password = "…"                             # a mailbox with no password is never tried
//! mailbox_password = "…"                     # optional second password
//! totp_secret = "…"                          # optional RFC 6238 secret or otpauth:// URI
//! webmail_url = "https://mail.proton.me/u/0/inbox"   # or imap_url / http_url; none: from the login's domain
//! receives = ["*@proton.example", "ai1@pm.example"]
//! ```
//!
//! A `receives` pattern is an exact email, `*@domain` or `*`. For a target email the pattern
//! naming the address itself beats the one naming its base address (without its `+tag`), which
//! beats the domain one, which beats `*`, which beats the file's legacy top-level fields; among
//! equals the first entry wins. [`resolve`] is that order; every reader of
//! [`super::headless_oauth::MailboxConfig`] goes through it.
//!
//! An entry leaves out what the file already holds for its login
//! ([`super::headless_oauth::MailboxConfig::completed_route`]): one whose login is the file's
//! top-level `user` is the top-level mailbox (its password, second password, TOTP secret and,
//! where it names no URL, its URLs), and any other takes the per-email credentials kept for its
//! login, so routing an address never cuts it off from a password that works.
//!
//! SAFETY of the owner's mailboxes: an entry with no password (or no login) is never tried and
//! is reported as `mailbox ab…@… has no password yet`, never as a failed sign-in; a mailbox
//! that refused its login on one source (an IMAP server, a webmail host) is not tried there
//! again by any row or process sharing the record ([`LOGIN_RECORD_FILE`]) until a wait of 15
//! minutes, doubling to 6 hours, has passed ([`login_held`]); a login is counted as signed in
//! only on a positive sign (the inbox's message list, or a code read), never on a page that
//! merely shows no sign-in form yet; and every mailbox login this module prints is masked
//! ([`mask_login`]).
//!
//! The importer ([`plan_import`], `aterm harness accounts mailboxes import`) merges the owner's
//! accounts CSV into `receives` lists without touching a single byte of any secret.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// File under the accounts store recording failed mailbox sign-ins by a digest of the source
/// and the login (never the login or any secret): `v1 <digest> <failures> <last failure, unix
/// seconds>`.
pub const LOGIN_RECORD_FILE: &str = "mailbox-logins.txt";

/// The largest login record read; a larger one reads as empty.
const LOGIN_RECORD_MAX: u64 = 64 * 1024;

/// The largest accounts CSV the importer reads.
pub const IMPORT_CSV_MAX: u64 = 1024 * 1024;

/// The largest `ai-mailbox.toml` the importer reads.
pub const IMPORT_TOML_MAX: u64 = 4 * 1024 * 1024;

/// One `[[mailboxes]]` entry of `ai-mailbox.toml`.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct MailboxRoute {
    /// The mailbox's own login (`login`, alias `user`), as written.
    pub login: String,
    /// Webmail inbox URL polled through the headless browser.
    pub webmail_url: Option<String>,
    /// IMAP or IMAPS URL.
    pub imap_url: Option<String>,
    /// HTTP(S) mailbox API URL.
    pub http_url: Option<String>,
    /// The login's password; missing or empty, the mailbox is never tried.
    pub password: Option<String>,
    /// A second-stage mailbox password (two-password webmail accounts).
    pub mailbox_password: Option<String>,
    /// A Base32 TOTP secret or `otpauth://` URI for the login's second factor.
    pub totp_secret: Option<String>,
    /// The normalized patterns of the addresses whose mail this mailbox receives
    /// ([`normalize_pattern`]).
    pub receives: Vec<String>,
}

impl std::fmt::Debug for MailboxRoute {
    /// The masked login and which fields are set; never a secret or an address.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MailboxRoute")
            .field("login", &mask_login(&self.login))
            .field("webmail_url", &self.webmail_url.is_some())
            .field("imap_url", &self.imap_url.is_some())
            .field("http_url", &self.http_url.is_some())
            .field("password", &self.ready())
            .field("mailbox_password", &self.mailbox_password.is_some())
            .field("totp_secret", &self.totp_secret.is_some())
            .field("receives", &self.receives.len())
            .finish()
    }
}

impl MailboxRoute {
    /// Whether this mailbox may be tried at all: it has a login and a non-empty password.
    #[must_use]
    pub fn ready(&self) -> bool {
        !self.login.trim().is_empty() && self.password.as_deref().is_some_and(|p| !p.is_empty())
    }

    /// Its login, masked ([`mask_login`]).
    #[must_use]
    pub fn masked(&self) -> String {
        mask_login(&self.login)
    }

    /// Why it is never tried, where it is not [`MailboxRoute::ready`].
    #[must_use]
    pub fn unready_words(&self) -> Option<String> {
        if self.login.trim().is_empty() {
            Some("a [[mailboxes]] entry has no login yet".to_owned())
        } else if !self.ready() {
            Some(format!("mailbox {} has no password yet", self.masked()))
        } else {
            None
        }
    }

    /// The domain of its login, lowercased.
    #[must_use]
    pub fn login_domain(&self) -> Option<String> {
        let (_, domain) = self.login.trim().rsplit_once('@')?;
        (!domain.is_empty()).then(|| domain.to_ascii_lowercase())
    }

    /// Whether it names any URL itself (then none is discovered from its login's domain).
    #[must_use]
    pub fn names_a_url(&self) -> bool {
        self.webmail_url.is_some() || self.imap_url.is_some() || self.http_url.is_some()
    }

    /// Whether it is one address's own mailbox: it receives one exact address, and that address
    /// without a `+tag` is its login without one. Any other entry (several addresses, a
    /// wildcard, or one address read through another login's mailbox) is shared.
    #[must_use]
    pub fn own(&self) -> bool {
        match self.receives.as_slice() {
            [only] if !only.contains('*') => {
                let login = self.login.trim().to_ascii_lowercase();
                !login.is_empty() && base_or_self(only) == base_or_self(&login)
            }
            _ => false,
        }
    }

    /// Whether it receives anything and is not one address's own mailbox ([`MailboxRoute::own`]):
    /// a catch-all, whose login stays with the stewards.
    #[must_use]
    pub fn shared(&self) -> bool {
        !self.receives.is_empty() && !self.own()
    }

    /// Whether it is `email`'s own mailbox ([`MailboxRoute::own`]) and names `email` itself: its
    /// login is then that account's own credential (sealed to the account's warm Macs), not a
    /// catch-all that stays with the stewards. An entry naming a base address is never private
    /// to the base's `+tag` addresses, which it also serves.
    #[must_use]
    pub fn private_to(&self, email: &str) -> bool {
        let email = email.trim().to_ascii_lowercase();
        self.own() && self.receives.first() == Some(&email)
    }
}

/// How closely a pattern matched a target ([`resolve`]): the lower the closer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// The target's address itself.
    Exact,
    /// The target's base address, without its `+tag`.
    Base,
    /// `*@domain` of the target's domain.
    Domain,
    /// `*`.
    Any,
}

/// `pattern` normalized: trimmed, lowercased, `@domain` read as `*@domain`; `None` for one
/// that is neither an email, `*@domain` nor `*` (it is ignored).
#[must_use]
pub fn normalize_pattern(pattern: &str) -> Option<String> {
    let lower = pattern.trim().to_ascii_lowercase();
    if lower == "*" {
        return Some(lower);
    }
    if lower.chars().any(char::is_whitespace) || lower.matches('@').count() != 1 {
        return None;
    }
    let (local, domain) = lower.split_once('@')?;
    if domain.is_empty() || domain.contains('*') {
        return None;
    }
    match local {
        "" | "*" => Some(format!("*@{domain}")),
        _ if local.contains('*') => None,
        _ => Some(lower),
    }
}

/// `email` without its `+tag` subaddress, where it has one.
fn base_address(email: &str) -> Option<String> {
    let (local, domain) = email.split_once('@')?;
    let base = local.split('+').next().unwrap_or(local);
    (base != local && !base.is_empty()).then(|| format!("{base}@{domain}"))
}

/// `address` without its `+tag` subaddress, or itself where it has none.
fn base_or_self(address: &str) -> String {
    base_address(address).unwrap_or_else(|| address.to_owned())
}

/// How a normalized `pattern` matches the lowercased `target`, if it does.
#[must_use]
pub fn match_tier(pattern: &str, target: Option<&str>) -> Option<Tier> {
    if pattern == "*" {
        return Some(Tier::Any);
    }
    let target = target?;
    if let Some(domain) = pattern.strip_prefix("*@") {
        let (_, target_domain) = target.rsplit_once('@')?;
        return (target_domain == domain).then_some(Tier::Domain);
    }
    if pattern == target {
        return Some(Tier::Exact);
    }
    (base_address(target).as_deref() == Some(pattern)).then_some(Tier::Base)
}

/// The mailbox `target_email`'s codes arrive in: the closest pattern of `routes` (the address,
/// then its base address, then `*@domain`, then `*`; the first entry among equals), or `None`
/// where no pattern matches and the legacy top-level mailbox applies. With no target only a `*`
/// entry matches.
#[must_use]
pub fn resolve<'a>(
    routes: &'a [MailboxRoute],
    target_email: Option<&str>,
) -> Option<&'a MailboxRoute> {
    resolve_index(routes, target_email).map(|at| &routes[at])
}

/// [`resolve`]'s entry, as its index in `routes`.
#[must_use]
pub fn resolve_index(routes: &[MailboxRoute], target_email: Option<&str>) -> Option<usize> {
    let target = target_email
        .map(|t| t.trim().to_ascii_lowercase())
        .filter(|t| !t.is_empty());
    let mut best: Option<(Tier, usize)> = None;
    for (at, route) in routes.iter().enumerate() {
        for pattern in &route.receives {
            if let Some(tier) = match_tier(pattern, target.as_deref())
                && best.is_none_or(|(held, _)| tier < held)
            {
                best = Some((tier, at));
            }
        }
    }
    best.map(|(_, at)| at)
}

/// A mailbox login (or any address) as aterm prints it: at most its first two characters, then
/// `…@…`; the domain is never shown. An empty login is `(no login)`.
#[must_use]
pub fn mask_login(login: &str) -> String {
    let login = login.trim();
    if login.is_empty() {
        return "(no login)".to_owned();
    }
    let local = login.split('@').next().unwrap_or(login);
    let shown: String = local.chars().take(2).collect();
    if login.contains('@') {
        format!("{shown}…@…")
    } else {
        format!("{shown}…")
    }
}

/// Parse every `[[mailboxes]]` entry of `table` (and, when it is another table, of `doc`'s
/// root), with `get_str` reading a trimmed non-empty string under any of its keys, `secret_file`
/// reading a `*_file` key's file, and `is_webmail` telling a webmail URL from an API one.
pub(super) fn parse_mailboxes(
    doc: &aterm_toml::Table,
    table: &aterm_toml::Table,
    get_str: &dyn Fn(&aterm_toml::Table, &[&str]) -> Option<String>,
    secret_file: &dyn Fn(&str) -> Option<String>,
    is_webmail: &dyn Fn(&str) -> bool,
) -> Vec<MailboxRoute> {
    let mut routes = Vec::new();
    let mut sources = vec![table];
    if !std::ptr::eq(doc, table) {
        sources.push(doc);
    }
    for source in sources {
        let Some(entries) = source
            .get("mailboxes")
            .and_then(aterm_toml::Value::as_array)
        else {
            continue;
        };
        for entry in entries.iter().filter_map(aterm_toml::Value::as_table) {
            routes.push(parse_entry(entry, get_str, secret_file, is_webmail));
        }
    }
    routes
}

fn parse_entry(
    entry: &aterm_toml::Table,
    get_str: &dyn Fn(&aterm_toml::Table, &[&str]) -> Option<String>,
    secret_file: &dyn Fn(&str) -> Option<String>,
    is_webmail: &dyn Fn(&str) -> bool,
) -> MailboxRoute {
    let get = |keys: &[&str]| get_str(entry, keys);
    let secret = |keys: &[&str], file_keys: &[&str]| {
        get(keys).or_else(|| get(file_keys).and_then(|p| secret_file(&p)))
    };
    let mut route = MailboxRoute {
        login: get(&["login", "user", "username"]).unwrap_or_default(),
        webmail_url: get(&["webmail_url", "webmail"]),
        imap_url: get(&["imap_url", "imap"]),
        http_url: get(&["http_url"]),
        password: secret(
            &[
                "password",
                "pass",
                "imap_password",
                "webmail_password",
                "proton_password",
            ],
            &["password_file", "pass_file", "webmail_password_file"],
        ),
        mailbox_password: secret(
            &["mailbox_password", "second_password"],
            &["mailbox_password_file", "second_password_file"],
        ),
        totp_secret: secret(
            &["totp_secret", "totp", "otpauth", "totp_uri", "otp_secret"],
            &[
                "totp_secret_file",
                "totp_file",
                "otpauth_file",
                "totp_uri_file",
            ],
        ),
        receives: Vec::new(),
    };
    if let Some(url) = get(&["url"]) {
        if url.starts_with("imap://") || url.starts_with("imaps://") {
            route.imap_url.get_or_insert(url);
        } else if is_webmail(&url) {
            route.webmail_url.get_or_insert(url);
        } else if url.starts_with("http://") || url.starts_with("https://") {
            route.http_url.get_or_insert(url);
        }
    }
    let patterns: Vec<&str> = match entry.get("receives") {
        Some(aterm_toml::Value::String(one)) => vec![one.as_str()],
        Some(aterm_toml::Value::Array(many)) => {
            many.iter().filter_map(aterm_toml::Value::as_str).collect()
        }
        _ => Vec::new(),
    };
    for pattern in patterns.into_iter().filter_map(normalize_pattern) {
        if !route.receives.contains(&pattern) {
            route.receives.push(pattern);
        }
    }
    route
}

/// `words` as a `&'static str`, for the mailbox code's `&'static str` errors: each distinct
/// text is kept once. The texts are built from masked logins ([`mask_login`]: two characters
/// and a fixed tail), so how many there can be is bounded.
pub(super) fn static_words(words: String) -> &'static str {
    use std::sync::{Mutex, PoisonError};
    static KEPT: Mutex<BTreeSet<&'static str>> = Mutex::new(BTreeSet::new());
    let mut kept = KEPT.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(known) = kept.get(words.as_str()) {
        return known;
    }
    let leaked: &'static str = Box::leak(words.into_boxed_str());
    kept.insert(leaked);
    leaked
}

/// The words of a mailbox whose login was refused and waits ([`login_held`]).
#[must_use]
pub fn refused_words(login: &str) -> &'static str {
    static_words(format!(
        "mailbox {} refused its last sign-in, so it is not tried again until its wait ends \
         (15 minutes, doubling to 6 hours)",
        mask_login(login)
    ))
}

/// The source a mailbox login is tried on, for its refusal record: the URL's scheme and host
/// (with its port), lowercased, without any `user[:password]@` or path:
/// `imaps://imap.example.test:993`, `https://mail.example.test`. A refusal on one source (a
/// local Bridge that takes only its own password) never holds the login on another (the
/// webmail that takes the account's).
#[must_use]
pub fn login_source(url: &str) -> String {
    let lower = url.trim().to_ascii_lowercase();
    let Some((scheme, rest)) = lower.split_once("://") else {
        return lower;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    format!("{scheme}://{host}")
}

/// The marker a login refused on `source` ([`login_source`]) leaves in one authorization's
/// in-memory seen set, so that authorization does not try it there again; it is never written
/// to a seen file.
#[must_use]
pub fn refused_marker(source: &str, login: &str) -> String {
    format!("refused-login:{}", login_digest(source, login))
}

/// FNV-1a over the source and the lowercased login: the record's key, so the record names no
/// address.
fn login_digest(source: &str, login: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let keyed = format!(
        "{}\n{}",
        source.trim().to_ascii_lowercase(),
        login.trim().to_ascii_lowercase()
    );
    for byte in keyed.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// One login's failures in the record: how many in a row, and the last one's time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Failures {
    count: u32,
    last: i64,
}

fn read_record(record: &Path) -> BTreeMap<String, Failures> {
    let text = super::accounts::read_bounded(record, LOGIN_RECORD_MAX)
        .ok()
        .flatten()
        .unwrap_or_default();
    text.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            if words.next() != Some("v1") {
                return None;
            }
            let digest = words.next()?.to_owned();
            let count = words.next()?.parse().ok()?;
            let last = words.next()?.parse().ok()?;
            Some((digest, Failures { count, last }))
        })
        .collect()
}

fn write_record(record: &Path, entries: &BTreeMap<String, Failures>) {
    let text: String = entries
        .iter()
        .map(|(digest, f)| format!("v1 {digest} {} {}\n", f.count, f.last))
        .collect();
    let _ = super::upgrade_models::write_atomic_mode(record, &text, Some(0o600));
}

/// Until when (unix seconds) `login` is not tried on `source` ([`login_source`]), where its
/// last sign-in there was refused less than its wait ago:
/// [`super::sign_in_ladder::backoff_after`] the failures in a row (15 minutes, doubling to 6
/// hours). `None` with no record or no failure.
#[must_use]
pub fn login_held(record: Option<&Path>, source: &str, login: &str, now: i64) -> Option<i64> {
    let failures = *read_record(record?).get(&login_digest(source, login))?;
    let wait = super::sign_in_ladder::backoff_after(failures.count).as_secs();
    let until = failures
        .last
        .saturating_add(i64::try_from(wait).unwrap_or(i64::MAX));
    (now < until).then_some(until)
}

/// Record that `login` was refused on `source` ([`login_source`]) at `now`.
pub fn note_login_refused(record: Option<&Path>, source: &str, login: &str, now: i64) {
    let Some(record) = record else {
        return;
    };
    let mut entries = read_record(record);
    let entry = entries
        .entry(login_digest(source, login))
        .or_insert(Failures {
            count: 0,
            last: now,
        });
    entry.count = entry.count.saturating_add(1);
    entry.last = now;
    write_record(record, &entries);
}

/// Record that `login` signed in on `source` ([`login_source`]): its failures there are
/// forgotten. Writes only where it had any.
pub fn note_login_accepted(record: Option<&Path>, source: &str, login: &str) {
    let Some(record) = record else {
        return;
    };
    let mut entries = read_record(record);
    if entries.remove(&login_digest(source, login)).is_some() {
        write_record(record, &entries);
    }
}

/// Unix seconds now (0 before the epoch).
#[must_use]
pub fn unix_now() -> i64 {
    aterm_time::unix_secs()
        .and_then(|t| i64::try_from(t).ok())
        .unwrap_or(0)
}

/// What the importer did, in counts and masked logins only ([`ImportPlan::summary`]). Its
/// `Debug` never shows `text`, which holds every secret of the file.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ImportPlan {
    /// The new `ai-mailbox.toml` text: the old one byte for byte except each changed
    /// `receives` value and the appended entries.
    pub text: String,
    /// Whether `text` differs from the file read.
    pub changed: bool,
    /// The `[[mailboxes]]` entries in `text`.
    pub mailboxes: usize,
    /// Masked logins of the entries in `text` with no password, of their own or kept for their
    /// login (each is never tried).
    pub need_password: Vec<String>,
    /// Masked logins of the entries the import appended that use a password the file already
    /// keeps for their login: the top-level one (the login is the file's `user`) or a
    /// per-email one ([`super::headless_oauth::MailboxConfig::completed_route`]).
    pub keep_password: Vec<String>,
    /// CSV rows that routed an AI email.
    pub routed: usize,
    /// The CSV rows read but skipped, each as its masked AI email (or `row N` where it has
    /// none): no AI email, or no mailbox login in the guess or correction.
    pub skipped: Vec<String>,
    /// AI emails taken off another entry's `receives`.
    pub moved: usize,
    /// Entries the import appended.
    pub created: usize,
}

impl std::fmt::Debug for ImportPlan {
    /// The counts and masked logins; never `text`, which holds the file's secrets.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportPlan")
            .field("text_bytes", &self.text.len())
            .field("changed", &self.changed)
            .field("mailboxes", &self.mailboxes)
            .field("need_password", &self.need_password)
            .field("keep_password", &self.keep_password)
            .field("routed", &self.routed)
            .field("skipped", &self.skipped)
            .field("moved", &self.moved)
            .field("created", &self.created)
            .finish()
    }
}

impl ImportPlan {
    /// Its first line: `3 mailboxes; 2 need a password: ab…@…, cd…@…`, naming first the new
    /// entries that keep a password the file already holds (`1 keeps the password already set
    /// for its login: ef…@…`).
    #[must_use]
    pub fn summary(&self) -> String {
        let noun = if self.mailboxes == 1 {
            "mailbox"
        } else {
            "mailboxes"
        };
        let mut parts = vec![format!("{} {noun}", self.mailboxes)];
        if !self.keep_password.is_empty() {
            let (verb, whose) = if self.keep_password.len() == 1 {
                ("keeps", "its")
            } else {
                ("keep", "their")
            };
            parts.push(format!(
                "{} {verb} the password already set for {whose} login: {}",
                self.keep_password.len(),
                self.keep_password.join(", ")
            ));
        }
        if self.need_password.is_empty() {
            parts.push("every one has a password".to_owned());
        } else {
            parts.push(format!(
                "{} need a password: {}",
                self.need_password.len(),
                self.need_password.join(", ")
            ));
        }
        parts.join("; ")
    }

    /// Its second line: what the CSV routed, and the skipped rows by masked email.
    #[must_use]
    pub fn detail(&self) -> String {
        let mut line = format!(
            "{} AI emails routed ({} moved from another mailbox, {} new mailboxes); {} rows \
             skipped (no AI email, or no mailbox login in the guess or correction)",
            self.routed,
            self.moved,
            self.created,
            self.skipped.len()
        );
        if !self.skipped.is_empty() {
            line.push_str(": ");
            line.push_str(&self.skipped.join(", "));
        }
        line
    }
}

/// The owner's CSV header columns the importer reads.
struct Columns {
    email: usize,
    guess: usize,
    correction: Option<usize>,
}

fn columns(header: &[String]) -> Result<Columns, String> {
    let find = |want: &dyn Fn(&str) -> bool| {
        header
            .iter()
            .position(|cell| want(&cell.trim().to_ascii_lowercase()))
    };
    let email = find(&|c| c == "ai login email" || c.contains("login email"))
        .ok_or("the CSV has no `AI login email` column")?;
    let guess = find(&|c| c.starts_with("codes arrive in"))
        .ok_or("the CSV has no `codes arrive in (my guess)` column")?;
    let correction = find(&|c| c.starts_with("your correction"));
    Ok(Columns {
        email,
        guess,
        correction,
    })
}

/// `cell` as the importer compares its words: lowercased, without any `(…)` aside, with its
/// runs of space and punctuation as one space, trimmed.
fn plain_words(cell: &str) -> String {
    let mut depth = 0usize;
    let mut out = String::new();
    for c in cell.to_lowercase().chars() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if depth > 0 => {}
            c if c.is_alphanumeric() || !c.is_ascii() => out.push(c),
            _ => {
                if !out.ends_with(' ') {
                    out.push(' ');
                }
            }
        }
    }
    out.trim().to_owned()
}

/// Whether a correction cell says the guess is right (`ok`, `yes`, `correct`, `✓`, …): the
/// guess is then read, as for a blank one.
fn affirms(cell: &str) -> bool {
    let trimmed = cell.trim();
    matches!(trimmed, "✓" | "✔" | "✅" | "👍" | "+1")
        || matches!(
            plain_words(trimmed).as_str(),
            "ok" | "okay"
                | "yes"
                | "y"
                | "yep"
                | "correct"
                | "right"
                | "fine"
                | "good"
                | "same"
                | "agreed"
                | "as guessed"
                | "guess is right"
                | "your guess is right"
                | "same as guess"
                | "same as your guess"
        )
}

/// Whether a cell, as a whole, says the AI email is its own mailbox (`itself`, `its own
/// mailbox`, `itself (gmail)`): never a cell that merely holds such a word (`same as above`).
fn says_itself(cell: &str) -> bool {
    matches!(
        plain_words(cell).as_str(),
        "itself"
            | "self"
            | "own"
            | "its own"
            | "its own mailbox"
            | "its own inbox"
            | "own mailbox"
            | "own inbox"
            | "the address itself"
            | "the email itself"
            | "the same address"
            | "same address"
    )
}

/// The mailbox login a guess or correction cell names: its first address, else the AI email's
/// own base address where the whole cell says so ([`says_itself`]).
fn login_in_cell(cell: &str, ai_email: &str) -> Option<String> {
    let address = cell
        .split(|c: char| c.is_whitespace() || ",;<>()[]\"'".contains(c))
        .map(|w| w.trim_matches(|c: char| c == '.' || c == ':'))
        .find(|w| {
            w.split_once('@')
                .is_some_and(|(l, d)| !l.is_empty() && d.contains('.') && !d.contains('@'))
        });
    if let Some(found) = address {
        return Some(found.to_owned());
    }
    says_itself(cell).then(|| base_address(ai_email).unwrap_or_else(|| ai_email.to_owned()))
}

/// What [`routes_in_csv`] read of the owner's CSV.
struct CsvRoutes {
    /// Each AI email (lowercased) and the mailbox login its codes arrive in.
    routes: Vec<(String, String)>,
    /// The skipped rows, each as its masked email (or `row N` where it has none).
    skipped: Vec<String>,
}

/// Each AI email (lowercased) of the CSV and the mailbox login its codes arrive in: the
/// correction where it is neither blank nor a yes to the guess ([`affirms`]), else the guess;
/// and the skipped rows, each as its masked email (or `row N` where it has none).
fn routes_in_csv(csv_text: &str) -> Result<CsvRoutes, String> {
    let mut records = super::accounts_sync::csv_records(csv_text);
    let header = loop {
        match records.next() {
            None => return Err("the CSV is empty".to_owned()),
            Some(Err(why)) => return Err(format!("the CSV is malformed: {why}")),
            Some(Ok(line)) if line.trim().is_empty() => {}
            Some(Ok(line)) => break super::accounts_sync::parse_csv_line(line),
        }
    };
    let cols = columns(&header)?;
    let mut routes: Vec<(String, String)> = Vec::new();
    let mut skipped = Vec::new();
    for (row, record) in records.enumerate() {
        let line = record.map_err(|why| format!("the CSV is malformed: {why}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let cells = super::accounts_sync::parse_csv_line(line);
        let cell = |at: usize| cells.get(at).map_or("", |c| c.trim());
        let email = cell(cols.email).to_ascii_lowercase();
        if normalize_pattern(&email).is_none_or(|p| p.contains('*')) {
            skipped.push(format!("row {}", row + 2));
            continue;
        }
        let correction = cols.correction.map_or("", cell);
        let said = if correction.is_empty() || affirms(correction) {
            cell(cols.guess)
        } else {
            correction
        };
        let Some(login) = login_in_cell(said, &email) else {
            skipped.push(mask_login(&email));
            continue;
        };
        match routes.iter().find(|(e, _)| *e == email) {
            Some((_, held)) if !held.eq_ignore_ascii_case(&login) => {
                return Err(format!(
                    "two rows route {} to two different mailboxes ({} and {}); make them agree",
                    mask_login(&email),
                    mask_login(held),
                    mask_login(&login)
                ));
            }
            Some(_) => {}
            None => routes.push((email, login)),
        }
    }
    Ok(CsvRoutes { routes, skipped })
}

/// One existing `[[mailboxes]]` entry as the importer edits it.
struct Entry {
    login: String,
    receives: Vec<String>,
    changed: bool,
    /// Byte range of its `receives` value, where it has one.
    value: Option<std::ops::Range<usize>>,
    /// Byte offset just past its `[[mailboxes]]` header line.
    after_header: usize,
}

/// A TOML basic string of `s`.
fn quoted(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A `receives` array: on one line for three patterns or fewer, else one per line.
fn receives_value(patterns: &[String]) -> String {
    if patterns.len() <= 3 {
        let inner: Vec<String> = patterns.iter().map(|p| quoted(p)).collect();
        return format!("[{}]", inner.join(", "));
    }
    let mut out = String::from("[\n");
    for p in patterns {
        out.push_str("    ");
        out.push_str(&quoted(p));
        out.push_str(",\n");
    }
    out.push(']');
    out
}

/// `entry` without its `receives`, for the check that the import kept everything else.
fn without_receives(entry: &aterm_toml::Value) -> Option<aterm_toml::Table> {
    let mut table = entry.as_table()?.clone();
    table.remove("receives");
    Some(table)
}

/// Merge `csv_text` (the owner's accounts CSV:
/// `computer,AI login email,services,codes arrive in (my guess),why I guessed,your correction
/// (leave blank if my guess is right)`) into `toml_text` (`ai-mailbox.toml`, `None` where none
/// exists yet). Each AI email goes into the `receives` of the mailbox the correction names,
/// else the guess, and leaves every other entry's exact `receives` (wildcards stay); a login no
/// entry has gets a new `[[mailboxes]]` entry: with no `password` line where the file already
/// keeps a password for that login (its top-level `user`'s, or a per-email one, which the entry
/// then uses: [`super::headless_oauth::MailboxConfig::completed_route`]), else with an empty
/// password to fill in. Every byte outside the changed `receives` values and the appended
/// entries is kept, which is checked: the result must parse, and every key but `receives` of
/// every old entry, and every key outside `mailboxes`, must read back equal, or nothing is
/// planned. `home` expands `~/` in a `password_file` to judge which mailboxes still need a
/// password.
///
/// # Errors
/// The CSV is malformed or lacks its columns, two rows route one email to two mailboxes, the
/// TOML does not parse or keeps its mailboxes another way than root `[[mailboxes]]` tables,
/// or the merged text would not read back as the old one plus the routes.
pub fn plan_import(
    csv_text: &str,
    toml_text: Option<&str>,
    home: Option<&Path>,
) -> Result<ImportPlan, String> {
    use aterm_toml::edit::{DocumentMut, Item};
    let CsvRoutes { routes, skipped } = routes_in_csv(csv_text)?;
    let original = toml_text.unwrap_or("");
    let (bom, body) = match original.strip_prefix('\u{feff}') {
        Some(rest) => ("\u{feff}", rest),
        None => ("", original),
    };
    let old: aterm_toml::Table = aterm_toml::from_str(body)
        .map_err(|_| "the mailbox file is not valid TOML; nothing was written".to_owned())?;
    let old_cfg = super::headless_oauth::MailboxConfig::from_toml_str(body, home);
    for nested in [
        &["ai", "mailbox"][..],
        &["harness", "mailbox"],
        &["mailbox"],
    ] {
        let mut at = Some(&old);
        for key in nested {
            at = at
                .and_then(|t| t.get(key))
                .and_then(aterm_toml::Value::as_table);
        }
        if at.is_some_and(|t| t.get("mailboxes").is_some()) {
            return Err(format!(
                "the mailbox file keeps its mailboxes under [{}]; the importer edits root \
                 [[mailboxes]] tables only, so nothing was written",
                nested.join(".")
            ));
        }
    }
    let doc: DocumentMut = body
        .parse()
        .map_err(|_| "the mailbox file is not valid TOML; nothing was written".to_owned())?;
    let mut entries: Vec<Entry> = Vec::new();
    match doc.get("mailboxes") {
        None => {}
        Some(Item::ArrayOfTables(tables)) => {
            for table in tables.iter() {
                let header = table
                    .span()
                    .ok_or("a [[mailboxes]] entry has no header to edit beside")?;
                let after_header = body[header.start..]
                    .find('\n')
                    .map_or(body.len(), |n| header.start + n + 1);
                let login = table
                    .get("login")
                    .or_else(|| table.get("user"))
                    .or_else(|| table.get("username"))
                    .and_then(Item::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_owned();
                let (receives, value) = match table.get("receives") {
                    None => (Vec::new(), None),
                    Some(item) => {
                        let span = item
                            .span()
                            .ok_or("a [[mailboxes]] receives value has no place to edit")?;
                        let patterns = match item.as_array() {
                            Some(array) => array
                                .iter()
                                .filter_map(|v| v.as_str())
                                .filter_map(normalize_pattern)
                                .collect(),
                            None => item
                                .as_str()
                                .and_then(normalize_pattern)
                                .into_iter()
                                .collect(),
                        };
                        (patterns, Some(span))
                    }
                };
                entries.push(Entry {
                    login,
                    receives,
                    changed: false,
                    value,
                    after_header,
                });
            }
        }
        Some(_) => {
            return Err(
                "the mailbox file names `mailboxes` another way than [[mailboxes]] tables; \
                 nothing was written"
                    .to_owned(),
            );
        }
    }
    let old_count = entries.len();
    let mut created: Vec<(String, Vec<String>)> = Vec::new();
    let mut moved = 0;
    for (email, login) in &routes {
        let lower_login = login.to_ascii_lowercase();
        for entry in entries
            .iter_mut()
            .filter(|e| e.login.to_ascii_lowercase() != lower_login)
        {
            let before = entry.receives.len();
            entry.receives.retain(|p| p != email);
            if entry.receives.len() != before {
                entry.changed = true;
                moved += 1;
            }
        }
        if let Some(entry) = entries
            .iter_mut()
            .find(|e| e.login.to_ascii_lowercase() == lower_login)
        {
            if !entry.receives.contains(email) {
                entry.receives.push(email.clone());
                entry.changed = true;
            }
        } else if let Some((_, receives)) = created
            .iter_mut()
            .find(|(l, _)| l.to_ascii_lowercase() == lower_login)
        {
            if !receives.contains(email) {
                receives.push(email.clone());
            }
        } else {
            created.push((login.clone(), vec![email.clone()]));
        }
    }
    let mut edits: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    for entry in entries.iter().filter(|e| e.changed) {
        let value = receives_value(&entry.receives);
        edits.push(match &entry.value {
            Some(span) => (span.clone(), value),
            None => (
                entry.after_header..entry.after_header,
                format!("receives = {value}\n"),
            ),
        });
    }
    edits.sort_by_key(|(span, _)| std::cmp::Reverse(span.start));
    let mut text = body.to_owned();
    for (span, with) in edits {
        if span.end > text.len()
            || !text.is_char_boundary(span.start)
            || !text.is_char_boundary(span.end)
        {
            return Err("an edit fell outside the mailbox file; nothing was written".to_owned());
        }
        text.replace_range(span, &with);
    }
    let mut keep_password = Vec::new();
    if !created.is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(
            "# Added by `aterm harness accounts mailboxes import`. Fill in each empty `password`\n\
             # (and, where the mailbox asks for them, `mailbox_password` and `totp_secret`): a\n\
             # mailbox with no password is never tried.\n",
        );
        for (at, (login, receives)) in created.iter().enumerate() {
            if at > 0 {
                text.push('\n');
            }
            let kept = old_cfg
                .as_ref()
                .and_then(|cfg| existing_password(cfg, login, receives));
            text.push_str(&format!("[[mailboxes]]\nlogin = {}\n", quoted(login)));
            match kept {
                Some(whose) => {
                    keep_password.push(mask_login(login));
                    text.push_str(&format!("# no `password`: {whose}\n"));
                }
                None => text.push_str("password = \"\"\n"),
            }
            text.push_str(&format!("receives = {}\n", receives_value(receives)));
        }
    }
    let new: aterm_toml::Table = aterm_toml::from_str(&text)
        .map_err(|_| "the merged mailbox file would not parse; nothing was written".to_owned())?;
    check_kept(&old, &new, old_count, created.len())?;
    let parsed = super::headless_oauth::MailboxConfig::from_toml_str(&text, home);
    let mailboxes = parsed.as_ref().map_or(0, |cfg| cfg.mailboxes.len());
    let need_password = parsed
        .map(|cfg| {
            cfg.mailboxes
                .iter()
                .filter(|m| {
                    !cfg.is_top_level_login(&m.login) && !cfg.completed_route(m, None).ready()
                })
                .map(MailboxRoute::masked)
                .collect()
        })
        .unwrap_or_default();
    let text = format!("{bom}{text}");
    Ok(ImportPlan {
        changed: text != original,
        text,
        mailboxes,
        need_password,
        keep_password,
        routed: routes.len(),
        skipped,
        moved,
        created: created.len(),
    })
}

/// Where `cfg` (the mailbox file before the import) already keeps a password for `login`, a
/// new entry for it receiving `receives` would use, in words for the comment the importer writes
/// in place of its `password` line: the top-level one where `login` is the file's `user` (kept
/// in the file or beside it, `~/.secrets/proton_password`), else a per-email one
/// ([`super::headless_oauth::MailboxConfig::completed_route`]).
fn existing_password(
    cfg: &super::headless_oauth::MailboxConfig,
    login: &str,
    receives: &[String],
) -> Option<&'static str> {
    if cfg.is_top_level_login(login) {
        return Some(
            "its login is the top-level `user`, so it uses the top-level password, second \
             password and TOTP secret",
        );
    }
    let entry = MailboxRoute {
        login: login.to_owned(),
        receives: receives.to_vec(),
        ..MailboxRoute::default()
    };
    cfg.completed_route(&entry, None)
        .ready()
        .then_some("it uses the per-email password this file keeps for its login")
}

/// That `new` is `old` with only `receives` values changed and `created` entries appended.
fn check_kept(
    old: &aterm_toml::Table,
    new: &aterm_toml::Table,
    old_count: usize,
    created: usize,
) -> Result<(), String> {
    let refuse = || {
        "the merged mailbox file would change more than `receives`; nothing was written".to_owned()
    };
    let strip = |t: &aterm_toml::Table| {
        let mut t = t.clone();
        t.remove("mailboxes");
        t
    };
    if strip(old) != strip(new) {
        return Err(refuse());
    }
    let list = |t: &aterm_toml::Table| -> Vec<aterm_toml::Value> {
        t.get("mailboxes")
            .and_then(aterm_toml::Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let (before, after) = (list(old), list(new));
    if before.len() != old_count || after.len() != old_count + created {
        return Err(refuse());
    }
    for (was, now) in before.iter().zip(&after) {
        if without_receives(was).is_none() || without_receives(was) != without_receives(now) {
            return Err(refuse());
        }
    }
    Ok(())
}

/// `aterm harness accounts mailboxes import <csv> --into <toml> [--dry-run]`: [`plan_import`],
/// then the result written atomically with mode 600 where it changed (never under `dry_run`).
/// Prints counts and masked logins only. A symlinked `<toml>` is written through to its target.
pub fn run_import(
    csv: &Path,
    into: &Path,
    dry_run: bool,
    home: Option<&Path>,
    out: &mut dyn std::io::Write,
    err: &mut dyn std::io::Write,
) -> std::process::ExitCode {
    const VERB: &str = "aterm harness accounts mailboxes import";
    let csv_text = match super::accounts::read_bounded(csv, IMPORT_CSV_MAX) {
        Ok(Some(text)) => text,
        Ok(None) => {
            let _ = writeln!(err, "{VERB}: the CSV is not there; nothing was written");
            return std::process::ExitCode::from(1);
        }
        Err(e) => {
            let _ = writeln!(
                err,
                "{VERB}: the CSV could not be read ({e}); nothing was written"
            );
            return std::process::ExitCode::from(1);
        }
    };
    let target = std::fs::canonicalize(into).unwrap_or_else(|_| into.to_path_buf());
    let toml_text = match super::accounts::read_bounded(&target, IMPORT_TOML_MAX) {
        Ok(text) => text,
        Err(e) => {
            let _ = writeln!(
                err,
                "{VERB}: the mailbox file could not be read ({e}); nothing was written"
            );
            return std::process::ExitCode::from(1);
        }
    };
    let plan = match plan_import(&csv_text, toml_text.as_deref(), home) {
        Ok(plan) => plan,
        Err(why) => {
            let _ = writeln!(err, "{VERB}: {why}");
            return std::process::ExitCode::from(1);
        }
    };
    if plan.changed
        && !dry_run
        && let Err(e) = super::upgrade_models::write_atomic_mode(&target, &plan.text, Some(0o600))
    {
        let _ = writeln!(
            err,
            "{VERB}: the mailbox file could not be written ({e}); it is unchanged"
        );
        return std::process::ExitCode::from(1);
    }
    let _ = writeln!(out, "{}", plan.summary());
    let _ = writeln!(out, "{}", plan.detail());
    let _ = writeln!(
        out,
        "{}",
        match (plan.changed, dry_run) {
            (true, false) => {
                "written (mode 600); no password, second password or TOTP secret was changed"
            }
            (true, true) => "--dry-run: nothing was written",
            (false, _) => "nothing changed; nothing was written",
        }
    );
    std::process::ExitCode::SUCCESS
}

#[cfg(test)]
#[path = "mailbox_routes_tests.rs"]
mod tests;
