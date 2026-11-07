// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Negative legacy-reader fence. Marker presence never grants owner or Slot
//! authority, including malformed, unreadable, linked or pending marker files.

use super::*;

pub const OWNER_MARKER_SUFFIX: &str = ".aterm-owner";
pub const OWNER_MARKER_TEXT: &[u8] = b"ATERM-CODEX-AUTH-OWNER 1\n";

pub(super) fn legacy_reader_allowed(home: &Path) -> bool {
    legacy_file_reader_allowed(&auth_path_in_home(home))
}

/// Negative marker fence for an already resolved exact auth file, including
/// custom basenames. True grants no source, Slot, ownership or readiness
/// authority; callers still need their own admission and startup gate.
pub fn legacy_file_reader_allowed(auth: &Path) -> bool {
    let Some(name) = auth.file_name() else {
        return false;
    };
    [OWNER_MARKER_SUFFIX, ".aterm-slot", ".aterm-slot-state"].iter().all(|suffix| {
        let mut marker = name.to_os_string();
        marker.push(suffix);
        matches!(std::fs::symlink_metadata(auth.with_file_name(marker)), Err(error) if error.kind()==std::io::ErrorKind::NotFound)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn retained_document_needs_exact_bytes_parent_and_its_actual_auth_lock() {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let other = root.path().join("other");
        for directory in [&home, &other] {
            aterm_types::fs_restricted::ensure_private_dir(directory).unwrap();
            write_auth_doc(directory, &json!({"auth_mode":"chatgpt"})).unwrap();
        }
        let path = auth_path_in_home(&home);
        let expected = std::fs::read(&path).unwrap();
        let witness = StagedWitness::open(&path).unwrap();
        let auth = AuthLock::try_take(&path).unwrap().unwrap();
        let wrong = AuthLock::try_take(&auth_path_in_home(&other))
            .unwrap()
            .unwrap();
        assert!(retained_document_current(&witness, &auth, &expected));
        assert!(!retained_document_current(&witness, &wrong, &expected));
        std::fs::write(&path, b"changed retained bytes").unwrap();
        assert!(!retained_document_current(&witness, &auth, &expected));
        std::fs::write(&path, &expected).unwrap();
        assert!(retained_document_current(&witness, &auth, &expected));
        std::fs::rename(&home, root.path().join("old-home")).unwrap();
        aterm_types::fs_restricted::ensure_private_dir(&home).unwrap();
        write_auth_doc(&home, &json!({"auth_mode":"chatgpt"})).unwrap();
        assert!(!retained_document_current(&witness, &auth, &expected));
    }

    #[test]
    fn exact_custom_auth_basename_is_never_replaced_by_a_default_guess() {
        let root = aterm_tempfile::tempdir().unwrap();
        let auth = root.path().join("custom.credentials");
        std::fs::write(root.path().join("auth.json.aterm-owner"), b"unrelated").unwrap();
        assert!(legacy_file_reader_allowed(&auth));
        for suffix in [OWNER_MARKER_SUFFIX, ".aterm-slot", ".aterm-slot-state"] {
            let mut name = auth.file_name().unwrap().to_os_string();
            name.push(suffix);
            let marker = auth.with_file_name(name);
            std::fs::write(&marker, b"malformed-marker-still-refuses").unwrap();
            assert!(!legacy_file_reader_allowed(&auth));
            std::fs::remove_file(&marker).unwrap();
        }
        assert!(legacy_file_reader_allowed(&auth));
        assert!(
            !auth.exists(),
            "negative fence must not open or create credentials"
        );
        assert!(!legacy_file_reader_allowed(Path::new("/")));
    }

    #[test]
    fn managed_markers_stop_legacy_snapshot_refresh_and_retained_reply_replay() {
        let now = 1_800_000_000;
        for suffix in [OWNER_MARKER_SUFFIX, ".aterm-slot", ".aterm-slot-state"] {
            let root = aterm_tempfile::tempdir().unwrap();
            let home = root.path();
            let token = super::super::tests::make_chatgpt_jwt(None, "plus", now - 1);
            let document = json!({"auth_mode":"chatgpt","tokens":{
                "id_token":token,"access_token":token,"refresh_token":"legacy-test-refresh",
                "account_id":"acct_native_test"
            }});
            write_auth_doc(home, &document).unwrap();
            let stored = load_stored_auth(home).unwrap();
            let response = RefreshJournal::Response {
                base: authentication_digest(&document).unwrap(),
                token: Some(refresh_token_digest("legacy-test-refresh")),
                tokens: RefreshedTokens {
                    access_token: "retained-test-access".into(),
                    id_token: None,
                    refresh_token: Some("retained-test-refresh".into()),
                    account_id: stored.account_id.clone(),
                    last_refresh: "2027-01-15T08:00:00Z".into(),
                },
            };
            response.write(home).unwrap();
            let journal = beside_auth(home, JOURNAL_SUFFIX);
            let auth_before = std::fs::read(auth_path_in_home(home)).unwrap();
            let journal_before = std::fs::read(&journal).unwrap();
            std::fs::write(beside_auth(home, suffix), OWNER_MARKER_TEXT).unwrap();
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            listener.set_nonblocking(true).unwrap();
            let issuer = format!("http://{}", listener.local_addr().unwrap());
            let snapshot = read_login_snapshot_using(
                home,
                &format!("{issuer}/usage"),
                &format!("{issuer}/oauth/token"),
                Duration::from_secs(4),
                now,
                None,
                None,
            );
            assert!(matches!(snapshot.reading, Reading::Unread));
            assert!(snapshot.context.is_none() && snapshot.identity.is_none());
            assert_eq!(std::fs::read(auth_path_in_home(home)).unwrap(), auth_before);
            assert_eq!(std::fs::read(&journal).unwrap(), journal_before);
            assert!(
                !lock_path(&auth_path_in_home(home)).exists(),
                "snapshot refuses before any legacy lock or recovery"
            );
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
            {
                let _held = AuthLock::try_take(&auth_path_in_home(home))
                    .unwrap()
                    .unwrap();
                assert!(matches!(recover_journal_held(home), Err(Refreshed::Failed)));
            }
            assert_eq!(std::fs::read(&journal).unwrap(), journal_before);
            std::fs::remove_file(&journal).unwrap();
            assert!(matches!(
                refresh_admitted(
                    home,
                    &stored,
                    &format!("{issuer}/oauth/token"),
                    Duration::from_secs(4),
                    now,
                    |_| true,
                    |_, _| panic!("managed legacy publication")
                ),
                Refreshed::Failed
            ));
            assert!(
                !journal.exists(),
                "managed refusal creates no spent-token journal"
            );
            assert_eq!(std::fs::read(auth_path_in_home(home)).unwrap(), auth_before);
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock,
                "managed legacy reader never sends a refresh POST"
            );
        }
    }

    #[test]
    fn malformed_linked_pending_and_unreadable_marker_names_never_grant_legacy_access() {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path();
        assert!(legacy_reader_allowed(home));
        let owner = beside_auth(home, OWNER_MARKER_SUFFIX);
        std::fs::write(&owner, b"not-an-owner-grant").unwrap();
        assert!(!legacy_reader_allowed(home));
        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};
            std::fs::set_permissions(&owner, std::fs::Permissions::from_mode(0o0)).unwrap();
            assert!(!legacy_reader_allowed(home));
            std::fs::remove_file(&owner).unwrap();
            symlink(home.join("missing"), &owner).unwrap();
            assert!(!legacy_reader_allowed(home));
        }
        std::fs::remove_file(&owner).unwrap();
        std::fs::create_dir(&owner).unwrap();
        assert!(!legacy_reader_allowed(home));
        std::fs::remove_dir(&owner).unwrap();
        assert!(legacy_reader_allowed(home));
        let custom = home.join("custom.json");
        std::fs::write(beside_auth(&custom, ".aterm-slot-state"), b"pending").unwrap();
        assert!(!legacy_reader_allowed(&custom));
    }
    #[test]
    #[cfg(unix)]
    fn marker_published_under_auth_lock_during_usage_refuses_401_refresh_and_stale_readiness() {
        let now = 1_800_000_000;
        let body=br#"{"plan_type":"pro","rate_limit":{"allowed":true,"primary_window":{"used_percent":42.0,"limit_window_seconds":18000,"reset_at":1800003600}}}"#;
        assert!(usage_reading(200, body, now).usage().is_some());
        for status in [200, 401] {
            let world = aterm_tempfile::tempdir().unwrap();
            let home = world.path();
            let token = super::super::tests::make_chatgpt_jwt(None, "plus", now + 3600);
            write_auth_doc(
                home,
                &json!({"auth_mode":"chatgpt","tokens":{"id_token":token,"access_token":token,
                "refresh_token":"legacy-race-refresh","account_id":"acct_native_test"}}),
            )
            .unwrap();
            let before = std::fs::read(auth_path_in_home(home)).unwrap();
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let issuer = format!("http://{}", listener.local_addr().unwrap());
            let served = listener.try_clone().unwrap();
            let target = home.to_owned();
            let publisher = std::thread::spawn(move || {
                let (mut stream, _) = served.accept().unwrap();
                stream
                    .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
                    .unwrap();
                let mut request = Vec::new();
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let mut part = [0; 1024];
                    let read = stream.read(&mut part).unwrap();
                    assert!(read > 0);
                    request.extend_from_slice(&part[..read]);
                }
                assert!(request.starts_with(b"GET /usage "));
                // This is the real lock a fresh marker publisher must retain.
                let _auth = AuthLock::try_take(&auth_path_in_home(&target))
                    .unwrap()
                    .unwrap();
                aterm_uds::private_file::write_private(
                    &target,
                    "auth.json.aterm-owner",
                    OWNER_MARKER_TEXT,
                )
                .unwrap();
                let wire = format!(
                    "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(wire.as_bytes()).unwrap();
                stream.write_all(body).unwrap();
                stream.flush().unwrap();
            });
            let snapshot = read_login_snapshot_using(
                home,
                &format!("{issuer}/usage"),
                &format!("{issuer}/oauth/token"),
                Duration::from_secs(4),
                now,
                None,
                None,
            );
            publisher.join().unwrap();
            assert!(matches!(snapshot.reading, Reading::Unread));
            assert!(snapshot.identity.is_none() && snapshot.context.is_none());
            assert_eq!(std::fs::read(auth_path_in_home(home)).unwrap(), before);
            assert!(!beside_auth(home, JOURNAL_SUFFIX).exists());
            listener.set_nonblocking(true).unwrap();
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock,
                "a late marker prevents the 401 refresh POST"
            );
        }
    }
}
