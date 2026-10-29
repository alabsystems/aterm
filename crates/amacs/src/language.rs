// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The closed language table that replaces Emacs's major modes (design §2.1; slice L1).

use std::ops::Range;

mod diff;
mod git;
mod markdown;
mod rust;
mod shell;
mod toml;

/// A document's language: the closed set that stands in for major modes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Language {
    /// Text with no syntax: no spans, no comments, no diagnostics.
    #[default]
    Plain,
    Markdown,
    Toml,
    Rust,
    /// POSIX sh and its kin (bash, zsh, ksh, dash).
    Shell,
    /// A unified diff or a patch mail.
    Diff,
    /// A message git asks for: `COMMIT_EDITMSG`, `MERGE_MSG`, `TAG_EDITMSG` and `SQUASH_MSG`.
    GitCommit,
    /// The todo list of `git rebase -i`.
    GitRebaseTodo,
    /// Comma-separated values: a table the grid face edits cell by cell.
    Csv,
    /// Tab-separated values: a table the grid face edits cell by cell.
    Tsv,
}

/// What a span of a line is, for a face to colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SyntaxClass {
    Keyword,
    String,
    Comment,
    Number,
    /// A heading or a title line: a Markdown heading, a TOML table header, a diff hunk header, a commit summary.
    Heading,
    Punct,
    /// A line a diff adds.
    Added,
    /// A line a diff removes.
    Removed,
    /// Text about the text: attributes and macros, lifetimes, variables, fences and link targets, diff file headers.
    Meta,
}

/// One classified run of a line.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    pub range: Range<usize>,
    pub class: SyntaxClass,
}

/// What a line starts inside of, as the line above left it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct LexState {
    mode: Mode,
    /// Bracket nesting at the start of the line (Rust and TOML).
    depth: u16,
    /// One bit per bracket depth below 32. Rust: the bracket at that depth is
    /// the first one its line left open, so it adds one indent level.
    levels: u32,
    /// Where a commit message is (GitCommit only).
    commit: CommitPhase,
}

/// The open construct a line starts inside of. Its meaning is the lexing
/// language's own; a state is never carried from one language to another.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
enum Mode {
    /// Nothing open: ordinary text or code.
    #[default]
    Code,
    /// Inside a Rust `/* */` comment nested this deep (at least 1).
    BlockComment(u8),
    /// Inside a string a double quote opened: Rust's `"`, Shell's `"`,
    /// TOML's `"""`.
    Quoted,
    /// Inside a Rust raw string that closes on `"` and this many `#`.
    RawQuoted(u8),
    /// Inside a string with no escapes: Shell's `'`, TOML's `'''`.
    Literal,
    /// Inside a Markdown fenced block: the fence byte, its run length (at
    /// most 255), and how many quote markers (`>`) the fence's lines start
    /// with, the quote it was opened in.
    Fence(u8, u8, u8),
    /// Inside a Markdown `<!-- -->` comment.
    HtmlComment,
    /// Inside a diff hunk with this many old-side and new-side lines left.
    Hunk(u32, u32),
    /// Inside a Shell here-document's body: the hash of the word that ends
    /// it, and whether its lines may start with tabs (`<<-`).
    Heredoc(u32, bool),
}

/// Where a line of a commit message is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
enum CommitPhase {
    /// Before the summary: only blank and comment lines so far.
    #[default]
    Summary,
    /// After the summary: the body and its trailers.
    Body,
    /// After the scissors line (`git commit -v`): a diff git cuts away.
    Verbose,
}

impl LexState {
    /// The state a document's first line starts in.
    pub const START: Self = Self {
        mode: Mode::Code,
        depth: 0,
        levels: 0,
        commit: CommitPhase::Summary,
    };

    /// The bracket nesting (`(`, `[`, `{`) the line starts at, in Rust and
    /// TOML; zero in the other languages.
    #[inline]
    #[must_use]
    pub const fn depth(self) -> u16 {
        self.depth
    }

    /// Whether the line starts inside a string, a comment or a fenced block rather than in code or prose.
    #[inline]
    #[must_use]
    pub const fn in_literal(self) -> bool {
        !matches!(self.mode, Mode::Code | Mode::Hunk(..))
    }

