// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The output door over hand-built Outcomes (the plan's §9.1, §9.4): projections, formats,
//! the answers, the receipt, failing writers and the terminal. No client: every Outcome is
//! made here, and only the captured streams are read.

use std::io::{Error as IoError, ErrorKind, Write};
use std::path::PathBuf;
use std::sync::Arc;

use agh::cli::args::Format;
use agh::cli::out::{OutputOpts, emit};
use agh::cli::project;
use agh::cli::verbs::Noun;
use agh::cli::{Answer, Captured, Completeness, Ctx, FileWritten, Outcome, Receipt};
use agh::download::DigestCheck;
use agh::error::{Error, Kind};
use agh::model::Sha256Digest;
use agh::page::Consistency;
use agh::rate::RateSnapshot;
use agh::wire::Endpoints;
use agh::wire::scripted::ScriptedTransport;
use aterm_json::{Value, json};

use crate::common;

/// A context with captured streams; its scratch root lives as long as the test.
fn context() -> (aterm_tempfile::TempDir, Ctx, Captured) {
    let root = common::scratch("output");
    let (ctx, captured) = Ctx::for_tests(
        root.path(),
        Arc::new(ScriptedTransport::new()),
        Endpoints::github_com(),
        common::env(),
    );
    (root, ctx, captured)
}

/// What one emit wrote: the exit, stdout and stderr.
struct Emitted {
    exit: u8,
    stdout: String,
    stderr: String,
}

fn emit_in(ctx: &mut Ctx, captured: &Captured, outcome: Outcome, opts: &OutputOpts) -> Emitted {
    let exit = emit(ctx, outcome, opts);
    Emitted {
        exit,
        stdout: captured.stdout_text(),
        stderr: captured.stderr_text(),
    }
}

fn emit_with(outcome: Outcome, opts: &OutputOpts) -> Emitted {
    let (_root, mut ctx, captured) = context();
    emit_in(&mut ctx, &captured, outcome, opts)
}

fn outcome(answer: Answer) -> Outcome {
    Outcome {
        answer,
        receipt: Receipt::new("release.view"),
    }
}

fn object(noun: Noun, value: Value) -> Outcome {
    outcome(Answer::Object { noun, value })
}

fn items(noun: Noun, items: Vec<Value>) -> Outcome {
    outcome(Answer::Items {
        noun,
        items,
        completeness: Completeness::Complete,
    })
}

fn fields(text: &str) -> OutputOpts {
    OutputOpts {
        fields: Some(text.to_string()),
        ..OutputOpts::default()
    }
}

fn parse(text: &str) -> Value {
    aterm_json::from_str(text.trim_end()).unwrap_or_else(|e| panic!("{text:?}: {e}"))
}

/// The keys of an object, sorted.
fn keys(value: &Value) -> Vec<String> {
    value
        .as_object()
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default()
}

/// The error object of a failure's second stderr line.
fn fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/jq/inputs")
        .join(name);
    aterm_json::from_slice(&std::fs::read(path).expect("the fixture")).expect("JSON")
}

fn release() -> Value {
    let mut value = fixture("release.json");
    let map = value.as_object_mut().expect("an object");
    map.insert("latest".into(), Value::Bool(true));
    map.insert("repo".into(), Value::from("alabsystems/aterm"));
    value
}

// ---------------------------------------------------------------- the default projections

#[test]
fn release_default_projection() {
    let out = emit_with(object(Noun::Release, release()), &OutputOpts::default());
    assert_eq!(out.exit, 0);
    let printed = parse(&out.stdout);
    assert_eq!(
        keys(&printed),
        [
            "assets",
            "created_at",
            "draft",
            "html_url",
            "id",
            "immutable",
            "latest",
            "name",
            "prerelease",
            "published_at",
            "repo",
            "tag_name",
            "target_commitish"
        ]
    );
    let asset = &printed["assets"][0];
    assert_eq!(
        keys(asset),
        [
            "content_type",
            "digest",
            "id",
            "name",
            "size",
            "state",
            "updated_at"
        ]
    );
    assert_eq!(asset["name"], Value::from("SHA256SUMS"));
    assert_eq!(printed["repo"], Value::from("alabsystems/aterm"));
    assert!(out.stderr.is_empty(), "silent on success: {}", out.stderr);
}

#[test]
fn release_list_default_projection() {
    let rows = fixture("release_list.json")
        .as_array()
        .cloned()
        .expect("an array");
    let out = emit_with(items(Noun::ReleaseList, rows), &OutputOpts::default());
    let printed = parse(&out.stdout);
    let first = &printed[0];
    assert_eq!(
        keys(first),
        [
            "assets",
            "draft",
            "latest",
            "prerelease",
            "published_at",
            "tag_name"
        ]
    );
    assert_eq!(
        first["latest"],
        Value::Null,
        "a context field the verb did not set"
    );
    assert_eq!(first["assets"][0], json!({"name": "aterm-appcast.toml"}));
    assert_eq!(printed.as_array().map(Vec::len), Some(4));
}

