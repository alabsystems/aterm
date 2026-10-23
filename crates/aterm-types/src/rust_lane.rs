// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Rust on this machine means the Trust toolchain.

use std::path::{Path, PathBuf};

/// The escape hatch an agent writes in front of a command that genuinely needs stock Rust.
pub const STOCK_REASON: &str = "ATERM_STOCK_REASON";

/// `build`, `check`, `test`: refused bare, and runnable in either lane.
pub const BOTH_LANES: &[&str] = &["build", "check", "test"];

/// Compilation verbs with no verified lane: `targo --unverified <verb>`.
pub const UNVERIFIED_ONLY: &[&str] = &[
    "run", "doc", "bench", "rustc", "rustdoc", "fix", "package", "install", "publish",
];

/// Verbs that take no lane at all (`--unverified` is refused on them).
pub const NO_LANE: &[&str] = &[
    "clean",
    "metadata",
    "tree",
    "update",
    "fetch",
    "generate-lockfile",
    "locate-project",
    "pkgid",
    "vendor",
    "add",
    "remove",
    "new",
    "init",
    "version",
    "help",
    "search",
    "info",
    "config",
    "report",
    "verify-project",
    "read-manifest",
    "owner",
    "fmt",
    "tippy",
];

/// The stock names [`trust_spelling`] and [`check`] know, each with the Trust tool that replaces it.
pub const STOCK_TOOLS: &[(&str, &str)] = &[
    ("cargo", "targo"),
    ("rustc", "trustc"),
    ("rustfmt", "trustfmt"),
    ("rustdoc", "trustdoc"),
    ("clippy", "tippy"),
    ("cargo-clippy", "targo tippy"),
    ("clippy-driver", "targo tippy"),
    ("cargo-fmt", "targo fmt"),
];

/// The note on the one spelling of a cargo verb in none of the verb classes, usually a cargo extension (`nextest`, `deny`).
pub const UNKNOWN_VERB_NOTE: &str =
    "if targo has this verb; a cargo extension targo lacks has no Trust spelling";

/// Whether `spellings` is the conditional one of a cargo verb targo is not known to have ([`UNKNOWN_VERB_NOTE`]).
#[must_use]
pub fn is_unknown_verb(spellings: &[Spelling]) -> bool {
    spellings.iter().any(|s| s.note == UNKNOWN_VERB_NOTE)
}

/// The label a spelling carries: which verification lane it runs in, if any.
pub const VERIFIED: &str = "VERIFIED";
/// See [`VERIFIED`].
pub const UNVERIFIED: &str = "UNVERIFIED";

/// One Trust command, the caller's own arguments filled in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spelling {
    /// [`VERIFIED`], [`UNVERIFIED`], or empty for a command that takes no lane.
    pub label: &'static str,
    /// The command to type.
    pub command: String,
    /// What it means, in a few words.
    pub note: &'static str,
}

/// Which lane(s) a cargo verb takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CargoVerb {
    /// `build`/`check`/`test`.
    BothLanes,
    /// A compilation verb with no verified lane.
    UnverifiedOnly,
    /// A verb that takes no lane.
    NoLane,
    /// `clippy` — the linter is `targo tippy`.
    Clippy,
    /// `trust` / `--unverified` in the verb position: the lane is already named in cargo's own spelling (`cargo trust check`).
    NamedLane,
    /// No verb at all: flags only (`cargo --version`) or nothing.
    FlagsOnly,
    /// A verb this table does not know — usually a cargo extension.
    Unknown,
}

/// The class of `verb`.
#[must_use]
pub fn cargo_verb(verb: &str) -> CargoVerb {
    if BOTH_LANES.contains(&verb) {
        CargoVerb::BothLanes
    } else if UNVERIFIED_ONLY.contains(&verb) {
        CargoVerb::UnverifiedOnly
    } else if NO_LANE.contains(&verb) {
        CargoVerb::NoLane
    } else if verb == "clippy" {
        CargoVerb::Clippy
    } else if verb == "trust" || verb == "--unverified" {
        CargoVerb::NamedLane
    } else {
        CargoVerb::Unknown
    }
}

/// `+<toolchain>` as the first argument (rustup's own spelling), or `None`.
#[must_use]
pub fn explicit_toolchain(args: &[String]) -> Option<&str> {
    let first = args.first()?;
    let toolchain = first.strip_prefix('+')?;
    (!toolchain.is_empty()).then_some(toolchain)
}

/// Whether a toolchain name is the Trust channel (`trust`, `trust-<rev>`, …).
#[must_use]
pub fn is_trust_channel(channel: &str) -> bool {
    channel.starts_with("trust")
}

/// Global cargo options that take their value as the next word.
const CARGO_VALUE_FLAGS: &[&str] = &["--color", "--config", "-Z", "-C", "--explain"];