    /// Whether the line starts inside a string, where every byte is the
    /// string's own and a line break adds and removes nothing.
    #[inline]
    const fn in_string(self) -> bool {
        matches!(
            self.mode,
            Mode::Quoted | Mode::RawQuoted(_) | Mode::Literal | Mode::Heredoc(..)
        )
    }
}

/// A language's comment delimiters, for comment-dwim (slice S8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommentSyntax {
    /// What starts a comment that runs to the end of the line (`"// "`).
    pub line: Option<&'static str>,
    /// What opens and closes a comment inside a line (`("/* ", " */")`).
    pub block: Option<(&'static str, &'static str)>,
}

/// How indentation is written: an indent level is `width` columns, tab stops are every `width` columns, and `tabs` writes whole stops as tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndentStyle {
    pub width: usize,
    pub tabs: bool,
}

impl Default for IndentStyle {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The most columns of indentation [`IndentStyle::render`] writes: a deeper computed indent is clamped, so an absurd width cannot allocate megabytes.
pub const MAX_INDENT_COLUMNS: usize = 1024;

impl IndentStyle {
    /// Four columns of spaces, the editor's defaults.
    pub const DEFAULT: Self = Self {
        width: 4,
        tabs: false,
    };

    /// The whitespace that indents a line to `columns` (at most [`MAX_INDENT_COLUMNS`]).
    pub fn render(self, columns: usize) -> String {
        let columns = indent_columns_cap(columns);
        let width = tab_stop(self.width);
        if !self.tabs {
            return " ".repeat(columns);
        }
        let mut out = "\t".repeat(columns / width);
        out.push_str(&" ".repeat(columns % width));
        out
    }

    /// The display columns of `line`'s leading blanks, a tab advancing to the next stop.
    pub fn columns_of(self, line: &str) -> usize {
        self.columns_through(line, leading_blanks(line))
    }

    /// The display columns of `line`'s first `bytes` bytes, counting each
    /// character after the blanks as one column (list markers are ASCII).
    fn columns_through(self, line: &str, bytes: usize) -> usize {
        let mut columns = 0usize;
        for byte in line.bytes().take(bytes) {
            if byte == b'\t' {
                columns = self.next_stop(columns);
            } else if !(0x80..0xC0).contains(&byte) {
                columns = columns.saturating_add(1);
            }
        }
        columns
    }

