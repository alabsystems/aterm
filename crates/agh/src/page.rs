// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Listings (design §13.1): options, consistency, the result, and the Link walker.

use std::collections::BTreeSet;

use aterm_json::Value;

use crate::client::{Call, Github};
use crate::error::{Error, IdentityRef, Kind, Result, Sub};
use crate::slug::Slug;
use crate::wire::{Endpoints, Origin};

/// Whether a listing is revalidated as one snapshot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Consistency {
    /// The listing is checked to be one state of the collection.
    #[default]
    Snapshot,
    /// The pages as read, unchecked (`--as-read`).
    AsRead,
}

impl Consistency {
    /// `snapshot`, `as-read`.
    #[inline]
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Consistency::Snapshot => "snapshot",
            Consistency::AsRead => "as-read",
        }
    }
}

/// How a listing is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListOpts {
    /// Items per page, at most 100.
    pub per_page: u8,
    /// The page cap.
    pub max_pages: u32,
    /// Snapshot or as read.
    pub consistency: Consistency,
    /// The first N items only.
    pub limit: Option<usize>,
    /// Accept an incomplete listing.
    pub partial: bool,
}

impl ListOpts {
    /// per_page 100, max_pages 20, Snapshot, no limit, not partial.
    pub const STANDARD: ListOpts = ListOpts {
        per_page: 100,
        max_pages: 20,
        consistency: Consistency::Snapshot,
        limit: None,
        partial: false,
    };

    /// What every compare-and-swap read uses: STANDARD.
    pub const CAS: ListOpts = ListOpts::STANDARD;

    /// The same options with `limit`.
    #[inline]
    #[must_use]
    pub const fn with_limit(self, limit: Option<usize>) -> ListOpts {
        ListOpts { limit, ..self }
    }

    /// The same options with `partial`.
    #[inline]
    #[must_use]
    pub const fn with_partial(self, partial: bool) -> ListOpts {
        ListOpts { partial, ..self }
    }

    /// The same options with `max_pages`.
    #[inline]
    #[must_use]
    pub const fn with_max_pages(self, max_pages: u32) -> ListOpts {
        ListOpts { max_pages, ..self }
    }

    /// The same options with `consistency`.
    #[inline]
    #[must_use]
    pub const fn with_consistency(self, consistency: Consistency) -> ListOpts {
        ListOpts {
            consistency,
            ..self
        }
    }

    /// The same options with `per_page`.
    #[inline]
    #[must_use]
    pub const fn with_per_page(self, per_page: u8) -> ListOpts {
        ListOpts { per_page, ..self }
    }
}

/// How a listing ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    /// The whole collection.
    Complete,
    /// Stopped at the page cap.
    Capped {
        /// The pages read.
        pages: u32,
    },
    /// Cut at `--limit`.
    Limited {
        /// The limit.
        at: usize,
    },
}

/// The `--limit` cut, the one rule every caller applies to items already in their final order: `items` cut to `limit`, and `end` `Limited { at: limit }` only when it was Complete and something was cut.
pub(crate) fn cut<T>(end: End, items: &mut Vec<T>, limit: Option<usize>) -> End {
    match limit {
        Some(limit) if items.len() > limit => {
            items.truncate(limit);
            if end == End::Complete {
                End::Limited { at: limit }
            } else {
                end
            }
        }
        _ => end,
    }
}

/// A listing.
#[derive(Clone, Debug, PartialEq)]
pub struct Listing<T> {
    /// The items, in the listing's order.
    pub items: Vec<T>,
    /// The pages read.
    pub pages: u32,
    /// The requests sent.
    pub requests: u32,
    /// How it ended.
    pub end: End,
    /// Whether it was revalidated.
    pub consistency: Consistency,
    /// Who it was read as.
    pub as_: IdentityRef,
}

/// Why a walk stopped at its page cap: `the listing stopped at the {pages}-page cap`.
pub(crate) fn capped_why(pages: u32) -> String {
    format!("the listing stopped at the {pages}-page cap")
}

/// Incomplete, `Sub::Capped { pages }`, why [`capped_why`], next `--max-pages N, or --partial`.
pub(crate) fn capped_error(pages: u32) -> Error {
    Error::new(Kind::Incomplete, capped_why(pages))
        .with_sub(Sub::Capped { pages })
        .with_next("--max-pages N, or --partial")
}

