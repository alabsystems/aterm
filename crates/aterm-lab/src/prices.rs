// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `prices [<claude-bin>] [--codex <codex-bin>]`.

use std::collections::{BTreeMap, BTreeSet};

use aterm_agent::harness::spend::{ModelPrice, PRICES_TOML, PriceTable, Rates};

use crate::{Fail, Res};

/// Two prices further apart than this fraction of the larger differ.
const GAP: f64 = 0.02;

/// The classes compared: the table's word, and the catalog's.
const CLASSES: [(&str, &str); 5] = [
    ("input", "input"),
    ("output", "output"),
    ("cache_write", "cache_write_5m"),
    ("cache_write_1h", "cache_write_1h"),
    ("cache_read", "cache_read"),
];

/// How deep the catalog's literal may nest before the read stops.
const MAX_DEPTH: usize = 64;

/// The bytes that open the catalog's tiers.
const MARK: &[u8] = b"pricing_tiers:{";

/// and its long-prompt tier: the threshold and the same classes.
#[derive(Debug, Clone, PartialEq, Default)]
struct Side {
    rates: [Option<f64>; 5],
    long: Option<(Option<f64>, [Option<f64>; 5])>,
}

fn classes_of(r: &Rates) -> [Option<f64>; 5] {
    [
        Some(r.input),
        Some(r.output),
        r.cache_write,
        r.cache_write_1h,
        r.cache_read,
    ]
}

fn table_side(p: &ModelPrice) -> Side {
    Side {
        rates: classes_of(&p.rates),
        long: p.long.map(|(over, r)| (Some(over as f64), classes_of(&r))),
    }
}

/// A value of the catalog's literal.
#[derive(Debug, Clone, PartialEq)]
enum Lit {
    Obj(BTreeMap<String, Lit>),
    List(Vec<Lit>),
    Str(String),
    Num(f64),
    Bool(bool),
    Null,
}

/// are read.
fn latin1(b: &[u8]) -> String {
    b.iter().copied().map(char::from).collect()
}

/// The length of a bare key (`[A-Za-z_$][A-Za-z0-9_$]*`) at the start of `b`.
fn ident(b: &[u8]) -> Option<usize> {
    let word = |c: &u8| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'$';
    let first = b.first()?;
    if first.is_ascii_digit() || !word(first) {
        return None;
    }
    Some(b.iter().take_while(|c| word(c)).count())
}

/// start of `b`.
fn number(b: &[u8]) -> Option<usize> {
    let digits = |from: usize| {
        b[from.min(b.len())..]
            .iter()
            .take_while(|c| c.is_ascii_digit())
            .count()
    };
    let mut i = usize::from(b.first() == Some(&b'-'));
    let whole = digits(i);
    i += whole;
    if b.get(i) == Some(&b'.') {
        let frac = digits(i + 1);
        if whole == 0 && frac == 0 {
            return None;
        }
        i += 1 + frac;
    } else if whole == 0 {
        return None;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        let sign = usize::from(matches!(b.get(i + 1), Some(b'+' | b'-')));
        let exp = digits(i + 1 + sign);
        if exp > 0 {
            i += 1 + sign + exp;
        }
    }
    Some(i)
}

