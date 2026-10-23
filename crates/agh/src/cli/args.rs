// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The command line (design §9.1, §9.2).

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::time::Duration;

use super::verbs::{Class, FlagSpec, GLOBAL_FLAGS, REFUSED, Takes, VERBS, Verb};
use crate::error::{Error, Result};
use crate::github::How;
use crate::page::{Consistency, ListOpts};
use crate::policy::{Access, ExplicitTarget, Named, Router};
use crate::slug::{ContentRef, RepoArg, Slug};

/// The longest `--timeout`: a day. It becomes every request's backstop, so an unbounded value
/// would reach a deadline no clock can hold.
const TIMEOUT_MAX: Duration = Duration::from_secs(24 * 3600);

/// `--format`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Format {
    /// One compact JSON document.
    #[default]
    Json,
    /// One compact JSON value per line.
    Ndjson,
    /// Tab-separated rows.
    Tsv,
    /// Padded columns.
    Text,
}

impl Format {
    /// `json`, `ndjson`, `tsv`, `text`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Format> {
        match text {
            "json" => Some(Format::Json),
            "ndjson" => Some(Format::Ndjson),
            "tsv" => Some(Format::Tsv),
            "text" => Some(Format::Text),
            _ => None,
        }
    }

    /// The format as `parse` reads it.
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Format::Json => "json",
            Format::Ndjson => "ndjson",
            Format::Tsv => "tsv",
            Format::Text => "text",
        }
    }
}

/// The global flags (design §9.2).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Global {
    /// `-R`, `--repo`.
    pub repo: Option<String>,
    /// `--as`, `--profile`.
    pub as_profile: Option<String>,
    /// `--anonymous`.
    pub anonymous: bool,
    /// `--timeout`.
    pub timeout: Option<Duration>,
    /// `--meta`.
    pub meta: bool,
    /// `--format`.
    pub format: Format,
    /// `--header`.
    pub header: bool,
    /// `--fields`.
    pub fields: Option<String>,
    /// `--raw`.
    pub raw: bool,
    /// `-q`, `--jq`.
    pub jq: Option<String>,
    /// `--limit`.
    pub limit: Option<usize>,
    /// `--partial`.
    pub partial: bool,
    /// `--as-read`.
    pub as_read: bool,
    /// `--max-pages`.
    pub max_pages: Option<u32>,
    /// `--no-probe`.
    pub no_probe: bool,
    /// `--record PATH`: tape this run into a new file (plan §4.6.28).
    pub record: Option<std::path::PathBuf>,
    /// `--replay PATH`: run from a tape, offline (plan §4.6.28).
    pub replay: Option<std::path::PathBuf>,
}

/// One parsed invocation of a verb.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invocation {
    /// The verb's path (`["release", "view"]`).
    pub path: Vec<&'static str>,
    /// The positionals, in order (without REPO under `-R`).
    pub positionals: Vec<String>,
    /// The verb's own flags in the order given, each use of a Repeat flag separately.
    pub flags: Vec<(&'static str, Option<String>)>,
    /// The global flags.
    pub global: Global,
}

/// `agh help {verb}`, with the path's words.
pub(crate) fn help_next(path: &[&str]) -> String {
    if path.is_empty() {
        "agh help".to_string()
    } else {
        format!("agh help {}", path.join(" "))
    }
}

/// The repository `text` names, and how it was named.
pub(crate) fn resolve_repo(text: &str, router: &Router) -> Result<(Slug, Named)> {
    match RepoArg::parse(text)? {
        RepoArg::Literal(slug) => {
            let literal = ExplicitTarget::from_argv(OsStr::new(text))?;
            Ok((slug, Named::Literal(literal)))
        }
        RepoArg::Target(name) => resolve_target(name, router),
    }
}

/// A `[targets]` row.
fn resolve_target(name: String, router: &Router) -> Result<(Slug, Named)> {
    match router.target(&name) {
        Some(slug) => Ok((slug.clone(), Named::Target(name))),
        None => Err(Error::usage(format!(
            "no target named @{name} in agh.toml [targets]"
        ))),
    }
}

