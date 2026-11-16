// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use std::path::Path;

use aterm_uds::spawnfd::Stdio;

use super::{in_tree, start_of};

const NOW: u64 = 1_760_000_000;

fn sweep_world() -> (aterm_tempfile::TempDir, crate::config::Dirs) {
    let root = crate::test_fixtures::scratch_dir("grant-lifetime");
    let dirs = crate::config::Dirs::under(root.path());
    crate::config::make_state_subdir(&dirs, &dirs.grants()).unwrap();
    (root, dirs)
}

#[test]
fn sweeps_preserve_live_versioned_bytes_without_authorizing_old_grants() {
    use crate::git::credential::{HelperIo, serve_with};
    use crate::identity::MapEnv;

    let root = crate::test_fixtures::scratch_dir("grant-credential");
    let token_file = root.path().join("dev.token");
    let token = agh_fake::token("dev");
    crate::test_fixtures::setup_file(&token_file, token.as_bytes());
    let text = format!(
        "[profiles.dev]\ntoken_file = \"{}\"\ntier = \"dev\"\n\
                login = \"fixture-dev-bot\"\n\n[[owners]]\nname = \"fixture-dev\"\n\
                tier = \"dev\"\nread = [\"dev\"]\nwrite = \"dev\"\n",
        crate::test_fixtures::toml_path(&token_file)
    );
    let parsed = crate::config::parse(&text).unwrap();
    let validated = crate::config::validate(&parsed, Path::new("/"), &|_| None).unwrap();
    let router = crate::policy::Router::from_validated(validated);
    let nonce = "ab".repeat(16);
    let env = MapEnv::new().with(crate::git::GRANT_VAR, &nonce);
    let credential = crate::identity::load(router.profile("dev").unwrap()).unwrap();
    let pid = std::process::id();
    let start = start_of(i32::try_from(pid).unwrap()).unwrap();
    let expiry = NOW + 10;
    for version in [1, 2] {
        let (_root, dirs) = sweep_world();
        let grant = super::Grant {
            nonce: nonce.clone(),
            slug: crate::Slug::parse("fixture-dev/app").unwrap(),
            profile: "dev".into(),
            credential_binding: credential.git_grant_binding(),
            pid,
            start,
            expires_at: expiry,
        };
        let mut body: aterm_json::Value = aterm_json::from_str(&grant.to_json()).unwrap();
        if version == 1 {
            body.as_object_mut()
                .unwrap()
                .insert("v".into(), aterm_json::Value::from(1));
            body.as_object_mut().unwrap().remove("credential_binding");
        }
        let bytes = body.to_string().into_bytes();
        let path = dirs.grants().join(format!("{nonce}.json"));
        crate::test_fixtures::setup_file(&path, &bytes);
        let helper = |now| {
            let mut input = std::io::Cursor::new(
                b"protocol=https\nhost=github.com\npath=fixture-dev/app.git\n\n",
            );
            let mut output = Vec::new();
            serve_with(
                "get",
                HelperIo {
                    stdin: &mut input,
                    stdin_is_terminal: false,
                    stdout: &mut output,
                },
                &router,
                &super::LiveGrants::new(&dirs),
                &env,
                now,
                &[],
            )
            .unwrap();
            output
        };
        let expected = if version == 1 {
            b"quit=1\n".to_vec()
        } else {
            format!("username=x-access-token\npassword={token}\n").into_bytes()
        };
        assert_eq!(helper(NOW), expected);
        for now in [NOW, expiry] {
            super::sweep(&dirs, now);
            assert_eq!(
                std::fs::read(&path).unwrap(),
                bytes,
                "version {version} at {now}"
            );
            assert_eq!(helper(now), expected, "version {version} at {now}");
        }
        super::sweep(&dirs, expiry + 1);
        if version == super::GRANT_VERSION {
            assert!(!path.exists(), "expired version {version} remained");
        } else {
            assert_eq!(
                std::fs::read(&path).unwrap(),
                bytes,
                "another version's record was removed"
            );
        }
        assert_eq!(helper(expiry + 1), b"quit=1\n");
    }
}

