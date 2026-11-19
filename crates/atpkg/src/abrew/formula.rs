// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Formula and cask models parsed from formulae.brew.sh JSON.

use std::cmp::Ordering;

use aterm_json::Value;

use super::machine::Machine;

/// A formula parsed from formulae.brew.sh JSON.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Formula {
    pub name: String,
    pub full_name: String,
    pub desc: String,
    pub homepage: String,
    pub license: String,
    pub version: String,
    pub revision: u64,
    pub stable_url: String,
    pub stable_sha256: String,
    /// Bottle files by tag (`arm64_tahoe`, `all`), sorted.
    pub bottle_files: Vec<Bottle>,
    /// The bottle block's `rebuild` counter (the `.bottle.N.` in filenames).
    pub bottle_rebuild: u64,
    /// Why no bottle of this formula pours natively whatever its cellar.
    pub pour_needs: Option<String>,
    /// `conflicts_with` names, evaluated against abrew's census at install.
    pub conflicts: Vec<String>,
    /// `conflicts_with_reasons`, parallel to `conflicts`
    /// (empty string where the record gives none).
    pub conflict_reasons: Vec<String>,
    /// Requirements in the plain data forms abrew evaluates at install.
    pub requirements: Vec<Requirement>,
    /// `pour_bottle_only_if: :clt_installed`.
    pub pour_requires_clt: bool,
    /// Per-system deviations, resolved by `resolve_variations` before use.
    pub variations: Vec<(String, FormulaVariation)>,
    pub dependencies: Vec<String>,
    pub build_dependencies: Vec<String>,
    /// Tag-side dependency lists, read by `uses` filters (the JSON
    /// lists are predicate projections of brew's merged deps).
    pub test_dependencies: Vec<String>,
    pub recommended_dependencies: Vec<String>,
    pub optional_dependencies: Vec<String>,
    /// `version_scheme` (keg ordering sorts by it before version).
    pub version_scheme: u64,
    /// `compatibility_version` when the formula declares one.
    pub compatibility_version: Option<u64>,
    pub keg_only: bool,
    /// `keg_only_reason` as (reason, explanation): the reason is a
    /// `:symbol` (inspect form) or free text.
    pub keg_only_reason: Option<(String, String)>,
    /// The `service` block, when the formula defines one with a run
    /// command (display data for caveats; services stay delegated).
    pub service: Option<Box<ServiceInfo>>,
    /// The `livecheck` block parsed as data from a tap file (the
    /// API JSON never carries it; `brew livecheck` reads the same
    /// tap files). `None` = no block or not data-parseable.
    pub livecheck: Option<Box<LivecheckSpec>>,
    /// The formula opts out of autobump (`no_autobump!`, or the
    /// API object's `autobump: false`). Brew's default is
    /// autobumped, hence the inverted, default-false field.
    pub no_autobump: bool,
    /// The API object's `skip_livecheck` (its `livecheck` block
    /// declares `skip`); tap records derive it from the block.
    pub skip_livecheck: bool,
    pub deprecate: bool,
    pub disable: bool,
    pub caveats: String,
    pub aliases: Vec<String>,
    pub oldnames: Vec<String>,
    /// A `head` spec exists (`urls.head` in the API object).
    pub head: bool,
    pub head_url: String,
    /// Declared build options as (flag, description) pairs.
    pub options: Vec<(String, String)>,
    pub deprecate_disable: DeprecateDisable,
    /// The API `executables` list (bottle PATH executables).
    pub executables: Vec<String>,
}

/// A formula `service` block reduced to what caveats display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceInfo {
    /// The `run` argv (API strings may carry `$HOMEBREW_PREFIX`).
    pub run: Vec<String>,
    /// `require_root: true` (a LaunchDaemon / system service).
    pub require_root: bool,
    /// `environment_variables` as (key, value) pairs, sorted by key
    /// (the JSON map abrew reads is alphabetical).
    pub environment_variables: Vec<(String, String)>,
    /// An explicit macOS service name (`name macos: ...`),
    /// overriding the default `sh.brew.<name>` label.
    pub name: Option<String>,
    /// `working_dir`, with `$HOMEBREW_PREFIX` placeholders.
    pub working_dir: Option<String>,
    /// `root_dir`, with `$HOMEBREW_PREFIX` placeholders.
    pub root_dir: Option<String>,
    /// `log_path`, with `$HOMEBREW_PREFIX` placeholders.
    pub log_path: Option<String>,
    /// `error_log_path`, with `$HOMEBREW_PREFIX` placeholders.
    pub error_log_path: Option<String>,
    /// `interval` seconds.
    pub interval: Option<i64>,
    /// `cron` schedule text.
    pub cron: Option<String>,
}

/// A `livecheck` block reduced to data (see
/// `tap::parse_livecheck_block` for the accepted grammar; the
/// full brew DSL also allows a strategy block of arbitrary
/// Ruby, recorded here only as `strategy_block`).
/// Stored in a spec regex where the tap wrote
/// `#{Regexp.escape(version.major_minor)}`: replaced
/// with the checked unit's escaped major.minor at
/// plan time.
pub const MAJOR_MINOR_SENTINEL: &str = "\u{1}";

/// The method names a `version…` interpolation
/// chain may use (`Cask::DSL::Version`'s surface
/// as the taps exercise it).
const VERSION_CHAIN_METHODS: &[&str] = &[
    "major",
    "minor",
    "patch",
    "major_minor",
    "major_minor_patch",
    "minor_patch",
    "csv",
    "first",
    "second",
    "third",
    "fourth",
    "fifth",
    "last",
    "before_comma",
    "after_comma",
    "no_dots",
    "no_hyphens",
    "no_underscores",
    "no_dividers",
    "dots_to_hyphens",
    "dots_to_underscores",
    "hyphens_to_dots",
    "hyphens_to_underscores",
    "underscores_to_dots",
    "underscores_to_hyphens",
];

/// Whether `token` is a `version(.method)*`
/// interpolation chain (syntax only).
#[must_use]
pub fn version_chain_syntax(token: &str) -> bool {
    let mut parts = token.split('.');
    if parts.next() != Some("version") {
        return false;
    }
    parts.all(|m| VERSION_CHAIN_METHODS.contains(&m))
}

/// One `MAJOR_MINOR_PATCH_REGEX` component of a
/// cask version (`^([^.,:]+)(?:.([^.,:]+)…` — the
/// separators are single arbitrary characters).
pub(crate) fn cask_mmp(version: &str) -> [String; 3] {
    let mut out = [String::new(), String::new(), String::new()];
    let mut rest = version;
    for slot in &mut out {
        let end = rest.find(['.', ',', ':']).unwrap_or(rest.len());
        *slot = rest[..end].to_string();
        rest = &rest[end..];
        if rest.is_empty() {
            break;
        }
        rest = &rest[rest.char_indices().nth(1).map_or(rest.len(), |(i, _)| i)..];
    }
    out
}

/// Evaluate a `version(.method)*` interpolation
/// token with `Cask::DSL::Version` semantics.
/// `None` for chains outside the modeled surface
/// or indexing a missing CSV part mid-chain.
#[must_use]
/// Split a `version(.method)*` token into chain
/// segments, keeping parenthesized literal args
/// (`tr(",", "-")`) whole. `None` outside that
/// grammar.
fn split_version_chain(token: &str) -> Option<Vec<String>> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut chars = token.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                cur.push(c);
                if c == '\\' {
                    if let Some(e) = chars.next() {
                        cur.push(e);
                    }
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' => {
                    quote = Some(c);
                    cur.push(c);
                }
                '(' => {
                    depth += 1;
                    cur.push('(');
                }
                ')' => {
                    depth = depth.checked_sub(1)?;
                    cur.push(')');
                }
                '.' if depth == 0 => {
                    parts.push(std::mem::take(&mut cur));
                }
                ' ' | '\t' if depth == 0 => return None,
                _ => cur.push(c),
            },
        }
    }
    if depth != 0 || quote.is_some() {
        return None;
    }
    parts.push(cur);
    Some(parts)
}

/// A quoted literal arg, unescaped for the common
/// escapes; `None` for anything else.
fn chain_str_arg(s: &str) -> Option<(String, &str)> {
    let s = s.trim();
    let q = *s.as_bytes().first()?;
    if q != b'"' && q != b'\'' {
        return None;
    }
    let mut out = String::new();
    let mut chars = s[1..].char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => {
                let (_, e) = chars.next()?;
                match e {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    '\\' | '"' | '\'' | '-' | '^' => out.push(e),
                    _ => return None,
                }
            }
            c if c as u32 == q as u32 => return Some((out, &s[i + 2..])),
            '#' if q == b'"' => {
                // Interpolation inside an arg is
                // outside the modeled surface.
                if s[1 + i + 1..].starts_with('{') {
                    return None;
                }
                out.push(c);
            }
            c => out.push(c),
        }
    }
    None
}

/// Ruby `String#tr` for the conservative subset:
/// no `^` negation, no ranges (`a-z`), no squeeze.
/// `None` outside it.
fn ruby_tr_set(s: &str, from: &str, to: &str) -> Option<String> {
    let from_c: Vec<char> = from.chars().collect();
    let to_c: Vec<char> = to.chars().collect();
    if from_c.first() == Some(&'^') {
        return None;
    }
    for (i, c) in from_c.iter().enumerate() {
        if *c == '-' && i != 0 && i + 1 != from_c.len() {
            return None;
        }
        if *c == '\\' {
            return None;
        }
    }
    for (i, c) in to_c.iter().enumerate() {
        if *c == '-' && i != 0 && i + 1 != to_c.len() {
            return None;
        }
        if *c == '\\' {
            return None;
        }
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match from_c.iter().position(|f| *f == c) {
            Some(idx) => {
                if let Some(t) = to_c.get(idx).or_else(|| to_c.last()) {
                    out.push(*t);
                }
            }
            None => out.push(c),
        }
    }
    Some(out)
}

/// Apply one arg-taking chain segment
/// (`tr(a, b)`, `sub(a, b)`, …) to `cur`.
fn apply_chain_arg_method(cur: &str, seg: &str) -> Option<String> {
    let (name, args) = seg.split_once('(')?;
    let args = args.strip_suffix(')')?;
    let (a, rest) = chain_str_arg(args)?;
    match name {
        "delete_prefix" => {
            if !rest.trim().is_empty() {
                return None;
            }
            Some(cur.strip_prefix(&a).unwrap_or(cur).to_string())
        }
        "delete_suffix" => {
            if !rest.trim().is_empty() {
                return None;
            }
            Some(cur.strip_suffix(&a).unwrap_or(cur).to_string())
        }
        "delete" => {
            if !rest.trim().is_empty() || a.starts_with('^') || a.contains('\\') {
                return None;
            }
            let set: Vec<char> = a.chars().collect();
            for (i, c) in set.iter().enumerate() {
                if *c == '-' && i != 0 && i + 1 != set.len() {
                    return None;
                }
            }
            Some(cur.chars().filter(|c| !set.contains(c)).collect())
        }
        "tr" | "sub" | "gsub" => {
            let rest = rest.trim().strip_prefix(',')?;
            let (b, rest) = chain_str_arg(rest)?;
            if !rest.trim().is_empty() {
                return None;
            }
            match name {
                "tr" => ruby_tr_set(cur, &a, &b),
                // Literal-string sub/gsub only; a
                // backslash in the replacement is
                // Ruby backref syntax, unmodeled.
                _ if b.contains('\\') => None,
                "sub" => Some(cur.replacen(&a, &b, 1)),
                _ => Some(cur.replace(&a, &b)),
            }
        }
        _ => None,
    }
}

/// Ruby `String#to_i`: optional sign and leading
/// digits, else 0.
fn ruby_to_i(s: &str) -> i64 {
    let s = s.trim_start();
    let (sign, s) = match s.strip_prefix('-') {
        Some(r) => (-1i64, r),
        None => (1i64, s.strip_prefix('+').unwrap_or(s)),
    };
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        0
    } else {
        sign * digits.parse::<i64>().unwrap_or(i64::MAX)
    }
}

/// One branch of a version ternary: a plain chain
/// or integer arithmetic on a chain's `to_i`
/// (optionally parenthesized).
fn version_ternary_branch_syntax(branch: &str) -> bool {
    let b = branch.trim();
    let b = b
        .strip_prefix('(')
        .and_then(|r| r.strip_suffix(')'))
        .map(str::trim)
        .unwrap_or(b);
    for op in [" + ", " - "] {
        if let Some((lhs, rhs)) = b.rsplit_once(op) {
            return rhs.trim().chars().all(|c| c.is_ascii_digit())
                && lhs
                    .trim()
                    .strip_suffix(".to_i")
                    .is_some_and(version_chain_syntax);
        }
    }
    version_chain_syntax(b) || b.strip_suffix(".to_i").is_some_and(version_chain_syntax)
}

/// Which string predicate a version condition
/// calls.
#[derive(Clone, Copy, PartialEq, Eq)]
enum VersionPred {
    Ends,
    Starts,
    Includes,
    /// `version.csv.length > N` (N in the literal).
    CsvLenGt,
}

/// `<recv>.end_with?("lit")` and friends over an
/// evaluable version chain: (receiver, predicate,
/// literal) — syntax only.
fn version_pred_parts(cond: &str) -> Option<(String, VersionPred, String)> {
    let cond = cond.trim();
    if let Some((r, a)) = cond.split_once(".csv.length > ") {
        let n = a.trim();
        if r.trim() != "version" || n.is_empty() || !n.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        return Some((r.trim().to_string(), VersionPred::CsvLenGt, n.to_string()));
    }
    let (recv, pred, args) = if let Some((r, a)) = cond.split_once(".end_with?(") {
        (r, VersionPred::Ends, a)
    } else if let Some((r, a)) = cond.split_once(".start_with?(") {
        (r, VersionPred::Starts, a)
    } else if let Some((r, a)) = cond.split_once(".include?(") {
        (r, VersionPred::Includes, a)
    } else {
        return None;
    };
    let (lit, tail) = chain_str_arg(args)?;
    if tail.trim() != ")" || !version_chain_syntax(recv.trim()) {
        return None;
    }
    Some((recv.trim().to_string(), pred, lit))
}

fn version_pred_value(version: &str, recv: &str, pred: VersionPred, lit: &str) -> Option<bool> {
    let recv = cask_version_token(version, recv)?;
    Some(match pred {
        VersionPred::Ends => recv.ends_with(lit),
        VersionPred::Starts => recv.starts_with(lit),
        VersionPred::Includes => recv.contains(lit),
        VersionPred::CsvLenGt => {
            version.split(',').count() > lit.parse::<usize>().unwrap_or(usize::MAX)
        }
    })
}

/// The parts of a `<pred> ? a : b` version ternary:
/// (receiver, predicate, literal, then-branch,
/// else-branch). `None` outside the shape (syntax
/// only; receivers and branches must be evaluable
/// version chains).
fn version_ternary_parts(token: &str) -> Option<(String, VersionPred, String, String, String)> {
    let (cond, rest) = token.split_once(" ? ")?;
    let (then, els) = rest.split_once(" : ")?;
    let (recv, pred, lit) = version_pred_parts(cond)?;
    if !version_ternary_branch_syntax(then) || !version_ternary_branch_syntax(els) {
        return None;
    }
    Some((
        recv,
        pred,
        lit,
        then.trim().to_string(),
        els.trim().to_string(),
    ))
}

/// Whether `token` is a version ternary
/// interpolation (syntax only), e.g. insomnia's
/// check-URL major probe.
#[must_use]
pub fn version_ternary_syntax(token: &str) -> bool {
    version_ternary_parts(token).is_some()
}

fn version_ternary_branch(version: &str, branch: &str) -> Option<String> {
    let b = branch.trim();
    let b = b
        .strip_prefix('(')
        .and_then(|r| r.strip_suffix(')'))
        .map(str::trim)
        .unwrap_or(b);
    for (op, neg) in [(" + ", false), (" - ", true)] {
        if let Some((lhs, rhs)) = b.rsplit_once(op)
            && let Ok(n) = rhs.trim().parse::<i64>()
            && let Some(chain) = lhs.trim().strip_suffix(".to_i")
            && version_chain_syntax(chain)
        {
            let v = ruby_to_i(&cask_version_token(version, chain)?);
            return Some((if neg { v - n } else { v + n }).to_string());
        }
    }
    if let Some(chain) = b.strip_suffix(".to_i")
        && version_chain_syntax(chain)
    {
        return Some(ruby_to_i(&cask_version_token(version, chain)?).to_string());
    }
    cask_version_token(version, b)
}

fn version_ternary_value(version: &str, token: &str) -> Option<String> {
    let (recv, pred, lit, then, els) = version_ternary_parts(token)?;
    let truth = version_pred_value(version, &recv, pred, &lit)?;
    version_ternary_branch(version, if truth { &then } else { &els })
}

/// `"-beta.0" if version.include?("-")`: a plain
/// quoted literal kept only when the version
/// predicate holds. Parts: (literal, negate, recv,
/// predicate, pred literal) — syntax only.
/// A double-quoted string's raw inner span,
/// tracking interpolation braces (and quotes
/// inside them) so a nested `#{...}` does not end
/// the string early.
fn dq_string_span(s: &str) -> Option<(String, &str)> {
    let bytes = s.as_bytes();
    if bytes.first() != Some(&b'"') {
        return None;
    }
    let mut depth = 0usize;
    let mut inner_quote: Option<u8> = None;
    let mut i = 1usize;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'\\' {
            i += 2;
            continue;
        }
        if depth == 0 {
            if c == b'"' {
                return Some((s[1..i].to_string(), &s[i + 1..]));
            }
            if c == b'#' && bytes.get(i + 1) == Some(&b'{') {
                depth = 1;
                i += 2;
                continue;
            }
        } else if let Some(q) = inner_quote {
            if c == q {
                inner_quote = None;
            }
        } else if c == b'"' || c == b'\'' {
            inner_quote = Some(c);
        } else if c == b'{' {
            depth += 1;
        } else if c == b'}' {
            depth -= 1;
        }
        i += 1;
    }
    None
}

fn version_conditional_string_parts(
    token: &str,
) -> Option<(String, bool, String, VersionPred, String)> {
    let token = token.trim();
    if !token.starts_with('"') {
        return None;
    }
    let (lit, tail) = if token.contains("#{") {
        dq_string_span(token)?
    } else {
        chain_str_arg(token)?
    };
    let tail = tail.trim();
    let (negate, cond) = if let Some(c) = tail.strip_prefix("if ") {
        (false, c)
    } else if let Some(c) = tail.strip_prefix("unless ") {
        (true, c)
    } else {
        return None;
    };
    let (recv, pred, pred_lit) = version_pred_parts(cond)?;
    Some((lit, negate, recv, pred, pred_lit))
}

