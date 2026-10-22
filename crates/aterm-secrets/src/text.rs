// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The vault's text file (docs/DESIGN-secrets-2026-10-20.md §4): what a person edits.
//!
//! [`Document`] keeps the source lines, so programmatic edits ([`Document::add_machine`],
//! [`Document::set_entry`], …) change only the lines they must and keep every comment and
//! blank line the owner wrote. [`Document::lockers`] splits the file into one plaintext per
//! audience (what [`crate::envelope`] encrypts); [`parse_locker`] reads one back.
//!
//! Grammar, in brief: LF line ends (a trailing CR is dropped; a CR anywhere else, and a NUL
//! anywhere, is an error); blank and `#` lines are comments outside a heredoc (a `#` later
//! in a line is not a comment); `[machines]` holds `name = age1…` (the spaces around `=` are
//! optional); `[admins]` holds one machine name per line; `[everyone]` and `[host host …]`
//! hold entries `name=value` (the value is the rest of the line, verbatim), `name<<TAG` …
//! `TAG`, or `name<<TAG base64` … `TAG`, either heredoc optionally ending ` if-absent` (the
//! file is written only where none exists: "fill gaps") or ` if-absent=<anchor>` (and only
//! where the entry named `<anchor>`, another ` if-absent` entry every machine receiving
//! this one also receives, is not the machine's own: a set of files filled together);
//! `[join]` (a whole vault's only, so never in a locker) holds at most one of `password = …`,
//! the join password (`crate::join`), and `open = yes` (an open vault: an admin's subscriber
//! lets in every machine that asks to join, no password), and a `code-only = age1…` line for
//! each key an open vault leaves to approval by code (a machine it let go, or one that asked
//! before it opened). A section may appear more than once; its items accumulate. Errors carry
//! a 1-based line number and never quote a value.

use crate::SecretBytes;
use std::collections::{BTreeMap, BTreeSet};
use zeroize::Zeroize;

/// Whom a section is for.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Audience {
    /// `[everyone]`: every machine in `[machines]`.
    Everyone,
    /// `[host host …]`: these machines, sorted and deduplicated.
    Hosts(Vec<String>),
}

impl Audience {
    /// The section header for this audience, e.g. `[everyone]` or `[m3 m22]`.
    pub fn header(&self) -> String {
        match self {
            Audience::Everyone => "[everyone]".to_string(),
            Audience::Hosts(hosts) => format!("[{}]", hosts.join(" ")),
        }
    }

    /// True when `host` is in this audience (every host is in `Everyone`).
    pub fn includes(&self, host: &str) -> bool {
        match self {
            Audience::Everyone => true,
            Audience::Hosts(hosts) => hosts.iter().any(|h| h == host),
        }
    }
}

/// A machine line of `[machines]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Machine {
    /// Its name (`[a-z0-9][a-z0-9-]{0,62}`, not `machines`, `admins`, `everyone` or `join`).
    pub name: String,
    /// Its `age1…` recipient, as written (lowercase bech32, 62 characters).
    pub recipient: String,
}

/// One secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Its name: a file name `[A-Za-z0-9][A-Za-z0-9._-]{0,127}` or a home path `~/a/b`
    /// whose components are such names.
    pub name: String,
    /// Its value.
    pub value: SecretBytes,
    /// The section it came from.
    pub audience: Audience,
    /// Written only where no file exists yet (a heredoc's ` if-absent`): a machine's own
    /// file is never replaced, and a file this vault wrote is updated until the machine
    /// changes it, when it becomes the machine's own.
    pub if_absent: bool,
    /// The ` if-absent` entry this one is filled with (a heredoc's ` if-absent=<anchor>`;
    /// only on an ` if-absent` entry): a gap is filled, and a file the vault filled kept,
    /// only where the anchor's file is not the machine's own, so files that belong together
    /// (a release key and the profile that names it) are filled as a set, never beside the
    /// machine's own. An aterm older than anchors refuses a locker that holds one.
    pub anchor: Option<String>,
}

/// An open vault's `[join]` line: an admin's subscriber lets in every machine that asks, with
/// no password and no person at an admin Mac, except a key on a `code-only` line. Only an admin
/// makes a vault open (`aterm secrets password open`, or the line written in `aterm secrets
/// edit`); a vault without the line is not.
const OPEN_LINE: &str = "open = yes";

/// A parse failure: the 1-based line and what is wrong, never the line's contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextError {
    /// 1-based line number (0 when the failure is about the whole file).
    pub line: usize,
    /// What is wrong.
    pub why: String,
}

impl std::fmt::Display for TextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.line == 0 {
            f.write_str(&self.why)
        } else {
            write!(f, "line {}: {}", self.line, self.why)
        }
    }
}

impl std::error::Error for TextError {}

/// An error about 1-based line `line`.
fn at(line: usize, why: impl Into<String>) -> TextError {
    TextError {
        line,
        why: why.into(),
    }
}

/// An error about the whole file (or an edit's arguments).
fn whole(why: impl Into<String>) -> TextError {
    TextError {
        line: 0,
        why: why.into(),
    }
}

/// One locker's plan: whom to encrypt to and what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockerPlan {
    /// The audience.
    pub audience: Audience,
    /// The `age1…` recipients of the audience, sorted and deduplicated.
    pub recipients: Vec<String>,
    /// The audience's sections in this file's syntax, headers included (what
    /// [`parse_locker`] reads back).
    pub text: SecretBytes,
}

/// A parsed, editable vault text. Holds the source, so it is itself a secret: `Debug` is
/// redacted and the lines are wiped on drop.
///
/// Every `Document` but one [`Document::new_vault`] made from an invalid name or recipient
/// is valid: it parses under [`Document::parse`]'s rules, and an edit that would break them
/// is refused and changes nothing.
#[derive(Clone)]
pub struct Document {
    lines: Vec<String>,
    model: Model,
}

impl std::fmt::Debug for Document {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Document(<{} lines redacted>)", self.lines.len())
    }
}

impl Drop for Document {
    fn drop(&mut self) {
        for line in &mut self.lines {
            line.zeroize();
        }
    }
}

/// The largest value, and the largest file.
pub const MAX_VALUE: usize = 1 << 20;
/// The largest text file. A vault's version is the tighter bound on what it can publish:
/// sealed, it holds every entry twice (the admins' locker and its audience's), armored,
/// within [`crate::envelope::MAX_ENVELOPE`], so about 5.9 MiB of entries.
pub const MAX_TEXT: usize = 8 << 20;
/// The most distinct sets of hosts a vault's sections name: each is sealed in a locker of
/// its own, beside the admins' and `[everyone]`'s, within [`crate::envelope::MAX_LOCKERS`].
pub const MAX_HOST_AUDIENCES: usize = crate::envelope::MAX_LOCKERS - 2;

/// The section names that are not host names.
const RESERVED: [&str; 4] = ["machines", "admins", "everyone", "join"];

/// Why an anchor is refused before any look at the entries.
const ANCHOR_NAME: &str =
    "an anchor names another entry: if-absent=<a file name or a ~/ home path>";

/// Why a machine name is refused.
const INVALID_MACHINE: &str =
    "an invalid machine name: [a-z0-9][a-z0-9-]{0,62}, not machines, admins, everyone or join";

/// Why a recipient is refused.
const INVALID_RECIPIENT: &str =
    "an invalid recipient: expected age1… (lowercase bech32, 62 characters)";

/// What a section header declares.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SectionKind {
    Machines,
    Admins,
    Join,
    Audience(Audience),
}

/// One section header and the extent of its items.
#[derive(Debug, Clone)]
struct Section {
    kind: SectionKind,
    /// 0-based index of the header line.
    header: usize,
    /// 0-based index of the last line of the section's last item (the header when it has
    /// none): where an appended item goes after.
    last_item: usize,
}

/// An entry and where it is.
#[derive(Debug, Clone)]
struct Located {
    entry: Entry,
    /// 0-based index of its first line.
    first: usize,
    /// 0-based index of its last line (the heredoc terminator, or `first`).
    last: usize,
}

/// What a parse found, with line indices for edits.
#[derive(Debug, Clone, Default)]
struct Model {
    machines: Vec<(Machine, usize)>,
    admins: Vec<(String, usize)>,
    /// The join password and its line.
    join: Option<(SecretBytes, usize)>,
    /// Where an open vault's `[join]` line is (never beside a password).
    open: Option<usize>,
    /// The keys `[join]`'s `code-only` lines name, and where.
    code_only: Vec<(String, usize)>,
    sections: Vec<Section>,
    entries: Vec<Located>,
}

/// Which rules a parse holds the text to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// A whole vault ([`Document::parse`]): hosts and admins are machines, at least one admin.
    Vault,
    /// One locker ([`parse_locker`]): no `[machines]`, `[admins]` or `[join]`, hosts
    /// unchecked.
    Locker,
    /// A person's own plain file ([`parse_plain`]): items before the first header are
    /// `[everyone]`'s, `[machines]`, `[admins]` and `[join]` are read but not held to a
    /// vault's rules, hosts unchecked.
    Plain,
}

impl Document {
    /// Parse and validate a whole vault text: every section known, every host in an
    /// audience or `[admins]` declared in `[machines]`, at least one admin, every recipient
    /// a valid `age1…`, no duplicate machine, recipient, admin, or entry name within one
    /// audience, no entry name in two host sections that share a host, at most one `[join]`
    /// line (a strong join password, or an `open` line), heredocs terminated, base64 heredocs
    /// canonical, names and values within their rules and sizes, at most
    /// [`MAX_HOST_AUDIENCES`] distinct sets of hosts.
    pub fn parse(text: &str) -> Result<Document, TextError> {
        let lines = split_lines(text)?;
        match parse_lines(&lines, Mode::Vault) {
            Ok(model) => Ok(Document { lines, model }),
            Err(error) => {
                wipe_lines(lines);
                Err(error)
            }
        }
    }

    /// A new vault text: a comment header explaining the format, `[machines]` with this one
    /// machine, `[admins]` with it, and an empty `[everyone]`. An invalid `machine` or
    /// `recipient` yields a document that does not validate: its edits are refused, and
    /// [`Document::parse`] of its [`Document::render`] names the line.
    pub fn new_vault(machine: &str, recipient: &str) -> Document {
        let mut lines: Vec<String> = TEMPLATE.iter().map(|line| (*line).to_string()).collect();
        lines.extend([
            String::new(),
            "[machines]".to_string(),
            format!("{machine} = {recipient}"),
            String::new(),
            "[admins]".to_string(),
            machine.to_string(),
            String::new(),
            "[everyone]".to_string(),
        ]);
        let model = parse_lines(&lines, Mode::Vault).unwrap_or_default();
        Document { lines, model }
    }

    /// The text, with every edit applied (LF line ends, a final LF).
    pub fn render(&self) -> SecretBytes {
        render_lines(self.lines.iter().map(String::as_str))
    }

    /// The machines, in file order.
    pub fn machines(&self) -> Vec<Machine> {
        self.model
            .machines
            .iter()
            .map(|(machine, _)| machine.clone())
            .collect()
    }

    /// The admins, in file order.
    pub fn admins(&self) -> Vec<String> {
        self.model
            .admins
            .iter()
            .map(|(admin, _)| admin.clone())
            .collect()
    }

    /// The join password (`[join]`'s `password`), if the vault has one.
    pub fn join_password(&self) -> Option<SecretBytes> {
        self.model
            .join
            .as_ref()
            .map(|(password, _)| password.clone())
    }

    /// Set the join password (`Some`), replacing any, or remove it (`None`). Refuses a weak
    /// one ([`crate::join::strength`]) without quoting it. Blanks around it are dropped. A
    /// password set on an open vault closes it: it takes the `open` line's place.
    pub fn set_join_password(&mut self, password: Option<&[u8]>) -> Result<(), TextError> {
        let existing = self.model.join.as_ref().map(|(_, line)| *line);
        let Some(password) = password else {
            let Some(line) = existing else {
                return Ok(());
            };
            let mut lines = self.lines.clone();
            lines.remove(line).zeroize();
            return self.commit(lines);
        };
        crate::join::strength(password).map_err(whole)?;
        let Ok(text) = std::str::from_utf8(password.trim_ascii()) else {
            return Err(whole("a join password is UTF-8 text"));
        };
        let spelled = format!("password = {text}");
        match existing.or(self.model.open) {
            Some(line) => {
                let mut lines = self.lines.clone();
                std::mem::replace(&mut lines[line], spelled).zeroize();
                self.commit(lines)
            }
            None => self.append_to(&SectionKind::Join, "[join]", vec![spelled]),
        }
    }

    /// Whether the vault is open (`[join]`'s `open = yes`): an admin's subscriber lets in
    /// every machine that asks, with no password, except a key [`Document::code_only`] names.
    pub fn open(&self) -> bool {
        self.model.open.is_some()
    }

    /// Make the vault open (`true`), its line in the join password's place if it has one, or
    /// close it (`false`), removing the `open` line and leaving a join password be. Either
    /// leaves the `code-only` lines be, and the state it is already in changes nothing, however its
    /// line is spelled.
    pub fn set_open(&mut self, open: bool) -> Result<(), TextError> {
        match (self.model.open, open) {
            (Some(_), true) | (None, false) => Ok(()),
            (Some(line), false) => {
                let mut lines = self.lines.clone();
                lines.remove(line).zeroize();
                self.commit(lines)
            }
            (None, true) => match self.model.join.as_ref().map(|(_, line)| *line) {
                Some(line) => {
                    let mut lines = self.lines.clone();
                    std::mem::replace(&mut lines[line], OPEN_LINE.to_string()).zeroize();
                    self.commit(lines)
                }
                None => self.append_to(&SectionKind::Join, "[join]", vec![OPEN_LINE.to_string()]),
            },
        }
    }