/// The index of cargo's verb in `args` (no leading `+toolchain`): the first word that is not a global option or an option's value.
#[must_use]
pub fn cargo_verb_index(args: &[String]) -> Option<usize> {
    let mut i = 0;
    while let Some(word) = args.get(i) {
        if word == "--unverified" {
            return Some(i);
        }
        if !word.starts_with('-') {
            return Some(i);
        }
        i += if CARGO_VALUE_FLAGS.contains(&word.as_str()) {
            2
        } else {
            1
        };
    }
    None
}

fn join_words(words: &[String]) -> String {
    let mut s = String::new();
    for w in words {
        if !s.is_empty() {
            s.push(' ');
        }
        s.push_str(w);
    }
    s
}

fn command_line(tool: &str, words: &[String]) -> String {
    let mut s = String::from(tool);
    for w in words {
        s.push(' ');
        s.push_str(w);
    }
    s
}

/// The Trust spelling of `<stock> <args>`, `args` exactly as the caller typed them (a leading `+<toolchain>` is dropped: no Trust tool takes a rustup directive).
#[must_use]
pub fn trust_spelling(stock: &str, args: &[String]) -> Option<Vec<Spelling>> {
    let args = if explicit_toolchain(args).is_some() {
        &args[1..]
    } else {
        args
    };
    let one = |command: String, note: &'static str| {
        vec![Spelling {
            label: "",
            command,
            note,
        }]
    };
    Some(match stock {
        "cargo" => cargo_spelling(args),
        "rustc" => vec![
            Spelling {
                label: VERIFIED,
                command: command_line("trustc", args),
                note: "proves as it compiles",
            },
            Spelling {
                label: UNVERIFIED,
                command: command_line("trustc -Ztrust-verify=off", args),
                note: "compiles as vanilla Rust",
            },
        ],
        "rustfmt" => one(command_line("trustfmt", args), "the Trust formatter"),
        "rustdoc" => one(command_line("trustdoc", args), "the Trust doc tool"),
        "clippy" => one(command_line("tippy", args), "the Trust linter"),
        "cargo-clippy" | "clippy-driver" => {
            one(command_line("targo tippy", args), "the Trust linter")
        }
        "cargo-fmt" => one(command_line("targo fmt", args), "the Trust formatter"),
        _ => return None,
    })
}

/// [`trust_spelling`] for `cargo`, verb by verb (the module table).
fn cargo_spelling(args: &[String]) -> Vec<Spelling> {
    let Some(at) = cargo_verb_index(args) else {
        return vec![Spelling {
            label: "",
            command: command_line("targo", args),
            note: "takes no lane",
        }];
    };
    let verb_first = |verb: &str| -> Vec<String> {
        let mut words = vec![verb.to_string()];
        words.extend_from_slice(&args[..at]);
        words.extend_from_slice(&args[at + 1..]);
        words
    };
    let verb = args[at].as_str();
    match cargo_verb(verb) {
        CargoVerb::BothLanes => {
            let words = verb_first(verb);
            vec![
                Spelling {
                    label: VERIFIED,
                    command: command_line("targo trust", &words),
                    note: "emits a proof claim",
                },
                Spelling {
                    label: UNVERIFIED,
                    command: command_line("targo --unverified", &words),
                    note: "no proof claim",
                },
            ]
        }
        CargoVerb::UnverifiedOnly => vec![Spelling {
            label: UNVERIFIED,
            command: command_line("targo --unverified", &verb_first(verb)),
            note: "no proof claim; this verb has no verified lane",
        }],
        CargoVerb::Clippy => vec![Spelling {
            label: "",
            command: command_line("targo", &verb_first("tippy")),
            note: "the Trust linter; takes no lane",
        }],
        CargoVerb::NoLane | CargoVerb::NamedLane | CargoVerb::FlagsOnly => vec![Spelling {
            label: "",
            command: command_line("targo", args),
            note: if matches!(cargo_verb(verb), CargoVerb::NamedLane) {
                "the lane is already named"
            } else {
                "takes no lane"
            },
        }],
        CargoVerb::Unknown => vec![Spelling {
            label: "",
            command: command_line("targo", args),
            note: UNKNOWN_VERB_NOTE,
        }],
    }
}

/// The spellings as indented lines, one per lane, the commands padded to one column.
#[must_use]
pub fn render_spellings(spellings: &[Spelling], indent: &str) -> String {
    let width = spellings.iter().map(|s| s.command.len()).max().unwrap_or(0) + 3;
    let label_width = spellings.iter().map(|s| s.label.len()).max().unwrap_or(0);
    let mut out = String::new();
    for s in spellings {
        out.push_str(indent);
        out.push_str(&s.command);
        if s.label.is_empty() && label_width == 0 {
            out.push('\n');
            continue;
        }
        for _ in s.command.len()..width {
            out.push(' ');
        }
        out.push_str(s.label);
        for _ in s.label.len()..label_width {
            out.push(' ');
        }
        out.push_str(" — ");
        out.push_str(s.note);
        out.push('\n');
    }
    out
}

