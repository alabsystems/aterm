// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `agh api`: reads of any API-origin path, and POST, PATCH, PUT and DELETE writes to a
//! repository's path under a permit.

use std::ffi::OsStr;

use aterm_json::{Map, Value};

use super::args::{Format, Invocation};
use super::verbs::Noun;
use super::write_out::{self, ReportOutcome, WriteReport};
use super::{Answer, Completeness, Ctx, Outcome, Receipt};
use crate::error::{Error, Kind, Result, SideEffect, Sub};
use crate::github::raw::RawSend;
use crate::github::{RawAnswer, RawGet};
use crate::journal::{Intent, IntentDraft, IntentId, IntentStore, IntentView, Settled};
use crate::policy::{ExplicitTarget, Named, RouteRequest, RouteTarget, Router};
use crate::slug::Slug;
use crate::wire::{Method, Origin};
use crate::write::{Outcome as WriteOutcome, WriteOp};

fn method(inv: &Invocation) -> Result<Method> {
    let text = inv.flag("--method").unwrap_or("GET").to_ascii_uppercase();
    match text.as_str() {
        "GET" => Ok(Method::Get),
        "HEAD" => Ok(Method::Head),
        "POST" => Ok(Method::Post),
        "PATCH" => Ok(Method::Patch),
        "PUT" => Ok(Method::Put),
        "DELETE" => Ok(Method::Delete),
        _ => Err(inv.usage("-X wants GET, HEAD, POST, PATCH, PUT or DELETE")),
    }
}

// Treat `+` as space before percent-decoding.
fn query_text(text: &str) -> String {
    aterm_codec::pct::decode(&text.replace('+', " "))
}

fn path_and_query(
    inv: &Invocation,
    typed: &str,
    api: &Origin,
) -> Result<(String, Vec<(String, String)>)> {
    let lower = typed.to_ascii_lowercase();
    let absolute;
    let typed = if lower.starts_with("http://") || lower.starts_with("https://") {
        match Origin::parse(typed) {
            Some((origin, path_and_query)) if origin == *api => {
                absolute = path_and_query;
                absolute.as_str()
            }
            _ => {
                return Err(inv.usage(format!(
                    "agh api reads only the API origin ({})",
                    api.base()
                )));
            }
        }
    } else {
        typed
    };
    // Strip any `#fragment` before routing so `/user#` routes as `/user`.
    let typed = typed.split('#').next().unwrap_or_default();
    let (path, query) = typed.split_once('?').unwrap_or((typed, ""));
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    // Refuse dot-segments before `-R` routing can send an id lookup request.
    crate::path::refuse_encoded_dot_segment("path", &path)?;
    let pairs = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (query_text(key), query_text(value))
        })
        .collect();
    Ok((path, pairs))
}

/// One `-f` or `-F` pair; `magic` when `-F` typed it ([`field_value`]).
struct Field {
    key: String,
    value: String,
    magic: bool,
}

fn fields(inv: &Invocation) -> Result<Vec<Field>> {
    let mut pairs = Vec::new();
    for (name, value) in &inv.flags {
        let flag = match *name {
            "--raw-field" => "-f",
            "--field" => "-F",
            _ => continue,
        };
        let text = value.as_deref().unwrap_or_default();
        let Some((key, value)) = text.split_once('=') else {
            return Err(inv.usage(format!("{flag} wants KEY=VALUE")));
        };
        if flag == "-F" && value.starts_with('@') {
            return Err(
                inv.usage("-F k=@file is not supported: use --input FILE for a JSON request body")
            );
        }
        pairs.push(Field {
            key: key.to_string(),
            value: value.to_string(),
            magic: flag == "-F",
        });
    }
    Ok(pairs)
}

