// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::super::tests::{run, spots};
use super::*;

const HEADER: &str = "Authorization";

#[test]
fn the_header_name_in_agh_outside_the_door_is_found() {
    let one_line = format!("const H: &str = \"{HEADER}\";\n");
    let multi = format!("const H: &str = \"first line\n{HEADER}: x\n\";\n");
    let found = run(
        &GITHUB_AUTH_DOOR,
        &[
            ("crates/agh/src/x.rs", &one_line),
            ("crates/agh/src/multi.rs", &multi),
        ],
    );
    assert_eq!(
        spots(&found),
        ["crates/agh/src/multi.rs:2", "crates/agh/src/x.rs:1"]
    );
}

#[test]
fn the_door_an_identifier_a_comment_and_a_test_are_no_finding() {
    let literal = format!("const H: &str = \"{HEADER}\";\n");
    let in_test_mod =
        format!("fn a() {{}}\n#[cfg(test)]\nmod tests {{\n    const H: &str = \"{HEADER}\";\n}}\n");
    let found = run(
        &GITHUB_AUTH_DOOR,
        &[
            ("crates/agh/src/wire/door.rs", &literal),
            (
                "crates/agh/src/ident.rs",
                "let h = door::AUTHORIZATION;\nlet y = door::is_authorization(n);\n",
            ),
            (
                "crates/agh/src/comment.rs",
                &format!("// the {HEADER} header\nfn a() {{}}\n"),
            ),
            ("crates/agh/tests/t.rs", &literal),
            ("crates/agh/src/gated.rs", &in_test_mod),
        ],
    );
    assert_eq!(spots(&found), Vec::<String>::new());
}

#[test]
fn a_bearer_header_beside_a_github_api_origin_is_found() {
    let origin = "const API: &str = \"https://api.github.com\";\n";
    let header = format!("let h = \"{HEADER}: Bearer x\";\n");
    let with_origin = format!("{origin}{header}");
    let bare_bearer = format!("{origin}let p = \"bearer \";\n");
    let token_file = format!("{origin}let p = \"token file\";\n");
    let found = run(
        &GITHUB_AUTH_DOOR,
        &[
            ("crates/x/src/with_origin.rs", &with_origin),
            ("crates/x/src/no_origin.rs", &header),
            ("crates/x/src/bare_bearer.rs", &bare_bearer),
            ("crates/x/src/token_file.rs", &token_file),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/x/src/bare_bearer.rs:2",
            "crates/x/src/with_origin.rs:2"
        ]
    );
}

#[test]
fn a_formatted_bearer_header_beside_a_github_api_origin_is_found() {
    let origin = "const API: &str = \"https://api.github.com\";\n";
    let bearer = "let h = format!(\"Bearer {t}\");\n";
    let token = "let h = format!(\"token {t}\");\n";
    let found = run(
        &GITHUB_AUTH_DOOR,
        &[
            ("crates/x/src/bearer.rs", &format!("{origin}{bearer}")),
            ("crates/x/src/token.rs", &format!("{origin}{token}")),
            ("crates/x/src/no_origin.rs", bearer),
        ],
    );
    assert_eq!(
        spots(&found),
        ["crates/x/src/bearer.rs:2", "crates/x/src/token.rs:2"]
    );
}

#[test]
fn the_rest_lane_holds_one_bearer_header_and_no_more() {
    let origin = "const API: &str = \"https://uploads.github.com\";\n";
    let one = format!("{origin}let h = \"{HEADER}: token x\";\n");
    let two = format!("{one}let g = \"{HEADER}: token y\";\n");
    let lane = "crates/aterm-update-core/src/github_rest.rs";
    assert_eq!(
        spots(&run(&GITHUB_AUTH_DOOR, &[(lane, &one)])),
        Vec::<String>::new()
    );
    assert_eq!(
        spots(&run(&GITHUB_AUTH_DOOR, &[(lane, &two)])),
        [format!("{lane}:3")]
    );
    let publish = "crates/aterm-release/src/publish.rs";
    assert_eq!(
        spots(&run(&GITHUB_AUTH_DOOR, &[(publish, &one)])),
        [format!("{publish}:2")]
    );
}