    /// The column a tab at `columns` advances to: the next tab stop.
    fn next_stop(self, columns: usize) -> usize {
        let width = tab_stop(self.width);
        columns.saturating_add(width.saturating_sub(columns % width))
    }
}

trust_contract_fn! {
    /// The tab stop an indent `width` gives: the width itself, or 1 for a
    /// width of 0, so a column can always be divided by it.
    fn tab_stop(width: usize) -> usize
    clauses [ensures result >= 1 by tab_stop_is_never_zero
             ensures width < 1 || result == width by tab_stop_keeps_a_nonzero_width]
    {
        if width < 1 { 1 } else { width }
    }
}

verify_island! {
    clean {
        theorem tab_stop_is_never_zero (width : UInt64) :
            Nat.le (UInt64.toNat 1) (UInt64.toNat (trust_import_amacs__language__tab_stop width)) :=
          @Decidable.casesOn (LT.lt width 1)
            (fun (d : Decidable (LT.lt width 1)) =>
              Nat.le (UInt64.toNat 1) (UInt64.toNat (@ite UInt64 (LT.lt width 1) d 1 width)))
            (instDecidableUInt64Lt width 1)
            (fun (at_least_one : Not (LT.lt width 1)) => Nat.ge_of_not_lt at_least_one)
            (fun (_ : LT.lt width 1) => Nat.le_refl (UInt64.toNat 1))

        theorem tab_stop_keeps_a_nonzero_width (width : UInt64) :
            Or (LT.lt width 1) (Eq (trust_import_amacs__language__tab_stop width) width) :=
          @Decidable.casesOn (LT.lt width 1)
            (fun (d : Decidable (LT.lt width 1)) =>
              Or (LT.lt width 1) (Eq (@ite UInt64 (LT.lt width 1) d 1 width) width))
            (instDecidableUInt64Lt width 1)
            (fun (_ : Not (LT.lt width 1)) => Or.inr (Eq.refl width))
            (fun (below : LT.lt width 1) => Or.inl below)
    }
}

trust_contract_fn! {
    /// `columns` of indentation, capped at [`MAX_INDENT_COLUMNS`] (1024). A
    /// select, not `columns.min(…)`: Trust kernel-imports a select, and E6 refuses calls.
    fn indent_columns_cap(columns: usize) -> usize
    clauses [ensures result <= 1024 by indent_columns_cap_is_at_most_the_cap
             ensures columns > 1024 || result == columns by indent_columns_cap_keeps_a_value_within_it]
    {
        if columns > MAX_INDENT_COLUMNS {
            MAX_INDENT_COLUMNS
        } else {
            columns
        }
    }
}

verify_island! {
    clean {
        theorem indent_columns_cap_is_at_most_the_cap (columns : UInt64) :
            Nat.le (UInt64.toNat (trust_import_amacs__language__indent_columns_cap columns)) (UInt64.toNat 1024) :=
          @Decidable.casesOn (LT.lt 1024 columns)
            (fun (d : Decidable (LT.lt 1024 columns)) =>
              Nat.le (UInt64.toNat (@ite UInt64 (LT.lt 1024 columns) d 1024 columns)) (UInt64.toNat 1024))
            (instDecidableUInt64Lt 1024 columns)
            (fun (within : Not (LT.lt 1024 columns)) => Nat.ge_of_not_lt within)
            (fun (_ : LT.lt 1024 columns) => Nat.le_refl (UInt64.toNat 1024))

        theorem indent_columns_cap_keeps_a_value_within_it (columns : UInt64) :
            Or (LT.lt 1024 columns) (Eq (trust_import_amacs__language__indent_columns_cap columns) columns) :=
          @Decidable.casesOn (LT.lt 1024 columns)
            (fun (d : Decidable (LT.lt 1024 columns)) =>
              Or (LT.lt 1024 columns) (Eq (@ite UInt64 (LT.lt 1024 columns) d 1024 columns) columns))
            (instDecidableUInt64Lt 1024 columns)
            (fun (_ : Not (LT.lt 1024 columns)) => Or.inr (Eq.refl columns))
            (fun (above : LT.lt 1024 columns) => Or.inl above)
    }
}

/// What a line break does to the line it splits ([`Language::line_break`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineBreak {
    /// How many bytes of `before` stay on the line above: all of it but its trailing blanks, all of it inside a string, and less when the break ends a list or a quote (the empty item's marker goes).
    pub keep: usize,
    /// How many bytes at the start of `after` go: its leading blanks, which `prefix` stands in for (none inside a string).
    pub skip: usize,
    /// What the new line starts with, before the rest of `after`.
    pub prefix: String,
}

/// The prefix fill keeps on a paragraph's lines, read off its first line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FillPrefix {
    /// How many bytes of the first line are prefix, kept as they are.
    pub first: usize,
    /// What every following line starts with.
    pub rest: String,
}

/// How bad a diagnostic is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Severity {
    /// Allowed, but against convention.
    Warning,
    /// The tool that reads the file refuses it.
    Error,
}

/// Which rule a diagnostic reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiagKind {
    /// A commit summary longer than the limit (50 characters).
    SummaryTooLong,
    /// A commit message whose line after the summary is not blank.
    SecondLineNotBlank,
    /// A rebase todo line whose command git does not know.
    UnknownRebaseCommand,
}

impl DiagKind {
    /// The rule's stable name, for machines (`doc diag`).
    pub const fn name(self) -> &'static str {
        match self {
            Self::SummaryTooLong => "summary-too-long",
            Self::SecondLineNotBlank => "second-line-not-blank",
            Self::UnknownRebaseCommand => "unknown-rebase-command",
        }
    }
}

/// One finding about a document.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Diag {
    pub range: Range<usize>,
    pub line: usize,
    pub severity: Severity,
    pub kind: DiagKind,
    pub message: String,
}

