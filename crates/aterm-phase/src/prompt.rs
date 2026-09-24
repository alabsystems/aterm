// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Parse Claude Code's approval box out of a screen. The shapes are the ones
//! measured on Claude Code 2.1.x (fixtures in the tests): a tool box headed
//! ` Bash command` (optionally `· from the "<name>" workflow`), an optional
//! ` Tip: auto mode …` row, the command (one indented row, or several rows each
//! led by `│`), its description, optional note rows, `Do you want to proceed?`,
//! the numbered options, and ` Esc to cancel · Tab to amend`; the workflow
//! box ` Run a dynamic workflow?` with `│`-led description rows; the folder
//! trust dialog ` Accessing workspace:` (2.1.280) with UNNUMBERED options
//! under a `❯` cursor and ` Enter to confirm · Esc to cancel`. Any other box
//! is a prompt of kind [`PromptKind::Other`] — the supervisor hands it to
//! the manager rather than guess.
//!
//! **DETECTION BY SHAPE** ([`find_box`]). A box is its key-hint FOOTER — a
//! row of `<key> to <verb>` hints joined by ` · `, one of them `Esc …`, in
//! the box's own columns ([`is_hint_footer`]) — with option rows above it.
//! Not the one phrase `Esc to cancel` anywhere on a row: that read a
//! worker's `⏺ … press Esc to cancel it …`, a bullet quoting it and a
//! shell's grep hit as live boxes (all three measured), and missed the
//! dialogs whose Esc says `go back`. The HEADER is the box's first row,
//! read from inside it ([`box_top`]: under the rule 2.1.280 draws over every
//! box, or under the nearest row in column 0), never the nearest
//! header-looking row above the footer: a Bash box whose description row
//! read `Create file x` or `Read file /etc/hosts` parsed as a Write or a
//! Read with no command (measured, 2.1.280).
//!
//! **PROMPT V2** ([`parse_prompt_v2`]). [`Prompt`] cannot be answered by
//! meaning: it has no option roles, no cursor, no numbering flag and no
//! cancel semantics. [`PromptV2`] adds the title, every row the command
//! may occupy and the readings a decider must classify ("READING THE
//! COMMAND" below), each option's [`Role`] from a label table and whether
//! the cursor is on it, how an option is chosen ([`Select`]: digits, or
//! arrows and Enter where no numbers are drawn), and what the footer's Esc
//! does ([`CancelEffect`]: the trust dialog's cancel EXITS Claude Code, per
//! the 2.1.280 binary's handler). The two parsers share one reading and
//! agree on every field they both have.
//!
//! **A `│`-LED DESCRIPTION.** Measured 2026-09-21 on Claude Code 2.1.278 in
//! a live aterm tab ([`fixtures::bash_multi_row_with_note`]): when the
//! command wraps over several `│` rows, the description row is led by `│`
//! too (`│ Extract three trees to scratch, format them identically, and diff
//! for real content changes`), where the earlier build drew it bare under
//! the bars ([`fixtures::bash_multi_row`], `   Sync the checkout before
//! verifying`). Both shapes are SHOWN: in a Bash block with several `│`
//! rows, the LAST `│` row is shown as the description when it is the last
//! row of the block and reads as prose — no shell metacharacter (none of
//! `|;&$(){}<>`), not led by `-`, at least three words, and an uppercase
//! ASCII first letter ([`reads_as_prose`]). That is a guess, and
//! `HOME=/tmp rm -rf ~/work` passes it, so the row stays in
//! [`PromptV2::command_rows`] either way; a bare row under the bars is the
//! description as before.
//!
//! **READING THE COMMAND** ([`PromptV2::readings`]). The command's rows are
//! the rows at its own column (its first row's; notes sit left of it, at
//! the box's column) down to the question, blank rows or not. Without bars
//! the command is one shell line that may wrap, and the model-written
//! description is drawn under it at the same column: the screen cannot tell
//! the command's wrapped tail from the description (Claude Code trims the
//! trailing spaces a wrap can hide), so every such row is a command row and
//! the first is only SHOWN as the command. A decider classifies every
//! reading — the shown command, and all the rows, space-joined, and
//! newline-joined too when the box has bars — and approves only when all of
//! them are harmless.
//!
//! **OPTIONS** ([`options_of`]). A box's options are read after its LAST
//! `Do you want …` row when it has one: above it are the model's words, and
//! a description reading `2. Yes` is not option 2. Roles are assigned only
//! when the options are sound — numbered `1.` to `n.` in order with only
//! blank and divider rows among and under them (or, unnumbered, one block
//! with one cursor), and one cursor at most. Otherwise every role is
//! [`Role::Other`] and nothing approves by them.
//!
//! **NOTE ROWS** ([`Prompt::notes`]): the rows of a Bash box left of the
//! command's column (at the box's own column), before `Do you want to
//! proceed?` or the first option row, are notes, one entry per row with a
//! leading `│` and the surrounding whitespace stripped: the critical-path
//! warning `│ Dangerous rm operation on possibly-empty variable path: …`
//! (2.1.278 and 2.1.280, measured), or the bare ` This command requires
//! approval` of the earlier build. `Tip:` rows are never notes, and
//! neither is 2.1.281's auto-deny countdown (`⚠ Claude Code will
//! automatically deny this request in 1:59, …`, [`PromptV2::auto_deny`]),
//! which it draws right under the note — joined into it, the rm breaker's
//! warning no longer read as one (the live E2E of 2026-09-24, D3). An
//! Edit/Write/Read box's later blocks are its diff or its contents, not
//! notes, so `notes` is empty there.
//!
//! **A BOX IN THE TRANSCRIPT IS NOT A PROMPT** ([`find_box`]). A
//! manager's screen shows its workers' boxes: a Monitor event or a tool's
//! output that prints a worker's screen puts ` Esc to cancel · Tab to amend`
//! in the MANAGER's transcript, and the whole-screen search this parser used
//! to do read that manager as `prompt`. (Observed live on 0.86.0, 2026-09-15:
//! a manager's own presence row read `phase=prompt`. That screen was not
//! kept; its footer — `bypass permissions on · 1 monitor` over the artifact
//! bar — reads busy or idle, never `prompt`.)
//! Three things mark a copy, and each is a fact of Claude Code's layout
//! rather than a guess about the words: the row hangs under the `⎿` gutter
//! of an output block; every row from a `⏺` row down to the footer sits in
//! the message's continuation column (two or more) — a live box's title,
//! options and footer sit at column one ([`box_top`]); or the worker's
//! later words (a `⏺` row) or a finished turn's done row lie between it and
//! the composer — the box a worker is blocked on sits in the live zone at
//! the bottom, and nothing the transcript gains while it waits is drawn
//! under it. A spinner row between the two decides nothing (where Claude
//! Code draws one beside a live box is not measured).

/// What the box is asking to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    /// ` Bash command` — run `command`.
    Bash,
    /// ` Edit file`.
    Edit,
    /// ` Write file` / ` Create file`.
    Write,
    /// ` Overwrite file` (the 2.1.280 binary titles a Write over an existing
    /// file so; no capture yet).
    Overwrite,
    /// ` Read file(s)`.
    Read,
    /// ` Run a dynamic workflow?`.
    Workflow,
    /// The folder-trust dialog, ` Accessing workspace:` (2.1.280, measured):
    /// unnumbered options, the cursor on `No, exit`, and a cancel that EXITS
    /// Claude Code.
    Trust,
    /// A box this parser has no header for.
    Other,
}

impl PromptKind {
    /// The lowercase word the CLI prints (`kind bash`).
    pub fn name(self) -> &'static str {
        match self {
            PromptKind::Bash => "bash",
            PromptKind::Edit => "edit",
            PromptKind::Write => "write",
            PromptKind::Overwrite => "overwrite",
            PromptKind::Read => "read",
            PromptKind::Workflow => "workflow",
            PromptKind::Trust => "trust",
            PromptKind::Other => "other",
        }
    }
}

/// One parsed approval box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    /// The box's kind.
    pub kind: PromptKind,
    /// The command line (Bash), the path (Edit/Write/Overwrite/Read), the
    /// folder (the trust dialog), empty otherwise.
    pub command: String,
    /// The one-line description under the command; the workflow's description;
    /// the surrounding rows for [`PromptKind::Other`].
    pub description: String,
    /// The note rows of a Bash box (module header, "NOTE ROWS"), one entry
    /// per row, `│` and surrounding whitespace stripped; empty for every
    /// other kind.
    pub notes: Vec<String>,
    /// The numbered options as `(number, text)` — `(1, "Yes")`,
    /// `(2, "Yes, and don't ask again for: git log *")`, `(4, "No")` — a
    /// label wrapped onto the rows under it joined back with one space.
    pub options: Vec<(u8, String)>,
}

/// What choosing an option does (module header, "PROMPT V2").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Allow this one call (`Yes`, `Yes, proceed`, `Yes, run it`).
    Once,
    /// Allow calls like it for the rest of the session (`Yes, and allow
    /// reading from ~/.ssh during this session`).
    Session,
    /// A durable grant (`Yes, and don't ask again for: git log *`, `Yes, and
    /// always allow access to <dir> from this project`).
    Persist,
    /// Switch the session's permission mode (`Yes, and switch to auto mode`,
    /// `… accept edits …`, anything marked `(shift+tab)`).
    ModeSwitch,
    /// Refuse (`No`, `No, and tell Claude what to do differently`).
    Deny,
    /// Quit the program (`No, exit`).
    Exit,
    /// Trust the folder (`Yes, I trust this folder`).
    Trust,
    /// None of the above: a label this table does not know (`View raw
    /// script`, a garbled row), or any option of a reader that assigns no
    /// roles. Never approve by it.
    Other,
}

impl Role {
    /// The lowercase word the CLI prints.
    pub fn name(self) -> &'static str {
        match self {
            Role::Once => "once",
            Role::Session => "session",
            Role::Persist => "persist",
            Role::ModeSwitch => "mode-switch",
            Role::Deny => "deny",
            Role::Exit => "exit",
            Role::Trust => "trust",
            Role::Other => "other",
        }
    }
}

/// One option of a box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opt {
    /// Its number (`1.`), `None` for a dialog that draws none.
    pub n: Option<u8>,
    /// Its label, wrapped rows joined with one space.
    pub label: String,
    pub role: Role,
    /// Whether the `❯` cursor is on it (what Enter picks).
    pub focused: bool,
    /// The row it starts on.
    pub row: usize,
}

/// How an option is chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Select {
    /// Its digit.
    Digits,
    /// The arrows to move the cursor, then Enter (no numbers are drawn).
    ArrowsEnter,
}

/// What the footer's Esc does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelEffect {
    /// Refuse the request (a permission box: `Esc to cancel`).
    Reject,
    /// Quit the program (the trust dialog's `Esc to cancel`: its cancel
    /// handler exits, per the 2.1.280 binary).
    Exit,
    /// Leave the dialog without deciding (`Esc to go back`, `… close`).
    Back,
}

/// The footer's Esc hint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cancel {
    /// The key word as drawn (`Esc`).
    pub key: String,
    /// The hint's verb (`cancel`, `go back`).
    pub verb: String,
    pub effect: CancelEffect,
}

