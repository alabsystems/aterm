// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A composed table: one long-tier row, one flat row, one the build
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

/// A build's bytes: minified code around the catalog literal, written as
/// the 2.1.301 bundle writes it (bare keys, `!0`, a long-prompt tier),
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

/// THE CHECK, id by id: a row the build prices alike is `same` (the
/// long-prompt tier and its threshold too), one the build lacks is
/// `table-only`, one the table lacks `build-only`, and an OpenAI row is
/// not compared. Negative control: the build's opus output at $30 against
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

/// A LONG-PROMPT TIER is compared by its threshold and its classes, and a
/// tier one side has and the other lacks differs. Negative control: the
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

/// A BUILD THIS READER DOES NOT KNOW is refused, never half-read: no
/// catalog, two catalogs, a literal it cannot read, or one that prices no
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

/// A Codex build's bytes: its model catalog as 0.163.0 embeds it (pretty
/// JSON, `"models": [` after the object's brace, escapes in its text,
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

/// THE OPENAI ROWS, by id against a Codex build's model catalog: a slug
/// the table prices is `priced`, one it does not `unpriced` (its tokens
/// show unpriced: the page lists no price for it), and an OpenAI row the
/// catalog does not name `table-only`; no Anthropic row is looked at.
/// Negative control: the table without the `gpt-6-sol` row leaves that
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

/// A CODEX BUILD THIS READER DOES NOT KNOW is refused, never half-read:
/// no catalog, two, one whose `models` opens no object, or one naming no
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
