// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The mirror's own statement of the rule, in the crate the dependency-free
/// `aterm drive --socket` client calls (`aterm-ctl` pins it against
/// `aterm-types`).
#[test]
fn token_names_pair_per_socket_and_never_reach_a_reserved_name() {
    assert_eq!(token_name_for_sock("aterm-7.sock"), "aterm-7.token");
    assert_eq!(token_name_for_sock("aterm-01.sock"), "aterm-1.token");
    assert_eq!(token_name_for_sock("a.sock"), "a.sock.token");
    assert_eq!(token_name_for_sock("b.sock"), "b.sock.token");
    assert_eq!(token_name_for_sock("aterm.sock"), "aterm.sock.token");
    assert_eq!(token_name_for_sock("aterm"), "aterm-sock-aterm.token");
    assert_eq!(token_name_for_sock("aterm-42"), "aterm-sock-aterm-42.token");
    assert_eq!(token_name_for_sock("ctl"), "aterm-sock-ctl.token");
    assert_eq!(token_name_for_sock(""), "aterm-sock-.token");
    assert_eq!(token_names_for_sock("aterm-7.sock"), vec!["aterm-7.token"]);
    assert_eq!(
        token_names_for_sock("a.sock"),
        vec!["a.sock.token".to_string(), "aterm.token".to_string()]
    );
}

/// A temp directory of this test's own (tests share one process).
fn test_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("aterm-uds-tok-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create test dir");
    dir
}

/// The per-socket file wins whenever it exists; the legacy shared name only in its
/// absence, and never for a per-instance socket.
#[test]
fn token_path_prefers_the_per_socket_file_over_the_legacy_shared_one() {
    let dir = test_dir("fallback");
    let sock = dir.join("a.sock");
    let sock = sock.to_string_lossy().into_owned();

    assert_eq!(
        token_path_for_sock(&sock).expect("resolves"),
        dir.join("a.sock.token")
    );

    std::fs::write(dir.join("aterm.token"), "beef\n").unwrap();
    assert_eq!(
        token_path_for_sock(&sock).expect("resolves"),
        dir.join("aterm.token")
    );

    std::fs::write(dir.join("a.sock.token"), "feed\n").unwrap();
    assert_eq!(
        token_path_for_sock(&sock).expect("resolves"),
        dir.join("a.sock.token")
    );

    #[cfg(unix)]
    {
        let token = dir.join("a.sock.token");
        std::fs::remove_file(&token).unwrap();
        std::os::unix::fs::symlink("missing-token", &token).unwrap();
        assert_eq!(token_path_for_sock(&sock).expect("resolves"), token);

        std::fs::remove_file(&token).unwrap();
        assert_eq!(
            token_path_for_sock(&sock).expect("resolves"),
            dir.join("aterm.token")
        );
    }

    let inst = dir.join("aterm-77.sock");
    assert_eq!(
        token_path_for_sock(&inst.to_string_lossy()).expect("resolves"),
        dir.join("aterm-77.token")
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The `latest` alias is resolved before the name rule, so a flagless
/// client reads the token of the instance the alias points at.
#[cfg(unix)]
#[test]
fn the_latest_alias_resolves_to_the_pointed_at_instance_token() {
    let dir = test_dir("alias");
    let link = dir.join("aterm.sock");
    publish(&link, &dir.join("aterm-101.sock").to_string_lossy());
    assert_eq!(
        token_path_for_sock(&link.to_string_lossy()).expect("resolves"),
        dir.join("aterm-101.token"),
        "the alias must name the instance's token, never its own"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