/// One approval box, read whole (module header, "PROMPT V2"): everything
/// [`Prompt`] says, plus the title, the command's own rows, the options'
/// roles and focus, how to choose, and what cancelling does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptV2 {
    pub kind: PromptKind,
    /// The box's first row, its header (`Bash command`, `Accessing
    /// workspace:`), read from inside the box — never a row above it.
    pub title: String,
    /// EVERY row the command may occupy (module header, "READING THE
    /// COMMAND"), a `│` bar and the indent stripped: in a box with bars,
    /// every `│` row at the command's column; without bars, every row at the
    /// command's column, the model-written description included, because
    /// the screen cannot tell a wrapped command's tail from a description
    /// (Claude Code trims the trailing spaces a wrap may hide). Bash only.
    /// A decider classifies [`Self::readings`], never a subset it chose.
    pub command_rows: Vec<String>,
    /// How many of the LAST [`Self::command_rows`] the parser SHOWS as the
    /// description ([`Self::description`]) — a guess for display, never a
    /// licence to leave them out of a safety decision.
    pub description_rows: usize,
    /// Whether the command was drawn with `│` bars (Claude Code draws them
    /// for a command with a newline in it). Without bars the command is one
    /// shell line, so a row break in it is a wrap, never a statement break.
    pub gutter: bool,
    /// [`Prompt::command`], for display: a Bash box's `command_rows` less
    /// the last [`Self::description_rows`], joined with single spaces; a
    /// file box's path; the trust dialog's folder.
    pub command: String,
    pub description: String,
    /// The note BLOCKS of a Bash box, each block's rows joined with one space
    /// (a wrapped `Dangerous rm operation …` warning is one note).
    pub notes: Vec<String>,
    /// The vendor's auto-deny countdown row, `⚠` stripped (`Claude Code will
    /// automatically deny this request in 1:59, to avoid blocking progress
    /// on an unattended session`; 2.1.281): the box will be DENIED when it
    /// runs out. Never a note (module header, "NOTE ROWS").
    pub auto_deny: Option<String>,
    /// The file an Edit/Write/Overwrite/Read box names, or the folder the
    /// trust dialog asks about — for the trust dialog, the rows of its path
    /// block joined with nothing between them (a path wraps mid-token). A
    /// space the wrap fell on is lost, so a decider checks the folder
    /// against the session's own working directory, not this text alone.
    pub path: Option<String>,
    /// The options: the rows after the box's question (`Do you want …`)
    /// when it has one. When they do not run `1.`, `2.`, … in order from
    /// one contiguous block, or more than one carries the cursor, every
    /// option's role is [`Role::Other`] (module header, "OPTIONS").
    pub options: Vec<Opt>,
    pub select: Select,
    pub cancel: Option<Cancel>,
    /// The box's rows, title to footer, inclusive.
    pub span: (usize, usize),
}

impl PromptV2 {
    /// All of [`Self::command_rows`] joined with newlines: in a box with
    /// bars, a row break may be the shell's statement break.
    #[must_use]
    pub fn joined(&self) -> String {
        self.command_rows.join("\n")
    }

    /// All of [`Self::command_rows`] joined with single spaces: a row break
    /// may be a wrap.
    #[must_use]
    pub fn spaced(&self) -> String {
        self.command_rows.join(" ")
    }

    /// Every reading of the command a decider must classify before calling
    /// it harmless (module header, "READING THE COMMAND"): the rows the
    /// parser takes for the command, and all of [`Self::command_rows`] —
    /// each space-joined, and newline-joined too when the box has bars.
    /// Approve only when EVERY reading is harmless: the first catches a
    /// description that turns a write into a read (`git stash` described
    /// `list …`), the second a command tail the parser took for the
    /// description. Empty for a box with no command rows.
    #[must_use]
    pub fn readings(&self) -> Vec<String> {
        let rows = &self.command_rows;
        let head = &rows[..rows.len().saturating_sub(self.description_rows)];
        let mut out = vec![head.join(" "), rows.join(" ")];
        if self.gutter {
            out.push(head.join("\n"));
            out.push(rows.join("\n"));
        }
        let mut seen: Vec<String> = Vec::new();
        for r in out {
            if !r.is_empty() && !seen.contains(&r) {
                seen.push(r);
            }
        }
        seen
    }

    /// The option the cursor is on.
    #[must_use]
    pub fn focused(&self) -> Option<&Opt> {
        self.options.iter().find(|o| o.focused)
    }

    /// The first option with `role`.
    #[must_use]
    pub fn with_role(&self, role: Role) -> Option<&Opt> {
        self.options.iter().find(|o| o.role == role)
    }
}

/// Parse the approval box on `rows`, if one is showing.
pub fn parse_prompt(rows: &[String]) -> Option<Prompt> {
    parse(rows).map(|(p, _)| p)
}

/// Parse the approval box on `rows` in full (module header, "PROMPT V2").
/// `Some` exactly when [`parse_prompt`] is.
pub fn parse_prompt_v2(rows: &[String]) -> Option<PromptV2> {
    parse(rows).map(|(_, v)| v)
}

/// The box's parts on the screen.
#[derive(Debug, Clone, Copy)]
struct Found {
    /// The key-hint footer row.
    footer: usize,
    /// The title row: the box's first non-blank row.
    title: usize,
}

fn parse(rows: &[String]) -> Option<(Prompt, PromptV2)> {
    let b = find_box(rows)?;
    let title = rows[b.title].trim();
    let kind = header_kind(title).unwrap_or(PromptKind::Other);
    let (options, content_end) = options_of(rows, b.title + 1, b.footer, kind);
    let body = &rows[b.title + 1..content_end];
    let select = if options.iter().any(|o| o.n.is_some()) {
        Select::Digits
    } else {
        Select::ArrowsEnter
    };
    let numbered: Vec<(u8, String)> = options
        .iter()
        .filter_map(|o| o.n.map(|n| (n, o.label.clone())))
        .collect();
    let mut v1 = Prompt {
        kind,
        command: String::new(),
        description: String::new(),
        notes: Vec::new(),
        options: numbered,
    };
    let mut v2 = PromptV2 {
        kind,
        title: title.to_string(),
        command_rows: Vec::new(),
        description_rows: 0,
        gutter: false,
        command: String::new(),
        description: String::new(),
        notes: Vec::new(),
        auto_deny: body.iter().find_map(|r| auto_deny_of(r)),
        path: None,
        options,
        select,
        cancel: cancel_of(&rows[b.footer], kind),
        span: (b.title, b.footer),
    };
    match kind {
        PromptKind::Other => {
            let start = b.footer.saturating_sub(30);
            let end = (b.footer + 10).min(rows.len());
            v1.description = rows[start..end].join("\n");
            v2.description = prose_of(body);
        }
        PromptKind::Workflow => {
            let description = body
                .iter()
                .map(|r| r.trim())
                .filter_map(|r| r.strip_prefix('│'))
                .map(str::trim)
                .collect::<Vec<_>>()
                .join(" ");
            v1.description.clone_from(&description);
            v2.description = description;
        }
        PromptKind::Trust => {
            v2.path = trust_path(body);
            v2.description = prose_of(body);
            v2.command = v2.path.clone().unwrap_or_default();
            v1.command.clone_from(&v2.command);
            v1.description.clone_from(&v2.description);
        }
        _ => {
            let c = content_of(body, kind);
            let head = c.command_rows.len() - c.description_rows;
            v1.command = c.command_rows[..head].join(" ");
            v1.description.clone_from(&c.description);
            v1.notes = c.note_rows;
            v2.command = v1.command.clone();
            v2.description = c.description;
            v2.notes = c.note_blocks;
            if kind == PromptKind::Bash {
                v2.command_rows = c.command_rows;
                v2.description_rows = c.description_rows;
                v2.gutter = c.gutter;
            } else if !v2.command.is_empty() {
                v2.path = Some(v2.command.clone());
            }
        }
    }
    Some((v1, v2))
}

/// The auto-deny countdown's text when `row` is that row (`⚠` and the
/// surrounding whitespace stripped), `None` otherwise.
fn auto_deny_of(row: &str) -> Option<String> {
    let t = row.trim().trim_start_matches('⚠').trim();
    t.starts_with(crate::anchors::anchor_text("box.auto_deny"))
        .then(|| t.to_string())
}

/// A tool box's content, read from the rows between its title and its
/// question (or its options).
struct Content {
    command_rows: Vec<String>,
    description_rows: usize,
    gutter: bool,
    description: String,
    note_rows: Vec<String>,
    note_blocks: Vec<String>,
}

fn content_of(body: &[String], kind: PromptKind) -> Content {
    use crate::phase::leading_spaces;
    let is_tip = |r: &String| r.trim().starts_with("Tip:");
    if kind != PromptKind::Bash {
        // A file box: its path is its first row; the rest is a diff or the
        // file's contents.
        let path = body
            .iter()
            .find(|r| !r.trim().is_empty() && !is_tip(r))
            .map(|r| r.trim().to_string());
        return Content {
            command_rows: path.into_iter().collect(),
            description_rows: 0,
            gutter: false,
            description: String::new(),
            note_rows: Vec::new(),
            note_blocks: Vec::new(),
        };
    }
    // The command's column is its first row's (module header, "READING THE
    // COMMAND"); every row at it or deeper is the command or its
    // description, whatever blank rows fall between, and a row left of it is
    // a note (a `Tip:` there is neither).
    let col = body
        .iter()
        .find(|r| !r.trim().is_empty() && !is_tip(r))
        .map_or(0, |r| leading_spaces(r));
    let mut block: Vec<&str> = Vec::new();
    let mut note_rows: Vec<String> = Vec::new();
    let mut note_blocks: Vec<String> = Vec::new();
    let mut in_note = false;
    for r in body {
        let t = r.trim();
        if t.is_empty() {
            in_note = false;
            continue;
        }
        if leading_spaces(r) >= col {
            block.push(t);
            in_note = false;
            continue;
        }
        if t.starts_with("Tip:") {
            continue;
        }
        if auto_deny_of(t).is_some() {
            in_note = false;
            continue;
        }
        let note = t.strip_prefix('│').map_or(t, str::trim);
        note_rows.push(note.to_string());
        match note_blocks.last_mut() {
            Some(last) if in_note => {
                last.push(' ');
                last.push_str(note);
            }
            _ => note_blocks.push(note.to_string()),
        }
        in_note = true;
    }
    let last_bar = block.iter().rposition(|r| r.starts_with('│'));
    let (command_rows, description_rows, description) = match last_bar {
        // No bars: one shell line, maybe wrapped, then the description — all
        // of it is kept; the first row is SHOWN as the command.
        None => {
            let desc = block.iter().skip(1).copied().collect::<Vec<_>>().join(" ");
            let rows: Vec<String> = block.iter().map(|r| r.to_string()).collect();
            let shown = rows.len().saturating_sub(1);
            (rows, shown, desc)
        }
        // Bars: every row down to the last bar is the command (a bare row
        // among them too); bare rows under the last bar are the description.
        Some(last) => {
            let rows: Vec<String> = block[..=last]
                .iter()
                .map(|r| r.strip_prefix('│').map_or(*r, str::trim).to_string())
                .collect();
            let mut desc: Vec<&str> = block[last + 1..].to_vec();
            // 2.1.278 leads the description of a wrapped command with `│`
            // too (module header): the last bar row is SHOWN as the
            // description when it closes the block and reads as prose.
            let bars = block[..=last].iter().filter(|r| r.starts_with('│')).count();
            let shown = usize::from(
                bars >= 2 && desc.is_empty() && rows.last().is_some_and(|r| reads_as_prose(r)),
            );
            if shown == 1 {
                desc.push(rows.last().map_or("", String::as_str));
            }
            let desc = desc.join(" ");
            (rows, shown, desc)
        }
    };
    Content {
        command_rows,
        description_rows,
        gutter: last_bar.is_some(),
        description,
        note_rows,
        note_blocks,
    }
}

