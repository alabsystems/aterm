// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The real Git backend's authentication, path and HTTP framing boundary.

use crate::raw::{plain, send_bytes};
use agh_fake::git_backend::{GitHttpBackend, Identity};
use agh_fake::{basic, token};

#[test]
fn real_backend_authenticates_before_cgi_and_advertises_actual_atomic_support() {
    let root = aterm_tempfile::tempdir().unwrap();
    let source = root.path().join("seed.git");
    let mut command = aterm_uds::fixture_git::git();
    command.args(["init", "--quiet", "--bare"]).arg(&source);
    let output = aterm_uds::deadline::output_until_complete_stdout(
        &mut command,
        std::time::Instant::now() + crate::raw::HANG,
    )
    .unwrap();
    assert!(output.status.success());
    let (reader, writer) = (token("reader"), token("writer"));
    assert!(GitHttpBackend::start("../escape", &source, &reader, &writer).is_err());
    assert!(GitHttpBackend::start("fixture-dev/app", &source, &writer, &writer).is_err());
    let backend = GitHttpBackend::start("fixture-dev/app", &source, &reader, &writer).unwrap();
    let target = "/fixture-dev/app.git/info/refs?service=git-receive-pack";
    assert_eq!(plain(target).send(backend.port()).status, 401);
    assert_eq!(
        plain(target)
            .header("Authorization", &format!("Basic {}", basic(&reader)))
            .send(backend.port())
            .status,
        401
    );
    let reply = plain(target)
        .header("Authorization", &format!("Basic {}", basic(&writer)))
        .send(backend.port());
    assert_eq!(reply.status, 200);
    let text = String::from_utf8(reply.body).unwrap();
    let capabilities = text.split_once('\0').expect("real receive capabilities").1;
    assert!(capabilities.split_ascii_whitespace().any(|c| c == "atomic"));
    for path in [
        "/../seed.git/info/refs?service=git-receive-pack",
        "/fixture-dev/app.git/info/refs?service=other",
        "/fixture-dev/app.git/objects/HEAD",
        "/fixture-dev/app.git/info/refs?service=git-upload-pack&extra=1",
    ] {
        assert_eq!(
            plain(path)
                .header("Authorization", &format!("Basic {}", basic(&writer)))
                .send(backend.port())
                .status,
            404
        );
    }
    let malformed = format!(
        "GET {target} HTTP/1.1\r\nContent-Length: 0\r\nContent-Length: 0\r\nAuthorization: Basic {}\r\n\r\n",
        basic(&writer)
    );
    assert_eq!(send_bytes(backend.port(), malformed.as_bytes()).status, 400);
    let seen = backend.seen();
    assert_eq!(seen[0].identity, Identity::Anonymous);
    assert_eq!(seen[1].identity, Identity::Reader);
    assert_eq!(seen.iter().filter(|r| r.backend_attempted).count(), 1);
    assert_eq!(seen[2].identity, Identity::Writer);
    assert!(seen[2].backend_attempted);
    assert!(seen.iter().all(|r| !r.response_dropped));
}