/// Whether `token` is a quoted conditional-string
/// version interpolation (syntax only).
#[must_use]
pub fn version_conditional_string_syntax(token: &str) -> bool {
    version_conditional_string_parts(token).is_some()
}

fn version_conditional_string_value(version: &str, token: &str) -> Option<String> {
    let (lit, negate, recv, pred, pred_lit) = version_conditional_string_parts(token)?;
    let truth = version_pred_value(version, &recv, pred, &pred_lit)?;
    if truth == negate {
        return Some(String::new());
    }
    // Interpolate the kept literal against the
    // version (`"/#{version.csv[1..].join("/")}"`).
    let mut out = String::with_capacity(lit.len());
    let mut rest = lit.as_str();
    while let Some(i) = rest.find("#{") {
        out.push_str(&rest[..i]);
        let inner = &rest[i + 2..];
        let mut depth = 1usize;
        let mut j = 0usize;
        let b = inner.as_bytes();
        let mut quote: Option<u8> = None;
        while j < b.len() {
            let c = b[j];
            if let Some(q) = quote {
                if c == q && b.get(j.wrapping_sub(1)) != Some(&b'\\') {
                    quote = None;
                }
            } else if c == b'"' || c == b'\'' {
                quote = Some(c);
            } else if c == b'{' {
                depth += 1;
            } else if c == b'}' {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            j += 1;
        }
        if depth != 0 {
            return None;
        }
        out.push_str(&cask_version_token(version, inner[..j].trim())?);
        rest = &inner[j + 1..];
    }
    out.push_str(rest);
    Some(out)
}

/// `version.csv[a..b].join("sep")` (and the
/// open-ended / exclusive spellings).
fn version_csv_range_join(version: &str, token: &str) -> Option<String> {
    let rest = token.strip_prefix("version.csv[")?;
    let close = rest.find(']')?;
    let inside = rest[..close].trim();
    let (a_s, excl, b_s) = match inside.split_once("...") {
        Some((a, b)) => (a, true, b),
        None => {
            let (a, b) = inside.split_once("..")?;
            (a, false, b)
        }
    };
    let a: usize = a_s.trim().parse().ok()?;
    let parts: Vec<&str> = version.split(',').collect();
    if a > parts.len() {
        return None;
    }
    let end = if b_s.trim().is_empty() {
        parts.len()
    } else {
        let b: usize = b_s.trim().parse().ok()?;
        if excl {
            b.min(parts.len())
        } else {
            (b + 1).min(parts.len())
        }
    };
    let rest = rest[close + 1..].trim().strip_prefix(".join(")?;
    let (sep, tail) = chain_str_arg(rest.trim_start())?;
    if tail.trim() != ")" {
        return None;
    }
    Some(parts[a..end.max(a)].join(&sep))
}

/// `<head>.split("s")[.map { |x| x.rjust(n, "p") }]
/// .join("j")` version pipelines (jpki's download
/// URL zero-pads each dotted part), or the same
/// split read at one index (`.first`, `[n]`, …).
/// The head is any evaluable version chain; Ruby
/// `split` drops trailing empty fields,
/// `rjust`/`ljust` cycle the padding string.
/// `None` outside this shape.
fn version_split_map_join(version: &str, token: &str) -> Option<String> {
    let (head, rest) = token.split_once(".split(")?;
    let cur = cask_version_token(version, head.trim())?;
    let (sep, rest) = chain_str_arg(rest)?;
    let rest = rest.trim().strip_prefix(')')?.trim();
    let mut parts: Vec<String> = if sep.is_empty() {
        cur.chars().map(|c| c.to_string()).collect()
    } else {
        cur.split(&sep).map(str::to_string).collect()
    };
    while parts.last().is_some_and(|p| p.is_empty()) {
        parts.pop();
    }
    // A range slice (`[3..4]`, `[2...5]`, `[1..]`):
    // Ruby clamps the end, an end before the start
    // is empty, a start past the end is nil (outside).
    let rest = if rest.starts_with('[')
        && rest
            .find(']')
            .is_some_and(|close| rest[..close].contains(".."))
    {
        let close = rest.find(']')?;
        let inner = rest[1..close].trim();
        let (a_s, excl, b_s) = match inner.split_once("...") {
            Some((a, b)) => (a, true, b),
            None => {
                let (a, b) = inner.split_once("..")?;
                (a, false, b)
            }
        };
        let a: usize = a_s.trim().parse().ok()?;
        if a > parts.len() {
            return None;
        }
        let end = if b_s.trim().is_empty() {
            parts.len()
        } else {
            let b: usize = b_s.trim().parse().ok()?;
            if excl {
                b.min(parts.len())
            } else {
                (b + 1).min(parts.len())
            }
        };
        parts = if end >= a {
            parts[a..end].to_vec()
        } else {
            Vec::new()
        };
        rest[close + 1..].trim()
    } else {
        rest
    };
    let rest = if let Some(r) = rest.strip_prefix(".each_with_index.map") {
        // `{ |n, i| (i < 1 || n.length > 1) ? n :
        // n.rjust(W, "P") }`: zero-pad single-char
        // parts after the first (silentknight's
        // no-dot filename version).
        let r = r.trim().strip_prefix('{')?.trim();
        let r = r.strip_prefix('|')?;
        let (params, r) = r.split_once('|')?;
        let mut pit = params.split(',');
        let var = pit.next()?.trim();
        let idx_name = pit.next()?.trim();
        if pit.next().is_some()
            || var.is_empty()
            || idx_name.is_empty()
            || !var.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            || !idx_name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return None;
        }
        let (body, r) = r.split_once('}')?;
        let body = body.trim().strip_prefix('(')?.trim_start();
        let body = body
            .strip_prefix(idx_name)?
            .trim_start()
            .strip_prefix("< 1 ||")?
            .trim_start();
        let body = body
            .strip_prefix(var)?
            .trim_start()
            .strip_prefix(".length > 1)")?
            .trim_start();
        let body = body
            .strip_prefix('?')?
            .trim_start()
            .strip_prefix(var)?
            .trim_start()
            .strip_prefix(':')?
            .trim_start();
        let body = body
            .strip_prefix(var)?
            .trim_start()
            .strip_prefix(".rjust(")?;
        let (width, body) = body.split_once(',')?;
        let width: usize = width.trim().parse().ok()?;
        let (pad, body) = chain_str_arg(body)?;
        if pad.is_empty() || body.trim() != ")" {
            return None;
        }
        let pad: Vec<char> = pad.chars().collect();
        for (i, part) in parts.iter_mut().enumerate() {
            let len = part.chars().count();
            if i >= 1 && len <= 1 && len < width {
                let fill: String = (0..width - len).map(|k| pad[k % pad.len()]).collect();
                *part = format!("{fill}{part}");
            }
        }
        r.trim()
    } else if let Some(r) = rest.strip_prefix(".map") {
        let r = r.trim().strip_prefix('{')?.trim();
        let r = r.strip_prefix('|')?;
        let (var, r) = r.split_once('|')?;
        let var = var.trim();
        if var.is_empty() || !var.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
        let r = r.trim().strip_prefix(var)?.trim();
        let (right, r) = if let Some(r) = r.strip_prefix(".rjust(") {
            (true, r)
        } else if let Some(r) = r.strip_prefix(".ljust(") {
            (false, r)
        } else {
            return None;
        };
        let (width, r) = r.split_once(',')?;
        let width: usize = width.trim().parse().ok()?;
        let (pad, r) = chain_str_arg(r)?;
        if pad.is_empty() {
            return None;
        }
        let r = r.trim().strip_prefix(')')?.trim().strip_prefix('}')?;
        let pad: Vec<char> = pad.chars().collect();
        for part in &mut parts {
            let len = part.chars().count();
            if len < width {
                let fill: String = (0..width - len).map(|i| pad[i % pad.len()]).collect();
                *part = if right {
                    format!("{fill}{part}")
                } else {
                    format!("{part}{fill}")
                };
            }
        }
        r.trim()
    } else {
        rest
    };
    // One-index terminals (`.first`, `[2]`, …): a
    // missing field is Ruby nil, interpolating as "".
    let index_tail = rest
        .strip_prefix('.')
        .or_else(|| rest.strip_prefix("&."))
        .unwrap_or(rest);
    let index = match index_tail {
        "first" => Some(0usize),
        "second" => Some(1),
        "third" => Some(2),
        "fourth" => Some(3),
        "fifth" => Some(4),
        "last" => None,
        _ => {
            if index_tail == "join" {
                return Some(parts.join(""));
            }
            if index_tail.starts_with(".join(") || index_tail.starts_with("join(") {
                let rest = if rest.starts_with("&.join(") {
                    &rest["&.join(".len()..]
                } else {
                    rest.strip_prefix(".join(")?
                };
                let (joiner, rest) = chain_str_arg(rest)?;
                if rest.trim() != ")" {
                    return None;
                }
                return Some(parts.join(&joiner));
            }
            let bracketed = rest.strip_prefix('[').and_then(|r| r.strip_suffix(']'))?;
            Some(bracketed.trim().parse().ok()?)
        }
    };
    let picked = match index {
        Some(i) => parts.get(i),
        None => parts.last(),
    };
    Some(picked.cloned().unwrap_or_default())
}

pub fn cask_version_token(version: &str, token: &str) -> Option<String> {
    if let Some(v) = version_csv_range_join(version, token) {
        return Some(v);
    }
    if token.contains(".split(")
        && let Some(v) = version_split_map_join(version, token)
    {
        return Some(v);
    }
    if let Some(v) = version_ternary_value(version, token) {
        return Some(v);
    }
    if let Some(v) = version_conditional_string_value(version, token) {
        return Some(v);
    }
    // A terminal character index
    // (`version.csv.second[0]`): Ruby `String#[]`
    // with an integer counts characters, negative
    // from the end; out of range is nil ("").
    if let Some(open) = token.rfind('[')
        && token.ends_with(']')
        && let Ok(idx) = token[open + 1..token.len() - 1].trim().parse::<i64>()
    {
        let base = cask_version_token(version, &token[..open])?;
        let chars: Vec<char> = base.chars().collect();
        let idx = if idx < 0 {
            idx.checked_add(i64::try_from(chars.len()).ok()?)?
        } else {
            idx
        };
        let idx = usize::try_from(idx).unwrap_or(usize::MAX);
        return Some(chars.get(idx).map_or_else(String::new, char::to_string));
    }
    if !version_chain_syntax(token) {
        // Arg-taking tails (`version.tr(",", "-")`):
        // evaluate segment-wise, plain methods by
        // the strict path below, arg methods by
        // `apply_chain_arg_method`.
        if !token.contains('(') {
            return None;
        }
        let parts = split_version_chain(token)?;
        if parts.first().map(String::as_str) != Some("version") {
            return None;
        }
        let mut cur = version.to_string();
        let mut plain: Vec<&str> = Vec::new();
        for seg in &parts[1..] {
            // `….csv.join("sep")`: split the current
            // value on commas and rejoin.
            if plain.last() == Some(&"csv")
                && let Some(sep) = seg
                    .strip_prefix("join(\"")
                    .and_then(|r| r.strip_suffix("\")"))
                    .or_else(|| {
                        seg.strip_prefix("join('")
                            .and_then(|r| r.strip_suffix("')"))
                    })
            {
                plain.pop();
                if !plain.is_empty() {
                    let joined = format!("version.{}", plain.join("."));
                    cur = cask_version_token(&cur, &joined)?;
                    plain.clear();
                }
                cur = cur.split(',').collect::<Vec<_>>().join(sep);
                continue;
            }
            if seg.contains('(') {
                if !plain.is_empty() {
                    let joined = format!("version.{}", plain.join("."));
                    cur = cask_version_token(&cur, &joined)?;
                    plain.clear();
                }
                cur = apply_chain_arg_method(&cur, seg)?;
            } else if VERSION_CHAIN_METHODS.contains(&seg.as_str()) {
                plain.push(seg);
            } else {
                return None;
            }
        }
        if !plain.is_empty() {
            let joined = format!("version.{}", plain.join("."));
            cur = cask_version_token(&cur, &joined)?;
        }
        return Some(cur);
    }
    let mut cur = version.to_string();
    let mut methods = token.split('.').skip(1).peekable();
    while let Some(m) = methods.next() {
        cur = match m {
            "major" | "minor" | "patch" | "major_minor" | "major_minor_patch" | "minor_patch" => {
                let [major, minor, patch] = cask_mmp(&cur);
                match m {
                    "major" => major,
                    "minor" => minor,
                    "patch" => patch,
                    "major_minor" => [major, minor]
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join("."),
                    "major_minor_patch" => [major, minor, patch]
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join("."),
                    _ => [minor, patch]
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join("."),
                }
            }
            "csv" => {
                // Only <part> selection may follow.
                let idx = match methods.next()? {
                    "first" => 0,
                    "second" => 1,
                    "third" => 2,
                    "fourth" => 3,
                    "fifth" => 4,
                    "last" => usize::MAX,
                    _ => return None,
                };
                let parts: Vec<&str> = cur.split(',').collect();
                let part = if idx == usize::MAX {
                    parts.last()
                } else {
                    parts.get(idx)
                };
                match part {
                    Some(p) => p.to_string(),
                    None if methods.peek().is_none() => String::new(),
                    None => return None,
                }
            }
            "before_comma" => cur.split(',').next().unwrap_or_default().to_string(),
            "after_comma" => cur
                .split_once(',')
                .map_or(String::new(), |(_, rest)| rest.to_string()),
            "no_dots" => cur.replace('.', ""),
            "no_hyphens" => cur.replace('-', ""),
            "no_underscores" => cur.replace('_', ""),
            "no_dividers" => cur.replace(['.', '-', '_'], ""),
            "dots_to_hyphens" => cur.replace('.', "-"),
            "dots_to_underscores" => cur.replace('.', "_"),
            "hyphens_to_dots" => cur.replace('-', "."),
            "hyphens_to_underscores" => cur.replace('-', "_"),
            "underscores_to_dots" => cur.replace('_', "."),
            "underscores_to_hyphens" => cur.replace('_', "-"),
            // Bare csv part selectors apply to the
            // whole string only via `csv`.
            _ => return None,
        };
    }
    Some(cur)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LivecheckSpec {
    /// The `url` argument: a `:symbol` (stored without the
    /// colon) or a literal string. Extra url options
    /// (`using: :post`, headers, …) set `url_options`.
    pub url: Option<LivecheckUrl>,
    pub url_options: bool,
    /// The `compressed: false` url option: fetch
    /// without `--compressed` (brew requests a
    /// compressed response by default).
    pub compressed_false: bool,
    /// The `user_agent:` url option: `":browser"` for the
    /// symbol, the literal string for a quoted value. Other
    /// url options still set `url_options` (and delegate).
    pub user_agent: Option<String>,
    /// The `post_form:` url option as written-order
    /// key/value pairs (values keep `#{version...}`
    /// tokens for check-time substitution).
    pub post_form: Option<Vec<(String, String)>>,
    /// The `post_json:` url option as generated JSON
    /// text (`"{}"` for the empty hash, which still
    /// forces a bodyless POST in the oracle).
    pub post_json: Option<String>,
    /// The `header:` url option(s), verbatim
    /// `Name: value` strings.
    pub header: Option<Vec<String>>,
    /// The regex source verbatim (delimiters stripped) and the
    /// option letters that followed it (e.g. `i`).
    pub regex: Option<String>,
    pub regex_flags: String,
    /// The `strategy :symbol`, stored without the colon.
    pub strategy: Option<String>,
    /// The strategy carries a Ruby block (never data).
    pub strategy_block: bool,
    /// A `:json` strategy block reduced to a data shape
    /// (see `JsonShape`); other blocks stay `None`.
    pub json_block: Option<JsonBlock>,
    /// A `:xml` strategy block reduced to a data shape
    /// (see `XmlShape`); other blocks stay `None`.
    pub xml_block: Option<XmlBlock>,
    /// A `:yaml` strategy block reduced to a data shape
    /// (see `YamlShape`); other blocks stay `None`.
    pub yaml_block: Option<YamlBlock>,
    /// An `:extract_plist` strategy block reduced to a
    /// data shape (see `PlistBlock`); other blocks stay
    /// `None`.
    pub plist_block: Option<PlistBlock>,
    /// A `:page_match`-family strategy block reduced to
    /// a data shape (see `PageShape`); other blocks stay
    /// `None`.
    pub page_block: Option<PageBlock>,
    /// A `:github_releases` strategy block reduced to
    /// a data shape (see `GhRelBlock`); other blocks
    /// stay `None`.
    pub ghrel_block: Option<GhRelBlock>,
    /// A `:git` strategy block reduced to a data shape
    /// (see `GitBlock`); other blocks stay `None`.
    pub git_block: Option<GitBlock>,
    /// A `:github_latest` strategy block reduced to
    /// a data shape (see `LatestBlock`); other blocks
    /// stay `None`.
    pub latest_block: Option<LatestBlock>,
    /// A `:sparkle` strategy block (or block-pass
    /// symbol) reduced to a data shape (see
    /// `SparkleBlock`); other blocks stay `None`.
    pub sparkle_block: Option<SparkleBlock>,
    /// A `:header_match` strategy block reduced to
    /// a data shape (see `HeaderBlock`).
    pub header_block: Option<HeaderBlock>,
    /// `skip` was declared; the string is its optional message.
    pub skip: Option<String>,
    /// `throttle <versions>` / `throttle days: <days>`.
    pub throttle: Option<u64>,
    pub throttle_days: Option<u64>,
    /// `formula "name"` / `cask "token"` indirection (`:parent`
    /// marks a resource's reference to its owning formula).
    pub formula_ref: Option<String>,
    pub cask_ref: Option<String>,
}

/// A `:json` strategy block reduced to data;
/// `has_regex` is whether it declares the regex
/// parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonBlock {
    pub has_regex: bool,
    pub shape: JsonShape,
}

/// One step of a JSON dig path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonKey {
    Key(String),
    Idx(usize),
}

/// One interpolation part of a `ListMaxPageHop`
/// render: literal text, the list maximum, or the
/// hopped page's regex capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonHopPart {
    Lit(String),
    Max,
    Date,
    /// The hopped-from key's value
    /// (`KeyHopDigMatch`).
    Key,
    /// The fetched document's regex capture
    /// (`KeyHopDigMatch`).
    Fetched,
}

