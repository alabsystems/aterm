// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Output rendering (`emit`), stderr error/receipt formatting, and terminal safety filtering.

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

/// Renders and writes `outcome` to stdout/stderr, returning the process exit code.
pub fn emit(ctx: &mut Ctx, outcome: Outcome, opts: &OutputOpts) -> u8 {
    let Outcome {
        answer,
        mut receipt,
    } = outcome;
    let rendered = match answer {
        Answer::FailedWith { value, error } => {
            let bytes = format!("{value}\n");
            let code = write_stdout(ctx, bytes.as_bytes(), 0);
            if code != 0 {
                return code;
            }
            return fail(ctx, &receipt.verb, &error);
        }
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

/// Replaces the default usage `next` template (`agh help <verb>`) with `verb`'s help command.
pub(crate) fn with_verb_help(error: Error, verb: &str) -> Error {
    if error.kind == Kind::Usage && error.next == Kind::Usage.next_action() {
        let words = verb.replace('.', " ");
        return error.with_next(format!("agh help {words}"));
    }
    error
}

/// Rejects `--fields`, `--jq`, `--raw`, and non-JSON `--format` for verbs that output raw bytes.
///
/// # Errors
///
/// Returns a `Kind::Usage` error when `--fields`, `--jq` or `--raw` is set, or `--format` is not
/// `json`.
pub(crate) fn refuse_projection_flags(verb: &str, opts: &OutputOpts) -> Result<()> {
    if opts.fields.is_some() || opts.jq.is_some() || opts.raw || opts.format != Format::Json {
        return Err(Error::usage(format!(
            "{verb} prints bytes: --fields, --jq, --raw and --format do not apply"
        )));
    }
    Ok(())
}

/// Validates `--fields`, `--jq`, and `--raw` before a verb executes.
///
/// # Errors
///
/// `Plan::of`'s, with `agh help {verb}` as next where the error names no page of its own.
pub(crate) fn check_opts(verb: &str, opts: &OutputOpts) -> Result<()> {
    Plan::of(opts)
        .map(|_| ())
        .map_err(|e| with_verb_help(e, verb))
}

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

fn validate(noun: Noun, plan: &Plan, sample: Option<&Value>) -> Result<()> {
    match &plan.fields {
        Some(fields) => project::validate_fields(noun, fields, sample),
        None => Ok(()),
    }
}

fn projection(noun: Noun, source: &Value, plan: &Plan, defaults: &[FieldPath]) -> Value {
    match &plan.fields {
        Some(fields) => project::project(source, fields),
        None if project::is_whole(noun) => source.clone(),
        None => project::project(source, defaults),
    }
}

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
    if values.iter().any(|value| !value.is_object()) {
        columns.push(FieldPath { steps: Vec::new() });
    }
    columns
}

fn raw_line(projected: &Value, plan: &Plan) -> String {
    let path = plan.fields.as_ref().and_then(|f| f.first());
    let value = path.map_or(Value::Null, |path| project::value_at(projected, path));
    format!("{}\n", format::cell(&value))
}

fn render_object(noun: Noun, value: &Value, plan: &Plan, opts: &OutputOpts) -> Result<Vec<u8>> {
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

fn render_items(noun: Noun, items: &[Value], plan: &Plan, opts: &OutputOpts) -> Result<Vec<u8>> {
    let defaults = project::defaults(noun);
    let mut sample = items
        .first()
        .map(|first| project::with_derived(noun, first));
    validate(noun, plan, sample.as_ref())?;
    if let Some(program) = &plan.program
        && plan.fields.is_none()
    {
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

/// Writes `error`'s two redacted lines to stderr and returns `error.exit()`.
pub fn fail(ctx: &mut Ctx, verb: &str, error: &Error) -> u8 {
    let line = error.line(verb, &ctx.redactor);
    let json = error.to_json(&ctx.redactor);
    write_stderr(ctx, &format!("{line}\n{json}\n"));
    error.exit()
}

fn write_stderr(ctx: &mut Ctx, text: &str) {
    let text = if ctx.stderr_is_terminal {
        aterm_codec::tty::safe(text.as_bytes())
    } else {
        std::borrow::Cow::Borrowed(text)
    };
    let _ = ctx.stderr.write_all(text.as_bytes());
    let _ = ctx.stderr.flush();
}

/// Writes `bytes` to stdout through `ctx.fx` and terminal filtering, returning the exit status.
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

/// Writes one redacted status or receipt line to stderr.
pub fn note(ctx: &mut Ctx, line: &str) {
    let text = format!("{}\n", ctx.redactor.learned_and_tokens(line));
    write_stderr(ctx, &text);
}

/// Appends a redacted receipt value to `out`, quoting when it contains spaces, quotes, or control bytes.
pub(crate) fn receipt_value(out: &mut String, value: &str) {
    if value.bytes().any(|b| b == b' ' || b == b'"' || b < 0x20) {
        out.push('"');
        push_escaped(out, value);
        out.push('"');
    } else {
        out.push_str(value);
    }
}

/// Formats `receipt` as an `agh: ok verb=...` line with redacted values.
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
