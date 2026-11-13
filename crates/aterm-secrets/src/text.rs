// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The vault's text file (docs/DESIGN-secrets-2026-10-20.md §4): what a person edits.

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
    /// Its name: a file name `[A-Za-z0-9][A-Za-z0-9._-]{0,127}` or a home path `~/a/b` whose components are such names.
    pub name: String,
    /// Its value.
    pub value: SecretBytes,
    /// The section it came from.
    pub audience: Audience,
    /// Written only where no file exists yet (a heredoc's ` if-absent`): a machine's own file is never replaced, and a file this vault wrote is updated until the machine changes it, when it becomes the machine's own.
    pub if_absent: bool,
    /// The ` if-absent` entry this one is filled with (a heredoc's ` if-absent=<anchor>`; only on an ` if-absent` entry): a gap is filled, and a file the vault filled kept, only where the anchor's file is not the machine's own, so files that belong together (a release key and the profile that names it) are filled as a set, never beside the machine's own.
    pub anchor: Option<String>,
}

/// An open vault's `[join]` note, the text after its `#` word for word. It says what the
/// `open = yes` line aterm 0.147 and earlier wrote says (an admin's subscriber lets in a machine a
/// person allowed on any member, with no password and no person at an admin Mac, except a key on
/// a `code-only` line), to aterm 0.148 and later only. An earlier aterm reads past it as a comment and reads the vault
/// as closed, so its approver no longer lets in a request its own server took for a LAN one by
/// its address alone (a phone hotspot's, a café's).
pub const OPEN_NOTE: &str =
    "open = yes (aterm 0.148 and later; an earlier aterm reads this vault as closed)";

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
    /// The audience's sections in this file's syntax, headers included (what [`parse_locker`] reads back).
    pub text: SecretBytes,
}