#[test]
fn sweep_keeps_unknown_payloads_and_retires_only_understood_expired_envelopes() {
    for staging in [false, true] {
        let (_root, dirs) = sweep_world();
        let unknown: &[&[u8]] = &[
            b"",
            b"{\"v\":",
            b"{}",
            b"{\"v\":3,\"expires_at\":1}",
            b"{\"v\":1,\"expires_at\":1}",
            b"{\"v\":1}",
            b"{\"v\":2,\"expires_at\":\"1\"}",
            b"{\"v\":2,\"expires_at\":-1}",
        ];
        for (index, bytes) in unknown.iter().enumerate() {
            let nonce = format!("{index:032x}");
            let name = if staging {
                format!(".{nonce}.tmp")
            } else {
                format!("{nonce}.json")
            };
            let path = dirs.grants().join(name);
            crate::test_fixtures::setup_file(&path, bytes);
            for now in [NOW, NOW + super::super::GRANT_MAX.as_secs() + 1] {
                super::sweep(&dirs, now);
                assert_eq!(std::fs::read(&path).unwrap(), *bytes);
            }
        }
        // Authorization is deliberately malformed, but this folder's own envelope still
        // establishes expiry. Cleanup neither validates nor repairs credential authority.
        let version = super::GRANT_VERSION;
        let nonce = format!("{:032x}", version + 100);
        let name = if staging {
            format!(".{nonce}.tmp")
        } else {
            format!("{nonce}.json")
        };
        let bytes = format!(r#"{{"v":{version},"expires_at":{NOW},"credential_binding":"bad"}}"#);
        let path = dirs.grants().join(name);
        crate::test_fixtures::setup_file(&path, bytes.as_bytes());
        super::sweep(&dirs, NOW);
        assert_eq!(std::fs::read(&path).unwrap(), bytes.as_bytes());
        super::sweep(&dirs, NOW + 1);
        assert!(!path.exists(), "known expired envelope was kept");
    }
}

#[test]
fn sweep_does_not_retire_unrelated_or_privately_unreadable_files() {
    use std::os::unix::fs::symlink;
    let (root, dirs) = sweep_world();
    let expired = br#"{"v":2,"expires_at":1}"#;
    for name in ["unrelated.json", ".unrelated.tmp"] {
        let path = dirs.grants().join(name);
        crate::test_fixtures::setup_file(&path, expired);
        super::sweep(&dirs, NOW);
        assert_eq!(std::fs::read(&path).unwrap(), expired);
    }
    let nonce = "cd".repeat(16);
    let large = dirs.grants().join(format!("{nonce}.json"));
    let mut bytes = expired.to_vec();
    bytes.resize(super::GRANT_FILE_MAX + 1, b' ');
    crate::test_fixtures::setup_file(&large, &bytes);
    let original = root.path().join("private-original");
    crate::test_fixtures::setup_file(&original, expired);
    let linked = dirs.grants().join(format!("{}.json", "ef".repeat(16)));
    std::fs::hard_link(&original, &linked).unwrap();
    let symbolic = dirs.grants().join(format!(".{}.tmp", "ed".repeat(16)));
    symlink(&original, &symbolic).unwrap();
    super::sweep(&dirs, NOW);
    assert_eq!(std::fs::read(&large).unwrap(), bytes);
    assert_eq!(std::fs::read(&linked).unwrap(), expired);
    assert!(
        std::fs::symlink_metadata(&symbolic)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read(&original).unwrap(), expired);
}

/// What an agh from before the version-2 record (aterm 0.201.0's `grant::sweep`) does to its
/// grants folder before each git run: it removes every `{nonce}.json` it does not read as a
/// version-1 record, and every one whose `expires_at` has passed.
fn sweep_as_version_one(folder: &Path, now: u64) {
    for entry in std::fs::read_dir(folder).unwrap().flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let live = match aterm_json::from_slice(&std::fs::read(&path).unwrap()) {
            Ok(aterm_json::Value::Object(record)) => {
                record.get("v").and_then(aterm_json::Value::as_u64) == Some(1)
                    && record
                        .get("expires_at")
                        .and_then(aterm_json::Value::as_u64)
                        .is_some_and(|expiry| expiry >= now)
            }
            _ => false,
        };
        if !live {
            std::fs::remove_file(&path).unwrap();
        }
    }
}