/// The longest commit summary git's convention allows, in characters.
pub const SUMMARY_LIMIT: usize = 50;

impl Language {
    /// Every language, in declaration order.
    pub const ALL: [Self; 10] = [
        Self::Plain,
        Self::Markdown,
        Self::Toml,
        Self::Rust,
        Self::Shell,
        Self::Diff,
        Self::GitCommit,
        Self::GitRebaseTodo,
        Self::Csv,
        Self::Tsv,
    ];

    /// The name config uses (`[amacs.language.git-commit]`, design §9).
    pub const fn name(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::Markdown => "markdown",
            Self::Toml => "toml",
            Self::Rust => "rust",
            Self::Shell => "shell",
            Self::Diff => "diff",
            Self::GitCommit => "git-commit",
            Self::GitRebaseTodo => "git-rebase-todo",
            Self::Csv => "csv",
            Self::Tsv => "tsv",
        }
    }

    /// The language config calls `name`.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|language| language.name() == name)
    }

    /// The modeline's name: `(GitCommit)`.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Plain => "Plain",
            Self::Markdown => "Markdown",
            Self::Toml => "Toml",
            Self::Rust => "Rust",
            Self::Shell => "Shell",
            Self::Diff => "Diff",
            Self::GitCommit => "GitCommit",
            Self::GitRebaseTodo => "GitRebaseTodo",
            Self::Csv => "CSV",
            Self::Tsv => "TSV",
        }
    }

    /// The language of the file at `path` whose first line is `first_line`.
    pub fn detect(path: &str, first_line: &str) -> Self {
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
        by_file_name(name)
            .or_else(|| by_extension(name))
            .or_else(|| by_first_line(first_line))
            .unwrap_or(Self::Plain)
    }

    /// What separates a table's cells: a comma for CSV, a tab for TSV, and none for a language
    /// that is not a table. A language with one opens in the grid face.
    pub const fn delimiter(self) -> Option<crate::table::Delimiter> {
        match self {
            Self::Csv => Some(crate::table::Delimiter::Comma),
            Self::Tsv => Some(crate::table::Delimiter::Tab),
            _ => None,
        }
    }

    /// The column fill wraps at by default: 72 for a commit message, as git's own tools wrap it, and Emacs's 70 for everything else.
    pub const fn fill_column(self) -> usize {
        match self {
            Self::GitCommit => 72,
            _ => 70,
        }
    }

    /// The summary limit [`diagnostics`](Self::diagnostics) applies: 50 for a commit message, none for the rest.
    pub const fn summary_limit(self) -> Option<usize> {
        match self {
            Self::GitCommit => Some(SUMMARY_LIMIT),
            _ => None,
        }
    }

    /// The language's comment delimiters.
    pub const fn comment(self) -> CommentSyntax {
        let (line, block) = match self {
            Self::Plain | Self::Diff | Self::Csv | Self::Tsv => (None, None),
            Self::Markdown => (None, Some(("<!-- ", " -->"))),
            Self::Rust => (Some("// "), Some(("/* ", " */"))),
            Self::Toml | Self::Shell | Self::GitCommit | Self::GitRebaseTodo => (Some("# "), None),
        };
        CommentSyntax { line, block }
    }

    /// Lex one line (without its `\n`) that starts in `state`.
    pub fn lex_line(self, state: LexState, line: &str) -> (Vec<Span>, LexState) {
        let mut out = Spans::default();
        let next = self.lex_into(state, line, &mut out);
        (out.0, next)
    }

    fn lex_into(self, state: LexState, line: &str, out: &mut Spans) -> LexState {
        match self {
            Self::Plain | Self::Csv | Self::Tsv => state,
            Self::Markdown => markdown::lex(state, line, out),
            Self::Toml => toml::lex(state, line, out),
            Self::Rust => rust::lex(state, line, out),
            Self::Shell => shell::lex(state, line, out),
            Self::Diff => diff::lex(state, line, out),
            Self::GitCommit => git::lex_commit(state, line, out),
            Self::GitRebaseTodo => git::lex_todo(state, line, out),
        }
    }

    /// The column `line`'s text should start at, for indent-for-tab-command.
    pub fn indent_column(
        self,
        state: LexState,
        previous: Option<&str>,
        line: &str,
        style: IndentStyle,
    ) -> usize {
        let kept = previous.map_or(0, |above| style.columns_of(above));
        match self {
            Self::Rust => rust::indent_column(state, kept, line, style),
            Self::Markdown => markdown::indent_column(state, previous, line, style).unwrap_or(kept),
            _ => kept,
        }
    }

    /// What a line break between `before` and `after` does: what stays of the line above, and what the new line starts with.
    pub fn line_break(
        self,
        state: LexState,
        before: &str,
        after: &str,
        style: IndentStyle,
    ) -> LineBreak {
        let at_break = self.lex_into(state, before, &mut Spans::default());
        if at_break.in_string() {
            return LineBreak {
                keep: before.len(),
                skip: 0,
                prefix: String::new(),
            };
        }
        let keep = before.trim_end_matches([' ', '\t']).len();
        let (keep, prefix) = match self {
            Self::Rust => (keep, rust::line_prefix(at_break, before, after, style)),
            Self::Markdown => markdown::line_break(state, before, keep),
            _ => (keep, leading(before).to_owned()),
        };
        LineBreak {
            keep,
            skip: leading_blanks(after),
            prefix,
        }
    }

    /// The prefix fill-paragraph keeps on each line of the paragraph whose first line is `line`: a comment leader (`    /// `, `# `), a Markdown quote (`> `), the hanging indent of a list item (`- ` on the first line, two spaces after it), or the line's own indentation.
    pub fn fill_prefix(self, line: &str, style: IndentStyle) -> FillPrefix {
        let item = match self {
            Self::Markdown | Self::GitCommit => markdown::hanging_indent(line, style),
            _ => None,
        };
        if let Some(prefix) = item {
            return prefix;
        }
        let leader = match self {
            Self::Rust => rust::comment_leader(line),
            Self::Markdown => markdown::quote_leader(line),
            Self::Toml | Self::Shell | Self::GitCommit | Self::GitRebaseTodo => {
                hash_comment_leader(line)
            }
            Self::Plain | Self::Diff | Self::Csv | Self::Tsv => None,
        };
        let first = leader.unwrap_or_else(|| leading_blanks(line));
        FillPrefix {
            first,
            rest: line.get(..first).unwrap_or("").to_owned(),
        }
    }

    /// The language's findings about `text`, with the default summary limit.
    pub fn diagnostics(self, text: &str) -> Vec<Diag> {
        self.diagnostics_with(text, SUMMARY_LIMIT)
    }

    /// [`diagnostics`](Self::diagnostics) with a configured summary limit (`[amacs.language.git-commit] summary_limit`).
    pub fn diagnostics_with(self, text: &str, summary_limit: usize) -> Vec<Diag> {
        match self {
            Self::GitCommit => git::commit_diagnostics(text, summary_limit),
            Self::GitRebaseTodo => git::todo_diagnostics(text),
            _ => Vec::new(),
        }
    }
}

