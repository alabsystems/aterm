// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Paginated GitHub REST collection reader.

use aterm_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// GitHub's API origin, which every pagination link of a listing read from github.com names.
pub const API: &str = "https://api.github.com";

/// The bound on pages read for one collection.
const MAX_PAGES: usize = 10_000;

/// One answered page: its JSON body and the one header pagination reads (`link`, lowercased).
pub type Page = (Value, BTreeMap<String, String>);

/// Parses a complete HTTP 200 response into a [`Page`].
///
/// # Errors
/// Another status, two `Link` headers, or a body that is not JSON.
pub fn page(
    status: u16,
    headers: impl IntoIterator<Item = (String, String)>,
    body: &[u8],
) -> Result<Page, String> {
    if status != 200 {
        return Err(format!("GitHub answered HTTP {status}, not 200"));
    }
    let mut fields = BTreeMap::new();
    for (name, value) in headers {
        let name = name.to_ascii_lowercase();
        if name == "link" && fields.insert(name, value.trim().into()).is_some() {
            return Err("ambiguous GitHub pagination headers".into());
        }
    }
    let value = aterm_json::from_slice(body).map_err(|_| "unrecognized GitHub listing response")?;
    Ok((value, fields))
}

/// Parses and validates the listing origin, endpoint path, and query parameters before any
/// network request runs.
fn parse_listing_base<'a>(
    api: &str,
    base: &'a str,
    per_page: usize,
) -> Result<(&'a str, BTreeMap<&'a str, &'a str>), String> {
    if per_page == 0 {
        return Err("invalid GitHub listing bound".into());
    }
    if api.is_empty() || api.ends_with('/') {
        return Err("invalid GitHub API origin".into());
    }
    if !base.starts_with('/') {
        return Err("invalid GitHub listing endpoint".into());
    }
    let (expected_path, expected_query) = base
        .split_once('?')
        .filter(|(path, query)| !path.is_empty() && !query.is_empty())
        .ok_or("invalid GitHub listing endpoint")?;
    let mut expected = BTreeMap::new();
    for part in expected_query.split('&') {
        let (key, value) = part
            .split_once('=')
            .filter(|(key, _)| !key.is_empty())
            .ok_or("invalid listing query")?;
        if key == "page" {
            return Err("invalid listing query: page is managed by pagination".into());
        }
        if expected.insert(key, value).is_some() {
            return Err("duplicate listing query key".into());
        }
    }
    Ok((expected_path, expected))
}

/// Fetches all pages of the collection at `base` using `get`, validating links against `api`.
///
/// # Errors
/// A page `get` could not read whole, a page that is not an array or holds more than `per_page`, a
/// collection whose pages disagree, or more than `MAX_PAGES` pages.
pub fn all(
    api: &str,
    base: &str,
    per_page: usize,
    mut get: impl FnMut(&str) -> Result<Page, String>,
) -> Result<Vec<Value>, String> {
    let (expected_path, expected_query) = parse_listing_base(api, base, per_page)?;
    let mut items = Vec::new();
    let mut last = None;
    let mut alias: Option<String> = None;
    for page in 1..=MAX_PAGES {
        let (value, headers) = get(&format!("{base}&page={page}"))?;
        let values = value
            .as_array()
            .ok_or("GitHub listing page is not an array")?;
        if values.len() > per_page {
            return Err("GitHub listing page exceeds its requested bound".into());
        }
        let scope = Scope {
            api,
            expected_path,
            expected_query: &expected_query,
            alias: &mut alias,
        };
        let (next, advertised_last) = pagination(headers.get("link"), scope, page)?;
        if let Some(advertised) = advertised_last {
            if advertised < page || last.is_some_and(|last| last != advertised) {
                return Err("GitHub pagination changed during the listing; run again".into());
            }
            last = Some(advertised);
        }
        items.extend(values.iter().cloned());
        if next {
            if values.is_empty() {
                return Err("an empty GitHub page advertises more".into());
            }
            continue;
        }
        if last.is_some_and(|last| page < last) {
            return Err("GitHub pagination ended before its advertised final page".into());
        }
        if values.len() == per_page {
            let (tail, tail_headers) = get(&format!("{base}&page={}", page + 1))?;
            let scope = Scope {
                api,
                expected_path,
                expected_query: &expected_query,
                alias: &mut alias,
            };
            let (tail_next, tail_last) = pagination(tail_headers.get("link"), scope, page + 1)?;
            if tail.as_array().is_none_or(|tail| !tail.is_empty())
                || tail_next
                || tail_last.is_some_and(|last| last != page)
            {
                return Err("GitHub pagination omitted more of the listing".into());
            }
        }
        return Ok(items);
    }
    Err("GitHub pagination exceeded its explicit bound".into())
}