/// The channel a toolchain file names.
#[must_use]
pub fn channel_in(text: &str) -> Option<String> {
    let lines = text.lines().map(str::trim).filter(|l| !l.starts_with('#'));
    let mut first_line = None;
    for l in lines {
        if l.is_empty() {
            continue;
        }
        if first_line.is_none() {
            first_line = Some(l);
        }
        let Some(rest) = l.strip_prefix("channel") else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(quote) = rest.chars().next().filter(|q| *q == '"' || *q == '\'') else {
            continue;
        };
        let rest = &rest[1..];
        let Some(end) = rest.find(quote) else {
            continue;
        };
        return Some(rest[..end].to_string());
    }
    let legacy = first_line?;
    let bare = legacy
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'));
    bare.then(|| legacy.to_string())
}

/// The toolchain file rustup would read for `start`, and the channel it names.
#[must_use]
pub fn find_pin(start: &Path) -> Option<(PathBuf, String)> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        for name in ["rust-toolchain", "rust-toolchain.toml"] {
            let file = d.join(name);
            if file.is_file() {
                let text = std::fs::read_to_string(&file).ok()?;
                return channel_in(&text).map(|ch| (file, ch));
            }
        }
        dir = d.parent();
    }
    None
}

/// Where a pinned channel was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinSource {
    /// `$RUSTUP_TOOLCHAIN`.
    Env,
    /// A toolchain file.
    File(PathBuf),
}

/// A stock Rust tool found at command position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// The stock spelling as used.
    pub used: String,
    /// The Trust spelling of the same command, the caller's arguments filled in.
    pub instead: Vec<Spelling>,
    /// Set when the command word is a Trust toolchain's own compatibility `cargo` by path.
    pub toolchain_dir: Option<String>,
    /// The rustup channel the command named.
    pub channel: Option<String>,
    /// Set when the command word is a Trust toolchain's compatibility `cargo`.
    pub compat: bool,
    /// Set when the tool was found in text nested deeper than the reader follows ([`MAX_DEPTH`]).
    pub unread: bool,
}

/// What [`verdict`] found in one command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// No stock Rust tool at command position.
    Clean,
    /// A stock Rust tool at command position, in a simple command that carries no escape: the one to refuse.
    Stock(Hit),
    /// Every stock Rust tool at command position sits in a simple command that carries a non-empty [`STOCK_REASON`].
    Escaped {
        /// The escaped stock command.
        hit: Hit,
        /// The `ATERM_STOCK_REASON` value, verbatim.
        reason: String,
    },
}

/// Fold one more reading into `found`. A [`Verdict::Stock`] ends the search (the
fn fold(found: &mut Verdict, next: Verdict) -> bool {
    match next {
        Verdict::Stock(_) => {
            *found = next;
            true
        }
        Verdict::Escaped { .. } if *found == Verdict::Clean => {
            *found = next;
            false
        }
        _ => false,
    }
}

/// The deepest nesting ([`check`] recursing into `$(…)`, backticks, `bash -c`, `eval`) that is parsed.
pub const MAX_DEPTH: usize = 8;

/// The first stock Rust tool at command position in `command` (a shell command line as an agent's Bash tool receives it) that no escape clears, or `None`.
#[must_use]
pub fn check(command: &str) -> Option<Hit> {
    match verdict(command) {
        Verdict::Stock(hit) => Some(hit),
        Verdict::Clean | Verdict::Escaped { .. } => None,
    }
}

/// [`check`]'s reading, with an escaped stock command reported as [`Verdict::Escaped`] rather than dropped, so a guard that lets it through can say what ran and why.
#[must_use]
pub fn verdict(command: &str) -> Verdict {
    verdict_in(command, &|_, _| false)
}

/// [`verdict`], with `beside(dir, name)` answering whether a file `name` sits in `dir`, the directory of a path-form command word as written, trailing `/` included (`build/aarch64-apple-darwin/stage2/bin/`).
#[must_use]
pub fn verdict_in(command: &str, beside: &dyn Fn(&str, &str) -> bool) -> Verdict {
    check_at(command, 0, beside)
}

/// The directory probe [`verdict_in`] threads through the reader.
type Beside<'a> = &'a dyn Fn(&str, &str) -> bool;

/// A Trust toolchain's compatibility names, each with the Trust name of the same tool, whose presence marks the directory as a Trust toolchain's `bin/`.
pub const COMPAT_TWINS: &[(&str, &str)] = &[
    ("rustc", "trustc"),
    ("rustdoc", "trustdoc"),
    ("cargo", "targo"),
];