fn target(
    inv: &Invocation,
    path: &str,
    router: &Router,
) -> Result<(RouteTarget, Named, Option<String>)> {
    let decoded: Vec<String> = path
        .split('/')
        .filter(|s| !s.is_empty())
        .map(aterm_codec::pct::decode)
        .collect();
    let segs: Vec<&str> = decoded.iter().map(String::as_str).collect();
    let given = match &inv.global.repo {
        Some(_) => Some(inv.repo(router)?),
        None => None,
    };
    let (target, named) = match segs.as_slice() {
        ["repositories", id, ..] => {
            return match given {
                Some((slug, named)) => {
                    Ok((RouteTarget::Repo(slug), named, Some((*id).to_string())))
                }
                None => {
                    Err(inv.usage("repositories/{id} paths need -R OWNER/NAME, which routes them"))
                }
            };
        }
        ["repos", owner, repo, ..] => match Slug::new(owner, repo) {
            Ok(slug) => {
                if let Some((given, _)) = &given
                    && given.key() != slug.key()
                {
                    return Err(inv.usage(format!(
                        "-R {given} and the path's {slug} name different repositories"
                    )));
                }
                let literal = ExplicitTarget::from_argv(OsStr::new(&format!("{owner}/{repo}")))?;
                (RouteTarget::Repo(slug), Named::Literal(literal))
            }
            Err(_) => (RouteTarget::Unscoped, Named::Default),
        },
        ["orgs" | "users", owner, ..] => (RouteTarget::Owner((*owner).to_string()), Named::Default),
        // `/user/{account_id}…` names another account by numeric id, not the profile's own.
        ["user", id, ..] if crate::path::is_digits(id) => (RouteTarget::Unscoped, Named::Default),
        ["user", ..] => (RouteTarget::Identity, Named::Default),
        _ => (RouteTarget::Unscoped, Named::Default),
    };
    Ok((target, named, None))
}

// Verify that `/repositories/{id}` matches the `-R` repository's id.
fn hold_id_to_repo(
    inv: &Invocation,
    gh: &crate::client::Github,
    route: &RouteRequest,
    path_id: &str,
) -> Result<u32> {
    if !crate::path::is_digits(path_id) {
        return Err(inv.usage(format!(
            "repositories/{path_id} wants a repository id (digits)"
        )));
    }
    let RouteTarget::Repo(slug) = &route.target else {
        return Ok(0);
    };
    let (id, requests) = gh.repo_id_counted(slug, route)?;
    if crate::path::digits::<u64>(path_id) != Some(id) {
        return Err(inv.usage(format!(
            "-R {slug} is repository {id}; the path names {path_id}"
        )));
    }
    Ok(requests)
}

fn headers_value(headers: &[(String, String)]) -> Value {
    let mut joined = Map::new();
    for (name, value) in headers {
        let name = name.to_ascii_lowercase();
        let next = match joined.get(&name).and_then(Value::as_str) {
            Some(before) => format!("{before}, {value}"),
            None => value.clone(),
        };
        joined.insert(name, Value::from(next));
    }
    Value::Object(joined)
}