/// A reader over the build's bytes, at an offset.
struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl Reader<'_> {
    fn fail(&self, what: &str) -> String {
        let at = self.i.min(self.b.len());
        let end = (at + 40).min(self.b.len());
        format!(
            "catalog: {what} at byte {at} ({:?})",
            latin1(&self.b[at..end])
        )
    }

    fn rest(&self) -> &[u8] {
        &self.b[self.i.min(self.b.len())..]
    }

    fn ws(&mut self) {
        while matches!(self.b.get(self.i), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.i += 1;
        }
    }

    /// Skip white space, then `s` if it is next.
    fn eat(&mut self, s: &[u8]) -> bool {
        self.ws();
        let next = self.rest().starts_with(s);
        if next {
            self.i += s.len();
        }
        next
    }

    /// The string whose quote is at the offset.
    fn string(&mut self) -> Result<String, String> {
        let quote = self.b[self.i];
        let mut j = self.i + 1;
        let mut out = String::new();
        loop {
            match self.b.get(j) {
                None => return Err(self.fail("an unterminated string")),
                Some(&c) if c == quote => break,
                Some(b'\\') => match self.b.get(j + 1) {
                    Some(b'u') => {
                        let code = self
                            .b
                            .get(j + 2..j + 6)
                            .and_then(|h| std::str::from_utf8(h).ok())
                            .and_then(|h| u32::from_str_radix(h, 16).ok())
                            .ok_or_else(|| self.fail("a bad \\u escape"))?;
                        out.push(char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER));
                        j += 6;
                    }
                    Some(&n) => {
                        out.push(match n {
                            b'n' => '\n',
                            b't' => '\t',
                            b'r' => '\r',
                            n => char::from(n),
                        });
                        j += 2;
                    }
                    None => return Err(self.fail("an unterminated string")),
                },
                Some(&c) => {
                    out.push(char::from(c));
                    j += 1;
                }
            }
        }
        self.i = j + 1;
        Ok(out)
    }

    fn key(&mut self) -> Result<String, String> {
        self.ws();
        if matches!(self.b.get(self.i), Some(b'"' | b'\'')) {
            return self.string();
        }
        let n = ident(self.rest())
            .or_else(|| number(self.rest()))
            .ok_or_else(|| self.fail("not a key"))?;
        let key = latin1(&self.rest()[..n]);
        self.i += n;
        Ok(key)
    }

    fn value(&mut self, depth: usize) -> Result<Lit, String> {
        if depth > MAX_DEPTH {
            return Err(self.fail("a literal nested too deep"));
        }
        self.ws();
        match self.b.get(self.i) {
            Some(b'{') => {
                self.i += 1;
                let mut obj = BTreeMap::new();
                if self.eat(b"}") {
                    return Ok(Lit::Obj(obj));
                }
                loop {
                    let key = self.key()?;
                    if !self.eat(b":") {
                        return Err(self.fail("no ':' after a key"));
                    }
                    obj.insert(key, self.value(depth + 1)?);
                    if self.eat(b"}") {
                        return Ok(Lit::Obj(obj));
                    }
                    if !self.eat(b",") {
                        return Err(self.fail("no ',' or '}' in an object"));
                    }
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut list = Vec::new();
                if self.eat(b"]") {
                    return Ok(Lit::List(list));
                }
                loop {
                    list.push(self.value(depth + 1)?);
                    if self.eat(b"]") {
                        return Ok(Lit::List(list));
                    }
                    if !self.eat(b",") {
                        return Err(self.fail("no ',' or ']' in a list"));
                    }
                }
            }
            Some(b'"' | b'\'') => self.string().map(Lit::Str),
            _ => {
                for (word, lit) in [
                    (&b"!0"[..], Lit::Bool(true)),
                    (b"!1", Lit::Bool(false)),
                    (b"null", Lit::Null),
                    (b"true", Lit::Bool(true)),
                    (b"false", Lit::Bool(false)),
                ] {
                    if self.rest().starts_with(word) {
                        self.i += word.len();
                        return Ok(lit);
                    }
                }
                let n = number(self.rest())
                    .ok_or_else(|| self.fail("a value this reader does not know"))?;
                let text = latin1(&self.rest()[..n]);
                let num = text.parse().map_err(|_| self.fail("not a number"))?;
                self.i += n;
                Ok(Lit::Num(num))
            }
        }
    }
}

/// Where `mark` next starts in `b`, at or after `from`.
fn find(b: &[u8], mark: &[u8], from: usize) -> Option<usize> {
    b.get(from..)?
        .windows(mark.len())
        .position(|w| w == mark)
        .map(|at| at + from)
}

fn num(v: Option<&Lit>) -> Option<f64> {
    match v {
        Some(Lit::Num(n)) => Some(*n),
        _ => None,
    }
}

fn catalog_classes(tier: &BTreeMap<String, Lit>) -> [Option<f64>; 5] {
    CLASSES.map(|(_, theirs)| num(tier.get(theirs)))
}