/// The usage error of a verb with no repository.
fn no_repo() -> Error {
    Error::usage("name the repository: OWNER/NAME or @TARGET")
}

impl Invocation {
    /// The last value of `name`.
    #[must_use]
    pub fn flag(&self, name: &str) -> Option<&str> {
        self.flags
            .iter()
            .rev()
            .find(|(n, _)| *n == name)
            .and_then(|(_, v)| v.as_deref())
    }

    /// Every value of a Repeat flag, in order.
    #[must_use]
    pub fn flag_all(&self, name: &str) -> Vec<&str> {
        self.flags
            .iter()
            .filter(|(n, _)| *n == name)
            .filter_map(|(_, v)| v.as_deref())
            .collect()
    }

    /// Whether `name` was given.
    #[must_use]
    pub fn has(&self, name: &str) -> bool {
        self.flags.iter().any(|(n, _)| *n == name)
    }

    /// Positional `i`.
    #[must_use]
    pub fn positional(&self, i: usize) -> Option<&str> {
        self.positionals.get(i).map(String::as_str)
    }

    /// Positional `i`, which must be there.
    /// # Errors
    /// Usage `{what} is required`, next `agh help {verb}`.
    pub fn require(&self, i: usize, what: &str) -> Result<&str> {
        self.positional(i)
            .ok_or_else(|| self.usage(format!("{what} is required")))
    }

    /// A usage error of this verb: `why`, next `agh help {verb}`.
    pub(crate) fn usage(&self, why: impl Into<String>) -> Error {
        Error::usage(why).with_next(help_next(&self.path))
    }

    /// For a verb whose first positional is REPO.
    /// # Errors
    /// Usage, next `agh help {verb}`.
    pub(crate) fn repo(&self, router: &Router) -> Result<(Slug, Named)> {
        self.named_repo(router)
            .map_err(|e| e.with_next(help_next(&self.path)))
    }

    /// `repo`, before its errors take the verb's next.
    fn named_repo(&self, router: &Router) -> Result<(Slug, Named)> {
        let text = match &self.global.repo {
            Some(text) => text.as_str(),
            None => self.positional(0).ok_or_else(no_repo)?,
        };
        resolve_repo(text, router)
    }

    /// For a verb whose first positional is CONTENT.
    /// # Errors
    /// As `repo`, and `ContentRef::parse`'s errors.
    pub(crate) fn content(&self, router: &Router) -> Result<(Slug, Named, ContentRef)> {
        self.named_content(router)
            .map_err(|e| e.with_next(help_next(&self.path)))
    }

    /// `content`, before its errors take the verb's next.
    fn named_content(&self, router: &Router) -> Result<(Slug, Named, ContentRef)> {
        if let Some(text) = &self.global.repo {
            let (slug, named) = resolve_repo(text, router)?;
            let at = match self.positional(0) {
                Some(text) => ContentRef::parse(text, false)?,
                None => ContentRef::default(),
            };
            return Ok((slug, named, at));
        }
        let text = self.positional(0).ok_or_else(no_repo)?;
        let at = ContentRef::parse(text, true)?;
        let (slug, named) = match &at.repo {
            Some(RepoArg::Literal(slug)) => {
                let literal = ExplicitTarget::from_argv(OsStr::new(&slug.to_string()))?;
                (slug.clone(), Named::Literal(literal))
            }
            Some(RepoArg::Target(name)) => resolve_target(name.clone(), router)?,
            None => return Err(no_repo()),
        };
        Ok((slug, named, at))
    }

    /// The positionals after REPO or CONTENT.
    #[must_use]
    pub fn rest(&self) -> &[String] {
        let content_first = super::verbs::find(&self.path)
            .is_some_and(|verb| verb.positionals.first() == Some(&"CONTENT"));
        if self.global.repo.is_some() && !content_first {
            &self.positionals
        } else {
            self.positionals.get(1..).unwrap_or(&[])
        }
    }

    /// `release.view`.
    #[must_use]
    pub fn verb_name(&self) -> String {
        verb_name(&self.path)
    }