/// A parsed, editable vault text.
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
/// The largest text file.
pub const MAX_TEXT: usize = 8 << 20;
/// The most distinct sets of hosts a vault's sections name: each is sealed in a locker of its own, beside the admins' and `[everyone]`'s, within [`crate::envelope::MAX_LOCKERS`].
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
    /// Where `[join]`'s open notes ([`OPEN_NOTE`]) are, in file order. A note beside a password
    /// opens nothing: the password holds.
    open_notes: Vec<usize>,
    /// The keys `[join]`'s `code-only` lines name, and where.
    code_only: Vec<(String, usize)>,
    /// The keys `[join]`'s notes ([`Document::set_code_only_notes`]) name, the version each says
    /// named it, and where: for a key noted twice, the last line holds.
    named_in: Vec<(String, u64, usize)>,
    /// The versions `[join]`'s run notes say were noted, from and through, and where: the last
    /// line holds.
    noted: Vec<(u64, u64, usize)>,
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
    /// Parse and validate a whole vault text: every section known, every host in an audience or `[admins]` declared in `[machines]`, at least one admin, every recipient a valid `age1…`, no duplicate machine, recipient, admin, or entry name within one audience, no entry name in two host sections that share a host, at most one `[join]` line (a strong join password, or an `open` line), heredocs terminated, base64 heredocs canonical, names and values within their rules and sizes, at most [`MAX_HOST_AUDIENCES`] distinct sets of hosts.
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

    /// A new vault text: a comment header explaining the format, `[machines]` with this one machine, `[admins]` with it, and an empty `[everyone]`.
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

    /// The text, with every edit applied (LF line ends, a final LF), and without a Claude Code
    /// token entry ([`crate::entry_class::claude_token`]), which no vault text holds: one a text
    /// an older build wrote still carries is left out, so no admin text, edit draft or locker
    /// this build writes holds it (DESIGN-claude-setup-token §4.4).
    pub fn render(&self) -> SecretBytes {
        let left_out = self.claude_token_lines();
        render_lines(
            self.lines
                .iter()
                .enumerate()
                .filter(|(n, _)| !left_out.iter().any(|lines| lines.contains(n)))
                .map(|(_, line)| line.as_str()),
        )
    }

    /// The lines of each Claude Code token entry this text holds ([`Document::render`]).
    fn claude_token_lines(&self) -> Vec<std::ops::RangeInclusive<usize>> {
        self.model
            .entries
            .iter()
            .filter(|located| crate::entry_class::claude_token(&located.entry.name))
            .map(|located| located.first..=located.last)
            .collect()
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

    /// Set the join password (`Some`), replacing any, an `open` line and every open note
    /// ([`OPEN_NOTE`]), or remove it (`None`).
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
        let mut next = self.clone();
        let notes = next.model.open_notes.clone();
        if !notes.is_empty() {
            let mut lines = next.lines.clone();
            for at in notes.into_iter().rev() {
                lines.remove(at).zeroize();
            }
            next.commit(lines)?;
        }
        match next
            .model
            .join
            .as_ref()
            .map(|(_, line)| *line)
            .or(next.model.open)
        {
            Some(line) => {
                let mut lines = next.lines.clone();
                std::mem::replace(&mut lines[line], spelled).zeroize();
                next.commit(lines)?;
            }
            None => next.append_to(&SectionKind::Join, "[join]", vec![spelled])?,
        }
        *self = next;
        Ok(())
    }

    /// Whether the vault is open: `[join]`'s `open = yes` line, or its open note ([`OPEN_NOTE`])
    /// with no join password beside it: a request needs a member's Allow or an admin's approval
    /// by code. A key [`Document::code_only`] names still needs explicit code approval.
    pub fn open(&self) -> bool {
        self.model.open.is_some()
            || (!self.model.open_notes.is_empty() && self.model.join.is_none())
    }

    /// Whether the vault is open by the `open = yes` line aterm 0.147 and earlier read, which
    /// [`Document::set_open`] writes as the open note instead.
    pub const fn open_line(&self) -> bool {
        self.model.open.is_some()
    }

    /// Make the vault open (`true`) by its open note alone ([`OPEN_NOTE`]), in the place of a join
    /// password or an `open = yes` line where it has one (so an earlier aterm reads it as
    /// closed), or close it (`false`), removing the `open` line and every open note and leaving a
    /// join password be.
    pub fn set_open(&mut self, open: bool) -> Result<(), TextError> {
        let note = format!("# {OPEN_NOTE}");
        let line = self.model.open;
        let notes = self.model.open_notes.clone();
        if open {
            let mut lines = self.lines.clone();
            let changed = match (
                line.or(self.model.join.as_ref().map(|(_, at)| *at)),
                notes.first(),
            ) {
                // The note holds already: the line or password beside it goes.
                (Some(at), Some(_)) => {
                    lines.remove(at).zeroize();
                    true
                }
                (Some(at), None) => {
                    std::mem::replace(&mut lines[at], note).zeroize();
                    true
                }
                (None, Some(_)) => false,
                (None, None) => {
                    wipe_lines(lines);
                    return self.append_to(&SectionKind::Join, "[join]", vec![note]);
                }
            };
            return if changed {
                self.commit(lines)
            } else {
                wipe_lines(lines);
                Ok(())
            };
        }
        let mut gone: Vec<usize> = line.into_iter().chain(notes).collect();
        if gone.is_empty() {
            return Ok(());
        }
        gone.sort_unstable();
        let mut lines = self.lines.clone();
        for at in gone.into_iter().rev() {
            lines.remove(at).zeroize();
        }
        self.commit(lines)
    }

    /// The keys a vault leaves to approval by code (`[join]`'s `code-only` lines), in file order: a machine it let go, or one that asked before it opened. An open vault's approver lets none of them in by itself.
    pub fn code_only(&self) -> Vec<String> {
        self.model
            .code_only
            .iter()
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// Name `recipient` on a `code-only` line in `[join]`, unless one does already.
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

    /// The version that named each key on a `code-only` line anew, as `[join]`'s notes say
    /// ([`Document::set_code_only_notes`]): a key no note names is absent, and a note of a key on
    /// no `code-only` line now is left out.
    pub fn code_only_named_in(&self) -> BTreeMap<String, u64> {
        self.model
            .named_in
            .iter()
            .filter(|(key, _, _)| self.model.code_only.iter().any(|(held, _)| held == key))
            .map(|(key, version, _)| (key.clone(), *version))
            .collect()
    }

    /// The versions `[join]`'s run note says were noted, from and through: each written by an
    /// aterm that keeps the notes ([`Document::set_code_only_notes`]).
    pub fn code_only_noted(&self) -> Option<(u64, u64)> {
        self.model
            .noted
            .last()
            .map(|(from, through, _)| (*from, *through))
    }

    /// Whether `[join]` holds a note ([`Document::set_code_only_notes`]), of a key on a
    /// `code-only` line or not.
    pub fn code_only_notes(&self) -> bool {
        !self.model.named_in.is_empty() || !self.model.noted.is_empty()
    }

    /// Rewrite `[join]`'s notes: remove every one, then, when `noted` is `Some((from, through))`
    /// and a key is on a `code-only` line, append after the last item of the last `[join]` one
    /// `# code-only <key> named in version <n>` line for each such key `named_in` names, in
    /// file order, and the run note `# code-only lines noted from version <from> through version
    /// <through>`. They are comments, which every aterm reads past and an edit keeps as they are;
    /// `[join]` is never made for them.
    pub fn set_code_only_notes(
        &mut self,
        named_in: &BTreeMap<String, u64>,
        noted: Option<(u64, u64)>,
    ) -> Result<(), TextError> {
        let mut next = self.clone();
        let mut gone: Vec<usize> = next
            .model
            .named_in
            .iter()
            .map(|(_, _, line)| *line)
            .chain(next.model.noted.iter().map(|(_, _, line)| *line))
            .collect();
        if !gone.is_empty() {
            gone.sort_unstable();
            gone.dedup();
            let mut lines = next.lines.clone();
            for line in gone.into_iter().rev() {
                lines.remove(line).zeroize();
            }
            next.commit(lines)?;
        }
        if let Some((from, through)) = noted
            && !next.model.code_only.is_empty()
        {
            let block: Vec<String> = next
                .model
                .code_only
                .iter()
                .filter_map(|(key, _)| {
                    named_in
                        .get(key)
                        .map(|version| format!("# code-only {key} named in version {version}"))
                })
                .chain(std::iter::once(format!(
                    "# code-only lines noted from version {from} through version {through}"
                )))
                .collect();
            next.append_to(&SectionKind::Join, "[join]", block)?;
        }
        *self = next;
        Ok(())
    }

    /// Every entry, in file order.
    pub fn entries(&self) -> Vec<Entry> {
        self.model
            .entries
            .iter()
            .map(|located| located.entry.clone())
            .collect()
    }

    /// Borrow every entry in file order without copying their secret values.
    pub fn entries_ref(&self) -> impl Iterator<Item = &Entry> {
        self.model.entries.iter().map(|located| &located.entry)
    }

    /// Why this text cannot be published over `base`: an entry it adds or changes is one no
    /// vault text writes ([`crate::entry_class::text_refusal`]: an AI CLI's login file, another
    /// name set apart, a Claude Code token), or shares its fold with another entry's name, which
    /// a Mac's disk makes one file ([`crate::entry_class::folded_pairs`]). One `base` holds as it
    /// is stays, so a vault an older build wrote still opens; no machine writes its file.
    pub fn added_refusal(&self, base: &Document) -> Option<String> {
        let entries: Vec<&Entry> = self
            .model
            .entries
            .iter()
            .map(|located| &located.entry)
            .collect();
        let changed = |entry: &&&Entry| base.entry(&entry.audience, &entry.name) != Some(**entry);
        if let Some(why) = entries
            .iter()
            .filter(changed)
            .find_map(|entry| crate::entry_class::text_refusal(&entry.name))
        {
            return Some(why);
        }
        let pairs =
            crate::entry_class::folded_pairs(entries.iter().map(|entry| entry.name.as_str()));
        entries.iter().filter(changed).find_map(|entry| {
            pairs
                .get(entry.name.as_str())
                .map(|other| crate::entry_class::folded_pair_refusal(&entry.name, other))
        })
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

    /// Explicit Commons eligibility without copying the secret entry values.
    #[cfg(unix)]
    pub(crate) fn commons_audience(&self) -> Option<&Audience> {
        self.model
            .entries
            .iter()
            .find(|entry| entry.entry.name == crate::commons::ENTRY)
            .map(|entry| &entry.entry.audience)
    }

    /// Whether any entry is anchored (` if-absent=<anchor>`), which an aterm older than anchors cannot read.
    pub fn anchored(&self) -> bool {
        self.model
            .entries
            .iter()
            .any(|located| located.entry.anchor.is_some())
    }

    /// One locker per distinct audience that has at least one machine, `[everyone]` first (always present, even with no entries, so every machine has a locker that tells it it is in), then host audiences in sorted order.
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
            // No locker holds a Claude Code token entry ([`Document::render`]).
            let body = self
                .model
                .entries
                .iter()
                .filter(|located| {
                    located.entry.audience == *audience
                        && !crate::entry_class::claude_token(&located.entry.name)
                })
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

    /// Append `name = recipient` to `[machines]` (creating the section if absent), and remove the `code-only` line naming `recipient`, if any.
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

    /// Remove a machine: its `[machines]` line, its `[admins]` line, and its name from every multi-host section header. Refuses to erase the Commons control record when removing its last audience member, even when dropping other private sections is allowed.
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
            if hosts.len() == 1
                && self.model.entries.iter().any(|entry| {
                    entry.entry.name == crate::commons::ENTRY
                    && matches!(&entry.entry.audience, Audience::Hosts(audience) if audience == hosts)
                })
            {
                return Err(at(
                    section.header + 1,
                    "move the Commons control audience before removing its last member",
                ));
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

    /// Add a machine to `[admins]` (it must be a machine).
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

    /// Remove a machine from `[admins]`.
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

    /// Set an entry in the section for `audience` (the first such section, created at the end if absent): replaces the existing entry's line(s) in place, or appends to the section. The reserved Commons control record moves atomically between audiences and removes its old empty section headers.
    pub fn set_entry(
        &mut self,
        audience: &Audience,
        name: &str,
        value: &[u8],
    ) -> Result<(), TextError> {
        self.set_entry_with(audience, name, value, false, None)
    }

    /// [`Document::set_entry`], marking the entry ` if-absent` when `if_absent` (always a heredoc then: a base64 one for a value a text heredoc cannot hold exactly), and anchoring it to the ` if-absent` entry `anchor` names (` if-absent=<anchor>`).
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
        if let Some(why) = cli_login_refusal(name) {
            return Err(whole(why));
        }
        if crate::entry_class::claude_token(name) {
            return Err(whole(crate::entry_class::claude_token_refusal(name)));
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
        if name == crate::commons::ENTRY
            && let Some(previous) = self
                .model
                .entries
                .iter()
                .find(|entry| entry.entry.name == name && entry.entry.audience != audience)
                .map(|entry| entry.entry.audience.clone())
        {
            // A control record has one audience. Rehome it on an isolated copy
            // so an invalid new audience never erases the current descriptor.
            let mut next = self.clone();
            next.unset_entry(&previous, name)?;
            let mut lines = next.lines.clone();
            for header in next
                .model
                .sections
                .iter()
                .rev()
                .filter(|section| {
                    section.kind == SectionKind::Audience(previous.clone())
                        && section.header == section.last_item
                })
                .map(|section| section.header)
            {
                lines.remove(header).zeroize();
            }
            next.commit(lines)?;
            next.set_entry_with(&audience, name, value, if_absent, anchor)?;
            *self = next;
            return Ok(());
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

    /// Remove an entry from the sections for `audience`.
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
    "#             Or `open = yes` instead: a machine a person allows on any member",
    "#             is let in, with no password, and reads [everyone].",
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

/// Lines joined by LF with a final LF (nothing for no lines), in one buffer sized up front:
/// one that grew would free each smaller block it outgrew, holding the text so far, unwiped.
fn render_lines<'a>(lines: impl Iterator<Item = &'a str> + Clone) -> SecretBytes {
    let mut out = Vec::with_capacity(lines.clone().map(|line| line.len() + 1).sum());
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
        if trimmed.is_empty() {
            i += 1;
            continue;
        }
        if let Some(comment) = trimmed.strip_prefix('#') {
            if current.is_some_and(|s| model.sections[s].kind == SectionKind::Join) {
                if comment.trim_matches(BLANK) == OPEN_NOTE {
                    model.open_notes.push(i);
                }
                note_line(comment, i, &mut model);
            }
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
                if crate::commons::reserved_name(&entry.name) {
                    if entry.name != crate::commons::ENTRY
                        || mode == Mode::Plain
                        || !matches!(entry.audience, Audience::Hosts(_))
                        || entry.if_absent
                        || entry.anchor.is_some()
                    {
                        return Err(at(
                            n,
                            "the Commons control entry requires its canonical name and an explicit signed host audience, without if-absent or an anchor",
                        ));
                    }
                    if model
                        .entries
                        .iter()
                        .any(|located| located.entry.name == crate::commons::ENTRY)
                    {
                        return Err(at(n, "the Commons control entry has exactly one audience"));
                    }
                }
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

/// Note a comment line of `[join]` at line index `i` (`comment`, its text after the `#`) when
/// it is one of the two notes [`Document::set_code_only_notes`] writes, word for word:
/// `code-only age1… named in version <n>` and `code-only lines noted from version <a> through
/// version <b>`, `a` no more than `b`. Any other comment is only a comment, as it is to an aterm
/// before the notes.
fn note_line(comment: &str, i: usize, model: &mut Model) {
    let words: Vec<&str> = comment.split_ascii_whitespace().collect();
    match words.as_slice() {
        ["code-only", key, "named", "in", "version", n] if valid_recipient(key) => {
            if let Some(n) = note_version(n) {
                model.named_in.push(((*key).to_string(), n, i));
            }
        }
        [
            "code-only",
            "lines",
            "noted",
            "from",
            "version",
            from,
            "through",
            "version",
            through,
        ] => {
            if let (Some(from), Some(through)) = (note_version(from), note_version(through))
                && from <= through
            {
                model.noted.push((from, through, i));
            }
        }
        _ => {}
    }
}

/// A version a note names: 1 to 20 ASCII digits that make a `u64`.
fn note_version(word: &str) -> Option<u64> {
    if !(1..=20).contains(&word.len()) || !word.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    word.parse().ok()
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
/// sections', and any other host (one that joins later, too) `[everyone]`'s alone.
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

/// Whether `entry`'s anchor, if it has one, is another ` if-absent` entry of what the machine receiving `entry` receives (`find` looks one up there by name).
pub(crate) fn anchor_received<'a>(
    entry: &Entry,
    find: impl FnOnce(&str) -> Option<&'a Entry>,
) -> bool {
    anchor_received_fields(&entry.name, entry.anchor.as_deref(), |anchor| {
        find(anchor).map(|entry| entry.if_absent)
    })
}

/// The same anchor rule over entry descriptors which need not retain their values.
pub(crate) fn anchor_received_fields(
    name: &str,
    anchor: Option<&str>,
    find: impl FnOnce(&str) -> Option<bool>,
) -> bool {
    anchor.is_none_or(|anchor| anchor != name && find(anchor) == Some(true))
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

/// The `[everyone]` locker text that holds `entries`, each spelled as a vault spells it, so
/// [`parse_locker`] reads the same entries back (a member's interim join bundle,
/// `docs/DESIGN-fluid-swarm-2026-10-28.md` F10). Each entry's own audience is not written: every
/// one is read back as [`Audience::Everyone`].
#[must_use]
pub fn render_everyone_locker(entries: &[Entry]) -> SecretBytes {
    let mut lines = vec![Audience::Everyone.header()];
    for entry in entries {
        lines.extend(spell(
            &entry.name,
            entry.value.as_slice(),
            entry.if_absent,
            entry.anchor.as_deref(),
        ));
    }
    let text = render_lines(lines.iter().map(String::as_str));
    wipe_lines(lines);
    text
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

/// Read one locker's plaintext (sections and entries only; `[machines]`, `[admins]` and `[join]` are refused, and hosts are not checked against a machine list).
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

/// Read a plain text file a person keeps themselves (§7): the §4 syntax, with items before the first header in `[everyone]`, and hosts not checked against `[machines]`.
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

/// Merge entries for `host` from several lockers: host-audience entries override `Everyone` entries of the same name; two host-audience entries of the same name that both include `host` are an error (naming the entry, not the value), as are two `Everyone` entries of the same name and an anchor that is not another ` if-absent` entry of the merged set.
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
const fn name_char(c: u8) -> bool {
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

/// What says whose AI CLI login file a home path is ([`on_cli_login`]).
static CLI_LOGIN: std::sync::OnceLock<fn(&str) -> Option<&'static str>> =
    std::sync::OnceLock::new();

/// Registers what says whose OAuth login file the home path `name` (`~/…`) is, by the product's
/// name ([`cli_login`]). Where each AI CLI keeps its login is in its connector row, and the rows
/// live in `aterm-agent`, which depends on this crate: the shipped executable registers
/// `aterm_agent::ai::connector::login_file` as it starts, so every vault path in it (`secrets
/// set` and `edit`, a document's entries, the materializer) asks the rows themselves. The first
/// registration wins; with none, or where it names no login, the built-in floor
/// ([`default_home_login`]) still answers, so a process that registered nothing fails closed.
pub fn on_cli_login(whose: fn(&str) -> Option<&'static str>) {
    let _ = CLI_LOGIN.set(whose);
}

/// Whose login the home path `name` is, where it is an AI CLI's OAuth login file: by what
/// [`on_cli_login`] registered, else by the built-in floor ([`default_home_login`]).
#[must_use]
pub fn cli_login(name: &str) -> Option<&'static str> {
    login_by(CLI_LOGIN.get().copied(), name)
}

/// [`cli_login`] with `registered` as what [`on_cli_login`] registered.
fn login_by(
    registered: Option<fn(&str) -> Option<&'static str>>,
    name: &str,
) -> Option<&'static str> {
    registered
        .and_then(|whose| whose(name))
        .or_else(|| default_home_login(name))
}

/// The AI CLI logins refused whatever [`on_cli_login`] registered, matched without case as
/// macOS's file system matches names: Claude Code's `.credentials.json`, in its home or any
/// directory a row gives it; Codex CLI's `~/.codex/auth.json` and the refresh aterm journals
/// beside it (`auth.json.refreshed`); Grok Build's `~/.grok/auth.json`; Kimi Code CLI's files
/// under `~/.kimi-code/credentials/`; Muse Code's `~/.config/muse/auth.json`; and any file
/// under `~/.aterm/logins/`, where aterm keeps an account pool's rows' logins.
fn default_home_login(name: &str) -> Option<&'static str> {
    let rel = name.strip_prefix("~/")?.to_ascii_lowercase();
    let components: Vec<&str> = rel.split('/').collect();
    match components[..] {
        [.., ".credentials.json"] => Some("Claude Code"),
        [".codex", "auth.json" | "auth.json.refreshed"] => Some("Codex CLI"),
        [".grok", "auth.json"] => Some("Grok Build"),
        [".kimi-code", "credentials", _, ..] => Some("Kimi Code CLI"),
        [".config", "muse", "auth.json"] => Some("Muse Code"),
        [".aterm", "logins", _, ..] => Some("an AI CLI"),
        _ => None,
    }
}

/// Why `name` cannot be a vault entry, where it is an AI CLI's login file ([`cli_login`]): its
/// OAuth refresh token is single-use and rotates, so a copy on another machine is spent by
/// whichever refreshes first and the other is signed out (`docs/DESIGN-secrets-2026-10-20.md`
/// §11, AI CLIs).
#[must_use]
pub fn cli_login_refusal(name: &str) -> Option<String> {
    let whose = cli_login(name)?;
    Some(format!(
        "{name} is {whose}'s login, which no vault carries: its refresh token is single-use and \
         rotates, so a copy on another machine signs one of them out; sign each machine in once \
         (aterm ai login); the lab registry (`aterm db alab`) keeps the account names"
    ))
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
#[path = "text_tests.rs"]
mod tests;