    /// The keys an open vault leaves to approval by code (`[join]`'s `code-only` lines), in
    /// file order: a machine it let go, or one that asked before it opened. Its approver never
    /// lets one in by itself.
    pub fn code_only(&self) -> Vec<String> {
        self.model
            .code_only
            .iter()
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// Name `recipient` on a `code-only` line in `[join]`, unless one does already. Refuses a
    /// bad recipient. [`Document::add_machine`] removes the line again.
    pub fn add_code_only(&mut self, recipient: &str) -> Result<(), TextError> {
        if !valid_recipient(recipient) {
            return Err(whole(INVALID_RECIPIENT));
        }
        if self.model.code_only.iter().any(|(key, _)| key == recipient) {
            return Ok(());
        }
        self.append_to(
            &SectionKind::Join,
            "[join]",
            vec![format!("code-only = {recipient}")],
        )
    }

    /// Every entry, in file order.
    pub fn entries(&self) -> Vec<Entry> {
        self.model
            .entries
            .iter()
            .map(|located| located.entry.clone())
            .collect()
    }

    /// The entry `name` of the sections for `audience`, if there is one.
    pub fn entry(&self, audience: &Audience, name: &str) -> Option<&Entry> {
        let audience = normalize(audience).ok()?;
        self.model
            .entries
            .iter()
            .map(|located| &located.entry)
            .find(|entry| entry.audience == audience && entry.name == name)
    }

    /// Whether any entry is anchored (` if-absent=<anchor>`), which an aterm older than
    /// anchors cannot read.
    pub fn anchored(&self) -> bool {
        self.model
            .entries
            .iter()
            .any(|located| located.entry.anchor.is_some())
    }

    /// One locker per distinct audience that has at least one machine, `[everyone]` first
    /// (always present, even with no entries, so every machine has a locker that tells it
    /// it is in), then host audiences in sorted order. A locker's text is one header and
    /// the audience's entries spelled exactly as in this file, in file order, without
    /// comments.
    pub fn lockers(&self) -> Vec<LockerPlan> {
        let mut audiences: BTreeSet<&Audience> = BTreeSet::new();
        for section in &self.model.sections {
            if let SectionKind::Audience(audience @ Audience::Hosts(_)) = &section.kind {
                audiences.insert(audience);
            }
        }
        let everyone = Audience::Everyone;
        let mut plans = Vec::with_capacity(audiences.len() + 1);
        for audience in std::iter::once(&everyone).chain(audiences) {
            let recipients: BTreeSet<&str> = self
                .model
                .machines
                .iter()
                .filter(|(machine, _)| audience.includes(&machine.name))
                .map(|(machine, _)| machine.recipient.as_str())
                .collect();
            if recipients.is_empty() {
                continue;
            }
            let header = audience.header();
            let body = self
                .model
                .entries
                .iter()
                .filter(|located| located.entry.audience == *audience)
                .flat_map(|located| {
                    self.lines[located.first..=located.last]
                        .iter()
                        .map(String::as_str)
                });
            plans.push(LockerPlan {
                audience: audience.clone(),
                recipients: recipients.into_iter().map(str::to_string).collect(),
                text: render_lines(std::iter::once(header.as_str()).chain(body)),
            });
        }
        plans
    }

    /// Append `name = recipient` to `[machines]` (creating the section if absent), and remove
    /// the `code-only` line naming `recipient`, if any. Refuses a duplicate name or recipient, a
    /// bad name, or a bad recipient.
    pub fn add_machine(&mut self, name: &str, recipient: &str) -> Result<(), TextError> {
        if !valid_machine_name(name) {
            return Err(whole(INVALID_MACHINE));
        }
        if !valid_recipient(recipient) {
            return Err(whole(INVALID_RECIPIENT));
        }
        for (machine, _) in &self.model.machines {
            if machine.name == name {
                return Err(whole(format!("machine `{name}` is already in [machines]")));
            }
            if machine.recipient == recipient {
                return Err(whole(format!(
                    "that recipient is already machine `{}`",
                    machine.name
                )));
            }
        }
        let mut next = self.clone();
        next.append_to(
            &SectionKind::Machines,
            "[machines]",
            vec![format!("{name} = {recipient}")],
        )?;
        // A key let in is no longer one left to approval by code.
        let held = next
            .model
            .code_only
            .iter()
            .find(|(key, _)| key == recipient);
        if let Some(line) = held.map(|(_, line)| *line) {
            let mut lines = next.lines.clone();
            lines.remove(line);
            next.commit(lines)?;
        }
        *self = next;
        Ok(())
    }

    /// Remove a machine: its `[machines]` line, its `[admins]` line, and its name from every
    /// multi-host section header. Refuses (changing nothing) when a section that names only
    /// this machine holds entries, saying which line, unless `drop_own_sections` is set, in
    /// which case those sections and their entries are removed too; such a section with no
    /// entry left (an `unset` of its last) goes with the machine either way. Returns false
    /// when no such machine. Refuses removing the last admin.
    pub fn remove_machine(
        &mut self,
        name: &str,
        drop_own_sections: bool,
    ) -> Result<bool, TextError> {
        if !self
            .model
            .machines
            .iter()
            .any(|(machine, _)| machine.name == name)
        {
            return Ok(false);
        }
        let mut drop: BTreeSet<usize> = BTreeSet::new();
        drop.extend(
            self.model
                .machines
                .iter()
                .filter(|(m, _)| m.name == name)
                .map(|(_, i)| *i),
        );
        drop.extend(
            self.model
                .admins
                .iter()
                .filter(|(a, _)| a == name)
                .map(|(_, i)| *i),
        );
        let mut rewrite: BTreeMap<usize, String> = BTreeMap::new();
        for section in &self.model.sections {
            let SectionKind::Audience(Audience::Hosts(hosts)) = &section.kind else {
                continue;
            };
            if !hosts.iter().any(|h| h == name) {
                continue;
            }
            if hosts.len() > 1 {
                let rest: Vec<String> = hosts.iter().filter(|h| *h != name).cloned().collect();
                rewrite.insert(section.header, Audience::Hosts(rest).header());
            } else if drop_own_sections || section.last_item == section.header {
                drop.extend(section.header..=section.last_item);
            } else {
                return Err(at(
                    section.header + 1,
                    format!(
                        "section [{name}] names only `{name}`: remove its entries first, or drop its sections with the machine"
                    ),
                ));
            }
        }
        let mut lines = Vec::with_capacity(self.lines.len());
        for (i, line) in self.lines.iter().enumerate() {
            if drop.contains(&i) {
                continue;
            }
            match rewrite.get(&i) {
                Some(header) => lines.push(header.clone()),
                None => lines.push(line.clone()),
            }
        }
        self.commit(lines)?;
        Ok(true)
    }

    /// Add a machine to `[admins]` (it must be a machine). No-op when already there.
    pub fn add_admin(&mut self, name: &str) -> Result<(), TextError> {
        if !self
            .model
            .machines
            .iter()
            .any(|(machine, _)| machine.name == name)
        {
            return Err(if valid_machine_name(name) {
                whole(format!("`{name}` is not in [machines]"))
            } else {
                whole("an invalid machine name")
            });
        }
        if self.model.admins.iter().any(|(admin, _)| admin == name) {
            return Ok(());
        }
        self.append_to(&SectionKind::Admins, "[admins]", vec![name.to_string()])
    }

    /// Remove a machine from `[admins]`. Refuses removing the last admin. Returns false when
    /// it was not an admin.
    pub fn remove_admin(&mut self, name: &str) -> Result<bool, TextError> {
        let Some(index) = self
            .model
            .admins
            .iter()
            .find(|(admin, _)| admin == name)
            .map(|(_, i)| *i)
        else {
            return Ok(false);
        };
        if self.model.admins.len() == 1 {
            return Err(whole(format!(
                "`{name}` is the last admin: add another admin first"
            )));
        }
        let mut lines = self.lines.clone();
        lines.remove(index).zeroize();
        self.commit(lines)?;
        Ok(true)
    }

    /// Set an entry in the section for `audience` (the first such section, created at the
    /// end if absent): replaces the existing entry's line(s) in place, or appends to the
    /// section. Picks the spelling: `name=value` for one line of UTF-8 without CR, LF or NUL;
    /// a text heredoc for UTF-8 ending in LF without CR or NUL; otherwise a base64 heredoc
    /// with 64-column lines. A heredoc TAG is `END`, or `END1`, `END2`, … if a value line
    /// equals it. Refuses (changing nothing) what would break the document's rules, such as
    /// a name another host section sharing a host already sets.
    pub fn set_entry(
        &mut self,
        audience: &Audience,
        name: &str,
        value: &[u8],
    ) -> Result<(), TextError> {
        self.set_entry_with(audience, name, value, false, None)
    }

    /// [`Document::set_entry`], marking the entry ` if-absent` when `if_absent` (always a
    /// heredoc then: a base64 one for a value a text heredoc cannot hold exactly), and
    /// anchoring it to the ` if-absent` entry `anchor` names (` if-absent=<anchor>`). An
    /// anchor without `if_absent`, or one that is not another ` if-absent` entry every host
    /// receiving this one also receives, is refused.
    pub fn set_entry_with(
        &mut self,
        audience: &Audience,
        name: &str,
        value: &[u8],
        if_absent: bool,
        anchor: Option<&str>,
    ) -> Result<(), TextError> {
        let audience = normalize(audience)?;
        if !valid_entry_name(name) {
            return Err(whole(
                "an invalid entry name: a file name [A-Za-z0-9][A-Za-z0-9._-]{0,127} or a ~/ home path",
            ));
        }
        if value.len() > MAX_VALUE {
            return Err(whole(format!("the value is larger than {MAX_VALUE} bytes")));
        }
        if let Some(anchor) = anchor {
            if !if_absent {
                return Err(whole("an anchor goes with if-absent"));
            }
            if !valid_entry_name(anchor) || anchor == name {
                return Err(whole(ANCHOR_NAME));
            }
        }
        let spelled = spell(name, value, if_absent, anchor);
        let existing = self
            .model
            .entries
            .iter()
            .find(|located| located.entry.audience == audience && located.entry.name == name)
            .map(|located| (located.first, located.last));
        if let Some((first, last)) = existing {
            let mut lines = self.lines.clone();
            for mut old in lines.splice(first..=last, spelled) {
                old.zeroize();
            }
            return self.commit(lines);
        }
        let header = audience.header();
        let first_section = self
            .model
            .sections
            .iter()
            .find(|section| section.kind == SectionKind::Audience(audience.clone()))
            .map(|section| section.last_item);
        match first_section {
            Some(last_item) => {
                let mut lines = self.lines.clone();
                lines.splice(last_item + 1..last_item + 1, spelled);
                self.commit(lines)
            }
            None => self.append_section(&header, spelled),
        }
    }

    /// Remove an entry from the sections for `audience`. Returns false when absent.
    pub fn unset_entry(&mut self, audience: &Audience, name: &str) -> Result<bool, TextError> {
        let Ok(audience) = normalize(audience) else {
            return Ok(false);
        };
        let existing = self
            .model
            .entries
            .iter()
            .find(|located| located.entry.audience == audience && located.entry.name == name)
            .map(|located| (located.first, located.last));
        let Some((first, last)) = existing else {
            return Ok(false);
        };
        let mut lines = self.lines.clone();
        for mut old in lines.drain(first..=last) {
            old.zeroize();
        }
        self.commit(lines)?;
        Ok(true)
    }

    /// Append `items` after the last item of the last section of `kind`, or as a new
    /// section headed `header` at the end of the file.
    fn append_to(
        &mut self,
        kind: &SectionKind,
        header: &str,
        items: Vec<String>,
    ) -> Result<(), TextError> {
        let last = self
            .model
            .sections
            .iter()
            .rev()
            .find(|section| section.kind == *kind)
            .map(|section| section.last_item);
        match last {
            Some(last_item) => {
                let mut lines = self.lines.clone();
                lines.splice(last_item + 1..last_item + 1, items);
                self.commit(lines)
            }
            None => self.append_section(header, items),
        }
    }

    /// Append a new section at the end of the file, after a blank line.
    fn append_section(&mut self, header: &str, items: Vec<String>) -> Result<(), TextError> {
        let mut lines = self.lines.clone();
        if lines.last().is_some_and(|line| !line.trim().is_empty()) {
            lines.push(String::new());
        }
        lines.push(header.to_string());
        lines.extend(items);
        self.commit(lines)
    }

    /// Replace the lines with `lines` if they are a valid vault; otherwise change nothing.
    fn commit(&mut self, lines: Vec<String>) -> Result<(), TextError> {
        match parse_lines(&lines, Mode::Vault) {
            Ok(model) => {
                wipe_lines(std::mem::replace(&mut self.lines, lines));
                self.model = model;
                Ok(())
            }
            Err(error) => {
                wipe_lines(lines);
                Err(error)
            }
        }
    }
}

/// The comment block a new vault starts with.
const TEMPLATE: &[&str] = &[
    "# aterm secrets vault (docs/DESIGN-secrets-2026-10-20.md section 4).",
    "# `aterm secrets edit` opens this file; saving it publishes a new version.",
    "#",
    "# Blank lines and lines starting with # are comments (except inside a heredoc).",
    "#",
    "# [machines]  one `name = age1...` line per machine; delete a line to revoke it.",
    "# [admins]    one machine name per line: the machines that may publish.",
    "# [everyone]  entries every machine receives.",
    "# [m1 m2]     entries only these machines receive; on them they override an",
    "#             [everyone] entry of the same name.",
    "# [join]      `password = ...`: a machine whose person types it at `aterm secrets",
    "#             join` is let in by an admin's subscriber within seconds.",
    "#             Or `open = yes` instead: every machine that reaches the server and",
    "#             asks is let in, with no password, and reads [everyone].",
    "#             `code-only = age1...`: a key an open vault lets in only by its code",
    "#             (a machine it let go, or one that asked before it opened); approving",
    "#             it by code drops the line.",
    "#",
    "# Entries:",
    "#   name=value          the rest of the line, verbatim (no quotes, no comments)",
    "#   name<<END           every following line up to a line that is exactly END,",
    "#   ...                 with a final newline",
    "#   END",
    "#   name<<END base64    the same lines, decoded from base64 (binary files)",
    "#   name<<END if-absent (or `base64 if-absent`) written only where no file",
    "#                       exists yet: fill gaps, never replace a machine's own file",
    "#   name<<END if-absent=other",
    "#                       and only where the if-absent entry `other` is not the",
    "#                       machine's own: files that belong together fill together",
    "#",
    "# A name is a file name (written to ~/.secrets/<name>) or a home path such as",
    "# ~/.aterm/release-credentials.toml (written there). Every file is written 0600.",
];

/// Split a text into lines: LF ends, a trailing CR dropped, a final LF optional.
fn split_lines(text: &str) -> Result<Vec<String>, TextError> {
    if text.len() > MAX_TEXT {
        return Err(whole(format!("the text is larger than {MAX_TEXT} bytes")));
    }
    let body = text.strip_suffix('\n').unwrap_or(text);
    if text.is_empty() {
        return Ok(Vec::new());
    }
    Ok(body
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_string())
        .collect())
}

