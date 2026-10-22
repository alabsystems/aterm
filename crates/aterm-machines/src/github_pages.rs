// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! GitHub's REST pagination, read to its end before a caller acts on any of it: `aterm init`'s
//! repository inventory (through agh's read door) and the machines controller's key listing
//! (through `port::github_rest`) both list a collection here (docs/DESIGN-init-system-2026-10-20.md
//! S14, §6.1 row 22: no `gh --paginate`).
//!
//! A collection is complete only when every page is: each page is a JSON array within its
//! requested size, every `Link` stays on the API origin with the requested path and query (only
//! `page` moves), a `next` page that answers empty or a `last` that moves is refused, and an
//! exactly full final page is confirmed by the empty page after it rather than read as the end
//! because no `Link` came with it.

use aterm_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// The origin every pagination link must name.
const API: &str = "https://api.github.com";

/// The bound on pages read for one collection.
const MAX_PAGES: usize = 10_000;

/// One answered page: its JSON body and the one header pagination reads (`link`, lowercased).
pub(crate) type Page = (Value, BTreeMap<String, String>);

/// A complete 200 answer as a [`Page`]. Repeated unrelated headers (`Set-Cookie`) are legal;
/// a repeated `Link` is ambiguous, so it is refused.
///
/// # Errors
/// Another status, two `Link` headers, or a body that is not JSON.
pub(crate) fn page(
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

/// Every item of the collection at `base`: an endpoint with a query (`/user/keys?per_page=100`)
/// that holds no `page` key, asked page by page through `get` with `&page=<n>` appended.
/// `per_page` is the size `base` asks for; a larger page is refused.
///
/// # Errors
/// A page `get` could not read whole, or a collection whose pages disagree (above).
pub(crate) fn all(
    base: &str,
    per_page: usize,
    mut get: impl FnMut(&str) -> Result<Page, String>,
) -> Result<Vec<Value>, String> {
    let mut items = Vec::new();
    let mut last = None;
    for page in 1..=MAX_PAGES {
        let (value, headers) = get(&format!("{base}&page={page}"))?;
        let values = value
            .as_array()
            .ok_or("GitHub listing page is not an array")?;
        if values.len() > per_page {
            return Err("GitHub listing page exceeds its requested bound".into());
        }
        let (next, advertised_last) = pagination(headers.get("link"), base, page)?;
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
            // Exactly one full final page is valid. Confirm the next page is empty rather
            // than treating a missing Link header as evidence of completion.
            let (tail, tail_headers) = get(&format!("{base}&page={}", page + 1))?;
            let (tail_next, tail_last) = pagination(tail_headers.get("link"), base, page + 1)?;
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

/// What one page's `Link` header says: whether a `next` page follows (it must be `page + 1`),
/// and the `last` page it advertises.
fn pagination(
    link: Option<&String>,
    base: &str,
    page: usize,
) -> Result<(bool, Option<usize>), String> {
    let Some(link) = link else {
        return Ok((false, None));
    };
    let (expected_path, expected_query) = base
        .split_once('?')
        .ok_or("invalid GitHub listing endpoint")?;
    let expected: BTreeMap<_, _> = expected_query
        .split('&')
        .map(|part| part.split_once('=').ok_or("invalid listing query"))
        .collect::<Result<_, _>>()?;
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
            .and_then(|url| url.strip_prefix(API))
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
            .and_then(|page| page.parse::<usize>().ok())
            .filter(|page| *page != 0)
            .ok_or("invalid GitHub pagination page")?;
        if path != expected_path || params != expected {
            return Err("GitHub pagination changed the requested listing scope".into());
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
            // An extra empty confirmation page can link back to its last real
            // page. The caller validates last against actual page contents.
            "last" => last = Some(target_page),
            "first" if target_page == 1 => {}
            "prev" if page > 1 && target_page == page - 1 => {}
            _ => return Err("inconsistent GitHub pagination".into()),
        }
    }
    Ok((next, last))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "/user/keys?per_page=2";

    fn link(rels: &[(&str, usize)]) -> BTreeMap<String, String> {
        let value = rels
            .iter()
            .map(|(rel, page)| format!("<{API}{BASE}&page={page}>; rel=\"{rel}\""))
            .collect::<Vec<_>>()
            .join(", ");
        BTreeMap::from([("link".to_owned(), value)])
    }

    #[test]
    fn every_page_is_read_and_a_full_final_page_is_confirmed_empty() {
        let mut asked = Vec::new();
        let items = all(BASE, 2, |endpoint| {
            asked.push(endpoint.to_owned());
            Ok(match endpoint.rsplit_once("page=").unwrap().1 {
                "1" => (aterm_json::json!([1, 2]), link(&[("next", 2), ("last", 2)])),
                "2" => (
                    aterm_json::json!([3, 4]),
                    link(&[("prev", 1), ("first", 1)]),
                ),
                _ => (aterm_json::json!([]), link(&[("prev", 2), ("last", 2)])),
            })
        })
        .unwrap();
        assert_eq!(items, [1, 2, 3, 4].map(Value::from));
        assert_eq!(
            asked.len(),
            3,
            "the full final page is confirmed: {asked:?}"
        );
    }

    /// A listing whose first page is `first` and whose second is one item with no `Link`: a
    /// first page that links on to it is read whole as three items, unless a guard refuses it.
    fn listing(first: (Value, BTreeMap<String, String>)) -> Result<Vec<Value>, String> {
        all(BASE, 2, |endpoint| {
            match endpoint.rsplit_once("page=").unwrap().1 {
                "1" => Ok(first.clone()),
                "2" => Ok((aterm_json::json!([3]), BTreeMap::new())),
                page => Err(format!("page {page} was never linked")),
            }
        })
    }

    /// Each guard alone refuses its page, with its own reason: a `next` off the API origin, one
    /// to another listing (another path, another query), one that skips a page, and an empty
    /// page that promises more. Negative control: the same first page linking to the next page
    /// of the same listing is read whole, so a guard that went missing would read it too.
    #[test]
    fn a_link_off_the_api_origin_or_scope_and_an_empty_page_promising_more_are_refused() {
        let header = |value: String| BTreeMap::from([("link".to_owned(), value)]);
        let full = aterm_json::json!([1, 2]);
        assert_eq!(
            listing((full.clone(), link(&[("next", 2)]))).unwrap(),
            [1, 2, 3].map(Value::from)
        );
        for (first, reason) in [
            (
                (
                    full.clone(),
                    header("<https://elsewhere.invalid/user/keys?per_page=2&page=2>; rel=\"next\"".into()),
                ),
                "outside the expected API",
            ),
            (
                (
                    full.clone(),
                    header(
                        "<https://api.github.com.elsewhere.invalid/user/keys?per_page=2&page=2>; rel=\"next\""
                            .into(),
                    ),
                ),
                "outside the expected API",
            ),
            (
                (
                    full.clone(),
                    header(format!("<{API}/user/repos?per_page=2&page=2>; rel=\"next\"")),
                ),
                "changed the requested listing scope",
            ),
            (
                (
                    full.clone(),
                    header(format!("<{API}/user/keys?per_page=100&page=2>; rel=\"next\"")),
                ),
                "changed the requested listing scope",
            ),
            ((full, link(&[("next", 3)])), "inconsistent GitHub pagination"),
            (
                (aterm_json::json!([]), link(&[("next", 2)])),
                "an empty GitHub page advertises more",
            ),
        ] {
            let why = listing(first.clone()).unwrap_err();
            assert!(why.contains(reason), "{first:?}: {why}");
        }
        assert!(
            all(BASE, 2, |_| Ok((
                aterm_json::json!([1, 2, 3]),
                BTreeMap::new()
            )))
            .is_err()
        );
        // Control: the same single short page with no Link is the whole listing.
        assert_eq!(
            all(BASE, 2, |_| Ok((aterm_json::json!([1]), BTreeMap::new()))).unwrap(),
            [Value::from(1)]
        );
    }

    #[test]
    fn a_listing_answer_is_a_200_with_at_most_one_link() {
        assert!(page(302, Vec::new(), b"[]").is_err());
        let links = vec![("Link".into(), "a".into()), ("link".into(), "b".into())];
        assert!(page(200, links, b"[]").is_err());
        let (value, headers) = page(200, vec![("Link".into(), " one ".into())], b"[1]").unwrap();
        assert_eq!(
            (value, headers["link"].as_str()),
            (aterm_json::json!([1]), "one")
        );
    }
}