/// The trust dialog's folder: its path block — the first row led by `/` or
/// `~` and the rows under it up to a blank row — joined with nothing between
/// them, because a path wraps mid-token (a 99-column path in a 120-column
/// grid is measured; a narrower grid wraps it).
fn trust_path(body: &[String]) -> Option<String> {
    let at = body.iter().position(|r| {
        let t = r.trim();
        t.starts_with('/') || t.starts_with('~')
    })?;
    Some(
        body[at..]
            .iter()
            .map(|r| r.trim())
            .take_while(|t| !t.is_empty())
            .collect(),
    )
}

/// A dialog's prose: its non-blank rows that are not options, not a path
/// block (a row led by `/` or `~` and the rows wrapped under it) and not a
/// link label (`Security guide`), joined with one space. The caller passes
/// the rows above the options.
fn prose_of(body: &[String]) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut in_path = false;
    for t in body.iter().map(|r| r.trim()) {
        if t.is_empty() {
            in_path = false;
            continue;
        }
        in_path |= t.starts_with('/') || t.starts_with('~');
        if !in_path
            && option_row(t).is_none()
            && !t.starts_with('❯')
            && t.split_whitespace().count() > 2
        {
            out.push(t);
        }
    }
    out.join(" ")
}

/// Whether a `│` row under a wrapped command is its description rather than
/// more of the command (module header, "A `│`-LED DESCRIPTION"): no shell
/// metacharacter (`|;&$(){}<>`), not led by `-`, at least three words, and an
/// uppercase ASCII first letter.
fn reads_as_prose(row: &str) -> bool {
    const SHELL: &[char] = &['|', ';', '&', '$', '(', ')', '{', '}', '<', '>'];
    !row.starts_with('-')
        && !row.contains(SHELL)
        && row.split_whitespace().count() >= 3
        && row.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

/// The inclusive row span of the box, its title row to its footer, for
/// printing it verbatim.
pub fn prompt_box_span(rows: &[String]) -> Option<(usize, usize)> {
    find_box(rows).map(|b| (b.title, b.footer))
}

/// The box's OWN first row — its title, the first row [`find_box`] reads as
/// the box's — never a transcript row above it, however tall the transcript
/// (round-25 review, 2026-09-23: a supervisor's badge quoted `⏺ transcript
/// row 19 done` from a fixed 30-row window). `None` without a live box.
pub fn prompt_box_first_row(rows: &[String]) -> Option<usize> {
    find_box(rows).map(|b| b.title)
}

/// How far above its footer a box's title may sit.
const BOX_HEIGHT: usize = 60;

/// The box a worker is blocked on (module header, "DETECTION BY SHAPE"):
/// the LAST key-hint footer on the screen ([`is_hint_footer`]) — unless that
/// row is a copy in the transcript ([`under_gutter`], [`said_under`]), then
/// `None`, because a live box would sit under every copy — with the box's
/// rows above it ([`box_top`]) holding its title and at least one option.
fn find_box(rows: &[String]) -> Option<Found> {
    let footer = rows.iter().rposition(|r| is_hint_footer(r))?;
    if under_gutter(rows, footer) || said_under(rows, footer) {
        return None;
    }
    let top = box_top(rows, footer)?;
    let title = (top..footer).find(|&i| !rows[i].trim().is_empty())?;
    rows[title..footer]
        .iter()
        .any(|r| option_row(r).is_some() || pointer_row(r))
        .then_some(Found { footer, title })
}

/// A box's footer: the whole row, trimmed, is key hints (`<key> to <verb>`)
/// joined by ` · `, one of them `Esc …` — `Esc to cancel · Tab to amend`,
/// `Enter to confirm · Esc to cancel`, `Esc to go back`, the question
/// dialog's `Enter to select · Tab/Arrow keys to navigate · Esc to cancel`
/// (2.1.280 binary) — starting within the box's own columns (three at
/// most). The Esc item's key is exactly `Esc`, so a worker's `⏺ … press Esc
/// to cancel it …`, a bullet `- Esc to cancel the run`, a grep hit quoting
/// the words, the busy footer's lowercase `esc to interrupt` and the limit
/// banner's `· esc to cancel` are not footers.
fn is_hint_footer(row: &str) -> bool {
    use crate::phase::leading_spaces;
    let t = row.trim();
    if t.is_empty() || leading_spaces(row) > 3 {
        return false;
    }
    let items: Vec<Option<(&str, &str)>> = t.split(" · ").map(hint_item).collect();
    items.iter().all(Option::is_some) && items.iter().flatten().any(|(key, _)| *key == "Esc")
}

/// `<key> to <verb>` → `(key, verb)`: a key of one to three words (at most
/// sixteen characters, each word led by a letter, a digit or an arrow:
/// `Esc`, `↑/↓`, `ctrl+g`, `Tab/Arrow keys`, `Any key`) and a lowercase verb
/// of at most six words (`edit in <editor>`).
fn hint_item(item: &str) -> Option<(&str, &str)> {
    let (key, verb) = item.split_once(" to ")?;
    let words = key.split(' ').collect::<Vec<_>>();
    let key_ok = !key.is_empty()
        && key.chars().count() <= 16
        && words.len() <= 3
        && words.iter().all(|w| {
            w.chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || "↑↓←→".contains(c))
        });
    let verb_ok =
        verb.starts_with(|c: char| c.is_lowercase()) && verb.split_whitespace().count() <= 6;
    (key_ok && verb_ok).then_some((key, verb))
}

/// Where the box above `footer` begins: under the nearest full-width rule
/// (Claude Code 2.1.280 draws one over every box), or under the nearest row
/// in column 0 — a shell's line, a user row, a transcript row — within
/// [`BOX_HEIGHT`] rows; the top of that window when there is neither. A
/// box's own rows are indented (its title and options at column one, its
/// content further). `None` under a `⏺` row or a `⎿` row when the rows run
/// straight on from it, or when every row from it down to the footer is
/// indented two or more — a message's continuation column: they are that
/// message's words or that tool's output, a box quoted, not a box. "Straight
/// on" is read past the said row's OWN continuation (the rows right under it
/// in column two or further: a wrapped message, a tool's second output row),
/// so a box under a blank row below a multi-row `⎿` output is still a box.
fn box_top(rows: &[String], footer: usize) -> Option<usize> {
    use crate::phase::{is_rule, leading_spaces};
    let floor = footer.saturating_sub(BOX_HEIGHT);
    for i in (floor..footer).rev() {
        let r = &rows[i];
        if is_rule(r) || is_frame_edge(r) || crate::phase::is_glyph_row(r) {
            return Some(i + 1);
        }
        let t = r.trim_start();
        let said = r.starts_with(['⏺', '●']) || t.starts_with('⎿');
        if said {
            let mut end = i + 1;
            while end < footer && !rows[end].trim().is_empty() && leading_spaces(&rows[end]) >= 2 {
                end += 1;
            }
            let own_column = rows[end..=footer]
                .iter()
                .any(|r| !r.trim().is_empty() && leading_spaces(r) < 2);
            return (rows[end].trim().is_empty() && own_column).then_some(end);
        }
        if r.starts_with(|c: char| c.is_alphanumeric() || matches!(c, '❯' | '$' | '%' | '>')) {
            return Some(i + 1);
        }
    }
    Some(floor)
}

/// A closed frame's top or bottom edge above a box (`╭───╮`, `╰───╯`): only
/// box-drawing characters, led by a corner. Never the indented `────`
/// separator inside a question dialog, which is the box's own row, and never
/// a bare `│` continuation row. Upstream's round-25 test pins it: a box under
/// a closed frame starts below the frame's edge, not on it.
fn is_frame_edge(row: &str) -> bool {
    let t = row.trim();
    t.starts_with(['╭', '╰', '┌', '└'])
        && t.chars()
            .all(|c| ('\u{2500}'..='\u{257F}').contains(&c) || c == ' ')
}

/// Whether row `footer` belongs to an output block under the `⎿` gutter — a
/// tool's output or a Monitor event, which Claude Code indents five columns
/// or more under the row that opens it with `⎿`. A live box's own rows start
/// at column one to three, so a row indented less than five is never under
/// the gutter; one indented more is, when walking up over blank rows and rows
/// indented five or more reaches a `⎿` row.
fn under_gutter(rows: &[String], footer: usize) -> bool {
    use crate::phase::leading_spaces;
    if rows[footer].trim_start().starts_with('⎿') {
        return true;
    }
    if leading_spaces(&rows[footer]) < 5 {
        return false;
    }
    for row in rows[..footer].iter().rev() {
        let t = row.trim_start();
        if t.starts_with('⎿') {
            return true;
        }
        if !t.is_empty() && leading_spaces(row) < 5 {
            return false;
        }
    }
    false
}

/// Whether the transcript went on under row `footer`: between it and the
/// composer's top rule there is a `⏺` row or a `⎿` output row (the worker
/// said or did something after the box), or a DONE row (`✻ Worked for 3m 21s
/// · done 8:50 PM` — the turn ended after it). A spinner row is neither: it
/// is left out on purpose (module header). Without the composer frame, or
/// with the row under the frame, `false`.
fn said_under(rows: &[String], footer: usize) -> bool {
    use crate::phase::{composer_top, is_done_row, is_glyph_row, is_transcript_row};
    let Some(top) = composer_top(rows).filter(|&top| top > footer) else {
        return false;
    };
    rows[footer + 1..top]
        .iter()
        .any(|r| is_transcript_row(r) || (is_glyph_row(r) && is_done_row(r)))
}

fn header_kind(row: &str) -> Option<PromptKind> {
    let t = row.trim();
    if t.starts_with("Bash command") {
        Some(PromptKind::Bash)
    } else if t.starts_with("Edit file") {
        Some(PromptKind::Edit)
    } else if t.starts_with("Write file") || t.starts_with("Create file") {
        Some(PromptKind::Write)
    } else if t.starts_with("Overwrite file") {
        Some(PromptKind::Overwrite)
    } else if t.starts_with("Read file") {
        Some(PromptKind::Read)
    } else if t.starts_with("Run a dynamic workflow?") {
        Some(PromptKind::Workflow)
    } else if t.starts_with("Accessing workspace") {
        Some(PromptKind::Trust)
    } else {
        None
    }
}

/// `❯ 1. Yes` / `  2. No` → `(1, "Yes")`.
fn option_row(row: &str) -> Option<(u8, String)> {
    let t = row.trim_start();
    let t = t.strip_prefix('❯').map(str::trim_start).unwrap_or(t);
    let digits: String = t.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || digits.len() > 2 {
        return None;
    }
    let rest = t[digits.len()..].strip_prefix('.')?;
    if !rest.starts_with(' ') {
        return None;
    }
    Some((digits.parse().ok()?, rest.trim().to_string()))
}

/// An unnumbered option under the cursor: `❯ No, exit` at the box's own
/// column (the trust dialog, 2.1.280).
fn pointer_row(row: &str) -> bool {
    use crate::phase::leading_spaces;
    let t = row.trim_start();
    leading_spaces(row) <= 3
        && t.strip_prefix('❯')
            .is_some_and(|rest| rest.starts_with(' ') && !rest.trim().is_empty())
}

/// A box's question at the box's own column: `Do you want to proceed?`, `Do
/// you want to make this edit to lib.rs?`.
fn question_row(row: &str) -> bool {
    crate::phase::leading_spaces(row) <= 2 && row.trim_start().starts_with("Do you want")
}

/// A divider drawn among a dialog's options (the question dialog draws one
/// over `Chat about this`, per the 2.1.280 binary): `─` and nothing else.
fn divider_row(row: &str) -> bool {
    let t = row.trim();
    t.chars().count() >= 3 && t.chars().all(|c| c == '─')
}