/// The listing a page's links must stay on: the API origin, the requested endpoint, and the
/// repository-id path the listing's links named so far.
struct Scope<'a> {
    api: &'a str,
    expected_path: &'a str,
    expected_query: &'a BTreeMap<&'a str, &'a str>,
    alias: &'a mut Option<String>,
}

/// Whether `path` is `expected` (`/repos/{owner}/{repo}/{suffix}`) spelled by repository id,
/// `/repositories/{id}/{suffix}`, with the same suffix.
fn names_repository_by_id(path: &str, expected: &str) -> bool {
    let Some(rest) = expected.strip_prefix("/repos/") else {
        return false;
    };
    let mut parts = rest.splitn(3, '/');
    let (Some(owner), Some(repo), Some(suffix)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    if owner.is_empty() || repo.is_empty() || suffix.is_empty() {
        return false;
    }
    path.strip_prefix("/repositories/")
        .and_then(|rest| rest.split_once('/'))
        .is_some_and(|(id, rest)| {
            !id.is_empty()
                && !id.starts_with('0')
                && id.bytes().all(|b| b.is_ascii_digit())
                && rest == suffix
        })
}

/// What one page's `Link` header says: whether a `next` page follows (it must be `page + 1`),
/// and the `last` page it advertises.
fn pagination(
    link: Option<&String>,
    scope: Scope<'_>,
    page: usize,
) -> Result<(bool, Option<usize>), String> {
    let Some(link) = link else {
        return Ok((false, None));
    };
    let Scope {
        api,
        expected_path,
        expected_query,
        alias,
    } = scope;
    let mut relations = BTreeSet::new();
    let (mut next, mut last) = (false, None);
    for entry in link.split(',') {
        let (target, relation) = entry
            .trim()
            .split_once(';')
            .ok_or("unrecognized GitHub pagination link")?;
        let url = target
            .strip_prefix('<')
            .and_then(|url| url.strip_suffix('>'))
            .and_then(|url| url.strip_prefix(api))
            .filter(|rest| rest.starts_with('/'))
            .ok_or("GitHub pagination points outside the expected API")?;
        let (path, query) = url
            .split_once('?')
            .ok_or("GitHub pagination has no query")?;
        let mut params = BTreeMap::new();
        for part in query.split('&') {
            let (key, value) = part
                .split_once('=')
                .ok_or("unrecognized GitHub pagination query")?;
            if params.insert(key, value).is_some() {
                return Err("duplicate GitHub pagination query key".into());
            }
        }
        let target_page = params
            .remove("page")
            .filter(|page| !page.is_empty() && page.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|page| page.parse::<usize>().ok())
            .filter(|page| *page != 0)
            .ok_or("invalid GitHub pagination page")?;
        if &params != expected_query {
            return Err("GitHub pagination changed the requested listing scope".into());
        }
        if path != expected_path {
            if !names_repository_by_id(path, expected_path) {
                return Err("GitHub pagination changed the requested listing scope".into());
            }
            match alias {
                Some(seen) if seen != path => {
                    return Err("GitHub pagination changed the requested listing scope".into());
                }
                Some(_) => {}
                None => *alias = Some(path.to_string()),
            }
        }
        let relation = relation
            .trim()
            .strip_prefix("rel=\"")
            .and_then(|relation| relation.strip_suffix('"'))
            .ok_or("unrecognized GitHub pagination relation")?;
        if !relations.insert(relation) {
            return Err("duplicate GitHub pagination relation".into());
        }
        match relation {
            "next" if target_page == page + 1 => next = true,
            "last" => last = Some(target_page),
            "first" if target_page == 1 => {}
            "prev" if page > 1 && target_page == page - 1 => {}
            _ => return Err("inconsistent GitHub pagination".into()),
        }
    }
    Ok((next, last))
}

#[cfg(test)]
#[path = "github_pages_tests.rs"]
mod tests;