/// The build's catalog, by first-party model id.
fn catalog(bytes: &[u8]) -> Result<BTreeMap<String, Side>, String> {
    let at = find(bytes, MARK, 0)
        .ok_or("no `pricing_tiers` in the build: not a build this reader knows")?;
    if find(bytes, MARK, at + 1).is_some() {
        return Err("two `pricing_tiers` literals: which one the build uses is not known".into());
    }
    let mut r = Reader {
        b: bytes,
        i: at + MARK.len() - 1,
    };
    let Lit::Obj(tiers) = r.value(0)? else {
        return Err(r.fail("`pricing_tiers` is not an object"));
    };
    if !r.eat(b",models:") {
        return Err(r.fail("no `models` after `pricing_tiers`"));
    }
    let Lit::List(models) = r.value(0)? else {
        return Err(r.fail("`models` is not a list"));
    };
    let mut out = BTreeMap::new();
    for model in &models {
        let Lit::Obj(model) = model else {
            continue;
        };
        let first = match model.get("provider_ids") {
            Some(Lit::Obj(ids)) => ids.get("first_party"),
            _ => None,
        };
        let (Some(Lit::Str(first)), Some(Lit::Str(tier))) = (first, model.get("pricing")) else {
            continue;
        };
        let Some(Lit::Obj(tier)) = tiers.get(tier) else {
            continue;
        };
        let long = match tier.get("long_prompt") {
            Some(Lit::Obj(lp)) => Some((num(lp.get("above_prompt_tokens")), catalog_classes(lp))),
            _ => None,
        };
        out.insert(
            first.clone(),
            Side {
                rates: catalog_classes(tier),
                long,
            },
        );
    }
    if out.is_empty() {
        return Err("the catalog names no first-party model with a price".into());
    }
    Ok(out)
}

fn differ(a: Option<f64>, b: Option<f64>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => (a - b).abs() > GAP * a.abs().max(b.abs()),
        (a, b) => a.is_some() != b.is_some(),
    }
}

fn word(v: Option<f64>) -> String {
    v.map_or_else(|| "-".to_owned(), |v| v.to_string())
}

/// The `(class, table, build)` words of every class that differs.
fn compare(ours: &Side, theirs: &Side) -> Vec<(String, String, String)> {
    fn classes(
        out: &mut Vec<(String, String, String)>,
        prefix: &str,
        a: &[Option<f64>; 5],
        b: &[Option<f64>; 5],
    ) {
        for (k, (class, _)) in CLASSES.iter().enumerate() {
            if differ(a[k], b[k]) {
                out.push((format!("{prefix}{class}"), word(a[k]), word(b[k])));
            }
        }
    }
    let mut out = Vec::new();
    classes(&mut out, "", &ours.rates, &theirs.rates);
    match (&ours.long, &theirs.long) {
        (None, None) => {}
        (Some((a_over, a)), Some((b_over, b))) => {
            if differ(*a_over, *b_over) {
                out.push(("long.over".to_owned(), word(*a_over), word(*b_over)));
            }
            classes(&mut out, "long.", a, b);
        }
        (a, b) => {
            let over = |l: &Option<(Option<f64>, [Option<f64>; 5])>| {
                l.as_ref().map_or_else(
                    || "none".to_owned(),
                    |(over, _)| format!("over {}", word(*over)),
                )
            };
            out.push(("long".to_owned(), over(a), over(b)));
        }
    }
    out
}

/// What the check found: its lines, and how many ids fell each way.
#[derive(Debug, Default, PartialEq)]
struct Check {
    lines: Vec<String>,
    same: usize,
    differ: usize,
    table_only: usize,
    build_only: usize,
}