fn by_file_name(name: &str) -> Option<Language> {
    Some(match name {
        "COMMIT_EDITMSG" | "MERGE_MSG" | "TAG_EDITMSG" | "SQUASH_MSG" => Language::GitCommit,
        "git-rebase-todo" => Language::GitRebaseTodo,
        "Cargo.lock" | "Pipfile" | "poetry.lock" | "uv.lock" => Language::Toml,
        ".bashrc" | ".bash_profile" | ".bash_login" | ".bash_logout" | ".bash_aliases"
        | ".profile" | ".zshrc" | ".zshenv" | ".zprofile" | ".zlogin" | ".zlogout" | ".kshrc"
        | ".envrc" | "PKGBUILD" | "APKBUILD" => Language::Shell,
        _ => return None,
    })
}

fn by_extension(name: &str) -> Option<Language> {
    let (stem, extension) = name.rsplit_once('.')?;
    if stem.is_empty() {
        return None;
    }
    let table: [(&[&str], Language); 7] = [
        (
            &["md", "markdown", "mdown", "mkd", "mkdn"],
            Language::Markdown,
        ),
        (&["toml"], Language::Toml),
        (&["rs"], Language::Rust),
        (&["sh", "bash", "zsh", "ksh", "dash"], Language::Shell),
        (&["diff", "patch"], Language::Diff),
        (&["csv"], Language::Csv),
        (&["tsv", "tab"], Language::Tsv),
    ];
    table.into_iter().find_map(|(extensions, language)| {
        extensions
            .iter()
            .any(|known| known.eq_ignore_ascii_case(extension))
            .then_some(language)
    })
}