/// The options between rows `from` and `to` (the footer), and the row the
/// box's content ends at (module header, "OPTIONS"). The options are read
/// after the box's LAST question row when it has one — the rows above it are
/// the model's words — else from `from`: the numbered rows, each with the
/// rows wrapped under it (indented past its number, before a blank row)
/// joined onto its label; or, when none is numbered, the rows of the last
/// block above `to` when that block holds the `❯` cursor, one option each.
/// Roles come from [`role_of`] only when the options are SOUND: numbered
/// `1.` to `n.` in order with nothing but blank and divider rows among and
/// under them, or unnumbered with exactly one cursor; and at most one
/// cursor either way. Otherwise every role is [`Role::Other`], so nothing
/// approves by a label the model may have written.
fn options_of(rows: &[String], from: usize, to: usize, kind: PromptKind) -> (Vec<Opt>, usize) {
    use crate::phase::leading_spaces;
    let question = (from..to).rev().find(|&i| question_row(&rows[i]));
    let start = question.map_or(from, |q| q + 1);
    let mut out: Vec<Opt> = Vec::new();
    let mut number_col: Option<usize> = None;
    // A row among or under the options that is none of theirs.
    let mut stray = false;
    for (i, r) in rows.iter().enumerate().take(to).skip(start) {
        if let Some((n, label)) = option_row(r) {
            out.push(Opt {
                n: Some(n),
                label,
                role: Role::Other,
                focused: r.trim_start().starts_with('❯'),
                row: i,
            });
            number_col = r.chars().position(|c| c.is_ascii_digit());
            continue;
        }
        let t = r.trim();
        match (number_col, out.last_mut()) {
            (Some(col), Some(last)) if !t.is_empty() && leading_spaces(r) > col => {
                last.label.push(' ');
                last.label.push_str(t);
            }
            _ => {
                number_col = None;
                stray |= !out.is_empty() && !t.is_empty() && !divider_row(r);
            }
        }
    }
    let (sound, content_end) = if let Some(first) = out.first().map(|o| o.row) {
        let in_order = out
            .iter()
            .enumerate()
            .all(|(k, o)| o.n.map(usize::from) == Some(k + 1));
        (!stray && in_order, question.unwrap_or(first))
    } else {
        // Unnumbered: the last block above the footer, if it holds the cursor.
        let block: Vec<usize> = (start..to)
            .rev()
            .skip_while(|&i| rows[i].trim().is_empty())
            .take_while(|&i| !rows[i].trim().is_empty())
            .collect();
        match block.last() {
            Some(&top) if block.iter().any(|&j| pointer_row(&rows[j])) => {
                out = block
                    .iter()
                    .rev()
                    .filter(|&&i| !divider_row(&rows[i]))
                    .map(|&i| {
                        let t = rows[i].trim();
                        Opt {
                            n: None,
                            label: t.trim_start_matches('❯').trim().to_string(),
                            role: Role::Other,
                            focused: t.starts_with('❯'),
                            row: i,
                        }
                    })
                    .collect();
                (true, question.unwrap_or(top))
            }
            _ => (false, question.unwrap_or(to)),
        }
    };
    let sound = sound && out.iter().filter(|o| o.focused).count() <= 1;
    if sound {
        for o in &mut out {
            o.role = role_of(&o.label, kind);
        }
    }
    (out, content_end)
}

/// The role of an option labelled `label` (module header, "PROMPT V2"): the
/// label table, measured on 2.1.267–2.1.280 boxes. A label it does not know
/// is [`Role::Other`].
fn role_of(label: &str, kind: PromptKind) -> Role {
    let l = label.replace('’', "'").to_lowercase();
    let no = l == "no" || l.starts_with("no,") || l.starts_with("no ");
    if l.starts_with("no, exit") || l == "exit" || l == "quit" {
        Role::Exit
    } else if no {
        Role::Deny
    } else if kind == PromptKind::Trust && l.starts_with("yes, i trust this folder") {
        Role::Trust
    } else if !l.starts_with("yes") {
        Role::Other
    } else if [
        "switch to",
        "(shift+tab)",
        "auto mode",
        "accept edits",
        "bypass permissions",
    ]
    .iter()
    .any(|k| l.contains(k))
    {
        Role::ModeSwitch
    } else if l.contains("don't ask again") || l.contains("always allow") {
        Role::Persist
    } else if l.contains("this session") {
        Role::Session
    } else if l == "yes" || l == "yes, proceed" || l == "yes, run it" {
        Role::Once
    } else {
        Role::Other
    }
}

/// The footer's Esc hint and what it does: a trust dialog's cancel exits
/// the program; a permission box's `Esc to cancel` (or `reject`, `deny`)
/// refuses; `exit`/`quit` exits; any other verb (`go back`, `close`) leaves
/// the dialog.
fn cancel_of(footer: &str, kind: PromptKind) -> Option<Cancel> {
    let (key, verb) = footer
        .trim()
        .split(" · ")
        .filter_map(hint_item)
        .find(|(key, _)| *key == "Esc")?;
    let v = verb.to_lowercase();
    let effect = if kind == PromptKind::Trust {
        CancelEffect::Exit
    } else if ["cancel", "reject", "deny"]
        .iter()
        .any(|k| v.starts_with(k))
    {
        CancelEffect::Reject
    } else if v.starts_with("exit") || v.starts_with("quit") {
        CancelEffect::Exit
    } else {
        CancelEffect::Back
    };
    Some(Cancel {
        key: key.to_string(),
        verb: verb.to_string(),
        effect,
    })
}

/// Screens measured on Claude Code 2.1.x, as rows — the fixtures this crate's
/// own tests and `aterm-agent`'s supervisor tests read phases and prompts from.
/// Always compiled (they are a few `Vec<String>` builders) so a dependent
/// crate's `#[cfg(test)]` code can reach them without a feature; not part of
/// the documented API.
#[doc(hidden)]
pub mod fixtures {
    /// A saved `text --json` capture of a worker mid-turn (`✶ Deliberating…`,
    /// nothing archived yet). The report tests in `aterm-agent` read it too.
    pub const WAIT_BG2: &str = include_str!("fixtures/wait_bg2.out");
    /// A saved capture whose head has scrolled off.
    pub const WAIT_BG3: &str = include_str!("fixtures/wait_bg3.out");
    /// A saved capture with the session survey parked under the done row.
    pub const WAIT_BG7: &str = include_str!("fixtures/wait_bg7.out");
    /// A saved capture of a worker idle at its composer after a limit notice
    /// and a `/model` switch.
    pub const IDLE_AFTER_LIMIT_AND_MODEL_SWITCH: &str =
        include_str!("fixtures/idle-after-limit-and-model-switch.txt");

    // Screens with their provenance on line 1 (`# <program> <version> · …`;
    // `HAND-BUILT` there when the rows were assembled, not captured). Read
    // them with [`screen`].

    /// Claude Code 2.1.280: a Bash box for `touch x`, the description row
    /// `Create file x` under the command (measured 2026-09-23).
    pub const BOX_BASH_TOUCH: &str = include_str!("fixtures/claude-2.1.280-box-bash-touch.txt");
    /// The same box with the tool row's `⏺` blinked off.
    pub const BOX_BASH_TOUCH_BLINK: &str =
        include_str!("fixtures/claude-2.1.280-box-bash-touch-blink.txt");
    /// Claude Code 2.1.280: an Edit box.
    pub const BOX_EDIT: &str = include_str!("fixtures/claude-2.1.280-box-edit.txt");
    /// Claude Code 2.1.280, bypass mode: the rm circuit-breaker box.
    pub const BOX_RM: &str = include_str!("fixtures/claude-2.1.280-box-rm.txt");
    /// Claude Code 2.1.281, bypass mode: the rm circuit-breaker box with the
    /// auto-deny countdown row under its note (the live E2E recorder: blank
    /// rows dropped).
    pub const BOX_RM_AUTO_DENY: &str = include_str!("fixtures/claude-2.1.281-box-rm-auto-deny.txt");
    /// Claude Code 2.1.280: the folder-trust dialog.
    pub const TRUST: &str = include_str!("fixtures/claude-2.1.280-trust.txt");
    /// The trust dialog under three earlier `Resume this session with:` exits.
    pub const TRUST_AFTER_RESUMES: &str =
        include_str!("fixtures/claude-2.1.280-trust-after-resumes.txt");
    /// codex 0.156.1: its folder-trust gate.
    pub const CODEX_TRUST: &str = include_str!("fixtures/codex-0.156.1-trust.txt");
    /// HAND-BUILT: a turn that ended on `API Error: 529 Overloaded`.
    pub const END_529: &str = include_str!("fixtures/hand-built-529-end-of-turn.txt");
    /// HAND-BUILT: the same turn ending on the session limit.
    pub const END_SESSION_LIMIT: &str =
        include_str!("fixtures/hand-built-session-limit-end-of-turn.txt");
    /// HAND-BUILT: the same turn ending on an offer.
    pub const END_OFFER: &str = include_str!("fixtures/hand-built-offer-end-of-turn.txt");
    /// Measured 2026-09-21 (version unrecorded): `◎ /goal active (3h)` over
    /// the frame and the dim suggestion `❯ keep going`, cursor at column 2.
    pub const GOAL_ACTIVE_SUGGESTION: &str =
        include_str!("fixtures/claude-goal-active-suggestion.txt");

    /// A fixture's rows, its provenance line dropped.
    #[must_use]
    pub fn screen(text: &str) -> Vec<String> {
        let body = match text.strip_prefix("# ") {
            Some(rest) => rest.split_once('\n').map_or("", |(_, b)| b),
            None => text,
        };
        body.lines().map(str::to_string).collect()
    }

    /// A fixture's provenance line (`claude-code 2.1.280 · MEASURED …`).
    #[must_use]
    pub fn provenance(text: &str) -> Option<&str> {
        text.strip_prefix("# ")?.lines().next()
    }

    /// The composer + footer every idle-or-prompt screen ends with (auto mode
    /// off): the separator, the caret row, the separator, the hint row.
    #[must_use]
    pub fn composer(footer: &str) -> Vec<String> {
        vec![
            "─".repeat(120),
            "❯".to_string(),
            "─".repeat(120),
            footer.to_string(),
        ]
    }