/// The table's Anthropic rows against the build's catalog, id by id.
fn check(table: &PriceTable, build: &BTreeMap<String, Side>) -> Check {
    let rows: BTreeMap<&str, Side> = table
        .models()
        .filter(|m| m.starts_with("claude-"))
        .filter_map(|m| Some((m, table_side(table.get(m)?))))
        .collect();
    let ids: BTreeSet<&str> = rows
        .keys()
        .copied()
        .chain(build.keys().map(String::as_str))
        .collect();
    let mut c = Check::default();
    for id in ids {
        match (rows.get(id), build.get(id)) {
            (Some(_), None) => {
                c.table_only += 1;
                c.lines.push(format!("table-only  {id}"));
            }
            (None, _) => {
                c.build_only += 1;
                c.lines.push(format!("build-only  {id}"));
            }
            (Some(ours), Some(theirs)) => {
                let gaps = compare(ours, theirs);
                if gaps.is_empty() {
                    c.same += 1;
                    c.lines.push(format!("same        {id}"));
                } else {
                    c.differ += 1;
                    for (class, a, b) in gaps {
                        c.lines
                            .push(format!("differs     {id}  {class} table {a} build {b}"));
                    }
                }
            }
        }
    }
    c
}

/// The bytes inside the Codex catalog's opening brace.
const CODEX_MARK: &[u8] = b"\"models\": [";