/// Lines joined by LF with a final LF (nothing for no lines).
fn render_lines<'a>(lines: impl Iterator<Item = &'a str>) -> SecretBytes {
    let mut out = Vec::new();
    for line in lines {
        out.extend_from_slice(line.as_bytes());
        out.push(b'\n');
    }
    SecretBytes::new(out)
}

fn wipe_lines(lines: Vec<String>) {
    for mut line in lines {
        line.zeroize();
    }
}

/// The characters a line may be indented or padded with.
const BLANK: [char; 2] = [' ', '\t'];

/// Parse and validate lines under `mode`.
fn parse_lines(lines: &[String], mode: Mode) -> Result<Model, TextError> {
    let total: usize = lines.iter().map(|line| line.len() + 1).sum();
    if total > MAX_TEXT {
        return Err(whole(format!("the text is larger than {MAX_TEXT} bytes")));
    }
    for (i, line) in lines.iter().enumerate() {
        if line.contains('\r') {
            return Err(at(
                i + 1,
                "a carriage return inside a line (only a line-ending CR is dropped)",
            ));
        }
        if line.contains('\0') {
            return Err(at(i + 1, "a NUL byte"));
        }
    }
    let mut model = Model::default();
    let mut current: Option<usize> = None;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].as_str();
        let n = i + 1;
        let trimmed = line.trim_matches(BLANK);
        if trimmed.is_empty() || trimmed.starts_with('#') {
            i += 1;
            continue;
        }
        if trimmed.starts_with('[') {
            let kind = parse_header(trimmed, n, mode)?;
            model.sections.push(Section {
                kind,
                header: i,
                last_item: i,
            });
            current = Some(model.sections.len() - 1);
            i += 1;
            continue;
        }
        let s = match current {
            Some(s) => s,
            None if mode == Mode::Plain => {
                model.sections.push(Section {
                    kind: SectionKind::Audience(Audience::Everyone),
                    header: i,
                    last_item: i,
                });
                current = Some(model.sections.len() - 1);
                model.sections.len() - 1
            }
            None => return Err(at(n, "an item before the first [section] header")),
        };
        let last = match model.sections[s].kind.clone() {
            SectionKind::Machines => {
                let machine = parse_machine(trimmed, n)?;
                for (other, j) in &model.machines {
                    if other.name == machine.name {
                        return Err(at(
                            n,
                            format!(
                                "machine `{}` is declared twice (lines {} and {n})",
                                machine.name,
                                j + 1
                            ),
                        ));
                    }
                    if other.recipient == machine.recipient {
                        return Err(at(
                            n,
                            format!("lines {} and {n} declare the same recipient", j + 1),
                        ));
                    }
                }
                model.machines.push((machine, i));
                i
            }
            SectionKind::Admins => {
                if !valid_machine_name(trimmed) {
                    return Err(at(n, "expected one machine name per line in [admins]"));
                }
                if let Some((_, j)) = model.admins.iter().find(|(admin, _)| admin == trimmed) {
                    return Err(at(
                        n,
                        format!(
                            "admin `{trimmed}` is listed twice (lines {} and {n})",
                            j + 1
                        ),
                    ));
                }
                model.admins.push((trimmed.to_string(), i));
                i
            }
            SectionKind::Join => {
                // Read in a plain file too (a locker refuses the header), so an entry
                // written under [join] by mistake is an error, never silently dropped.
                let line = parse_join(trimmed, n)?;
                let password = model.join.as_ref().map(|(_, j)| *j);
                let (what, same, other) = match &line {
                    JoinLine::Password(_) => ("one password", password, model.open),
                    JoinLine::Open => ("one `open` line", model.open, password),
                    JoinLine::CodeOnly(key) => (
                        "one `code-only` line for a key",
                        model
                            .code_only
                            .iter()
                            .find(|(held, _)| held == key)
                            .map(|(_, j)| *j),
                        None,
                    ),
                };
                if let Some(j) = same {
                    return Err(at(
                        n,
                        format!("[join] holds {what} (lines {} and {n})", j + 1),
                    ));
                }
                if let Some(j) = other {
                    return Err(at(
                        n,
                        format!(
                            "[join] holds a password or an `open` line, not both (lines {} and {n})",
                            j + 1
                        ),
                    ));
                }
                match line {
                    JoinLine::Password(password) => model.join = Some((password, i)),
                    JoinLine::Open => model.open = Some(i),
                    JoinLine::CodeOnly(key) => model.code_only.push((key, i)),
                }
                i
            }
            SectionKind::Audience(audience) => {
                let (entry, last) = parse_entry(lines, i, audience)?;
                model.entries.push(Located {
                    entry,
                    first: i,
                    last,
                });
                last
            }
        };
        model.sections[s].last_item = last;
        i = last + 1;
    }
    check_duplicates(&model)?;
    // A locker is one audience's share of what a host receives, so an anchor in it may
    // name an entry of another locker: the whole text and a plain file are checked.
    if mode != Mode::Locker {
        check_anchors(&model)?;
    }
    if mode == Mode::Vault {
        check_vault(&model)?;
    }
    Ok(model)
}

/// A section header (`trimmed` starts with `[`).
fn parse_header(trimmed: &str, n: usize, mode: Mode) -> Result<SectionKind, TextError> {
    let Some(inner) = trimmed
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    else {
        return Err(at(
            n,
            "a section header is [name] or [host host …], alone on its line",
        ));
    };
    let words: Vec<&str> = inner.split(BLANK).filter(|word| !word.is_empty()).collect();
    match words.as_slice() {
        [] => return Err(at(n, "an empty section header")),
        ["machines"] | ["admins"] | ["join"] if mode == Mode::Locker => {
            return Err(at(
                n,
                "a locker holds no [machines], [admins] or [join] section",
            ));
        }
        ["machines"] => return Ok(SectionKind::Machines),
        ["admins"] => return Ok(SectionKind::Admins),
        ["join"] => return Ok(SectionKind::Join),
        ["everyone"] => return Ok(SectionKind::Audience(Audience::Everyone)),
        _ => {}
    }
    let mut hosts = BTreeSet::new();
    for word in words {
        if RESERVED.contains(&word) {
            return Err(at(
                n,
                format!("`{word}` is a section of its own, not a host"),
            ));
        }
        if !valid_machine_name(word) {
            return Err(at(
                n,
                "an invalid host name in a section header: [a-z0-9][a-z0-9-]{0,62}",
            ));
        }
        hosts.insert(word.to_string());
    }
    Ok(SectionKind::Audience(Audience::Hosts(
        hosts.into_iter().collect(),
    )))
}

/// A `[machines]` line: `name = age1…` (spaces around `=` optional).
fn parse_machine(trimmed: &str, n: usize) -> Result<Machine, TextError> {
    let Some((name, recipient)) = trimmed.split_once('=') else {
        return Err(at(n, "expected `name = age1…` in [machines]"));
    };
    let (name, recipient) = (name.trim_matches(BLANK), recipient.trim_matches(BLANK));
    if !valid_machine_name(name) {
        return Err(at(n, INVALID_MACHINE));
    }
    if !valid_recipient(recipient) {
        return Err(at(n, INVALID_RECIPIENT));
    }
    Ok(Machine {
        name: name.to_string(),
        recipient: recipient.to_string(),
    })
}

/// What a `[join]` line says.
enum JoinLine {
    Password(SecretBytes),
    Open,
    CodeOnly(String),
}

/// A `[join]` line (spaces around `=` optional): `password = …`, a password
/// [`crate::join::strength`] accepts, held without the ASCII blanks around it as `strength`
/// and [`crate::join::Key::derive`] read it; `open = yes`; or `code-only = age1…`, a key an
/// open vault leaves to approval by code.
fn parse_join(trimmed: &str, n: usize) -> Result<JoinLine, TextError> {
    let shape = "expected `password = …`, `open = yes` or `code-only = age1…` in [join]";
    let Some((key, value)) = trimmed.split_once('=') else {
        return Err(at(n, shape));
    };
    match key.trim_matches(BLANK) {
        "password" => {
            let value = value.trim_ascii();
            crate::join::strength(value.as_bytes()).map_err(|why| at(n, why))?;
            Ok(JoinLine::Password(SecretBytes::from(value.as_bytes())))
        }
        "open" if value.trim_matches(BLANK) == "yes" => Ok(JoinLine::Open),
        "code-only" => {
            let value = value.trim_matches(BLANK);
            if !valid_recipient(value) {
                return Err(at(n, INVALID_RECIPIENT));
            }
            Ok(JoinLine::CodeOnly(value.to_string()))
        }
        _ => Err(at(n, shape)),
    }
}

/// The entry starting at line index `i`, in a section for `audience`, and its last line
/// index.
fn parse_entry(
    lines: &[String],
    i: usize,
    audience: Audience,
) -> Result<(Entry, usize), TextError> {
    let line = lines[i].as_str();
    let n = i + 1;
    let shape = "expected name=value, name<<TAG or name<<TAG base64";
    let Some(cut) = line.find(['=', '<']) else {
        return Err(at(n, shape));
    };
    let name = &line[..cut];
    if !valid_entry_name(name) {
        return Err(at(
            n,
            "an invalid entry name: a file name [A-Za-z0-9][A-Za-z0-9._-]{0,127} or a ~/ home path, with nothing before it",
        ));
    }
    let rest = &line[cut..];
    if let Some(value) = rest.strip_prefix('=') {
        if value.len() > MAX_VALUE {
            return Err(at(n, format!("the value is larger than {MAX_VALUE} bytes")));
        }
        let entry = Entry {
            name: name.to_string(),
            value: SecretBytes::from(value.as_bytes()),
            audience,
            if_absent: false,
            anchor: None,
        };
        return Ok((entry, i));
    }
    let Some(spec) = rest.strip_prefix("<<") else {
        return Err(at(n, shape));
    };
    let spec = spec.trim_end_matches(BLANK);
    let words: Vec<&str> = spec.split(' ').collect();
    let opens = "a heredoc opens with name<<TAG, then optionally ` base64`, then optionally ` if-absent` or ` if-absent=<anchor>`";
    let (tag, base64, fill) = match words.as_slice() {
        [tag] => (*tag, false, None),
        [tag, "base64"] => (*tag, true, None),
        [tag, fill] => (*tag, false, Some(*fill)),
        [tag, "base64", fill] => (*tag, true, Some(*fill)),
        _ => return Err(at(n, opens)),
    };
    let (if_absent, anchor) = match fill.map(|word| (word, word.strip_prefix("if-absent="))) {
        None => (false, None),
        Some(("if-absent", _)) => (true, None),
        Some((_, Some(anchor))) if valid_entry_name(anchor) && anchor != name => {
            (true, Some(anchor.to_string()))
        }
        Some((_, Some(_))) => return Err(at(n, ANCHOR_NAME)),
        Some((_, None)) => return Err(at(n, opens)),
    };
    if !valid_tag(tag) {
        return Err(at(n, "a heredoc TAG is [A-Z][A-Z0-9_]{0,31}"));
    }
    let Some(end) = (i + 1..lines.len()).find(|&j| lines[j] == tag) else {
        return Err(at(
            n,
            format!("the heredoc never ends: no line `{tag}` follows"),
        ));
    };
    let body = &lines[i + 1..end];
    let size: usize = body
        .iter()
        .map(|line| line.len() + usize::from(!base64))
        .sum();
    if size > MAX_VALUE * 4 / 3 + 4 || (!base64 && size > MAX_VALUE) {
        return Err(at(n, format!("the value is larger than {MAX_VALUE} bytes")));
    }
    let value = if base64 {
        let mut joined = String::with_capacity(size);
        for line in body {
            joined.push_str(line);
        }
        let decoded = crate::b64::decode_padded(&joined);
        joined.zeroize();
        match decoded {
            Some(bytes) if bytes.len() <= MAX_VALUE => SecretBytes::new(bytes),
            Some(mut bytes) => {
                bytes.zeroize();
                return Err(at(n, format!("the value is larger than {MAX_VALUE} bytes")));
            }
            None => return Err(at(n, "the base64 heredoc is not canonical padded base64")),
        }
    } else {
        let mut value = Vec::with_capacity(size);
        for line in body {
            value.extend_from_slice(line.as_bytes());
            value.push(b'\n');
        }
        SecretBytes::new(value)
    };
    let entry = Entry {
        name: name.to_string(),
        value,
        audience,
        if_absent,
        anchor,
    };
    Ok((entry, end))
}