    /// `How { access, anonymous: --anonymous, as_profile: --as, named, no_probe: --no-probe, retry:
    /// None }`.
    pub(crate) fn how(&self, access: Access, named: Named) -> How {
        let mut how = How::read()
            .anonymous(self.global.anonymous)
            .as_profile(self.global.as_profile.clone())
            .named(named)
            .no_probe(self.global.no_probe);
        how.access = access;
        how
    }

    /// `ListOpts::STANDARD`, with `--limit`, `--partial`, `--max-pages`, and AsRead under
    /// `--as-read`.
    #[must_use]
    pub const fn list_opts(&self) -> ListOpts {
        let mut opts = ListOpts::STANDARD
            .with_limit(self.global.limit)
            .with_partial(self.global.partial);
        if let Some(pages) = self.global.max_pages {
            opts = opts.with_max_pages(pages);
        }
        if self.global.as_read {
            opts = opts.with_consistency(Consistency::AsRead);
        }
        opts
    }
}

/// What a command line asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Parsed {
    /// `agh` alone, or global flags with no verb word and no `--replay`.
    Bare,
    /// `-h` or `--help`: the words of the verb path (or every word typed, when no verb matched).
    Help(Vec<String>),
    /// `--replay PATH` with no verb word: run the tape's own command line (plan §4.6.28).
    ReplayTaped(Global),
    /// A verb to run.
    Run(&'static Verb, Invocation),
}

/// A command line that did not parse, and the verb it named (`-` when none).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// The verb, dotted, or `-`.
    pub verb: String,
    /// Why.
    pub error: Error,
}

/// `["release", "view"]` → `release.view`; empty → `-`.
#[must_use]
pub fn verb_name(path: &[&str]) -> String {
    if path.is_empty() {
        "-".to_string()
    } else {
        path.join(".")
    }
}

/// Rule 6 (what a tape cannot hold), for `verb` under `global`.
/// # Errors
/// Usage, next `agh help {verb}`.
pub(crate) fn tape_rule(verb: &Verb, global: &Global) -> Result<()> {
    if global.record.is_none() && global.replay.is_none() {
        return Ok(());
    }
    let usage = |why: String| Error::usage(why).with_next(help_next(verb.path));
    if global.as_profile.is_some() {
        return Err(usage("M1's tapes are anonymous: drop --as".to_string()));
    }
    if matches!(verb.class, Class::Local | Class::Provision) {
        return Err(usage(format!(
            "{} cannot be taped: it answers from this machine, not from GitHub",
            verb.path.join(" ")
        )));
    }
    if verb.path == ["release", "download"] {
        return Err(usage(
            "release download cannot be taped in M1: it streams a body".to_string(),
        ));
    }
    Ok(())
}

/// `30s`, `2m`, `1h`, or bare seconds: a positive integer.
#[must_use]
pub fn parse_duration(text: &str) -> Option<Duration> {
    let (digits, unit) = match text.char_indices().last()? {
        (i, 's') => (&text[..i], 1),
        (i, 'm') => (&text[..i], 60),
        (i, 'h') => (&text[..i], 3600),
        _ => (text, 1),
    };
    let n: u64 = crate::path::digits(digits)?;
    if n == 0 {
        return None;
    }
    Some(Duration::from_secs(n.checked_mul(unit)?))
}

/// Every verb path and refused path.
fn all_paths() -> impl Iterator<Item = &'static [&'static str]> {
    VERBS
        .iter()
        .map(|v| v.path)
        .chain(REFUSED.iter().map(|r| r.path))
}

/// Whether some path starts with `words`.
fn some_path_starts_with(words: &[String]) -> bool {
    all_paths().any(|path| {
        path.len() >= words.len() && path.iter().zip(words).all(|(p, w)| *p == w.as_str())
    })
}

/// Whether `words` is exactly `path`.
fn is_path(words: &[String], path: &[&str]) -> bool {
    path.len() == words.len() && path.iter().zip(words).all(|(p, w)| *p == w.as_str())
}

/// The spec a flag token names, among `specs`: `--name`, or `-c` for a short.
fn lookup<'a>(specs: &'a [FlagSpec], name: &str) -> Option<&'a FlagSpec> {
    specs.iter().find(|spec| {
        spec.name == name
            || spec.short.is_some_and(|c| {
                let mut chars = name.chars();
                chars.next() == Some('-') && chars.next() == Some(c) && chars.next().is_none()
            })
    })
}