/// objects with a `slug`.
fn codex_catalog(bytes: &[u8]) -> Result<Vec<String>, String> {
    let at = find(bytes, CODEX_MARK, 0)
        .ok_or("no `\"models\": [` in the build: not a Codex build this reader knows")?;
    if find(bytes, CODEX_MARK, at + 1).is_some() {
        return Err("two model catalogs: which one the build uses is not known".into());
    }
    let open = bytes[..at]
        .iter()
        .rposition(|b| !matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
        .filter(|&i| bytes[i] == b'{')
        .ok_or("the catalog's `models` opens no object")?;
    let mut r = Reader { b: bytes, i: open };
    let Lit::Obj(catalog) = r.value(0)? else {
        return Err(r.fail("the catalog is not an object"));
    };
    let Some(Lit::List(models)) = catalog.get("models") else {
        return Err("the catalog's `models` is not a list".into());
    };
    let slugs: Vec<String> = models
        .iter()
        .filter_map(|m| match m {
            Lit::Obj(m) => match m.get("slug") {
                Some(Lit::Str(slug)) => Some(slug.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    if slugs.is_empty() {
        return Err("the catalog names no model".into());
    }
    Ok(slugs)
}

/// What the Codex check found: its lines, and how many ids fell each way.
#[derive(Debug, Default, PartialEq)]
struct CodexCheck {
    lines: Vec<String>,
    priced: usize,
    unpriced: usize,
    table_only: usize,
}

/// does not name.
fn check_codex(table: &PriceTable, slugs: &[String]) -> CodexCheck {
    let mut c = CodexCheck::default();
    for slug in slugs {
        if table.get(slug).is_some() {
            c.priced += 1;
            c.lines.push(format!("priced      {slug}"));
        } else {
            c.unpriced += 1;
            c.lines.push(format!("unpriced    {slug}"));
        }
    }
    for id in table.models().filter(|m| !m.starts_with("claude-")) {
        if !slugs.iter().any(|s| s == id) {
            c.table_only += 1;
            c.lines.push(format!("table-only  {id}"));
        }
    }
    c
}

/// catalog, printed; an error when the build cannot be read.
fn codex_prices(table: &PriceTable, bin: &str) -> Result<(), Fail> {
    let bytes =
        std::fs::read(bin).map_err(|e| Fail::exit(2, format!("prices: cannot read {bin}: {e}")))?;
    let slugs = codex_catalog(&bytes).map_err(|e| Fail::exit(2, format!("prices: {bin}: {e}")))?;
    let c = check_codex(table, &slugs);
    for line in &c.lines {
        println!("{line}");
    }
    println!(
        "prices: {} priced, {} unpriced, {} table-only (OpenAI rows by id: the Codex build carries \
         no price, and an unpriced slug is one the public page lists none for; the table of {} \
         against {bin})",
        c.priced,
        c.unpriced,
        c.table_only,
        table.as_of().unwrap_or("?"),
    );
    Ok(())
}

/// `prices [<claude-bin>] [--codex <codex-bin>]`.
pub fn cmd_prices(args: &[String]) -> Res {
    let usage = || Fail::usage("prices: [<claude-bin>] [--codex <codex-bin>]");
    let (claude, codex) = match args {
        [] => (None, None),
        [flag, codex] if flag == "--codex" => (None, Some(codex.clone())),
        [bin] if !bin.starts_with('-') => (Some(bin.clone()), None),
        [bin, flag, codex] if flag == "--codex" && !bin.starts_with('-') => {
            (Some(bin.clone()), Some(codex.clone()))
        }
        _ => return Err(usage()),
    };
    let bin = match claude {
        Some(bin) => bin,
        None => crate::lab::find_claude().map_err(Fail::die)?,
    };
    let table = PriceTable::from_toml(PRICES_TOML)
        .map_err(|e| Fail::exit(2, format!("prices: the shipped table: {e}")))?;
    let bytes = std::fs::read(&bin)
        .map_err(|e| Fail::exit(2, format!("prices: cannot read {bin}: {e}")))?;
    let build = catalog(&bytes).map_err(|e| Fail::exit(2, format!("prices: {bin}: {e}")))?;
    let c = check(&table, &build);
    for line in &c.lines {
        println!("{line}");
    }
    println!(
        "prices: {} same, {} differ, {} table-only, {} build-only (Anthropic rows; the table of {} against {bin})",
        c.same,
        c.differ,
        c.table_only,
        c.build_only,
        table.as_of().unwrap_or("?"),
    );
    match codex.or_else(crate::lab::find_codex) {
        Some(codex) => codex_prices(&table, &codex)?,
        None => println!(
            "prices: no managed Codex (set LAB_CODEX, or --codex <codex-bin>): the table's OpenAI \
             rows were not checked"
        ),
    }
    Ok(i32::from(c.differ > 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// does not carry, and an OpenAI row (never compared).
    const TABLE: &str = r#"
format = 1
as_of = "2026-10-15"

[models."claude-haiku-5-5"]
input = 1.0
output = 5.0
cache_write = 1.25
cache_write_1h = 2.0
cache_read = 0.1
source = "https://example.com/pricing, 2026-10-15"
id_from = "https://example.com/models, 2026-10-15"

[models."claude-haiku-5-5".long]
over = 100000
input = 2.0
output = 7.5
cache_write = 2.5
cache_write_1h = 4.0
cache_read = 0.2

[models."claude-opus-5-5"]
input = 5.0
output = 25.0
cache_write = 6.25
cache_write_1h = 10.0
cache_read = 0.5
source = "https://example.com/pricing, 2026-10-15"
id_from = "https://example.com/models, 2026-10-15"

[models."claude-fable-5-1"]
input = 3.0
output = 15.0
source = "https://example.com/pricing, 2026-10-15"
id_from = "https://example.com/models, 2026-10-15"

[models."gpt-6-sol"]
input = 2.0
output = 10.0
source = "https://example.com/pricing, 2026-10-15"
id_from = "https://example.com/models, 2026-10-15"

[models."gpt-5.5"]
input = 5.0
output = 30.0
source = "https://example.com/pricing, 2026-10-15"
id_from = "https://example.com/models, 2026-10-15"
"#;

    /// with bytes that are no text either side.
    fn build(opus_output: &str) -> Vec<u8> {
        let literal = format!(
            "var a=1;let X={{schema_version:1,pricing_tiers:{{haiku:{{input:1,output:5,\
             cache_write_5m:1.25,cache_write_1h:2,cache_read:.1,long_prompt:{{above_prompt_tokens:1e5,\
             input:2,output:7.5,cache_write_5m:2.5,cache_write_1h:4,cache_read:0.2}}}},\"opus\":{{input:5,\
             output:{opus_output},cache_write_5m:6.25,cache_write_1h:10,cache_read:0.5}},old:{{input:3,\
             output:15}}}},models:[{{id:'haiku',provider_ids:{{first_party:\"claude-haiku-5-5\",bedrock:null}},\
             pricing:\"haiku\",tools:!0}},{{provider_ids:{{first_party:\"claude-opus-5-5\"}},pricing:\"opus\",\
             beta:!1}},{{provider_ids:{{first_party:\"claude-3-7-sonnet-20250219\"}},pricing:\"old\"}},\
             {{provider_ids:{{bedrock:\"x\"}},pricing:\"old\"}},\"not a model\"]}};"
        );
        [&b"\x00\xffjunk;"[..], literal.as_bytes(), b"\xfe\x00tail"].concat()
    }

    fn table() -> PriceTable {
        PriceTable::from_toml(TABLE).expect("the composed table loads")
    }

    /// the table's $25 differs on that class alone, and the exit says so.
    #[test]
    fn the_table_is_checked_against_the_builds_catalog() {
        let c = check(&table(), &catalog(&build("25")).expect("the catalog reads"));
        assert_eq!(
            c.lines,
            [
                "build-only  claude-3-7-sonnet-20250219",
                "table-only  claude-fable-5-1",
                "same        claude-haiku-5-5",
                "same        claude-opus-5-5",
            ]
        );
        assert_eq!((c.same, c.differ, c.table_only, c.build_only), (2, 0, 1, 1));
        let c = check(&table(), &catalog(&build("30")).expect("the catalog reads"));
        assert_eq!(c.differ, 1);
        assert!(
            c.lines
                .contains(&"differs     claude-opus-5-5  output table 25 build 30".to_owned()),
            "{:?}",
            c.lines
        );
        assert_eq!(
            check(&table(), &catalog(&build("25.4")).expect("reads")).differ,
            0
        );
    }

    /// same tier on both sides is the same.
    #[test]
    fn a_long_prompt_tier_differs_by_threshold_class_or_presence() {
        let flat = [Some(1.0), Some(5.0), Some(1.25), Some(2.0), Some(0.1)];
        let long = |over: f64, input: f64| {
            Some((
                Some(over),
                [Some(input), Some(7.5), Some(2.5), Some(4.0), Some(0.2)],
            ))
        };
        let side = |long| Side { rates: flat, long };
        assert_eq!(
            compare(&side(long(1e5, 2.0)), &side(long(2e5, 3.0))),
            [
                (
                    "long.over".to_owned(),
                    "100000".to_owned(),
                    "200000".to_owned()
                ),
                ("long.input".to_owned(), "2".to_owned(), "3".to_owned()),
            ]
        );
        assert_eq!(
            compare(&side(long(1e5, 2.0)), &side(None)),
            [(
                "long".to_owned(),
                "over 100000".to_owned(),
                "none".to_owned()
            )]
        );
        let mut no_read = side(None);
        no_read.rates[4] = None;
        assert_eq!(
            compare(&side(None), &no_read),
            [("cache_read".to_owned(), "0.1".to_owned(), "-".to_owned())]
        );
        assert!(compare(&side(long(1e5, 2.0)), &side(long(1e5, 2.0))).is_empty());
    }

    /// first-party model. Negative control: the composed build reads.
    #[test]
    fn a_build_without_one_readable_catalog_is_refused() {
        let good = build("25");
        let mut two = good.clone();
        two.extend_from_slice(b"pricing_tiers:{}");
        let cut = &good[..good.len() / 2];
        let none_first = b"pricing_tiers:{a:{input:1}},models:[{pricing:\"a\"}]";
        for (bytes, why) in [
            (&b"no catalog here"[..], "no `pricing_tiers`"),
            (&two[..], "two `pricing_tiers`"),
            (cut, "catalog:"),
            (
                &b"pricing_tiers:{a:`x`},models:[]"[..],
                "a value this reader does not know",
            ),
            (&none_first[..], "no first-party model"),
        ] {
            let e = catalog(bytes).expect_err(why);
            assert!(e.contains(why), "{e}");
        }
        assert_eq!(catalog(&good).expect("reads").len(), 3);
    }

    /// booleans and nulls), with bytes that are no text either side.
    fn codex_build(slugs: &[&str]) -> Vec<u8> {
        let models: Vec<String> = slugs
            .iter()
            .map(|s| {
                format!(
                    "    {{\n      \"slug\": \"{s}\",\n      \"supported_in_api\": true,\n      \
                     \"upgrade\": null,\n      \"context_window\": 272000,\n      \
                     \"description\": \"Say \\\"hi\\\"\\n\"\n    }}"
                )
            })
            .collect();
        let catalog = format!(
            "{{\n  \"models\": [\n{},\n    {{\"no_slug\": 1}}\n  ]\n}}",
            models.join(",\n")
        );
        [
            &b"\x00\xffmodel_info.rs\x00\x00"[..],
            catalog.as_bytes(),
            b"\x00tail",
        ]
        .concat()
    }

    /// slug unpriced.
    #[test]
    fn the_openai_rows_are_checked_by_id_against_the_codex_catalog() {
        let slugs = codex_catalog(&codex_build(&["gpt-6-sol", "gpt-daybreak-blue-latest"]))
            .expect("the catalog reads");
        assert_eq!(slugs, ["gpt-6-sol", "gpt-daybreak-blue-latest"]);
        let c = check_codex(&table(), &slugs);
        assert_eq!(
            c.lines,
            [
                "priced      gpt-6-sol",
                "unpriced    gpt-daybreak-blue-latest",
                "table-only  gpt-5.5",
            ]
        );
        assert_eq!((c.priced, c.unpriced, c.table_only), (1, 1, 1));
        let without = PriceTable::from_toml(&TABLE.replace(
            "[models.\"gpt-6-sol\"]\ninput = 2.0\noutput = 10.0\nsource = \"https://example.com/pricing, 2026-10-15\"\nid_from = \"https://example.com/models, 2026-10-15\"\n",
            "",
        ))
        .expect("the table loads");
        assert!(without.get("gpt-6-sol").is_none(), "the row is gone");
        let c = check_codex(&without, &slugs);
        assert_eq!((c.priced, c.unpriced), (0, 2));
        assert!(c.lines.contains(&"unpriced    gpt-6-sol".to_owned()));
    }

    /// slug. Negative control: the composed build reads.
    #[test]
    fn a_codex_build_without_one_readable_catalog_is_refused() {
        let good = codex_build(&["gpt-6-sol"]);
        let mut two = good.clone();
        two.extend_from_slice(b"{\"models\": []}");
        for (bytes, why) in [
            (&b"no catalog here"[..], "no `\"models\": [`"),
            (&two[..], "two model catalogs"),
            (&b"x \"models\": [{\"slug\": \"a\"}]"[..], "opens no object"),
            (&b"{\"models\": [{\"no_slug\": 1}]}"[..], "names no model"),
        ] {
            let e = codex_catalog(bytes).expect_err(why);
            assert!(e.contains(why), "{e}");
        }
        assert_eq!(codex_catalog(&good).expect("reads"), ["gpt-6-sol"]);
    }

    /// The literal's scalars as the bundle writes them.
    #[test]
    fn the_literal_reader_takes_the_bundles_scalars() {
        let mut r = Reader {
            b: br#"{a:!0,"b":!1,'c':null,d:-1.5e2,e:.5,f:"xA\n",g:[1,2.,{}],h:true,i:false}"#,
            i: 0,
        };
        let Lit::Obj(o) = r.value(0).expect("reads") else {
            panic!("not an object");
        };
        assert_eq!(o["a"], Lit::Bool(true));
        assert_eq!(o["b"], Lit::Bool(false));
        assert_eq!(o["c"], Lit::Null);
        assert_eq!(o["d"], Lit::Num(-150.0));
        assert_eq!(o["e"], Lit::Num(0.5));
        assert_eq!(o["f"], Lit::Str("xA\n".to_owned()));
        assert_eq!(
            o["g"],
            Lit::List(vec![
                Lit::Num(1.0),
                Lit::Num(2.0),
                Lit::Obj(BTreeMap::new())
            ])
        );
        assert_eq!((&o["h"], &o["i"]), (&Lit::Bool(true), &Lit::Bool(false)));
        assert_eq!(number(b".x"), None);
        assert_eq!(number(b"1.x"), Some(2));
    }
}
