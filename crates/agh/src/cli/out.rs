// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! THE output door (design §8): stdout carries data only; a failure is two stderr lines.
//! `emit` renders a verb's answer: the projection (`--fields` or the noun's default), then
//! `--jq`, `--raw` or `--format`, then the write and the receipt (the plan's §9.1).

use std::io::Write as _;

use aterm_json::{Map, Value};

use super::args::{Format, Global};
use super::project::{self, FieldPath};
use super::verbs::Noun;
use super::{Answer, Completeness, Ctx, FileWritten, Outcome, Receipt, format, jq};
use crate::download::DigestCheck;
use crate::error::{Error, Kind, Result, push_escaped};
use crate::redact::Redactor;

/// How a verb's answer is rendered.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutputOpts {
    /// `--format`.
    pub format: Format,
    /// `--header`.
    pub header: bool,
    /// `--fields`.
    pub fields: Option<String>,
    /// `--raw`.
    pub raw: bool,
    /// `--jq`.
    pub jq: Option<String>,
    /// `--meta`.
    pub meta: bool,
}

impl OutputOpts {
    /// The output flags of `global`.
    #[must_use]
    pub fn from_global(global: &Global) -> OutputOpts {
        OutputOpts {
            format: global.format,
            header: global.header,
            fields: global.fields.clone(),
            raw: global.raw,
            jq: global.jq.clone(),
            meta: global.meta,
        }
    }
}

/// Render and write `outcome`: 0, the FailedWith error's exit, or `write_stdout`'s answer when
/// it is not 0 (1 when stdout could not be written, 3 for a replay's divergence). Any error
/// in rendering (a bad `--fields`, a `--jq` that does not compile or fails) goes out through
/// `fail` before anything has reached stdout.
pub fn emit(ctx: &mut Ctx, outcome: Outcome, opts: &OutputOpts) -> u8 {
    let Outcome {
        answer,
        mut receipt,
    } = outcome;
    let rendered = match answer {
        // Written before anything else is read: no flag can refuse it.
        Answer::FailedWith { value, error } => {
            let bytes = format!("{value}\n");
            let code = write_stdout(ctx, bytes.as_bytes(), 0);
            if code != 0 {
                return code;
            }
            return fail(ctx, &receipt.verb, &error);
        }
        // The verb has already written a stream's bytes, so nothing after it may fail: the
        // flags were refused before the verb ran (`refuse_projection_flags`).
        Answer::Streamed { written } => {
            receipt.notes.push(("bytes".into(), written.to_string()));
            Ok(None)
        }
        Answer::Nothing => Ok(None),
        Answer::Bytes(bytes) => refuse_projection_flags(&receipt.verb, opts).map(|()| Some(bytes)),
        Answer::Object { noun, value } => {
            Plan::of(opts).and_then(|plan| render_object(noun, &value, &plan, opts).map(Some))
        }
        Answer::Items {
            noun,
            items,
            completeness,
        } => render_list(noun, &items, completeness, &mut receipt, opts).map(Some),
        Answer::Files(rows) => render_files(&rows, &mut receipt, opts).map(Some),
    };
    let bytes = match rendered {
        Ok(bytes) => bytes,
        Err(e) => return fail(ctx, &receipt.verb, &with_verb_help(e, &receipt.verb)),
    };
    if let Some(bytes) = bytes {
        let code = write_stdout(ctx, &bytes, 0);
        if code != 0 {
            return code;
        }
    }
    if opts.meta || receipt.must_print {
        let line = receipt_line(&receipt, &ctx.redactor);
        note(ctx, &line);
    }
    0
}

/// A usage error that still carries the exit table's template next (`agh help <verb>`) names
/// this verb's page instead, as the parser's own errors do; a jq error keeps `agh help jq`.
pub(crate) fn with_verb_help(error: Error, verb: &str) -> Error {
    if error.kind == Kind::Usage && error.next == Kind::Usage.next_action() {
        let words = verb.replace('.', " ");
        return error.with_next(format!("agh help {words}"));
    }
    error
}

/// The refusal of the output flags for a verb that prints bytes, a `Bytes` answer or a stream
/// to stdout: usage `{verb} prints bytes: --fields, --jq, --raw and --format do not apply`
/// when any of them is given. `cli::run` checks it for `--output -` before the verb runs, so
/// a stream never ends in an error.
///
/// # Errors
///
/// As above.
pub(crate) fn refuse_projection_flags(verb: &str, opts: &OutputOpts) -> Result<()> {
    if opts.fields.is_some() || opts.jq.is_some() || opts.raw || opts.format != Format::Json {
        return Err(Error::usage(format!(
            "{verb} prints bytes: --fields, --jq, --raw and --format do not apply"
        )));
    }
    Ok(())
}