/// The enumerable data-only shapes a `:json` block body
/// can take (see `tap::parse_json_shape`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonShape {
    /// `<list> = <dig>&.filter_map { |i| i["k"] }&.uniq`,
    /// `<v> = list.max_by { Version.new }`, then a
    /// literal `page_content` page's regex capture,
    /// rendered together (eclipse-ide).
    ListMaxPageHop {
        path: Vec<JsonKey>,
        key: String,
        url: String,
        cap: usize,
        parts: Vec<JsonHopPart>,
    },
    /// `<v> = json["K"]`, fetch `pre#{v}post`,
    /// parse it as JSON, dig, take regex capture
    /// `cap`, render (firefox@nightly).
    KeyHopDigMatch {
        key: String,
        url_pre: String,
        url_post: String,
        path: Vec<JsonKey>,
        cap: usize,
        parts: Vec<JsonHopPart>,
    },
    /// `<v> = json["K"]`; when `v` equals the
    /// current csv first field the answer is the
    /// current version, else fetch a links
    /// document and take the first list item of a
    /// given type whose URL field matches
    /// (minecraft-server).
    KeyCurrentGateDigMatch {
        key: String,
        url: String,
        path: Vec<JsonKey>,
        type_key: String,
        type_val: String,
        url_key: String,
        cap: usize,
    },
    /// `strategy :json, &:to_s` — the root value's
    /// Ruby `to_s`.
    RootToS,
    /// `json["a"]`, `json.dig("a", "b")` — the value.
    Dig(Vec<JsonKey>),
    /// `….delete("v")` — value with chars removed.
    DigDelete(Vec<JsonKey>, String),
    /// `….split("-")&.first` — value up to the first
    /// separator.
    DigSplitFirst(Vec<JsonKey>, String),
    /// `….[](regex, 1)` / `….match(regex)` first capture.
    DigRegex(Vec<JsonKey>),
    /// `….scan(regex).map { |match| … }` over a dug
    /// string (scan-indexed parts).
    DigScan(Vec<JsonKey>, Vec<PagePart>),
    /// `….match(regex)&.captures` — every
    /// participating capture as a version.
    DigCaptures(Vec<JsonKey>),
    /// `….map { |_, item| item["k"] }` over a dug
    /// object's values.
    ValuesKey(Vec<JsonKey>, String),
    /// `….join(".")` over a dug array.
    DigJoin(Vec<JsonKey>, String),
    /// `….map { |key, item| <key guards> item["k"]… }`
    /// over a dug object's entries (document order).
    EntriesMap(Vec<JsonKey>, Box<MapGuard>),
    /// Nested `each` loops taking the first inner
    /// field matching the block regex, rendered
    /// over its captures (unity's downloads scan).
    NestedFirstMatch {
        outer: Vec<JsonKey>,
        inner: Vec<JsonKey>,
        field: String,
        render: Box<JsonRender>,
    },
    /// `<v> = <dig> [|| <dig>…]; match = v&.match(regex)`;
    /// rendered over the match (quarkclouddrive's
    /// fallback download URL).
    DigMatch {
        paths: Vec<Vec<JsonKey>>,
        render: Box<JsonRender>,
    },
    /// A dug string is itself a JSON document; dig
    /// again inside it and match the block regex
    /// (kdocs).
    DigParseMatch {
        outer: Vec<JsonKey>,
        inner: Vec<JsonKey>,
        render: Box<JsonRender>,
    },
    /// Several named dig matches (each with any
    /// `||=` fallback attempts), rendered from their
    /// captures in one final template.
    MultiMatch {
        matches: Vec<JsonMultiMatch>,
        render: Vec<JsonMultiPart>,
    },
    /// `version = nil; <dig>.filter_map { |_k, item|
    /// match = item.dig(…)&.match(regex); next unless
    /// match; version = <render>; break }; version`
    /// over a dug object's values (expressvpn).
    EntriesFirstMatch {
        path: Vec<JsonKey>,
        dig: Vec<JsonKey>,
        render: Box<JsonRender>,
    },
    /// `<outer>.map { |item| item<dig>&.filter_map {
    /// |a| a[field]&.[](regex, 1) } }.flatten`: every
    /// inner first capture (rnote).
    NestedCollect {
        outer: Vec<JsonKey>,
        inner: Vec<JsonKey>,
        field: String,
    },
    /// `<outer>.map { |g| g<inner>&.map { |item|
    /// <guarded projection> } }&.flatten`: a guarded
    /// map over each outer item's dug array.
    NestedMap {
        outer: Vec<JsonKey>,
        inner: Vec<JsonKey>,
        guard: Box<MapGuard>,
    },
    /// `<outer>.map { |o| o<inner>&.map { |i|
    /// guards; next o["outer"] unless i["inner"];
    /// "#{o["outer"]},#{i["inner"]}" } }` (gimp's
    /// release/build list).
    NestedTemplate {
        outer: Vec<JsonKey>,
        inner: Vec<JsonKey>,
        guards: Vec<JsonGuard>,
        outer_field: String,
        inner_field: String,
    },
    /// A guarded map with a trailing `&.max`:
    /// the single lexicographic maximum (Ruby
    /// `Array#max` on strings).
    MapGuardedMax(Vec<JsonKey>, Box<MapGuard>),
    /// `….map { |key, item| <guards on item> key }`:
    /// the entry keys whose values pass the guards.
    EntriesKeys(Vec<JsonKey>, Vec<JsonGuard>),
    /// `….to_s.sub(/pat/flags, "literal")` — plain
    /// (non-backreference) replacement.
    DigSubPlain(Vec<JsonKey>, String, String, String),
    /// `….map { |item| item["k"] }` (item key `None` =
    /// the item itself).
    MapKey(Vec<JsonKey>, Option<String>),
    /// `….map/filter_map { |item| item["k"]&.[](regex, 1) }`.
    MapRegex(Vec<JsonKey>, Option<String>),
    /// `json.first["k"]`.
    FirstKey(String),
    /// `….map { |item| <next guards> item["k"]… }`.
    MapGuarded(Vec<JsonKey>, Box<MapGuard>),
    /// `….map { |_, g| g["k"]&.map { |item| … } }&.flatten`:
    /// a guarded map over each object value's `k`
    /// array, flattened.
    ValuesMap(Vec<JsonKey>, String, Box<MapGuard>),
    /// A statement body: locals dug from the base
    /// value, an optional guarded regex match, and a
    /// value/template rendering (cask-tap forms).
    Template(Box<JsonTemplate>),
}

/// One named value a `:json` template body digs
/// from its base (the root, or one map item).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonLocal {
    Dig(Vec<JsonKey>),
    /// `….split("s")&.first/second` (index 0/1).
    DigSplit(Vec<JsonKey>, String, usize),
    /// `….[](regex, 1)` with the block regex.
    DigCapture(Vec<JsonKey>),
    /// `….[/lit/flags, 1]` (source, flags).
    DigCaptureLit(Vec<JsonKey>, String, String),
    /// `<dig> || <dig> || …`: the first present dig.
    DigOr(Vec<Vec<JsonKey>>),
    /// `(<dig> || <dig>)&.tail`: the first present
    /// dig with a string tail transform.
    DigOrTail(Vec<Vec<JsonKey>>, JsonTail),
    /// A dig with a string tail transform
    /// (`&.tr("v", "")`).
    DigTail(Vec<JsonKey>, JsonTail),
    /// `….sub(/pat/flags, "tmpl")` (first
    /// substitution, backref template).
    DigSubRe(Vec<JsonKey>, String, String, String),
    /// `<field>.split(line).map { split(col) }`
    /// then `.find { first.end_with?(suffix) }`:
    /// the found row, joined by U+001F for a
    /// `LocalMd5Field` render part.
    SplitRowsFind {
        path: Vec<JsonKey>,
        line_sep: String,
        field_sep: String,
        first_suffix: String,
    },
}

/// One part of a `:json` template string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonPart {
    Lit(String),
    /// `match[N]` of the body's regex match.
    Cap(usize),
    /// `match[N].tr("from", "to")`.
    CapTr(usize, String, String),
    /// A body local by index.
    Local(usize),
    /// `Digest::MD5.hexdigest(<local>[N])` over a
    /// `SplitRowsFind` local's field.
    LocalMd5Field(usize, usize),
    /// `item["k"]` in a map body.
    Field(String),
    /// A dig from the document root in a map body
    /// (`json["shortrev"]`).
    RootDig(Vec<JsonKey>),
}

/// A trailing transform on a single rendered
/// value (`&.tr(...)` and friends).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonTail {
    None,
    ToS,
    Strip,
    Tr(String, String),
    Sub(String, String),
    DeletePrefix(String),
    DeleteSuffix(String),
    DeleteChars(String),
}

/// What a `:json` template body returns per item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonRender {
    Template(Vec<JsonPart>),
    /// One value (a local index, a match capture,
    /// or a map item field) plus its tail chain.
    Value(JsonValue, JsonTail),
    /// `match[N] ? A : B` (`present` = `.present?`,
    /// which also drops empty captures).
    CapFork {
        cap: usize,
        present: bool,
        then: Box<JsonRender>,
        els: Box<JsonRender>,
    },
    /// `(match[A] == match[B]) ? A-expr : B-expr`.
    CapEqFork {
        a: usize,
        b: usize,
        then: Box<JsonRender>,
        els: Box<JsonRender>,
    },
    /// `(localA == localB) ? A-expr : B-expr`.
    LocalEqFork {
        a: usize,
        b: usize,
        then: Box<JsonRender>,
        els: Box<JsonRender>,
    },
    /// `<render>.gsub(/pat/flags, "repl")` with a
    /// literal (backslash-free) replacement.
    GsubRegex {
        inner: Box<JsonRender>,
        pat: String,
        flags: String,
        repl: String,
    },
}

/// A single-value source for `JsonRender::Value`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonValue {
    Local(usize),
    Cap(usize),
    Field(String),
    /// `<local>[regex, 1]`: the block regex's first
    /// capture of a local's string value.
    LocalCapture(usize),
}

/// A statement-form `:json` block reduced to data
/// (see `tap::parse_json_template_shape`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonTemplate {
    /// Dig path to the base value (the map's array
    /// when `map`, else the base object itself).
    pub path: Vec<JsonKey>,
    pub map: bool,
    /// The map iterates the dug object's values
    /// (`….values&.filter_map`), not an array.
    pub values: bool,
    pub locals: Vec<JsonLocal>,
    /// Locals guarded `blank?`: a missing/empty one
    /// skips the item (or yields nothing).
    pub require: Vec<usize>,
    /// The dig whose value the block regex matches
    /// (`match = <dig>&.match(regex)`), if any.
    pub match_src: Option<Vec<JsonKey>>,
    /// Skip unless this local matches the regex.
    pub local_match_guard: Option<usize>,
    pub guards: Vec<JsonGuard>,
    /// `find { … }` element selections applied in
    /// order from the root to reach the base value
    /// (non-map bodies only).
    pub finds: Vec<JsonFind>,
    /// Finds that must resolve (`next unless x` =
    /// truthy, `next if x.blank?` = non-blank).
    pub require_find: Vec<(usize, bool)>,
    pub render: JsonRender,
}

/// One named match in a `MultiMatch` json body:
/// its attempts in fallback order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonMultiMatch {
    pub attempts: Vec<JsonMatchAttempt>,
}

/// One `<dig>&.match(<regex>)` attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonMatchAttempt {
    pub path: Vec<JsonKey>,
    pub regex: JsonMatchRegex,
}

/// The regex a `JsonMatchAttempt` matches with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonMatchRegex {
    /// The block's declared `regex`.
    Block,
    /// A literal regex (source, flags).
    Literal(String, String),
}

/// One part of a `MultiMatch` final template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonMultiPart {
    Lit(String),
    /// Capture `1` of match `0`.
    Cap(usize, usize),
}

/// One `&.find { |v| <cond> }` over the array at
/// `path` (dug from the previous base).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonFind {
    pub path: Vec<JsonKey>,
    pub cond: JsonFindCond,
    /// Take the version-maximum by this field
    /// (`.select { cond }.max_by { Version.new }`)
    /// instead of the first match.
    pub max_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonFindCond {
    /// `v["f"] == lit`
    FieldEq(String, JsonLit),
    /// `v.dig(…)&.start_with?("lit")`
    DigStartsWith(Vec<JsonKey>, String),
}

/// A guarded map body reduced to data: the projection
/// (item key, `None` = the item itself), its capture
/// and trailing `tr`/`sub` chain, and its `next`
/// guards in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapGuard {
    pub item_key: Option<String>,
    pub capture: MapCapture,
    pub tr: Option<(String, String)>,
    /// A trailing `.sub(/pat/flags, "template")`
    /// (backreference template), after any `tr`.
    pub sub: Option<(String, String, String)>,
    pub guards: Vec<JsonGuard>,
}

/// How a guarded map's projection captures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MapCapture {
    /// The projected value verbatim.
    None,
    /// The block's `regex`, first capture.
    BlockRegex,
    /// A literal regex in the projection
    /// (source, flags), first capture.
    Literal(String, String),
}

/// One `next if`/`next unless` guard (`unless` false =
/// `next if`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonGuard {
    pub unless: bool,
    pub cond: JsonCond,
}

/// The enumerable guard conditions over a map item's
/// fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonCond {
    Eq(String, JsonLit),
    Ne(String, JsonLit),
    Truthy(String),
    Or(Vec<JsonCond>),
    /// `item["f"]&.start_with?(version.major_minor)`.
    StartsWithMajorMinor(String),
    /// `item["f"]&.downcase&.include?("lit")`.
    DowncaseIncludes(String, String),
    /// `item["f"].none? { |el| el["g"].match?(regex) }`
    /// (outer field, element field).
    NoneMatch(String, String),
    /// `item["f"]&.match?("lit")` (substring search).
    MatchLit(String, String),
    /// `item["f"]&.include?("lit")`.
    Includes(String, String),
    /// `item["f"].blank?`
    FieldBlank(String),
    /// `item.dig(…).blank?`
    DigBlank(Vec<JsonKey>),
    /// `item["f"].match?(/pat/flags)` — a literal
    /// regex in the block.
    MatchRe(String, String, String),
    /// `item["f"]&.any? { |v| v =~ /pat/flags }`.
    AnyMatchRe(String, String, String),
    /// `[lit, …].include?(item["f"])`.
    FieldIn(String, Vec<JsonLit>),
    /// `item["f"]&.any? { |e| e["a"] == lit && … }`:
    /// some element object satisfies every equality.
    AnyEq(String, Vec<(String, JsonLit)>),
    /// `key.match?(/pat/flags)` on an object-entry
    /// map's key (entries maps only).
    KeyMatchRe(String, String),
    /// `item.dig(…)` truthy (present, not `false`).
    DigTruthy(Vec<JsonKey>),
    /// `item["f"]&.end_with?("lit")`.
    EndsWith(String, String),
}

/// A literal in a guard comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonLit {
    Str(String),
    Bool(bool),
    Int(i64),
}

/// An `:xml` strategy block reduced to data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlBlock {
    pub has_regex: bool,
    pub shape: XmlShape,
}

/// The data-only shapes an `:xml` block body can take:
/// element texts at a descendant path, optionally with
/// the regex's first capture applied to each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XmlShape {
    Texts(Vec<String>),
    TextsRegex(Vec<String>),
    /// A statement body over XPath queries (see
    /// `XmlTemplate`).
    Template(Box<XmlTemplate>),
    /// Two mapped lists zipped pairwise
    /// (`versions.zip(builds).map { |v, b| … }`):
    /// parts read Local(0) = a-item, Local(1) =
    /// b-item.
    ZipPair {
        a: XmlMap,
        b: XmlMap,
        parts: Vec<XmlPart>,
    },
    /// Two mapped lists reduced to their extremes:
    /// the version list's `max_by { Version.new }`
    /// and the build list's lexical `max`, joined
    /// with a comma (oxygen's build feed).
    TwoListExtreme {
        versions: XmlMap,
        builds: XmlMap,
    },
    /// One element's text holds a JSON document;
    /// the version is one string key of it
    /// (adobe-creative-cloud's feature-entry data).
    TextJsonKey {
        path: XmlPathQ,
        key: String,
    },
    /// Map over items matching a child element's
    /// text against the spec regex; when capture 1's
    /// leading digits equal the checked unit's own
    /// `version.csv.first.major` (and a CSV second
    /// part exists), render `cap1,csv_second`, else
    /// bare `cap1` (endnote's update feed).
    ChildMatchOwnFork {
        path: XmlPathQ,
        child: String,
    },
    /// One element's text is a URL; the version is
    /// rendered from the spec regex matched against
    /// that URL's merged response header (canon).
    HeaderHop {
        path: XmlPathQ,
        header: String,
        parts: Vec<XmlPart>,
    },
    /// The `max_by` child-text element of a list
    /// gives a version (literal-regex capture of one
    /// child) and a second XML document URL (another
    /// child); the spec regex matches a text in the
    /// second document and both render (turbotax).
    HopMatch {
        path: XmlPathQ,
        max_child: String,
        version_child: String,
        pat: String,
        flags: String,
        url_child: String,
        path2: XmlPathQ,
        parts: Vec<XmlPart>,
    },
}

/// One XPath step: a tag (bare or `prefix:local`)
/// with an optional enumerable predicate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlStep {
    pub tag: String,
    pub pred: Option<XmlPred>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XmlPred {
    /// `[text()='v']`
    TextEq(String),
    /// `[Child='v']`
    ChildTextEq(String, String),
    /// `[@attr='v']`
    AttrEq(String, String),
    /// `[contains(@attr,'v')]`
    AttrContains(String, String),
    /// `[@a='v' and not(@b) and …]`: each attribute
    /// must equal its value, or be absent (`None`).
    AttrAll(Vec<(String, Option<String>)>),
    /// `[1]`: the 1-based position among the
    /// step's candidates for each context node.
    Index(usize),
    /// `[Child[text()='k']/following-sibling::*[1][text()='v']]`: a
    /// child with text `child_text` whose next sibling element's
    /// text is `text`.
    SiblingAfterChild {
        child: String,
        child_text: String,
        text: String,
    },
}

/// An XPath path: `//a/b` (descendant first step),
/// `/a/b` (absolute from the root element), or `a/b`
/// (relative child steps, only under an element local).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlPathQ {
    pub absolute: bool,
    pub relative: bool,
    pub steps: Vec<XmlStep>,
}

/// A named value an `:xml` template body reads:
/// the first matching element's text, optionally
/// its next sibling element's. `from` indexes
/// `XmlTemplate::elems` for `item.elements[…]` reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlLocal {
    pub path: XmlPathQ,
    pub next: bool,
    pub from: Option<usize>,
}

/// How one mapped element projects to a string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XmlProj {
    /// `item.text&.strip`
    Text,
    /// `item.text&.strip&.delete_suffix("s")`
    TextDeleteSuffix(String),
    /// `item.next_element&.text&.strip`
    NextText,
    /// `item.attributes["k"]`
    Attr(String),
    /// `item.attributes["k"]&.[](regex, 1)`
    AttrCapture(String),
    /// `match = item.text&.match(regex)` + render
    /// (`true` reads the next sibling's text).
    TextMatch(bool),
    /// `match = item.attributes["k"]&.match(regex)`
    /// + render.
    AttrMatch(String),
    /// A match over a child element's attribute,
    /// skipping items whose named child's text
    /// includes the literal (macgamestore's beta
    /// filter): (child, attr, skip child, skip text).
    ChildAttrMatch {
        child: String,
        attr: String,
        skip_child: Option<(String, String)>,
    },
}

