// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::{Asked, CredentialAnswer, CredentialRequest, explained};
use crate::git::grant::MapGrants;
use crate::policy::{Presence, Router};

/// One dev profile from the environment, reading and writing for `fixture-dev`: no file is
/// read, so the decision is the same on every OS but for the off-unix rule.
fn router() -> Router {
    let text = "[profiles.dev]\nenv = \"AGH_TEST_TOKEN\"\ntier = \"dev\"\n\
                login = \"fixture-dev-bot\"\n\n[[owners]]\nname = \"fixture-dev\"\n\
                tier = \"dev\"\nread = [\"dev\"]\nwrite = \"dev\"\n";
    let parsed = crate::config::parse(text).unwrap_or_else(|f| panic!("{}: {}", f.rule, f.what));
    let validated = crate::config::validate(&parsed, std::path::Path::new("/"), &|_| None)
        .unwrap_or_else(|faults| panic!("{} faults", faults.len()));
    Router::from_validated(validated)
}

/// `whoami --git` explains what the helper answers: off unix that is always a quit, as
/// `serve_with` answers there, even for a profile that is Present.
#[test]
fn the_helper_answers_only_on_unix() {
    let req = CredentialRequest::parse(
        "get",
        "protocol=https\nhost=github.com\npath=fixture-dev/app.git\n\n",
    )
    .unwrap_or_else(|e| panic!("the request: {}", e.why));
    let (answer, granted) = explained(
        &req,
        &Asked::default(),
        &router(),
        &MapGrants::new(),
        &|_| Presence::Present,
    );
    assert!(!granted);
    assert_eq!(
        matches!(answer, CredentialAnswer::Credential { .. }),
        cfg!(unix),
        "{answer:?}"
    );
}