/// Whether a file `name` sits in `dir`, a directory as a shell command line writes it ([`verdict_in`]'s probe).
#[must_use]
pub fn file_beside(dir: &str, name: &str, cwd: Option<&Path>, home: Option<&Path>) -> bool {
    let from_home = ["~/", "$HOME/", "${HOME}/"]
        .iter()
        .find_map(|prefix| dir.strip_prefix(prefix));
    let rest = from_home.unwrap_or(dir);
    if rest.starts_with('~') || rest.contains(['$', '`', '*', '?', '[']) {
        return false;
    }
    let base = match (from_home, home, cwd) {
        (Some(rest), Some(home), _) => home.join(rest),
        (None, _, _) if Path::new(dir).is_absolute() => PathBuf::from(dir),
        (None, _, Some(cwd)) => cwd.join(dir),
        (Some(_), None, _) | (None, _, None) => return false,
    };
    base.join(name).is_file()
}

/// The directory of a path-form command word, trailing `/` included
fn dir_of(word: &str) -> Option<&str> {
    word.rfind('/').map(|at| &word[..=at])
}

fn check_at(command: &str, depth: usize, beside: Beside<'_>) -> Verdict {
    if depth > MAX_DEPTH {
        return unread(command);
    }
    let mut found = Verdict::Clean;
    let (text, heredoc_subs) = strip_heredocs(command);
    for sub in heredoc_subs {
        if fold(&mut found, check_at(&sub, depth + 1, beside)) {
            return found;
        }
    }
    let (segments, subs) = split_commands(&text);
    for segment in segments {
        if fold(&mut found, offending(&segment, depth, beside)) {
            return found;
        }
    }
    for sub in subs {
        if fold(&mut found, check_at(&sub, depth + 1, beside)) {
            return found;
        }
    }
    found
}

/// The verdict for text nested deeper than [`MAX_DEPTH`], which is not parsed: the
fn unread(text: &str) -> Verdict {
    let Some(name) = text
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .find(|word| STOCK_TOOLS.iter().any(|(stock, _)| stock == word))
    else {
        return Verdict::Clean;
    };
    Verdict::Stock(Hit {
        used: name.to_string(),
        instead: trust_spelling(name, &[]).unwrap_or_default(),
        toolchain_dir: None,
        channel: None,
        compat: false,
        unread: true,
    })
}

/// Remove heredoc bodies — text fed to a command on stdin, not commands. The
fn strip_heredocs(cmd: &str) -> (String, Vec<String>) {
    let lines: Vec<&str> = cmd.split('\n').collect();
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    let mut subs = Vec::new();
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        out.push(line);
        let pending = heredoc_operators(line, &mut quote);
        i += 1;
        for (term, dash, quoted) in pending {
            while i < lines.len() {
                let body = lines[i];
                let candidate = if dash {
                    body.trim_start_matches('\t')
                } else {
                    body
                };
                i += 1;
                if candidate == term {
                    break;
                }
                if !quoted {
                    let (_, found) = split_commands(body);
                    subs.extend(found);
                }
            }
        }
    }
    (out.join("\n"), subs)
}

/// The heredoc operators on one line, outside quotes (quote state carried
fn heredoc_operators(line: &str, quote: &mut Option<char>) -> Vec<(String, bool, bool)> {
    let b: Vec<char> = line.chars().collect();
    let mut found = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match *quote {
            Some('\'') => {
                if c == '\'' {
                    *quote = None;
                }
                i += 1;
                continue;
            }
            Some(q) => {
                if c == '\\' {
                    i += 2;
                    continue;
                }
                if c == q {
                    *quote = None;
                }
                i += 1;
                continue;
            }
            None => {}
        }
        if c == '\\' {
            i += 2;
            continue;
        }
        if c == '\'' || c == '"' {
            *quote = Some(c);
            i += 1;
            continue;
        }
        if c == '#' && (i == 0 || b[i - 1].is_whitespace()) {
            break;
        }
        if c == '<' && b.get(i + 1) == Some(&'<') {
            if b.get(i + 2) == Some(&'<') {
                i += 3;
                continue;
            }
            let mut j = i + 2;
            let dash = b.get(j) == Some(&'-');
            if dash {
                j += 1;
            }
            while b.get(j).is_some_and(|c| *c == ' ' || *c == '\t') {
                j += 1;
            }
            let (term, quoted, end) = heredoc_word(&b, j);
            if !term.is_empty() && (quoted || !term.starts_with(|c: char| c.is_ascii_digit())) {
                found.push((term, dash, quoted));
            }
            i = end.max(i + 2);
            continue;
        }
        i += 1;
    }
    found
}

/// The heredoc delimiter word starting at `b[j]`: `(delimiter, quoted, end)` —
fn heredoc_word(b: &[char], mut j: usize) -> (String, bool, usize) {
    let mut term = String::new();
    let mut quoted = false;
    while let Some(&c) = b.get(j) {
        match c {
            '\'' | '"' => {
                quoted = true;
                j += 1;
                while let Some(&inner) = b.get(j) {
                    j += 1;
                    if inner == c {
                        break;
                    }
                    term.push(inner);
                }
            }
            '\\' => {
                quoted = true;
                if let Some(&next) = b.get(j + 1) {
                    term.push(next);
                }
                j += 2;
            }
            c if c.is_whitespace() || ";&|<>()".contains(c) => break,
            c => {
                term.push(c);
                j += 1;
            }
        }
    }
    (term, quoted, j)
}