    #[must_use]
    pub fn rows(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    /// A one-row Bash box, permission mode (four options).
    #[must_use]
    pub fn bash_one_row() -> Vec<String> {
        let mut r = rows(&[
            "⏺ Let me look at the recent history.",
            "",
            " Bash command",
            "",
            "   git log --oneline -5",
            "   Show the five most recent commits",
            "",
            " Do you want to proceed?",
            " ❯ 1. Yes",
            "   2. Yes, and don’t ask again for: git log *",
            "   3. Yes, and switch to auto mode · Claude edits, runs, and asks only for the risky ones",
            "   4. No",
            "",
            " Esc to cancel · Tab to amend",
        ]);
        r.extend(composer("  ? for shortcuts"));
        r
    }

    /// A multi-row Bash box from a workflow, auto mode (three options), with a
    /// note row and a tip row.
    #[must_use]
    pub fn bash_multi_row() -> Vec<String> {
        let mut r = rows(&[
            " Bash command · from the \"verify-merge\" workflow",
            " Tip: auto mode approves reads for you",
            "",
            "   │ cd ~/ay && git status --short --branch && git pull 2>&1 | tail",
            "   │ -20",
            "   Sync the checkout before verifying",
            "",
            " This command requires approval",
            " Do you want to proceed?",
            " ❯ 1. Yes",
            "   2. Yes, and don’t ask again for: git pull *",
            "   3. No",
            "",
            " Esc to cancel · Tab to amend",
        ]);
        r.extend(composer("  ⏵⏵ auto mode on (shift+tab to cycle)"));
        r
    }

    /// A multi-row Bash box from a workflow on Claude Code 2.1.278 (measured
    /// 2026-09-21 in a live aterm tab; the command rows are shortened): the
    /// description row is led by `│` like the command rows, and a `│`-led
    /// note block (the critical-path removal warning) sits between the block
    /// and the question. Bypass mode: two options.
    #[must_use]
    pub fn bash_multi_row_with_note() -> Vec<String> {
        let mut r = rows(&[
            " Bash command · from the \"trust-branch-assessment\" workflow",
            "",
            "   │ cd /work/trust-vc && S=/tmp/scratch &&",
            "   │ grep -h edition <(git show origin/main:Cargo.toml) <(git show origin/main:crates/trust-vc-core/Cargo.toml) <(git show 630604f8:Cargo.toml) 2>/dev/null | sort",
            "   │ | uniq -c; ED=$(git show origin/main:Cargo.toml | grep -m1 edition | grep -o '20[0-9][0-9]'); ED=${ED:-2021}; echo \"edition=$ED\"; for pair in \"t_mb 630604f8\"",
            "   │ \"t_sv salvage/overlay-raw-20260721\" \"t_om origin/main\"; do set -- $pair; rm -rf $S/$1; mkdir -p $S/$1; git archive $2 | tar -x -C $S/$1; done; ls $S/t_sv |",
            "   │ $S/fmt_sv_om.txt; wc -l $S/fmt_sv_om.txt; diff -rq t_mb t_sv | grep -v '^Only' | head; echo \"=== files semantically changed salvage vs MB (post-fmt) ===\"; sed",
            "   │ 's/^Files \\(.*\\) and .* differ$/\\1/' $S/fmt_mb_sv.txt | sed 's#^t_mb/##'",
            "   │ Extract three trees to scratch, format them identically, and diff for real content changes",
            "",
            " │ Dangerous rm operation on possibly-empty variable path: $S/$1 in `rm -rf $S/$1` (bind $1 and rewrite its $S as \"${S:?}\" or use a literal path)",
            "",
            " Do you want to proceed?",
            " ❯ 1. Yes",
            "   2. No",
            "",
            " Esc to cancel · Tab to amend",
        ]);
        r.extend(composer(
            "  ⏵⏵ bypass permissions on · 1 shell · ← for agents · ↓ to manage",
        ));
        r
    }

    #[must_use]
    pub fn workflow_box() -> Vec<String> {
        let mut r = rows(&[
            " Run a dynamic workflow?",
            "  │ Fan out the 12 benchmark families to 4 agents and collect the",
            "  │ standings table.",
            "",
            "  ❯ 1. Yes, run it",
            "    2. View raw script",
            "    3. No",
            "",
            "  Esc to cancel · Tab to amend",
        ]);
        r.extend(composer("  ? for shortcuts"));
        r
    }

    #[must_use]
    pub fn edit_box() -> Vec<String> {
        let mut r = rows(&[
            " Edit file",
            "",
            "   crates/ay-test-support/src/lib.rs",
            "",
            "   2158    -    let cargo = \"cargo\";",
            "   2158    +    let cargo = std::env::var(\"CARGO\").unwrap_or_else(|_| \"cargo\".into());",
            "",
            " Do you want to make this edit to lib.rs?",
            " ❯ 1. Yes",
            "   2. Yes, allow all edits during this session (shift+tab)",
            "   3. No",
            "",
            " Esc to cancel · Tab to amend",
        ]);
        r.extend(composer("  ? for shortcuts"));
        r
    }

    /// A ` Read file(s)` box for a path outside the working directory.
    #[must_use]
    pub fn read_box() -> Vec<String> {
        let mut r = rows(&[
            " Read file(s)",
            "",
            "   ~/.ssh/config",
            "",
            " Claude wants to read a file outside the current working directory",
            " Do you want to proceed?",
            " ❯ 1. Yes",
            "   2. Yes, and allow reading from ~/.ssh during this session",
            "   3. No",
            "",
            " Esc to cancel · Tab to amend",
        ]);
        r.extend(composer("  ? for shortcuts"));
        r
    }

    /// HAND-BUILT, and NOT Claude Code's trust dialog: an invented `1. Yes,
    /// proceed / 2. No, exit` box that reads as kind `other`. The measured
    /// 2.1.280 dialog is [`TRUST`] (`Accessing workspace:`, unnumbered
    /// options). Kept only because `aterm-agent`'s escalation test pins this
    /// box's first row; that test moves to [`TRUST`] and this goes.
    #[must_use]
    pub fn trust_box() -> Vec<String> {
        let mut r = rows(&[
            " Do you trust the files in this folder?",
            "",
            "   /Users//x/proj",
            "",
            " Reading untrusted files may lead Claude Code to behave unexpectedly.",
            "",
            " ❯ 1. Yes, proceed",
            "   2. No, exit",
            "",
            " Esc to cancel",
        ]);
        r.extend(composer("  ? for shortcuts"));
        r
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    /// The box's own first row: the header where there is one, and for a headerless
    /// box the first row under whatever above it is not the box's — never a transcript
    /// row, however tall the transcript (round-25 review, 2026-09-23).
    #[test]
    fn a_boxs_first_row_is_its_own_never_a_transcript_row() {
        assert_eq!(prompt_box_first_row(&bash_one_row()), Some(2));
        assert_eq!(prompt_box_first_row(&bash_multi_row_with_note()), Some(0));
        let question = " Do you trust the files in this folder?";
        // Alone on the screen, under a 40-row transcript, under a rule, and under a
        // spinner's done row: the question row every time.
        let transcript: Vec<String> = (0..40)
            .map(|i| format!("⏺ transcript row {i} done"))
            .collect();
        let under = |above: Vec<String>| {
            let mut r = above;
            r.push(String::new());
            r.extend(trust_box());
            r
        };
        for screen in [
            trust_box(),
            under(transcript.clone()),
            under(vec!["─".repeat(80)]),
            under(vec!["╰".to_string() + &"─".repeat(40) + "╯"]),
            under(vec!["✻ Worked for 3m 21s · done 8:50 PM".to_string()]),
            under(vec!["  ⎿  output under the gutter".to_string()]),
        ] {
            let first = prompt_box_first_row(&screen).expect("a live box");
            assert_eq!(screen[first], question, "{screen:?}");
        }
        // A bare `│` row inside a headerless box is the box's, not a rule above it.
        let mut quoted = rows(&[
            "⏺ Here is the plan.",
            "",
            " Would you like to proceed with this plan?",
            "   │ step one",
            "   │",
            "   │ step two",
            "",
            " ❯ 1. Yes",
            "   2. No",
            "",
            " Esc to cancel",
        ]);
        quoted.extend(composer("  ? for shortcuts"));
        assert_eq!(prompt_box_first_row(&quoted), Some(2));
        // The span shares the box's own first row since the footer-shaped
        // box reader replaced the fixed 30-row window, which landed in the
        // transcript for the tall one.
        let tall = under(transcript);
        let (a, _) = prompt_box_span(&tall).unwrap();
        assert_eq!(tall[a], question, "the span starts at the box's own row");
        // No box, no row.
        assert_eq!(prompt_box_first_row(&rows(&["⏺ hello"])), None);
    }

    #[test]
    fn a_one_row_bash_box_parses_command_description_and_four_options() {
        let p = parse_prompt(&bash_one_row()).expect("a prompt");
        assert_eq!(p.kind, PromptKind::Bash);
        assert_eq!(p.command, "git log --oneline -5");
        assert_eq!(p.description, "Show the five most recent commits");
        assert_eq!(p.options.len(), 4, "{:?}", p.options);
        assert_eq!(p.options[0], (1, "Yes".to_string()));
        assert_eq!(
            p.options[1],
            (2, "Yes, and don’t ask again for: git log *".to_string())
        );
        assert!(p.options[2].1.starts_with("Yes, and switch to auto mode"));
        assert_eq!(p.options[3], (4, "No".to_string()));
        assert!(p.notes.is_empty(), "{:?}", p.notes);
        assert_eq!(prompt_box_span(&bash_one_row()), Some((2, 13)));
    }

    /// `│`-led rows are ONE command joined with single spaces (the wrapped
    /// `│ -20` is a flag, not a description); the workflow suffix on the
    /// header and the tip are not content; the bare note row under the block
    /// is a note; auto mode has three options.
    #[test]
    fn a_multi_row_bash_box_joins_the_bars_skips_tips_and_reads_the_note() {
        let p = parse_prompt(&bash_multi_row()).expect("a prompt");
        assert_eq!(p.kind, PromptKind::Bash);
        assert_eq!(
            p.command,
            "cd ~/ay && git status --short --branch && git pull 2>&1 | tail -20"
        );
        assert_eq!(p.description, "Sync the checkout before verifying");
        assert_eq!(p.notes, vec!["This command requires approval".to_string()]);
        assert_eq!(
            p.options,
            vec![
                (1, "Yes".to_string()),
                (2, "Yes, and don’t ask again for: git pull *".to_string()),
                (3, "No".to_string()),
            ]
        );
    }

    /// The 2.1.278 shape (module header): the `│`-led description row is the
    /// description, not the command's tail; the `│`-led note block is one
    /// note; bypass mode has two options.
    #[test]
    fn a_bar_led_description_and_a_bar_led_note_are_read_as_such() {
        let p = parse_prompt(&bash_multi_row_with_note()).expect("a prompt");
        assert_eq!(p.kind, PromptKind::Bash);
        assert!(
            p.command.starts_with("cd /work/trust-vc && S="),
            "{}",
            p.command
        );
        assert!(p.command.ends_with("sed 's#^t_mb/##'"), "{}", p.command);
        assert!(!p.command.contains("Extract three trees"), "{}", p.command);
        assert_eq!(
            p.description,
            "Extract three trees to scratch, format them identically, and diff for real content changes"
        );
        assert_eq!(p.notes.len(), 1, "{:?}", p.notes);
        assert!(
            p.notes[0].starts_with("Dangerous rm operation on possibly-empty variable path"),
            "{}",
            p.notes[0]
        );
        assert!(
            p.notes[0].ends_with("or use a literal path)"),
            "{}",
            p.notes[0]
        );
        assert_eq!(
            p.options,
            vec![(1, "Yes".to_string()), (2, "No".to_string())]
        );
        assert_eq!(prompt_box_span(&bash_multi_row_with_note()), Some((0, 16)));
    }

    /// A last `│` row that looks like shell stays in the command: a pipe, a
    /// wrapped flag, a lowercase word, two words. A single `│` row is always
    /// the command.
    #[test]
    fn a_last_bar_row_that_reads_as_shell_stays_in_the_command() {
        let boxed = |last: &str| {
            let mut r = rows(&[
                " Bash command",
                "",
                "   │ cd ~/ay && git log --oneline",
                "   │ -5 | sort",
                &format!("   │ {last}"),
                "",
                " Do you want to proceed?",
                " ❯ 1. Yes",
                "   2. No",
                "",
                " Esc to cancel · Tab to amend",
            ]);
            r.extend(composer("  ? for shortcuts"));
            parse_prompt(&r).expect("a prompt")
        };
        for shell in [
            "| uniq -c",
            "-20 | tail",
            "echo Done with it",
            "Two words",
            "Three words $HERE",
            "Ends the (command)",
        ] {
            let p = boxed(shell);
            assert_eq!(
                p.command,
                format!("cd ~/ay && git log --oneline -5 | sort {shell}"),
                "{shell}"
            );
            assert_eq!(p.description, "", "{shell}");
        }
        let p = boxed("Show the recent commits");
        assert_eq!(p.command, "cd ~/ay && git log --oneline -5 | sort");
        assert_eq!(p.description, "Show the recent commits");

        assert!(reads_as_prose("Extract three trees to scratch"));
        assert!(!reads_as_prose("-20"));
        assert!(!reads_as_prose("extract three trees"));
        assert!(!reads_as_prose("Extract trees"));
        assert!(!reads_as_prose("Extract $S trees now"));

        let mut one = rows(&[
            " Bash command",
            "",
            "   │ Make the thing now",
            "",
            " Do you want to proceed?",
            " ❯ 1. Yes",
            "   2. No",
            "",
            " Esc to cancel · Tab to amend",
        ]);
        one.extend(composer("  ? for shortcuts"));
        let p = parse_prompt(&one).expect("a prompt");
        assert_eq!(p.command, "Make the thing now");
        assert_eq!(p.description, "");
    }

    #[test]
    fn a_workflow_box_is_kind_workflow_with_its_description() {
        let p = parse_prompt(&workflow_box()).expect("a prompt");
        assert_eq!(p.kind, PromptKind::Workflow);
        assert_eq!(p.command, "");
        assert_eq!(
            p.description,
            "Fan out the 12 benchmark families to 4 agents and collect the standings table."
        );
        assert_eq!(p.options[0], (1, "Yes, run it".to_string()));
        assert_eq!(p.options[1], (2, "View raw script".to_string()));
        assert_eq!(p.options[2], (3, "No".to_string()));
    }

    #[test]
    fn an_edit_box_is_kind_edit_with_the_path() {
        let p = parse_prompt(&edit_box()).expect("a prompt");
        assert_eq!(p.kind, PromptKind::Edit);
        assert_eq!(p.command, "crates/ay-test-support/src/lib.rs");
        assert!(p.notes.is_empty(), "a diff is not a note: {:?}", p.notes);
        assert_eq!(p.options.len(), 3);
    }

    /// An unknown box that still says `Esc to cancel` is kind Other, carrying the
    /// surrounding rows so the manager can read it; no cancel row → no prompt.
    #[test]
    fn an_unknown_box_is_other_and_a_plain_screen_is_none() {
        let mut r = rows(&[
            "⏺ Done.",
            "",
            " Select a model",
            " ❯ 1. Opus",
            "   2. Sonnet",
            "",
            " Esc to cancel",
        ]);
        r.extend(composer("  ? for shortcuts"));
        let p = parse_prompt(&r).expect("a prompt");
        assert_eq!(p.kind, PromptKind::Other);
        assert!(p.description.contains("Select a model"));
        assert_eq!(
            p.options,
            vec![(1, "Opus".to_string()), (2, "Sonnet".to_string())]
        );

        let mut idle = rows(&["⏺ Done.", "", "✻ Cogitated for 2m 54s · done 2:41 PM", ""]);
        idle.extend(composer("  ? for shortcuts"));
        assert_eq!(parse_prompt(&idle), None);
    }

    // ---- measured 2.1.280 screens, Prompt v2 -------------------------------

    /// Every fixture file this crate added names its program and version on
    /// its first line, and says HAND-BUILT when its rows were assembled.
    #[test]
    fn every_new_fixture_names_its_program_and_version_on_line_one() {
        for (text, want) in [
            (BOX_BASH_TOUCH, "claude-code 2.1.280 · MEASURED"),
            (BOX_BASH_TOUCH_BLINK, "claude-code 2.1.280 · MEASURED"),
            (BOX_EDIT, "claude-code 2.1.280 · MEASURED"),
            (BOX_RM, "claude-code 2.1.280 · MEASURED"),
            (TRUST, "claude-code 2.1.280 · MEASURED"),
            (TRUST_AFTER_RESUMES, "claude-code 2.1.280 · MEASURED"),
            (CODEX_TRUST, "codex 0.156.1 · MEASURED"),
            (END_529, "claude-code (unrecorded) · HAND-BUILT"),
            (END_SESSION_LIMIT, "claude-code (unrecorded) · HAND-BUILT"),
            (END_OFFER, "claude-code (unrecorded) · HAND-BUILT"),
            (
                GOAL_ACTIVE_SUGGESTION,
                "claude-code (version unrecorded) · MEASURED",
            ),
        ] {
            let line = provenance(text).expect("a provenance line");
            assert!(line.starts_with(want), "{line}");
            assert!(!screen(text)[0].starts_with("# "), "{line}");
        }
    }

    /// The 2.1.280 Bash box for `touch x`: its description row reads `Create
    /// file x`, and the old nearest-header scan took that row for the header
    /// (kind write, no command — measured). The header is the box's first
    /// row; the command and description are read under it. The `⏺` tool row
    /// above blinks (measured); both frames read the same. Option 2 wraps
    /// onto the row under it; option 3 is drawn `3. Nooject` in the grid
    /// (the garbled row, measured) and gets no role.
    #[test]
    fn the_touch_box_is_bash_whatever_its_description_says() {
        for text in [BOX_BASH_TOUCH, BOX_BASH_TOUCH_BLINK] {
            let r = screen(text);
            let p = parse_prompt_v2(&r).expect("the box");
            assert_eq!(p.kind, PromptKind::Bash);
            assert_eq!(p.title, "Bash command");
            assert_eq!(p.command, "touch x");
            // The description row is kept among the command's rows: the
            // screen cannot prove it is not the command's wrapped tail.
            assert_eq!(
                p.command_rows,
                vec!["touch x".to_string(), "Create file x".to_string()]
            );
            assert_eq!(p.description_rows, 1);
            assert!(!p.gutter);
            assert_eq!(p.readings(), vec!["touch x", "touch x Create file x"]);
            assert_eq!(p.description, "Create file x");
            assert_eq!(p.select, Select::Digits);
            assert_eq!(p.options.len(), 3, "{:?}", p.options);
            assert_eq!(p.options[0].label, "Yes");
            assert_eq!(p.options[0].role, Role::Once);
            assert!(p.options[0].focused);
            assert!(
                p.options[1]
                    .label
                    .starts_with("Yes, and always allow access to /private/tmp/"),
                "{}",
                p.options[1].label
            );
            assert!(p.options[1].label.ends_with("work1 from this"));
            assert_eq!(p.options[1].role, Role::Persist);
            assert!(!p.options[1].focused);
            assert_eq!(p.options[2].label, "Nooject");
            assert_eq!(p.options[2].role, Role::Other);
            assert_eq!(p.focused().map(|o| o.n), Some(Some(1)));
            let cancel = p.cancel.as_ref().expect("a cancel hint");
            assert_eq!(cancel.key, "Esc");
            assert_eq!(cancel.verb, "cancel");
            assert_eq!(cancel.effect, CancelEffect::Reject);
            assert_eq!(r[p.span.0].trim(), "Bash command");
            assert_eq!(r[p.span.1].trim(), "Esc to cancel · Tab to amend");

            let old = parse_prompt(&r).expect("the box");
            assert_eq!(old.kind, PromptKind::Bash);
            assert_eq!(old.command, "touch x");
            assert_eq!(old.options[1].1, p.options[1].label);
            assert_eq!(prompt_box_span(&r), Some(p.span));
        }
    }

    /// DERIVED from the touch capture (only the two content rows replaced;
    /// the audit saw this box live, 2026-09-23, but kept no capture of it):
    /// `wc -l /etc/hosts` described `Read file /etc/hosts` is a Bash box. A
    /// real ` Read file(s)` box stays a Read box — the control.
    #[test]
    fn a_read_file_description_does_not_make_a_bash_box_a_read_box() {
        let r: Vec<String> = screen(BOX_BASH_TOUCH)
            .into_iter()
            .map(|row| match row.as_str() {
                "   touch x" => "   wc -l /etc/hosts".to_string(),
                "   Create file x" => "   Read file /etc/hosts".to_string(),
                _ => row,
            })
            .collect();
        let p = parse_prompt_v2(&r).expect("the box");
        assert_eq!(p.kind, PromptKind::Bash);
        assert_eq!(p.command, "wc -l /etc/hosts");
        assert_eq!(p.description, "Read file /etc/hosts");
        let read = parse_prompt_v2(&read_box()).expect("the read box");
        assert_eq!(read.kind, PromptKind::Read);
        assert_eq!(read.path.as_deref(), Some("~/.ssh/config"));
        assert_eq!(read.with_role(Role::Session).map(|o| o.n), Some(Some(2)));
    }

    /// The 2.1.280 Edit box: the path at column 1 under the header, a diff
    /// between dashed rules, and option 2 switching the session to
    /// accept-edits mode.
    #[test]
    fn the_edit_box_reads_its_path_and_the_mode_switch_option() {
        let p = parse_prompt_v2(&screen(BOX_EDIT)).expect("the box");
        assert_eq!(p.kind, PromptKind::Edit);
        assert_eq!(p.title, "Edit file");
        assert_eq!(p.command, "notes.txt");
        assert_eq!(p.path.as_deref(), Some("notes.txt"));
        assert!(p.command_rows.is_empty());
        let roles: Vec<Role> = p.options.iter().map(|o| o.role).collect();
        assert_eq!(roles, vec![Role::Once, Role::ModeSwitch, Role::Deny]);
        assert_eq!(p.cancel.map(|c| c.effect), Some(CancelEffect::Reject));
    }

    /// The 2.1.281 rm circuit breaker (the live E2E of 2026-09-24, D3): the
    /// auto-deny countdown row under the warning is the box's
    /// [`PromptV2::auto_deny`], never part of the note — joined into it, the
    /// warning no longer read as the breaker's and the rm rule never ran.
    /// Negative controls: the 2.1.280 box has no countdown; the countdown
    /// after a blank row is still no second note; any other `⚠` row is.
    #[test]
    fn the_auto_deny_countdown_is_not_a_note() {
        let r = screen(BOX_RM_AUTO_DENY);
        let p = parse_prompt_v2(&r).expect("the box");
        assert_eq!(p.kind, PromptKind::Bash);
        assert_eq!(p.notes.len(), 1, "{:?}", p.notes);
        assert!(
            p.notes[0].starts_with("Dangerous rm operation on possibly-empty variable path: $S/$1")
                && p.notes[0].ends_with("\"${S:?}\" or use a literal path)"),
            "{}",
            p.notes[0]
        );
        assert_eq!(
            p.auto_deny.as_deref(),
            Some(
                "Claude Code will automatically deny this request in 1:59, to avoid blocking \
                 progress on an unattended session"
            )
        );
        assert_eq!(
            p.command_rows,
            [
                "S=/private/tmp/claude-502/scratch/e2e/work/tmp;",
                "for p in a b; do set -- $p; rm -rf $S/$1; done",
            ]
        );
        assert_eq!(parse_prompt(&r).expect("v1").notes.len(), 2);
        assert_eq!(
            p.options.iter().map(|o| o.role).collect::<Vec<_>>(),
            vec![Role::Once, Role::Deny]
        );

        assert_eq!(
            parse_prompt_v2(&screen(BOX_RM)).expect("box").auto_deny,
            None
        );
        let at = r
            .iter()
            .position(|row| row.contains("automatically deny"))
            .expect("the row");
        let mut spaced = r.clone();
        spaced.insert(at, String::new());
        let p = parse_prompt_v2(&spaced).expect("the box");
        assert_eq!(p.notes.len(), 1, "{:?}", p.notes);
        assert!(p.auto_deny.is_some());
        let mut other = r.clone();
        other[at] = " ⚠ Something else the vendor warns about".to_string();
        let p = parse_prompt_v2(&other).expect("the box");
        assert_eq!(p.auto_deny, None);
        assert!(
            p.notes[0].ends_with("⚠ Something else the vendor warns about"),
            "{:?}",
            p.notes
        );
    }

    /// The 2.1.280 rm circuit breaker, bypass mode: the command, its
    /// description, the warning that wraps over two `│` rows — one note in
    /// v2, one per row in the old shape — and two options.
    #[test]
    fn the_rm_breaker_box_reads_one_wrapped_note() {
        let r = screen(BOX_RM);
        let p = parse_prompt_v2(&r).expect("the box");
        assert_eq!(p.kind, PromptKind::Bash);
        assert_eq!(
            p.command,
            "S=$PWD/tmp; for p in a b; do set -- $p; rm -rf $S/$1; done"
        );
        assert_eq!(p.description, "Remove directories tmp/a and tmp/b");
        assert_eq!(p.notes.len(), 1, "{:?}", p.notes);
        assert!(
            p.notes[0].starts_with("Dangerous rm operation on possibly-empty variable path: $S/$1")
        );
        assert!(
            p.notes[0].ends_with("\"${S:?}\" or use a literal path)"),
            "{}",
            p.notes[0]
        );
        assert_eq!(
            p.options.iter().map(|o| o.role).collect::<Vec<_>>(),
            vec![Role::Once, Role::Deny]
        );
        assert_eq!(parse_prompt(&r).expect("the box").notes.len(), 2);
    }

    /// The 2.1.280 trust dialog (measured twice): `Accessing workspace:`, the
    /// folder, UNNUMBERED options with the cursor on `No, exit`, chosen by
    /// arrows and Enter, and an Esc that exits Claude Code. The old parser
    /// read it as kind `other` with no options.
    #[test]
    fn the_trust_dialog_reads_its_folder_options_and_exit() {
        for (text, folder) in [(TRUST, "/work1"), (TRUST_AFTER_RESUMES, "/lv/work2")] {
            let r = screen(text);
            assert_eq!(crate::phase::worker_phase(&r), crate::phase::Phase::Prompt);
            let p = parse_prompt_v2(&r).expect("the dialog");
            assert_eq!(p.kind, PromptKind::Trust);
            assert_eq!(p.title, "Accessing workspace:");
            let path = p.path.clone().expect("the folder");
            assert!(
                path.starts_with("/private/tmp/") && path.ends_with(folder),
                "{path}"
            );
            assert_eq!(p.select, Select::ArrowsEnter);
            assert_eq!(p.options.len(), 2, "{:?}", p.options);
            assert_eq!(p.options[0].label, "No, exit");
            assert_eq!(p.options[0].role, Role::Exit);
            assert!(p.options[0].focused);
            assert_eq!(p.options[0].n, None);
            assert_eq!(p.options[1].label, "Yes, I trust this folder");
            assert_eq!(p.options[1].role, Role::Trust);
            assert!(!p.options[1].focused);
            assert_eq!(p.focused().map(|o| o.label.as_str()), Some("No, exit"));
            let cancel = p.cancel.expect("the footer's Esc");
            assert_eq!(cancel.effect, CancelEffect::Exit);
            assert!(
                p.description.starts_with("Quick safety check"),
                "{}",
                p.description
            );
            // The option labels are options, not prose.
            assert!(
                p.description
                    .ends_with("Claude Code'll be able to read, edit, and execute files here."),
                "{}",
                p.description
            );
            let old = parse_prompt(&r).expect("the dialog");
            assert_eq!(old.kind, PromptKind::Trust);
            assert!(old.options.is_empty());
            // The span starts at the title, not at the shell rows above it.
            assert_eq!(
                prompt_box_span(&r).map(|(a, _)| r[a].trim()),
                Some("Accessing workspace:")
            );
        }
    }

    /// NOT A BOX: a worker's message that says `press Esc to cancel`, a
    /// bullet quoting it, a zsh screen after `grep 'Esc to cancel'`, and a
    /// box quoted straight on under a `⏺` row. The control: the same footer
    /// under real option rows is a box.
    #[test]
    fn words_that_quote_the_footer_are_not_a_box() {
        let said = {
            let mut r = rows(&[
                "⏺ The watcher is armed. When the box appears, press Esc to cancel it or 1 to approve.",
                "",
            ]);
            r.extend(composer("  ? for shortcuts"));
            r
        };
        let bullet = {
            let mut r = rows(&[
                "⏺ Two ways out:",
                "  - 1 to approve",
                "  - Esc to cancel the run",
                "",
            ]);
            r.extend(composer("  ? for shortcuts"));
            r
        };
        let zsh = rows(&[
            "% grep -rn 'Esc to cancel' crates/aterm-phase/src | head -2",
            "crates/aterm-phase/src/prompt.rs:397:            \" Esc to cancel · Tab to amend\",",
            "crates/aterm-phase/src/prompt.rs:421:            \" Esc to cancel · Tab to amend\",",
            "% ",
        ]);
        let quoted = {
            let mut r = rows(&[
                "⏺ The worker's box read:",
                "   Do you want to proceed?",
                "   ❯ 1. Yes",
                "     2. No",
                "   Esc to cancel · Tab to amend",
                "",
            ]);
            r.extend(composer("  ? for shortcuts"));
            r
        };
        let no_options = {
            let mut r = rows(&["⏺ Done.", "", " Esc to cancel", ""]);
            r.extend(composer("  ? for shortcuts"));
            r
        };
        for (name, r) in [
            ("said", &said),
            ("bullet", &bullet),
            ("zsh", &zsh),
            ("quoted", &quoted),
            ("no options", &no_options),
        ] {
            assert_eq!(parse_prompt(r), None, "{name}");
            assert_eq!(parse_prompt_v2(r), None, "{name}");
            assert_ne!(
                crate::phase::worker_phase(r),
                crate::phase::Phase::Prompt,
                "{name}"
            );
        }
        assert!(is_hint_footer(" Esc to cancel · Tab to amend"));
        assert!(is_hint_footer(" Enter to confirm · Esc to cancel"));
        assert!(is_hint_footer("  Esc to go back"));
        assert!(!is_hint_footer("  esc to interrupt"));
        assert!(!is_hint_footer(
            "⚠ Usage limit reached · continuing shortly · esc to cancel"
        ));
        assert!(!is_hint_footer("      Esc to cancel · Tab to amend"));
        assert!(!is_hint_footer(" Tab to amend"));
    }

    /// A box with no known header is `other`, titled by its own first row
    /// however much transcript sits above it (the old span started 30 rows
    /// above the footer, on whatever row was there); an `Esc to go back`
    /// dialog backs out; an ` Overwrite file` box (the 2.1.280 binary's
    /// title; no capture) is its own kind.
    #[test]
    fn an_unknown_box_is_titled_by_its_first_row_and_backs_out() {
        let mut r: Vec<String> = (0..40).map(|i| format!("⏺ transcript line {i}")).collect();
        r.extend(rows(&[
            "",
            " Select a model",
            " ❯ 1. Opus",
            "   2. Sonnet",
            "",
            " Enter to select · Esc to go back",
        ]));
        let p = parse_prompt_v2(&r).expect("a box");
        assert_eq!(p.kind, PromptKind::Other);
        assert_eq!(p.title, "Select a model");
        assert_eq!(p.cancel.map(|c| c.effect), Some(CancelEffect::Back));
        assert_eq!(prompt_box_span(&r).map(|(a, _)| a), Some(41));
        assert!(p.options.iter().all(|o| o.role == Role::Other));

        let mut o = rows(&[
            " Overwrite file",
            "",
            "   src/x.rs",
            "",
            " Do you want to overwrite x.rs?",
            " ❯ 1. Yes",
            "   2. No",
            "",
            " Esc to cancel · Tab to amend",
        ]);
        o.extend(composer("  ? for shortcuts"));
        let p = parse_prompt_v2(&o).expect("the box");
        assert_eq!(p.kind, PromptKind::Overwrite);
        assert_eq!(p.path.as_deref(), Some("src/x.rs"));
    }

    /// The role table, label by label, and v1/v2 agreeing on every fixture.
    #[test]
    fn roles_come_from_the_label_table_and_the_two_parsers_agree() {
        let k = PromptKind::Bash;
        for (label, role) in [
            ("Yes", Role::Once),
            ("Yes, proceed", Role::Once),
            ("Yes, run it", Role::Once),
            ("Yes, and don’t ask again for: git log *", Role::Persist),
            (
                "Yes, and always allow access to /w from this project",
                Role::Persist,
            ),
            (
                "Yes, and allow reading from ~/.ssh during this session",
                Role::Session,
            ),
            (
                "Yes, allow all edits during this session (shift+tab)",
                Role::ModeSwitch,
            ),
            (
                "Yes, and switch to auto mode · Claude edits, runs",
                Role::ModeSwitch,
            ),
            ("No", Role::Deny),
            ("No, and tell Claude what to do differently", Role::Deny),
            ("No, exit", Role::Exit),
            ("View raw script", Role::Other),
            ("Nooject", Role::Other),
            ("Yes, I trust this folder", Role::Other),
        ] {
            assert_eq!(role_of(label, k), role, "{label}");
        }
        assert_eq!(
            role_of("Yes, I trust this folder", PromptKind::Trust),
            Role::Trust
        );

        for r in [
            bash_one_row(),
            bash_multi_row(),
            bash_multi_row_with_note(),
            workflow_box(),
            edit_box(),
            read_box(),
            screen(BOX_BASH_TOUCH),
            screen(BOX_EDIT),
            screen(BOX_RM),
            screen(TRUST),
        ] {
            let (a, b) = (
                parse_prompt(&r).expect("v1"),
                parse_prompt_v2(&r).expect("v2"),
            );
            assert_eq!(a.kind, b.kind);
            assert_eq!(a.command, b.command);
            assert_eq!(a.description, b.description);
            let numbered: Vec<(u8, String)> = b
                .options
                .iter()
                .filter_map(|o| o.n.map(|n| (n, o.label.clone())))
                .collect();
            assert_eq!(a.options, numbered);
            assert_eq!(b.span, prompt_box_span(&r).expect("span"));
        }
    }

    /// A Bash box: its title, the given content rows, the question, the
    /// given option rows and the footer.
    fn bash_box(command_rows: &[&str], options: &[&str]) -> Vec<String> {
        let mut r = rows(&[" Bash command", ""]);
        r.extend(rows(command_rows));
        r.extend(rows(&["", " Do you want to proceed?"]));
        r.extend(rows(options));
        r.extend(rows(&["", " Esc to cancel · Tab to amend"]));
        r
    }

    const PERSIST_OPTIONS: [&str; 3] = [
        " ❯ 1. Yes",
        "   2. Yes, and always allow access to /Users//x/proj from this project",
        "   3. No",
    ];

    /// REVIEW BLOCKER 1: a model-written description row that reads `2. Yes`
    /// is not option 2 — pressing 2 there grants `always allow`. Options are
    /// read under the question; and when the numbers repeat or run out of
    /// order, no option gets a role. The control: the same box without the
    /// description row reads `Yes` as option 1, role once.
    #[test]
    fn a_description_that_reads_as_an_option_never_becomes_one() {
        let r = bash_box(&["   ls", "   2. Yes"], &PERSIST_OPTIONS);
        let p = parse_prompt_v2(&r).expect("the box");
        let numbered: Vec<(Option<u8>, &str, Role)> = p
            .options
            .iter()
            .map(|o| (o.n, o.label.as_str(), o.role))
            .collect();
        assert_eq!(numbered.len(), 3, "{numbered:?}");
        assert_eq!(p.with_role(Role::Once).map(|o| o.n), Some(Some(1)));
        assert_eq!(p.with_role(Role::Persist).map(|o| o.n), Some(Some(2)));
        assert_eq!(parse_prompt(&r).expect("v1").options.len(), 3);
        // The description row is still visible to a decider.
        assert!(p.readings().iter().any(|x| x.contains("2. Yes")));

        // No question row, so the description row is read with the options:
        // the numbers run 2, 1, 2, 3 and no option has a role.
        let mut noq = r.clone();
        noq.retain(|row| row.trim() != "Do you want to proceed?");
        let p = parse_prompt_v2(&noq).expect("the box");
        assert!(p.options.iter().any(|o| o.n == Some(2) && o.label == "Yes"));
        assert!(
            p.options.iter().all(|o| o.role == Role::Other),
            "{:?}",
            p.options
        );
        assert_eq!(p.with_role(Role::Once), None);

        // Repeated numbers and a stray row among the options: no roles.
        for options in [
            &[" ❯ 1. Yes", "   1. Yes", "   2. No"][..],
            &[" ❯ 1. Yes", "   3. No"],
            &[" ❯ 1. Yes", " ❯ 2. No"],
            &[" ❯ 1. Yes", " Stray words", "   2. No"],
        ] {
            let p =
                parse_prompt_v2(&bash_box(&["   ls", "   List files"], options)).expect("the box");
            assert!(
                p.options.iter().all(|o| o.role == Role::Other),
                "{options:?}: {:?}",
                p.options
            );
        }
        // The control: sound options get their roles.
        let p = parse_prompt_v2(&bash_box(&["   ls", "   List files"], &PERSIST_OPTIONS))
            .expect("the box");
        let roles: Vec<Role> = p.options.iter().map(|o| o.role).collect();
        assert_eq!(roles, vec![Role::Once, Role::Persist, Role::Deny]);
    }

    /// REVIEW BLOCKER 2: no row that may run is left out of what a decider
    /// reads. A `│` row that reads as prose (`HOME=/tmp rm -rf ~/work`,
    /// `Xargs rm -rf build out dist`) is SHOWN as the description but stays
    /// a command row; without bars every row at the command's column is a
    /// command row — a wrapped tail, a row after a blank row, a `Tip:` at the
    /// command's column. Each reading of the command is listed.
    #[test]
    fn every_row_that_may_run_is_in_the_readings() {
        let two = [" ❯ 1. Yes", "   2. No"];
        for (cmd, tail) in [
            (
                &["   │ cat README.md", "   │ HOME=/tmp rm -rf ~/work"][..],
                "rm -rf ~/work",
            ),
            (
                &[
                    "   │ find . -name '*.tmp' -print |",
                    "   │ Xargs rm -rf build out dist",
                ],
                "Xargs rm",
            ),
            (
                &["   cat README.md", "   HOME=/tmp rm -rf ~/work"],
                "rm -rf",
            ),
            (&["   cat README.md", "", "   ; rm -rf ~/work"], "; rm -rf"),
            (
                &["   cat README.md", "   Tip: x; rm -rf ~/work"],
                "; rm -rf",
            ),
        ] {
            let p = parse_prompt_v2(&bash_box(cmd, &two)).expect("the box");
            assert_eq!(p.kind, PromptKind::Bash);
            assert!(
                p.command_rows.iter().any(|r| r.contains(tail)),
                "{cmd:?}: {:?}",
                p.command_rows
            );
            assert!(
                p.readings().iter().any(|r| r.contains(tail)),
                "{cmd:?}: {:?}",
                p.readings()
            );
        }
        // The bar case: shown as the description, read as the command.
        let p = parse_prompt_v2(&bash_box(
            &["   │ cat README.md", "   │ HOME=/tmp rm -rf ~/work"],
            &two,
        ))
        .expect("the box");
        assert!(p.gutter);
        assert_eq!(p.command, "cat README.md");
        assert_eq!(p.description, "HOME=/tmp rm -rf ~/work");
        assert_eq!(p.description_rows, 1);
        assert_eq!(
            p.readings(),
            vec![
                "cat README.md",
                "cat README.md HOME=/tmp rm -rf ~/work",
                "cat README.md\nHOME=/tmp rm -rf ~/work",
            ]
        );
        // The measured 2.1.278 shape keeps its prose row among the command
        // rows too.
        let p = parse_prompt_v2(&bash_multi_row_with_note()).expect("the box");
        assert!(
            p.command_rows
                .last()
                .is_some_and(|r| r.starts_with("Extract three trees")),
            "{:?}",
            p.command_rows
        );
        assert!(
            p.readings()
                .iter()
                .any(|r| r.ends_with("real content changes"))
        );
        // The negative control: the notes are left of the command's column
        // and are notes, not command rows.
        let p = parse_prompt_v2(&screen(BOX_RM)).expect("the box");
        assert_eq!(p.command_rows.len(), 2, "{:?}", p.command_rows);
        assert_eq!(p.notes.len(), 1);
    }

    /// REVIEW BLOCKER 3 (a regression): the footers the 2.1.280 binary draws
    /// under its question dialog and its tabbed dialogs, with multi-word keys
    /// (`Tab/Arrow keys`) and a verb naming an editor. The negative controls
    /// stay in [`words_that_quote_the_footer_are_not_a_box`].
    #[test]
    fn the_question_dialog_footers_are_footers() {
        for footer in [
            " Enter to select · Tab/Arrow keys to navigate · Esc to cancel",
            " Enter to select · ↑/↓ to navigate · Esc to cancel",
            " Enter to select · Tab/Arrow keys to navigate · ctrl+g to edit in VS Code · Esc to cancel",
            " ←/→ to switch · ↑/↓ to navigate · Enter to select · Esc to close",
        ] {
            assert!(is_hint_footer(footer), "{footer}");
            let r = rows(&[
                "⏺ I have two questions before I start.",
                "",
                "────────────────────────────────────────────────────────────",
                " ←  ☐ Scope  ☐ Tests  ✔ Submit  →",
                "",
                " Which scope should the refactor cover?",
                "",
                " ❯ 1. Small",
                "      Just the parser",
                "   2. Large",
                "      Everything under crates/",
                "   3. Type something.",
                " ────────────────────────────────────────",
                "   4. Chat about this",
                "",
                footer,
            ]);
            assert_eq!(
                crate::phase::worker_phase(&r),
                crate::phase::Phase::Prompt,
                "{footer}"
            );
            let p = parse_prompt_v2(&r).expect("the dialog");
            assert_eq!(p.kind, PromptKind::Other);
            let labels: Vec<&str> = p.options.iter().map(|o| o.label.as_str()).collect();
            assert_eq!(
                labels,
                vec![
                    "Small Just the parser",
                    "Large Everything under crates/",
                    "Type something.",
                    "Chat about this"
                ]
            );
            assert!(p.options.iter().all(|o| o.role == Role::Other));
        }
        assert!(!is_hint_footer(" Enter to select · - Esc to cancel"));
        assert!(!is_hint_footer(" Press Esc to cancel"));
    }

    /// A live box under a blank row below a MULTI-ROW `⎿` output (the tool's
    /// second row in the output column, as `aterm-agent`'s
    /// `a_quoted_indicator_over_a_box_never_fires` draws it) is a box: the
    /// output's own continuation is not the box running straight on. Negative
    /// control: with no blank row between that output and the box, the rows
    /// run straight on and it is not a box.
    #[test]
    fn a_box_under_a_multi_row_tool_output_is_a_box() {
        let box_rows = [
            "",
            " Bash command",
            "",
            "   git push",
            "   Push the branch",
            "",
            " Do you want to proceed?",
            " ❯ 1. Yes",
            "   2. No",
            "",
            " Esc to cancel · Tab to amend",
        ];
        let quote = format!("     {}1% until auto-compact", " ".repeat(77));
        for above in [
            vec![
                "⏺ Bash(aterm ctl @s-2 text)",
                "  ⎿  ✢ Booping…",
                quote.as_str(),
            ],
            vec!["⏺ Bash(ls)", "  ⎿  a", "     b", "     c"],
            vec!["⏺ A message that wraps", "  onto a second row."],
        ] {
            let mut r = rows(&above);
            r.extend(rows(&box_rows));
            r.extend(composer("  ? for shortcuts"));
            assert_eq!(
                parse_prompt(&r).map(|p| p.kind),
                Some(PromptKind::Bash),
                "{above:?}"
            );
            assert_eq!(crate::phase::worker_phase(&r), crate::phase::Phase::Prompt);
            // Negative control: straight on, no blank row — not a box.
            let mut on = rows(&above);
            on.extend(rows(&box_rows[1..]));
            on.extend(composer("  ? for shortcuts"));
            assert_eq!(parse_prompt(&on), None, "{above:?}");
        }
    }

    /// REVIEW MAJOR: a box quoted in the worker's own message — a blank row
    /// under its `⏺` row, every row in the continuation column — is not a
    /// box, whether it quotes a trust dialog (with a folder the worker chose)
    /// or a Bash box. The control: the same rows at the box's own column
    /// under the `⏺` row are a box.
    #[test]
    fn a_box_quoted_in_a_message_is_not_a_box() {
        let trust = [
            "⏺ For reference, the dialog looked like this:",
            "",
            "  Accessing workspace:",
            "",
            "  /Users//x/trusted/proj",
            "",
            "  ❯ No, exit",
            "    Yes, I trust this folder",
            "",
            "  Enter to confirm · Esc to cancel",
            "",
        ];
        let bash = [
            "⏺ It asked:",
            "",
            "  Bash command",
            "",
            "    ls -la",
            "    List files",
            "",
            "  Do you want to proceed?",
            "  ❯ 1. Yes",
            "    2. No",
            "",
            "  Esc to cancel · Tab to amend",
            "",
        ];
        for quoted in [&trust[..], &bash] {
            let mut r = rows(quoted);
            r.extend(composer("  ? for shortcuts"));
            assert_eq!(parse_prompt_v2(&r), None, "{quoted:?}");
            assert_ne!(
                crate::phase::worker_phase(&r),
                crate::phase::Phase::Prompt,
                "{quoted:?}"
            );
            // The control: at the box's own column it is a box.
            let live: Vec<String> = quoted
                .iter()
                .map(|row| row.strip_prefix(' ').unwrap_or(row).to_string())
                .collect();
            assert!(parse_prompt_v2(&live).is_some(), "{live:?}");
        }
    }

    /// REVIEW MAJOR: a trust dialog's folder that wraps is read whole — the
    /// rows of its path block joined with nothing between them — so a cut at
    /// a trust root's boundary does not read as the root.
    #[test]
    fn a_wrapped_trust_path_is_read_whole() {
        let full = screen(TRUST)
            .iter()
            .map(|r| r.trim())
            .find(|t| t.starts_with("/private/"))
            .expect("the folder")
            .to_string();
        for (path, cut) in [
            (full.as_str(), 40),
            (
                "/private/tmp/claude-502-evil/proj",
                "/private/tmp/claude-502".len(),
            ),
        ] {
            let r: Vec<String> = screen(TRUST)
                .into_iter()
                .flat_map(|row| {
                    if row.trim().starts_with("/private/") {
                        vec![format!(" {}", &path[..cut]), format!(" {}", &path[cut..])]
                    } else {
                        vec![row]
                    }
                })
                .collect();
            let p = parse_prompt_v2(&r).expect("the dialog");
            assert_eq!(p.kind, PromptKind::Trust);
            assert_eq!(p.path.as_deref(), Some(path));
            assert_eq!(p.command, path);
            assert!(!p.description.contains("-evil"), "{}", p.description);
        }
    }

    #[test]
    fn option_rows_need_a_number_a_dot_and_a_space() {
        assert_eq!(option_row(" ❯ 1. Yes"), Some((1, "Yes".to_string())));
        assert_eq!(option_row("   12. Many"), Some((12, "Many".to_string())));
        assert_eq!(option_row("   2.5 GB free"), None);
        assert_eq!(option_row("   2158    -    let x = 1;"), None);
        assert_eq!(option_row("  1.Yes"), None);
        assert_eq!(option_row("  ❯ 3. No"), Some((3, "No".to_string())));
    }
}