/// Executes `agh api PATH` on the API origin: a read with `GET` or `HEAD`, a write with
/// `POST`, `PATCH`, `PUT` or `DELETE` (`api_write`).
///
/// # Errors
///
/// Usage for another method, HEAD or `--include` with `--all`, `--dry-run` or `--confirm` on a
/// read, another origin, a dot-segment, a bad field, an unroutable path, a `/repositories/{id}`
/// that is not `-R`'s, or object pages with neither ndjson nor `--jq`; the read's error; the
/// write's (`api_write`).
pub(crate) fn api(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let method = method(inv)?;
    if !method.is_read() {
        return api_write(ctx, inv, method);
    }
    if inv.has("--input") {
        return Err(
            inv.usage("--input supplies a write's JSON body; use -X POST, PATCH, PUT or DELETE")
        );
    }
    if inv.has("--dry-run") || inv.has("--confirm") {
        return Err(inv.usage("--dry-run and --confirm are a write's flags"));
    }
    let all = inv.has("--all");
    if method == Method::Head && all {
        return Err(inv.usage("-X HEAD reads no body, so --all has no pages to walk"));
    }
    if all && inv.has("--include") {
        return Err(inv.usage(
            "--include answers one response's status and headers, and --all reads many: give one",
        ));
    }
    let typed = inv.require(0, "PATH")?;
    let gh = ctx.github()?;
    let (path, mut query) = path_and_query(inv, typed, gh.endpoints().api())?;
    query.extend(
        fields(inv)?
            .into_iter()
            .map(|field| (field.key, field.value)),
    );
    let (route_target, named, path_id) = target(inv, &path, gh.router())?;
    let route = RouteRequest::read(route_target)
        .with_anonymous(inv.global.anonymous)
        .with_as(inv.global.as_profile.clone())
        .with_named(named);
    let lookups = match &path_id {
        Some(path_id) => hold_id_to_repo(inv, &gh, &route, path_id)?,
        None => 0,
    };
    let include = inv.has("--include");
    let request = RawGet {
        method,
        path,
        query,
        accept: inv.flag("--accept").map(str::to_string),
        route,
        all,
        include,
        opts: inv.list_opts().with_limit(None),
    };
    let (outcome, end) = gh.raw().get_with_end(&request, inv.global.no_probe)?;
    let limited = |mut items: Vec<Value>| {
        let completeness = Completeness::of(crate::page::cut(end, &mut items, inv.global.limit));
        (items, completeness)
    };
    let answer = match outcome.answer {
        _ if method == Method::Head && !include => Answer::Nothing,
        RawAnswer::Json(Value::Array(items)) => {
            let (items, completeness) = limited(items);
            Answer::Items {
                noun: Noun::Raw,
                items,
                completeness,
            }
        }
        RawAnswer::Json(value) => Answer::Object {
            noun: Noun::Raw,
            value,
        },
        RawAnswer::Pages(pages) => {
            if inv.global.jq.is_none() && inv.global.format != Format::Ndjson {
                return Err(inv.usage(
                    "these pages are objects, not arrays: add --format ndjson to print one \
                     per line",
                ));
            }
            let (items, completeness) = limited(pages);
            Answer::Items {
                noun: Noun::Raw,
                items,
                completeness,
            }
        }
        RawAnswer::Bytes(bytes) => Answer::Bytes(bytes),
        RawAnswer::Included {
            status,
            headers,
            body,
        } => {
            let mut value = Map::new();
            value.insert("body".into(), body);
            value.insert("headers".into(), headers_value(&headers));
            value.insert("status".into(), Value::from(u64::from(status)));
            Answer::Object {
                noun: Noun::Raw,
                value: Value::Object(value),
            }
        }
    };
    let mut receipt = Receipt::new(&inv.verb_name()).metered(&gh.meter());
    receipt.requests = outcome.requests.saturating_add(lookups);
    receipt.pages = outcome.pages;
    receipt.as_ = Some(outcome.as_);
    if let RouteTarget::Repo(slug) = &request.route.target {
        receipt.repo = Some(slug.to_string());
    }
    Ok(Outcome { answer, receipt })
}

/// GitHub's answer as a write's report carries it: its JSON body, `null` for an empty body
/// (a 204), its text for a body that is not JSON, or `{body, headers, status}` under
/// `--include`.
fn answer_value(answer: RawAnswer) -> Value {
    match answer {
        RawAnswer::Json(value) => value,
        RawAnswer::Pages(pages) => Value::Array(pages),
        RawAnswer::Bytes(bytes) if bytes.is_empty() => Value::Null,
        RawAnswer::Bytes(bytes) => Value::String(String::from_utf8_lossy(&bytes).into_owned()),
        RawAnswer::Included {
            status,
            headers,
            body,
        } => {
            let mut value = Map::new();
            value.insert("body".into(), body);
            value.insert("headers".into(), headers_value(&headers));
            value.insert("status".into(), Value::from(u64::from(status)));
            Value::Object(value)
        }
    }
}