#[test]
fn a_gh_child_is_found_past_its_files_cap() {
    let gh_rs = "crates/agh/src/fx/mod.rs";
    let found = run(
        &GH_SPAWN,
        &[
            (
                "crates/x/src/spawn.rs",
                "let c = aterm_uds::spawnfd::command(\"gh\");\n\
                 let d = Command::new(\"/opt/homebrew/bin/gh\");\n\
                 const GH: &str = \"gh\";\n",
            ),
            (
                gh_rs,
                "let c = command(\"gh\");\nlet d = command(\"gh\");\n",
            ),
            (
                "crates/atpkg-publish/src/host/gh.rs",
                "let c = command(\"gh\");\n",
            ),
            (
                "crates/aterm-release/src/publish.rs",
                "let c = command(\"gh\");\n",
            ),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            format!("{gh_rs}:2"),
            "crates/aterm-release/src/publish.rs:1".to_string(),
            "crates/atpkg-publish/src/host/gh.rs:1".to_string(),
            "crates/x/src/spawn.rs:1".to_string(),
            "crates/x/src/spawn.rs:2".to_string(),
            "crates/x/src/spawn.rs:3".to_string(),
        ]
    );
}

#[test]
fn a_gh_literal_that_spawns_nothing_or_sits_within_a_cap_is_no_finding() {
    let found = run(
        &GH_SPAWN,
        &[
            ("crates/x/src/assert.rs", "assert_eq!(x, \"gh\");\n"),
            ("crates/agh/src/fx/mod.rs", "let c = command(\"gh\");\n"),
            ("crates/x/tests/t.rs", "let c = command(\"gh\");\n"),
            (
                "crates/aterm-census/src/guards/x.rs",
                "let c = command(\"gh\");\n",
            ),
            ("crates/x/src/run.rs", "let c = run_command(\"gh\");\n"),
        ],
    );
    assert_eq!(spots(&found), Vec::<String>::new());
}

#[test]
fn a_clock_a_sleep_a_socket_or_a_round_trip_in_agh_is_found() {
    let lines = [
        "let t = Instant::now();",
        "std::thread::sleep(d);",
        "let e = t.elapsed();",
        "self.transport.round_trip(req)",
        "Transport::round_trip(&*t, req)",
        "use std::net::TcpStream;",
        "aterm_http::Client::new(trust, proxy, d)",
        "use aterm_http::{proxy::EnvSource, Client};",
    ];
    let client = lines.join("\n") + "\n";
    let found = run(&AGH_FX, &[("crates/agh/src/client.rs", &client)]);
    let expected: Vec<String> = (1..=lines.len())
        .map(|no| format!("crates/agh/src/client.rs:{no}"))
        .collect();
    assert_eq!(spots(&found), expected);
}

#[test]
fn a_sleep_or_a_clock_bound_by_an_import_is_found_at_the_import() {
    let lines = [
        "use std::thread::{self, sleep};",
        "fn a(d: Duration) { sleep(d); }",
        "use std::time::Instant as Clock;",
        "fn b() { let t = Clock::now(); }",
        "use std::thread::*;",
        "use std::{",
        "    net::TcpStream as Socket,",
        "};",
        "fn c(t: Instant) { std::thread::sleep_until(t); }",
        "fn d(d: Duration) { std::thread::park_timeout(d); }",
        "type Clock = std::time::Instant;",
        "fn r() -> Clock { Clock::now() }",
        "pub type Wall = std::time::SystemTime;",
    ];
    let client = lines.join("\n") + "\n";
    let found = run(&AGH_FX, &[("crates/agh/src/client.rs", &client)]);
    assert_eq!(
        spots(&found),
        [1, 3, 5, 6, 9, 10, 11, 13].map(|no| format!("crates/agh/src/client.rs:{no}"))
    );
}

#[test]
fn aterm_http_bound_under_another_name_is_found_outside_its_file() {
    let renamed = "use aterm_http as http;\nfn a() { http::Client::new(t, p, d); }\n\
                   use {aterm_http as h2, std::fmt};\n\
                   extern crate aterm_http as h3;\n";
    let not_renamed = "use aterm_http as _;\nuse aterm_http::proxy::{self as p};\n\
                       extern crate aterm_http;\n";
    let found = run(
        &AGH_FX,
        &[
            ("crates/agh/src/client.rs", renamed),
            ("crates/agh/src/fx/mod.rs", "use aterm_http as http;\n"),
            ("crates/agh/src/wire/http.rs", renamed),
            ("crates/agh/src/other.rs", not_renamed),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/agh/src/client.rs:1",
            "crates/agh/src/client.rs:3",
            "crates/agh/src/client.rs:4",
            "crates/agh/src/fx/mod.rs:1",
        ]
    );
}