/// Split into simple-command segments, quote-aware, the way the shell reads
fn split_commands(cmd: &str) -> (Vec<String>, Vec<String>) {
    let b: Vec<char> = cmd.chars().collect();
    let n = b.len();
    let mut segs = Vec::new();
    let mut subs = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut case = CaseState::default();
    let mut i = 0;
    while i < n {
        let c = b[i];
        if quote.is_none() && case.want_pattern {
            if cur.trim().is_empty() && starts_word(&b, i, "esac") {
                // `esac` where a pattern could start: the construct ends here.
                case.want_pattern = false;
            } else if c == ')' {
                cur.clear();
                case.want_pattern = false;
                i += 1;
                continue;
            } else if c == '(' && cur.trim().is_empty() {
                i += 1;
                continue;
            } else if c == '|' {
                cur.push(c);
                i += 1;
                continue;
            }
        }
        if quote == Some('\'') {
            cur.push(c);
            if c == '\'' {
                quote = None;
            }
            i += 1;
            continue;
        }
        if c == '\\' && i + 1 < n {
            cur.push(c);
            cur.push(b[i + 1]);
            i += 2;
            continue;
        }
        if c == '$' && b.get(i + 1) == Some(&'(') && b.get(i + 2) != Some(&'(') {
            let mut depth = 1usize;
            let mut j = i + 2;
            while j < n && depth > 0 {
                match b[j] {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
            let end = if depth == 0 { j - 1 } else { j };
            subs.push(b[i + 2..end].iter().collect());
            cur.push_str("$SUB");
            i = j;
            continue;
        }
        if c == '`' {
            let close = b[i + 1..].iter().position(|x| *x == '`');
            let end = close.map_or(n, |p| i + 1 + p);
            subs.push(b[i + 1..end].iter().collect());
            cur.push_str("$SUB");
            i = end + 1;
            continue;
        }
        if quote == Some('"') {
            cur.push(c);
            if c == '"' {
                quote = None;
            }
            i += 1;
            continue;
        }
        if c == '\'' || c == '"' {
            quote = Some(c);
            cur.push(c);
            i += 1;
            continue;
        }
        if c == '$' && matches!(b.get(i + 1), Some('{' | '(')) {
            let open = b[i + 1];
            let end = matching_close(&b, i + 1, open, if open == '{' { '}' } else { ')' });
            let inner: String = b[(i + 2).min(end)..end.saturating_sub(1).max(i + 2)]
                .iter()
                .collect();
            subs.extend(split_commands(&inner).1);
            cur.extend(&b[i..end]);
            i = end;
            continue;
        }
        if c == '#' && cur.chars().last().is_none_or(|p| " \t\n;&|(".contains(p)) {
            while i < n && b[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c.is_whitespace() && is_case_header(&cur) {
            cur.clear();
            case.open += 1;
            case.want_pattern = true;
            i += 1;
            continue;
        }
        if c == ';' && matches!(b.get(i + 1), Some(';' | '&')) {
            close_segment(&mut segs, &mut cur, &mut case);
            i += 2;
            if b.get(i) == Some(&'&') {
                i += 1;
            }
            case.want_pattern = case.open > 0;
            continue;
        }
        if c == '&' && (cur.ends_with('>') || cur.ends_with('<') || b.get(i + 1) == Some(&'>')) {
            cur.push(c);
            i += 1;
            continue;
        }
        if (c == '{' || c == '}')
            && !(cur.trim().is_empty()
                && b.get(i + 1)
                    .is_none_or(|x| x.is_whitespace() || (c == '}' && ";&|)".contains(*x))))
        {
            cur.push(c);
            i += 1;
            continue;
        }
        if ";&|\n(){}".contains(c) {
            close_segment(&mut segs, &mut cur, &mut case);
            i += 1;
            continue;
        }
        cur.push(c);
        i += 1;
    }
    close_segment(&mut segs, &mut cur, &mut case);
    let segs = segs
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    (segs, subs)
}

/// [`split_commands`]' view of the `case` constructs around it.
#[derive(Default)]
struct CaseState {
    /// `case … in` headers read without their `esac`.
    open: usize,
    /// A pattern may start at the next word: after `in`, and after `;;`.
    want_pattern: bool,
}

/// End the current segment. An `esac` closes the innermost open `case`.
fn close_segment(segs: &mut Vec<String>, cur: &mut String, case: &mut CaseState) {
    let seg = std::mem::take(cur);
    if seg.split_whitespace().next() == Some("esac") {
        case.open = case.open.saturating_sub(1);
        case.want_pattern = false;
    }
    segs.push(seg);
}

/// Whether `cur` is a whole `case WORD in` header.
fn is_case_header(cur: &str) -> bool {
    let words: Vec<&str> = cur.split_whitespace().collect();
    words.len() >= 3 && words[0] == "case" && words[words.len() - 1] == "in"
}

/// Whether the word `word` starts at `b[i]` and ends there too (a separator,
fn starts_word(b: &[char], i: usize, word: &str) -> bool {
    let w: Vec<char> = word.chars().collect();
    b.get(i..i + w.len()).is_some_and(|s| s == w.as_slice())
        && b.get(i + w.len())
            .is_none_or(|x| x.is_whitespace() || ";&|)".contains(*x))
}

/// The index just past the `close` matching the `open` at `b[at]` (nesting
const fn matching_close(b: &[char], at: usize, open: char, close: char) -> usize {
    let mut depth = 0usize;
    let mut j = at;
    while j < b.len() {
        if b[j] == open {
            depth += 1;
        } else if b[j] == close {
            depth -= 1;
            if depth == 0 {
                return j + 1;
            }
        }
        j += 1;
    }
    b.len()
}

/// POSIX word splitting (quotes removed, backslashes resolved), as
fn words(segment: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = segment.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(x) => cur.push(x),
                        None => return None,
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.peek() {
                            Some(&x) if x == '"' || x == '\\' || x == '$' || x == '`' => {
                                cur.push(x);
                                chars.next();
                            }
                            _ => cur.push('\\'),
                        },
                        Some(x) => cur.push(x),
                        None => return None,
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(x) = chars.next()
                    && x != '\n'
                {
                    cur.push(x);
                }
            }
            c if c.is_whitespace() => {
                if in_word {
                    out.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        out.push(cur);
    }
    Some(out)
}

fn basename(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

fn is_assignment(word: &str) -> bool {
    let Some(eq) = word.find('=') else {
        return false;
    };
    let name = &word[..eq];
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Prefix words that run the command after them. The value-taking options of
fn wrapper_options(word: &str) -> Option<&'static [&'static str]> {
    Some(match word {
        "nohup" | "builtin" | "noglob" | "then" | "do" | "else" | "elif" | "if" | "while"
        | "until" | "!" | "setsid" | "unbuffer" => &[],
        "time" => &["-f", "-o", "--format", "--output"],
        "env" => &["-u", "-C", "-S", "--unset", "--chdir", "--split-string"],
        "nice" => &["-n", "--adjustment"],
        "exec" => &["-a"],
        "caffeinate" => &["-t", "-w"],
        "sudo" => &["-u", "-g", "-h", "-p", "-C", "-D", "-r", "-t", "-U", "-T"],
        "doas" => &["-u", "-C"],
        "timeout" => &["-s", "-k", "--signal", "--kill-after"],
        "stdbuf" => &["-i", "-o", "-e", "--input", "--output", "--error"],
        "ionice" => &[
            "-c",
            "-n",
            "-p",
            "-P",
            "-u",
            "--class",
            "--classdata",
            "--pid",
            "--pgid",
            "--uid",
        ],
        "taskset" => &[],
        "xargs" => &[
            "-n", "-I", "-L", "-P", "-d", "-E", "-s", "-a", "-J", "-R", "-S",
        ],
        "command" => &[],
        _ => return None,
    })
}

/// Why a segment is (or is not) a stock Rust tool. An unterminated quote is
fn offending(segment: &str, depth: usize, beside: Beside<'_>) -> Verdict {
    words(segment).map_or(Verdict::Clean, |words| {
        offending_words(&words, depth, beside)
    })
}

/// How many words a leading redirection occupies: `2>/dev/null` is one, `> out`
fn redirection_words(word: &str) -> Option<usize> {
    let op = word.trim_start_matches(|c: char| c.is_ascii_digit());
    let op = op.strip_prefix('&').unwrap_or(op);
    let body = op.trim_start_matches(['<', '>']);
    if body.len() == op.len() {
        return None;
    }
    let body = body.strip_prefix('&').unwrap_or(body);
    Some(if body.is_empty() { 2 } else { 1 })
}

fn offending_words(words: &[String], depth: usize, beside: Beside<'_>) -> Verdict {
    let mut i = 0;
    let mut reason: Option<String> = None;
    while let Some(w) = words.get(i) {
        if let Some(skip) = redirection_words(w) {
            i += skip;
            continue;
        }
        if is_assignment(w) {
            if let Some(value) = w
                .strip_prefix(STOCK_REASON)
                .and_then(|r| r.strip_prefix('='))
            {
                let value = value.trim();
                reason = (!value.is_empty()).then(|| value.to_string());
            }
            i += 1;
            continue;
        }
        let Some(options) = wrapper_options(basename(w)) else {
            break;
        };
        let wrapper = basename(w);
        i += 1;
        if wrapper == "command"
            && words
                .get(i)
                .is_some_and(|o| o.starts_with('-') && (o.contains('v') || o.contains('V')))
        {
            return Verdict::Clean;
        }
        while let Some(o) = words.get(i) {
            if o == "--" {
                i += 1;
                break;
            }
            if !o.starts_with('-') || o.len() < 2 {
                break;
            }
            i += if options.contains(&o.as_str()) { 2 } else { 1 };
        }
        if matches!(wrapper, "timeout" | "taskset") && words.get(i).is_some() {
            i += 1;
        }
    }
    match (tool_verdict(words, i, depth, beside), reason) {
        (Verdict::Stock(hit), Some(reason)) => Verdict::Escaped { hit, reason },
        (found, _) => found,
    }
}

/// The verdict for the command word at `words[i]` (every prefix word already
fn tool_verdict(words: &[String], i: usize, depth: usize, beside: Beside<'_>) -> Verdict {
    let Some(head_word) = words.get(i) else {
        return Verdict::Clean;
    };
    let mut tool_word = head_word.as_str();
    let mut head = basename(head_word);
    let mut rest = &words[i + 1..];
    let mut used_prefix = String::new();
    let mut run_channel: Option<&str> = None;
    if matches!(head, "bash" | "sh" | "zsh" | "dash" | "ksh") {
        return shell_command_string(rest)
            .map_or(Verdict::Clean, |script| check_at(script, depth + 1, beside));
    }
    if head == "eval" {
        return check_at(&join_words(rest), depth + 1, beside);
    }
    if head == "find" {
        return find_exec(rest, depth, beside);
    }
    if head == "rustup" {
        // `rustup run <toolchain> <tool> …` runs <tool> from that toolchain.
        if rest.len() >= 3 && rest[0] == "run" {
            used_prefix = String::from("rustup run ");
            used_prefix.push_str(&rest[1]);
            used_prefix.push(' ');
            run_channel = Some(rest[1].as_str());
            tool_word = &rest[2];
            head = basename(tool_word);
            rest = &rest[3..];
        } else {
            return Verdict::Clean;
        }
    }
    let mut plain_rest = Vec::with_capacity(rest.len());
    let mut j = 0;
    while let Some(w) = rest.get(j) {
        match redirection_words(w) {
            Some(skip) => j += skip,
            None => {
                plain_rest.push(w.clone());
                j += 1;
            }
        }
    }
    let rest = plain_rest.as_slice();
    let Some(mut instead) = trust_spelling(head, rest) else {
        return Verdict::Clean;
    };
    let channel = run_channel.or_else(|| explicit_toolchain(rest));
    if is_trust_alias(head) && channel.is_some_and(is_trust_channel) {
        return Verdict::Clean;
    }
    let toolchain_dir = dir_of(tool_word).filter(|dir| {
        COMPAT_TWINS
            .iter()
            .any(|(compat, twin)| *compat == head && beside(dir, twin))
    });
    if toolchain_dir.is_some() && is_trust_alias(head) {
        return Verdict::Clean;
    }
    let mut used = used_prefix;
    if let Some(dir) = toolchain_dir {
        used.push_str(dir);
        for spelling in &mut instead {
            if spelling.command.starts_with("targo") {
                spelling.command.insert_str(0, dir);
            }
        }
    }
    used.push_str(head);
    if head == "cargo" {
        if let Some(tc) = explicit_toolchain(rest) {
            used.push_str(" +");
            used.push_str(tc);
        }
        let plain = if explicit_toolchain(rest).is_some() {
            &rest[1..]
        } else {
            rest
        };
        if let Some(at) = cargo_verb_index(plain) {
            used.push(' ');
            used.push_str(&plain[at]);
        }
    } else if let Some(tc) = explicit_toolchain(rest) {
        used.push_str(" +");
        used.push_str(tc);
    }
    Verdict::Stock(Hit {
        used,
        instead,
        compat: head == "cargo"
            && (toolchain_dir.is_some() || channel.is_some_and(is_trust_channel)),
        toolchain_dir: toolchain_dir.map(str::to_string),
        channel: channel.map(str::to_string),
        unread: false,
    })
}

/// Whether `name` is a Trust toolchain's compatibility name for its own compiler
fn is_trust_alias(name: &str) -> bool {
    name != "cargo" && COMPAT_TWINS.iter().any(|(compat, _)| *compat == name)
}

/// The command string of `sh -c '…'` (any flag cluster carrying `c`, e.g.
fn shell_command_string(rest: &[String]) -> Option<&str> {
    let mut saw_c = false;
    let mut i = 0;
    while let Some(w) = rest.get(i) {
        if w == "--" {
            i += 1;
            break;
        }
        if (w.starts_with('-') || w.starts_with('+')) && w.len() > 1 {
            if w.starts_with("--") {
                i += 1;
                continue;
            }
            if w[1..].contains('c') {
                saw_c = true;
            }
            i += if w[1..].ends_with('o') || w[1..].ends_with('O') {
                2
            } else {
                1
            };
            continue;
        }
        break;
    }
    if !saw_c {
        return None;
    }
    rest.get(i).map(String::as_str)
}

/// `find … -exec <cmd> … ;` (and `-execdir`, `-ok`, `-okdir`): the command runs.
fn find_exec(rest: &[String], depth: usize, beside: Beside<'_>) -> Verdict {
    let mut found = Verdict::Clean;
    let mut i = 0;
    while i < rest.len() {
        if matches!(rest[i].as_str(), "-exec" | "-execdir" | "-ok" | "-okdir") {
            let start = i + 1;
            let mut end = start;
            while end < rest.len() && rest[end] != ";" && rest[end] != "+" {
                end += 1;
            }
            if fold(
                &mut found,
                offending_words(&rest[start..end], depth, beside),
            ) {
                return found;
            }
            i = end;
        }
        i += 1;
    }
    found
}

/// The longest a rendered command may be in a [`refusal`].
pub const REFUSAL_COMMAND_CAP: usize = 240;

/// `text` as one line of at most [`REFUSAL_COMMAND_CAP`] bytes: every control
fn one_command_line(text: &str) -> String {
    let folded: String = text
        .chars()
        .map(|c| {
            if c.is_control() || c == '\u{2028}' || c == '\u{2029}' {
                ' '
            } else {
                c
            }
        })
        .collect();
    if folded.len() <= REFUSAL_COMMAND_CAP {
        return folded;
    }
    let mut end = REFUSAL_COMMAND_CAP;
    while !folded.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &folded[..end])
}

/// What a guard says back to the agent whose command it refused.
#[must_use]
pub fn refusal(hit: &Hit, pin: Option<(&PinSource, &str)>) -> String {
    let used = one_command_line(&hit.used);
    let mut out = if hit.unread {
        format!(
            "`{used}` is nested more than {MAX_DEPTH} levels deep (`$( )`, backticks, `bash -c`, \
             `eval`), past what this reader follows, so it is refused unread. Flatten the command \
             so the tool stands where it can be read.\n"
        )
    } else if hit.toolchain_dir.is_some() {
        format!(
            "`{used}` is a Trust toolchain's compatibility cargo: upstream cargo's surface, \
             which names no lane.\n"
        )
    } else if let Some(channel) = hit.channel.as_deref().filter(|_| hit.compat) {
        format!(
            "`{used}` is the compatibility cargo of rustup's {} toolchain: upstream cargo's \
             surface, which names no lane.\n",
            one_command_line(channel)
        )
    } else {
        format!(
            "`{used}` is stock Rust. Rust here is the Trust toolchain, even under a stock \
             rust-toolchain.toml pin: that pin moves only rustup's proxies (cargo, rustc, rustfmt, \
             cargo-clippy), never targo, tippy, trustfmt or trustdoc.\n"
        )
    };
    if let Some((source, channel)) = pin
        .filter(|(_, ch)| !is_trust_channel(ch))
        .filter(|_| !hit.compat && hit.channel.is_none() && !hit.unread)
    {
        let channel = one_command_line(channel);
        out.push_str(&match source {
            PinSource::File(file) => format!(
                "This directory pins stock \"{channel}\" ({}); build it with the Trust spelling \
                 all the same.\n",
                one_command_line(&file.display().to_string())
            ),
            PinSource::Env => format!(
                "$RUSTUP_TOOLCHAIN names stock \"{channel}\"; build with the Trust spelling all \
                 the same.\n"
            ),
        });
    }
    if is_unknown_verb(&hit.instead) {
        out.push_str(
            "targo may not have this verb (`targo nextest` and `targo deny` answer \"no such \
             command\"); if it does, the Trust spelling is:\n",
        );
    } else if hit.unread {
        out.push_str("The Trust tool for it:\n");
    } else if hit.toolchain_dir.is_some() {
        out.push_str("Run that toolchain's targo:\n");
    } else {
        out.push_str("Run the Trust spelling of the same command:\n");
    }
    let bounded: Vec<Spelling> = hit
        .instead
        .iter()
        .map(|s| Spelling {
            label: s.label,
            command: one_command_line(&s.command),
            note: s.note,
        })
        .collect();
    out.push_str(&render_spellings(&bounded, "  "));
    if is_unknown_verb(&hit.instead) {
        out.push_str(
            "If it answers \"no such command\", the extension has no Trust spelling yet: use the \
             escape below.\n",
        );
    }
    if hit.compat {
        out.push_str(&format!(
            "If upstream cargo's surface is genuinely required, put {STOCK_REASON}='<why>' in \
             front of the command and say why in your reply."
        ));
    } else {
        out.push_str(&format!(
            "If stock Rust is genuinely required, put {STOCK_REASON}='<why>' in front of the \
             command and say why in your reply. `aterm help rust` measures what this directory gets."
        ));
    }
    out
}

#[cfg(test)]
#[path = "rust_lane_tests.rs"]
mod tests;