fn by_first_line(line: &str) -> Option<Language> {
    if let Some(command) = line.strip_prefix("#!") {
        return shebang(command);
    }
    let patch_mail = line
        .strip_prefix("From ")
        .and_then(|rest| rest.get(..40))
        .is_some_and(|sha| sha.bytes().all(|byte| byte.is_ascii_hexdigit()));
    (line.starts_with("diff --git ") || patch_mail).then_some(Language::Diff)
}

/// The language a `#!` line's interpreter writes. `env` is looked through,
/// with its options and assignments (`#!/usr/bin/env -S bash -e`).
fn shebang(command: &str) -> Option<Language> {
    let mut words = command.split_ascii_whitespace();
    let mut program = words.next()?.rsplit('/').next()?;
    if program == "env" {
        program = words.find(|word| !word.starts_with('-') && !word.contains('='))?;
        program = program.rsplit('/').next()?;
    }
    match program {
        "sh" | "bash" | "zsh" | "ksh" | "dash" | "ash" | "mksh" => Some(Language::Shell),
        "cargo" | "rust-script" => Some(Language::Rust),
        _ => None,
    }
}

/// `line`'s leading blanks (spaces and tabs).
fn leading(line: &str) -> &str {
    line.get(..leading_blanks(line)).unwrap_or("")
}

/// How many bytes of leading blanks (spaces and tabs) `line` has.
fn leading_blanks(line: &str) -> usize {
    line.bytes()
        .take_while(|&byte| byte == b' ' || byte == b'\t')
        .count()
}

/// The length of a `#` comment leader at the start of `line` (its blanks,
/// the run of `#` and the blanks after it), when the line is a comment.
fn hash_comment_leader(line: &str) -> Option<usize> {
    let blanks = leading_blanks(line);
    let rest = line.get(blanks..)?;
    let hashes = rest.bytes().take_while(|&byte| byte == b'#').count();
    if hashes == 0 {
        return None;
    }
    let after = rest.get(hashes..)?;
    Some(
        blanks
            .saturating_add(hashes)
            .saturating_add(leading_blanks(after)),
    )
}

trust_contract_fn! {
    /// Whether `byte` is a blank inside a line, for every lexer: a space, a
    /// tab, or the `\r` of a CRLF line end.
    fn line_space(byte: u8) -> bool
    clauses [ensures result == (byte == 32 || (byte == 9 || byte == 13)) by line_space_is_exactly_the_blanks]
    {
        byte == b' ' || (byte == b'\t' || byte == b'\r')
    }
}

verify_island! {
    clean {
        theorem line_space_is_exactly_the_blanks (byte : UInt8) :
            trust_import_amacs__language__line_space byte =
              (decide (byte = 32) || (decide (byte = 9) || decide (byte = 13))) := by
          simp only [trust_import_amacs__language__line_space]
    }
}

/// `value` with the decimal digit `byte` (`b'0'` to `b'9'`) appended, or
/// `u32::MAX` once `value` is past [`decimal_room`]'s bound: a run of digits
/// read this way saturates and never wraps.
fn decimal_push(value: u32, byte: u8) -> u32 {
    let digit = u32::from(byte.saturating_sub(b'0')).min(9);
    if decimal_room(value) {
        decimal_append(value, digit)
    } else {
        u32::MAX
    }
}

trust_contract_fn! {
    /// Whether `value` takes one more decimal digit with no wrap: at most
    /// 429,496,728, the largest value whose every next digit fits in a `u32`.
    #[must_use]
    fn decimal_room(value: u32) -> bool
    clauses [ensures result == (value <= 429496728) by decimal_room_is_the_bound]
    {
        value <= 429_496_728
    }
}