/// Whether the token names a flag that takes a value, in any table.
fn takes_value_anywhere(name: &str) -> bool {
    let takes = |spec: &FlagSpec| spec.takes != Takes::None;
    lookup(GLOBAL_FLAGS, name).is_some_and(takes)
        || VERBS
            .iter()
            .any(|verb| lookup(verb.flags, name).is_some_and(takes))
}

/// A flag token's name and inline value: `--name=value` splits; a short flag never does.
fn split_flag(arg: &str) -> (&str, Option<&str>) {
    if arg.starts_with("--") {
        match arg.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (arg, None),
        }
    } else {
        (arg, None)
    }
}

/// Whether `arg` is a flag token (a lone `-` is a positional).
fn is_flag(arg: &str) -> bool {
    arg.len() > 1 && arg.starts_with('-')
}

/// Rule 1's pre-scan: `-h` or `--help` before any `--`, and the words typed.
fn help_words(args: &[String]) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut help = false;
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        i += 1;
        if arg == "--" {
            break;
        }
        if arg == "-h" || arg == "--help" {
            help = true;
        } else if is_flag(arg) {
            let (name, inline) = split_flag(arg);
            if inline.is_none() && takes_value_anywhere(name) {
                i += 1;
            }
        } else {
            words.push(arg.clone());
        }
    }
    if !help {
        return None;
    }
    for k in 1..=words.len() {
        if all_paths().any(|path| is_path(&words[..k], path)) {
            words.truncate(k);
            break;
        }
    }
    Some(words)
}

/// The parser's state.
struct Parser {
    path: Vec<&'static str>,
    verb: Option<&'static Verb>,
    words: Vec<String>,
    positionals: Vec<String>,
    flags: Vec<(&'static str, Option<String>)>,
    global: Global,
    seen: BTreeSet<&'static str>,
}

impl Parser {
    /// A usage error naming the verb as known so far.
    fn usage(&self, why: impl Into<String>) -> ParseError {
        ParseError {
            verb: verb_name(&self.path),
            error: Error::usage(why).with_next(help_next(&self.path)),
        }
    }

    /// A word before the verb is known: rule 3.
    fn word(&mut self, word: &str) -> std::result::Result<(), ParseError> {
        self.words.push(word.to_string());
        if let Some(refused) = REFUSED.iter().find(|r| is_path(&self.words, r.path)) {
            return Err(ParseError {
                verb: verb_name(refused.path),
                error: Error::refused(format!(
                    "{} is refused: {}",
                    refused.path.join(" "),
                    refused.why
                ))
                .with_next(refused.next),
            });
        }
        if let Some(verb) = VERBS.iter().find(|v| is_path(&self.words, v.path)) {
            self.verb = Some(verb);
            self.path = verb.path.to_vec();
            return Ok(());
        }
        if !some_path_starts_with(&self.words) {
            return Err(ParseError {
                verb: "-".to_string(),
                error: Error::usage(format!("unknown verb: {}", self.words.join(" ")))
                    .with_next("agh help"),
            });
        }
        Ok(())
    }

    /// One flag, its value taken from `inline` or `next`; whether `next` was consumed.
    fn flag(
        &mut self,
        token: &str,
        inline: Option<&str>,
        next: Option<&String>,
    ) -> std::result::Result<bool, ParseError> {
        let global = lookup(GLOBAL_FLAGS, token);
        let own = self.verb.and_then(|verb| lookup(verb.flags, token));
        let Some(spec) = global.or(own) else {
            return Err(self.usage(format!("unknown flag {token}")));
        };
        let (value, consumed) = match (spec.takes, inline) {
            (Takes::None, Some(_)) => {
                return Err(self.usage(format!("{} takes no value", spec.name)));
            }
            (Takes::None, None) => (None, false),
            (_, Some(value)) => (Some(value.to_string()), false),
            (_, None) => match next {
                Some(value) => (Some(value.clone()), true),
                None => return Err(self.usage(format!("{} needs a value", spec.name))),
            },
        };
        let key = if spec.name == "--profile" {
            "--as"
        } else {
            spec.name
        };
        if spec.takes != Takes::Repeat && !self.seen.insert(key) {
            return Err(self.usage(format!("{} given twice", spec.name)));
        }
        if global.is_some() {
            self.set_global(spec.name, value)?;
        } else {
            self.flags.push((spec.name, value));
        }
        Ok(consumed)
    }

