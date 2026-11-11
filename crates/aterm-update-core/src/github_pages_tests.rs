// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
    let items = all(API, BASE, 2, |endpoint| {
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
    all(API, BASE, 2, |endpoint| {
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
        all(API, BASE, 2, |_| Ok((
            aterm_json::json!([1, 2, 3]),
            BTreeMap::new()
        )))
        .is_err()
    );
    assert_eq!(
        all(API, BASE, 2, |_| Ok((
            aterm_json::json!([1]),
            BTreeMap::new()
        )))
        .unwrap(),
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

/// A repository's listing linked by id, as GitHub links its releases: every page read under
/// one id. Negative controls, each refused: a second page linking another id, a link to
/// another collection of the repository, and a link by id from a listing that is not a
/// repository's. The loopback origin a fixture serves is as good as `API` when named.
#[test]
fn a_repository_listing_may_link_by_id_but_only_one_id_and_its_own_suffix() {
    const RELEASES: &str = "/repos/o/r/releases?per_page=2";
    let by_id = |id: &str, rest: &str, page: usize, rel: &str| {
        BTreeMap::from([(
            "link".to_owned(),
            format!(
                "<http://127.0.0.1:9/repositories/{id}/{rest}?per_page=2&page={page}>; rel=\"{rel}\""
            ),
        )])
    };
    let listing = |second_link: BTreeMap<String, String>, base: &str| {
        all("http://127.0.0.1:9", base, 2, |endpoint| {
            Ok(match endpoint.rsplit_once("page=").unwrap().1 {
                "1" => (
                    aterm_json::json!([1, 2]),
                    by_id("1000", "releases", 2, "next"),
                ),
                "2" => (aterm_json::json!([3, 4]), second_link.clone()),
                _ => (aterm_json::json!([]), BTreeMap::new()),
            })
        })
    };
    assert_eq!(
        listing(by_id("1000", "releases", 1, "prev"), RELEASES).unwrap(),
        [1, 2, 3, 4].map(Value::from)
    );
    for (second, base) in [
        (by_id("1001", "releases", 1, "prev"), RELEASES),
        (by_id("1000", "tags", 1, "prev"), RELEASES),
    ] {
        let why = listing(second, base).unwrap_err();
        assert!(why.contains("changed the requested listing scope"), "{why}");
    }
    let why = listing(BTreeMap::new(), "/user/keys?per_page=2").unwrap_err();
    assert!(why.contains("changed the requested listing scope"), "{why}");
    let why = all(API, RELEASES, 2, |_| {
        Ok((
            aterm_json::json!([1, 2]),
            by_id("1000", "releases", 2, "next"),
        ))
    })
    .unwrap_err();
    assert!(why.contains("outside the expected API"), "{why}");
}

/// NEGATIVE CONTROL: invalid listing bounds, origins, missing or duplicate base queries,
/// pre-set `page` parameters, and degenerate repository-id paths are refused before any
/// request is sent or link is accepted.
#[test]
fn malformed_listing_bases_and_zero_or_empty_repo_aliases_refuse_before_fetch() {
    for (api, base, per_page) in [
        (API, "/user/keys?per_page=2", 0),
        ("", "/user/keys?per_page=2", 2),
        ("https://api.github.com/", "/user/keys?per_page=2", 2),
        (API, "user/keys?per_page=2", 2),
        (API, "/user/keys", 2),
        (API, "/user/keys?", 2),
        (API, "/user/keys?=2", 2),
        (API, "/user/keys?per_page=2&per_page=10", 2),
        (API, "/user/keys?per_page=2&page=1", 2),
    ] {
        let err = all(api, base, per_page, |_| {
            panic!("malformed base ({api:?}, {base:?}, {per_page}) must not call get")
        })
        .unwrap_err();
        assert!(!err.is_empty());
    }

    assert!(names_repository_by_id(
        "/repositories/1000/releases",
        "/repos/o/r/releases"
    ));
    for (path, expected) in [
        ("/repositories/0/releases", "/repos/o/r/releases"),
        ("/repositories/0100/releases", "/repos/o/r/releases"),
        ("/repositories/1000/releases", "/repos//r/releases"),
        ("/repositories/1000/releases", "/repos/o//releases"),
        ("/repositories/1000/", "/repos/o/r/"),
    ] {
        assert!(
            !names_repository_by_id(path, expected),
            "({path:?}, {expected:?}) must be rejected"
        );
    }
}