/// Two aterm versions on one Mac share agh's state directory, and the older one sweeps its
/// grants folder before each of its git runs. A grant that sweep could reach would be gone
/// before git asked the helper for it, so the grant lives where that sweep never looks; the
/// same record put in the older folder (the control) is removed.
#[test]
fn a_version_one_sweep_never_reaches_a_live_grant() {
    use super::GrantView as _;

    let root = crate::test_fixtures::scratch_dir("grant-older-sweep");
    let dirs = crate::config::Dirs::under(root.path());
    let slug = crate::Slug::parse("fixture-dev/app").unwrap();
    let ttl = super::super::GRANT_MAX;
    let grant = super::write(&dirs, &slug, "dev", [0xa5; 32], NOW, ttl).unwrap();
    let name = format!("{}.json", grant.nonce);
    let older = dirs.state.join("grants");
    crate::config::make_state_subdir(&dirs, &older).unwrap();
    let reached = older.join(&name);
    crate::test_fixtures::setup_file(&reached, grant.to_json().as_bytes());
    let its_own = older.join(format!("{}.json", "cd".repeat(16)));
    let version_one = format!(r#"{{"v":1,"expires_at":{}}}"#, NOW + 10);
    crate::test_fixtures::setup_file(&its_own, version_one.as_bytes());

    sweep_as_version_one(&older, NOW);
    assert!(
        !reached.exists(),
        "the control: a version-1 sweep removes a version-2 record it reaches"
    );
    assert!(
        its_own.exists(),
        "the control: it keeps its own live record"
    );
    assert_eq!(
        super::LiveGrants::new(&dirs).find(&grant.nonce, &slug, NOW),
        Some(grant)
    );

    // This agh's sweep retires its own expired grant and leaves the older folder alone.
    super::sweep(&dirs, NOW + ttl.as_secs() + 1);
    assert!(!dirs.grants().join(&name).exists());
    assert_eq!(std::fs::read(&its_own).unwrap(), version_one.as_bytes());
}

/// The grants folder is named for the record's version, and what this agh writes there holds
/// that version and reads back. So a record that changes again moves to a folder of its own
/// by construction, and no folder holds two versions' records.
#[test]
fn the_grants_folder_carries_the_record_version() {
    let root = crate::test_fixtures::scratch_dir("grant-folder-version");
    let dirs = crate::config::Dirs::under(root.path());
    let folder = format!("grants-v{}", super::GRANT_VERSION);
    assert_eq!(dirs.grants(), dirs.state.join(&folder));
    let slug = crate::Slug::parse("fixture-dev/app").unwrap();
    let ttl = super::super::GRANT_MAX;
    let grant = super::write(&dirs, &slug, "dev", [0x5a; 32], NOW, ttl).unwrap();
    let made: Vec<String> = std::fs::read_dir(&dirs.state)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(made, [folder]);
    let bytes = std::fs::read(dirs.grants().join(format!("{}.json", grant.nonce))).unwrap();
    let record: aterm_json::Value = aterm_json::from_slice(&bytes).unwrap();
    assert_eq!(
        record.get("v").and_then(aterm_json::Value::as_u64),
        Some(super::GRANT_VERSION)
    );
    assert_eq!(super::Grant::from_json(&grant.nonce, &bytes), Some(grant));
}

/// Two installed agh versions share agh's state directory. A grant record of another version
/// (the one before this agh's, or the one after it) in this agh's grants folder, with a nonce,
/// slug and spawner this agh would answer, is refused by the helper's reader, and this agh's
/// sweep never removes it, expired or not: it is not this agh's record to judge, which is the
/// mistake 0.201.0's sweep made with version-2 grants. The control, this agh's own record
/// with the same fields, is answered and then retired once expired.
#[test]
fn a_record_of_another_version_is_refused_and_never_removed() {
    use super::GrantView as _;

    let (_root, dirs) = sweep_world();
    let slug = crate::Slug::parse("fixture-dev/app").unwrap();
    let pid = std::process::id();
    let start = start_of(i32::try_from(pid).unwrap()).unwrap();
    let expiry = NOW + 10;
    let own = super::Grant {
        nonce: "ab".repeat(16),
        slug: slug.clone(),
        profile: "dev".into(),
        credential_binding: [0xa5; 32],
        pid,
        start,
        expires_at: expiry,
    };
    let mut others = Vec::new();
    for version in [super::GRANT_VERSION - 1, super::GRANT_VERSION + 1] {
        for staging in [false, true] {
            let nonce = format!("{:030x}{version:x}{}", 0, u8::from(staging));
            let mut record: aterm_json::Value = aterm_json::from_str(
                &super::Grant {
                    nonce: nonce.clone(),
                    ..own.clone()
                }
                .to_json(),
            )
            .unwrap();
            record
                .as_object_mut()
                .unwrap()
                .insert("v".into(), aterm_json::Value::from(version));
            let bytes = record.to_string().into_bytes();
            assert_eq!(super::Grant::from_json(&nonce, &bytes), None, "v{version}");
            let name = if staging {
                format!(".{nonce}.tmp")
            } else {
                format!("{nonce}.json")
            };
            let path = dirs.grants().join(name);
            crate::test_fixtures::setup_file(&path, &bytes);
            assert_eq!(
                super::LiveGrants::new(&dirs).find(&nonce, &slug, NOW),
                None,
                "v{version} was answered"
            );
            others.push((path, bytes, version));
        }
    }
    let own_path = dirs.grants().join(format!("{}.json", own.nonce));
    crate::test_fixtures::setup_file(&own_path, own.to_json().as_bytes());
    assert_eq!(
        super::LiveGrants::new(&dirs).find(&own.nonce, &slug, NOW),
        Some(own.clone())
    );

    for now in [
        NOW,
        expiry + 1,
        expiry + super::super::GRANT_MAX.as_secs() + 1,
    ] {
        super::sweep(&dirs, now);
        for (path, bytes, version) in &others {
            assert_eq!(
                std::fs::read(path).unwrap(),
                *bytes,
                "v{version} was removed at {now}"
            );
        }
    }
    assert!(
        !own_path.exists(),
        "the control: an expired own record is retired"
    );
}

#[test]
fn operation_grants_require_the_whole_version_two_binding() {
    let mut body = aterm_json::json!({
        "v": 2,
        "slug": "fixture-dev/app",
        "profile": "dev",
        "pid": 1,
        "start": 1,
        "expires_at": 42,
        "credential_binding": "a5".repeat(32),
    });
    let parse = |value: &aterm_json::Value| {
        super::Grant::from_json(&"ab".repeat(16), value.to_string().as_bytes())
    };
    assert_eq!(parse(&body).unwrap().credential_binding, [0xa5; 32]);
    for binding in [
        "a5".repeat(31),
        "A5".repeat(32),
        "g5".repeat(32),
        String::new(),
    ] {
        body.as_object_mut().unwrap().insert(
            "credential_binding".into(),
            aterm_json::Value::from(binding),
        );
        assert!(parse(&body).is_none());
    }
    body.as_object_mut().unwrap().insert(
        "credential_binding".into(),
        aterm_json::Value::from("a5".repeat(32)),
    );
    body.as_object_mut()
        .unwrap()
        .insert("v".into(), aterm_json::Value::from(1));
    assert!(
        parse(&body).is_none(),
        "unbound old grants must not be accepted"
    );
    body.as_object_mut()
        .unwrap()
        .insert("v".into(), aterm_json::Value::from(2));
    body.as_object_mut().unwrap().remove("credential_binding");
    assert!(parse(&body).is_none());
}

/// The pid as `in_tree` and `start_of` take it.
fn pid(raw: u32) -> i32 {
    i32::try_from(raw).expect("a pid that fits an i32")
}

/// The walk climbs from this process: this process and its parent are in the tree, at
/// their own starts, and a live child of this process, at its own start, is not. So a
/// grant names a spawner the asking helper descends from, never any live process whose
/// pid and start match (the model's `GrantPidBound`, against the real walk).
#[test]
fn the_ancestry_walk_holds_ancestors_and_not_a_live_child() {
    let me = std::process::id();
    let my_start = start_of(pid(me)).expect("this process's start");
    assert!(in_tree(me, my_start), "this process is in its own tree");
    assert!(!in_tree(me, my_start + 1), "a recycled pid never matches");
    let parent = aterm_uds::process::read_process_parent(pid(me)).expect("the parent");
    let parent_start = start_of(pid(parent)).expect("the parent's start");
    assert!(in_tree(parent, parent_start), "the parent is an ancestor");
    let cat = aterm_uds::deadline::stand_in(Path::new("/bin/cat"));
    let mut child = aterm_uds::spawnfd::command(&cat)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("the child");
    let stdin = aterm_uds::deadline::cat_echoes(&mut child);
    let child_pid = child.id();
    let child_start = start_of(pid(child_pid)).expect("the live child's start");
    let outside = in_tree(child_pid, child_start);
    drop(stdin);
    let _ = child.wait();
    assert!(!outside, "a live child is outside the asker's ancestry");
}