#[test]
fn an_import_under_its_own_name_or_in_the_seam_is_no_finding() {
    let imports = "use std::time::Instant;\nuse std::thread;\nuse std::time::Instant as _;\n\
                   use std::time::{Duration, Instant};\ntype D = std::time::Duration;\n\
                   type Instant = std::time::Instant;\n";
    let found = run(
        &AGH_FX,
        &[
            ("crates/agh/src/client.rs", imports),
            (
                "crates/agh/src/fx/mod.rs",
                "use std::thread::{self, sleep};\nuse std::time::Instant as Clock;\n",
            ),
        ],
    );
    assert_eq!(spots(&found), Vec::<String>::new());
}

#[test]
fn the_seam_a_definition_a_field_and_a_string_are_no_finding() {
    let clocks = "let t = Instant::now();\nstd::thread::sleep(d);\nlet e = t.elapsed();\n\
                  let s = SystemTime::now();\n";
    let found = run(
        &AGH_FX,
        &[
            ("crates/agh/src/fx/mod.rs", clocks),
            (
                "crates/agh/src/wire/scripted.rs",
                "fn round_trip(&self, req: WireRequest<'_>) -> Result<WireResponse, WireError> {\n}\n",
            ),
            (
                "crates/agh/src/wait.rs",
                "self.fx.sleep(d)?;\nlet s = beat.elapsed;\nreceipt.elapsed_ms = ms;\n\
                 // Instant::now is the seam's\nlet w = \"thread::sleep\";\n",
            ),
            ("crates/agh/tests/t.rs", clocks),
            (
                "crates/agh/src/wire/http.rs",
                "aterm_http::Client::new(trust, proxy, d)\n",
            ),
            (
                "crates/agh/src/identity/mod.rs",
                "use aterm_http::proxy::{EnvSource, ProcessEnv};\n",
            ),
            (
                "crates/agh/src/wire/mod.rs",
                "let e = aterm_http::Error::TooLarge;\n",
            ),
            ("crates/aterm-agent/src/supervise/fx.rs", clocks),
        ],
    );
    assert_eq!(spots(&found), Vec::<String>::new());
}

const SPAWNS: &str = "use std::process::Command;\n\
                      fn a() { let c = aterm_uds::spawnfd::command(\"git\"); }\n\
                      fn b() { let c = Command::new(\"git\"); }\n\
                      fn d(c: C, t: T) { let o = aterm_uds::deadline::output_until(c, t); }\n\
                      fn e() { let p = aterm_uds::pspawn::spawn_cloexec_default(\"git\", &[]); }\n\
                      fn f() { let c = aterm_uds::fixture_git::git(); }\n\
                      use aterm_uds::pspawn::spawn_cloexec_default;\n\
                      use aterm_uds::fixture_git;\n";

#[test]
fn a_spawn_in_agh_outside_its_child_door_is_found() {
    let found = run(&AGH_FX, &[("crates/agh/src/x.rs", SPAWNS)]);
    assert_eq!(
        spots(&found),
        [
            "crates/agh/src/x.rs:1",
            "crates/agh/src/x.rs:2",
            "crates/agh/src/x.rs:3",
            "crates/agh/src/x.rs:4",
            "crates/agh/src/x.rs:5",
            "crates/agh/src/x.rs:6",
            "crates/agh/src/x.rs:7",
            "crates/agh/src/x.rs:8"
        ]
    );
}

#[test]
fn a_spawn_in_the_child_door_a_test_module_or_a_test_file_is_no_finding() {
    let in_test_mod = format!("fn a() {{}}\n#[cfg(test)]\nmod tests {{\n{SPAWNS}}}\n");
    let found = run(
        &AGH_FX,
        &[
            ("crates/agh/src/fx/child.rs", SPAWNS),
            ("crates/agh/src/x.rs", &in_test_mod),
            ("crates/agh/tests/t.rs", SPAWNS),
        ],
    );
    assert_eq!(spots(&found), Vec::<String>::new());
}