/// No entry name twice in one audience, and none in two host audiences that share a host.
fn check_duplicates(model: &Model) -> Result<(), TextError> {
    let mut seen: BTreeMap<(&Audience, &str), usize> = BTreeMap::new();
    for located in &model.entries {
        let entry = &located.entry;
        if let Some(prev) = seen.insert((&entry.audience, &entry.name), located.first) {
            return Err(at(
                located.first + 1,
                format!(
                    "`{}` appears twice in {} (lines {} and {})",
                    entry.name,
                    entry.audience.header(),
                    prev + 1,
                    located.first + 1
                ),
            ));
        }
    }
    let mut per_host: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for located in &model.entries {
        let Audience::Hosts(hosts) = &located.entry.audience else {
            continue;
        };
        for host in hosts {
            if let Some(prev) = per_host.insert((&located.entry.name, host), located.first) {
                return Err(at(
                    located.first + 1,
                    format!(
                        "`{}` is set for host `{host}` by both line {} and line {}",
                        located.entry.name,
                        prev + 1,
                        located.first + 1
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// Every anchor names another ` if-absent` entry of what each host receiving its entry
/// receives: a host a section names gets `[everyone]`'s entries overridden by name by its
/// sections', and any other host (one that joins later, too) `[everyone]`'s alone. The
/// error names the anchored entry's line.
fn check_anchors(model: &Model) -> Result<(), TextError> {
    if model
        .entries
        .iter()
        .all(|located| located.entry.anchor.is_none())
    {
        return Ok(());
    }
    let mut named: BTreeSet<&str> = BTreeSet::new();
    for section in &model.sections {
        if let SectionKind::Audience(Audience::Hosts(hosts)) = &section.kind {
            named.extend(hosts.iter().map(String::as_str));
        }
    }
    // `None` is every host no section names; first, so an `[everyone]` entry whose anchor
    // only some hosts get is refused for the hosts that do not.
    for host in std::iter::once(None).chain(named.into_iter().map(Some)) {
        let mut set: BTreeMap<&str, &Located> = BTreeMap::new();
        for located in &model.entries {
            if located.entry.audience == Audience::Everyone {
                set.insert(&located.entry.name, located);
            }
        }
        for located in &model.entries {
            if let (Audience::Hosts(_), Some(host)) = (&located.entry.audience, host)
                && located.entry.audience.includes(host)
            {
                set.insert(&located.entry.name, located);
            }
        }
        let dangling = set
            .values()
            .filter(|located| {
                !anchor_received(&located.entry, |name| {
                    set.get(name).map(|anchor| &anchor.entry)
                })
            })
            .min_by_key(|located| located.first);
        if let Some(located) = dangling {
            let whom = match host {
                Some(host) => format!("host `{host}` receives"),
                None => "in [everyone], all a host no section names receives".to_string(),
            };
            return Err(at(
                located.first + 1,
                format!(
                    "`{}` is anchored to `{}`, which is not an if-absent entry {whom}",
                    located.entry.name,
                    located.entry.anchor.as_deref().unwrap_or_default()
                ),
            ));
        }
    }
    Ok(())
}

/// Whether `entry`'s anchor, if it has one, is another ` if-absent` entry of what the
/// machine receiving `entry` receives (`find` looks one up there by name).
pub(crate) fn anchor_received<'a>(
    entry: &Entry,
    find: impl FnOnce(&str) -> Option<&'a Entry>,
) -> bool {
    entry.anchor.as_deref().is_none_or(|anchor| {
        anchor != entry.name && find(anchor).is_some_and(|anchor| anchor.if_absent)
    })
}

/// Every host and admin a machine; at least one admin.
fn check_vault(model: &Model) -> Result<(), TextError> {
    let machines: BTreeSet<&str> = model
        .machines
        .iter()
        .map(|(machine, _)| machine.name.as_str())
        .collect();
    for section in &model.sections {
        if let SectionKind::Audience(Audience::Hosts(hosts)) = &section.kind
            && let Some(host) = hosts.iter().find(|host| !machines.contains(host.as_str()))
        {
            return Err(at(
                section.header + 1,
                format!("host `{host}` is not in [machines]"),
            ));
        }
    }
    for (admin, i) in &model.admins {
        if !machines.contains(admin.as_str()) {
            return Err(at(i + 1, format!("admin `{admin}` is not in [machines]")));
        }
    }
    if model.admins.is_empty() {
        return Err(whole("no admin: [admins] must name at least one machine"));
    }
    let mut audiences: BTreeSet<&Audience> = BTreeSet::new();
    for section in &model.sections {
        if let SectionKind::Audience(audience @ Audience::Hosts(_)) = &section.kind
            && audiences.insert(audience)
            && audiences.len() > MAX_HOST_AUDIENCES
        {
            return Err(at(
                section.header + 1,
                format!(
                    "more than {MAX_HOST_AUDIENCES} distinct sets of hosts: each is sealed in a locker of its own, and a version holds {} with the admins' and [everyone]'s",
                    crate::envelope::MAX_LOCKERS
                ),
            ));
        }
    }
    Ok(())
}

/// An audience as the parser makes them: hosts valid, sorted and deduplicated.
fn normalize(audience: &Audience) -> Result<Audience, TextError> {
    match audience {
        Audience::Everyone => Ok(Audience::Everyone),
        Audience::Hosts(hosts) => {
            if hosts.is_empty() {
                return Err(whole("an audience names at least one host"));
            }
            if !hosts.iter().all(|host| valid_machine_name(host)) {
                return Err(whole("an invalid host name in an audience"));
            }
            let sorted: BTreeSet<&String> = hosts.iter().collect();
            Ok(Audience::Hosts(sorted.into_iter().cloned().collect()))
        }
    }
}

/// The lines that spell entry `name` with `value` (see [`Document::set_entry_with`]).
fn spell(name: &str, value: &[u8], if_absent: bool, anchor: Option<&str>) -> Vec<String> {
    let suffix = match (if_absent, anchor) {
        (false, _) => String::new(),
        (true, None) => " if-absent".to_string(),
        (true, Some(anchor)) => format!(" if-absent={anchor}"),
    };
    match std::str::from_utf8(value) {
        Ok(text) if !if_absent && !text.contains(['\r', '\n', '\0']) => {
            vec![format!("{name}={text}")]
        }
        Ok(text) if text.ends_with('\n') && !text.contains(['\r', '\0']) => {
            let body: Vec<&str> = text[..text.len() - 1].split('\n').collect();
            let tag = pick_tag(&body);
            let mut lines = Vec::with_capacity(body.len() + 2);
            lines.push(format!("{name}<<{tag}{suffix}"));
            lines.extend(body.iter().map(|line| (*line).to_string()));
            lines.push(tag);
            lines
        }
        _ => {
            let mut encoded = crate::b64::encode_padded(value);
            let body: Vec<&str> = encoded
                .as_bytes()
                .chunks(64)
                .map(|chunk| std::str::from_utf8(chunk).unwrap_or_default())
                .collect();
            let tag = pick_tag(&body);
            let mut lines = Vec::with_capacity(body.len() + 2);
            lines.push(format!("{name}<<{tag} base64{suffix}"));
            lines.extend(body.iter().map(|line| (*line).to_string()));
            lines.push(tag);
            encoded.zeroize();
            lines
        }
    }
}

/// `END`, or the first of `END1`, `END2`, … that no body line equals.
fn pick_tag(body: &[&str]) -> String {
    let mut tag = "END".to_string();
    let mut k = 0u32;
    while body.iter().any(|line| *line == tag) {
        k += 1;
        tag = format!("END{k}");
    }
    tag
}

/// A heredoc TAG: `[A-Z][A-Z0-9_]{0,31}`.
fn valid_tag(tag: &str) -> bool {
    let bytes = tag.as_bytes();
    (1..=32).contains(&bytes.len())
        && bytes[0].is_ascii_uppercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || *b == b'_')
}

/// A valid `age1…` X25519 recipient, checksum and point included.
fn valid_recipient(text: &str) -> bool {
    crate::age::Recipient::parse(text).is_ok()
}

/// Read one locker's plaintext (sections and entries only; `[machines]`, `[admins]` and
/// `[join]` are refused, and hosts are not checked against a machine list). Each entry
/// keeps its ` if-absent` mark.
pub fn parse_locker(text: &[u8]) -> Result<Vec<Entry>, TextError> {
    let text = match std::str::from_utf8(text) {
        Ok(text) => text,
        Err(error) => {
            let line = text[..error.valid_up_to()]
                .iter()
                .filter(|b| **b == b'\n')
                .count()
                + 1;
            return Err(at(line, "not UTF-8"));
        }
    };
    let lines = split_lines(text)?;
    let parsed = parse_lines(&lines, Mode::Locker);
    wipe_lines(lines);
    Ok(parsed?
        .entries
        .into_iter()
        .map(|located| located.entry)
        .collect())
}

/// Read a plain text file a person keeps themselves (§7): the §4 syntax, with items before
/// the first header in `[everyone]`, and hosts not checked against `[machines]`. Returns
/// every entry; [`merge_for`] picks a host's.
pub fn parse_plain(text: &[u8]) -> Result<Vec<Entry>, TextError> {
    let text = match std::str::from_utf8(text) {
        Ok(text) => text,
        Err(error) => {
            let line = text[..error.valid_up_to()]
                .iter()
                .filter(|b| **b == b'\n')
                .count()
                + 1;
            return Err(at(line, "not UTF-8"));
        }
    };
    let lines = split_lines(text)?;
    let parsed = parse_lines(&lines, Mode::Plain);
    wipe_lines(lines);
    Ok(parsed?
        .entries
        .into_iter()
        .map(|located| located.entry)
        .collect())
}

/// Merge entries for `host` from several lockers: host-audience entries override
/// `Everyone` entries of the same name; two host-audience entries of the same name that
/// both include `host` are an error (naming the entry, not the value), as are two
/// `Everyone` entries of the same name and an anchor that is not another ` if-absent`
/// entry of the merged set. Entries whose audience does not include `host` are left out.
/// Sorted by name.
pub fn merge_for(host: &str, entries: Vec<Entry>) -> Result<Vec<Entry>, TextError> {
    let mut everyone: BTreeMap<String, Entry> = BTreeMap::new();
    let mut hosts: BTreeMap<String, Entry> = BTreeMap::new();
    for entry in entries {
        if !entry.audience.includes(host) {
            continue;
        }
        let map = match entry.audience {
            Audience::Everyone => &mut everyone,
            Audience::Hosts(_) => &mut hosts,
        };
        match map.get(&entry.name) {
            None => {
                map.insert(entry.name.clone(), entry);
            }
            Some(first) => {
                return Err(whole(format!(
                    "`{}` is set twice for host `{host}` (by {} and by {})",
                    entry.name,
                    first.audience.header(),
                    entry.audience.header()
                )));
            }
        }
    }
    everyone.extend(hosts);
    if let Some(entry) = everyone
        .values()
        .find(|entry| !anchor_received(entry, |name| everyone.get(name)))
    {
        return Err(whole(format!(
            "`{}` is anchored to `{}`, which is not an if-absent entry host `{host}` receives",
            entry.name,
            entry.anchor.as_deref().unwrap_or_default()
        )));
    }
    Ok(everyone.into_values().collect())
}

/// True when `c` may be in a file name.
fn name_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-')
}

/// A file name: `[A-Za-z0-9][A-Za-z0-9._-]{0,127}`.
fn valid_file_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=128).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes.iter().all(|b| name_char(*b))
}

/// A home path's component: up to 128 of `[A-Za-z0-9._-]`, starting with a letter, a digit
/// or a dot (`.aterm`), and not `.` or `..`.
fn valid_home_component(component: &str) -> bool {
    let bytes = component.as_bytes();
    (1..=128).contains(&bytes.len())
        && (bytes[0].is_ascii_alphanumeric() || bytes[0] == b'.')
        && bytes.iter().all(|b| name_char(*b))
        && component != "."
        && component != ".."
}

/// The longest home path name.
const MAX_HOME_PATH: usize = 1024;

/// True when `name` is a valid entry name (file name or `~/` home path).
///
/// A home path may not reach into `~/.secrets` (spell such an entry by its file name) or
/// `~/.config/aterm/secrets` (the vault's own key and state); both are compared without
/// case, as macOS's default volume does.
pub fn valid_entry_name(name: &str) -> bool {
    let Some(rel) = name.strip_prefix("~/") else {
        return valid_file_name(name);
    };
    if name.len() > MAX_HOME_PATH || !rel.split('/').all(valid_home_component) {
        return false;
    }
    let folded = rel.to_ascii_lowercase();
    let components: Vec<&str> = folded.split('/').collect();
    !(components.starts_with(&[".secrets"])
        || components.starts_with(&[".config", "aterm", "secrets"]))
}

/// True when `name` is a valid machine name.
pub fn valid_machine_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=63).contains(&bytes.len())
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
        && !RESERVED.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHARSET: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";

    fn polymod(values: &[u8]) -> u32 {
        const GEN: [u32; 5] = [
            0x3b6a_57b2,
            0x2650_8e6d,
            0x1ea1_19fa,
            0x3d42_33dd,
            0x2a14_62b3,
        ];
        let mut check: u32 = 1;
        for value in values {
            let top = check >> 25;
            check = ((check & 0x01ff_ffff) << 5) ^ u32::from(*value);
            for (i, g) in GEN.iter().enumerate() {
                if (top >> i) & 1 == 1 {
                    check ^= g;
                }
            }
        }
        check
    }

    /// BIP 173 Bech32 of `data` under `hrp` (a real checksum, so these recipients also pass
    /// the full age decoder once it replaces the structural check).
    fn bech32(hrp: &str, data: &[u8]) -> String {
        let mut five = Vec::new();
        let (mut acc, mut bits) = (0u32, 0u32);
        for byte in data {
            acc = ((acc << 8) | u32::from(*byte)) & 0xffff;
            bits += 8;
            while bits >= 5 {
                bits -= 5;
                five.push(((acc >> bits) & 31) as u8);
            }
        }
        if bits > 0 {
            five.push(((acc << (5 - bits)) & 31) as u8);
        }
        let mut values: Vec<u8> = hrp.bytes().map(|c| c >> 5).collect();
        values.push(0);
        values.extend(hrp.bytes().map(|c| c & 31));
        values.extend(&five);
        values.extend([0u8; 6]);
        let check = polymod(&values) ^ 1;
        let sum = (0..6).map(|i| ((check >> (5 * (5 - i))) & 31) as u8);
        let tail: String = five
            .iter()
            .copied()
            .chain(sum)
            .map(|v| CHARSET[usize::from(v)] as char)
            .collect();
        format!("{hrp}1{tail}")
    }

    /// A synthetic recipient from a fixed seed.
    fn recipient(seed: u8) -> String {
        let key: [u8; 32] = std::array::from_fn(|i| {
            seed.wrapping_mul(29)
                .wrapping_add((i as u8).wrapping_mul(7))
        });
        bech32("age", &key)
    }

    fn vault() -> String {
        format!(
            "# comment\n\
             \n\
             [machines]\n\
             m22 = {}\n\
             m3={}\n\
             \x20 m9   =   {}\n\
             \n\
             [admins]\n\
             m22\n\
             \n\
             [everyone]\n\
             gh_token=a value with spaces # and = signs  \n\
             notes.txt<<END\n\
             # not a comment\n\
             [not a header]\n\
             \n\
             END\n\
             \n\
             [m22]\n\
             ~/.aterm/release.toml<<END\n\
             key = 1\n\
             END\n\
             gh_token=m22 override\n\
             blob<<END base64\n\
             AAEC\n\
             /w==\n\
             END\n\
             \n\
             [m3 m22]\n\
             shared=both\n",
            recipient(1),
            recipient(2),
            recipient(3)
        )
    }

    fn doc() -> Document {
        Document::parse(&vault()).expect("valid vault")
    }

    fn text(document: &Document) -> String {
        String::from_utf8(document.render().as_slice().to_vec()).expect("utf-8")
    }

    fn hosts(names: &[&str]) -> Audience {
        Audience::Hosts(names.iter().map(|h| (*h).to_string()).collect())
    }

    fn value(entries: &[Entry], name: &str) -> Vec<u8> {
        entries
            .iter()
            .find(|e| e.name == name)
            .expect(name)
            .value
            .as_slice()
            .to_vec()
    }

    fn error(text: &str) -> TextError {
        Document::parse(text).expect_err("refused")
    }

    /// A minimal valid vault with `extra` appended.
    fn base(extra: &str) -> String {
        format!(
            "[machines]\nm22 = {}\nm3 = {}\n[admins]\nm22\n{extra}",
            recipient(1),
            recipient(2)
        )
    }

    #[test]
    fn an_unedited_document_renders_byte_for_byte() {
        let source = vault();
        assert_eq!(text(&doc()), source);
        let crlf = source.replace('\n', "\r\n");
        assert_eq!(text(&Document::parse(&crlf).expect("crlf")), source);
        let unterminated = source.strip_suffix('\n').expect("final LF");
        assert_eq!(
            text(&Document::parse(unterminated).expect("no final LF")),
            source
        );
    }

    #[test]
    fn machines_admins_and_entries_are_read() {
        let d = doc();
        let machines = d.machines();
        assert_eq!(
            machines.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            ["m22", "m3", "m9"]
        );
        assert_eq!(machines[2].recipient, recipient(3));
        assert_eq!(d.admins(), ["m22"]);
        let entries = d.entries();
        let names: Vec<(&str, &Audience)> = entries
            .iter()
            .map(|e| (e.name.as_str(), &e.audience))
            .collect();
        assert_eq!(
            names,
            [
                ("gh_token", &Audience::Everyone),
                ("notes.txt", &Audience::Everyone),
                ("~/.aterm/release.toml", &hosts(&["m22"])),
                ("gh_token", &hosts(&["m22"])),
                ("blob", &hosts(&["m22"])),
                ("shared", &hosts(&["m22", "m3"])),
            ]
        );
        assert_eq!(
            entries[0].value.as_slice(),
            b"a value with spaces # and = signs  "
        );
        assert_eq!(
            entries[1].value.as_slice(),
            b"# not a comment\n[not a header]\n\n"
        );
        assert_eq!(entries[2].value.as_slice(), b"key = 1\n");
        assert_eq!(entries[4].value.as_slice(), [0u8, 1, 2, 0xff]);
    }

    #[test]
    fn host_headers_are_sorted_and_deduplicated() {
        let d = Document::parse(&base("[m3 m22 m3]\na=1\n")).expect("valid");
        assert_eq!(d.entries()[0].audience, hosts(&["m22", "m3"]));
        assert_eq!(hosts(&["m22", "m3"]).header(), "[m22 m3]");
        assert_eq!(Audience::Everyone.header(), "[everyone]");
        assert!(Audience::Everyone.includes("anything"));
        assert!(hosts(&["m22", "m3"]).includes("m3"));
        assert!(!hosts(&["m22"]).includes("m3"));
    }

    #[test]
    fn heredocs_empty_and_one_empty_line() {
        let d = Document::parse(&base(
            "[everyone]\nnone<<END\nEND\nblank<<END\n\nEND\nb64<<END base64\nEND\nempty=\n",
        ))
        .expect("valid");
        let entries = d.entries();
        assert_eq!(value(&entries, "none"), b"");
        assert_eq!(value(&entries, "blank"), b"\n");
        assert_eq!(value(&entries, "b64"), b"");
        assert_eq!(value(&entries, "empty"), b"");
    }

    #[test]
    fn entries_for_a_host_override_everyone_by_name() {
        let d = doc();
        let m22 = merge_for("m22", d.entries()).expect("merges");
        assert_eq!(
            m22.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            [
                "blob",
                "gh_token",
                "notes.txt",
                "shared",
                "~/.aterm/release.toml"
            ]
        );
        assert_eq!(value(&m22, "gh_token"), b"m22 override");
        let m3 = merge_for("m3", d.entries()).expect("merges");
        assert_eq!(
            m3.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            ["gh_token", "notes.txt", "shared"]
        );
        assert_eq!(
            value(&m3, "gh_token"),
            b"a value with spaces # and = signs  "
        );
        let m9 = merge_for("m9", d.entries()).expect("merges");
        assert_eq!(
            m9.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            ["gh_token", "notes.txt"]
        );
    }

    #[test]
    fn lockers_are_everyone_first_then_sorted_audiences_and_read_back() {
        let d = doc();
        let plans = d.lockers();
        let audiences: Vec<&Audience> = plans.iter().map(|p| &p.audience).collect();
        assert_eq!(
            audiences,
            [
                &Audience::Everyone,
                &hosts(&["m22"]),
                &hosts(&["m22", "m3"])
            ]
        );
        let mut all = vec![recipient(1), recipient(2), recipient(3)];
        all.sort();
        assert_eq!(plans[0].recipients, all);
        assert_eq!(plans[1].recipients, [recipient(1)]);
        let mut two = vec![recipient(1), recipient(2)];
        two.sort();
        assert_eq!(plans[2].recipients, two);
        let entries = d.entries();
        for plan in &plans {
            let back = parse_locker(plan.text.as_slice()).expect("locker parses");
            let want: Vec<&Entry> = entries
                .iter()
                .filter(|e| e.audience == plan.audience)
                .collect();
            assert_eq!(
                back.iter().collect::<Vec<_>>(),
                want,
                "{}",
                plan.audience.header()
            );
            assert!(
                plan.text
                    .as_slice()
                    .starts_with(plan.audience.header().as_bytes())
            );
        }
        let merged: Vec<Entry> = plans
            .iter()
            .filter(|p| p.audience.includes("m22"))
            .flat_map(|p| parse_locker(p.text.as_slice()).expect("parses"))
            .collect();
        assert_eq!(
            merge_for("m22", merged).expect("merges"),
            merge_for("m22", d.entries()).expect("merges")
        );
    }

    #[test]
    fn the_everyone_locker_exists_without_entries_and_sections_accumulate() {
        let d = Document::parse(&base("[m3]\na=1\n[m22]\nb=2\n[m3]\nc=3\n")).expect("valid");
        let plans = d.lockers();
        assert_eq!(plans[0].audience, Audience::Everyone);
        assert_eq!(plans[0].text.as_slice(), b"[everyone]\n");
        assert_eq!(plans.len(), 3);
        assert_eq!(plans[2].audience, hosts(&["m3"]));
        assert_eq!(plans[2].text.as_slice(), b"[m3]\na=1\nc=3\n");
    }

    #[test]
    fn a_locker_refuses_machines_and_admins_and_does_not_check_hosts() {
        let entries = parse_locker(b"[nobody-known]\na=1\n").expect("hosts unchecked");
        assert_eq!(entries[0].audience, hosts(&["nobody-known"]));
        assert_eq!(parse_locker(b"[machines]\n").expect_err("refused").line, 1);
        assert_eq!(
            parse_locker(b"[everyone]\n[admins]\n")
                .expect_err("refused")
                .line,
            2
        );
        assert!(parse_locker(b"").expect("empty").is_empty());
        let bad = parse_locker(b"[everyone]\na=1\nb=\xff\n").expect_err("not utf-8");
        assert_eq!(bad.line, 3);
    }

    #[test]
    fn merge_for_reports_a_clash_between_two_host_audiences() {
        let entry = |name: &str, v: &str, audience: Audience| Entry {
            name: name.to_string(),
            value: SecretBytes::from(v.as_bytes()),
            audience,
            if_absent: false,
            anchor: None,
        };
        let clash = merge_for(
            "m3",
            vec![
                entry("a", "x", hosts(&["m3"])),
                entry("a", "y", hosts(&["m22", "m3"])),
            ],
        )
        .expect_err("clash");
        assert!(clash.why.contains("`a`"), "{clash}");
        let merged = merge_for(
            "m22",
            vec![
                entry("a", "x", hosts(&["m3"])),
                entry("a", "y", hosts(&["m22", "m3"])),
                entry("a", "z", Audience::Everyone),
                entry("b", "w", Audience::Everyone),
            ],
        )
        .expect("only one includes m22");
        assert_eq!(merged.len(), 2);
        assert_eq!(value(&merged, "a"), b"y");
        assert!(
            merge_for(
                "m22",
                vec![
                    entry("a", "1", Audience::Everyone),
                    entry("a", "2", Audience::Everyone)
                ]
            )
            .is_err()
        );
    }

    #[test]
    fn structural_errors_name_their_line() {
        let cases: Vec<(String, usize, &str)> = vec![
            ("a=1\n[machines]\n".to_string(), 1, "before the first"),
            (
                base("[foo bar baz]\n"),
                6,
                "host `bar` is not in [machines]",
            ),
            (base("[Bad]\n"), 6, "invalid host name"),
            (base("[]\n"), 6, "empty section"),
            (base("[m3 everyone]\n"), 6, "section of its own"),
            (base("[m3\n"), 6, "section header"),
            (base("[everyone] # x\n"), 6, "section header"),
            (base("[m99]\na=1\n"), 6, "host `m99` is not in [machines]"),
            (
                base("[everyone]\na=1\na=2\n"),
                8,
                "`a` appears twice in [everyone] (lines 7 and 8)",
            ),
            (
                base("[m3 m22]\na=1\n[m22 m3]\na=2\n"),
                9,
                "`a` appears twice in [m22 m3]",
            ),
            (
                base("[m3]\na=1\n[m3 m22]\na=2\n"),
                9,
                "`a` is set for host `m3` by both line 7 and line 9",
            ),
            (
                base("[everyone]\nno equals sign\n"),
                7,
                "expected name=value",
            ),
            (base("[everyone]\n a=1\n"), 7, "invalid entry name"),
            (base("[everyone]\na =1\n"), 7, "invalid entry name"),
            (base("[everyone]\na<b\n"), 7, "expected name=value"),
            (base("[everyone]\na<<end\nend\n"), 7, "TAG"),
            (base("[everyone]\na<<END text\nEND\n"), 7, "heredoc opens"),
            (base("[everyone]\nb=1\na<<END\nx\n"), 8, "never ends"),
            (base("[everyone]\na<<END base64\nZg\nEND\n"), 7, "base64"),
            (base("[everyone]\na<<END base64\nZh==\nEND\n"), 7, "base64"),
            (base("[everyone]\na=x\ry\n"), 7, "carriage return"),
            (base("[everyone]\na=x\0\n"), 7, "NUL"),
            (base("[machines]\nm4\n"), 7, "expected `name = age1…`"),
            (base("[machines]\nEvery = x\n"), 7, "invalid machine name"),
            (
                base("[machines]\neveryone = x\n"),
                7,
                "invalid machine name",
            ),
            (base("[admins]\nm3 m22\n"), 7, "one machine name per line"),
            (
                base("[admins]\nm22\n"),
                7,
                "admin `m22` is listed twice (lines 5 and 7)",
            ),
            (
                base("[admins]\nm77\n"),
                7,
                "admin `m77` is not in [machines]",
            ),
            (base("[machines]\nm22 = x\n"), 7, "invalid recipient"),
        ];
        for (source, line, why) in cases {
            let e = error(&source);
            assert_eq!(e.line, line, "{e} for {source:?}");
            assert!(e.why.contains(why), "{e} for {source:?}");
            assert_eq!(format!("{e}"), format!("line {line}: {}", e.why));
        }
    }

    #[test]
    fn machine_lines_are_checked() {
        let dup_name = format!(
            "[machines]\nm22 = {}\nm22 = {}\n[admins]\nm22\n",
            recipient(1),
            recipient(2)
        );
        assert!(
            error(&dup_name)
                .why
                .contains("declared twice (lines 2 and 3)")
        );
        let dup_key = format!(
            "[machines]\nm22 = {}\nm3 = {}\n[admins]\nm22\n",
            recipient(1),
            recipient(1)
        );
        assert!(error(&dup_key).why.contains("same recipient"));
        let r = recipient(1);
        for bad in [
            r.to_uppercase(),
            r[..61].to_string(),
            format!("{r}q"),
            format!("{}b{}", &r[..10], &r[11..]),
            format!("age2{}", &r[4..]),
            format!("{r} # inline comment"),
        ] {
            let source = format!("[machines]\nm22 = {bad}\n[admins]\nm22\n");
            let e = error(&source);
            assert_eq!(e.line, 2, "{e}");
            assert!(e.why.contains("invalid recipient"), "{e}");
        }
    }

    #[test]
    fn a_vault_needs_an_admin_but_a_locker_does_not() {
        let e = error(&format!("[machines]\nm22 = {}\n", recipient(1)));
        assert_eq!(e.line, 0);
        assert!(e.why.contains("no admin"), "{e}");
        assert_eq!(format!("{e}"), e.why);
        assert!(parse_locker(b"[everyone]\na=1\n").is_ok());
    }

    #[test]
    fn sizes_are_bounded() {
        let big = "x".repeat(MAX_VALUE + 1);
        let e = error(&base(&format!("[everyone]\na={big}\n")));
        assert_eq!(e.line, 7);
        assert!(e.why.contains("larger"), "{e}");
        let fits = "x".repeat(MAX_VALUE);
        assert!(Document::parse(&base(&format!("[everyone]\na={fits}\n"))).is_ok());
        let heredoc = format!("[everyone]\na<<END\n{}\nEND\n", "y".repeat(MAX_VALUE));
        assert!(
            error(&base(&heredoc)).why.contains("larger"),
            "the final LF makes it one too many"
        );
        let encoded = crate::b64::encode_padded(&vec![7u8; MAX_VALUE + 1]);
        assert!(
            error(&base(&format!(
                "[everyone]\na<<END base64\n{encoded}\nEND\n"
            )))
            .why
            .contains("larger")
        );
        let encoded = crate::b64::encode_padded(&vec![7u8; MAX_VALUE]);
        let d = Document::parse(&base(&format!(
            "[everyone]\na<<END base64\n{encoded}\nEND\n"
        )))
        .expect("fits");
        assert_eq!(d.entries()[0].value.len(), MAX_VALUE);
        let huge = "#".repeat(MAX_TEXT + 1);
        assert_eq!(error(&huge).line, 0);
        assert!(error(&huge).why.contains("larger"));
    }

    #[test]
    fn a_vault_names_as_many_sets_of_hosts_as_one_version_seals() {
        let names: Vec<String> = (1..=MAX_HOST_AUDIENCES + 1)
            .map(|n| format!("m{n}"))
            .collect();
        let mut source = String::from("[machines]\n");
        for (n, name) in names.iter().enumerate() {
            source.push_str(&format!("{name} = {}\n", recipient(n as u8)));
        }
        source.push_str("[admins]\nm1\n[m1 m2]\nshared=1\n");
        for name in &names[..MAX_HOST_AUDIENCES - 1] {
            source.push_str(&format!("[{name}]\nown=1\n"));
        }
        // A section of an audience named before adds no locker.
        source.push_str("[m2 m1]\nalso=1\n");
        let mut d = Document::parse(&source).expect("62 sets of hosts");
        assert_eq!(
            d.lockers().len() + 1,
            crate::envelope::MAX_LOCKERS,
            "with the admins', a full version"
        );
        let before = text(&d);
        let last = Audience::Hosts(vec![names[MAX_HOST_AUDIENCES - 1].clone()]);
        let e = d.set_entry(&last, "own", b"1").expect_err("a 63rd");
        assert!(e.why.contains("distinct sets of hosts"), "{e}");
        assert_eq!(text(&d), before, "changes nothing");
        let e = error(&format!("{source}[m62]\nown=1\n"));
        assert_eq!(e.line, before.lines().count() + 1);
    }

    #[test]
    fn entry_and_machine_names() {
        for good in [
            "a",
            "gh_access_token",
            "accounts.txt",
            "A-1.b_c",
            "~/.aterm/release-credentials.toml",
            "~/x",
            "~/.ssh/config",
            "~/a.../b",
            "~/.config/aterm/other",
            "~/.config/aterm/secretsfoo",
        ] {
            assert!(valid_entry_name(good), "{good}");
        }
        let long = "a".repeat(129);
        let long_path = format!("~/{}", vec!["abcdefgh"; 120].join("/"));
        for bad in [
            "",
            ".hidden",
            "-dash",
            "_under",
            "a b",
            "a/b",
            "a=b",
            "~",
            "~/",
            "~/a/",
            "~//a",
            "~/./a",
            "~/../a",
            "~/a/..",
            "~/-a",
            "~/.secrets/x",
            "~/.Secrets/x",
            "~/.secrets",
            "~/.config/aterm/secrets/identity",
            "~/.CONFIG/Aterm/SECRETS/state.toml",
            long.as_str(),
            long_path.as_str(),
            "~/a\\b",
            "é",
        ] {
            assert!(!valid_entry_name(bad), "{bad:?}");
        }
        assert!(valid_entry_name(&"a".repeat(128)));
        for good in ["m22", "m3", "0", "a-b", "x".repeat(63).as_str()] {
            assert!(valid_machine_name(good), "{good}");
        }
        for bad in [
            "",
            "M22",
            "-a",
            "a_b",
            "a.b",
            "machines",
            "admins",
            "everyone",
            "x".repeat(64).as_str(),
        ] {
            assert!(!valid_machine_name(bad), "{bad}");
        }
    }

    #[test]
    fn set_entry_replaces_in_place_and_keeps_comments() {
        let mut d = doc();
        d.set_entry(&Audience::Everyone, "gh_token", b"new")
            .expect("set");
        let want = vault().replace(
            "gh_token=a value with spaces # and = signs  \n",
            "gh_token=new\n",
        );
        assert_eq!(text(&d), want);
        d.set_entry(&Audience::Everyone, "notes.txt", b"one line")
            .expect("set");
        assert!(
            text(&d).contains("gh_token=new\nnotes.txt=one line\n\n[m22]\n"),
            "{}",
            text(&d)
        );
        assert!(text(&d).starts_with("# comment\n\n[machines]\n"));
    }

    #[test]
    fn set_entry_appends_to_the_first_section_or_makes_one() {
        let mut d = Document::parse(&base(
            "[everyone]\na=1\n# trailing comment\n\n[m3]\nb=2\n[everyone]\nc=3\n",
        ))
        .expect("valid");
        d.set_entry(&Audience::Everyone, "new", b"v").expect("set");
        assert!(
            text(&d).contains("[everyone]\na=1\nnew=v\n# trailing comment\n"),
            "{}",
            text(&d)
        );
        d.set_entry(&hosts(&["m3", "m22", "m3"]), "pair", b"p")
            .expect("set");
        assert!(
            text(&d).ends_with("c=3\n\n[m22 m3]\npair=p\n"),
            "{}",
            text(&d)
        );
        let entries = merge_for("m3", d.entries()).expect("merges");
        assert_eq!(value(&entries, "pair"), b"p");
        assert_eq!(value(&entries, "new"), b"v");
    }

    #[test]
    fn set_entry_spells_each_kind_of_value_so_it_reads_back() {
        let values: Vec<Vec<u8>> = vec![
            b"".to_vec(),
            b"plain".to_vec(),
            b" spaced = # value ".to_vec(),
            b"line\n".to_vec(),
            b"\n".to_vec(),
            b"two\nlines\n".to_vec(),
            b"END\nEND1\nx\n".to_vec(),
            b"no final newline\nhere".to_vec(),
            b"crlf\r\n".to_vec(),
            b"nul\0".to_vec(),
            vec![0xff, 0xfe, 0, 1],
            (0..=255u8).cycle().take(1000).collect(),
        ];
        for (i, v) in values.iter().enumerate() {
            let mut d = Document::parse(&base("[everyone]\n")).expect("valid");
            let name = format!("v{i}");
            d.set_entry(&Audience::Everyone, &name, v).expect("set");
            let back = Document::parse(std::str::from_utf8(d.render().as_slice()).expect("utf-8"))
                .expect("re-parses");
            assert_eq!(value(&back.entries(), &name), *v, "value {i}");
            assert_eq!(value(&d.entries(), &name), *v, "value {i}");
        }
        let mut d = Document::parse(&base("[everyone]\n")).expect("valid");
        d.set_entry(&Audience::Everyone, "t", b"END\nEND1\nx\n")
            .expect("set");
        assert!(
            text(&d).contains("t<<END2\nEND\nEND1\nx\nEND2\n"),
            "{}",
            text(&d)
        );
        d.set_entry(&Audience::Everyone, "b", &[0u8; 100])
            .expect("set");
        let rendered = text(&d);
        let b64: Vec<&str> = rendered
            .split('\n')
            .skip_while(|l| *l != "b<<END base64")
            .skip(1)
            .take_while(|l| *l != "END")
            .collect();
        assert_eq!(b64.iter().map(|l| l.len()).collect::<Vec<_>>(), [64, 64, 8]);
    }

    #[test]
    fn set_entry_refuses_what_would_break_the_rules_and_changes_nothing() {
        let mut d = Document::parse(&base("[m3]\na=1\n")).expect("valid");
        let before = text(&d);
        let e = d
            .set_entry(&hosts(&["m22", "m3"]), "a", b"2")
            .expect_err("overlap");
        assert!(e.why.contains("`a`"), "{e}");
        assert!(
            d.set_entry(&hosts(&["m99"]), "b", b"1").is_err(),
            "unknown host"
        );
        assert!(d.set_entry(&hosts(&[]), "b", b"1").is_err());
        assert!(d.set_entry(&hosts(&["Bad"]), "b", b"1").is_err());
        assert!(d.set_entry(&Audience::Everyone, "bad name", b"1").is_err());
        assert!(
            d.set_entry(&Audience::Everyone, "big", &vec![b'x'; MAX_VALUE + 1])
                .is_err()
        );
        assert_eq!(text(&d), before);
        assert_eq!(d.entries().len(), 1);
    }

    #[test]
    fn unset_entry_removes_every_line_of_it() {
        let mut d = doc();
        assert!(d.unset_entry(&hosts(&["m22"]), "blob").expect("unset"));
        assert!(!text(&d).contains("AAEC"));
        assert!(
            d.unset_entry(&Audience::Everyone, "notes.txt")
                .expect("unset")
        );
        assert!(!text(&d).contains("not a comment"));
        assert!(
            !d.unset_entry(&Audience::Everyone, "notes.txt")
                .expect("gone")
        );
        assert!(
            !d.unset_entry(&hosts(&["m3"]), "shared")
                .expect("other audience")
        );
        assert!(!d.unset_entry(&hosts(&[]), "shared").expect("no audience"));
        assert!(
            d.unset_entry(&hosts(&["m3", "m22"]), "shared")
                .expect("unset")
        );
        let names: Vec<String> = d.entries().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["gh_token", "~/.aterm/release.toml", "gh_token"]);
        assert!(Document::parse(&text(&d)).is_ok());
    }

    #[test]
    fn add_machine_appends_and_refuses_duplicates() {
        let mut d = doc();
        d.add_machine("m40", &recipient(40)).expect("added");
        assert!(text(&d).contains(&format!(
            "m9   =   {}\nm40 = {}\n\n[admins]",
            recipient(3),
            recipient(40)
        )));
        assert_eq!(
            d.machines().last().map(|m| m.name.clone()),
            Some("m40".to_string())
        );
        assert_eq!(d.lockers()[0].recipients.len(), 4);
        let before = text(&d);
        assert!(d.add_machine("m40", &recipient(41)).is_err());
        assert!(d.add_machine("m41", &recipient(40)).is_err());
        assert!(d.add_machine("M41", &recipient(41)).is_err());
        assert!(d.add_machine("everyone", &recipient(41)).is_err());
        assert!(d.add_machine("m41", "age1short").is_err());
        assert_eq!(text(&d), before);
    }

    #[test]
    fn remove_machine_edits_headers_and_refuses_single_host_sections() {
        let mut d = doc();
        let before = text(&d);
        let e = d
            .remove_machine("m22", false)
            .expect_err("[m22] names only m22");
        assert_eq!(e.line, 19);
        assert_eq!(text(&d), before, "changes nothing");
        assert!(!d.remove_machine("m99", false).expect("absent"));
        assert!(d.remove_machine("m3", false).expect("removed"));
        let after = text(&d);
        assert!(!after.contains(&recipient(2)));
        assert!(after.contains("\n[m22]\nshared=both\n"), "{after}");
        assert_eq!(d.lockers().len(), 2);
        assert!(
            merge_for("m3", d.entries())
                .expect("merges")
                .iter()
                .all(|e| e.audience == Audience::Everyone)
        );
    }

    #[test]
    fn remove_machine_can_drop_its_own_sections() {
        let mut d = Document::parse(&base(
            "[everyone]\nk=1\n\n[m3]\nmine=1\n# inside\nmore<<END\nx\nEND\n# next\n[m22]\ns=2\n",
        ))
        .expect("valid");
        d.add_admin("m3").expect("admin");
        assert!(d.remove_machine("m3", true).expect("removed"));
        let after = text(&d);
        assert!(
            !after.contains("mine=") && !after.contains("more<<") && !after.contains("# inside"),
            "{after}"
        );
        assert!(after.contains("k=1\n\n# next\n[m22]\ns=2\n"), "{after}");
        assert_eq!(d.admins(), ["m22"]);
        assert_eq!(d.machines().len(), 1);
    }

    /// `unset` of a host section's last entry leaves its header. A section with nothing left
    /// in it holds nothing to lose, so it goes with its machine: revoke's "remove its entries
    /// first" is a way through.
    #[test]
    fn a_section_its_entries_left_goes_with_the_machine() {
        let mut d = Document::parse(&base(
            "[everyone]\nk=1\n\n[m3]\ntoken=1\n# a note\n[m22]\ns=2\n",
        ))
        .expect("valid");
        assert!(d.unset_entry(&hosts(&["m3"]), "token").expect("unset"));
        assert!(text(&d).contains("\n[m3]\n"), "unset leaves the header");
        assert!(d.remove_machine("m3", false).expect("nothing left to lose"));
        let after = text(&d);
        assert!(!after.contains("[m3]"), "{after}");
        assert!(after.contains("k=1\n\n# a note\n[m22]\ns=2\n"), "{after}");
        assert_eq!(d.machines().len(), 1);
    }

    #[test]
    fn the_last_admin_is_never_removed() {
        let mut d = Document::parse(&base("")).expect("valid");
        assert!(
            d.remove_machine("m22", false)
                .expect_err("last admin")
                .why
                .contains("no admin")
        );
        assert!(
            d.remove_admin("m22")
                .expect_err("last admin")
                .why
                .contains("last admin")
        );
        assert!(!d.remove_admin("m3").expect("not an admin"));
        d.add_admin("m3").expect("added");
        d.add_admin("m3").expect("no-op");
        assert_eq!(d.admins(), ["m22", "m3"]);
        assert!(text(&d).contains("[admins]\nm22\nm3\n"), "{}", text(&d));
        assert!(
            d.add_admin("m99")
                .expect_err("not a machine")
                .why
                .contains("not in [machines]")
        );
        assert!(d.remove_admin("m22").expect("removed"));
        assert_eq!(d.admins(), ["m3"]);
        assert!(d.remove_machine("m22", false).expect("no longer an admin"));
    }

    #[test]
    fn a_new_vault_explains_itself_and_parses() {
        let d = Document::new_vault("m22", &recipient(1));
        let rendered = text(&d);
        assert!(rendered.starts_with("# aterm secrets vault"));
        assert!(rendered.ends_with("[everyone]\n"));
        let back = Document::parse(&rendered).expect("valid");
        assert_eq!(
            back.machines(),
            [Machine {
                name: "m22".into(),
                recipient: recipient(1)
            }]
        );
        assert_eq!(back.admins(), ["m22"]);
        assert!(back.entries().is_empty());
        let plans = d.lockers();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].recipients, [recipient(1)]);
        let mut d = d;
        d.set_entry(&Audience::Everyone, "first", b"1")
            .expect("set");
        assert!(text(&d).ends_with("[everyone]\nfirst=1\n"));
        let invalid = Document::new_vault("M22", &recipient(1));
        assert!(Document::parse(&text(&invalid)).is_err());
        let mut invalid = invalid;
        assert!(invalid.set_entry(&Audience::Everyone, "a", b"1").is_err());
    }

    #[test]
    fn no_error_or_debug_output_ever_holds_a_value() {
        // Not a name, a host, a tag or base64, so no rule could quote it as one.
        let sentinel = "Zq9!sentinel+value";
        let r = recipient(1);
        let head = format!(
            "[machines]\nm22 = {r}\nm3 = {}\n[admins]\nm22\n",
            recipient(2)
        );
        let cases = vec![
            format!("{sentinel}\n{head}"),
            format!("{head}[everyone]\na={sentinel}\na={sentinel}\n"),
            format!("{head}[everyone]\n{sentinel}\n"),
            format!("{head}[everyone]\n  a={sentinel}\n"),
            format!("{head}[everyone]\na<<END\n{sentinel}\n"),
            format!("{head}[everyone]\na<<END base64\n{sentinel}\nEND\n"),
            format!(
                "{head}[everyone]\na={}\n",
                sentinel.repeat(MAX_VALUE / sentinel.len() + 1)
            ),
            format!("{head}[everyone]\na={sentinel}\r{sentinel}\n"),
            format!("{head}[everyone]\na={sentinel}\0\n"),
            format!("{head}[machines]\nm4 = {sentinel}\n"),
            format!("{head}[machines]\n{sentinel}\n"),
            format!("{head}[admins]\n{sentinel}\n"),
            format!("{head}[{sentinel}]\n"),
            format!("{head}[everyone] {sentinel}\n"),
            format!("{head}[m3]\na={sentinel}\n[m3 m22]\na={sentinel}\n"),
            format!("{head}[m99]\na={sentinel}\n"),
            format!("[machines]\nm22 = {r}\n[everyone]\na={sentinel}\n"),
            format!("{head}[everyone]\na<<END if-absent=b\n{sentinel}\nEND\n"),
            format!("{head}[everyone]\nb={sentinel}\na<<END if-absent=b\n{sentinel}\nEND\n"),
            format!("{head}[everyone]\na<<END if-absent={sentinel}\nx\nEND\n"),
        ];
        let mut shown = Vec::new();
        for source in &cases {
            let e = Document::parse(source).expect_err("refused");
            shown.push(format!("{e} {e:?}"));
            let locker = source
                .replace("[machines]", "[m1]")
                .replace("[admins]", "[m2]");
            if let Err(e) = parse_locker(locker.as_bytes()) {
                shown.push(format!("{e} {e:?}"));
            }
        }
        let mut d = Document::parse(&format!("{head}[m3]\na={sentinel}\n")).expect("valid");
        shown.push(format!("{d:?}"));
        shown.push(format!("{:?}", d.entries()));
        shown.push(format!(
            "{:?}",
            merge_for("m3", d.entries()).expect("merges")
        ));
        shown.push(format!("{:?}", d.lockers()));
        let e = d
            .set_entry(&hosts(&["m3", "m22"]), "a", sentinel.as_bytes())
            .expect_err("overlap");
        shown.push(format!("{e} {e:?}"));
        let entries =
            parse_locker(format!("[m3]\na={sentinel}\n[m3 m22]\na={sentinel}\n").as_bytes())
                .expect_err("overlap in a locker");
        shown.push(format!("{entries} {entries:?}"));
        let everyone = Entry {
            name: "a".into(),
            value: SecretBytes::from(sentinel.as_bytes()),
            audience: Audience::Everyone,
            if_absent: false,
            anchor: None,
        };
        let e = merge_for("m3", vec![everyone.clone(), everyone]).expect_err("clash");
        shown.push(format!("{e} {e:?}"));
        for line in shown {
            assert!(!line.contains(sentinel), "{line}");
            assert!(!line.contains("sentinel"), "{line}");
        }
    }

    /// A join password `strength` accepts, made at run time.
    fn strong(tag: &str) -> String {
        format!("{tag}-{}", "kmnpq".repeat(3))
    }

    #[test]
    fn a_join_password_is_set_rendered_parsed_replaced_and_removed() {
        let mut d = doc();
        assert!(d.join_password().is_none());
        let first = strong("first");
        d.set_join_password(Some(format!("  {first}\n").as_bytes()))
            .expect("set");
        assert_eq!(
            d.join_password().expect("held").as_slice(),
            first.as_bytes()
        );
        let rendered = text(&d);
        assert_eq!(
            rendered,
            format!("{}\n[join]\npassword = {first}\n", vault())
        );
        let back = Document::parse(&rendered).expect("re-parses");
        assert_eq!(
            back.join_password().expect("held").as_slice(),
            first.as_bytes()
        );
        assert_eq!(back.entries(), d.entries());
        assert!(!format!("{d:?} {back:?} {:?}", d.join_password()).contains(&first));

        // Replaced in place, characters the grammar gives meaning to included.
        let second = format!("= #[x] {}", strong("second"));
        d.set_join_password(Some(second.as_bytes()))
            .expect("replaced");
        let rendered = text(&d);
        assert_eq!(rendered.matches("password = ").count(), 1, "{rendered}");
        let back = Document::parse(&rendered).expect("re-parses");
        assert_eq!(
            back.join_password().expect("held").as_slice(),
            second.as_bytes()
        );

        // Removed: the line goes, the header stays for the next one, the rest is untouched.
        d.set_join_password(None).expect("removed");
        assert!(d.join_password().is_none());
        assert_eq!(text(&d), format!("{}\n[join]\n", vault()));
        assert!(
            Document::parse(&text(&d))
                .expect("re-parses")
                .join_password()
                .is_none()
        );
        d.set_join_password(None).expect("nothing to remove");
        d.set_join_password(Some(first.as_bytes())).expect("again");
        assert_eq!(
            text(&d),
            format!("{}\n[join]\npassword = {first}\n", vault())
        );

        // Blanks a `[join]` line pads the password with are not part of it.
        let padded = Document::parse(&base(&format!("[join]\n\t password=\t{first} \x0c\n")))
            .expect("valid");
        assert_eq!(
            padded.join_password().expect("held").as_slice(),
            first.as_bytes()
        );
    }

    #[test]
    fn weak_join_passwords_are_refused_without_quoting_them() {
        let mut d = doc();
        let before = text(&d);
        let long = format!("Zq9!{}", "x".repeat(300));
        let weak: Vec<Vec<u8>> = vec![
            b"Zq9!sent".to_vec(),
            b"   Zq9!sent   \n".to_vec(),
            b"Zq9!sentinel\nnext-line".to_vec(),
            b"Zq9!sentinel\x07bell".to_vec(),
            b"\xffZq9!sentinel-x".to_vec(),
            long.into_bytes(),
            Vec::new(),
        ];
        for password in &weak {
            let e = d.set_join_password(Some(password)).expect_err("refused");
            let shown = format!("{e} {e:?}");
            assert!(!shown.contains("Zq9") && !shown.contains("sent"), "{shown}");
            assert_eq!(e.line, 0);
        }
        assert_eq!(text(&d), before, "a refused password changes nothing");
        assert!(d.join_password().is_none());

        let e = error(&base(&format!("[join]\npassword = {}\n", "Zq9!sent")));
        assert_eq!(e.line, 7);
        assert!(e.why.contains("at least"), "{e}");
        assert!(!format!("{e} {e:?}").contains("Zq9"));
        let pw = strong("k");
        for item in [
            format!("passwd = {pw}"),
            format!("Password = {pw}"),
            pw.clone(),
            "a=1".to_string(),
            "a<<END\nx\nEND".to_string(),
        ] {
            let e = error(&base(&format!("[join]\n{item}\n")));
            assert_eq!(e.line, 7, "{e}");
            assert!(e.why.contains("expected `password = …`"), "{e}");
            assert!(!format!("{e:?}").contains(&pw));
        }
    }

    #[test]
    fn a_vault_holds_one_join_password_at_most() {
        let (a, b) = (strong("a"), strong("b"));
        let e = error(&base(&format!("[join]\npassword = {a}\npassword = {b}\n")));
        assert_eq!(e.line, 8);
        assert!(e.why.contains("one password (lines 7 and 8)"), "{e}");
        assert!(!format!("{e:?}").contains(&a) && !format!("{e:?}").contains(&b));
        let e = error(&base(&format!(
            "[join]\npassword = {a}\n[everyone]\nx=1\n[join]\npassword = {b}\n"
        )));
        assert_eq!(e.line, 11);
        // Sections accumulate: an empty [join] beside the one that holds it is fine, and the
        // password replaced in place stays the only one.
        let mut d = Document::parse(&base(&format!(
            "[join]\n# none here\n[everyone]\nx=1\n[join]\npassword = {a}\n"
        )))
        .expect("valid");
        assert_eq!(d.join_password().expect("held").as_slice(), a.as_bytes());
        d.set_join_password(Some(b.as_bytes())).expect("replaced");
        assert!(
            text(&d).ends_with(&format!("[join]\npassword = {b}\n")),
            "{}",
            text(&d)
        );
        assert!(text(&d).contains("[join]\n# none here\n[everyone]"));
    }

    #[test]
    fn an_open_vault_is_set_rendered_parsed_and_closed() {
        let pw = strong("pw");
        let mut d = Document::parse(&base(&format!("[join]\npassword = {pw}\n"))).expect("valid");
        assert!(!d.open());
        // Opening takes the password's place: a vault holds one or the other.
        d.set_open(true).expect("open");
        assert!(text(&d).ends_with("[join]\nopen = yes\n"), "{}", text(&d));
        assert!(d.join_password().is_none());
        let reread = Document::parse(&text(&d)).expect("parses");
        assert!(reread.open());
        assert_eq!(reread.render().as_slice(), d.render().as_slice());
        // The line it already holds, however spelled, changes nothing.
        let mut spelled = Document::parse(&base("[join]\n  open=yes \n")).expect("valid");
        let before = text(&spelled);
        spelled.set_open(true).expect("same");
        assert_eq!(text(&spelled), before);
        // A password closes it in the open line's place; closing a vault with a password
        // leaves the password be.
        d.set_join_password(Some(pw.as_bytes())).expect("set");
        assert!(!d.open());
        assert!(text(&d).ends_with(&format!("[join]\npassword = {pw}\n")));
        d.set_open(false).expect("not open");
        assert!(d.join_password().is_some());
        // Closing an open vault removes its line and nothing else.
        let mut open =
            Document::parse(&base("[join]\nopen = yes\n[everyone]\nx=1\n")).expect("valid");
        open.set_open(false).expect("close");
        assert_eq!(text(&open), base("[join]\n[everyone]\nx=1\n"));
        assert!(!open.open());
    }

    #[test]
    fn an_open_line_is_checked_and_never_beside_a_password() {
        let pw = strong("pw");
        let e = error(&base(&format!("[join]\nopen = yes\npassword = {pw}\n")));
        assert_eq!(e.line, 8);
        assert!(e.why.contains("not both (lines 7 and 8)"), "{e}");
        assert!(!format!("{e:?}").contains(&pw));
        let e = error(&base("[join]\nopen = yes\n[join]\nopen = yes\n"));
        assert!(e.why.contains("one `open` line"), "{e}");
        for bad in [
            "open = no",
            "open = YES",
            "open =",
            "open = yes please",
            "open = after 5",
            "opened = yes",
        ] {
            let e = error(&base(&format!("[join]\n{bad}\n")));
            assert_eq!(e.line, 7, "{bad}");
            assert!(e.why.contains("open = yes"), "{bad}: {e}");
        }
        // Only a whole vault holds it: a locker refuses [join].
        assert!(parse_locker(b"[join]\nopen = yes\n").is_err());
        let mut d = doc();
        d.set_open(true).expect("open");
        for plan in d.lockers() {
            let body = std::str::from_utf8(plan.text.as_slice()).expect("utf-8");
            assert!(!body.contains("open") && !body.contains("[join]"), "{body}");
        }
    }

    #[test]
    fn a_code_only_key_is_named_once_and_dropped_when_it_is_let_in() {
        let pw = strong("pw");
        let gone = recipient(9);
        let mut d = Document::parse(&base(&format!("[join]\npassword = {pw}\n"))).expect("valid");
        assert!(d.code_only().is_empty());
        // Named beside the password, once however often it is let go.
        d.add_code_only(&gone).expect("code-only");
        d.add_code_only(&gone).expect("again");
        assert_eq!(d.code_only(), std::slice::from_ref(&gone));
        assert!(text(&d).ends_with(&format!("[join]\npassword = {pw}\ncode-only = {gone}\n")));
        assert_eq!(
            Document::parse(&text(&d)).expect("parses").code_only(),
            std::slice::from_ref(&gone)
        );
        // Opening and closing the vault leave it be.
        d.set_open(true).expect("open");
        assert!(text(&d).ends_with(&format!("[join]\nopen = yes\ncode-only = {gone}\n")));
        d.set_open(false).expect("close");
        assert_eq!(d.code_only(), std::slice::from_ref(&gone));
        d.set_join_password(Some(pw.as_bytes())).expect("set");
        assert_eq!(d.code_only(), std::slice::from_ref(&gone));
        // A bad key is refused, written or parsed, and a key named twice names both lines.
        let before = text(&d);
        assert!(d.add_code_only("age1nope").is_err());
        assert_eq!(text(&d), before);
        let e = error(&base("[join]\ncode-only = age1nope\n"));
        assert_eq!(e.line, 7);
        assert!(e.why.contains("invalid recipient"), "{e}");
        let e = error(&base(&format!(
            "[join]\ncode-only = {gone}\ncode-only={gone}\n"
        )));
        assert_eq!(e.line, 8);
        assert!(e.why.contains("(lines 7 and 8)"), "{e}");
        // A key let in again is no longer one the vault let go; another one's line stays.
        let other = recipient(8);
        d.add_code_only(&other).expect("code-only");
        d.add_machine("back", &gone).expect("add");
        assert_eq!(d.code_only(), std::slice::from_ref(&other));
        assert!(!text(&d).contains(&format!("code-only = {gone}")));
        // Never in a locker.
        for plan in d.lockers() {
            let body = std::str::from_utf8(plan.text.as_slice()).expect("utf-8");
            assert!(
                !body.contains("code-only") && !body.contains(&other),
                "{body}"
            );
        }
    }

    #[test]
    fn the_join_password_is_a_whole_vaults_only() {
        let pw = strong("pw");
        let mut d = doc();
        d.set_join_password(Some(pw.as_bytes())).expect("set");
        d.set_entry(&hosts(&["m3"]), "mine", b"v").expect("set");
        let plans = d.lockers();
        assert_eq!(plans.len(), 4);
        for plan in &plans {
            let body = std::str::from_utf8(plan.text.as_slice()).expect("utf-8");
            assert!(
                !body.contains(&pw) && !body.contains("[join]") && !body.contains("password"),
                "{body}"
            );
            parse_locker(plan.text.as_slice()).expect("parses");
        }
        assert!(
            d.entries()
                .iter()
                .all(|e| e.value.as_slice() != pw.as_bytes())
        );

        // A locker that names [join] is refused, wherever the header is.
        let e = parse_locker(format!("[join]\npassword = {pw}\n").as_bytes()).expect_err("refused");
        assert_eq!(e.line, 1);
        let e = parse_locker(format!("[everyone]\na=1\n[join]\npassword = {pw}\n").as_bytes())
            .expect_err("refused");
        assert_eq!(e.line, 3);
        assert!(e.why.contains("[join]"), "{e}");
        assert!(!format!("{e} {e:?}").contains(&pw));

        // A plain file reads [join] as a vault does: the password is no entry, and an entry
        // written under [join] is an error, never dropped.
        let plain =
            parse_plain(format!("a=1\n[join]\npassword = {pw}\n[everyone]\nb=2\n").as_bytes())
                .expect("plain");
        assert_eq!(
            plain.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        let e = parse_plain(format!("[join]\npassword = {pw}\nc=3\n").as_bytes())
            .expect_err("an entry under [join]");
        assert_eq!(e.line, 3);
    }

    #[test]
    fn no_machine_may_be_named_join() {
        let r = recipient(40);
        let e = error(&base(&format!("[machines]\njoin = {r}\n")));
        assert_eq!(e.line, 7);
        assert!(
            e.why.contains("not machines, admins, everyone or join"),
            "{e}"
        );
        assert_eq!(error(&base("[admins]\njoin\n")).line, 7);
        let e = error(&base("[m3 join]\na=1\n"));
        assert_eq!(e.line, 6);
        assert!(e.why.contains("`join` is a section of its own"), "{e}");
        assert!(!valid_machine_name("join"));
        assert!(valid_machine_name("join1") && valid_machine_name("joiner"));
        let mut d = doc();
        let before = text(&d);
        let e = d.add_machine("join", &r).expect_err("reserved");
        assert!(e.why.contains("join"), "{e}");
        assert!(d.set_entry(&hosts(&["join"]), "a", b"1").is_err());
        assert!(d.add_admin("join").is_err());
        assert!(!d.remove_machine("join", false).expect("no such machine"));
        assert_eq!(text(&d), before);
    }

    #[test]
    fn if_absent_entries_round_trip_in_every_spelling() {
        let cases: Vec<(&str, Vec<u8>, &str)> = vec![
            // One line: a text heredoc would add a final LF, so it is base64.
            (
                "one",
                b"single line".to_vec(),
                "one<<END base64 if-absent\nc2luZ2xlIGxpbmU=\nEND\n",
            ),
            (
                "text",
                b"two\nlines\n".to_vec(),
                "text<<END if-absent\ntwo\nlines\nEND\n",
            ),
            ("empty", Vec::new(), "empty<<END base64 if-absent\nEND\n"),
            (
                "crlf",
                b"x\r\n".to_vec(),
                "crlf<<END base64 if-absent\neA0K\nEND\n",
            ),
            (
                "bin",
                vec![0xff, 0],
                "bin<<END base64 if-absent\n/wA=\nEND\n",
            ),
            (
                "tagged",
                b"END\n".to_vec(),
                "tagged<<END1 if-absent\nEND\nEND1\n",
            ),
        ];
        for (name, value, spelled) in &cases {
            let mut d = Document::parse(&base("[everyone]\n")).expect("valid");
            d.set_entry_with(&Audience::Everyone, name, value, true, None)
                .expect("set");
            let rendered = text(&d);
            assert!(
                rendered.ends_with(&format!("[everyone]\n{spelled}")),
                "{rendered}"
            );
            let back = Document::parse(&rendered).expect("re-parses");
            let entry = back
                .entries()
                .into_iter()
                .find(|e| e.name == *name)
                .expect(name);
            assert!(entry.if_absent, "{name}");
            assert_eq!(entry.value.as_slice(), value.as_slice(), "{name}");
            assert_eq!(back.entries(), d.entries(), "{name}");
        }
    }

    #[test]
    fn if_absent_is_set_and_cleared_in_place_and_reaches_the_lockers() {
        let mut d = doc();
        d.set_entry_with(&hosts(&["m22"]), "~/.aterm/machine.key", b"k\n", true, None)
            .expect("set");
        d.set_entry_with(&Audience::Everyone, "gh_token", b"t", true, None)
            .expect("set");
        assert!(
            text(&d).contains(
                "[everyone]\ngh_token<<END base64 if-absent\ndA==\nEND\nnotes.txt<<END\n"
            ),
            "{}",
            text(&d)
        );
        assert!(
            text(&d).contains("/w==\nEND\n~/.aterm/machine.key<<END if-absent\nk\nEND\n"),
            "{}",
            text(&d)
        );
        for plan in d.lockers() {
            let back = parse_locker(plan.text.as_slice()).expect("locker");
            let want: Vec<Entry> = d
                .entries()
                .into_iter()
                .filter(|e| e.audience == plan.audience)
                .collect();
            assert_eq!(back, want, "{}", plan.audience.header());
        }
        let m22: Vec<Entry> = d
            .lockers()
            .iter()
            .filter(|p| p.audience.includes("m22"))
            .flat_map(|p| parse_locker(p.text.as_slice()).expect("parses"))
            .collect();
        let merged = merge_for("m22", m22).expect("merges");
        let find = |name: &str| merged.iter().find(|e| e.name == name).expect(name);
        assert!(find("~/.aterm/machine.key").if_absent);
        assert_eq!(find("~/.aterm/machine.key").value.as_slice(), b"k\n");
        assert!(
            !find("gh_token").if_absent,
            "m22's own gh_token overrides the if-absent one"
        );
        assert!(
            merge_for("m3", d.entries())
                .expect("merges")
                .iter()
                .any(|e| e.name == "gh_token" && e.if_absent)
        );

        // Set again without the flag: the plain spelling comes back, in place.
        d.set_entry(&Audience::Everyone, "gh_token", b"t")
            .expect("set");
        assert!(
            text(&d).contains("[everyone]\ngh_token=t\nnotes.txt<<END\n"),
            "{}",
            text(&d)
        );
        assert!(!d.entries()[0].if_absent);
    }

    #[test]
    fn the_if_absent_modifier_has_one_place() {
        for (extra, why) in [
            (
                "[everyone]\na<<END if-absent base64\nEND\n",
                "heredoc opens",
            ),
            ("[everyone]\na<<END  if-absent\nEND\n", "heredoc opens"),
            (
                "[everyone]\na<<END if-absent if-absent\nEND\n",
                "heredoc opens",
            ),
            ("[everyone]\na<<END IF-ABSENT\nEND\n", "heredoc opens"),
            ("[everyone]\na<<END\tif-absent\nEND\n", "TAG"),
            ("[everyone]\na<<if-absent\nEND\n", "TAG"),
        ] {
            let e = error(&base(extra));
            assert_eq!(e.line, 7, "{extra:?}");
            assert!(e.why.contains(why), "{e} for {extra:?}");
        }
        // A one-line value is the rest of its line, verbatim: it takes no modifier.
        let d = Document::parse(&base(
            "[everyone]\na=v if-absent\nb<<END if-absent \t\nx\nEND\n",
        ))
        .expect("valid");
        let entries = d.entries();
        assert_eq!(entries[0].value.as_slice(), b"v if-absent");
        assert!(!entries[0].if_absent);
        assert!(
            entries[1].if_absent,
            "blanks after the modifier are dropped"
        );
        assert_eq!(entries[1].value.as_slice(), b"x\n");
    }

    #[test]
    fn a_new_vault_takes_a_join_password_and_if_absent_entries() {
        let mut d = Document::new_vault("m22", &recipient(1));
        let pw = strong("new");
        d.set_join_password(Some(pw.as_bytes())).expect("password");
        d.set_entry_with(
            &Audience::Everyone,
            "~/.aterm/machine.key",
            &[0, 1, 2],
            true,
            None,
        )
        .expect("entry");
        d.add_machine("m3", &recipient(2)).expect("machine");
        let rendered = text(&d);
        assert!(rendered.starts_with("# aterm secrets vault"));
        assert!(
            rendered.ends_with(&format!(
                "[everyone]\n~/.aterm/machine.key<<END base64 if-absent\nAAEC\nEND\n\n[join]\npassword = {pw}\n"
            )),
            "{rendered}"
        );
        let back = Document::parse(&rendered).expect("valid");
        assert_eq!(
            back.join_password().expect("held").as_slice(),
            pw.as_bytes()
        );
        assert_eq!(back.entries(), d.entries());
        assert!(back.entries()[0].if_absent);
        assert_eq!(back.machines().len(), 2);
        assert_eq!(text(&back), rendered);
        assert_eq!(back.lockers()[0].recipients.len(), 2);
    }

    const KEY: &str = "~/.aterm/machine.key";
    const PROFILE: &str = "~/.aterm/release-credentials.toml";

    #[test]
    fn anchored_entries_are_spelled_parsed_and_reach_the_lockers() {
        let mut d = Document::parse(&base("[everyone]\n")).expect("valid");
        // Two entries anchored to each other, and a third anchored to one of them.
        d.set_entry_with(&Audience::Everyone, KEY, &[0xff, 0], true, None)
            .expect("key");
        d.set_entry_with(&Audience::Everyone, PROFILE, b"id = 1\n", true, Some(KEY))
            .expect("profile");
        d.set_entry_with(&Audience::Everyone, KEY, &[0xff, 0], true, Some(PROFILE))
            .expect("key anchored back");
        d.set_entry_with(
            &Audience::Everyone,
            "~/.aterm/machine.toml",
            b"one line",
            true,
            Some(KEY),
        )
        .expect("machine.toml");
        let rendered = text(&d);
        assert!(
            rendered.ends_with(&format!(
                "[everyone]\n\
                 {KEY}<<END base64 if-absent={PROFILE}\n/wA=\nEND\n\
                 {PROFILE}<<END if-absent={KEY}\nid = 1\nEND\n\
                 ~/.aterm/machine.toml<<END base64 if-absent={KEY}\nb25lIGxpbmU=\nEND\n"
            )),
            "{rendered}"
        );
        let back = Document::parse(&rendered).expect("re-parses");
        assert_eq!(back.entries(), d.entries());
        let anchors: Vec<(String, Option<String>, bool)> = back
            .entries()
            .into_iter()
            .map(|e| (e.name, e.anchor, e.if_absent))
            .collect();
        assert_eq!(
            anchors,
            [
                (KEY.to_string(), Some(PROFILE.to_string()), true),
                (PROFILE.to_string(), Some(KEY.to_string()), true),
                (
                    "~/.aterm/machine.toml".to_string(),
                    Some(KEY.to_string()),
                    true
                ),
            ]
        );
        assert_eq!(back.entries()[0].value.as_slice(), [0xff, 0]);
        for plan in d.lockers() {
            let entries = parse_locker(plan.text.as_slice()).expect("locker");
            assert_eq!(entries, d.entries());
            assert_eq!(
                merge_for("m3", entries).expect("merges"),
                merge_for("m3", d.entries()).expect("merges")
            );
        }

        // Set again without an anchor, or without if-absent: the plain spellings return.
        d.set_entry_with(
            &Audience::Everyone,
            "~/.aterm/machine.toml",
            b"x\n",
            true,
            None,
        )
        .expect("unanchored");
        assert!(text(&d).ends_with("~/.aterm/machine.toml<<END if-absent\nx\nEND\n"));
        d.set_entry(&Audience::Everyone, "~/.aterm/machine.toml", b"x")
            .expect("plain");
        assert!(text(&d).ends_with("~/.aterm/machine.toml=x\n"));
    }

    #[test]
    fn an_anchor_is_another_if_absent_entry_its_receivers_all_get() {
        let refused = [
            // No such entry.
            (
                "[everyone]\na<<END if-absent=b\nx\nEND\n",
                7,
                "anchored to `b`",
            ),
            // Not an if-absent one.
            (
                "[everyone]\nb=1\na<<END if-absent=b\nx\nEND\n",
                8,
                "anchored to `b`",
            ),
            (
                "[everyone]\nb<<END\n1\nEND\na<<END if-absent=b\nx\nEND\n",
                10,
                "anchored to `b`",
            ),
            // Itself, or not an entry name at all.
            (
                "[everyone]\na<<END if-absent=a\nx\nEND\n",
                7,
                "names another entry",
            ),
            (
                "[everyone]\na<<END if-absent=\nx\nEND\n",
                7,
                "names another entry",
            ),
            (
                "[everyone]\na<<END if-absent=../b\nx\nEND\n",
                7,
                "names another entry",
            ),
            (
                "[everyone]\na<<END if-absent=~/.secrets/b\nx\nEND\n",
                7,
                "names another entry",
            ),
            // The anchor only some of the entry's receivers get.
            (
                "[m22]\nb<<END if-absent\n1\nEND\n[everyone]\na<<END if-absent=b\nx\nEND\n",
                11,
                "[everyone]",
            ),
            (
                "[everyone]\nb<<END if-absent\n1\nEND\na<<END if-absent=b\nx\nEND\n[m3]\nb=own\n",
                10,
                "host `m3`",
            ),
            // The modifier keeps its one place.
            (
                "[everyone]\nb<<END if-absent\n1\nEND\na<<END if-absent=b base64\nEND\n",
                10,
                "heredoc opens",
            ),
            (
                "[everyone]\nb<<END if-absent\n1\nEND\na<<END IF-ABSENT=b\nEND\n",
                10,
                "heredoc opens",
            ),
        ];
        for (extra, line, why) in refused {
            let e = error(&base(extra));
            assert_eq!(e.line, line, "{e} for {extra:?}");
            assert!(e.why.contains(why), "{e} for {extra:?}");
        }
        // A host's anchor may be an [everyone] entry, and a base64 heredoc takes one too.
        let d = Document::parse(&base(
            "[everyone]\nb<<END if-absent\n1\nEND\n[m22]\na<<END base64 if-absent=b\nAAEC\nEND\n",
        ))
        .expect("valid");
        assert_eq!(d.entries()[1].anchor.as_deref(), Some("b"));
        assert_eq!(merge_for("m22", d.entries()).expect("merges").len(), 2);
        assert_eq!(merge_for("m3", d.entries()).expect("merges").len(), 1);

        // An edit that would leave an anchor dangling is refused and changes nothing.
        let mut d = Document::parse(&base(
            "[everyone]\nb<<END if-absent\n1\nEND\na<<END if-absent=b\nx\nEND\nc=1\n",
        ))
        .expect("valid");
        let before = text(&d);
        for result in [
            d.set_entry_with(&Audience::Everyone, "z", b"v\n", true, Some("c")),
            d.set_entry_with(&Audience::Everyone, "z", b"v\n", true, Some("nothing")),
            d.set_entry_with(&Audience::Everyone, "z", b"v\n", false, Some("b")),
            d.set_entry_with(&Audience::Everyone, "z", b"v\n", true, Some("z")),
            d.set_entry(&Audience::Everyone, "b", b"no longer if-absent"),
            d.unset_entry(&Audience::Everyone, "b").map(|_| ()),
        ] {
            assert!(result.is_err(), "{result:?}");
        }
        assert_eq!(text(&d), before);
        // A locker is one audience's share, so its anchors are not checked on their own; a
        // merge and a plain file are.
        let locker = parse_locker(b"[m22]\na<<END if-absent=b\nx\nEND\n").expect("locker");
        let e = merge_for("m22", locker).expect_err("dangling");
        assert!(e.why.contains("`a` is anchored to `b`"), "{e}");
        let e = parse_plain(b"a<<END if-absent=b\nx\nEND\n").expect_err("dangling");
        assert_eq!(e.line, 1, "{e}");
    }
}