/// One per-item local in an xml map body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XmlItemLocal {
    /// `item.attributes["k"]`
    Attr(String),
    /// `item.text`
    Text,
    /// `item.text&.[](regex, 1)`
    TextCapture,
}

/// The `get_elements(...).map` half of a template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlMap {
    pub path: XmlPathQ,
    pub proj: XmlProj,
    /// A trailing `.grep(regex)` keeps only
    /// projections the regex matches (whole).
    pub grep: bool,
    /// Per-item locals (attr/text reads rendered
    /// by a template), empty for plain projections.
    pub locals: Vec<XmlItemLocal>,
}

/// One part of an `:xml` template string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XmlPart {
    Lit(String),
    Local(usize),
    /// `#{local.strip}`
    LocalStrip(usize),
    /// `match[N]` (MatchData indexing).
    Cap(usize),
    /// `match[N].tr("from", "to")`.
    CapTr(usize, String, String),
}

/// What an `:xml` template body returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XmlRender {
    Template(Vec<XmlPart>),
    /// `match[N].present? ? A : B` in a map match.
    CapPresentFork {
        cap: usize,
        then: Vec<XmlPart>,
        els: Vec<XmlPart>,
    },
    /// One local's value, optionally stripped.
    LocalValue(usize, bool),
    /// `match[N]` of the body's regex match.
    CapValue(usize),
    /// The map projection itself.
    Projection,
}

/// A statement-form `:xml` block reduced to data
/// (see `tap::parse_xml_template_shape`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlTemplate {
    pub locals: Vec<XmlLocal>,
    /// Element locals (`item = xml.elements[…]`);
    /// `next` reads the next sibling element instead.
    pub elems: Vec<XmlLocal>,
    pub require: Vec<usize>,
    /// Element locals that must exist (`next unless item`).
    pub require_elems: Vec<usize>,
    /// The local the block regex matches
    /// (`match = <local>.match(regex)`).
    pub match_local: Option<usize>,
    /// `<name> ||= <read>` fallbacks: (dst, src)
    /// local indexes; dst takes src's value when nil.
    pub or_pairs: Vec<(usize, usize)>,
    pub map: Option<XmlMap>,
    pub render: XmlRender,
}

/// A `:yaml` strategy block reduced to data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YamlBlock {
    pub has_regex: bool,
    pub shape: YamlShape,
}

/// A `:page_match`-family strategy block reduced to data:
/// `page.scan(regex).map { … }` with an enumerable
/// per-match transform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageBlock {
    pub shape: PageShape,
}

/// One part of a page-match template string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PagePart {
    Lit(String),
    Cap(usize),
    /// `match[N].tr("from", "to")`
    CapTr(usize, String, String),
    /// `#{<template> if match[N]}`: the inner
    /// template, only when the capture participates.
    CondCap(usize, Vec<PagePart>),
    /// `match[N].split("", L).join(sep)`
    /// (`L == 0` = `chars.join(sep)`).
    CapSplitJoin(usize, usize, String),
    /// `match[N].delete_prefix("p")`
    CapDeletePrefix(usize, String),
    /// `match[N].sub(/pat/flags, 'tmpl')`
    /// (backreference template).
    CapSubBackref(usize, String, String, String),
    /// `match[N][.tr(a, b)].split(sep).map(&:to_i).join(j)`
    /// (the to_i map optional when a tr precedes).
    CapNormInts(usize, Option<(String, String)>, String, String, bool),
}

/// One length arm of a `ScanSplitFork`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageSplitArm {
    pub len: usize,
    pub parts: Vec<PageSplitPart>,
}

/// One part of a `PageSplitArm` template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageSplitPart {
    Lit(String),
    /// The split field at this index.
    Field(usize),
}

/// One part of a two-match template: a literal,
/// or capture `cap` of match `which` (0 or 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TwoPart {
    Lit(String),
    Cap(usize, usize),
}

/// An enumerable condition over one match's
/// captures (indexes in the shape's own space).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageCond {
    CapEq(usize, usize),
    CapNe(usize, usize),
    CapEqLit(usize, String),
    /// `match[N].blank?`
    CapBlank(usize),
    /// `match[N].present?`
    CapPresent(usize),
    /// bare `match[N]` (participating capture).
    CapTruthy(usize),
    /// `match[A] == match[B].tr(from, to)`
    CapTrEq(usize, usize, String, String),
    /// `match[N].include?("lit")`
    CapIncludes(usize, String),
    /// `match[N].count("c") >= K`
    CapCountGe(usize, char, usize),
    /// `match[A] == match[B][/pat/flags]` (the whole
    /// sub-match of capture B).
    CapEqSub(usize, usize, String, String),
    And(Box<PageCond>, Box<PageCond>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageShape {
    /// `strategy :page_match, &:strip` — the whole
    /// page, stripped.
    PageStrip,
    /// `match[0].tr("from", "to")` (also written
    /// `match.first.tr(…)`): Ruby `String#tr` of the
    /// regex's first capture.
    ScanTr(String, String),
    /// `Date.parse(match.first)&.strftime("%Y%m%d")`:
    /// the capture parsed as a date, rendered basic
    /// ISO (`YYYYMMDD`).
    ScanDateYmd,
    /// `match[0].gsub("from", "to")`: literal
    /// replace-all (no backslash escapes in either
    /// argument).
    ScanGsub(String, String),
    /// `match.first.sub(/pat/flags, "to")`: first
    /// regex match replaced by a literal (no
    /// backreferences — the replacement is
    /// backslash-free).
    ScanSub(String, String, String),
    /// `((v = match.first) == "x") ? nil : v`: the
    /// named version is dropped (`nil`, compacted
    /// away by `handle_block_return`).
    ScanRejectEq(String),
    /// `match.join(".")`: all captures joined
    /// (nil renders empty, as Ruby's `join`).
    ScanJoin(String),
    /// `match[1] ? "#{match[0]},#{match[1]}" : match[0]`.
    ScanCsvOpt,
    /// `v = match[0].tr(a, b); match[1].present? ?
    /// "#{v},#{match[1]}" : v`.
    ScanCsvOptTr(String, String),
    /// `"#{match[2]},#{match[0]}"` per scan match
    /// (indexes are capture indexes, 0 = group 1).
    ScanTemplate(Vec<PagePart>),
    /// `match = page.match(regex)` + template
    /// (indexes are MatchData indexes, 0 = whole).
    MatchTemplate(Vec<PagePart>),
    /// Scan-space fork: `<cond> ? A : B` or
    /// if/else, arms as template parts.
    ScanFork {
        cond: PageCond,
        then: Vec<PagePart>,
        els: Vec<PagePart>,
    },
    /// MatchData-space fork after `page.match`.
    MatchFork {
        cond: PageCond,
        then: Vec<PagePart>,
        els: Vec<PagePart>,
    },
    /// `match[0] = match[0].tr(a, b)` then
    /// `match.compact.join(sep)`.
    ScanTrCompactJoin(String, String, String),
    /// `match[0].split("", N).join(sep)`: at most
    /// N char pieces (last keeps the remainder).
    ScanSplitChars(usize, String),
    /// `scan.map(&:first)[.uniq].min_by/max_by {
    /// Version.new }`: the extreme first capture
    /// by version order (`true` = min).
    ScanExtreme(bool),
    /// A scan fork whose items are first skipped
    /// when a capture matches a literal regex
    /// (`next if match[N]&.match?(/…/)`).
    ScanGuardedFork {
        skip_cap: usize,
        skip_pat: String,
        skip_flags: String,
        cond: PageCond,
        then: Vec<PagePart>,
        els: Vec<PagePart>,
    },
    /// `parse_json(match[N])` then a dig.
    MatchJsonDig { cap: usize, path: Vec<JsonKey> },
    /// A literal-regex capture parsed as JSON, dug,
    /// then matched against the block regex and
    /// rendered (popo).
    MatchJsonDigMatch {
        lit: (String, String),
        cap: usize,
        path: Vec<JsonKey>,
        parts: Vec<HopFinalPart>,
    },
    /// A literal-regex capture parsed as JSON, dug
    /// to a hash of file lists; each file's `field`
    /// matches the block regex and renders with an
    /// optional second field (the `Hop` part),
    /// blank-guarded (lg-onscreen-control).
    MatchJsonNestedMap {
        lit: (String, String),
        cap: usize,
        path: Vec<JsonKey>,
        field: String,
        extra: Option<String>,
        parts: Vec<HopFinalPart>,
    },
    /// `page.scan(/lit/).map { |m| t = m[idx];
    /// t = t.gsub(/strip/, repl) until stable;
    /// t[regex, cap] }` (ripx's tag stripping).
    ScanStripMatch {
        pat: String,
        flags: String,
        idx: usize,
        strip: (String, String),
        repl: String,
        cap: usize,
    },
    /// The block-regex scan's lexical maximum; a
    /// dotless maximum is remapped to the first
    /// literal-scan candidate with the same
    /// dotless form (font-kikai-chokoku-jis).
    ScanDottedMax { lit: (String, String) },
    /// Scan maximum by `Time.parse` of capture
    /// `time_idx` (scan indexing); the winner's
    /// first capture split on `sep` (limit 2)
    /// into version/build, held back to the
    /// current csv while within `step` builds
    /// (bettertouchtool@alpha).
    ScanTimeMaxThrottle {
        time_idx: usize,
        sep: String,
        step: i64,
    },
    /// The block-regex scan maximum as a Version,
    /// with an optional literal-regex capture
    /// appended after a separator (hy-rpe2).
    ScanMaxOptCap {
        pat: String,
        flags: String,
        cap: usize,
        sep: String,
        tr: Option<(String, String)>,
    },
    /// `page.split(/pat/).filter_map { |section|
    /// [next if section.match?(skip)] section[regex, 1] }`
    SplitCapture {
        pat: String,
        flags: String,
        skip: Option<(String, String)>,
    },
    /// `page.split(/pat/).map { |section|
    /// section.scan(regex).map { |match| "…" } }
    /// .flatten`: the block regex scans each
    /// split section in isolation.
    SplitScan {
        pat: String,
        flags: String,
        parts: Vec<PagePart>,
    },
    /// `page.scan(/pat/).filter_map { |row|
    /// match = row[0].match(regex); <fork over
    /// match> }`: the block regex matches within
    /// each scanned section's first capture.
    SectionMatch {
        pat: String,
        flags: String,
        cond: PageCond,
        then: Vec<PagePart>,
        els: Vec<PagePart>,
    },
    /// `<capture>.split(sep)` rendered by the arm
    /// for its field count; an unmatched count
    /// re-emits the previous arm's value (the
    /// body's local persists across iterations).
    ScanSplitFork {
        sep: String,
        arms: Vec<PageSplitArm>,
    },
    /// `page.split("sep").first[a..b].chars.join("j")`
    /// (no regex): chars `start..start+len` of the
    /// first split segment, joined.
    SplitHeadChars {
        sep: String,
        start: usize,
        len: usize,
        join: String,
    },
    /// Two literal `page.match(/…/)` locals and a
    /// template over both matches' captures
    /// (no block regex).
    TwoMatch {
        a_pat: String,
        a_flags: String,
        b_pat: String,
        b_flags: String,
        parts: Vec<TwoPart>,
    },
    /// `match.first.sub(/pat/flags, '\1.\2')`: first
    /// regex match replaced by a backreference
    /// template (`\1`..`\9`, `\0` whole, `\\`).
    ScanSubBackref(String, String, String),
    /// `match[0].tr(a, b).sub(/pat/flags, '-\1')`:
    /// the two transforms chained.
    ScanTrSubBackref(String, String, String, String, String),
    /// `match[0].scan(/\d{N}/).map(&:to_i).join(sep)`:
    /// N-digit chunks read as integers, joined.
    ScanChunks(usize, String),
    /// Two-stage directory walk: scan version
    /// directories on the first page, fetch the
    /// newest directory's page, scan it for files.
    DirWalk(Box<DirWalk>),
    /// `Date.parse(page[regex, 1]).strftime(fmt)`
    /// (or via `page.match(regex)[1]`): the first
    /// capture parsed as a date — `true` renders
    /// dashed (`%Y-%m-%d`), `false` basic (`%Y%m%d`).
    FirstDate(bool),
    /// `page.scan(regex).select { |m| Version.new(m.first) < N }.flatten`:
    /// keep captures whose version is below the bound
    /// (the regex must have exactly one group).
    ScanSelectLt(String),
    /// A second-fetch ("hop") body: extract a path or
    /// directory value from the first page, fetch the
    /// page it names, and scan that with the block
    /// regex.
    HopScan(Box<HopScan>),
    /// Pick a value from page one, fetch the named
    /// URL's response headers, and render from a
    /// header match (grafx, platinum-notes, linqpad).
    HeaderHop(Box<HeaderHop>),
}

/// An intermediate page fetch + bracket
/// extraction between a header hop's first pick
/// and its headers request (mixed-in-key).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidHop {
    pub url_pre: String,
    pub url_post: String,
    pub lit: (String, String),
    pub cap: usize,
}

/// A headers-hop page block reduced to data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderHop {
    pub hop_re: Option<(String, String)>,
    pub pick: HopPick,
    pub target: HopTarget,
    /// The header read first (lowercase).
    pub header: String,
    /// `(headers["a"] || headers["b"])` fallback.
    pub fallback: Option<String>,
    /// Render per response header set (the
    /// `page_headers(...).map` form) instead of
    /// over the merged headers.
    pub per_set: bool,
    /// Match the header with this literal regex
    /// (capture 1) instead of the block regex.
    pub lit_cap: Option<(String, String)>,
    pub parts: Vec<HopFinalPart>,
    /// A chained second pick through an
    /// intermediate page (its value replaces the
    /// first pick everywhere downstream).
    pub mid: Option<MidHop>,
}

/// A reduced second-fetch page block: how the hop
/// value is picked from page one, how the second
/// URL is formed, and whether only the last stage-2
/// capture is the version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HopScan {
    /// The hop regex (source, flags); `None` = the
    /// block regex.
    pub hop_re: Option<(String, String)>,
    pub pick: HopPick,
    pub target: HopTarget,
    /// Stage 2 keeps only its last capture
    /// (`.scan(regex)&.flatten&.last`).
    pub last_only: bool,
    /// Stage 2 instead single-matches the block
    /// regex and renders this template over its
    /// captures and the hop value.
    pub final_match: Option<Vec<HopFinalPart>>,
    /// Stage 2 instead scans and renders this
    /// template per match (scan-indexed captures).
    pub final_scan: Option<Vec<PagePart>>,
}

/// One part of a hop-scan final-match template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HopFinalPart {
    Lit(String),
    /// `match[N]`
    Cap(usize),
    /// The hop value itself.
    Hop,
}

/// How a hop-scan picks its value from page one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HopPick {
    /// `page[/lit/, 1]`
    First,
    /// `page.scan(re)&.flatten&.last`
    ScanLast,
    /// `page.scan(re).map { … }.max_by { Version.new }`
    ScanMax,
    /// `page.scan(re).flatten.uniq.max` (lexical).
    ScanLexMax,
    /// `page.scan(re).max_by { |m| Version.new(m[K]) }
    /// &.first`: the winning row's first capture,
    /// keyed by scan capture K (0-based).
    ScanMaxKey(usize),
    /// `page.match(re)`: capture 1 of the first
    /// match; the hop slot spells `#{var[1]}`.
    MatchFirst,
    /// Scan the hop regex inside a section of the
    /// page delimited by this literal regex's whole
    /// match, taking the Version maximum.
    SectionScanMax { section: (String, String) },
}

/// How a hop-scan forms the second URL from the
/// hop value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HopTarget {
    /// `URI.join("<literal>", v)`
    JoinLiteral(String),
    /// `URI.join(@url, v)`
    JoinSelf,
    /// `"<pre>#{v}<post>"` (`\u{2}` marks the spec
    /// URL, from a `#{url}` interpolation).
    Template(String, String),
    /// `URI.join(base, "<pre>#{v}<post>")`; `None`
    /// base is `@url`.
    JoinTemplate {
        base: Option<String>,
        pre: String,
        post: String,
    },
}

/// A two-stage directory-walk page block reduced
/// to data (gnu-getopt, texlive, singular).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirWalk {
    /// Stage-1 scan regex (source, flags); `None` =
    /// the block's `regex`.
    pub stage1_regex: Option<(String, String)>,
    /// `Version.new(v.tr(a, b))` on stage-1 captures.
    pub stage1_tr: Option<(String, String)>,
    /// `.reject { |v| v.patch >= N }`.
    pub reject_patch_ge: Option<u64>,
    /// Try the last two directories (gnu-getopt)
    /// instead of only the newest.
    pub last_two: bool,
    /// Try every stage-1 candidate in scan order
    /// until one's stage-2 scan is non-empty
    /// (qt-creator's lazy walk).
    pub scan_order: bool,
    /// `url.sub(needle, "<pre>#{v}<post>")` directory
    /// URL formation instead of a join.
    pub url_sub: Option<(String, String, String)>,
    /// Directory URL template around the (possibly
    /// `tr`-ed) stage-1 version: pre + version + post,
    /// joined onto the livecheck URL.
    pub dir_pre: String,
    pub dir_tr: Option<(String, String)>,
    pub dir_post: String,
    /// Stage-2 scan regex; `None` = the block's.
    pub stage2_regex: Option<(String, String)>,
    /// A blank directory page: try the next
    /// candidate, return stage 1, or return nothing.
    pub blank: DirBlank,
    /// An empty stage-2 scan falls back to the
    /// stage-1 versions (`dir_versions.presence`).
    pub empty_scan_stage1: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirBlank {
    NextCandidate,
    Stage1,
    Nothing,
}