trust_contract_fn! {
    /// One decimal digit appended to `value`, `value * 10 + digit`, in
    /// wrapping `u32` arithmetic: [`decimal_push`] calls it only below
    /// [`decimal_room`]'s bound, with `digit` at most 9, where nothing wraps.
    #[must_use]
    fn decimal_append(value: u32, digit: u32) -> u32
    clauses [ensures result == decimal_append_step(value, digit) by decimal_append_is_the_step]
    {
        value.wrapping_mul(10).wrapping_add(digit)
    }
}

verify_island! {
    clean {
        theorem decimal_room_is_the_bound (value : UInt32) :
            trust_import_amacs__language__decimal_room value = decide (value <= 429496728) := by
          simp only [trust_import_amacs__language__decimal_room]

        theorem decimal_room_bound_takes_any_digit :
            Nat.le (Nat.add (Nat.mul 429496728 10) 9) 4294967295 := by
          decide

        theorem decimal_room_bound_is_the_largest :
            Nat.lt 4294967295 (Nat.add (Nat.mul 429496729 10) 9) := by
          decide

        def decimal_append_step (value digit : UInt32) : UInt32 := value * 10 + digit

        theorem decimal_append_is_the_step (value digit : UInt32) :
            trust_import_amacs__language__decimal_append value digit = decimal_append_step value digit := by
          simp only [trust_import_amacs__language__decimal_append, decimal_append_step]
    }
}

/// A line's spans as a lexer emits them, in order: an empty span is dropped,
/// and a span that continues the last one in the same class extends it.
#[derive(Default)]
struct Spans(Vec<Span>);

impl Spans {
    fn push(&mut self, start: usize, end: usize, class: SyntaxClass) {
        if start >= end {
            return;
        }
        if let Some(last) = self.0.last_mut()
            && last.class == class
            && last.range.end == start
        {
            last.range.end = end;
            return;
        }
        self.0.push(Span {
            range: start..end,
            class,
        });
    }
}

#[cfg(test)]
thread_local! {
    /// How many byte reads the lexers made on this thread: the work measure
    /// the linear-cost test holds to O(line).
    static BYTE_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A forward reader over one line's bytes. Every lexer reads through it, and
/// a read never goes past the line, so a lexer's position only moves forward.
struct Cursor<'a> {
    line: &'a str,
    pos: usize,
}

impl<'a> Cursor<'a> {
    const fn new(line: &'a str) -> Self {
        Self { line, pos: 0 }
    }

    const fn pos(&self) -> usize {
        self.pos
    }

    const fn len(&self) -> usize {
        self.line.len()
    }

    const fn done(&self) -> bool {
        self.pos >= self.line.len()
    }

    /// The byte `ahead` bytes past the position.
    fn nth(&self, ahead: usize) -> Option<u8> {
        #[cfg(test)]
        BYTE_READS.with(|reads| reads.set(reads.get() + 1));
        self.line
            .as_bytes()
            .get(self.pos.saturating_add(ahead))
            .copied()
    }

    fn peek(&self) -> Option<u8> {
        self.nth(0)
    }

    const fn bump(&mut self) {
        if self.pos < self.line.len() {
            self.pos += 1;
        }
    }

    /// Step over `count` bytes, stopping at the end of the line.
    fn bump_n(&mut self, count: usize) {
        self.pos = self.pos.saturating_add(count).min(self.line.len());
    }

    fn eat(&mut self, byte: u8) -> bool {
        let found = self.peek() == Some(byte);
        if found {
            self.bump();
        }
        found
    }

    /// Step over bytes while `keep` holds; returns how many.
    fn eat_while(&mut self, mut keep: impl FnMut(u8) -> bool) -> usize {
        let start = self.pos;
        while let Some(byte) = self.peek() {
            if !keep(byte) {
                break;
            }
            self.bump();
        }
        self.pos.saturating_sub(start)
    }