/// Whether `error` is GitHub's result cap: a 422 whose message reads `Only the first N
/// results are available.` (the releases listing, past item 1000) or `Only the first N search
/// results are available` (search).
fn is_result_cap(error: &Error) -> bool {
    let status = error.http.as_ref().map(|http| http.status);
    let message = error.github.as_ref().and_then(|g| g.message.as_deref());
    status == Some(422)
        && message.is_some_and(|m| {
            m.strip_prefix("Only the first ").is_some_and(|rest| {
                let (count, rest) = rest.split_once(' ').unwrap_or((rest, ""));
                !count.is_empty()
                    && count.bytes().all(|b| b.is_ascii_digit())
                    && rest
                        .trim_end_matches('.')
                        .ends_with("results are available")
            })
        })
}

/// A walk refused at GitHub's result cap after `pages` pages: Incomplete, `Sub::Capped
/// { pages }`, the exchange kept, next `--partial` (no `--max-pages` reads past it).
fn result_cap_error(error: Error, pages: u32) -> Error {
    let message = error
        .github
        .as_ref()
        .and_then(|g| g.message.clone())
        .unwrap_or_default();
    error
        .recast(
            Kind::Incomplete,
            format!("the listing stopped after {pages} pages, at GitHub's result cap: {message}"),
        )
        .with_sub(Sub::Capped { pages })
        .with_next("--partial")
}

impl<T> Listing<T> {
    /// Capped → Incomplete, `Sub::Capped { pages }`, why `the listing stopped at the {pages}-page cap`, next `--max-pages N, or --partial`; Complete and Limited → `Ok(self)`.
    /// # Errors
    /// As above.
    pub fn complete(self) -> Result<Listing<T>> {
        match self.end {
            End::Capped { pages } => Err(capped_error(pages)),
            End::Complete | End::Limited { .. } => Ok(self),
        }
    }

    /// Whether it ended `Complete`.
    #[inline]
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        matches!(self.end, End::Complete)
    }

    /// Each item through `f`.
    /// # Errors
    /// The first error `f` returns.
    pub fn map<U>(self, f: impl FnMut(T) -> Result<U>) -> Result<Listing<U>> {
        let items = self.items.into_iter().map(f).collect::<Result<Vec<U>>>()?;
        Ok(Listing {
            items,
            pages: self.pages,
            requests: self.requests,
            end: self.end,
            consistency: self.consistency,
            as_: self.as_,
        })
    }
}

/// A validated next page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NextPage {
    /// The page's path and query, on the api origin.
    pub path_and_query: String,
    /// The page's number.
    pub page: u32,
}

/// Which next links a listing accepts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkScope {
    /// `/repos/{o}/{r}<suffix>` (case-insensitive), or `/repositories/{id}<suffix>` with the repository's id.
    Repo(Slug),
    /// The first path, or `/organizations/{id}<rest>` or `/user/{id}<rest>`, where `rest` is the first path without its first two segments.
    Owner {
        /// The owner's id.
        id: u64,
    },
    /// The first path exactly.
    Exact,
}

/// The params of a `Link` entry: the text up to the first `,` outside quotes (RFC 8288),
/// and the text after that `,`.
fn link_params(text: &str) -> (&str, &str) {
    let mut quoted = false;
    for (i, c) in text.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ',' if !quoted => return (&text[..i], &text[i + 1..]),
            _ => {}
        }
    }
    (text, "")
}

/// The `rel="next"` URL of a `Link` header.
#[must_use]
pub fn link_next(link: &str) -> Option<String> {
    let mut rest = link;
    loop {
        let (_, open) = rest.split_once('<')?;
        let (url, after) = open.split_once('>')?;
        let (params, tail) = link_params(after);
        rest = tail;
        let next = params.split(';').any(|param| {
            let Some((name, value)) = param.trim().split_once('=') else {
                return false;
            };
            name.trim().eq_ignore_ascii_case("rel")
                && value
                    .trim()
                    .trim_matches('"')
                    .split_ascii_whitespace()
                    .any(|rel| rel.eq_ignore_ascii_case("next"))
        });
        if next {
            return Some(url.to_string());
        }
    }
}

/// The refusal of a next link: CouldNotRun, `Sub::Protocol`.
fn refused_link(url: &str, reason: &str) -> Error {
    Error::protocol(format!("GitHub's next link {url} is refused: {reason}"))
}