/// A `:github_releases` strategy block reduced to
/// data: per-release guards, an optional release-
/// level pre-capture local, the text the regex
/// captures from, and what the map returns per match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhRelBlock {
    pub skip_draft: bool,
    pub skip_prerelease: bool,
    /// `next unless release["prerelease"]` (and its
    /// `!`-guard spelling).
    pub require_prerelease: bool,
    /// `next if release["tag_name"] != "lit"`.
    pub tag_ne: Option<String>,
    /// `next unless release["tag_name"]&.start_with?("lit")`.
    pub tag_prefix: Option<String>,
    /// `next unless release["assets"]&.any? { … }`:
    /// at least one asset must satisfy the test.
    pub asset_any: Option<GhRelAny>,
    /// Skip when the release `name` matches this
    /// regex (source, flags), searched.
    pub name_skip: Option<(String, String)>,
    /// `local = release[field]&.[](re, 1)` with a
    /// blank guard; renders read it as `GhRelPart::Local`.
    pub pre_capture: Option<GhRelCapture>,
    pub source: GhRelSource,
    /// The `each` + `break` form: only the first
    /// rendered version is returned.
    pub first_only: bool,
    pub then: GhRelThen,
    /// `end.presence || release["tag_name"]&.[](/…/, 1)`
    /// on an assets `filter_map`: when a release's
    /// assets yield nothing, this literal regex
    /// (source, flags) captures from the tag instead.
    pub fallback_tag: Option<(String, String)>,
    /// `next <local> if match.blank?`: an asset whose
    /// text does not match yields the local instead.
    pub blank_emit_local: bool,
    /// `json[0...N].map`: only the first N releases.
    pub limit: Option<usize>,
    /// Per-asset `next if <cap form> != <own form>`
    /// guards (free-gpgmail's major/suffix pins).
    pub own_guards: Vec<GhRelMatchGuard>,
}

/// One transform over a capture or an own-version
/// csv field inside a `GhRelMatchGuard`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GhRelXform {
    Id,
    /// `.split("sep").first`.
    SplitFirst(String),
    /// `&.sub(/pat/flags, "rep")` (searched, literal
    /// replacement without backrefs).
    SubRe(String, String, String),
}

/// `next if match[cap]<xform> != version.csv.n<xform>`:
/// the asset is skipped unless both sides read equal
/// (nil compares unequal to any string).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhRelMatchGuard {
    pub cap: usize,
    pub cap_xform: GhRelXform,
    /// Own-version csv field index (0 = first).
    pub own_csv: usize,
    pub own_xform: GhRelXform,
}

/// An asset-existence test over asset names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GhRelAny {
    /// `asset["name"]&.match?(/…/)` (source, flags).
    NameMatch(String, String),
    /// `asset["name"]&.end_with?("lit")`.
    NameEndsWith(String),
}

/// How a release-level ghrel local reads its field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GhRelLocalOp {
    /// `&.[](re, 1)`: first capture of `lit` (or the
    /// livecheck regex when `lit` is `None`).
    Capture,
    /// The field verbatim.
    Field,
    /// `&.delete_prefix("lit")` on the field.
    DeletePrefix(String),
}

/// A release-level local over one release field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhRelCapture {
    /// `"name"` or `"tag_name"`.
    pub field: String,
    /// A literal regex (source, flags); `None` reads
    /// the livecheck's own regex.
    pub lit: Option<(String, String)>,
    pub op: GhRelLocalOp,
    /// `next if <local>.blank?` skips the release
    /// when the local reads blank.
    pub blank_guard: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GhRelSource {
    /// `release["tag_name"]`
    TagName,
    /// Each asset's `browser_download_url`
    /// (the block flattens).
    AssetDownloadUrl,
    /// Each asset's `name` (the block flattens).
    AssetName,
    /// No per-item match: the render reads only the
    /// pre-capture local and the tag.
    PreCaptureOnly,
    /// `release["name"]`
    Name,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GhRelThen {
    /// `match[1]`
    Identity,
    /// `match[1]&.tr("from", "to")`
    Tr(String, String),
    /// `Version.new(match[1])`, skipped when its
    /// patch token is zero/absent.
    NonZeroPatch,
    /// An interpolation/body expression over parts.
    Template(Vec<GhRelPart>),
    /// `(a == b) ? then_parts : else_parts`.
    EqElse {
        a: GhRelOp,
        b: GhRelOp,
        then: Vec<GhRelPart>,
        els: Vec<GhRelPart>,
    },
    /// `if b && a != b then … else … end`: the
    /// then-render when `b` participates and the
    /// operands differ.
    CapNeElse {
        a: GhRelOp,
        b: GhRelOp,
        then: Vec<GhRelPart>,
        els: Vec<GhRelPart>,
    },
    /// `match[n] ? then : els` (Ruby truthiness: only
    /// a non-participating capture is falsy).
    TruthyElse {
        cap: usize,
        then: Vec<GhRelPart>,
        els: Vec<GhRelPart>,
    },
    /// `match.to_a.compact.drop(1).join("sep")`: every
    /// participating capture joined.
    CompactJoin(String),
}

/// One operand of a render template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GhRelPart {
    /// `match[n]` (MatchData indexing: 0 is whole).
    Cap(usize),
    Lit(String),
    /// The pre-capture local.
    Local,
    /// `release["tag_name"]` verbatim.
    Tag,
    /// `release["tag_name"]&.tr("from", "to")`.
    TagTr(String, String),
    /// `release["tag_name"]&.[](/…/, 1)` (source, flags).
    TagCapture(String, String),
}

/// One side of an `EqElse` comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GhRelOp {
    Cap(usize),
    Local,
    /// The release's raw `tag_name`.
    Tag,
}

/// A `:git` strategy block reduced to data: tags to
/// skip outright, and the transform applied to each
/// remaining tag's regex capture (`tag[regex, 1]`).
/// `map` and `filter_map` bodies are the same data:
/// the oracle compacts block returns either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitBlock {
    /// The body reads `regex` without declaring it as
    /// a block parameter, i.e. the livecheck's own
    /// regex; without one the oracle yields no regex
    /// to the body (and crashes), so planning refuses.
    pub needs_spec_regex: bool,
    /// Tags skipped before capture
    /// (`next if malformed_tags.include?(tag)`).
    pub skip_tags: Vec<String>,
    pub op: GitOp,
    /// A `match = tag.match(regex)` body rendering
    /// over all captures (supersedes `op`).
    pub match_template: Option<Vec<LatestPart>>,
    /// `match[1].split(sep).include?(lit)` skips
    /// the tag.
    pub skip_split_contains: Option<(String, String)>,
}

/// The transform a reduced `:git` block applies to a
/// tag's regex capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitOp {
    /// The capture verbatim.
    Identity,
    /// `&.tr("from", "to")`
    Tr(String, String),
    /// `&.gsub("from", "to")` (literal, all matches).
    GsubLit(String, String),
    /// `&.gsub(/pat/flags, "template")` (all matches;
    /// template `\0`..`\9` backreferences, `\\` a
    /// backslash).
    GsubRe(String, String, String),
}

/// A `:github_latest` strategy block reduced to
/// data: which release text the regex reads, scan
/// vs first capture, and an optional `tr` on each
/// capture. With a block the oracle applies no
/// draft/prerelease filter of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatestBlock {
    /// The body reads `regex` without declaring it
    /// (see `GitBlock::needs_spec_regex`).
    pub needs_spec_regex: bool,
    pub source: LatestSource,
    /// `.scan(regex).map { |m| m[0] }` (every match's
    /// first capture) vs a single first capture.
    pub scan: bool,
    pub tr: Option<(String, String)>,
    /// A match/scan render beyond the plain capture
    /// (`Capture` keeps the capture+`tr` behavior).
    pub render: LatestRender,
    /// For `LatestRender::TwoEq`: the second field
    /// whose first capture `source`'s is compared to.
    pub two_b: Option<LatestSource>,
    /// A block-level local computed from one field
    /// before the assets map (blank-guarded); renders
    /// read it as `LatestPart::Local`.
    pub pre_capture: Option<LatestPre>,
    /// The match reads the pre-capture local itself
    /// (`match = tag&.match(regex)` after a transform).
    pub match_pre: bool,
    /// `assets = json["assets"]&.select { %w[…]
    /// .include?(asset["name"]) }` with a count guard:
    /// (names, required count).
    pub asset_pick: Option<(Vec<String>, usize)>,
}

/// A `:github_latest` pre-capture local: one field's
/// first capture (or the field itself) with plain
/// string ops applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatestPre {
    pub source: LatestSource,
    /// A literal capture regex (source, flags);
    /// `None` captures with the livecheck regex.
    pub lit: Option<(String, String)>,
    /// Whether the local is a regex capture at all
    /// (vs the field text with `ops` applied).
    pub capture: bool,
    pub ops: Vec<LatestPreOp>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LatestPreOp {
    Tr(String, String),
    DeletePrefix(String),
    /// `&.sub(/pat/flags, "rep")` (backref-free).
    SubRe(String, String, String),
}

/// What a reduced `:github_latest` block returns
/// per match (MatchData indexing except the `Scan*`
/// forms, which index scan capture arrays where 0
/// is capture 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LatestRender {
    Capture,
    Template(Vec<LatestPart>),
    EqElse {
        a: LatestOp,
        b: LatestOp,
        then: Vec<LatestPart>,
        els: Vec<LatestPart>,
    },
    PresentElse {
        cap: usize,
        then: Vec<LatestPart>,
        els: Vec<LatestPart>,
    },
    /// `match[n] ? then : els` (Ruby truthiness).
    TruthyElse {
        cap: usize,
        then: Vec<LatestPart>,
        els: Vec<LatestPart>,
    },
    ScanTemplate(Vec<LatestPart>),
    ScanPresentElse {
        cap: usize,
        then: Vec<LatestPart>,
        els: Vec<LatestPart>,
    },
    /// `(a == b) ? b : "#{a},#{b}"` over two fields'
    /// first captures (`source` is `a`, `two_b` is `b`).
    TwoEq,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LatestPart {
    Cap(usize),
    /// `match[n].tr("from", "to")`.
    CapTr(usize, String, String),
    Lit(String),
    /// The `LatestPre` local.
    Local,
    /// `<picked>.map { |a| a["updated_at"] }.max
    /// .gsub(/\D/, "")`: the newest picked asset's
    /// update timestamp, digits only.
    AssetsMaxUpdatedDigits,
}

/// One side of a latest `EqElse` comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LatestOp {
    Cap(usize),
    /// `match[n].tr("from", "to")`.
    CapTr(usize, String, String),
    /// `Version.new(match[n]).major_minor_patch.to_s`.
    CapMmp(usize),
    Local,
}

/// Which latest-release text a reduced
/// `:github_latest` block regexes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LatestSource {
    /// `json["tag_name"]`
    TagName,
    /// `json["name"]`
    Name,
    /// `json["body"]`
    Body,
    /// `json["published_at"]`
    PublishedAt,
    /// Each `json["assets"]` entry's `name`
    /// (first capture each).
    AssetName,
    /// Each `json["assets"]` entry's
    /// `browser_download_url`.
    AssetDownloadUrl,
}

/// A `:header_match` strategy block reduced to
/// data: one response header's value, matched or
/// scanned with the spec regex, rendered as a
/// version template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderBlock {
    pub header: String,
    pub shape: HeaderShape,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeaderShape {
    /// `match = headers[h]&.match(regex)` guarded,
    /// MatchData-indexed template.
    Template(Vec<PagePart>),
    /// `headers[h]&.scan(regex)&.map { … }`,
    /// scan-indexed per match.
    ScanTemplate(Vec<PagePart>),
    /// `match = headers[h]&.scan(regex)&.flatten`
    /// guarded, template over the flat captures.
    ScanFlatten(Vec<PagePart>),
    /// `headers[h]` verbatim.
    Bare,
    /// `all_headers.each` first match: the first
    /// response whose `header` value matches,
    /// rendered as a MatchData template.
    AllTemplate(Vec<PagePart>),
    /// `match[2] ? "#{match[1]},#{match[2]}" : match[1]`.
    CsvOpt,
    /// A guarded-match fork over captures
    /// (MatchData indexes).
    Fork {
        cond: PageCond,
        then: Vec<PagePart>,
        els: Vec<PagePart>,
    },
}

/// A `:sparkle` strategy block reduced to data.
/// The oracle filters and sorts appcast items, then
/// yields either the first (`|item|`, block-pass
/// `&:field`) or all of them (`|items|`); the block
/// returns version strings the oracle compacts and
/// deduplicates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SparkleBlock {
    /// `|items|`: the expression runs per item and
    /// every result is a version. Otherwise only the
    /// first (channel-matching) item is evaluated.
    pub all: bool,
    /// The block is an `items.find` form: the match
    /// guard participates in item selection (the
    /// oracle's `find` skips non-matching items)
    /// rather than vetoing the newest item.
    pub find: bool,
    /// A channel gate (`items.find { |item|
    /// item.channel.nil? }`, `next if item.channel
    /// != "release"`).
    pub channel: Option<SpChannel>,
    /// A presence gate: skip the item unless the
    /// regex matches this field
    /// (`next unless item.x&.match?(regex)`).
    pub match_guard: Option<SpField>,
    /// Field-presence gates: skip the item when any
    /// listed field is blank (`next if
    /// item.version.blank?`, `next if
    /// item&.version.nil? || …`).
    pub present: Vec<SpField>,
    /// The body reads the spec `regex` (declared as
    /// a block parameter or not); without one the
    /// oracle errors, so planning refuses.
    pub needs_spec_regex: bool,
    /// `next version if item.<field> == "lit"`: yield
    /// the package's own version instead of `expr`.
    pub own_if: Option<(SpField, String)>,
    pub expr: SpExpr,
}

/// A sparkle channel gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpChannel {
    /// `item.channel.nil?`
    Nil,
    /// `item.channel == "x"`
    Eq(String),
    /// `item.channel != "x"` (a `next if` skip).
    Ne(String),
    /// `item.channel == "x" || item.channel.nil?`
    EqOrNil(String),
}

/// A Sparkle item field a block can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpField {
    Short,
    Full,
    Nice,
    Title,
    Url,
}

/// One part of a sparkle string-interpolation
/// template (`"#{item.short_version},#{match[1]}"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpPart {
    Lit(String),
    Field(SpField),
    /// `match[N]`; in a `scan` the indexing is over
    /// the captures array (N is capture N+1).
    Cap(usize),
    /// `match[N].tr("from", "to")`
    CapTr(usize, String, String),
    /// `match[N].presence || "fallback"`
    CapOr(usize, String),
    /// A precomputed local (`id = item.url[regex, 1]`)
    /// rendered in the template.
    Local,
    /// `item.field.split("sep").first` (or `.last`
    /// when the flag is set) in the template.
    FieldSplit(SpField, String, bool),
    /// `item.field[regex, N]` inline in the template.
    FieldIndex(SpField, usize),
    /// `item.field.sub("a", "b")` inline in the
    /// template (first literal replacement).
    FieldSub(SpField, String, String),
    /// `(item.field.count("c") >= min) ? item.field :
    /// "#{item.field}<suffix>"` as a named local.
    CountPad {
        field: SpField,
        needle: String,
        min: usize,
        suffix: String,
    },
}

/// The enumerable expression shapes a reduced
/// `:sparkle` block body can take (see
/// `tap::parse_sparkle_shape`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpExpr {
    /// `item.field` (also block-pass `&:field` and
    /// `items.map(&:field)`).
    Field(SpField),
    /// `item.field.split.first`
    FieldSplitFirst(SpField),
    /// `item.field.tr("from", "to")`
    FieldTr(SpField, String, String),
    /// `item.field.delete_prefix("p")`
    FieldDeletePrefix(SpField, String),
    /// `item.field.delete_suffix("p")`
    FieldDeleteSuffix(SpField, String),
    /// `item.field[regex, N]` (optionally safe-nav).
    FieldIndex(SpField, usize),
    /// `item.field[regex, N]&.tr("a", "b")`.
    FieldIndexTr(SpField, usize, String, String),
    /// `match.captures.join(",")` over the match of
    /// the regex against a field.
    CapturesJoin(SpField),
    /// A `"#{...}"` template. `from` names the field
    /// the regex matched (`match = item.f.match(regex)`,
    /// blank-guarded); `local` names the field+index
    /// a `Local` part renders (`id = item.f[regex, N]`,
    /// blank-guarded). A `Cap(0)` is the whole match.
    Template {
        from: Option<SpField>,
        local: Option<(SpField, usize)>,
        parts: Vec<SpPart>,
    },
    /// `item.field.scan(regex).map { |match| "..." }`
    /// — one template per match; `Cap(N)` indexes the
    /// captures array.
    Scan { from: SpField, parts: Vec<SpPart> },
    /// `item.field.chars.join("sep")`.
    FieldCharsJoin(SpField, String),
    /// `(a == b) ? then : els` over the match against
    /// `from` (blank-guarded like `Template`).
    EqElse {
        from: Option<SpField>,
        local: Option<(SpField, usize)>,
        a: SpOp,
        b: SpOp,
        then: Vec<SpPart>,
        els: Vec<SpPart>,
    },
    /// `next <then> unless match[n]` (or `if`, when
    /// `invert` is false) before the `els` template.
    CapElse {
        from: Option<SpField>,
        cap: usize,
        invert: bool,
        then: Vec<SpPart>,
        els: Vec<SpPart>,
    },
    /// `next <blank> if match.blank?` before the
    /// `parts` template.
    BlankElse {
        from: Option<SpField>,
        blank: Vec<SpPart>,
        parts: Vec<SpPart>,
    },
    /// `next <blank> if match.nil? ||
    /// match[cap].blank?` before the `parts`
    /// template (a missing match counts blank).
    CapBlankElse {
        from: Option<SpField>,
        cap: usize,
        blank: Vec<SpPart>,
        parts: Vec<SpPart>,
    },
    /// `match = <local>&.match(/literal/)` (blank
    /// guarded) over a find-selected item's field,
    /// rendered as a template (plistedit-pro).
    LitMatch {
        field: SpField,
        pat: String,
        flags: String,
        parts: Vec<SpPart>,
    },
    /// `if match.blank?` yields the package's own
    /// version when `item.field == version.csv.first`,
    /// else the `blank` render; a match renders
    /// `parts` (soundsource@test).
    BlankOwnElse {
        from: SpField,
        field: SpField,
        blank: Vec<SpPart>,
        parts: Vec<SpPart>,
    },
    /// Compare the dotless short version with the
    /// dotless filename capture: equal renders the
    /// short version, a longer filename appends its
    /// extra digits as a new component, otherwise
    /// the item is skipped (moom).
    DotlessFork {
        short: SpField,
        url: SpField,
        cap: usize,
    },
    /// All items' field values, sorted as versions
    /// and deduplicated, keeping the first of each
    /// patch bucket (`patch - patch % bucket`)
    /// in ascending order (commander).
    ThrottleUniq { field: SpField, bucket: i64 },
    /// `item.field.scan(regex).map do |match|
    /// if a == b … else … end end`.
    ScanEqElse {
        from: SpField,
        a: SpOp,
        b: SpOp,
        then: Vec<SpPart>,
        els: Vec<SpPart>,
    },
}

/// One side of a sparkle `EqElse` comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpOp {
    Cap(usize),
    Field(SpField),
}

/// An `:extract_plist` strategy block reduced to data:
/// one bundle-identifier lookup of one version property,
/// optionally with the regex's first capture applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlistBlock {
    pub has_regex: bool,
    pub bundle_id: String,
    pub prop: PlistProp,
    pub regex: bool,
}