    /// Whether the bytes at the position spell `text`.
    fn looking_at(&self, text: &[u8]) -> bool {
        text.iter()
            .enumerate()
            .all(|(ahead, &byte)| self.nth(ahead) == Some(byte))
    }

    /// Step past continuation bytes, onto the next char boundary.
    fn skip_to_boundary(&mut self) {
        while self.peek().is_some_and(|byte| (0x80..0xC0).contains(&byte)) {
            self.bump();
        }
    }

    /// Jump to the end of the line.
    const fn finish(&mut self) {
        self.pos = self.line.len();
    }

    /// The text from `start` to the position.
    fn since(&self, start: usize) -> &'a str {
        self.line.get(start..self.pos).unwrap_or("")
    }

    /// The text from the position to the end of the line.
    fn rest(&self) -> &'a str {
        self.line.get(self.pos..).unwrap_or("")
    }
}

/// One lexed line in a [`LexCache`].
#[derive(Clone, Debug, PartialEq, Eq)]
struct LexedLine {
    start: LexState,
    spans: Vec<Span>,
}

/// A document's lines, lexed, kept current across edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LexCache {
    language: Language,
    lines: Vec<LexedLine>,
    /// The state after the last line.
    end: LexState,
}

impl LexCache {
    /// Lex all of `text`.
    pub fn new(language: Language, text: &str) -> Self {
        let mut cache = Self {
            language,
            lines: Vec::new(),
            end: LexState::START,
        };
        let mut state = LexState::START;
        for line in text.split('\n') {
            let (spans, next) = language.lex_line(state, line);
            cache.lines.push(LexedLine {
                start: state,
                spans,
            });
            state = next;
        }
        cache.end = state;
        cache
    }

    pub const fn language(&self) -> Language {
        self.language
    }

    /// How many lines the cache holds.
    pub const fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// The spans of `line`; none for a line past the end.
    pub fn spans(&self, line: usize) -> &[Span] {
        self.lines.get(line).map_or(&[], |lexed| &lexed.spans)
    }

    /// The state `line` starts in; for a line past the end, the state after the last line.
    pub fn state_at(&self, line: usize) -> LexState {
        self.lines.get(line).map_or(self.end, |lexed| lexed.start)
    }

    /// Bring the cache up to date with `text` after an edit replaced lines `first .. first + removed` of the old text with lines `first .. first + inserted` of `text`.
    pub fn splice(&mut self, text: &str, first: usize, removed: usize, inserted: usize) -> usize {
        let first = first.min(self.lines.len());
        let removed = removed.min(self.lines.len().saturating_sub(first));
        let offset = match first.checked_sub(1) {
            None => Some(0),
            Some(breaks) => text
                .match_indices('\n')
                .nth(breaks)
                .map(|(at, _)| at.saturating_add(1)),
        };
        let Some(offset) = offset else {
            *self = Self::new(self.language, text);
            return self.lines.len();
        };
        let mut lines = text.get(offset..).unwrap_or("").split('\n');
        let mut state = self.state_at(first);
        let mut fresh = Vec::with_capacity(inserted.min(4096));
        for line in lines.by_ref().take(inserted) {
            let (spans, next) = self.language.lex_line(state, line);
            fresh.push(LexedLine {
                start: state,
                spans,
            });
            state = next;
        }
        let mut index = first.saturating_add(fresh.len());
        let mut lexed = fresh.len();
        self.lines
            .splice(first..first.saturating_add(removed), fresh);
        while let Some(old) = self.lines.get_mut(index) {
            if old.start == state {
                return lexed;
            }
            let Some(line) = lines.next() else {
                break;
            };
            let (spans, next) = self.language.lex_line(state, line);
            *old = LexedLine {
                start: state,
                spans,
            };
            state = next;
            index = index.saturating_add(1);
            lexed = lexed.saturating_add(1);
        }
        self.lines.truncate(index);
        for line in lines {
            let (spans, next) = self.language.lex_line(state, line);
            self.lines.push(LexedLine {
                start: state,
                spans,
            });
            state = next;
            lexed = lexed.saturating_add(1);
        }
        self.end = state;
        lexed
    }
}

#[cfg(test)]
mod tests;
