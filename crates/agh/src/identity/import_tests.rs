// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use std::path::PathBuf;
use std::sync::Arc;

use super::{
    AddFrom, AddOptions, AddRequest, ChildOut, GH_ABSOLUTE, ImportRequest, import_from_gh,
    token_line,
};
use crate::client::GithubBuilder;
use crate::config::Dirs;
use crate::error::Kind;
use crate::fx::Fx;
use crate::fx::child::ScriptedChild;
use crate::identity::MapEnv;
use crate::policy::{Router, Tier};
use crate::wire::Endpoints;
use crate::wire::scripted::ScriptedTransport;

#[test]
fn legacy_loose_is_refused_before_gh_runs() {
    let token = agh_fake::token("loose-import");
    let fx = Arc::new(
        Fx::simulated(1_760_000_000).with_children(vec![ScriptedChild {
            program: PathBuf::from(GH_ABSOLUTE[0]),
            args: ["auth", "token", "--hostname", "github.com"]
                .map(str::to_string)
                .to_vec(),
            out: out(format!("{token}\n").as_bytes()),
        }]),
    );
    // A transport with nothing scripted, at loopback ports nothing listens on: a call
    // this test should never make fails here and reaches no network.
    let gh = GithubBuilder::new(Router::empty())
        .endpoints(Endpoints::loopback(1, 1, 1, 1))
        .transport(Arc::new(ScriptedTransport::new()))
        .fx(fx.clone())
        .build();
    let req = ImportRequest {
        user: None,
        add: AddRequest {
            name: "dev2".to_string(),
            from: AddFrom::Stdin,
            tier: Tier::Dev,
            owners: vec!["fixture-dev".to_string()],
            write: false,
            drafts: false,
        },
    };
    // Nothing is read or written before the refusal, so the scratch root stays empty.
    let root = crate::test_fixtures::scratch_dir("import-loose");
    let dirs = Dirs::under(root.path());
    let error = import_from_gh(
        &gh,
        &dirs,
        &MapEnv::new(),
        &req,
        AddOptions::default().legacy_loose(true),
    )
    .expect_err("--legacy-loose takes a --file");
    assert_eq!(error.kind, Kind::Usage);
    assert!(fx.children_run().is_empty(), "{:?}", fx.children_run());
    let made: Vec<_> = std::fs::read_dir(root.path())
        .expect("the scratch root")
        .collect();
    assert!(made.is_empty(), "{made:?}");
}

fn out(stdout: &[u8]) -> ChildOut {
    ChildOut {
        code: Some(0),
        stdout: stdout.to_vec(),
        stderr: Vec::new(),
    }
}

#[test]
fn one_line_with_at_most_one_newline_is_a_token() {
    let token = agh_fake::token("line");
    for stdout in [token.clone(), format!("{token}\n"), format!("{token}\r\n")] {
        let secret = token_line(&out(stdout.as_bytes())).expect("one token line");
        assert_eq!(secret.expose(), token.as_bytes());
    }
    for stdout in [
        String::new(),
        "\n".to_string(),
        format!("{token}\n\n"),
        format!("{token} x\n"),
        format!("{token}\tx"),
        format!("a\n{token}\n"),
        format!("{token}\u{7f}"),
        // The shapes `auth add` refuses: under 20 bytes, over 255, and a byte past ASCII.
        "short-line\n".to_string(),
        "g".repeat(300),
        format!("{token}\u{e9}"),
    ] {
        assert!(token_line(&out(stdout.as_bytes())).is_none(), "{stdout:?}");
    }
}