#[test]
fn commit_derives_date_subject_and_author() {
    let out = emit_with(
        object(Noun::Commit, fixture("commit.json")),
        &OutputOpts::default(),
    );
    assert_eq!(
        parse(&out.stdout),
        json!({
            "author": "fixture-dev-bot",
            "date": "2026-10-18T12:05:00Z",
            "sha": "0123456789abcdef0123456789abcdef01234567",
            "subject": "packs: read the index once"
        })
    );
    let commits = fixture("commits.json").as_array().cloned().expect("array");
    let out = emit_with(items(Noun::Commit, commits), &OutputOpts::default());
    let printed = parse(&out.stdout);
    assert_eq!(
        printed[1]["author"],
        Value::from("Fixture Ext"),
        "no login: the name"
    );
}

#[test]
fn branch_tag_and_compare_derive_their_shas() {
    let branches = fixture("branches.json").as_array().cloned().expect("array");
    let out = emit_with(
        items(Noun::Branch, branches.clone()),
        &OutputOpts::default(),
    );
    assert_eq!(
        parse(&out.stdout)[0],
        json!({"name": "main", "sha": "0123456789abcdef0123456789abcdef01234567"})
    );
    let out = emit_with(items(Noun::Tag, branches), &OutputOpts::default());
    assert_eq!(keys(&parse(&out.stdout)[2]), ["name", "sha"]);

    let mut compare = fixture("compare.json");
    let map = compare.as_object_mut().expect("object");
    map.insert("base".into(), Value::from("main"));
    map.insert("head".into(), Value::from("parity_journal"));
    let out = emit_with(object(Noun::Compare, compare), &OutputOpts::default());
    assert_eq!(
        parse(&out.stdout),
        json!({
            "ahead_by": 3,
            "base": "main",
            "behind_by": 1,
            "head": "parity_journal",
            "merge_base": "fedcba9876543210fedcba9876543210fedcba98",
            "status": "diverged",
            "total_commits": 3
        })
    );
}

#[test]
fn repo_asset_entry_tree_and_user_projections() {
    let out = emit_with(
        object(Noun::Repo, fixture("repo.json")),
        &OutputOpts::default(),
    );
    let printed = parse(&out.stdout);
    assert_eq!(printed["parent"], json!({"full_name": "fixture-ext/packs"}));
    assert_eq!(
        keys(&printed),
        [
            "default_branch",
            "description",
            "fork",
            "full_name",
            "parent",
            "private",
            "pushed_at",
            "visibility"
        ]
    );

    let assets = release()["assets"].as_array().cloned().expect("assets");
    let out = emit_with(items(Noun::Asset, assets), &OutputOpts::default());
    assert_eq!(
        keys(&parse(&out.stdout)[0]),
        [
            "content_type",
            "digest",
            "id",
            "name",
            "size",
            "state",
            "updated_at"
        ]
    );

    let entries = fixture("contents_dir.json")
        .as_array()
        .cloned()
        .expect("array");
    let out = emit_with(items(Noun::ContentEntry, entries), &OutputOpts::default());
    assert_eq!(
        keys(&parse(&out.stdout)[0]),
        ["name", "sha", "size", "type"]
    );

    let tree = fixture("tree.json")["tree"]
        .as_array()
        .cloned()
        .expect("tree");
    let out = emit_with(items(Noun::TreeEntry, tree), &OutputOpts::default());
    let printed = parse(&out.stdout);
    assert_eq!(keys(&printed[0]), ["path", "size", "type"]);
    assert_eq!(printed[1]["size"], Value::Null, "a tree has no size");

    let out = emit_with(
        object(Noun::User, fixture("user.json")),
        &OutputOpts::default(),
    );
    assert_eq!(keys(&parse(&out.stdout)), ["id", "login", "name", "type"]);
}

#[test]
fn whole_value_nouns_print_whole() {
    let value = json!({"decision": {"use": "dev"}, "op": "read", "z": [1, 2]});
    for noun in [Noun::Route, Noun::Raw, Noun::Wait, Noun::Meta, Noun::Policy] {
        let out = emit_with(object(noun, value.clone()), &OutputOpts::default());
        assert_eq!(parse(&out.stdout), value, "{noun:?}");
    }
}