/// The part of the output flags no answer can change: `--fields` parses, `--jq` compiles and
/// `--raw` has its one path. `cli::run` checks it before the verb runs, so a typo spends no
/// request, no wait and no file; `emit` checks it again with the answer in hand.
///
/// # Errors
///
/// `Plan::of`'s, with `agh help {verb}` as next where the error names no page of its own.
pub(crate) fn check_opts(verb: &str, opts: &OutputOpts) -> Result<()> {
    Plan::of(opts)
        .map(|_| ())
        .map_err(|e| with_verb_help(e, verb))
}

/// `--fields` and `--jq`, parsed and compiled, with the `--raw` rule checked.
struct Plan {
    fields: Option<Vec<FieldPath>>,
    program: Option<jq::Program>,
}

impl Plan {
    /// # Errors
    ///
    /// `parse_fields`' and `jq::compile`'s errors; usage `--raw needs exactly one --fields
    /// path` for `--raw` without `--jq` and with any other number of paths.
    fn of(opts: &OutputOpts) -> Result<Plan> {
        let fields = opts
            .fields
            .as_deref()
            .map(project::parse_fields)
            .transpose()?;
        let program = opts.jq.as_deref().map(jq::compile).transpose()?;
        if opts.raw && program.is_none() && fields.as_ref().is_none_or(|f| f.len() != 1) {
            return Err(Error::usage("--raw needs exactly one --fields path"));
        }
        Ok(Plan { fields, program })
    }
}

/// A list answer's bytes, with the receipt's completeness notes filled in.
fn render_list(
    noun: Noun,
    items: &[Value],
    completeness: Completeness,
    receipt: &mut Receipt,
    opts: &OutputOpts,
) -> Result<Vec<u8>> {
    let plan = Plan::of(opts)?;
    match completeness {
        Completeness::Complete => {}
        Completeness::Limited { asked } => {
            receipt.notes.push(("limit".into(), asked.to_string()));
        }
        Completeness::Partial { why } => receipt.partial(why),
    }
    render_items(noun, items, &plan, opts)
}

/// A download's rows as Download objects, with the receipt's `digest` and `written` notes
/// (one file) or its `files` count.
fn render_files(rows: &[FileWritten], receipt: &mut Receipt, opts: &OutputOpts) -> Result<Vec<u8>> {
    let plan = Plan::of(opts)?;
    if let [one] = rows {
        let digest = match one.digest {
            DigestCheck::Matched => one.sha256.to_string(),
            DigestCheck::Absent => "absent".to_string(),
        };
        receipt.notes.push(("digest".into(), digest));
        let written = if one.written { "1" } else { "0" };
        receipt.notes.push(("written".into(), written.into()));
    } else {
        receipt.notes.push(("files".into(), rows.len().to_string()));
    }
    let items: Vec<Value> = rows.iter().map(download_row).collect();
    render_items(Noun::Download, &items, &plan, opts)
}

/// One downloaded file as a Download object.
fn download_row(row: &FileWritten) -> Value {
    let mut map = Map::new();
    map.insert("asset_id".into(), Value::from(row.asset_id));
    map.insert("bytes".into(), Value::from(row.bytes));
    let digest = match row.digest {
        DigestCheck::Matched => "matched",
        DigestCheck::Absent => "absent",
    };
    map.insert("digest".into(), Value::from(digest));
    map.insert(
        "path".into(),
        Value::from(row.path.to_string_lossy().into_owned()),
    );
    map.insert("sha256".into(), Value::from(row.sha256.to_string()));
    map.insert("written".into(), Value::Bool(row.written));
    Value::Object(map)
}

/// `--fields`, when given, validated once against `sample` (the first item's source), before
/// any item is projected.
fn validate(noun: Noun, plan: &Plan, sample: Option<&Value>) -> Result<()> {
    match &plan.fields {
        Some(fields) => project::validate_fields(noun, fields, sample),
        None => Ok(()),
    }
}

/// One item's projection: `--fields` (already validated), the whole value, or the noun's
/// default.
fn projection(noun: Noun, source: &Value, plan: &Plan, defaults: &[FieldPath]) -> Value {
    match &plan.fields {
        Some(fields) => project::project(source, fields),
        None if project::is_whole(noun) => source.clone(),
        None => project::project(source, defaults),
    }
}