    /// Rule 5's values, into `Global`.
    fn set_global(
        &mut self,
        name: &'static str,
        value: Option<String>,
    ) -> std::result::Result<(), ParseError> {
        let text = value.unwrap_or_default();
        match name {
            "--repo" => self.global.repo = Some(text),
            "--as" | "--profile" => self.global.as_profile = Some(text),
            "--anonymous" => self.global.anonymous = true,
            "--timeout" => {
                let timeout = parse_duration(&text)
                    .filter(|d| *d <= TIMEOUT_MAX)
                    .ok_or_else(|| {
                        self.usage("--timeout wants 30s, 2m, 1h or seconds, at most 24h")
                    })?;
                self.global.timeout = Some(timeout);
            }
            "--meta" => self.global.meta = true,
            "--format" => {
                self.global.format = Format::parse(&text)
                    .ok_or_else(|| self.usage("--format wants json, ndjson, tsv or text"))?;
            }
            "--header" => self.global.header = true,
            "--fields" => self.global.fields = Some(text),
            "--raw" => self.global.raw = true,
            "--jq" => self.global.jq = Some(text),
            "--limit" => {
                let limit = text
                    .parse::<usize>()
                    .ok()
                    .filter(|n| *n >= 1)
                    .ok_or_else(|| self.usage("--limit wants a whole number of at least 1"))?;
                self.global.limit = Some(limit);
            }
            "--partial" => self.global.partial = true,
            "--as-read" => self.global.as_read = true,
            "--max-pages" => {
                let pages = text
                    .parse::<u32>()
                    .ok()
                    .filter(|n| (1..=1000).contains(n))
                    .ok_or_else(|| self.usage("--max-pages wants 1 to 1000"))?;
                self.global.max_pages = Some(pages);
            }
            "--no-probe" => self.global.no_probe = true,
            "--record" | "--replay" => {
                if text.is_empty() {
                    return Err(self.usage(format!("{name} needs a path")));
                }
                let path = Some(std::path::PathBuf::from(text));
                if name == "--record" {
                    self.global.record = path;
                } else {
                    self.global.replay = path;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Rule 5's combinations.
    fn combinations(&self) -> std::result::Result<(), ParseError> {
        let g = &self.global;
        if g.as_profile.is_some() && g.anonymous {
            return Err(self.usage("--as and --anonymous contradict each other"));
        }
        if g.jq.is_some() && g.format != Format::Json {
            return Err(self.usage("--jq prints its own lines: drop --format"));
        }
        if g.record.is_some() && g.record == g.replay {
            return Err(self.usage("--record and --replay name the same file"));
        }
        Ok(())
    }

    /// Rule 6: what a tape cannot hold.
    fn tapes(&self, verb: &Verb) -> std::result::Result<(), ParseError> {
        tape_rule(verb, &self.global).map_err(|error| ParseError {
            verb: verb_name(verb.path),
            error,
        })
    }

    /// Rule 7: the positionals against the verb's names.
    fn positionals(&mut self, verb: &Verb) -> std::result::Result<(), ParseError> {
        let mut names: Vec<&str> = verb.positionals.to_vec();
        let mut first_optional = false;
        if self.global.repo.is_some() {
            match names.first() {
                Some(&"REPO") => {
                    names.remove(0);
                }
                Some(&"CONTENT") => first_optional = true,
                _ if verb.path == ["api"] => {}
                _ => {
                    return Err(self.usage(format!("{} takes no -R", verb.path.join(" "))));
                }
            }
        }
        let mut at = 0;
        for (i, name) in names.iter().enumerate() {
            if name.ends_with("…?") {
                at = self.positionals.len();
            } else if let Some(base) = name.strip_suffix('…') {
                if at >= self.positionals.len() {
                    return Err(self.usage(format!("{base} is required")));
                }
                at = self.positionals.len();
            } else if name.ends_with('?') {
                if at < self.positionals.len() {
                    at += 1;
                }
            } else if at < self.positionals.len() {
                at += 1;
            } else if i == 0 && first_optional {
                // Under -R, CONTENT may be left out: the repository's root at its default
                // branch.
            } else {
                return Err(self.usage(format!("{name} is required")));
            }
        }
        if let Some(extra) = self.positionals.get(at) {
            return Err(self.usage(format!("unexpected argument: {extra}")));
        }
        Ok(())
    }
}

/// Parse `args` (design §9.1 and the plan's rules 1 to 7).
/// # Errors
/// As the rules say.
pub fn parse(args: &[String]) -> std::result::Result<Parsed, ParseError> {
    parse_noting_tapes(args, &mut Vec::new())
}

/// The indices of `args` that `parse` reads as a tape flag.
pub(crate) fn tape_tokens(args: &[String]) -> Vec<usize> {
    let mut at = Vec::new();
    let _ = parse_noting_tapes(args, &mut at);
    at
}

/// `parse`, noting in `tape_at` the indices `tape_tokens` names.
fn parse_noting_tapes(
    args: &[String],
    tape_at: &mut Vec<usize>,
) -> std::result::Result<Parsed, ParseError> {
    if args.is_empty() {
        return Ok(Parsed::Bare);
    }
    if let Some(words) = help_words(args) {
        return Ok(Parsed::Help(words));
    }
    let mut p = Parser {
        path: Vec::new(),
        verb: None,
        words: Vec::new(),
        positionals: Vec::new(),
        flags: Vec::new(),
        global: Global::default(),
        seen: BTreeSet::new(),
    };
    let mut dashdash = false;
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        i += 1;
        if dashdash {
            p.positionals.push(arg.clone());
        } else if arg == "--" {
            dashdash = true;
        } else if is_flag(arg) {
            let (token, inline) = split_flag(arg);
            let consumed = p.flag(token, inline, args.get(i))?;
            if matches!(token, "--record" | "--replay") {
                tape_at.push(i - 1);
                if consumed {
                    tape_at.push(i);
                }
            }
            if consumed {
                i += 1;
            }
        } else if p.verb.is_none() {
            p.word(arg)?;
        } else {
            p.positionals.push(arg.clone());
        }
    }
    p.combinations()?;
    let Some(verb) = p.verb else {
        if p.words.is_empty() {
            if let Some(extra) = p.positionals.first() {
                return Err(ParseError {
                    verb: "-".to_string(),
                    error: Error::usage(format!("unknown verb: {extra}")).with_next("agh help"),
                });
            }
            if p.global.replay.is_some() {
                if p.global.as_profile.is_some() {
                    return Err(p.usage("M1's tapes are anonymous: drop --as"));
                }
                if let Some(flag) = p
                    .seen
                    .iter()
                    .find(|k| !matches!(**k, "--record" | "--replay"))
                {
                    return Err(p.usage(format!(
                        "a bare --replay runs the tape's own command line: drop {flag}"
                    )));
                }
                return Ok(Parsed::ReplayTaped(p.global));
            }
            if p.global.record.is_some() {
                return Err(p.usage("--record needs a verb to record"));
            }
            return Ok(Parsed::Bare);
        }
        let words = p.words.join(" ");
        let paths: Vec<String> = all_paths()
            .filter(|path| {
                path.len() > p.words.len()
                    && path.iter().zip(&p.words).all(|(a, b)| *a == b.as_str())
            })
            .map(|path| path.join(" "))
            .collect();
        return Err(ParseError {
            verb: "-".to_string(),
            error: Error::usage(format!("{words} needs a verb: {}", paths.join(", ")))
                .with_next(format!("agh help {words}")),
        });
    };
    p.tapes(verb)?;
    p.positionals(verb)?;
    Ok(Parsed::Run(
        verb,
        Invocation {
            path: p.path,
            positionals: p.positionals,
            flags: p.flags,
            global: p.global,
        },
    ))
}

#[cfg(test)]
#[path = "args_tests.rs"]
mod tests;