#[test]
fn context_nouns_project_their_own_fields() {
    let identity = json!({
        "profile": "dev", "tier": "dev", "source": "file", "presence": "present", "why": null,
        "fingerprint": "sha256:00", "kind": "classic", "login": "fixture-dev-bot",
        "remedy": null, "extra": 1
    });
    let out = emit_with(
        items(Noun::Identity, vec![identity]),
        &OutputOpts::default(),
    );
    let printed = parse(&out.stdout);
    assert_eq!(
        keys(&printed[0]),
        [
            "fingerprint",
            "kind",
            "login",
            "presence",
            "profile",
            "remedy",
            "source",
            "tier",
            "why"
        ]
    );

    let rate = json!({
        "as": "dev", "source": "file", "resource": "core", "limit": 5000, "remaining": 4999,
        "reset": 1_700_000_000u64, "used": 1, "extra": 1
    });
    let out = emit_with(items(Noun::Rate, vec![rate]), &OutputOpts::default());
    assert_eq!(
        keys(&parse(&out.stdout)[0]),
        [
            "as",
            "limit",
            "remaining",
            "reset",
            "resource",
            "source",
            "used"
        ]
    );

    let permission = json!({
        "repo": "alabsystems/aterm", "as": "dev", "op": "pull", "granted": true,
        "permissions": {"pull": true, "push": false}, "extra": 1
    });
    let out = emit_with(object(Noun::Permission, permission), &OutputOpts::default());
    assert_eq!(
        keys(&parse(&out.stdout)),
        ["as", "granted", "op", "permissions", "repo"]
    );
}

#[test]
fn whole_value_rows_that_are_not_objects_are_their_own_cell() {
    let tsv = OutputOpts {
        format: Format::Tsv,
        header: true,
        ..OutputOpts::default()
    };
    let text = OutputOpts {
        format: Format::Text,
        ..OutputOpts::default()
    };
    let rows = || items(Noun::Raw, vec![json!("a"), json!("b")]);
    let out = emit_with(rows(), &tsv);
    assert_eq!((out.exit, out.stdout.as_str()), (0, ".\na\nb\n"));
    let out = emit_with(rows(), &text);
    assert_eq!((out.exit, out.stdout.as_str()), (0, ".\na\nb\n"));
    let out = emit_with(object(Noun::Raw, json!("x")), &tsv);
    assert_eq!((out.exit, out.stdout.as_str()), (0, ".\nx\n"));
    let out = emit_with(object(Noun::Raw, json!(["a", "b"])), &text);
    assert_eq!((out.exit, out.stdout.as_str()), (0, ".\na\nb\n"));
    let mixed = items(Noun::Raw, vec![json!({"k": 1}), json!(2)]);
    let out = emit_with(mixed, &tsv);
    assert_eq!(out.stdout, "k\t.\n1\t{\"k\":1}\n\t2\n");
}