/// The query parameters GitHub pages a listing by in place of `page` (`/repositories` and
/// `/users` by `since`, webhook deliveries by `cursor`, alert listings by `after` and
/// `before`).
const CURSORS: [&str; 4] = ["since", "cursor", "after", "before"];

/// The refusal of a next link with no usable `page`.
fn unnumbered_link(url: &str, query: &str) -> Error {
    match CURSORS
        .iter()
        .find(|name| query_value(query, name).is_some())
    {
        Some(name) => refused_link(
            url,
            &format!("it names no page: the listing pages by {name}=, which agh does not walk"),
        )
        .with_next(
            "read one page at a time (agh api PATH, without --all), passing the cursor \
             yourself",
        ),
        None => refused_link(url, "it names no page"),
    }
}

/// A path's `/`-separated segments, after its leading `/`.
fn segments(path: &str) -> Vec<&str> {
    path.trim_start_matches('/').split('/').collect()
}

/// The first value a query gives `name`.
fn query_value<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query
        .split('&')
        .map(|pair| pair.split_once('=').unwrap_or((pair, "")))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value)
}

/// Whether a next link's path is one `LinkScope::Repo(slug)` accepts: `/repos/{o}/{r}` with
/// the first path's suffix, or `/repositories/{repo_id}` with it.
fn repo_link(path: &str, first: &str, slug: &Slug, repo_id: Option<u64>) -> Result<(), String> {
    let first = segments(first);
    let Some(suffix) = first.get(3..).filter(|_| first.first() == Some(&"repos")) else {
        return Err("the listing's first path is not under a repository".to_string());
    };
    let next = segments(path);
    match next.first() {
        Some(&"repos") if next.len() >= 3 => {
            let same = next[1].eq_ignore_ascii_case(slug.owner())
                && next[2].eq_ignore_ascii_case(slug.repo())
                && next[3..] == *suffix;
            if same {
                Ok(())
            } else {
                Err(format!("its path is not {slug}'s listing"))
            }
        }
        Some(&"repositories") if next.len() >= 2 => {
            let Some(id) = repo_id else {
                return Err(format!("{slug}'s repository id is not known"));
            };
            if next[1] == id.to_string() && next[2..] == *suffix {
                Ok(())
            } else {
                Err(format!(
                    "its path is not {slug}'s listing (repository id {id})"
                ))
            }
        }
        _ => Err(format!("its path is not {slug}'s listing")),
    }
}

/// Whether a next link's path is one `LinkScope::Owner { id }` accepts: the first path, or
/// `/organizations/{id}` or `/user/{id}` followed by the first path less its first two segments.
fn owner_link(path: &str, first: &str, id: u64) -> Result<(), String> {
    if path == first {
        return Ok(());
    }
    let first = segments(first);
    let next = segments(path);
    let rest = first.get(2..).unwrap_or_default();
    let numbered = next.len() >= 2
        && matches!(next[0], "organizations" | "user")
        && next[1] == id.to_string()
        && next[2..] == *rest;
    if numbered {
        Ok(())
    } else {
        Err(format!("its path is not the listing of owner {id}"))
    }
}

/// Validate a next link (design §13.1): the api origin; a path `scope` accepts (`repo_id` for a numeric repository link, `None` there → Protocol); `page` strictly above `last_page`; `per_page` at most 100.
/// # Errors
/// As above.
pub fn next_link(
    next_url: &str,
    endpoints: &Endpoints,
    scope: &LinkScope,
    repo_id: Option<u64>,
    first_path: &str,
    last_page: u32,
) -> Result<NextPage> {
    let refuse = |reason: &str| refused_link(next_url, reason);
    let Some((origin, path_and_query)) = Origin::parse(next_url) else {
        return Err(refuse("it is not an http or https URL"));
    };
    if &origin != endpoints.api() {
        return Err(refuse(&format!(
            "it leaves the api origin {}",
            endpoints.api().base()
        )));
    }
    let (path, query) = path_and_query
        .split_once('?')
        .unwrap_or((path_and_query.as_str(), ""));
    let first = first_path.split('?').next().unwrap_or_default();
    let scoped = match scope {
        LinkScope::Repo(slug) => repo_link(path, first, slug, repo_id),
        LinkScope::Owner { id } => owner_link(path, first, *id),
        LinkScope::Exact if path == first => Ok(()),
        LinkScope::Exact => Err("its path is not the listing's".to_string()),
    };
    scoped.map_err(|reason| refuse(&reason))?;
    let page = query_value(query, "page")
        .and_then(|v| v.parse::<u32>().ok())
        .ok_or_else(|| unnumbered_link(next_url, query))?;
    if page <= last_page {
        return Err(refuse(&format!(
            "page {page} does not follow page {last_page}"
        )));
    }
    if let Some(per_page) = query_value(query, "per_page") {
        let per_page = per_page
            .parse::<u64>()
            .map_err(|_| refuse("its per_page is not a number"))?;
        if per_page > MAX_PER_PAGE {
            return Err(refuse(&format!(
                "per_page {per_page} is over {MAX_PER_PAGE}"
            )));
        }
    }
    Ok(NextPage {
        path_and_query: path_and_query.clone(),
        page,
    })
}