/// Which plist version a block reads (`short_version`
/// = `CFBundleShortVersionString`, `version` =
/// `CFBundleVersion`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlistProp {
    Short,
    Bundle,
}

/// The data-only shapes a `:yaml` block body can take:
/// a dig/index path into the document, optionally with
/// the regex's first capture applied (the dig path reuses
/// `JsonKey` steps).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum YamlShape {
    Dig(Vec<JsonKey>),
    DigRegex(Vec<JsonKey>),
    /// `….split("-")&.first` — value up to the first
    /// separator.
    DigSplitFirst(Vec<JsonKey>, String),
    /// A dug string with a plain tail transform
    /// (`yaml["version"]&.delete_suffix("-ow")`).
    DigTail(Vec<JsonKey>, YamlTail),
    /// `yaml["files"].map { |item| match item[key] … }`
    /// (the electron-builder files list).
    FilesMap {
        key: String,
        render: YamlRender,
    },
    /// `match = yaml[key]&.match(regex); … render`
    /// against a root scalar (the electron-builder
    /// root `path`).
    RootMatch {
        key: String,
        render: YamlRender,
    },
    /// `yaml[path]&.map { |item| <guards> item[key] }`:
    /// each dug item's scalar (raw spelling, so a
    /// trailing `.to_s` is already satisfied).
    MapKey {
        path: Vec<JsonKey>,
        key: String,
        guards: Vec<JsonGuard>,
    },
}

/// A plain-string tail on a dug yaml scalar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum YamlTail {
    DeleteSuffix(String),
    DeletePrefix(String),
    /// `sub("a", "b")` (first literal occurrence).
    SubLit(String, String),
    /// `sub(/pat/flags, "b")` with a backslash-free
    /// replacement (no backrefs modeled).
    SubRe(String, String, String),
}

/// A yaml match render over regex captures and root
/// fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum YamlRender {
    Template(Vec<YamlPart>),
    /// `match[n].present? ? then : els`.
    PresentElse {
        cap: usize,
        then: Vec<YamlPart>,
        els: Vec<YamlPart>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum YamlPart {
    /// `match[n]` (MatchData indexing).
    Cap(usize),
    Lit(String),
    /// `yaml["field"]` as a string.
    Root(String),
}

/// A livecheck `url` argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LivecheckUrl {
    /// `:stable`, `:homepage`, `:head`, `:url`, … (no colon).
    Symbol(String),
    Literal(String),
}

/// Parse the API `service` object; a service without a run command
/// (name-only `to_hash`) has nothing caveats can honestly show.
fn parse_service(v: Option<&Value>) -> Option<ServiceInfo> {
    let v = v?;
    let run = match v.get("run") {
        Some(Value::Array(items)) => items
            .iter()
            .map(|i| i.as_str().map(str::to_string))
            .collect::<Option<Vec<_>>>()?,
        Some(Value::String(single)) => vec![single.clone()],
        _ => return None,
    };
    if run.is_empty() {
        return None;
    }
    let mut environment_variables = Vec::new();
    if let Some(Value::Object(map)) = v.get("environment_variables") {
        for (key, value) in map {
            if let Some(value) = value.as_str() {
                environment_variables.push((key.clone(), value.to_string()));
            }
        }
    }
    Some(ServiceInfo {
        run,
        require_root: v
            .get("require_root")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        environment_variables,
        name: match v.get("name") {
            Some(Value::String(one)) => Some(one.clone()),
            Some(other) => other
                .get("macos")
                .and_then(Value::as_str)
                .map(str::to_string),
            None => None,
        }
        .filter(|n| !n.is_empty()),
        working_dir: v
            .get("working_dir")
            .and_then(Value::as_str)
            .map(str::to_string),
        root_dir: v
            .get("root_dir")
            .and_then(Value::as_str)
            .map(str::to_string),
        log_path: v
            .get("log_path")
            .and_then(Value::as_str)
            .map(str::to_string),
        error_log_path: v
            .get("error_log_path")
            .and_then(Value::as_str)
            .map(str::to_string),
        interval: v.get("interval").and_then(Value::as_i64),
        cron: v
            .get("cron")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|c| !c.trim().is_empty()),
    })
}

/// The `deprecate!`/`disable!` fields of an API object, kept
/// verbatim for brew's DeprecateDisable message rendering.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeprecateDisable {
    pub deprecation_reason: Option<String>,
    pub deprecation_date: Option<String>,
    pub disable_reason: Option<String>,
    pub disable_date: Option<String>,
    pub deprecation_replacement_formula: Option<String>,
    pub deprecation_replacement_cask: Option<String>,
    pub disable_replacement_formula: Option<String>,
    pub disable_replacement_cask: Option<String>,
}

fn parse_deprecate_disable(v: &Value) -> DeprecateDisable {
    let field = |key: &str| v.get(key).and_then(Value::as_str).map(str::to_owned);
    DeprecateDisable {
        deprecation_reason: field("deprecation_reason"),
        deprecation_date: field("deprecation_date"),
        disable_reason: field("disable_reason"),
        disable_date: field("disable_date"),
        deprecation_replacement_formula: field("deprecation_replacement_formula"),
        deprecation_replacement_cask: field("deprecation_replacement_cask"),
        disable_replacement_formula: field("disable_replacement_formula"),
        disable_replacement_cask: field("disable_replacement_cask"),
    }
}

/// One bottle file of a formula's stable version.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Bottle {
    pub tag: String,
    pub url: String,
    pub sha256: String,
    /// Where the bottle was built to live: `:any_skip_relocation` needs no relocation.
    pub cellar: String,
}

fn s(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).unwrap_or_default().to_string()
}

fn str_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn empty_metadata(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::Array(items)) => items.is_empty(),
        Some(Value::Object(items)) => items.is_empty(),
        _ => false,
    }
}

fn plain_strings(value: Option<&Value>) -> Option<Vec<String>> {
    match value {
        None | Some(Value::Null) => Some(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().map(str::to_owned))
            .collect(),
        _ => None,
    }
}

fn plain_names(value: Option<&Value>) -> Option<Vec<String>> {
    if empty_metadata(value) {
        return Some(Vec::new());
    }
    let names = plain_strings(value)?;
    names
        .iter()
        .all(|name| super::valid_name(name))
        .then_some(names)
}

pub(super) fn numeric_macos_version(version: &str) -> bool {
    !version.is_empty()
        && version.split('.').all(|part| {
            !part.is_empty()
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && part.parse::<u64>().is_ok()
        })
}

fn appdir_source(source: &str) -> String {
    match source.strip_prefix("$APPDIR/") {
        Some(rest) => format!("/Applications/{rest}"),
        None => source.to_owned(),
    }
}

/// The supported Generic destination mapping without a concrete prefix.
pub(super) fn generic_target_relative(
    target: &str,
) -> Option<std::borrow::Cow<'_, std::path::Path>> {
    if let Some(rest) = target.strip_prefix("$APPDIR/") {
        let rest_path = std::path::Path::new(rest);
        if rest.is_empty()
            || rest_path
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return None;
        }
        return Some(std::borrow::Cow::Owned(
            std::path::PathBuf::from("Applications").join(rest_path),
        ));
    }
    let relative = target
        .strip_prefix("$HOMEBREW_PREFIX/")
        .or_else(|| target.strip_prefix("~/"))
        .or_else(|| target.strip_prefix("/$HOME/"))
        .or_else(|| target.strip_prefix("$HOME/"))?;
    let path = std::path::Path::new(relative);
    if relative.is_empty()
        || path
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
        || (!target.starts_with("$HOMEBREW_PREFIX/") && !relative.starts_with("Library/"))
    {
        return None;
    }
    Some(std::borrow::Cow::Borrowed(path))
}

/// One formula `variations` entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FormulaVariation {
    pub dependencies: Option<Vec<String>>,
    pub requirements: Option<Vec<Requirement>>,
    pub requirements_unsupported: bool,
    pub conflicts: Option<Vec<String>>,
    pub unsupported: bool,
}

fn parse_formula_variations(v: Option<&Value>) -> Vec<(String, FormulaVariation)> {
    let mut out = Vec::new();
    let Some(Value::Object(map)) = v else {
        if !empty_metadata(v) {
            out.push((
                String::new(),
                FormulaVariation {
                    unsupported: true,
                    ..FormulaVariation::default()
                },
            ));
        }
        return out;
    };
    for (key, entry) in map {
        let mut variation = FormulaVariation::default();
        let Value::Object(fields) = entry else {
            variation.unsupported = true;
            out.push((key.clone(), variation));
            continue;
        };
        for (field, value) in fields {
            match field.as_str() {
                "dependencies" => match plain_names(Some(value)) {
                    Some(names) => variation.dependencies = Some(names),
                    None => variation.unsupported = true,
                },
                "conflicts_with" => match plain_names(Some(value)) {
                    Some(names) => variation.conflicts = Some(names),
                    None => variation.unsupported = true,
                },
                "requirements" => match parse_requirements(Some(value)) {
                    Ok(reqs) => variation.requirements = Some(reqs),
                    Err(()) => variation.requirements_unsupported = true,
                },
                "build_dependencies"
                | "head_dependencies"
                | "test_dependencies"
                | "caveats"
                | "patches"
                | "no_linkage_dependencies"
                | "conflicts_with_reasons"
                | "link_overwrites" => {}
                _ => variation.unsupported = true,
            }
        }
        out.push((key.clone(), variation));
    }
    out
}

/// A formula requirement in a form abrew can evaluate from data alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Requirement {
    /// `depends_on :macos` with an optional numeric floor.
    MacosMin(Option<String>),
    /// `depends_on maximum_macos: X`.
    MacosMax(String),
    /// `depends_on arch: :arm64` / `:x86_64`.
    Arch(String),
    /// `depends_on xcode:` with an optional numeric floor.
    XcodeMin(Option<String>),
}

/// The API `options` array: `{"option": "--with-x",
/// "description": "..."}` entries. Malformed entries are dropped;
/// options are display data only.
fn parse_options(v: Option<&Value>) -> Vec<(String, String)> {
    let Some(items) = v.and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let flag = item.get("option").and_then(Value::as_str)?;
            let description = item
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Some((flag.to_string(), description.to_string()))
        })
        .collect()
}

fn parse_requirements(v: Option<&Value>) -> Result<Vec<Requirement>, ()> {
    let mut out = Vec::new();
    let Some(items) = v.and_then(Value::as_array) else {
        return if empty_metadata(v) { Ok(out) } else { Err(()) };
    };
    for item in items {
        let Value::Object(_) = item else {
            return Err(());
        };
        let specs = plain_strings(item.get("specs")).ok_or(())?;
        if !specs.is_empty() && !specs.iter().any(|s| s == "stable") {
            continue;
        }
        let contexts = plain_strings(item.get("contexts")).ok_or(())?;
        if !contexts.is_empty()
            && contexts
                .iter()
                .all(|c| matches!(c.as_str(), "build" | "test"))
        {
            continue;
        }
        if !contexts.is_empty() {
            return Err(());
        }
        let version = match item.get("version") {
            None | Some(Value::Null) => None,
            Some(Value::String(version)) => Some(version.as_str()),
            _ => return Err(()),
        };
        match item.get("name").and_then(Value::as_str) {
            Some("macos") if version.is_none_or(numeric_macos_version) => {
                out.push(Requirement::MacosMin(version.map(str::to_owned)));
            }
            Some("maximum_macos") if version.is_some_and(numeric_macos_version) => {
                out.push(Requirement::MacosMax(version.ok_or(())?.to_string()));
            }
            Some("arch") if matches!(version, Some("arm64" | "x86_64")) => {
                out.push(Requirement::Arch(version.unwrap_or_default().to_string()));
            }
            Some("xcode") if version.is_none_or(numeric_macos_version) => {
                out.push(Requirement::XcodeMin(version.map(str::to_owned)));
            }
            _ => return Err(()),
        }
    }
    Ok(out)
}

impl Formula {
    /// Parse one formulae.brew.sh formula object.
    pub fn from_api(v: &Value) -> Result<Formula, String> {
        let name = s(v.get("name"));
        if name.is_empty() {
            return Err("abrew: formula JSON has no name".to_string());
        }
        let versions = v.get("versions");
        let stable = v.pointer("/urls/stable");
        let mut bottle_files = Vec::new();
        let pour = v.get("pour_bottle_only_if").and_then(Value::as_str);
        let pour_requires_clt = pour == Some("clt_installed");
        let requirements = parse_requirements(v.get("requirements"));
        let parsed_conflicts = plain_names(v.get("conflicts_with"));
        let conflicts_plain = parsed_conflicts.is_some();
        let conflicts = parsed_conflicts.unwrap_or_default();
        let conflict_reasons = str_list(v.get("conflicts_with_reasons"));
        let pour_needs = if !matches!(
            v.get("post_install_defined"),
            None | Some(Value::Bool(false))
        ) {
            Some("a post_install recipe".to_string())
        } else if !(matches!(v.get("pour_bottle_only_if"), None | Some(Value::Null))
            || pour_requires_clt)
        {
            Some(match pour {
                Some(condition) => format!("pour_bottle_only_if :{condition}"),
                None => "a pour condition abrew cannot read".to_string(),
            })
        } else if !empty_metadata(v.get("post_install_steps")) {
            Some("post-install steps".to_string())
        } else if requirements.is_err() {
            Some("requirements abrew cannot evaluate".to_string())
        } else if !conflicts_plain {
            Some("conflicts abrew cannot evaluate".to_string())
        } else {
            None
        };
        if let Some(files) = v.pointer("/bottle/stable/files")
            && let Value::Object(map) = files
        {
            for (tag, f) in map {
                bottle_files.push(Bottle {
                    tag: tag.clone(),
                    url: s(f.get("url")),
                    sha256: s(f.get("sha256")),
                    cellar: s(f.get("cellar")),
                });
            }
        }
        bottle_files.sort();
        Ok(Formula {
            name: name.clone(),
            full_name: {
                let f = s(v.get("full_name"));
                if f.is_empty() { name } else { f }
            },
            desc: s(v.get("desc")),
            homepage: s(v.get("homepage")),
            license: s(v.get("license")),
            version: s(versions.and_then(|x| x.get("stable"))),
            revision: v.get("revision").and_then(Value::as_u64).unwrap_or(0),
            stable_url: s(stable.and_then(|x| x.get("url"))),
            stable_sha256: s(stable.and_then(|x| x.get("checksum"))),
            bottle_files,
            bottle_rebuild: v
                .pointer("/bottle/stable/rebuild")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            pour_needs,
            conflicts,
            conflict_reasons,
            requirements: requirements.unwrap_or_default(),
            pour_requires_clt,
            variations: parse_formula_variations(v.get("variations")),
            dependencies: str_list(v.get("dependencies")),
            build_dependencies: str_list(v.get("build_dependencies")),
            test_dependencies: str_list(v.get("test_dependencies")),
            recommended_dependencies: str_list(v.get("recommended_dependencies")),
            optional_dependencies: str_list(v.get("optional_dependencies")),
            version_scheme: v.get("version_scheme").and_then(Value::as_u64).unwrap_or(0),
            compatibility_version: v.get("compatibility_version").and_then(Value::as_u64),
            keg_only: v.get("keg_only").and_then(Value::as_bool).unwrap_or(false),
            keg_only_reason: v.get("keg_only_reason").and_then(|r| {
                let reason = r.get("reason").and_then(Value::as_str)?;
                let explanation = r
                    .get("explanation")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                Some((reason.to_string(), explanation.to_string()))
            }),
            service: parse_service(v.get("service")).map(Box::new),
            livecheck: None,
            no_autobump: !v.get("autobump").and_then(Value::as_bool).unwrap_or(true),
            skip_livecheck: v
                .get("skip_livecheck")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            deprecate: v
                .get("deprecated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            disable: v.get("disabled").and_then(Value::as_bool).unwrap_or(false),
            caveats: s(v.get("caveats")),
            aliases: str_list(v.get("aliases")),
            oldnames: str_list(v.get("oldnames")),
            head: matches!(v.pointer("/urls/head"), Some(Value::Object(_))),
            head_url: s(v.pointer("/urls/head/url")),
            options: parse_options(v.get("options")),
            deprecate_disable: parse_deprecate_disable(v),
            executables: str_list(v.get("executables")),
        })
    }

    /// The bottle file for this machine, best tag first.
    #[must_use]
    pub fn bottle_for(&self, tags: &[String]) -> Option<&Bottle> {
        tags.iter()
            .map(String::as_str)
            .chain(["all"])
            .find_map(|want| self.bottle_files.iter().find(|b| b.tag == want))
    }

    /// The bottle for `tags` that pours natively, or why it cannot ([`Formula::pour_needs`]).
    pub fn native_bottle(&self, tags: &[String]) -> Result<&Bottle, String> {
        let bottle = self
            .bottle_for(tags)
            .ok_or_else(|| "no bottle for this machine".to_string())?;
        if let Some(needs) = &self.pour_needs {
            return Err(needs.clone());
        }
        let relocatable =
            bottle.cellar == ":any" && cfg!(target_os = "macos") && !bottle.tag.contains("linux");
        if bottle.cellar != ":any_skip_relocation" && !relocatable {
            return Err(format!("its bottle relocates: cellar {}", bottle.cellar));
        }
        Ok(bottle)
    }

    /// Apply the variation for `machine`, mirroring cask selection.
    pub fn resolve_variations(&mut self, machine: &Machine) -> bool {
        if self.variations.is_empty() {
            return true;
        }
        if self
            .variations
            .iter()
            .any(|(key, entry)| key.is_empty() && entry.unsupported)
        {
            return self.resolve_variations_for_keys(&[]);
        }
        let Some(keys) = machine.variation_keys() else {
            self.pour_needs
                .get_or_insert_with(|| "variations for a system abrew cannot name".to_string());
            self.variations.clear();
            return false;
        };
        self.resolve_variations_for_keys(&keys)
    }

    fn resolve_variations_for_keys(&mut self, keys: &[String]) -> bool {
        if self
            .variations
            .iter()
            .any(|(key, entry)| key.is_empty() && entry.unsupported)
        {
            self.pour_needs
                .get_or_insert_with(|| "variations abrew cannot read".to_string());
            self.variations.clear();
            return false;
        }
        let found = keys.iter().find_map(|key| {
            self.variations
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, variation)| variation.clone())
        });
        self.variations.clear();
        let Some(variation) = found else {
            return true;
        };
        if variation.unsupported || variation.requirements_unsupported {
            self.pour_needs
                .get_or_insert_with(|| "a variation abrew cannot resolve".to_string());
            return false;
        }
        if let Some(deps) = variation.dependencies {
            self.dependencies = deps;
        }
        if let Some(reqs) = variation.requirements {
            self.requirements = reqs;
        }
        if let Some(conflicts) = variation.conflicts {
            self.conflicts = conflicts;
        }
        true
    }

    /// Returns `full` without its tap prefix.
    #[must_use]
    pub fn short_name(full: &str) -> &str {
        full.rsplit('/').next().unwrap_or(full)
    }

    /// Whether `request` (bare or tap-qualified) names this formula's
    /// canonical name, one of its aliases, or one of its `oldnames`.
    #[must_use]
    pub fn matches_name(&self, request: &str) -> bool {
        let short = Self::short_name(request);
        self.name == short
            || self.aliases.iter().any(|a| a == short)
            || self.oldnames.iter().any(|o| o == short)
    }
}