#[test]
fn a_whole_array_prints_its_elements_as_rows() {
    let table = json!([
        {"exit": 0, "slug": "ok"},
        {"exit": 1, "slug": "refused"}
    ]);
    let tsv = OutputOpts {
        format: Format::Tsv,
        header: true,
        ..OutputOpts::default()
    };
    let out = emit_with(object(Noun::Meta, table.clone()), &tsv);
    assert_eq!(
        (out.exit, out.stdout.as_str()),
        (0, "exit\tslug\n0\tok\n1\trefused\n")
    );
    let text = OutputOpts {
        format: Format::Text,
        ..OutputOpts::default()
    };
    let out = emit_with(object(Noun::Meta, table.clone()), &text);
    assert_eq!(out.stdout, "exit  slug\n0     ok\n1     refused\n");
    let out = emit_with(object(Noun::Meta, table.clone()), &OutputOpts::default());
    assert_eq!(parse(&out.stdout), table, "json prints the array whole");
    let ndjson = OutputOpts {
        format: Format::Ndjson,
        ..OutputOpts::default()
    };
    let out = emit_with(object(Noun::Meta, table.clone()), &ndjson);
    assert_eq!(
        out.stdout.lines().count(),
        1,
        "ndjson: one answer, one line"
    );
    assert_eq!(parse(&out.stdout), table, "ndjson prints the array whole");
    let out = emit_with(
        object(Noun::Meta, table.clone()),
        &format(Format::Tsv, false, "slug"),
    );
    assert_eq!(out.stdout, "ok\nrefused\n");
    let out = emit_with(object(Noun::Meta, table), &fields("nosuch"));
    assert_eq!(out.exit, 2);
    let why = common::error_json(&out.stderr)["why"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(why.contains("exit") && why.contains("slug"), "{why}");
}

#[test]
fn an_output_usage_error_names_the_verb_page() {
    let next = |opts: &OutputOpts| {
        let out = emit_with(object(Noun::Release, release()), opts);
        assert_eq!(out.exit, 2, "{opts:?}");
        common::error_json(&out.stderr)["next"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    };
    assert_eq!(next(&fields("nosuch")), "agh help release view");
    assert_eq!(next(&fields("a-b")), "agh help release view");
    let raw = OutputOpts {
        raw: true,
        ..OutputOpts::default()
    };
    assert_eq!(next(&raw), "agh help release view");
    let jq = OutputOpts {
        jq: Some("(".into()),
        ..OutputOpts::default()
    };
    assert_eq!(next(&jq), "agh help jq", "a jq error keeps its own page");
}

// ---------------------------------------------------------------- --fields

#[test]
fn fields_that_share_a_prefix_merge() {
    let out = emit_with(
        object(Noun::Release, release()),
        &fields("tag_name, assets[].name,assets[].size"),
    );
    let printed = parse(&out.stdout);
    assert_eq!(keys(&printed), ["assets", "tag_name"]);
    assert_eq!(
        printed["assets"][0],
        json!({"name": "SHA256SUMS", "size": 512})
    );
}

#[test]
fn a_missing_path_is_null() {
    let out = emit_with(
        object(Noun::Release, release()),
        &fields("assets[].nope,author.login,author.nope.deeper,tag_name.x"),
    );
    let printed = parse(&out.stdout);
    assert_eq!(printed["assets"][0], json!({"nope": null}));
    assert_eq!(
        printed["author"],
        json!({"login": "fixture-release-bot", "nope": null})
    );
    assert_eq!(printed["tag_name"], Value::Null, "a key step on a string");

    let rows = vec![json!({"name": "a", "x": 1}), json!({"name": "b"})];
    let out = emit_with(items(Noun::Asset, rows), &fields("name,x"));
    assert_eq!(
        parse(&out.stdout),
        json!([{"name": "a", "x": 1}, {"name": "b", "x": null}])
    );
}

#[test]
fn an_unknown_field_is_usage_listing_the_fields() {
    let out = emit_with(object(Noun::Release, release()), &fields("tag_name,nosuch"));
    assert_eq!(out.exit, 2);
    assert!(out.stdout.is_empty(), "nothing on stdout: {}", out.stdout);
    let error = common::error_json(&out.stderr);
    assert_eq!(error["kind"], Value::from("usage"));
    let why = error["why"].as_str().expect("why");
    assert!(why.contains("nosuch"), "{why}");
    for name in ["assets", "author", "body", "latest", "repo", "tag_name"] {
        assert!(why.contains(name), "{why} lists {name}");
    }
}

#[test]
fn the_names_list_is_the_schema_and_the_sample_sorted_once() {
    // release.json holds neither `latest` nor `repo`, two names of the Release schema.
    let sample = fixture("release.json");
    assert!(sample.get("latest").is_none() && sample.get("repo").is_none());
    let out = emit_with(object(Noun::Release, sample.clone()), &fields("nosuch"));
    assert_eq!(out.exit, 2);
    let why = common::error_json(&out.stderr)["why"]
        .as_str()
        .expect("why")
        .to_string();
    let listed: Vec<&str> = why
        .split_once("fields: ")
        .map(|(_, list)| list.split(", ").collect())
        .unwrap_or_default();
    let mut expected: Vec<String> = project::default_fields(Noun::Release)
        .iter()
        .chain(project::derived_fields(Noun::Release))
        .filter_map(|text| text.split(['.', '[']).next())
        .map(str::to_string)
        .chain(keys(&sample))
        .collect();
    expected.sort();
    expected.dedup();
    assert_eq!(listed, expected, "{why}");
    assert!(listed.contains(&"latest"), "a schema-only name: {why}");
}

#[test]
fn a_schema_name_the_value_lacks_projects_to_null() {
    // A repository that is not a fork has no `parent`; `parent.full_name` is a default field.
    let mut repo = fixture("repo.json");
    repo.as_object_mut().expect("an object").remove("parent");
    let out = emit_with(object(Noun::Repo, repo), &fields("parent.full_name"));
    assert_eq!(out.exit, 0, "{}", out.stderr);
    assert_eq!(out.stdout, "{\"parent\":null}\n");
}

#[test]
fn with_no_items_any_field_passes() {
    let out = emit_with(items(Noun::Asset, Vec::new()), &fields("anything,at.all"));
    assert_eq!(out.exit, 0);
    assert_eq!(out.stdout, "[]\n");
}

#[test]
fn a_bad_field_path_is_usage() {
    for text in ["", "a,,b", "a.", "[]", "a[0]", "a-b"] {
        let out = emit_with(object(Noun::Release, release()), &fields(text));
        assert_eq!(out.exit, 2, "{text:?}");
        assert!(out.stdout.is_empty());
    }
}

// ---------------------------------------------------------------- --raw

#[test]
fn raw_wants_exactly_one_field() {
    let raw = |fields: Option<&str>| OutputOpts {
        raw: true,
        fields: fields.map(str::to_string),
        ..OutputOpts::default()
    };
    let none = emit_with(object(Noun::Release, release()), &raw(None));
    assert_eq!(none.exit, 2);
    assert!(
        common::error_json(&none.stderr)["why"]
            .as_str()
            .unwrap_or_default()
            .contains("--raw")
    );
    let two = emit_with(object(Noun::Release, release()), &raw(Some("tag_name,id")));
    assert_eq!(two.exit, 2);
    let one = emit_with(object(Noun::Release, release()), &raw(Some("tag_name")));
    assert_eq!((one.exit, one.stdout.as_str()), (0, "v0.97.0\n"));
    let list = emit_with(
        object(Noun::Release, release()),
        &raw(Some("assets[].name")),
    );
    assert_eq!(
        list.stdout,
        "SHA256SUMS,aterm-0.97.0.dmg,aterm-appcast.toml\n"
    );
    let rows = release()["assets"].as_array().cloned().expect("assets");
    let each = emit_with(items(Noun::Asset, rows), &raw(Some("id")));
    assert_eq!(each.stdout, "300000201\n300000202\n300000203\n");
}

// ---------------------------------------------------------------- --format

fn format(format: Format, header: bool, fields: &str) -> OutputOpts {
    OutputOpts {
        format,
        header,
        fields: Some(fields.to_string()),
        ..OutputOpts::default()
    }
}

#[test]
fn tsv_escapes_as_jq_does() {
    let row = json!({
        "s": "a\tb\nc\\d\re",
        "nul": "a\u{0}b",
        "n": 1.5,
        "b": true,
        "z": null,
        "list": ["x", 2, null, [3, 4]],
        "obj": {"k": "v"}
    });
    let out = emit_with(
        items(Noun::Asset, vec![row.clone()]),
        &format(Format::Tsv, false, "s,n,b,z,list,obj"),
    );
    assert_eq!(
        out.stdout,
        "a\\tb\\nc\\\\d\\re\t1.5\ttrue\t\tx,2,,3,4\t{\"k\":\"v\"}\n"
    );
    let out = emit_with(
        items(Noun::Asset, vec![row]),
        &format(Format::Tsv, false, "nul"),
    );
    assert_eq!(
        out.stdout, "a\\0b\n",
        "a NUL is written \\0, as jq writes it"
    );
    let out = emit_with(
        items(
            Noun::Asset,
            vec![json!({"name": "x"}), json!({"name": "y"})],
        ),
        &format(Format::Tsv, true, "name,size"),
    );
    assert_eq!(out.stdout, "name\tsize\nx\t\ny\t\n");
}

#[test]
fn tsv_takes_the_default_columns() {
    let entries = fixture("contents_dir.json")
        .as_array()
        .cloned()
        .expect("array");
    let out = emit_with(
        items(Noun::ContentEntry, entries),
        &OutputOpts {
            format: Format::Tsv,
            header: true,
            ..OutputOpts::default()
        },
    );
    let first_two: Vec<&str> = out.stdout.lines().take(2).collect();
    assert_eq!(
        first_two,
        [
            "type\tname\tsize\tsha",
            "file\tCargo.toml\t1834\ta1b2c3d4e5f60718293a4b5c6d7e8f9012345678"
        ]
    );
}

#[test]
fn ndjson_is_one_value_per_line() {
    let rows = vec![
        json!({"name": "a", "size": 1}),
        json!({"name": "b", "size": 2}),
    ];
    let out = emit_with(
        items(Noun::Asset, rows),
        &format(Format::Ndjson, false, "name"),
    );
    assert_eq!(out.stdout, "{\"name\":\"a\"}\n{\"name\":\"b\"}\n");
    let out = emit_with(
        object(Noun::Release, release()),
        &format(Format::Ndjson, false, "id"),
    );
    assert_eq!(out.stdout, "{\"id\":190000101}\n");
}

#[test]
fn text_pads_columns_for_a_person() {
    let rows = vec![
        json!({"name": "SHA256SUMS", "size": 512, "note": null}),
        json!({"name": "a", "size": 48_123_456, "note": "tab\there"}),
    ];
    let out = emit_with(
        items(Noun::Asset, rows),
        &format(Format::Text, false, "name,size,note"),
    );
    assert_eq!(
        out.stdout,
        "name        size      note\n\
         SHA256SUMS  512\n\
         a           48123456  tab\\there\n"
    );
}

#[test]
fn json_is_an_array_for_a_list_and_a_value_for_an_object() {
    let out = emit_with(items(Noun::Asset, Vec::new()), &OutputOpts::default());
    assert_eq!(out.stdout, "[]\n");
    let out = emit_with(object(Noun::Release, release()), &fields("id"));
    assert_eq!(out.stdout, "{\"id\":190000101}\n");
}

// ---------------------------------------------------------------- --jq

#[test]
fn jq_reads_github_object_plus_derived_names() {
    let opts = OutputOpts {
        jq: Some(".author.login, .subject, .date".to_string()),
        ..OutputOpts::default()
    };
    let out = emit_with(object(Noun::Commit, fixture("commit.json")), &opts);
    assert_eq!(
        out.stdout,
        "fixture-dev-bot\npacks: read the index once\n2026-10-18T12:05:00Z\n"
    );
}

#[test]
fn jq_runs_on_the_projection_under_fields() {
    let opts = OutputOpts {
        jq: Some(".author".to_string()),
        fields: Some("author".to_string()),
        ..OutputOpts::default()
    };
    let out = emit_with(object(Noun::Commit, fixture("commit.json")), &opts);
    assert_eq!(out.stdout, "fixture-dev-bot\n");
}

#[test]
fn jq_runs_once_over_a_list() {
    let rows = fixture("branches.json").as_array().cloned().expect("array");
    let opts = OutputOpts {
        jq: Some("length, .[0].sha".to_string()),
        ..OutputOpts::default()
    };
    let out = emit_with(items(Noun::Branch, rows), &opts);
    assert_eq!(out.stdout, "3\n0123456789abcdef0123456789abcdef01234567\n");
}

#[test]
fn a_jq_error_is_usage_with_nothing_on_stdout() {
    for program in [".tag_name.x", "reduce .[] as $x (0; .)", ".["] {
        let opts = OutputOpts {
            jq: Some(program.to_string()),
            ..OutputOpts::default()
        };
        let out = emit_with(object(Noun::Release, release()), &opts);
        assert_eq!(out.exit, 2, "{program}");
        assert!(out.stdout.is_empty(), "{program}: {}", out.stdout);
        assert_eq!(
            common::error_json(&out.stderr)["kind"],
            Value::from("usage")
        );
    }
}

#[test]
fn a_bad_jq_or_fields_is_refused_before_the_verb_sends_anything() {
    let repo = aterm_json::to_string(&fixture("repo.json")).expect("JSON");
    for (flag, value) in [("--jq", ".["), ("--fields", "a,,b"), ("--raw", "")] {
        let root = common::scratch("output-early");
        let transport = Arc::new(ScriptedTransport::new());
        transport.answer(
            200,
            &[("content-type", "application/json")],
            repo.as_bytes(),
        );
        let (mut ctx, captured) = Ctx::for_tests(
            root.path(),
            transport.clone(),
            Endpoints::github_com(),
            common::env(),
        );
        let mut args = vec!["repo", "view", "fixture-ext/packs", flag];
        if !value.is_empty() {
            args.push(value);
        }
        let exit = common::run_cli(&mut ctx, &args);
        assert_eq!(exit, 2, "{flag} {value}: {}", captured.stderr_text());
        assert!(captured.stdout_text().is_empty(), "{flag}");
        assert!(
            transport.requests().is_empty(),
            "{flag} {value}: the verb sent {:?}",
            transport.requests()
        );
    }
}

// ---------------------------------------------------------------- the other answers

#[test]
fn bytes_print_exactly() {
    let bytes = b"\x00raw\r\nbytes\xff".to_vec();
    let out = emit_with(
        outcome(Answer::Bytes(bytes.clone())),
        &OutputOpts::default(),
    );
    assert_eq!(out.exit, 0);
    let (_root, mut ctx, captured) = context();
    emit(
        &mut ctx,
        outcome(Answer::Bytes(bytes.clone())),
        &OutputOpts::default(),
    );
    assert_eq!(captured.stdout(), bytes);
}

#[test]
fn bytes_refuse_the_projection_flags() {
    let cases = [
        fields("a"),
        OutputOpts {
            jq: Some(".".into()),
            ..OutputOpts::default()
        },
        OutputOpts {
            format: Format::Tsv,
            ..OutputOpts::default()
        },
    ];
    for opts in cases {
        let out = emit_with(outcome(Answer::Bytes(b"x".to_vec())), &opts);
        assert_eq!(out.exit, 2, "{opts:?}");
        assert!(out.stdout.is_empty());
        let error = common::error_json(&out.stderr);
        assert_eq!(error["kind"], Value::from("usage"));
        assert!(
            error["why"]
                .as_str()
                .unwrap_or_default()
                .contains("release.view"),
            "the verb is named"
        );
    }
}

fn file(name: &str, written: bool, digest: DigestCheck) -> FileWritten {
    FileWritten {
        path: PathBuf::from(format!("dl/{name}")),
        bytes: 512,
        sha256: Sha256Digest::from_bytes([0xab; 32]),
        asset_id: 300_000_201,
        written,
        digest,
    }
}

fn meta() -> OutputOpts {
    OutputOpts {
        meta: true,
        ..OutputOpts::default()
    }
}

#[test]
fn one_file_prints_its_row_and_notes() {
    let out = emit_with(
        outcome(Answer::Files(vec![file(
            "SHA256SUMS",
            true,
            DigestCheck::Matched,
        )])),
        &meta(),
    );
    assert_eq!(out.exit, 0);
    let hex = "ab".repeat(32);
    assert_eq!(
        parse(&out.stdout),
        json!([{
            "asset_id": 300_000_201u64,
            "bytes": 512,
            "digest": "matched",
            "path": "dl/SHA256SUMS",
            "sha256": format!("sha256:{hex}"),
            "written": true
        }])
    );
    assert!(
        out.stderr.contains(&format!(" digest=sha256:{hex}")),
        "{}",
        out.stderr
    );
    assert!(out.stderr.contains(" written=1"), "{}", out.stderr);

    let out = emit_with(
        outcome(Answer::Files(vec![file("x", false, DigestCheck::Absent)])),
        &meta(),
    );
    assert!(out.stderr.contains(" digest=absent"), "{}", out.stderr);
    assert!(out.stderr.contains(" written=0"), "{}", out.stderr);
}

#[test]
fn several_files_note_their_count() {
    let rows = vec![
        file("a", true, DigestCheck::Matched),
        file("b", true, DigestCheck::Matched),
    ];
    let out = emit_with(outcome(Answer::Files(rows)), &meta());
    assert_eq!(parse(&out.stdout).as_array().map(Vec::len), Some(2));
    assert!(out.stderr.contains(" files=2"), "{}", out.stderr);
    assert!(!out.stderr.contains("digest="), "{}", out.stderr);
}

#[test]
fn streamed_and_nothing_write_no_stdout() {
    let out = emit_with(outcome(Answer::Streamed { written: 42 }), &meta());
    assert_eq!((out.exit, out.stdout.as_str()), (0, ""));
    assert!(out.stderr.contains(" bytes=42"), "{}", out.stderr);
    let out = emit_with(outcome(Answer::Nothing), &OutputOpts::default());
    assert_eq!(
        (out.exit, out.stdout.as_str(), out.stderr.as_str()),
        (0, "", "")
    );
}

#[test]
fn a_stream_never_ends_in_an_output_error() {
    // The verb already wrote the stream: a flag that cannot apply is refused before it runs,
    // so emit has nothing left to refuse.
    for opts in [
        OutputOpts {
            jq: Some("(".into()),
            ..OutputOpts::default()
        },
        OutputOpts {
            raw: true,
            ..OutputOpts::default()
        },
        fields("a-b"),
    ] {
        let out = emit_with(outcome(Answer::Streamed { written: 7 }), &opts);
        assert_eq!((out.exit, out.stdout.as_str()), (0, ""), "{opts:?}");
    }
}

#[test]
fn output_dash_refuses_the_projection_flags_before_the_verb_runs() {
    let (_root, mut ctx, captured) = context();
    let exit = common::run_cli(
        &mut ctx,
        &[
            "release",
            "download",
            "alabsystems/aterm",
            "v0.97.0",
            "--asset",
            "SHA256SUMS",
            "--output",
            "-",
            "--jq",
            ".",
        ],
    );
    assert_eq!(exit, 2);
    assert!(captured.stdout().is_empty());
    let error = common::error_json(&captured.stderr_text());
    assert_eq!(error["kind"], Value::from("usage"), "{error}");
    let why = error["why"].as_str().unwrap_or_default();
    assert!(
        why.contains("release.download") && why.contains("--jq"),
        "the verb and the refused flag are named: {error}"
    );
    assert_eq!(error["next"], Value::from("agh help release download"));
}

#[test]
fn failed_with_prints_the_report_then_two_lines() {
    let error = Error::new(Kind::Pending, "x");
    let report = json!({"conditions": [], "elapsed_s": 30});
    let out = emit_with(
        outcome(Answer::FailedWith {
            value: report.clone(),
            error,
        }),
        &fields("ignored"),
    );
    assert_eq!(out.exit, 8);
    assert_eq!(parse(&out.stdout), report);
    let lines: Vec<&str> = out.stderr.lines().collect();
    assert_eq!(lines.len(), 2, "{}", out.stderr);
    assert!(
        lines[0].starts_with("agh: pending exit=8 verb=release.view"),
        "{}",
        lines[0]
    );
    assert_eq!(common::error_json(&out.stderr)["exit"], Value::from(8u64));
}

// ---------------------------------------------------------------- the receipt

fn receipt_outcome(completeness: Completeness) -> Outcome {
    let mut receipt = Receipt::new("release.list");
    receipt.repo = Some("alabsystems/aterm".into());
    receipt.requests = 7;
    receipt.pages = 3;
    receipt.consistency = Some(Consistency::Snapshot);
    receipt.elapsed_ms = 812;
    receipt.rate = Some(RateSnapshot {
        resource: "core".into(),
        limit: 5000,
        remaining: 4987,
        reset: 1_760_000_000,
        used: 13,
    });
    receipt.notes.push(("moved".into(), "a/b->c/d".into()));
    Outcome {
        answer: Answer::Items {
            noun: Noun::Asset,
            items: vec![json!({"name": "a"})],
            completeness,
        },
        receipt,
    }
}

/// `key=value` fields of a receipt line.
fn receipt_fields(line: &str) -> Vec<(String, String)> {
    line.trim_start_matches("agh: ok ")
        .split(' ')
        .filter_map(|pair| pair.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn the_receipt_prints_under_meta() {
    let out = emit_with(receipt_outcome(Completeness::Complete), &meta());
    assert_eq!(out.exit, 0);
    let line = out.stderr.lines().next().expect("the receipt");
    assert!(line.starts_with("agh: ok "), "{line}");
    let fields = receipt_fields(line);
    let get = |key: &str| {
        fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    };
    assert_eq!(get("verb"), Some("release.list"));
    assert_eq!(get("repo"), Some("alabsystems/aterm"));
    assert_eq!(get("reqs"), Some("7"));
    assert_eq!(get("pages"), Some("3"));
    assert_eq!(get("complete"), Some("1"));
    assert_eq!(get("consistency"), Some("snapshot"));
    assert_eq!(get("ms"), Some("812"));
    assert_eq!(get("rate"), Some("core:4987/5000"));
    assert_eq!(get("moved"), Some("a/b->c/d"));
}

#[test]
fn a_partial_list_prints_the_receipt_without_meta() {
    let out = emit_with(
        receipt_outcome(Completeness::Partial {
            why: "the listing stopped at the 20-page cap".into(),
        }),
        &OutputOpts::default(),
    );
    assert_eq!(out.exit, 0);
    let line = out.stderr.lines().next().expect("the receipt");
    assert!(line.contains(" complete=0"), "{line}");
    assert!(line.contains(" partial=\""), "a quoted why: {line}");
    assert!(line.contains("20-page"), "{line}");
}

#[test]
fn a_limited_list_notes_the_limit_and_stays_silent() {
    let out = emit_with(
        receipt_outcome(Completeness::Limited { asked: 3 }),
        &OutputOpts::default(),
    );
    assert!(out.stderr.is_empty(), "{}", out.stderr);
    let out = emit_with(receipt_outcome(Completeness::Limited { asked: 3 }), &meta());
    assert!(out.stderr.contains(" limit=3"), "{}", out.stderr);
}

#[test]
fn no_receipt_without_meta_or_a_reason() {
    let out = emit_with(
        receipt_outcome(Completeness::Complete),
        &OutputOpts::default(),
    );
    assert!(out.stderr.is_empty(), "{}", out.stderr);
}

// ---------------------------------------------------------------- writers and the terminal

/// A stdout that fails every write with `kind`.
struct Failing(ErrorKind);

impl Write for Failing {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        Err(IoError::from(self.0))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Err(IoError::from(self.0))
    }
}

#[test]
fn a_full_disk_is_exit_1_with_no_receipt() {
    let (_root, mut ctx, captured) = context();
    ctx.stdout = Box::new(Failing(ErrorKind::StorageFull));
    let out = emit_in(
        &mut ctx,
        &captured,
        object(Noun::Release, release()),
        &meta(),
    );
    assert_eq!(out.exit, 1);
    assert!(!out.stderr.contains("agh: ok"), "{}", out.stderr);
    assert_eq!(
        out.stderr.lines().count(),
        1,
        "the one line: {}",
        out.stderr
    );
}

#[test]
fn a_gone_reader_is_exit_0() {
    let (_root, mut ctx, captured) = context();
    ctx.stdout = Box::new(Failing(ErrorKind::BrokenPipe));
    let out = emit_in(
        &mut ctx,
        &captured,
        object(Noun::Release, release()),
        &OutputOpts::default(),
    );
    assert_eq!(out.exit, 0);
    assert!(out.stderr.is_empty(), "{}", out.stderr);
}

#[test]
fn a_terminal_gets_escapes_and_a_pipe_gets_exact_bytes() {
    let hostile = "x\u{202e}y\u{1b}[31mz";
    let value = json!({"name": hostile});
    let raw = OutputOpts {
        raw: true,
        fields: Some("name".into()),
        ..OutputOpts::default()
    };

    let piped = emit_with(object(Noun::Asset, value.clone()), &raw);
    assert_eq!(piped.stdout, format!("{hostile}\n"));
    let piped_json = emit_with(object(Noun::Asset, value.clone()), &fields("name"));
    assert!(
        piped_json.stdout.contains('\u{202e}'),
        "JSON leaves U+202E as it is"
    );

    let (_root, ctx, captured) = context();
    let mut ctx = ctx.with_terminals(true, false);
    let shown = emit_in(
        &mut ctx,
        &captured,
        object(Noun::Asset, value.clone()),
        &raw,
    );
    assert!(!shown.stdout.contains('\u{202e}'), "{:?}", shown.stdout);
    assert!(!shown.stdout.contains('\u{1b}'), "{:?}", shown.stdout);
    assert!(shown.stdout.contains("\\u{202e}"), "{:?}", shown.stdout);

    let (_root, ctx, captured) = context();
    let mut ctx = ctx.with_terminals(true, false);
    let shown = emit_in(
        &mut ctx,
        &captured,
        object(Noun::Asset, value),
        &fields("name"),
    );
    assert!(!shown.stdout.contains('\u{202e}'), "JSON on a terminal too");
}