/// The largest `per_page` GitHub honours.
const MAX_PER_PAGE: u64 = 100;

/// What a walk returns as its items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// The arrays' items, concatenated (`walk`).
    Items,
    /// Each page's body whole (`walk_pages`).
    Pages,
}

/// One page as a pass read it: the call that read it, its validator and, for a Snapshot walk
/// (the one walk `changed` revalidates), its body's stable form, computed once as it is read.
struct PageRead {
    call: Call,
    etag: Option<String>,
    stable: Option<String>,
}

/// One pass over a listing.
struct Pass {
    pages: Vec<PageRead>,
    items: Vec<Value>,
    end: End,
    as_: IdentityRef,
}

/// What a walk carries between its passes.
struct Walk<'a> {
    gh: &'a Github,
    first: Call,
    opts: &'a ListOpts,
    /// The caller's scope, until a page follows a move: then the new name's.
    scope: LinkScope,
    shape: Shape,
    consistency: Consistency,
    /// The repository's id, once a numeric link asked for it.
    repo_id: Option<u64>,
    /// Every request of the walk.
    requests: u32,
}

/// Whether a next link is the numeric `/repositories/{id}` form.
fn numeric_link(url: &str) -> bool {
    Origin::parse(url).is_some_and(|(_, path)| path.starts_with("/repositories/"))
}

/// The fields of a repository object that move with every star, fork, issue and push (design
/// plan §13.1's volatile fields), which a repository listing's stable form leaves out.
const REPOSITORY_VOLATILE: [&str; 10] = [
    "stargazers_count",
    "watchers_count",
    "watchers",
    "forks_count",
    "forks",
    "open_issues_count",
    "open_issues",
    "size",
    "pushed_at",
    "updated_at",
];

/// A page's stable form: `download_count` and the `reactions` rollup removed at the top level
/// of each item (or of the body, when it is an object), `download_count` from every
/// `assets[]` item, and, from an item that is a repository object (it carries `full_name` and
/// `stargazers_count`), its counters and timestamps (`REPOSITORY_VOLATILE`); rendered by
/// aterm-json.
fn stable(mut value: Value) -> String {
    let strip = |item: &mut Value| {
        let Some(object) = item.as_object_mut() else {
            return;
        };
        object.remove("download_count");
        object.remove("reactions");
        if object.contains_key("full_name") && object.contains_key("stargazers_count") {
            for name in REPOSITORY_VOLATILE {
                object.remove(name);
            }
        }
        if let Some(assets) = object.get_mut("assets").and_then(Value::as_array_mut) {
            for asset in assets {
                if let Some(asset) = asset.as_object_mut() {
                    asset.remove("download_count");
                }
            }
        }
    };
    match value.as_array_mut() {
        Some(items) => items.iter_mut().for_each(strip),
        None => strip(&mut value),
    }
    aterm_json::to_string(&value).unwrap_or_default()
}