/// A cask parsed from formulae.brew.sh JSON.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cask {
    pub token: String,
    pub deprecated: bool,
    pub disabled: bool,
    pub name: String,
    pub desc: String,
    pub homepage: String,
    pub version: String,
    pub url: String,
    /// `None` when the cask says `no_check`.
    pub sha256: Option<String>,
    /// The cask explicitly states `sha256 :no_check` (a tap
    /// cask with no sha256 stanza at all has neither).
    pub sha256_no_check: bool,
    pub artifacts: Vec<Artifact>,
    pub auto_updates: bool,
    /// Declared `language` blocks, in API order (info display).
    pub languages: Vec<String>,
    pub caveats: String,
    pub deprecate_disable: DeprecateDisable,
    /// The first top-level constraint the native resolver cannot evaluate.
    pub unsupported_constraints: Option<&'static str>,
    /// The `depends_on macos: ">= X"` floor, when the API states one in plain numeric form.
    pub macos_min: Option<String>,
    /// The `depends_on macos: "== X"` exact-release bound.
    pub macos_exact: Option<String>,
    /// `conflicts_with` cask/formula names.
    pub conflicts: Vec<String>,
    /// The `depends_on arch:` requirement.
    pub arch: Option<String>,
    /// `depends_on formula:` names, installed before this cask.
    pub depends_formula: Vec<String>,
    /// `depends_on cask:` tokens, installed before this cask.
    pub depends_cask: Vec<String>,
    /// Per-system overrides.
    pub variations: Vec<(String, Variation)>,
    /// File directives of the `zap` stanza (`trash:` paths).
    pub zap_trash: Vec<String>,
    /// File directives of the `zap` stanza (`delete:` paths);
    /// brew runs these under sudo, so their presence delegates.
    pub zap_delete: Vec<String>,
    /// File directives of the `zap` stanza (`rmdir:` paths).
    pub zap_rmdir: Vec<String>,
    /// A zap stanza exists at all (drives brew's "No zap stanza").
    pub zap_present: bool,
    /// The zap stanza uses a directive beyond trash/delete/rmdir.
    pub zap_unsupported: bool,
    /// File directives of the `uninstall` stanza (`trash:` paths).
    pub uninstall_trash: Vec<String>,
    /// File directives of the `uninstall` stanza (`rmdir:` paths).
    pub uninstall_rmdir: Vec<String>,
    /// The uninstall stanza uses a directive beyond trash/rmdir
    /// (quit, launchctl, pkgutil, delete, …): zapping delegates.
    pub uninstall_unsupported: bool,
    /// `container type: :naked`.
    pub naked: bool,
    /// (branch, only_path) when `url_specs` is exactly a git
    /// branch + subdirectory checkout (raw-fetchable file set).
    pub git_branch_path: Option<(String, String)>,
    /// `container nested: "<path>"`.
    pub nested: Option<String>,
    /// Renamed-from tokens (`old_tokens`; `migrate` resolution).
    pub old_tokens: Vec<String>,
    /// The web URL of the cask file for tap-loaded records
    /// (info's `From:` line); API records use the core cask
    /// tap URL instead.
    pub source_url: Option<String>,
    /// Qualified `user/repo/token` for tap-loaded records
    /// (install displays); empty for API records.
    pub full_name: String,
    /// The `livecheck` block parsed as data from a tap file
    /// (see [`Formula::livecheck`]).
    pub livecheck: Option<Box<LivecheckSpec>>,
    /// The cask opts out of autobump (see [`Formula::no_autobump`]).
    pub no_autobump: bool,
    /// See [`Formula::skip_livecheck`].
    pub skip_livecheck: bool,
}

/// One cask `variations` entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Variation {
    /// This entry uses fields/shapes abrew cannot evaluate.
    pub unsupported: bool,
    pub url: Option<String>,
    /// Outer None: field absent.
    pub sha256: Option<Option<String>>,
    pub version: Option<String>,
    pub artifacts: Option<Vec<Artifact>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Artifact {
    /// Source path as brew spells it (payload-relative, or where brew
    /// installed it: `cask::payload_source`) and the installed name the
    /// cask's `target:` rename asks for, when it names a plain file name.
    App(String, Option<String>),
    Binary(String, Option<String>),
    Font(String, Option<String>),
    Pkg(String),
    Manpage(String, Option<String>),
    Suite(String, Option<String>),
    KeyboardLayout(String, Option<String>),
    ScreenSaver(String, Option<String>),
    PrefPane(String, Option<String>),
    Service(String, Option<String>),
    /// Generic `artifact` with its full stated target.
    Generic(String, Option<String>),
    /// `command_wrapper`: wrapper name, the program it runs (a payload source
    /// or a bare command name), fixed leading args, installed-name rename.
    Wrapper(String, String, Vec<String>, Option<String>),
    /// A plugin/completion artifact linked into a fixed prefix-relative directory.
    Library(String, Option<String>, String),
    Other(String),
}

/// Brew's `Requirement#display_s` for one parsed formula
/// requirement (what `deps --include-requirements` prints after
/// the colon).
#[must_use]
pub fn requirement_display(req: &Requirement) -> String {
    match req {
        Requirement::MacosMin(Some(v)) => format!("macOS >= {v}"),
        Requirement::MacosMin(None) => "macOS".to_string(),
        Requirement::MacosMax(v) => format!("macOS <= {v}"),
        Requirement::Arch(a) => format!("{a} architecture"),
        Requirement::XcodeMin(Some(v)) => format!("Xcode >= {v} (on macOS)"),
        Requirement::XcodeMin(None) => "Xcode (on macOS)".to_string(),
    }
}

/// Ruby's `String#capitalize`: first character up, the rest down.
#[must_use]
pub fn ruby_capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
    }
}

fn macos_symbol_number(sym: &str) -> Option<&'static str> {
    Some(match sym {
        "tahoe" => "26",
        "sequoia" => "15",
        "sonoma" => "14",
        "ventura" => "13",
        "monterey" => "12",
        "big_sur" => "11",
        "catalina" => "10.15",
        "mojave" => "10.14",
        "high_sierra" => "10.13",
        "sierra" => "10.12",
        "el_capitan" => "10.11",
        _ => return None,
    })
}

/// Requirements of a cask as brew's `CaskDependent#requirements`
/// renders them (arch entries, cask tokens capitalized, macOS
/// bounds), read loosely from the raw API `depends_on` — display
/// must work even for casks the installer cannot evaluate.
#[must_use]
pub fn cask_requirement_displays(item: &Value) -> Vec<String> {
    cask_requirement_displays_impl(item, true)
}

/// The `info` Requirements variant: brew excludes the
/// cask-token requirements (`CaskDependent::Requirement`) there.
#[must_use]
pub fn cask_info_requirement_displays(item: &Value) -> Vec<String> {
    cask_requirement_displays_impl(item, false)
}