/// A field's JSON value. `-f` is always a string. `-F` reads `true`, `false`, `null` and an
/// integer as their JSON types, as `gh api -F` does, because a REST body such as a pull
/// request's `draft` refuses the string `"true"`; any other `-F` value is a string.
fn field_value(value: String, magic: bool) -> Value {
    if magic {
        match value.as_str() {
            "true" => return Value::Bool(true),
            "false" => return Value::Bool(false),
            "null" => return Value::Null,
            text => {
                if let Ok(number) = text.parse::<i64>() {
                    return Value::from(number);
                }
            }
        }
    }
    Value::String(value)
}

/// One step of a field's key past its name: `[k]` a member of an object, `[]` an array's
/// next element.
enum Step {
    Member(String),
    Append,
}

/// A field's key split as `gh api` reads it: `name`, then any `[k]` and `[]` steps, so
/// `labels[]` is an array element and `required_status_checks[strict]` an object's member.
fn key_steps(key: &str) -> std::result::Result<(String, Vec<Step>), String> {
    let (name, mut rest) = match key.find('[') {
        Some(at) => (&key[..at], &key[at..]),
        None => (key, ""),
    };
    if name.is_empty() {
        return Err(format!("field key {key:?} has no name before its brackets"));
    }
    let mut steps = Vec::new();
    while !rest.is_empty() {
        let Some(inner) = rest.strip_prefix('[') else {
            return Err(format!(
                "field key {key:?} has text after a bracket: {rest:?}"
            ));
        };
        let Some(close) = inner.find(']') else {
            return Err(format!("field key {key:?} has an unclosed bracket"));
        };
        let member = &inner[..close];
        if member.contains('[') {
            return Err(format!("field key {key:?} nests a bracket in a bracket"));
        }
        steps.push(if member.is_empty() {
            Step::Append
        } else {
            Step::Member(member.to_string())
        });
        rest = &inner[close + 1..];
    }
    Ok((name.to_string(), steps))
}

/// Puts `leaf` at `key` then `steps` inside `map`: a plain key's last value wins, a `[k]`
/// step opens (or reuses) an object, and a `[]` step an array.
fn put(map: &mut Map, key: String, steps: &[Step], leaf: Value) -> std::result::Result<(), String> {
    let Some((step, more)) = steps.split_first() else {
        map.insert(key, leaf);
        return Ok(());
    };
    match step {
        Step::Member(member) => {
            let child = map
                .entry(key.clone())
                .or_insert_with(|| Value::Object(Map::new()));
            let Value::Object(object) = child else {
                return Err(format!("field {key} is both a value and an object"));
            };
            put(object, member.clone(), more, leaf)
        }
        Step::Append => {
            let child = map
                .entry(key.clone())
                .or_insert_with(|| Value::Array(Vec::new()));
            let Value::Array(items) = child else {
                return Err(format!("field {key} is both a value and an array"));
            };
            append(items, &key, more, leaf)
        }
    }
}

/// Appends to an array as `gh api` does: `a[]=v` pushes `v`; `a[][k]=v` sets `k` in the
/// last element when that element is an object still without `k` (or `k` leads deeper), and
/// otherwise starts the next object, so `a[][path]=x a[][line]=1 a[][path]=y` is two objects.
fn append(
    items: &mut Vec<Value>,
    key: &str,
    steps: &[Step],
    leaf: Value,
) -> std::result::Result<(), String> {
    match steps.split_first() {
        None => {
            items.push(leaf);
            Ok(())
        }
        Some((Step::Member(member), more)) => {
            let reuse = matches!(
                items.last(),
                Some(Value::Object(last)) if !more.is_empty() || !last.contains_key(member)
            );
            if !reuse {
                items.push(Value::Object(Map::new()));
            }
            let Some(Value::Object(object)) = items.last_mut() else {
                return Err(format!("field {key}[] mixes values and objects"));
            };
            put(object, member.clone(), more, leaf)
        }
        Some((Step::Append, _)) => Err(format!("field {key}[][] nests an array in an array")),
    }
}