impl Walk<'_> {
    /// The repository the walk lists, when its scope names one.
    fn repo(&self) -> Option<&Slug> {
        match &self.scope {
            LinkScope::Repo(slug) => Some(slug),
            _ => None,
        }
    }

    /// A page followed a 301 from `old` to `new` (plan §8 step 5): the rest of the walk is the new
    /// name's.
    fn moved(&mut self, old: &Slug, new: &Slug, call: &mut Call, pages: &mut [PageRead]) {
        crate::client::retarget(&mut self.first, old, new);
        crate::client::retarget(call, old, new);
        for page in pages {
            crate::client::retarget(&mut page.call, old, new);
        }
        if matches!(&self.scope, LinkScope::Repo(slug) if slug.key() == old.key()) {
            self.scope = LinkScope::Repo(new.clone());
        }
        self.repo_id = None;
    }

    /// One call, its requests counted.
    fn call(&mut self, call: &Call) -> Result<crate::client::Answered> {
        let answered = self.gh.call(call)?;
        self.requests = self.requests.saturating_add(answered.requests);
        Ok(answered)
    }

    /// Read every page from the first.
    fn pass(&mut self) -> Result<Pass> {
        let mut call = self.first.clone();
        let start = self.first.path_and_query();
        let start_query = start.split_once('?').map_or("", |(_, query)| query);
        let mut page_no = query_value(start_query, "page")
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(1);
        let mut pages: Vec<PageRead> = Vec::new();
        let mut items: Vec<Value> = Vec::new();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut as_: Option<IdentityRef> = None;
        loop {
            let answered = match self.call(&call) {
                Ok(answered) => answered,
                Err(error) if !pages.is_empty() && is_result_cap(&error) => {
                    let read = u32::try_from(pages.len()).unwrap_or(u32::MAX);
                    if self.opts.partial {
                        return Ok(self.ended(pages, items, End::Capped { pages: read }, as_));
                    }
                    return Err(result_cap_error(error, read));
                }
                Err(error) => return Err(error),
            };
            if let Some((old, new)) = &answered.moved {
                self.moved(old, new, &mut call, &mut pages);
            }
            let value = answered.json()?;
            let n = pages.len() + 1;
            let kept = (self.consistency == Consistency::Snapshot).then(|| stable(value.clone()));
            match self.shape {
                Shape::Items => {
                    let Value::Array(array) = value else {
                        return Err(Error::protocol(format!("page {n} is not a JSON array")));
                    };
                    for item in array {
                        if self.consistency == Consistency::AsRead
                            && let Some(id) = item.get("id")
                            && !seen.insert(aterm_json::to_string(id).unwrap_or_default())
                        {
                            continue;
                        }
                        items.push(item);
                    }
                }
                Shape::Pages => items.push(value),
            }
            as_.get_or_insert_with(|| answered.as_.clone());
            let next = answered.header("link").and_then(link_next);
            pages.push(PageRead {
                call: call.clone(),
                etag: answered.etag.clone(),
                stable: kept,
            });
            let read = u32::try_from(pages.len()).unwrap_or(u32::MAX);
            if let Some(limit) = self.opts.limit
                && items.len() >= limit
                && (items.len() > limit || next.is_some())
            {
                items.truncate(limit);
                return Ok(self.ended(pages, items, End::Limited { at: limit }, as_));
            }
            let Some(next) = next else {
                return Ok(self.ended(pages, items, End::Complete, as_));
            };
            if read >= self.opts.max_pages {
                if self.opts.partial {
                    return Ok(self.ended(pages, items, End::Capped { pages: read }, as_));
                }
                return Err(capped_error(read));
            }
            if self.repo_id.is_none()
                && numeric_link(&next)
                && let Some(slug) = self.repo().cloned()
            {
                let (id, sent) = self.gh.repo_id_counted(&slug, &self.first.route)?;
                self.requests = self.requests.saturating_add(sent);
                self.repo_id = Some(id);
            }
            let page = next_link(
                &next,
                self.gh.endpoints(),
                &self.scope,
                self.repo_id,
                &self.first.path,
                page_no,
            )?;
            page_no = page.page;
            call = self.first.clone();
            call.path = page.path_and_query;
            call.query = Vec::new();
            call.if_none_match = None;
        }
    }

    /// A pass that ended.
    fn ended(
        &self,
        pages: Vec<PageRead>,
        items: Vec<Value>,
        end: End,
        as_: Option<IdentityRef>,
    ) -> Pass {
        Pass {
            pages,
            items,
            end,
            as_: as_.unwrap_or(IdentityRef::Anonymous),
        }
    }

    /// The first page that changed since `pass` read it (1-based), each page requested again
    /// with `If-None-Match`: a 304 is unchanged, and a 200 is compared on its stable form.
    fn changed(&mut self, pass: &Pass) -> Result<Option<u32>> {
        for (i, page) in pass.pages.iter().enumerate() {
            let call = page.call.clone().if_none_match(page.etag.clone());
            let answered = self.call(&call)?;
            if answered.status == 304 {
                continue;
            }
            if page.stable.as_deref() != Some(stable(answered.json()?).as_str()) {
                return Ok(Some(u32::try_from(i + 1).unwrap_or(u32::MAX)));
            }
        }
        Ok(None)
    }

    /// The walk: one pass, then, for a Snapshot of more than one page, the revalidation, one
    /// rescan on a change, and Torn on a second.
    fn run(mut self) -> Result<Listing<Value>> {
        let read = self.read();
        let repo = self.repo().cloned();
        read.map_err(|error| match repo {
            Some(slug) if error.repo.is_none() => error.with_repo(slug.to_string()),
            _ => error,
        })
    }

    /// `run`'s walk.
    fn read(&mut self) -> Result<Listing<Value>> {
        let mut pass = self.pass()?;
        if self.consistency == Consistency::Snapshot
            && pass.pages.len() > 1
            && self.changed(&pass)?.is_some()
        {
            pass = self.pass()?;
            if let Some(page) = self.changed(&pass)? {
                return Err(Error::new(
                    Kind::Incomplete,
                    format!("the listing changed twice while it was read (page {page})"),
                )
                .with_sub(Sub::Torn { page })
                .with_next("run it again"));
            }
        }
        Ok(Listing {
            pages: u32::try_from(pass.pages.len()).unwrap_or(u32::MAX),
            items: pass.items,
            requests: self.requests,
            end: pass.end,
            consistency: self.consistency,
            as_: pass.as_,
        })
    }
}