fn cask_requirement_displays_impl(item: &Value, include_cask_tokens: bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let Some(dep) = item.get("depends_on").and_then(Value::as_object) else {
        return out;
    };
    if let Some(arches) = dep.get("arch").and_then(Value::as_array) {
        for entry in arches {
            let ty = entry.get("type").and_then(Value::as_str).unwrap_or("");
            let bits = entry.get("bits").and_then(Value::as_u64).unwrap_or(0);
            let name = if bits == 64 {
                if ty == "intel" {
                    "x86_64".to_string()
                } else {
                    format!("{ty}64")
                }
            } else if ty == "intel" && bits == 32 {
                "i386".to_string()
            } else {
                ty.to_string()
            };
            out.push(format!("{name} architecture"));
        }
    }
    if include_cask_tokens && let Some(casks) = dep.get("cask").and_then(Value::as_array) {
        for token in casks.iter().filter_map(Value::as_str) {
            out.push(ruby_capitalize(token));
        }
    }
    let mut macos = |value: Option<&Value>| {
        let Some(value) = value else { return };
        match value {
            Value::Object(bounds) => {
                if bounds.is_empty() {
                    out.push("macOS".to_string());
                }
                for (cmp, versions) in bounds {
                    let list: Vec<String> = versions
                        .as_array()
                        .map(|vs| {
                            vs.iter()
                                .filter_map(Value::as_str)
                                .map(|v| {
                                    macos_symbol_number(v)
                                        .map_or_else(|| v.to_string(), str::to_string)
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    if !list.is_empty() {
                        out.push(format!("macOS {cmp} {}", list.join(" / ")));
                    }
                }
            }
            Value::Array(items) => {
                // Legacy shape: [">= :sonoma"].
                for item in items.iter().filter_map(Value::as_str) {
                    let item = item.trim();
                    if let Some((cmp, sym)) = item.split_once(' ') {
                        let sym = sym.trim_start_matches(':');
                        let num = macos_symbol_number(sym).unwrap_or(sym);
                        out.push(format!("macOS {cmp} {num}"));
                    } else if item == ":macos" || item == "macos" {
                        out.push("macOS".to_string());
                    }
                }
            }
            _ => {}
        }
    };
    macos(dep.get("macos"));
    macos(dep.get("maximum_macos"));
    out
}

/// One cask `depends_on` in the plain data forms abrew evaluates.
#[derive(Debug, Default)]
struct Depends {
    floor: Option<String>,
    exact: Option<String>,
    arch: Option<String>,
    formula: Vec<String>,
    cask: Vec<String>,
}

fn parse_depends(v: Option<&Value>) -> Option<Depends> {
    let Value::Object(map) = v? else {
        return None;
    };
    let mut out = Depends::default();
    for (key, value) in map {
        match key.as_str() {
            "macos" => {
                let Value::Object(macos) = value else {
                    return None;
                };
                if macos.is_empty() {
                    continue;
                }
                if macos.len() != 1 {
                    return None;
                }
                let (op, bounds) = match (macos.get(">="), macos.get("==")) {
                    (Some(Value::Array(bounds)), None) => (">=", bounds),
                    (None, Some(Value::Array(bounds))) => ("==", bounds),
                    _ => return None,
                };
                let [Value::String(bound)] = bounds.as_slice() else {
                    return None;
                };
                if !numeric_macos_version(bound) {
                    return None;
                }
                if op == ">=" {
                    out.floor = Some(bound.clone());
                } else {
                    out.exact = Some(bound.clone());
                }
            }
            "arch" => {
                let Value::Array(items) = value else {
                    return None;
                };
                let [Value::Object(spec)] = items.as_slice() else {
                    return None;
                };
                if spec.len() != 2 {
                    return None;
                }
                let ty = spec.get("type").and_then(Value::as_str)?;
                let bits = spec.get("bits").and_then(Value::as_u64)?;
                out.arch = Some(
                    match (ty, bits) {
                        ("arm", 64) => "arm64",
                        ("intel", 64) => "x86_64",
                        _ => return None,
                    }
                    .to_string(),
                );
            }
            "formula" | "cask" => {
                let Value::Array(items) = value else {
                    return None;
                };
                let names = if key == "formula" {
                    &mut out.formula
                } else {
                    &mut out.cask
                };
                for item in items {
                    let Value::String(name) = item else {
                        return None;
                    };
                    if !super::valid_component(name) {
                        return None;
                    }
                    names.push(name.clone());
                }
            }
            _ => return None,
        }
    }
    Some(out)
}

fn parse_conflicts(v: Option<&Value>) -> Option<Vec<String>> {
    let Value::Object(map) = v? else {
        return None;
    };
    let mut names = Vec::new();
    for (kind, vals) in map {
        if !matches!(kind.as_str(), "cask" | "formula") {
            return None;
        }
        let Value::Array(items) = vals else {
            return None;
        };
        for item in items {
            let Value::String(name) = item else {
                return None;
            };
            if !super::valid_name(name) {
                return None;
            }
            names.push(name.clone());
        }
    }
    Some(names)
}

/// The file directives of a cask's `zap` and `uninstall` stanzas,
/// plus whether either uses a directive abrew does not run.
#[derive(Debug, Default)]
pub struct StanzaFiles {
    pub zap_trash: Vec<String>,
    pub zap_delete: Vec<String>,
    pub zap_rmdir: Vec<String>,
    pub zap_present: bool,
    pub zap_unsupported: bool,
    pub uninstall_trash: Vec<String>,
    pub uninstall_rmdir: Vec<String>,
    pub uninstall_unsupported: bool,
}

fn stanza_paths(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => out.push(s.clone()),
        Value::Array(items) => {
            for item in items {
                stanza_paths(item, out);
            }
        }
        _ => {}
    }
}

fn parse_uninstall_stanzas(v: Option<&Value>) -> StanzaFiles {
    let mut out = StanzaFiles::default();
    let Some(arr) = v.and_then(Value::as_array) else {
        return out;
    };
    for item in arr {
        let Value::Object(map) = item else { continue };
        for (stanza, entries) in map {
            let is_zap = match stanza.as_str() {
                "zap" => true,
                "uninstall" => false,
                _ => continue,
            };
            if is_zap {
                out.zap_present = true;
            }
            let Value::Array(entries) = entries else {
                continue;
            };
            for entry in entries {
                let Value::Object(entry) = entry else {
                    continue;
                };
                for (directive, value) in entry {
                    match (is_zap, directive.as_str()) {
                        (true, "trash") => stanza_paths(value, &mut out.zap_trash),
                        (true, "delete") => stanza_paths(value, &mut out.zap_delete),
                        (true, "rmdir") => stanza_paths(value, &mut out.zap_rmdir),
                        (true, _) => out.zap_unsupported = true,
                        (false, "trash") => stanza_paths(value, &mut out.uninstall_trash),
                        (false, "rmdir") => stanza_paths(value, &mut out.uninstall_rmdir),
                        (false, _) => out.uninstall_unsupported = true,
                    }
                }
            }
        }
    }
    out
}

fn parse_artifacts(v: Option<&Value>) -> Vec<Artifact> {
    let mut artifacts = Vec::new();
    if let Some(arr) = v.and_then(Value::as_array) {
        for a in arr {
            if !matches!(a, Value::Object(_))
                || a.get("target")
                    .is_some_and(|value| !matches!(value, Value::String(_)))
            {
                artifacts.push(Artifact::Other(
                    "unsupported artifact row or target".to_string(),
                ));
                continue;
            }
            let raw_target = a.get("target").and_then(Value::as_str).map(str::to_owned);
            let dest_name = a.get("target").and_then(Value::as_str).map(|t| {
                std::path::Path::new(t)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
            });
            if let Value::Object(map) = a {
                for (kind, vals) in map {
                    if matches!(kind.as_str(), "target" | "uninstall" | "zap") {
                        continue;
                    }
                    if kind == "command_wrapper" {
                        let parsed = (|| {
                            let items = vals.as_array()?;
                            let [Value::String(name), Value::Object(opts)] = items.as_slice()
                            else {
                                return None;
                            };
                            if !opts
                                .keys()
                                .all(|k| matches!(k.as_str(), "executable" | "args"))
                            {
                                return None;
                            }
                            if let Some(Value::Array(items)) = opts.get("args") {
                                for item in items {
                                    let arg = item.as_str()?;
                                    let scrubbed = arg
                                        .replace("$HOMEBREW_PREFIX/", "")
                                        .replace("$APPDIR/", "");
                                    if scrubbed.contains('$') {
                                        return None;
                                    }
                                }
                            }
                            let exe =
                                appdir_source(opts.get("executable").and_then(Value::as_str)?);
                            let args = match opts.get("args") {
                                None => Vec::new(),
                                Some(Value::Array(items)) => {
                                    let mut out = Vec::new();
                                    for item in items {
                                        out.push(item.as_str()?.to_string());
                                    }
                                    out
                                }
                                Some(_) => return None,
                            };
                            let dest = match &dest_name {
                                Some(Some(name))
                                    if !name.is_empty()
                                        && !name.contains('/')
                                        && !name.contains('\\') =>
                                {
                                    Some(name.clone())
                                }
                                Some(_) => return None,
                                None => None,
                            };
                            Some(Artifact::Wrapper(name.clone(), exe, args, dest))
                        })();
                        artifacts.push(parsed.unwrap_or_else(|| {
                            Artifact::Other(
                                "command_wrapper: unsupported artifact options".to_string(),
                            )
                        }));
                        continue;
                    }
                    let items = vals.as_array();
                    let simple = items
                        .and_then(|items| items.first())
                        .and_then(Value::as_str);
                    let inline_target = items
                        .and_then(|items| items.get(1))
                        .and_then(|options| options.get("target"))
                        .and_then(Value::as_str);
                    if kind == "artifact"
                        && raw_target.as_deref().zip(inline_target).is_some_and(|(sibling, inline)| {
                            sibling != inline
                                && !matches!((generic_target_relative(sibling), generic_target_relative(inline)),
                                    (Some(a), Some(b)) if a == b)
                        })
                    {
                        artifacts.push(Artifact::Other("artifact: conflicting full targets".to_string()));
                        continue;
                    }
                    let raw_target = raw_target.clone().or_else(|| {
                        items
                            .and_then(|items| items.get(1))
                            .and_then(|opts| opts.get("target"))
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    });
                    let inline_dest = match items.map(Vec::as_slice) {
                        Some([_]) => None,
                        Some([_, Value::Object(opts)]) if opts.len() == 1 => {
                            Some(opts.get("target").and_then(Value::as_str).and_then(|t| {
                                std::path::Path::new(t)
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                            }))
                        }
                        _ => Some(None),
                    };
                    let (Some(first), inline_dest) = (simple, inline_dest) else {
                        artifacts.push(Artifact::Other(format!(
                            "{kind}: unsupported artifact options"
                        )));
                        continue;
                    };
                    let first = appdir_source(first);
                    if matches!(inline_dest, Some(None))
                        || matches!((&dest_name, &inline_dest), (Some(Some(a)), Some(Some(b))) if a != b)
                    {
                        artifacts.push(Artifact::Other(format!(
                            "{kind}: unsupported artifact options"
                        )));
                        continue;
                    }
                    let dest = match dest_name.clone().or(inline_dest) {
                        Some(Some(name))
                            if !name.is_empty() && !name.contains('/') && !name.contains('\\') =>
                        {
                            Some(name)
                        }
                        Some(_) => {
                            artifacts
                                .push(Artifact::Other(format!("{kind}: unusable target rename")));
                            continue;
                        }
                        None => None,
                    };
                    artifacts.push(match kind.as_str() {
                        "app" => Artifact::App(first, dest),
                        "binary" => Artifact::Binary(first, dest),
                        "font" => Artifact::Font(first, dest),
                        "pkg" => Artifact::Pkg(first),
                        "manpage" => Artifact::Manpage(first, dest),
                        "suite" => Artifact::Suite(first, dest),
                        "keyboard_layout" => Artifact::KeyboardLayout(first, dest),
                        "screen_saver" => Artifact::ScreenSaver(first, dest),
                        "prefpane" => Artifact::PrefPane(first, dest),
                        "service" => Artifact::Service(first, dest),
                        "artifact" => Artifact::Generic(first, raw_target.clone()),
                        "qlplugin" => Artifact::Library(first, dest, "Library/QuickLook".into()),
                        "colorpicker" => {
                            Artifact::Library(first, dest, "Library/ColorPickers".into())
                        }
                        "dictionary" => {
                            Artifact::Library(first, dest, "Library/Dictionaries".into())
                        }
                        "input_method" => {
                            Artifact::Library(first, dest, "Library/Input Methods".into())
                        }
                        "internet_plugin" => {
                            Artifact::Library(first, dest, "Library/Internet Plug-Ins".into())
                        }
                        "mdimporter" => Artifact::Library(first, dest, "Library/Spotlight".into()),
                        "audio_unit_plugin" => Artifact::Library(
                            first,
                            dest,
                            "Library/Audio/Plug-Ins/Components".into(),
                        ),
                        "vst_plugin" => {
                            Artifact::Library(first, dest, "Library/Audio/Plug-Ins/VST".into())
                        }
                        "vst3_plugin" => {
                            Artifact::Library(first, dest, "Library/Audio/Plug-Ins/VST3".into())
                        }
                        "bash_completion" => Artifact::Library(
                            first,
                            dest,
                            "share/bash-completion/completions".into(),
                        ),
                        "zsh_completion" => {
                            Artifact::Library(first, dest, "share/zsh/site-functions".into())
                        }
                        "fish_completion" => {
                            Artifact::Library(first, dest, "share/fish/vendor_completions.d".into())
                        }
                        other => Artifact::Other(format!("{other}:{first}")),
                    });
                }
            }
        }
    }
    artifacts
}

/// The admitted `variations` shape.
fn parse_variations(v: Option<&Value>) -> Option<Vec<(String, Variation)>> {
    let Value::Object(map) = v? else {
        return None;
    };
    let mut out = Vec::new();
    for (key, entry) in map {
        let mut variation = Variation::default();
        let Value::Object(fields) = entry else {
            variation.unsupported = true;
            out.push((key.clone(), variation));
            continue;
        };
        for (field, value) in fields {
            match field.as_str() {
                "url" => match value.as_str() {
                    Some(u) => variation.url = Some(u.to_string()),
                    None => variation.unsupported = true,
                },
                "version" => match value.as_str() {
                    Some(v) => variation.version = Some(v.to_string()),
                    None => variation.unsupported = true,
                },
                "sha256" => match value.as_str() {
                    Some("no_check") => variation.sha256 = Some(None),
                    Some(raw) if raw.len() == 64 && raw.bytes().all(|b| b.is_ascii_hexdigit()) => {
                        variation.sha256 = Some(Some(raw.to_string()));
                    }
                    _ => variation.unsupported = true,
                },
                "artifacts" => variation.artifacts = Some(parse_artifacts(Some(value))),
                "skip_livecheck" => {}
                _ => variation.unsupported = true,
            }
        }
        out.push((key.clone(), variation));
    }
    Some(out)
}

impl Cask {
    /// The variation Homebrew would pick for `machine`.
    #[must_use]
    pub fn variation_for(&self, machine: &Machine) -> Option<&Variation> {
        machine.variation_keys()?.iter().find_map(|key| {
            self.variations
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, variation)| variation)
        })
    }

    /// Parse one formulae.brew.sh cask object.
    pub fn from_api(v: &Value) -> Result<Cask, String> {
        let token = s(v.get("token"));
        if token.is_empty() {
            return Err("abrew: cask JSON has no token".to_string());
        }
        let sha_raw = s(v.get("sha256"));
        let mut artifacts = parse_artifacts(v.get("artifacts"));
        let stanza_files = parse_uninstall_stanzas(v.get("artifacts"));
        let naked = matches!(v.get("container"), Some(Value::Object(fields))
            if fields.len() == 1 && fields.get("type").and_then(Value::as_str) == Some("naked"));
        // A git checkout limited to one branch and subdirectory,
        // the Google-Fonts shape. Anything richer (tag, revision,
        // a download strategy field) leaves the field unset and
        // the cask delegated as before.
        let git_branch_path = match v.get("url_specs") {
            Some(Value::Object(fields))
                if fields
                    .keys()
                    .all(|k| matches!(k.as_str(), "branch" | "only_path" | "verified")) =>
            {
                match (
                    fields.get("branch").and_then(Value::as_str),
                    fields.get("only_path").and_then(Value::as_str),
                ) {
                    (Some(branch), Some(path))
                        if super::single_normal_segment(branch)
                            && !path.is_empty()
                            && path.split('/').all(super::single_normal_segment) =>
                    {
                        Some((branch.to_string(), path.to_string()))
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        let dmg = matches!(v.get("container"), Some(Value::Object(fields))
            if fields.len() == 1 && fields.get("type").and_then(Value::as_str) == Some("dmg"));
        let nested = match v.get("container") {
            Some(Value::Object(fields)) if fields.len() == 1 => fields
                .get("nested")
                .and_then(Value::as_str)
                .filter(|p| {
                    !p.is_empty()
                        && std::path::Path::new(p)
                            .components()
                            .all(|c| matches!(c, std::path::Component::Normal(_)))
                        && super::fetch::archive_supported(p)
                })
                .map(str::to_owned),
            _ => None,
        };
        let mut unsupported_constraints = None;
        if !empty_metadata(v.get("container")) && !naked && !dmg && nested.is_none() {
            unsupported_constraints = unsupported_constraints.or(Some("container"));
            artifacts.push(Artifact::Other("unsupported container".to_string()));
        }
        let mut macos_min = None;
        let mut macos_exact = None;
        let mut arch = None;
        let mut depends_formula = Vec::new();
        let mut depends_cask = Vec::new();
        if !empty_metadata(v.get("depends_on")) {
            match parse_depends(v.get("depends_on")) {
                Some(deps) => {
                    macos_min = deps.floor;
                    macos_exact = deps.exact;
                    arch = deps.arch;
                    depends_formula = deps.formula;
                    depends_cask = deps.cask;
                }
                None => {
                    unsupported_constraints = unsupported_constraints.or(Some("depends_on"));
                    artifacts.push(Artifact::Other("unsupported depends_on".to_string()));
                }
            }
        }
        let mut variations = Vec::new();
        if !empty_metadata(v.get("variations")) {
            match parse_variations(v.get("variations")) {
                Some(parsed) => variations = parsed,
                None => {
                    unsupported_constraints = unsupported_constraints.or(Some("variations"));
                    artifacts.push(Artifact::Other("unsupported variations".to_string()));
                }
            }
        }
        let mut conflicts = Vec::new();
        if !empty_metadata(v.get("conflicts_with")) {
            match parse_conflicts(v.get("conflicts_with")) {
                Some(names) => conflicts = names,
                None => {
                    unsupported_constraints = unsupported_constraints.or(Some("conflicts_with"));
                    artifacts.push(Artifact::Other("unsupported conflicts_with".to_string()));
                }
            }
        }
        let no_check = sha_raw == "no_check";
        let sha256 = if sha_raw == "no_check" {
            None
        } else if sha_raw.len() == 64 && sha_raw.bytes().all(|b| b.is_ascii_hexdigit()) {
            Some(sha_raw)
        } else {
            return Err(format!(
                "abrew: cask {token} has no usable sha256 (missing or malformed)"
            ));
        };
        let names = str_list(v.get("name"));
        Ok(Cask {
            deprecated: v
                .get("deprecated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            disabled: v.get("disabled").and_then(Value::as_bool).unwrap_or(false),
            token,
            name: names.into_iter().next().unwrap_or_default(),
            desc: s(v.get("desc")),
            homepage: s(v.get("homepage")),
            version: s(v.get("version")),
            url: s(v.get("url")),
            sha256_no_check: no_check,
            sha256,
            artifacts,
            auto_updates: v
                .get("auto_updates")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            languages: str_list(v.get("languages")),
            caveats: s(v.get("caveats")),
            deprecate_disable: parse_deprecate_disable(v),
            unsupported_constraints,
            macos_min,
            macos_exact,
            conflicts,
            arch,
            depends_formula,
            depends_cask,
            variations,
            zap_trash: stanza_files.zap_trash,
            zap_delete: stanza_files.zap_delete,
            zap_rmdir: stanza_files.zap_rmdir,
            zap_present: stanza_files.zap_present,
            zap_unsupported: stanza_files.zap_unsupported,
            uninstall_trash: stanza_files.uninstall_trash,
            uninstall_rmdir: stanza_files.uninstall_rmdir,
            uninstall_unsupported: stanza_files.uninstall_unsupported,
            naked,
            git_branch_path,
            nested,
            old_tokens: str_list(v.get("old_tokens")),
            source_url: None,
            full_name: String::new(),
            livecheck: None,
            no_autobump: !v.get("autobump").and_then(Value::as_bool).unwrap_or(true),
            skip_livecheck: v
                .get("skip_livecheck")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }
}

/// Homebrew-shaped version comparison (a faithful
/// port of brew's `Version` token classes and
/// comparison loop, with the `PkgVersion` revision
/// suffix compared last).
#[must_use]
pub fn version_cmp(a: &str, b: &str) -> Ordering {
    let (a_main, a_rev) = split_revision(a);
    let (b_main, b_rev) = split_revision(b);
    if a_main == b_main {
        return a_rev.cmp(&b_rev);
    }
    let head = |v: &str| v == "HEAD" || v.starts_with("HEAD-");
    match (head(&a_main), head(&b_main)) {
        (true, false) => return Ordering::Greater,
        (false, true) => return Ordering::Less,
        (true, true) => return a_rev.cmp(&b_rev),
        (false, false) => {}
    }
    let lt = brew_tokens(&a_main);
    let rt = brew_tokens(&b_main);
    let max = lt.len().max(rt.len());
    let (mut l, mut r) = (0usize, 0usize);
    while l < max {
        let a_tok = lt.get(l).unwrap_or(&BrewTok::Null);
        let b_tok = rt.get(r).unwrap_or(&BrewTok::Null);
        if brew_tok_cmp(a_tok, b_tok) == Ordering::Equal {
            l += 1;
            r += 1;
            continue;
        }
        let (a_num, b_num) = (
            matches!(a_tok, BrewTok::Num(_)),
            matches!(b_tok, BrewTok::Num(_)),
        );
        if a_num && !b_num {
            if brew_tok_cmp(a_tok, &BrewTok::Null) == Ordering::Greater {
                return Ordering::Greater;
            }
            l += 1;
        } else if !a_num && b_num {
            if brew_tok_cmp(b_tok, &BrewTok::Null) == Ordering::Greater {
                return Ordering::Less;
            }
            r += 1;
        } else {
            return brew_tok_cmp(a_tok, b_tok);
        }
    }
    a_rev.cmp(&b_rev)
}

/// A `pkg_version` split into its version and revision parts
/// (`1.2.3_1` -> (`1.2.3`, 1); no numeric `_N` suffix -> 0).
pub(super) fn split_revision(v: &str) -> (String, u64) {
    if let Some((head, tail)) = v.rsplit_once('_')
        && let Ok(n) = tail.parse::<u64>()
        && tail.chars().all(|c| c.is_ascii_digit())
        && !head.is_empty()
    {
        return (head.to_string(), n);
    }
    (v.to_string(), 0)
}

/// One brew `Version::Token`: composite tokens keep
/// their raw text (for the lexical `super` compare)
/// and their first digit run as the revision.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BrewTok {
    Null,
    Num(u128),
    Str(String),
    Alpha(String, u128),
    Beta(String, u128),
    Pre(String, u128),
    Rc(String, u128),
    Patch(String, u128),
    Post(String, u128),
}

impl BrewTok {
    fn raw(&self) -> &str {
        match self {
            BrewTok::Null => "",
            BrewTok::Num(_) => "",
            BrewTok::Str(s)
            | BrewTok::Alpha(s, _)
            | BrewTok::Beta(s, _)
            | BrewTok::Pre(s, _)
            | BrewTok::Rc(s, _)
            | BrewTok::Patch(s, _)
            | BrewTok::Post(s, _) => s,
        }
    }

    fn is_composite(&self) -> bool {
        matches!(
            self,
            BrewTok::Alpha(..)
                | BrewTok::Beta(..)
                | BrewTok::Pre(..)
                | BrewTok::Rc(..)
                | BrewTok::Patch(..)
                | BrewTok::Post(..)
        )
    }

    /// Position in brew's alpha < beta < pre < rc <
    /// patch/post ordering (`None` for non-composites).
    fn class_rank(&self) -> Option<u8> {
        match self {
            BrewTok::Alpha(..) => Some(0),
            BrewTok::Beta(..) => Some(1),
            BrewTok::Pre(..) => Some(2),
            BrewTok::Rc(..) => Some(3),
            BrewTok::Patch(..) | BrewTok::Post(..) => Some(4),
            _ => None,
        }
    }
}

/// Case-insensitive ASCII prefix match at `pos`,
/// returning the end position after an optional
/// digit run (`Some` also requires the run when
/// `need_digits`).
fn brew_word(bytes: &[u8], pos: usize, word: &str, need_digits: bool) -> Option<usize> {
    let w = word.as_bytes();
    if bytes.len() < pos + w.len()
        || !bytes[pos..pos + w.len()]
            .iter()
            .zip(w)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
    {
        return None;
    }
    let mut end = pos + w.len();
    let start = end;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    if need_digits && end == start {
        return None;
    }
    Some(end)
}

/// The length of the brew `SCAN_PATTERN` match at
/// `pos` (alternatives tried in union order).
fn brew_scan_len(bytes: &[u8], pos: usize) -> Option<usize> {
    for (word, need) in [
        ("alpha", false),
        ("a", true),
        ("beta", false),
        ("b", true),
        ("pre", false),
        ("rc", false),
        ("p", false),
    ] {
        if let Some(e) = brew_word(bytes, pos, word, need) {
            return Some(e - pos);
        }
    }
    // PostToken: any one char + "post" + digits.
    if pos < bytes.len()
        && let Some(e) = brew_word(bytes, pos + 1, "post", true)
    {
        return Some(e - pos);
    }
    let c = *bytes.get(pos)?;
    if c.is_ascii_digit() || c.is_ascii_alphabetic() {
        let pred: fn(&u8) -> bool = if c.is_ascii_digit() {
            u8::is_ascii_digit
        } else {
            u8::is_ascii_alphabetic
        };
        let mut end = pos + 1;
        while end < bytes.len() && pred(&bytes[end]) {
            end += 1;
        }
        return Some(end - pos);
    }
    None
}

/// Whole-string classification (`Token.create` order).
fn brew_classify(s: &str) -> BrewTok {
    let b = s.as_bytes();
    let rev = {
        let digits: String = s
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(|c| c.is_ascii_digit())
            .collect();
        digits.parse::<u128>().unwrap_or(0)
    };
    let full = |word: &str, need: bool| brew_word(b, 0, word, need) == Some(b.len());
    if full("alpha", false) || full("a", true) {
        BrewTok::Alpha(s.to_string(), rev)
    } else if full("beta", false) || full("b", true) {
        BrewTok::Beta(s.to_string(), rev)
    } else if full("rc", false) {
        BrewTok::Rc(s.to_string(), rev)
    } else if full("pre", false) {
        BrewTok::Pre(s.to_string(), rev)
    } else if full("p", false) {
        BrewTok::Patch(s.to_string(), rev)
    } else if brew_word(b, 1, "post", true) == Some(b.len()) && !b.is_empty() {
        BrewTok::Post(s.to_string(), rev)
    } else if !b.is_empty() && b.iter().all(u8::is_ascii_digit) {
        BrewTok::Num(s.parse().unwrap_or(u128::MAX))
    } else {
        BrewTok::Str(s.to_string())
    }
}

fn brew_tokens(v: &str) -> Vec<BrewTok> {
    let bytes = v.as_bytes();
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < bytes.len() {
        match brew_scan_len(bytes, pos) {
            Some(len) => {
                out.push(brew_classify(&v[pos..pos + len]));
                pos += len;
            }
            None => pos += 1,
        }
    }
    out
}

/// Brew's `Token#<=>` over token classes.
fn brew_tok_cmp(x: &BrewTok, y: &BrewTok) -> Ordering {
    use BrewTok as T;
    match (x, y) {
        (T::Null, T::Null) => Ordering::Equal,
        (T::Null, T::Num(v)) => {
            if *v == 0 {
                Ordering::Equal
            } else {
                Ordering::Less
            }
        }
        (T::Null, T::Alpha(..) | T::Beta(..) | T::Pre(..) | T::Rc(..)) => Ordering::Greater,
        (T::Null, _) => Ordering::Less,
        (T::Num(v), T::Null) => {
            if *v == 0 {
                Ordering::Equal
            } else {
                Ordering::Greater
            }
        }
        (_, T::Null) => brew_tok_cmp(y, x).reverse(),
        (T::Num(a), T::Num(b)) => a.cmp(b),
        (T::Num(_), _) => Ordering::Greater,
        (_, T::Num(_)) => Ordering::Less,
        _ => {
            // StringToken family vs string token:
            // same composite class compares revisions,
            // cross-class by brew's ordering (patch vs
            // post falls back to lexical, like `super`).
            let same_class = std::mem::discriminant(x) == std::mem::discriminant(y);
            if same_class && x.is_composite() {
                let rev = |t: &BrewTok| match t {
                    T::Alpha(_, r)
                    | T::Beta(_, r)
                    | T::Pre(_, r)
                    | T::Rc(_, r)
                    | T::Patch(_, r)
                    | T::Post(_, r) => *r,
                    _ => 0,
                };
                return rev(x).cmp(&rev(y));
            }
            let patch_post = matches!(
                (x, y),
                (T::Patch(..), T::Post(..)) | (T::Post(..), T::Patch(..))
            );
            if !patch_post && let (Some(a), Some(b)) = (x.class_rank(), y.class_rank()) {
                return a.cmp(&b);
            }
            x.raw().cmp(y.raw())
        }
    }
}

#[cfg(test)]
#[path = "formula_tests.rs"]
mod tests;
