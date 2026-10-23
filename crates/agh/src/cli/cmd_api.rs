// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `agh api`: read requests against any API-origin path.

use std::ffi::OsStr;

use aterm_json::{Map, Value};

use super::args::{Format, Invocation};
use super::verbs::Noun;
use super::{Answer, Completeness, Ctx, Outcome, Receipt};
use crate::error::Result;
use crate::github::{RawAnswer, RawGet};
use crate::policy::{ExplicitTarget, Named, RouteRequest, RouteTarget, Router};
use crate::slug::Slug;
use crate::wire::{Method, Origin};

fn method(inv: &Invocation) -> Result<Method> {
    let text = inv.flag("--method").unwrap_or("GET").to_ascii_uppercase();
    match text.as_str() {
        "GET" => Ok(Method::Get),
        "HEAD" => Ok(Method::Head),
        "POST" | "PATCH" | "PUT" | "DELETE" => {
            Err(inv.usage("writes arrive in M2: agh api reads with GET or HEAD"))
        }
        _ => Err(inv.usage("-X wants GET or HEAD")),
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

fn fields(inv: &Invocation) -> Result<Vec<(String, String)>> {
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
            return Err(inv.usage("-F k=@file is not supported: agh api reads"));
        }
        pairs.push((key.to_string(), value.to_string()));
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

/// Executes `agh api PATH` with `GET` or `HEAD` on the API origin.
///
/// # Errors
///
/// Usage for a write method, HEAD or `--include` with `--all`, another origin, a dot-segment, a
/// bad field, an unroutable path, a `/repositories/{id}` that is not `-R`'s, or object pages
/// with neither ndjson nor `--jq`; the read's error.
pub(crate) fn api(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let method = method(inv)?;
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
    query.extend(fields(inv)?);
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