/// The walk of `walk` and `walk_pages`.
fn walk_as(
    gh: &Github,
    first: Call,
    opts: &ListOpts,
    scope: &LinkScope,
    shape: Shape,
) -> Result<Listing<Value>> {
    let too_many = || Error::usage(format!("per_page is at most {MAX_PER_PAGE}"));
    if u64::from(opts.per_page) > MAX_PER_PAGE {
        return Err(too_many());
    }
    let path_query = first.path.split_once('?').map_or("", |(_, query)| query);
    let named = first
        .query
        .iter()
        .find(|(key, _)| key == "per_page")
        .map(|(_, value)| value.as_str())
        .or_else(|| query_value(path_query, "per_page"));
    let first = match named {
        Some(value) if value.parse::<u64>().is_ok_and(|n| n <= MAX_PER_PAGE) => first,
        Some(_) => return Err(too_many()),
        None => first.query("per_page", opts.per_page.to_string()),
    };
    let consistency = match shape {
        Shape::Items => opts.consistency,
        Shape::Pages => Consistency::AsRead,
    };
    Walk {
        gh,
        first,
        opts,
        scope: scope.clone(),
        shape,
        consistency,
        repo_id: None,
        requests: 0,
    }
    .run()
}

/// Walk an array listing to its end (design §13.1): 1. the first call gets `per_page` (`opts.per_page`, unless its query names one; over 100 → Usage `per_page is at most 100`); 2. each page is a `gh.call`, whose body must be a JSON array (else CouldNotRun `Protocol`, why `page {n} is not a JSON array`), and whose `Link` next goes through `next_link`: a numeric `/repositories/{id}` link asks `gh.repo_id` once, through the memo; 3. at `max_pages` with a next link: `partial` → `End::Capped { pages }`; otherwise Incomplete `Capped { pages }`, next `--max-pages N, or --partial`; a page reached by a next link that GitHub answers 422 `Only the first N results are available.` (its result cap, which the releases listing has at 1000) ends the walk the same way: `partial` → `End::Capped { pages }` (the pages read); otherwise Incomplete `Capped { pages }`, the exchange kept, next `--partial`; 4. a limit stops once `limit` items are held and more remain: `End::Limited`; 5.
/// # Errors
/// The first page's error; Incomplete (cap, torn); Protocol (a refused link).
pub fn walk(
    gh: &Github,
    first: Call,
    opts: &ListOpts,
    scope: &LinkScope,
) -> Result<Listing<Value>> {
    walk_as(gh, first, opts, scope, Shape::Items)
}

/// Walk any listing, returning each page's body whole (`api --all`): AsRead, no merging.
/// # Errors
/// As `walk`.
pub fn walk_pages(
    gh: &Github,
    first: Call,
    opts: &ListOpts,
    scope: &LinkScope,
) -> Result<Listing<Value>> {
    walk_as(gh, first, opts, scope, Shape::Pages)
}