/// The `-f` and `-F` pairs as a write's JSON body, each key read by [`key_steps`];
/// `None` when no field was given.
///
/// # Errors
///
/// A usage error for a key whose brackets do not parse, one that makes a field both a value and
/// an object or array, or mixed field and file bodies; or an unreadable or invalid JSON file.
fn body_of(inv: &Invocation, pairs: Vec<Field>) -> Result<Option<Value>> {
    if let Some(file) = inv.flag("--input") {
        if !pairs.is_empty() {
            return Err(inv.usage("--input and -f/-F cannot supply the same request body"));
        }
        return read_input(file).map(Some);
    }
    if pairs.is_empty() {
        return Ok(None);
    }
    let mut object = Map::new();
    for Field { key, value, magic } in pairs {
        let (name, steps) = key_steps(&key).map_err(|why| inv.usage(why))?;
        put(&mut object, name, &steps, field_value(value, magic)).map_err(|why| inv.usage(why))?;
    }
    Ok(Some(Value::Object(object)))
}

/// A bounded JSON body supplied explicitly, avoiding the operating system's argv limit.
/// Open nonblocking and judge the opened handle so a FIFO cannot wait for a writer.
pub(super) fn read_input<T: serde::de::DeserializeOwned>(file: &str) -> Result<T> {
    use std::io::Read as _;
    const MAX: u64 = 8 * 1024 * 1024;
    let unread = |e: &dyn std::fmt::Display| Error::local(format!("{file}: {e}"));
    let invalid = |why: &str| Error::new(Kind::Invalid, format!("{file}: {why}"));
    let opened =
        crate::download::open_nonblocking(std::path::Path::new(file)).map_err(|e| unread(&e))?;
    let meta = opened.metadata().map_err(|e| unread(&e))?;
    if !meta.is_file() {
        return Err(invalid("JSON input must be a regular file"));
    }
    if meta.len() > MAX {
        return Err(invalid("JSON input exceeds 8 MiB"));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len().saturating_add(1)).unwrap_or(0));
    opened
        .take(MAX + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| unread(&e))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX {
        return Err(invalid("JSON input exceeds 8 MiB"));
    }
    // Parse errors must not copy request contents (which may include private data).
    aterm_json::from_slice(&bytes).map_err(|_| invalid("input is not valid JSON for this request"))
}

/// The journal as `Raw::send` writes it, keeping the id its `begin` returned: the report names
/// this run's intent with no second read, which could fail after the write landed or find
/// another process's intent of the same key. A PATCH, PUT or DELETE is never journaled, so it
/// opens no journal (`journal` is `None`) and `Raw::send` calls none of these.
struct Begun<'a> {
    journal: Option<&'a dyn IntentStore>,
    id: std::sync::Mutex<Option<IntentId>>,
}

impl Begun<'_> {
    /// The intent this run began, if it began one.
    fn id(&self) -> Option<IntentId> {
        self.id
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The journal, or could_not_run when this write opened none.
    fn journal(&self) -> Result<&dyn IntentStore> {
        self.journal
            .ok_or_else(|| Error::local("a PATCH, PUT or DELETE is not journaled"))
    }
}

impl IntentStore for Begun<'_> {
    fn begin(&self, draft: &IntentDraft) -> Result<IntentId> {
        let id = self.journal()?.begin(draft)?;
        *self
            .id
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(id.clone());
        Ok(id)
    }

    fn settle(&self, id: &IntentId, outcome: &Settled) -> Result<()> {
        self.journal()?.settle(id, outcome)
    }

    fn observe(&self, id: &IntentId, found: Option<u64>) -> Result<()> {
        self.journal()?.observe(id, found)
    }

    fn open(&self) -> Result<Vec<Intent>> {
        self.journal()?.open()
    }

    fn latest(&self, op: WriteOp, key: &str) -> Result<Option<IntentView>> {
        self.journal()?.latest(op, key)
    }
}

