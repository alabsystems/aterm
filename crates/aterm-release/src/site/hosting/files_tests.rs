// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn p(text: &str) -> Pattern {
    Pattern::new(text).unwrap()
}

/// The site's own ignore list and firebase-tools' always-ignored names match as glob's
/// ignore does (dot files included). NEGATIVE CONTROL: each also leaves the paths it
/// must not take, so a matcher that took everything fails.
#[test]
fn the_ignore_patterns_match_as_firebase_tools_ignores() {
    let cases: [(&str, &[&str], &[&str]); 6] = [
        (
            "firebase.json",
            &["firebase.json"],
            &["assets/firebase.json", "firebase.jsonx"],
        ),
        (
            "**/.*",
            &[".DS_Store", "assets/.hidden", ".well-known"],
            &["index.html", ".well-known/assetlinks.json", "a.b"],
        ),
        (
            "**/node_modules/**",
            &["node_modules/x.js", "a/node_modules/b/c.js", "node_modules"],
            &["a/node_modules_x/c.js", "node_modulesx/c.js"],
        ),
        (
            "**/firebase-debug.*.log",
            &["firebase-debug.1.log", "a/firebase-debug.x.log"],
            &["firebase-debug.log", "a/firebase-debug.log.txt"],
        ),
        (
            ".firebase/*",
            &[".firebase/hosting.cache"],
            &[".firebase/a/b", "x/.firebase/y"],
        ),
        (
            "assets/term/aterm_wasm.?.js",
            &["assets/term/aterm_wasm.a.js", "assets/term/aterm_wasm.é.js"],
            &["assets/term/aterm_wasm.ab.js", "assets/term/aterm_wasm..js"],
        ),
    ];
    for (pattern, hits, misses) in cases {
        let pat = p(pattern);
        for hit in hits {
            assert!(pat.matches(hit), "{pattern} matches {hit}");
        }
        for miss in misses {
            assert!(!pat.matches(miss), "{pattern} leaves {miss}");
        }
    }
    assert!(p("**/node_modules/**").prunes("a/node_modules"));
    assert!(!p("**/node_modules/**").prunes("a/node"));
    assert!(!p("**/.*").prunes(".well-known"), "only X/** prunes");
}

/// A pattern this matcher does not implement is refused by name, never matched some
/// other way. NEGATIVE CONTROL: the patterns it implements are accepted.
#[test]
fn an_unsupported_pattern_is_refused() {
    for bad in [
        "**/*.{js,css}",
        "[ab].html",
        "!keep.html",
        "@(a|b)",
        "/abs",
        "///",
        "a//b",
        "./secret.json",
        "a/../secret.json",
        "",
        "a\\*",
    ] {
        assert!(Pattern::new(bad).is_err(), "{bad:?} refused");
    }
    for good in ["*", "**", "a/**/b?.html", "**/.*"] {
        assert!(Pattern::new(good).is_ok(), "{good:?} accepted");
    }
}

/// The listing walks `public/`, prunes an ignored directory, skips ignored files, refuses a
/// symbolic link, and lays a dry run's planned content over what is on disk.
/// NEGATIVE CONTROL: with no ignore list every file is listed, the dot file included.
#[cfg(unix)]
#[test]
fn the_listing_leaves_what_ignore_names_and_refuses_a_link() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-hosting-list-")
        .tempdir()
        .unwrap();
    let public = dir.path().join("public");
    for (rel, text) in [
        ("index.html", "home"),
        (".DS_Store", "junk"),
        ("assets/a.css", "css"),
        ("node_modules/pkg/x.js", "dep"),
        ("firebase.json", "{}"),
    ] {
        let path = public.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let ignore = patterns(&[
        "firebase.json".into(),
        "**/.*".into(),
        "**/node_modules/**".into(),
    ])
    .unwrap();
    let mut overlay = BTreeMap::new();
    overlay.insert("index.html".to_string(), b"planned".to_vec());
    overlay.insert("releases.html".to_string(), b"new".to_vec());
    let listed = list(&public, &ignore, &overlay).unwrap();
    assert_eq!(
        listed.keys().collect::<Vec<_>>(),
        ["assets/a.css", "index.html", "releases.html"]
    );
    assert_eq!(listed["index.html"], b"planned");
    let all = list(&public, &[], &BTreeMap::new()).unwrap();
    assert_eq!(all.len(), 5, "{:?}", all.keys().collect::<Vec<_>>());
    std::os::unix::fs::symlink("/etc/hosts", public.join("hosts")).unwrap();
    let why = list(&public, &ignore, &BTreeMap::new()).unwrap_err();
    assert!(
        why.contains("hosts") && why.contains("not a regular file"),
        "{why}"
    );
    let root_link = dir.path().join("public-link");
    std::os::unix::fs::symlink(&public, &root_link).unwrap();
    let root_why = list(&root_link, &ignore, &BTreeMap::new()).unwrap_err();
    assert!(
        root_why.contains("public-link") && root_why.contains("not a plain directory"),
        "{root_why}"
    );
}