/// The columns `tsv` and `text` print: the `--fields` paths, the defaults, or for a
/// whole-value noun the values' top-level keys, sorted, and the empty path when a value is
/// not an object.
fn columns(noun: Noun, plan: &Plan, defaults: Vec<FieldPath>, values: &[Value]) -> Vec<FieldPath> {
    if let Some(fields) = &plan.fields {
        return fields.clone();
    }
    if !project::is_whole(noun) {
        return defaults;
    }
    let mut keys: Vec<&String> = values
        .iter()
        .filter_map(Value::as_object)
        .flat_map(Map::keys)
        .collect();
    keys.sort();
    keys.dedup();
    let mut columns: Vec<FieldPath> = keys
        .into_iter()
        .map(|key| FieldPath {
            steps: vec![project::Step::Key(key.clone())],
        })
        .collect();
    // A row that is not an object (`agh api`'s array of strings) has no keys: the row itself
    // is its one cell, under the empty path, which reads the whole value and is headed `.`.
    if values.iter().any(|value| !value.is_object()) {
        columns.push(FieldPath { steps: Vec::new() });
    }
    columns
}

/// `--raw`'s line for one projected value.
fn raw_line(projected: &Value, plan: &Plan) -> String {
    let path = plan.fields.as_ref().and_then(|f| f.first());
    let value = path.map_or(Value::Null, |path| project::value_at(projected, path));
    format!("{}\n", format::cell(&value))
}

/// An `Object` answer's bytes.
fn render_object(noun: Noun, value: &Value, plan: &Plan, opts: &OutputOpts) -> Result<Vec<u8>> {
    // A whole value that is an array (`schema exit`, `nouns`, `verbs`) has no top-level keys
    // to make columns of: under `tsv`, `text`, `--fields` and `--raw` its elements are the
    // rows, as a list's items are. `json`, `ndjson` and `--jq` alone keep plan §9.1 step 3: the
    // array is one value, on one line.
    let by_rows =
        plan.fields.is_some() || opts.raw || matches!(opts.format, Format::Tsv | Format::Text);
    if project::is_whole(noun)
        && by_rows
        && let Value::Array(elements) = value
    {
        return render_items(noun, elements, plan, opts);
    }
    let source = project::with_derived(noun, value);
    let defaults = project::defaults(noun);
    validate(noun, plan, Some(&source))?;
    let projected = projection(noun, &source, plan, &defaults);
    if let Some(program) = &plan.program {
        let input = if plan.fields.is_some() {
            projected
        } else {
            project::jq_input(noun, value)
        };
        let results = program.run(&input)?;
        return Ok(jq::render_results(&results).into_bytes());
    }
    if opts.raw {
        return Ok(raw_line(&projected, plan).into_bytes());
    }
    let values = [projected];
    let columns = columns(noun, plan, defaults, &values);
    Ok(format::render(&values, false, opts.format, opts.header, &columns).into_bytes())
}

/// An `Items` answer's bytes.
fn render_items(noun: Noun, items: &[Value], plan: &Plan, opts: &OutputOpts) -> Result<Vec<u8>> {
    let defaults = project::defaults(noun);
    let mut sample = items
        .first()
        .map(|first| project::with_derived(noun, first));
    validate(noun, plan, sample.as_ref())?;
    if let Some(program) = &plan.program
        && plan.fields.is_none()
    {
        // `--jq` alone reads the items as they are: no projection is made to throw away.
        let input = Value::Array(
            items
                .iter()
                .map(|item| project::jq_input(noun, item))
                .collect(),
        );
        let results = program.run(&input)?;
        return Ok(jq::render_results(&results).into_bytes());
    }
    let mut projected = Vec::with_capacity(items.len());
    for item in items {
        // The first item's source is the sample, already derived.
        let source = sample
            .take()
            .unwrap_or_else(|| project::with_derived(noun, item));
        projected.push(projection(noun, &source, plan, &defaults));
    }
    if let Some(program) = &plan.program {
        let results = program.run(&Value::Array(projected))?;
        return Ok(jq::render_results(&results).into_bytes());
    }
    if opts.raw {
        let lines: String = projected.iter().map(|p| raw_line(p, plan)).collect();
        return Ok(lines.into_bytes());
    }
    let columns = columns(noun, plan, defaults, &projected);
    Ok(format::render(&projected, true, opts.format, opts.header, &columns).into_bytes())
}

/// `error`'s two lines on stderr (`Error::line`, then `to_json` compact), each with `\n`, both
/// through `ctx.redactor`, then the terminal filter when `ctx.stderr_is_terminal`;
/// `error.exit()`.
pub fn fail(ctx: &mut Ctx, verb: &str, error: &Error) -> u8 {
    let line = error.line(verb, &ctx.redactor);
    let json = error.to_json(&ctx.redactor);
    write_stderr(ctx, &format!("{line}\n{json}\n"));
    error.exit()
}