/// `agh api` with `POST`, `PATCH`, `PUT` or `DELETE` (design §9.4, plan §7 body 8). The
/// repository is the literal the typed PATH holds, a `DELETE` needs `--confirm PATH` typed
/// again, and `--dry-run` routes and mints the permit and sends and journals nothing. Off
/// unix every write, a dry run included, prints `not_sent` and exits 3 (`write::unix_only`).
/// The answer is a `WriteReport`, op `api`, key `api {METHOD} {path}`.
///
/// # Errors
///
/// Usage for `--all`, a path outside `/repos/{owner}/{repo}/…`, and the read path's path and
/// field rules; Refused (exit 1) for a DELETE without its `--confirm`; the permit's Usage
/// refusal (`--anonymous`, an unknown or unscoped `--as`) and `Raw::send`'s Usage, exit 2 with
/// no report. The permit's other refusals and every other error of the journal or
/// `Raw::send` are a refused, unknown or not-sent write, which prints its report and exits 1
/// (an answered 401, 403 or 429 its own 4, 5 or 11), 13 or 3, or the error's own exit.
fn api_write(ctx: &mut Ctx, inv: &Invocation, method: Method) -> Result<Outcome> {
    if inv.has("--all") {
        return Err(inv.usage("--all pages a read"));
    }
    let typed = inv.require(0, "PATH")?;
    if method == Method::Delete && inv.flag("--confirm") != Some(typed) {
        return Err(Error::refused(format!("a DELETE needs --confirm {typed}"))
            .with_next(format!("type the path again: --confirm {typed}")));
    }
    let gh = ctx.github()?;
    let (path, query) = path_and_query(inv, typed, gh.endpoints().api())?;
    let body = body_of(inv, fields(inv)?)?;
    let (route_target, named, path_id) = target(inv, &path, gh.router())?;
    let slug = match (&route_target, path_id) {
        (RouteTarget::Repo(slug), None) => slug.clone(),
        _ => {
            return Err(inv.usage(format!(
                "a write names its repository in its path: /repos/OWNER/NAME/…, not {path}"
            )));
        }
    };
    let path = if query.is_empty() {
        path
    } else {
        format!("{path}?{}", crate::path::query(&query))
    };
    let route = RouteRequest::write(route_target)
        .with_anonymous(inv.global.anonymous)
        .with_as(inv.global.as_profile.clone())
        .with_named(named);
    let key = crate::github::raw::journal_key(method, &path);
    // Every report of a dry run says so, its refusals included, as `Write::report` marks them.
    let dry = inv.has("--dry-run");
    let report = |outcome: ReportOutcome| {
        let report = WriteReport::new(outcome.slug(), WriteOp::Api, &slug, &key);
        if dry { report.dry_run() } else { report }
    };
    let receipt = |gh: &crate::client::Github| {
        let mut receipt = Receipt::new(&inv.verb_name()).metered(&gh.meter());
        receipt.repo = Some(slug.to_string());
        receipt
    };
    // The permit's refusal prints the write's report, as every other write verb's does
    // (plan §4.6.20): outcome `refused`, with the refusal's own exit. A Usage refusal
    // (`--anonymous`, an unknown or unscoped `--as`) prints none, as `Write::finish` and
    // `Raw::send`'s arm below answer it (plan §10 body 1.5).
    let permit = match gh.permit(&route) {
        Ok(permit) => permit,
        Err(error) if error.kind == Kind::Usage => return Err(error),
        Err(error) => {
            return Ok(Outcome {
                answer: write_out::failed(report(ReportOutcome::of(&error)), error),
                receipt: receipt(&gh),
            });
        }
    };
    // Off unix every write answers `write::unix_only()` before anything is journaled or sent
    // (`Raw::send`'s and `gh.journal`'s first step), so a dry run answers it too: it
    // predicts the real run, which never sends.
    if cfg!(not(unix)) {
        return Ok(Outcome {
            answer: Answer::FailedWith {
                value: report(ReportOutcome::NotSent).to_value(),
                error: crate::write::unix_only(),
            },
            receipt: receipt(&gh),
        });
    }
    if dry {
        let value = report(ReportOutcome::WouldSend).to_value();
        return Ok(Outcome {
            answer: Answer::Object {
                noun: Noun::Write,
                value,
            },
            receipt: receipt(&gh),
        });
    }
    let mut send = RawSend::new(method, &path, route);
    send.body = body;
    send.accept = inv.flag("--accept").map(str::to_string);
    send.include = inv.has("--include");
    // Only a POST is journaled (plan §7 body 7): a PATCH, PUT or DELETE opens no journal.
    let journal = if method == Method::Post {
        gh.journal(&slug).map(Some)
    } else {
        Ok(None)
    };
    let begun = Begun {
        journal: journal
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .map(|journal| journal as &dyn IntentStore),
        id: std::sync::Mutex::new(None),
    };
    let sent = match &journal {
        Ok(_) => gh.raw().send(&permit, &send, &begun),
        Err(error) => Err(error.clone()),
    };
    let mut receipt = receipt(&gh);
    let answer = match sent {
        Ok(WriteOutcome::Done(raw)) => {
            receipt.as_ = Some(raw.as_.clone());
            let intent = begun.id();
            let value = answer_value(raw.answer);
            // Under `--include` the answer is `{body, headers, status}`: the object is its body.
            let object = if send.include {
                value.get("body").unwrap_or(&Value::Null)
            } else {
                &value
            };
            let id = object.get("id").and_then(Value::as_u64);
            let url = ["html_url", "url"]
                .iter()
                .find_map(|name| object.get(name).and_then(Value::as_str))
                .map(str::to_string);
            let report = report(ReportOutcome::Created)
                .with_object(id, url.as_deref())
                .with_intent(intent.as_ref())
                .with_answer(Some(value));
            Answer::Object {
                noun: Noun::Write,
                value: report.to_value(),
            }
        }
        Ok(WriteOutcome::NeverSent(error)) => Answer::FailedWith {
            value: report(ReportOutcome::NotSent).to_value(),
            error,
        },
        Ok(WriteOutcome::Refused(error)) => {
            // A refused write exits 1 (plan §7 body 8), but an answered 401, 403 or 429 keeps
            // its own exit (4, 5 or 11), which a script that backs off or re-authenticates reads.
            let error = if matches!(
                error.kind,
                Kind::Unauthenticated | Kind::Forbidden | Kind::RateLimited
            ) {
                error
            } else {
                let why = error.why.clone();
                error.recast(Kind::Refused, why)
            };
            Answer::FailedWith {
                value: report(ReportOutcome::Refused).to_value(),
                error,
            }
        }
        Ok(WriteOutcome::Unknown { intent, error }) => {
            let value = report(ReportOutcome::Unknown)
                .with_intent(Some(&intent))
                .to_value();
            Answer::FailedWith {
                value,
                // Its OutcomeUnknown error (exit 13).
                error: crate::write::unknown_error(&intent, error),
            }
        }
        // A PATCH, PUT or DELETE that may have landed has no intent: it is OutcomeUnknown
        // (exit 13), and its next reads the state before anything is sent again. A POST's
        // error (its settle failed after an unknown answer) keeps its own next, `agh intent
        // show`, and its report names the intent it left open.
        Err(error) if error.side_effect == SideEffect::Unknown => {
            let why = error.why.clone();
            let error = error.recast(Kind::OutcomeUnknown, why);
            let error = if method == Method::Post {
                error
            } else {
                error.with_next(format!("agh api {path}"))
            };
            Answer::FailedWith {
                value: report(ReportOutcome::Unknown)
                    .with_intent(begun.id().as_ref())
                    .to_value(),
                error,
            }
        }
        Err(error) if error.kind == Kind::Usage => return Err(error),
        // Every other error prints its report and keeps its own exit: an open intent's
        // refusal at `begin` (OutcomeUnknown, exit 13) names that intent, a settle that failed
        // after a 2xx (could_not_run, side effect applied) names the intent it left open, and
        // a journal that would not open is not sent.
        Err(error) => {
            let intent = match &error.sub {
                Sub::IntentOpen { intent } => IntentId::parse(intent),
                _ => begun.id(),
            };
            Answer::FailedWith {
                value: report(ReportOutcome::of(&error))
                    .with_intent(intent.as_ref())
                    .to_value(),
                error,
            }
        }
    };
    Ok(Outcome { answer, receipt })
}