/// A service-account key under `public/` is refused by name, wherever it sits and whatever
/// it is called, and its bytes are never quoted; an ignored one is never read. NEGATIVE
/// CONTROL: JSON that only mentions `private_key`, or holds one that is not a string, is
/// listed.
#[test]
fn a_key_under_public_is_never_published() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-hosting-key-")
        .tempdir()
        .unwrap();
    let public = dir.path().join("public");
    std::fs::create_dir_all(public.join("assets")).unwrap();
    std::fs::write(public.join("index.html"), "home").unwrap();
    std::fs::write(
        public.join("notes.json"),
        r#"{"doc": "a key's \"private_key\" field never ships", "private_key": 7}"#,
    )
    .unwrap();
    let listed = list(&public, &[], &BTreeMap::new()).unwrap();
    assert_eq!(
        listed.keys().collect::<Vec<_>>(),
        ["index.html", "notes.json"]
    );
    let key = super::super::key::tests::key_json("https://oauth2.googleapis.com/token");
    std::fs::write(public.join("assets/deployer.txt"), &key).unwrap();
    let why = list(&public, &[], &BTreeMap::new()).unwrap_err();
    assert!(
        why.contains("deployer.txt") && why.contains("private key"),
        "{why}"
    );
    let body = aterm_codec::base64::encode(super::super::key::tests::TEST_PKCS8).unwrap();
    assert!(
        key.contains(&body[130..170]),
        "the probe is the key's own bytes"
    );
    assert!(!why.contains(&body[130..170]), "{why}");
    let ignored = list(
        &public,
        &patterns(&["assets/deployer.txt".into()]).unwrap(),
        &BTreeMap::new(),
    )
    .unwrap();
    assert!(!ignored.contains_key("assets/deployer.txt"));

    std::fs::remove_file(public.join("assets/deployer.txt")).unwrap();
    let mut overlay = BTreeMap::new();
    overlay.insert("assets/new-key.json".to_string(), key.into_bytes());
    let why_overlay = list(&public, &[], &overlay).unwrap_err();
    assert!(
        why_overlay.contains("new-key.json") && why_overlay.contains("private key"),
        "{why_overlay}"
    );
    for (name, secret) in [
        ("escaped.json", br#"{"private\u005fkey":"secret"}"#.to_vec()),
        (
            "nested.json",
            br#"{"creds":{"private_key":"secret"}}"#.to_vec(),
        ),
        (
            "pem.txt",
            format!(
                "{}\nMIIBVQ==\n{}\n",
                concat!("-----BEGIN ", "RSA PRIVATE KEY-----"),
                concat!("-----END ", "RSA PRIVATE KEY-----"),
            )
            .into_bytes(),
        ),
    ] {
        let mut extra = BTreeMap::new();
        extra.insert(name.to_string(), secret);
        let why_extra = list(&public, &[], &extra).unwrap_err();
        assert!(
            why_extra.contains(name) && why_extra.contains("private key"),
            "{why_extra}"
        );
    }
}

/// A file is named by the SHA-256 of its gzip member, and the member is the same bytes
/// for the same content. NEGATIVE CONTROL: other content is another hash.
#[test]
fn a_file_is_named_by_the_hash_of_its_member() {
    let f = File::new("/index.html".into(), b"<p>x</p>");
    assert_eq!(f.bytes, 8);
    assert_eq!(
        f.hash,
        aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&f.gz))
    );
    assert_eq!(f, File::new("/index.html".into(), b"<p>x</p>"));
    assert_ne!(f.hash, File::new("/index.html".into(), b"<p>y</p>").hash);
}