/// `text` on stderr, made terminal-safe (`aterm_codec::tty::safe`) when
/// `ctx.stderr_is_terminal`: GitHub's texts reach these lines, and the line's escapes do not
/// cover DEL, C1 controls or bidi marks. A failed write is ignored.
fn write_stderr(ctx: &mut Ctx, text: &str) {
    let text = if ctx.stderr_is_terminal {
        aterm_codec::tty::safe(text.as_bytes())
    } else {
        std::borrow::Cow::Borrowed(text)
    };
    let _ = ctx.stderr.write_all(text.as_bytes());
    let _ = ctx.stderr.flush();
}

/// `bytes` on stdout: `code`, or 1 with `aterm_uds::stdout::answer_status`'s line on stderr
/// when the write failed for any reason but a gone reader. In order: the exact bytes cross
/// `ctx.fx.emit_stdout(bytes)` (an `Err` there writes nothing and answers 3); when
/// `ctx.stdout_is_terminal`, the bytes are made terminal-safe (`aterm_codec::tty::safe`); then
/// the write.
pub fn write_stdout(ctx: &mut Ctx, bytes: &[u8], code: u8) -> u8 {
    if ctx.fx.emit_stdout(bytes).is_err() {
        return 3;
    }
    let safe;
    let bytes = if ctx.stdout_is_terminal {
        safe = aterm_codec::tty::safe(bytes).into_owned().into_bytes();
        safe.as_slice()
    } else {
        bytes
    };
    let written = ctx
        .stdout
        .write_all(bytes)
        .and_then(|()| ctx.stdout.flush());
    match aterm_uds::stdout::answer_status("agh", written, code) {
        Ok(code) => code,
        Err(line) => {
            note(ctx, &line);
            1
        }
    }
}

/// One stderr line (a heartbeat, a receipt) through the redactor, with `\n`, then the terminal
/// filter when `ctx.stderr_is_terminal`. A failed write is ignored.
///
/// The caller redacts each value with `Redactor::apply` as it builds the line (`receipt_line`
/// does). The whole line then passes `Redactor::learned_and_tokens` only, because rules 2 and
/// 3 over a line joined from fields would match across a field boundary: `bearer` ending one
/// value would take the next `key=value` for a credential.
pub fn note(ctx: &mut Ctx, line: &str) {
    let text = format!("{}\n", ctx.redactor.learned_and_tokens(line));
    write_stderr(ctx, &text);
}

/// A receipt value, quoted with the error line's escapes when it holds a space, `"` or a byte
/// below 0x20. The caller has redacted it.
pub(crate) fn receipt_value(out: &mut String, value: &str) {
    if value.bytes().any(|b| b == b' ' || b == b'"' || b < 0x20) {
        out.push('"');
        push_escaped(out, value);
        out.push('"');
    } else {
        out.push_str(value);
    }
}

/// `agh: ok verb={verb} [repo={repo}] [as={as}] reqs={requests} pages={pages}
/// complete={0|1} [consistency={slug}] ms={elapsed_ms} [rate={rate.line()}] [{k}={v}]…`.
/// Each value passes `redactor.apply` raw, before it is quoted, as `Error::line`'s do.
#[must_use]
pub fn receipt_line(receipt: &Receipt, redactor: &Redactor) -> String {
    let mut pairs: Vec<(&str, String)> = vec![("verb", receipt.verb.clone())];
    if let Some(repo) = &receipt.repo {
        pairs.push(("repo", repo.clone()));
    }
    if let Some(as_) = &receipt.as_ {
        pairs.push(("as", as_.label().to_string()));
    }
    pairs.push(("reqs", receipt.requests.to_string()));
    pairs.push(("pages", receipt.pages.to_string()));
    pairs.push((
        "complete",
        if receipt.complete { "1" } else { "0" }.to_string(),
    ));
    if let Some(consistency) = receipt.consistency {
        pairs.push(("consistency", consistency.slug().to_string()));
    }
    pairs.push(("ms", receipt.elapsed_ms.to_string()));
    if let Some(rate) = &receipt.rate {
        pairs.push(("rate", rate.line()));
    }
    let mut line = String::from("agh: ok");
    for (key, value) in pairs
        .iter()
        .map(|(k, v)| (*k, v.as_str()))
        .chain(receipt.notes.iter().map(|(k, v)| (k.as_str(), v.as_str())))
    {
        line.push(' ');
        line.push_str(key);
        line.push('=');
        receipt_value(&mut line, &redactor.apply(value));
    }
    line
}
